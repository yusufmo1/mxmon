//! Tiered background sampling. Two threads produce [`Update`]s over an mpsc
//! channel; the UI thread just receives and draws. Each tier runs at the
//! fastest rate its syscall cost justifies, all scaled by one shared knob.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use super::battery::{BatteryCollector, BatterySample};
use super::cpu::{CpuCollector, CpuSample, load_avg, uptime_secs};
use super::disk::{DiskCollector, DiskSample};
use super::flows::{FlowSample, FlowsCollector};
use super::gpu::{GpuCollector, GpuSample};
use super::kernel::{KernelCollector, KernelSnapshot};
use super::mem::{self, MemSample};
use super::net::{NetCollector, NetSample};
use super::ping::{PingCollector, PingSample};
use super::power::{PowerCollector, PowerSample};
use super::procs::{ProcCollector, ProcSample};
use super::selfcpu::SelfCpu;
use super::soc::SocInfo;
use super::storage::{StorageCollector, StorageSample};
use super::temps::{TempCollector, TempSample};

/// Messages flowing from the sampler threads to the UI.
pub enum Update {
    Fast(Box<FastSnapshot>),
    Power(Box<PowerSample>),
    Slow(Box<SlowSnapshot>),
    Procs(Box<ProcSample>),
    Ping(Box<PingSample>),
    Flows(Box<FlowSample>),
    /// Slow-moving health facts: SMART, volume cache behaviour, controller
    /// throttle. Its own tier because none of it changes at UI cadence.
    Health(Box<StorageSample>),
    /// Interrupt activity and wake assertions, on the same slow tier.
    Kernel(Box<KernelSnapshot>),
    /// A collector failed at startup; its panel should show why.
    SourceDown {
        source: &'static str,
        error: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct FastSnapshot {
    pub cpu: Option<CpuSample>,
    pub gpu: Option<GpuSample>,
    pub mem: Option<MemSample>,
    pub net: Option<NetSample>,
    pub disk: Option<DiskSample>,
    pub load: [f64; 3],
    pub uptime_secs: u64,
    /// mxmon's own CPU use, as a fraction of one core.
    pub self_cpu: f32,
}

#[derive(Debug, Clone, Default)]
pub struct SlowSnapshot {
    pub temps: Option<TempSample>,
    pub battery: Option<BatterySample>,
}

/// Shared, live-tunable sampling control.
pub struct Control {
    /// Fast-tier interval in ms; other tiers are fixed multiples.
    pub fast_ms: AtomicU64,
    pub paused: AtomicBool,
    pub shutdown: AtomicBool,
}

pub const FAST_MS_DEFAULT: u64 = 250;
pub const FAST_MS_MIN: u64 = 100;
pub const FAST_MS_MAX: u64 = 2000;
// Power/temps multipliers are pub(crate): the motion layer derives each
// tier's inter-sample interval from them for interpolation phases.
pub(crate) const POWER_EVERY: u64 = 2; // × fast
pub(crate) const TEMPS_EVERY: u64 = 2; // × fast — SMC sweep (cores, clusters, fans, power)
const SLOW_EVERY: u64 = 4; // × fast — HID die-sensor refresh + battery registry
pub(crate) const PROCS_EVERY: u64 = 8;
pub(crate) const PING_EVERY: u64 = 4; // × fast — one 64-byte ICMP echo (1 s at defaults)
pub(crate) const FLOWS_EVERY: u64 = 4; // × fast — one ntstat poll (1 s at defaults)
/// × fast — SMART + APFS + controller counters (10 s at defaults). None of it
/// moves faster than that, and the SMART call is the priciest thing the app
/// makes, so it earns the slowest tier.
const HEALTH_EVERY: u64 = 40;
const _: () = assert!(HEALTH_EVERY.is_multiple_of(FLOWS_EVERY));
const HEALTH_PER_WAKE: u64 = HEALTH_EVERY / FLOWS_EVERY;
// The procs thread wakes at the flows cadence and runs the process pass on
// every Nth wake; that only divides evenly when:
const _: () = assert!(PROCS_EVERY.is_multiple_of(FLOWS_EVERY));
const PROCS_PER_WAKE: u64 = PROCS_EVERY / FLOWS_EVERY;

impl Control {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            fast_ms: AtomicU64::new(FAST_MS_DEFAULT),
            paused: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        })
    }
}

/// Spawn the metric + process sampler threads, and the connectivity prober
/// when a ping host is configured (`None` = probing disabled).
pub fn spawn(
    soc: SocInfo,
    control: Arc<Control>,
    tx: Sender<Update>,
    ping_host: Option<String>,
    health_on: bool,
    kernel_on: bool,
) {
    let tx_metrics = tx.clone();
    let ctl_metrics = Arc::clone(&control);
    let soc_metrics = soc;
    thread::Builder::new()
        .name("mxmon-metrics".into())
        .spawn(move || metrics_loop(soc_metrics, &ctl_metrics, &tx_metrics))
        .expect("spawn metrics thread");

    if let Some(host) = ping_host {
        let tx_ping = tx.clone();
        let ctl_ping = Arc::clone(&control);
        thread::Builder::new()
            .name("mxmon-ping".into())
            .spawn(move || ping_loop(&host, &ctl_ping, &tx_ping))
            .expect("spawn ping thread");
    }

    thread::Builder::new()
        .name("mxmon-procs".into())
        .spawn(move || procs_loop(&control, &tx, health_on, kernel_on))
        .expect("spawn procs thread");
}

fn metrics_loop(soc: SocInfo, ctl: &Control, tx: &Sender<Update>) {
    crate::trace::mark("metrics: init begin");
    // Cheap collectors first — their tick-0 data goes out while the expensive
    // subscriptions below are still initializing.
    let mut cpu = Some(CpuCollector::new());
    let mut net = Some(NetCollector::new());
    let mut disk = match DiskCollector::new() {
        Ok(d) => Some(d),
        Err(e) => {
            let _ = tx.send(Update::SourceDown {
                source: "disk",
                error: e.to_string(),
            });
            None
        }
    };
    let mut self_cpu = SelfCpu::new();
    let mut gpu = match GpuCollector::new() {
        Ok(g) => Some(g),
        Err(e) => {
            let _ = tx.send(Update::SourceDown {
                source: "gpu",
                error: e.to_string(),
            });
            None
        }
    };
    crate::trace::mark("metrics: gpu ready");

    // Power consumes the SocInfo; the temps thread reads its own clone.
    let soc_temps = soc.clone();
    // The IOReport subscription (~60 ms) and SMC/HID discovery (~70 ms cached,
    // ~600 ms on the first-ever run) initialize in parallel while tick 0
    // paints the fast tier. Power takes its baseline in-thread so its first
    // sample after the join already spans a real measurement window.
    let (power, temps) = thread::scope(|s| {
        let power = s.spawn(move || -> Result<PowerCollector, String> {
            let mut p = PowerCollector::new(soc)?;
            let _ = p.sample(); // baseline; the first real read is tick 0
            crate::trace::mark("metrics: power ready");
            Ok(p)
        });
        let temps = s.spawn(|| {
            let t = TempCollector::new(&soc_temps);
            crate::trace::mark("metrics: temps ready");
            t
        });

        // Tick 0, part 1: absolutes (gpu, memory, load, uptime) are real
        // immediately; cpu/net prime their delta baselines here and read as
        // zero until tick 1.
        let fast = FastSnapshot {
            cpu: cpu.as_mut().and_then(|c| c.sample().ok()),
            gpu: gpu.as_mut().and_then(|g| g.sample().ok()),
            mem: mem::sample().ok(),
            net: net.as_mut().and_then(|n| n.sample().ok()),
            disk: disk.as_mut().and_then(|d| d.sample().ok()),
            load: load_avg(),
            uptime_secs: uptime_secs(),
            self_cpu: self_cpu.sample(),
        };
        let _ = tx.send(Update::Fast(Box::new(fast)));
        crate::trace::mark("fast tick 0 sent");
        (power.join(), temps.join())
    });
    let mut power = match power.expect("power init thread panicked") {
        Ok(p) => Some(p),
        Err(e) => {
            let _ = tx.send(Update::SourceDown {
                source: "power",
                error: e,
            });
            None
        }
    };
    let mut temps = match temps.expect("temps init thread panicked") {
        Ok(t) => Some(t),
        Err(e) => {
            let _ = tx.send(Update::SourceDown {
                source: "temps",
                error: e.to_string(),
            });
            None
        }
    };
    let battery = BatteryCollector::new();

    // Tick 0, part 2: the first power window (spanning the init overlap) and
    // the first temps + battery sweep. Every panel is live from here.
    if let Some(p) = power.as_mut()
        && let Ok(Some(sample)) = p.sample()
    {
        if tx.send(Update::Power(Box::new(sample))).is_err() {
            return;
        }
        crate::trace::mark("power tick 0 sent");
    }
    let slow = SlowSnapshot {
        temps: temps.as_mut().map(|t| t.sample(true)),
        battery: battery.sample(),
    };
    if tx.send(Update::Slow(Box::new(slow))).is_err() {
        return;
    }
    crate::trace::mark("slow tick 0 sent");

    let mut tick: u64 = 0;
    let mut next_at = Instant::now();
    loop {
        if ctl.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let fast_ms = ctl.fast_ms.load(Ordering::Relaxed);
        next_at += Duration::from_millis(fast_ms);
        // Sleep to the absolute deadline so sampling cost never drifts the cadence.
        let now = Instant::now();
        if next_at > now {
            thread::sleep(next_at - now);
        } else {
            next_at = now; // fell behind (e.g. system sleep); resync
        }
        if ctl.paused.load(Ordering::Relaxed) {
            continue;
        }
        tick += 1;

        let fast = FastSnapshot {
            cpu: cpu.as_mut().and_then(|c| c.sample().ok()),
            gpu: gpu.as_mut().and_then(|g| g.sample().ok()),
            mem: mem::sample().ok(),
            net: net.as_mut().and_then(|n| n.sample().ok()),
            disk: disk.as_mut().and_then(|d| d.sample().ok()),
            load: load_avg(),
            uptime_secs: uptime_secs(),
            self_cpu: self_cpu.sample(),
        };
        if tx.send(Update::Fast(Box::new(fast))).is_err() {
            return; // UI gone
        }
        if crate::trace::enabled() && tick <= 4 {
            crate::trace::mark(&format!("fast tick {tick} sent"));
        }

        // Tick 1 is a warm-up for every deferred tier: rates over one fast
        // interval right after the tick-0 absolutes, then each tier settles
        // into its cadence.
        if (tick == 1 || tick.is_multiple_of(POWER_EVERY))
            && let Some(p) = power.as_mut()
            && let Ok(Some(sample)) = p.sample()
        {
            if tx.send(Update::Power(Box::new(sample))).is_err() {
                return;
            }
            if crate::trace::enabled() && tick <= 8 {
                crate::trace::mark(&format!("power tick {tick} sent"));
            }
        }

        if tick == 1 || tick.is_multiple_of(TEMPS_EVERY) {
            let refresh_hid = tick.is_multiple_of(SLOW_EVERY);
            let slow = SlowSnapshot {
                temps: temps.as_mut().map(|t| t.sample(refresh_hid)),
                // The battery registry read stays on the slow cadence;
                // `None` here means "no new reading", not "no battery".
                battery: refresh_hid.then(|| battery.sample()).flatten(),
            };
            if tx.send(Update::Slow(Box::new(slow))).is_err() {
                return;
            }
            if crate::trace::enabled() && tick <= 8 {
                crate::trace::mark(&format!("slow tick {tick} sent"));
            }
        }
    }
}

/// The "expensive stuff" thread: process table every `PROCS_EVERY` fast
/// ticks, ntstat flow poll every `FLOWS_EVERY` (each wake). One thread —
/// both collectors together cost a few ms per second.
fn procs_loop(ctl: &Control, tx: &Sender<Update>, health_on: bool, kernel_on: bool) {
    let mut procs = ProcCollector::new();
    // Storage health shares this thread: it is slow, occasional, and must not
    // sit on the metrics cadence. Constructed only when enabled, so a disabled
    // setting costs not even a subscription.
    let mut storage = health_on.then(StorageCollector::new);
    let mut kernel = kernel_on.then(KernelCollector::new);
    let mut flows = match FlowsCollector::new() {
        Ok(f) => Some(f),
        Err(e) => {
            let _ = tx.send(Update::SourceDown {
                source: "flows",
                error: e.to_string(),
            });
            None
        }
    };
    // On any later sampling error the collector already retried once
    // internally — give up loudly rather than log-spam.
    let sample_flows = |flows: &mut Option<FlowsCollector>| -> bool {
        let Some(f) = flows.as_mut() else { return true };
        match f.sample() {
            Ok(sample) => tx.send(Update::Flows(Box::new(sample))).is_ok(),
            Err(e) => {
                let _ = tx.send(Update::SourceDown {
                    source: "flows",
                    error: e.to_string(),
                });
                *flows = None;
                true
            }
        }
    };

    let mut sent: u64 = 0;
    // Warm-up: rows (names, memory, threads, state) are valid immediately;
    // CPU% needs a delta, so a second pass follows one fast tick later. The
    // cadences start from there. Flows piggy-back both rounds (poll 1 asks
    // for the sweep, poll 2 folds it in), so `--json` settles fast.
    for round in 0..2_u64 {
        if ctl.shutdown.load(Ordering::Relaxed) {
            return;
        }
        if round == 1 {
            thread::sleep(Duration::from_millis(ctl.fast_ms.load(Ordering::Relaxed)));
        }
        if !sample_flows(&mut flows) {
            return;
        }
        if round == 1 {
            if let Some(st) = storage.as_mut()
                && tx.send(Update::Health(Box::new(st.sample()))).is_err()
            {
                return;
            }
            // Primes the interrupt deltas; the first real numbers land one
            // health tick later.
            if let Some(kc) = kernel.as_mut()
                && tx.send(Update::Kernel(Box::new(kc.sample()))).is_err()
            {
                return;
            }
        }
        if let Ok(sample) = procs.sample() {
            if tx.send(Update::Procs(Box::new(sample))).is_err() {
                return;
            }
            sent += 1;
            if crate::trace::enabled() {
                crate::trace::mark(&format!("procs sample {sent} sent"));
            }
        }
    }
    let mut wake: u64 = 0;
    let mut next_at = Instant::now();
    loop {
        if ctl.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let interval = ctl.fast_ms.load(Ordering::Relaxed) * FLOWS_EVERY;
        next_at += Duration::from_millis(interval);
        let now = Instant::now();
        if next_at > now {
            thread::sleep(next_at - now);
        } else {
            next_at = now;
        }
        if ctl.paused.load(Ordering::Relaxed) {
            continue;
        }
        wake += 1;
        if !sample_flows(&mut flows) {
            return;
        }
        if wake.is_multiple_of(HEALTH_PER_WAKE) {
            if let Some(st) = storage.as_mut()
                && tx.send(Update::Health(Box::new(st.sample()))).is_err()
            {
                return;
            }
            if let Some(kc) = kernel.as_mut()
                && tx.send(Update::Kernel(Box::new(kc.sample()))).is_err()
            {
                return;
            }
        }
        if wake.is_multiple_of(PROCS_PER_WAKE)
            && let Ok(sample) = procs.sample()
        {
            if tx.send(Update::Procs(Box::new(sample))).is_err() {
                return;
            }
            sent += 1;
            if crate::trace::enabled() && sent <= 2 {
                crate::trace::mark(&format!("procs sample {sent} sent"));
            }
        }
    }
}

/// Connectivity prober. Owns the only blocking recv in the app, so it gets
/// its own thread — a dead upstream can never stall the metrics cadence.
/// Probes immediately (startup-burst convention), then every ×4 tick.
fn ping_loop(host: &str, ctl: &Control, tx: &Sender<Update>) {
    let mut ping = match PingCollector::new(host) {
        Ok(p) => p,
        Err(error) => {
            let _ = tx.send(Update::SourceDown {
                source: "ping",
                error,
            });
            return;
        }
    };
    let mut sent: u64 = 0;
    let mut next_at = Instant::now();
    loop {
        if ctl.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let interval = ctl.fast_ms.load(Ordering::Relaxed) * PING_EVERY;
        if !ctl.paused.load(Ordering::Relaxed) {
            // Cap the wait under one probe period so a black-holed link
            // yields a miss per tick instead of a stalled prober.
            let timeout = Duration::from_millis(interval.saturating_sub(100).clamp(200, 900));
            let sample = ping.sample(timeout);
            if tx.send(Update::Ping(Box::new(sample))).is_err() {
                return;
            }
            sent += 1;
            if crate::trace::enabled() && sent == 1 {
                crate::trace::mark("ping tick 0 sent");
            }
        }
        next_at += Duration::from_millis(interval);
        let now = Instant::now();
        if next_at > now {
            thread::sleep(next_at - now);
        } else {
            next_at = now;
        }
    }
}
