//! Application state: latest snapshots, history rings, and UI state.

use std::collections::VecDeque;

use crate::collect::battery::BatterySample;
use crate::collect::flows::FlowSample;
use crate::collect::ping::PingSample;
use crate::collect::power::PowerSample;
use crate::collect::procs::{ProcRow, ProcSample};
use crate::collect::sampler::{FastSnapshot, Update};
use crate::collect::soc::SocInfo;
use crate::collect::temps::TempSample;
use crate::config::Config;

/// Fixed-capacity history ring for graph data.
#[derive(Debug, Clone)]
pub struct Ring {
    buf: VecDeque<f32>,
    cap: usize,
    /// Monotonic count of every push ever made — keeps [`Ring::buckets`]
    /// phase-anchored to absolute sample indices even after old samples
    /// fall off the front.
    total: u64,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap),
            cap,
            total: 0,
        }
    }

    pub fn push(&mut self, v: f32) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
        self.total += 1;
    }

    /// Latest `n` values, oldest first (fewer if not enough history yet).
    pub fn last_n(&self, n: usize) -> impl Iterator<Item = f32> + '_ {
        let skip = self.buf.len().saturating_sub(n);
        self.buf.iter().skip(skip).copied()
    }

    pub fn latest(&self) -> Option<f32> {
        self.buf.back().copied()
    }

    pub fn max(&self) -> f32 {
        self.buf.iter().copied().fold(0.0, f32::max)
    }

    /// The last `slots` *buckets* of `k` samples each, oldest-first — the
    /// render-time zoom behind the `graph_window` setting. Bucket boundaries
    /// are phase-anchored to the absolute push counter, so a completed
    /// bucket never re-aggregates as new samples land: the graph body holds
    /// perfectly still and the frame advances exactly one dot per completed
    /// bucket. The newest bucket is the live head — `((total-1) % k) + 1`
    /// samples, growing every push, so the rightmost column keeps moving at
    /// full tick rate while the body crawls. `k <= 1` is an exact,
    /// unfiltered passthrough of [`Ring::last_n`].
    pub fn buckets(&self, slots: usize, k: usize, agg: Agg) -> Vec<f32> {
        if k <= 1 {
            return self.last_n(slots).collect();
        }
        if slots == 0 || self.buf.is_empty() {
            return Vec::new();
        }
        let head = ((self.total - 1) % k as u64) as usize + 1;
        let want = head.saturating_add((slots - 1).saturating_mul(k));
        let window: Vec<f32> = self.last_n(want).collect();
        // A tiny cap can leave fewer samples than one head bucket; aggregate
        // whatever survives.
        let (body, head) = window.split_at(window.len().saturating_sub(head));
        // Newest-first: the head, then `rchunks` walking the body backwards
        // stays aligned to the head boundary (the oldest chunk may be short
        // when the ring is young — aggregate what's there).
        let mut out: Vec<f32> = std::iter::once(head)
            .chain(body.rchunks(k))
            .map(|bucket| agg.fold(bucket))
            .collect();
        out.reverse();
        out
    }

    /// Monotonic push count — the motion layer phase-aligns its head/shift
    /// interpolation to the same absolute counter [`Ring::buckets`] uses.
    pub(crate) fn pushes(&self) -> u64 {
        self.total
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Ring sample meaning "no one was watching this tick" — the app was not
/// running (see `history`), as opposed to a NaN, which means a sample was
/// due and didn't arrive (a missed probe, a downed source).
///
/// Negative infinity is safe as the sentinel because every ring stores a
/// display-space quantity that is bounded below in practice — percentages,
/// watts, bytes/s, milliseconds, degrees — so no collector can produce it.
/// It survives [`Agg::fold`] and reaches the renderer, where
/// `widgets::slot_at` maps it to `Slot::Uncovered` and nothing is drawn.
/// That is what lets a restored shutdown gap of *any* length stay honest at
/// every graph width: unobserved time renders as absence, never as a floor
/// reading, with no threshold to tune.
pub const UNOBSERVED: f32 = f32::NEG_INFINITY;

/// Whether a ring sample marks unobserved time (see [`UNOBSERVED`]).
pub fn is_unobserved(v: f32) -> bool {
    v.is_infinite() && v.is_sign_negative()
}

/// Per-bucket aggregation for [`Ring::buckets`], curated per metric by the
/// panels — never configurable, always chosen by what the series means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    /// Largest finite sample — bursty series (cpu, gpu, power, net, disk)
    /// where a one-tick spike is the signal and a mean would erase it.
    Max,
    /// Mean of the finite samples — smooth series (temps, memory) where
    /// drift is the signal and peaks are noise.
    Mean,
    /// NaN if *any* sample is non-finite, else the max — the ping strip's
    /// "a probe missed somewhere in this window" contract.
    Worst,
}

impl Agg {
    /// Fold one bucket; NaN when no sample survives the finite filter.
    /// `pub(crate)` for the motion layer, which folds the live head with and
    /// without its newest sample to ease between the two.
    pub(crate) fn fold(self, bucket: &[f32]) -> f32 {
        // A bucket nothing ever looked at stays unobserved, and survives the
        // fold as such — otherwise it would collapse into the same NaN a
        // missed sample produces, and the graph would draw time the app
        // wasn't running as a gap in data it was watching.
        if !bucket.is_empty() && bucket.iter().copied().all(is_unobserved) {
            return UNOBSERVED;
        }
        // `Worst` means "a probe was due here and didn't land". Unobserved
        // time had no probe due, so it must not poison the bucket — else a
        // bucket straddling a shutdown boundary (part restored, part live)
        // reports an outage that never happened, and every relaunch paints
        // a fake miss into the strip.
        if self == Self::Worst && bucket.iter().any(|v| !v.is_finite() && !is_unobserved(*v)) {
            return f32::NAN;
        }
        let finite = bucket.iter().copied().filter(|v| v.is_finite());
        match self {
            // `f32::max` keeps the non-NaN operand, so the NAN seed
            // vanishes at the first finite sample.
            Self::Max | Self::Worst => finite.fold(f32::NAN, f32::max),
            Self::Mean => {
                // f64 accumulator: k finite-huge f32s must not sum to inf.
                let (sum, n) = finite.fold((0f64, 0u32), |(s, n), v| (s + f64::from(v), n + 1));
                if n == 0 {
                    f32::NAN
                } else {
                    (sum / f64::from(n)) as f32
                }
            }
        }
    }
}

/// Ring capacity: enough raw samples that a 300-cell-wide graph still fills
/// every dot column at the ×8 graph window (600 slots × 8) — about 20 min of
/// fast-tier history at the default 250 ms tick, ~0.7 MB across all rings.
pub const HISTORY: usize = 4800;

/// All history rings (each advances with its own tier's cadence).
pub struct Histories {
    pub cpu_total: Ring,
    pub per_core: Vec<Ring>,
    pub ecpu_usage: Ring,
    pub pcpu_usage: Ring,
    pub gpu: Ring,
    pub package_w: Ring,
    pub cpu_w: Ring,
    pub gpu_w: Ring,
    pub ane_w: Ring,
    pub dram_w: Ring,
    /// Memory-controller fabric (`AMCC*`) — its own rail, not part of `dram_w`.
    pub amcc_w: Ring,
    /// DRAM command scheduler / PHY (`DCS*`).
    pub dcs_w: Ring,
    pub disp_w: Ring,
    pub sys_w: Ring,
    pub mem_used: Ring,
    pub net_rx: Ring,
    pub net_tx: Ring,
    /// Probe RTT in ms; a miss is stored as NaN (`Ring::max` ignores NaN).
    pub ping_ms: Ring,
    pub disk_rd: Ring,
    pub disk_wr: Ring,
    pub cpu_temp: Ring,
    pub gpu_temp: Ring,
    /// SMC backlight rail (temps tier); NaN when the key is absent
    /// (desktops) so the flow panel's sink gate can average total windows.
    pub backlight_w: Ring,
}

impl Histories {
    pub(crate) fn new(cores: usize) -> Self {
        let r = || Ring::new(HISTORY);
        Self {
            cpu_total: r(),
            per_core: (0..cores).map(|_| Ring::new(HISTORY)).collect(),
            ecpu_usage: r(),
            pcpu_usage: r(),
            gpu: r(),
            package_w: r(),
            cpu_w: r(),
            gpu_w: r(),
            ane_w: r(),
            dram_w: r(),
            amcc_w: r(),
            dcs_w: r(),
            disp_w: r(),
            sys_w: r(),
            mem_used: r(),
            net_rx: r(),
            net_tx: r(),
            ping_ms: r(),
            disk_rd: r(),
            disk_wr: r(),
            cpu_temp: r(),
            gpu_temp: r(),
            backlight_w: r(),
        }
    }
}

/// Which main view is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Overview,
    Processes,
    Thermal,
    Connections,
}

/// Process table sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Cpu,
    Memory,
    Power,
    Net,
    Pid,
    Name,
    User,
    Threads,
}

impl SortKey {
    pub fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU%",
            Self::Memory => "MEM",
            Self::Power => "PWR",
            Self::Net => "NET",
            Self::Pid => "PID",
            Self::Name => "NAME",
            Self::User => "USER",
            Self::Threads => "THR",
        }
    }
}

/// Active modal overlay, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    Kill {
        pid: i32,
        name: String,
        selected: usize,
    },
    SortMenu {
        selected: usize,
    },
    Details {
        pid: i32,
    },
    /// The settings card. Its cursor lives in [`App::settings`] rather than
    /// here, so closing and reopening returns to the page you were on.
    Settings,
    /// The inspector: the slow-tier facts that have no room on a card.
    /// Tabbed rather than three separate modals, reusing the settings card's
    /// tab-strip idiom instead of inventing a second one.
    Inspect {
        tab: usize,
    },
}

/// Pages of the inspector, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectTab {
    Storage,
    Kernel,
    Battery,
}

pub const INSPECT_TABS: [InspectTab; 3] =
    [InspectTab::Storage, InspectTab::Kernel, InspectTab::Battery];

impl InspectTab {
    pub fn title(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Kernel => "kernel",
            Self::Battery => "battery",
        }
    }

    /// Cursors are hostile input; clamp rather than index.
    pub fn at(index: usize) -> Self {
        INSPECT_TABS[index.min(INSPECT_TABS.len() - 1)]
    }
}

/// Cursor and edit state for the settings card. Every field is treated as
/// hostile input by the renderer (clamped before indexing), so a stale cursor
/// after a section change can only mis-highlight, never panic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsUi {
    /// Index into [`crate::settings::SECTIONS`].
    pub section: usize,
    /// Row within the active section.
    pub row: usize,
    pub edit: Option<Edit>,
}

/// The card's two capture modes — the states where keys stop meaning
/// navigation and start meaning content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Typing into a text setting; `buf` is the pending value.
    Text {
        id: crate::settings::Id,
        buf: String,
    },
    /// Waiting for the next key press to bind it to `action`.
    Capture { action: crate::keys::Action },
}

/// Signals offered by the kill modal.
pub const KILL_SIGNALS: [(&str, i32); 4] = [
    ("SIGTERM  · graceful", libc::SIGTERM),
    ("SIGKILL  · force", libc::SIGKILL),
    ("SIGINT   · interrupt", libc::SIGINT),
    ("SIGHUP   · hangup", libc::SIGHUP),
];

pub const SORT_KEYS: [SortKey; 8] = [
    SortKey::Cpu,
    SortKey::Memory,
    SortKey::Power,
    SortKey::Net,
    SortKey::Pid,
    SortKey::Name,
    SortKey::User,
    SortKey::Threads,
];

/// Transient status message shown in the footer.
pub struct Toast {
    pub text: String,
    pub until: std::time::Instant,
    pub error: bool,
}

pub struct App {
    pub soc: SocInfo,
    pub config: Config,

    // Latest data per tier.
    pub fast: FastSnapshot,
    pub power: Option<PowerSample>,
    pub temps: Option<TempSample>,
    /// Bumped on every new `temps` sample; render caches key off it.
    pub temps_seq: u64,
    pub battery: Option<BatterySample>,
    pub ping: Option<PingSample>,
    pub procs: ProcSample,
    pub flows: FlowSample,
    /// Latest storage-health pass; `None` until the first one lands (or
    /// forever, when the setting is off).
    pub storage: Option<crate::collect::storage::StorageSample>,
    /// Latest interrupt/assertion pass; `None` until the first one lands.
    pub kernel: Option<crate::collect::kernel::KernelSnapshot>,
    pub source_errors: Vec<(&'static str, String)>,

    pub hist: Histories,

    // UI state.
    pub view: View,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub filter: String,
    pub filter_editing: bool,
    pub selected: usize,
    pub scroll: usize,
    pub modal: Option<Modal>,
    /// Where the settings card is parked — kept outside `modal` so it
    /// survives the card being closed and reopened.
    pub settings: SettingsUi,
    pub paused: bool,
    pub show_hud: bool,
    pub toast: Option<Toast>,
    /// Interactive element currently under the mouse (drives hover
    /// affordances). Updated only when the pointer crosses a target
    /// boundary, so idle motion never costs a redraw.
    pub hover: Option<crate::ui::widgets::Target>,

    /// Sorted + filtered view of `procs.rows` (indices into it).
    pub visible_rows: Vec<usize>,

    // Perf HUD numbers.
    pub last_frame_us: u64,
    pub frames: u64,

    /// Per-tier arrival stamps for the fluid-graph interpolation.
    pub motion_clock: crate::ui::motion::MotionClock,
    /// The wall-clock instant of the frame being rendered — set once per
    /// frame by the draw loop so every panel interpolates against the same
    /// moment (tests set it directly; no sleeping).
    pub frame_now: std::time::Instant,
}

impl App {
    pub fn new(soc: SocInfo, config: Config) -> Self {
        let cores = soc.total_cores().max(1);
        Self {
            soc,
            config,
            fast: FastSnapshot::default(),
            power: None,
            temps: None,
            temps_seq: 0,
            battery: None,
            ping: None,
            procs: ProcSample::default(),
            flows: FlowSample::default(),
            storage: None,
            kernel: None,
            source_errors: Vec::new(),
            hist: Histories::new(cores),
            view: View::Overview,
            sort: SortKey::Cpu,
            sort_desc: true,
            filter: String::new(),
            filter_editing: false,
            selected: 0,
            scroll: 0,
            modal: None,
            settings: SettingsUi::default(),
            paused: false,
            show_hud: false,
            toast: None,
            hover: None,
            visible_rows: Vec::new(),
            last_frame_us: 0,
            frames: 0,
            motion_clock: crate::ui::motion::MotionClock::default(),
            frame_now: std::time::Instant::now(),
        }
    }

    /// Samples aggregated per graph dot column (the `graph_window` setting,
    /// clamped ≥ 1 so a hand-edited 0 can't wedge the resampler).
    pub fn graph_k(&self) -> usize {
        usize::from(self.config.graph_window.max(1))
    }

    /// The display vector for one graph: [`Ring::buckets`] carried by the
    /// motion layer's constant-velocity conveyor (each slot a convex blend
    /// of two adjacent real bucket values, the live head fold eased). With
    /// motion off — or at a completed bucket's settled instant — this is
    /// bit-identical to `buckets`, so rendering at rest never shows a value
    /// that wasn't sampled.
    pub fn series(
        &self,
        ring: &Ring,
        slots: usize,
        agg: Agg,
        tier: crate::ui::motion::Tier,
    ) -> Vec<f32> {
        let k = self.graph_k();
        if !self.config.motion {
            return ring.buckets(slots, k, agg);
        }
        let phase = crate::ui::motion::phase(
            self.frame_now,
            self.motion_clock.last(tier),
            tier.interval(self.config.interval_ms),
        );
        crate::ui::motion::series(ring, slots, k, agg, phase)
    }

    /// The scale basis for one graph: the *raw* bucket window a drifting
    /// [`Self::series`] samples from. Autoscales and axis windows must read
    /// this, never the interpolated series — otherwise the axis pumps with
    /// the blend every frame (a lone spike's windowed max dips toward half
    /// mid-drift, so the whole waveform breathes at bucket cadence). Every
    /// interpolated value is a convex blend of two adjacent entries here,
    /// so a scale from this window can never clip the drawn data — and it
    /// only ever changes when a sample actually lands.
    pub fn series_span(&self, ring: &Ring, slots: usize, agg: Agg) -> Vec<f32> {
        let k = self.graph_k();
        if !self.config.motion {
            return ring.buckets(slots, k, agg);
        }
        // One extra bucket to the left: the conveyor's full source window.
        ring.buckets(slots.saturating_add(1), k, agg)
    }

    /// Fold a sampler update into state + histories.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Fast(f) => {
                if let Some(cpu) = &f.cpu {
                    // Rings store percentages (0..100) — what the panels show.
                    let total: f32 = cpu.per_core.iter().map(|r| r.as_percent()).sum::<f32>()
                        / cpu.per_core.len().max(1) as f32;
                    self.hist.cpu_total.push(total);
                    for (i, r) in cpu.per_core.iter().enumerate() {
                        if let Some(ring) = self.hist.per_core.get_mut(i) {
                            ring.push(r.as_percent());
                        }
                    }
                }
                if let Some(gpu) = &f.gpu {
                    self.hist.gpu.push(gpu.device.0);
                }
                if let Some(mem) = &f.mem {
                    self.hist.mem_used.push(mem.used_ratio().0);
                }
                if let Some(net) = &f.net {
                    self.hist.net_rx.push(net.rx_per_sec.0 as f32);
                    self.hist.net_tx.push(net.tx_per_sec.0 as f32);
                }
                if let Some(disk) = &f.disk {
                    self.hist.disk_rd.push(disk.read_per_sec.0 as f32);
                    self.hist.disk_wr.push(disk.write_per_sec.0 as f32);
                }
                self.fast = *f;
                self.motion_clock.fast = Some(std::time::Instant::now());
            }
            Update::Power(p) => {
                self.hist.package_w.push(p.package().0);
                self.hist.cpu_w.push(p.cpu.0);
                self.hist.gpu_w.push(p.gpu.0);
                self.hist.ane_w.push(p.ane.0);
                self.hist.dram_w.push(p.dram.0);
                self.hist.amcc_w.push(p.amcc.0);
                self.hist.dcs_w.push(p.dcs.0);
                self.hist.disp_w.push(p.display.0);
                self.hist.ecpu_usage.push(p.ecpu.usage.0);
                self.hist.pcpu_usage.push(p.pcpu.usage.0);
                self.power = Some(*p);
                self.motion_clock.power = Some(std::time::Instant::now());
            }
            Update::Slow(s) => {
                if let Some(t) = &s.temps {
                    self.hist.cpu_temp.push(t.cpu_avg.0);
                    self.hist.gpu_temp.push(t.gpu_avg.0);
                    self.hist
                        .backlight_w
                        .push(t.backlight_power.map_or(f32::NAN, |w| w.0));
                    if let Some(w) = t.sys_power {
                        self.hist.sys_w.push(w.0);
                    }
                }
                if let Some(t) = s.temps {
                    self.temps = Some(t);
                    self.temps_seq += 1;
                    self.motion_clock.temps = Some(std::time::Instant::now());
                }
                // Battery arrives on a slower cadence than temps; a None
                // between readings means "unchanged", so keep the last one.
                if let Some(b) = s.battery {
                    self.battery = Some(b);
                }
            }
            Update::Procs(p) => {
                self.procs = *p;
                self.refresh_visible();
            }
            Update::Ping(p) => {
                self.hist.ping_ms.push(p.rtt_ms.unwrap_or(f32::NAN));
                self.ping = Some(*p);
            }
            Update::Flows(f) => {
                self.flows = *f;
                // Only the net sort key reads flow data — don't churn the
                // row order under other sorts between procs ticks.
                if self.sort == SortKey::Net {
                    self.refresh_visible();
                }
            }
            Update::Health(h) => {
                self.storage = Some(*h);
            }
            Update::Kernel(k) => {
                self.kernel = Some(*k);
            }
            Update::SourceDown { source, error } => {
                self.source_errors.push((source, error));
            }
        }
    }

    /// Rebuild `visible_rows` after data, sort, or filter changes.
    pub fn refresh_visible(&mut self) {
        let filter = self.filter.to_lowercase();
        let rows = &self.procs.rows;
        self.visible_rows = (0..rows.len())
            .filter(|&i| {
                filter.is_empty()
                    || rows[i].name.to_lowercase().contains(&filter)
                    || rows[i].pid.to_string() == filter
                    || rows[i].user.to_lowercase().contains(&filter)
            })
            .collect();

        let key = self.sort;
        let desc = self.sort_desc;
        let net = &self.flows.by_pid;
        self.visible_rows.sort_by(|&a, &b| {
            let (ra, rb) = (&rows[a], &rows[b]);
            let ord = match key {
                SortKey::Cpu => ra
                    .cpu
                    .map_or(-1.0, |c| c.0)
                    .total_cmp(&rb.cpu.map_or(-1.0, |c| c.0)),
                SortKey::Memory => ra
                    .memory
                    .map_or(0, |m| m.0)
                    .cmp(&rb.memory.map_or(0, |m| m.0)),
                // Unreadable rows sink below a legitimate 0 W reading.
                SortKey::Power => ra
                    .power
                    .map_or(-1.0, |w| w.0)
                    .total_cmp(&rb.power.map_or(-1.0, |w| w.0)),
                SortKey::Net => {
                    let na = net.get(&ra.pid).map_or(0, |&(rx, tx)| rx + tx);
                    let nb = net.get(&rb.pid).map_or(0, |&(rx, tx)| rx + tx);
                    na.cmp(&nb)
                }
                SortKey::Pid => ra.pid.cmp(&rb.pid),
                SortKey::Name => ra.name.to_lowercase().cmp(&rb.name.to_lowercase()),
                SortKey::User => ra.user.cmp(&rb.user),
                SortKey::Threads => ra.threads.unwrap_or(0).cmp(&rb.threads.unwrap_or(0)),
            };
            if desc { ord.reverse() } else { ord }
        });

        self.selected = self.selected.min(self.visible_rows.len().saturating_sub(1));
    }

    /// The currently selected process row, if any.
    pub fn selected_row(&self) -> Option<&ProcRow> {
        self.visible_rows
            .get(self.selected)
            .map(|&i| &self.procs.rows[i])
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            until: std::time::Instant::now() + std::time::Duration::from_secs(4),
            error,
        });
    }

    /// Move the process selection by `delta`, clamped.
    pub fn move_selection(&mut self, delta: i64) {
        let len = self.visible_rows.len();
        if len == 0 {
            return;
        }
        let new = (self.selected as i64 + delta).clamp(0, len as i64 - 1);
        self.selected = new as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::{Agg, App, Ring, SortKey, UNOBSERVED, is_unobserved};
    use crate::collect::sampler::{SlowSnapshot, Update};
    use crate::config::Config;
    use crate::testutil as tu;

    #[test]
    fn ring_windows_and_max() {
        let mut r = Ring::new(4);
        assert!(r.is_empty());
        for v in [1.0, 2.0, 5.0, 3.0, 4.0] {
            r.push(v);
        }
        // Capacity 4: the 1.0 fell off.
        assert_eq!(r.last_n(10).collect::<Vec<_>>(), vec![2.0, 5.0, 3.0, 4.0]);
        assert_eq!(r.last_n(2).collect::<Vec<_>>(), vec![3.0, 4.0]);
        assert!((r.max() - 5.0).abs() < f32::EPSILON);
        assert_eq!(r.latest(), Some(4.0));
    }

    #[test]
    fn ring_max_ignores_nan_gaps() {
        // Ping misses are stored as NaN; the autoscale must see through them.
        let mut r = Ring::new(8);
        for v in [1.0, f32::NAN, 3.0, f32::NAN] {
            r.push(v);
        }
        assert!((r.max() - 3.0).abs() < f32::EPSILON);
    }

    fn ring_of(values: impl IntoIterator<Item = f32>) -> Ring {
        let mut r = Ring::new(super::HISTORY);
        for v in values {
            r.push(v);
        }
        r
    }

    #[test]
    fn buckets_k1_is_exact_passthrough() {
        // ×1 must be byte-identical to today's rendering: raw values,
        // NaN gaps included, no aggregation filter.
        let r = ring_of([1.0, f32::NAN, 3.0, 4.0]);
        for k in [0, 1] {
            let out = r.buckets(3, k, Agg::Max);
            let raw: Vec<f32> = r.last_n(3).collect();
            assert_eq!(out.len(), raw.len());
            for (a, b) in out.iter().zip(&raw) {
                assert_eq!(a.to_bits(), b.to_bits(), "k={k} passthrough");
            }
        }
    }

    #[test]
    fn buckets_aggregate_and_live_head() {
        let r = ring_of((1..=10).map(|v| v as f32));
        // total=10, k=4 → head bucket = [9, 10] (2 live samples), completed
        // buckets [1..4] and [5..8] behind it.
        assert_eq!(r.buckets(3, 4, Agg::Max), vec![4.0, 8.0, 10.0]);
        assert_eq!(r.buckets(3, 4, Agg::Mean), vec![2.5, 6.5, 9.5]);
        // Fewer slots trims from the old end, never the live head.
        assert_eq!(r.buckets(2, 4, Agg::Max), vec![8.0, 10.0]);
        assert_eq!(r.buckets(1, 4, Agg::Max), vec![10.0]);
    }

    #[test]
    fn buckets_completed_buckets_never_change() {
        // The still-body contract: once a bucket completes, its aggregate is
        // frozen — later pushes only grow the head or append new buckets, so
        // the graph body never shimmers. Track every completed bucket by its
        // absolute index and assert a re-render never disagrees.
        let mut r = Ring::new(super::HISTORY);
        let mut seen = std::collections::HashMap::<u64, f32>::new();
        for v in 1..=23u64 {
            r.push(v as f32);
            let out = r.buckets(8, 3, Agg::Max);
            let completed = &out[..out.len() - 1];
            // Buckets strictly behind the live head after `v` pushes.
            let behind = (v - 1) / 3;
            for (j, &val) in completed.iter().rev().enumerate() {
                let idx = behind - 1 - j as u64;
                if let Some(&old) = seen.get(&idx) {
                    // Bit-exact on purpose: frozen means frozen.
                    assert_eq!(
                        old.to_bits(),
                        val.to_bits(),
                        "push {v}: bucket {idx} re-aggregated"
                    );
                }
                seen.insert(idx, val);
            }
        }
        // 23 pushes at k=3: live head is [22, 23], body caps at 21, 18, 15.
        assert_eq!(r.buckets(4, 3, Agg::Max), vec![15.0, 18.0, 21.0, 23.0]);
    }

    #[test]
    fn buckets_phase_survives_capacity_wrap() {
        // A wrapped ring keeps absolute bucket boundaries: with cap 4 and 10
        // pushes, the survivors [7,8,9,10] still split as [7,8 | 9,10] —
        // anchored by the push counter, not the window edge.
        let mut r = Ring::new(4);
        for v in 1..=10 {
            r.push(v as f32);
        }
        assert_eq!(r.buckets(3, 4, Agg::Max), vec![8.0, 10.0]);
    }

    #[test]
    fn buckets_aggregators_handle_misses() {
        // Bucket [1, NaN, 3] then a clean [4, 5, 6].
        let r = ring_of([1.0, f32::NAN, 3.0, 4.0, 5.0, 6.0]);
        let worst = r.buckets(2, 3, Agg::Worst);
        assert!(worst[0].is_nan(), "a miss anywhere poisons Worst");
        assert_eq!(worst[1].to_bits(), 6.0f32.to_bits());
        assert_eq!(r.buckets(2, 3, Agg::Max), vec![3.0, 6.0]);
        assert_eq!(r.buckets(2, 3, Agg::Mean), vec![2.0, 5.0]);

        // Unobserved time is not a miss. A bucket straddling a shutdown
        // boundary — part restored history, part live — reports the live
        // samples, not an outage; otherwise every relaunch paints a fake
        // red cell into the ping strip.
        let edge = ring_of([UNOBSERVED, UNOBSERVED, 7.0]);
        assert_eq!(
            edge.buckets(1, 3, Agg::Worst)[0].to_bits(),
            7.0f32.to_bits(),
            "unobserved samples must not poison Worst"
        );
        // But a real miss beside unobserved time still reads as a miss.
        let missed = ring_of([UNOBSERVED, f32::NAN, 7.0]);
        assert!(missed.buckets(1, 3, Agg::Worst)[0].is_nan());
        // Wholly unobserved folds to unobserved under every aggregator, so
        // the renderer can tell "we weren't watching" from "we missed".
        let away = ring_of([UNOBSERVED, UNOBSERVED, UNOBSERVED]);
        for agg in [Agg::Max, Agg::Mean, Agg::Worst] {
            assert!(
                is_unobserved(away.buckets(1, 3, agg)[0]),
                "{agg:?} must preserve unobserved time"
            );
        }

        // A bucket with no finite sample at all renders as a gap.
        let dead = ring_of([f32::NAN, f32::INFINITY, f32::NEG_INFINITY]);
        assert!(dead.buckets(1, 3, Agg::Max)[0].is_nan());
        assert!(dead.buckets(1, 3, Agg::Mean)[0].is_nan());
        // Mean survives finite-huge values via the f64 accumulator.
        let huge = ring_of([f32::MAX, f32::MAX]);
        assert_eq!(huge.buckets(1, 2, Agg::Mean), vec![f32::MAX]);
    }

    #[test]
    fn buckets_degenerate_inputs() {
        assert!(Ring::new(4).buckets(5, 3, Agg::Max).is_empty());
        assert!(ring_of([1.0]).buckets(0, 3, Agg::Max).is_empty());
        assert_eq!(ring_of([1.0]).buckets(4, 8, Agg::Max), vec![1.0]);
    }

    mod bucket_props {
        use proptest::prelude::*;

        use super::super::{Agg, Ring};

        proptest! {
            /// Total for any input: never panics, never exceeds `slots`,
            /// and k≤1 is bit-exact with `last_n`.
            #[test]
            fn buckets_never_panic_and_k1_matches_last_n(
                values in proptest::collection::vec(proptest::num::f32::ANY, 0..200),
                cap in 1usize..64,
                slots in 0usize..64,
                k in 0usize..16,
            ) {
                let mut r = Ring::new(cap);
                for v in &values {
                    r.push(*v);
                }
                for agg in [Agg::Max, Agg::Mean, Agg::Worst] {
                    prop_assert!(r.buckets(slots, k, agg).len() <= slots);
                }
                let id = r.buckets(slots, 1, Agg::Mean);
                let raw: Vec<f32> = r.last_n(slots).collect();
                prop_assert_eq!(id.len(), raw.len());
                for (a, b) in id.iter().zip(&raw) {
                    prop_assert_eq!(a.to_bits(), b.to_bits());
                }
            }
        }
    }

    fn app() -> App {
        App::new(tu::soc(), Config::default())
    }

    #[test]
    fn apply_fast_pushes_every_ring() {
        let mut app = app();
        app.apply(Update::Fast(Box::new(tu::fast_at(3))));
        let h = &app.hist;
        assert!(h.cpu_total.latest().is_some());
        assert_eq!(h.per_core.len(), 16);
        assert!(h.per_core.iter().all(|r| r.latest().is_some()));
        for r in [
            &h.gpu,
            &h.mem_used,
            &h.net_rx,
            &h.net_tx,
            &h.disk_rd,
            &h.disk_wr,
        ] {
            assert!(r.latest().is_some());
        }
        assert!(app.fast.cpu.is_some(), "snapshot retained for panels");
    }

    #[test]
    fn apply_stamps_the_motion_clock_per_tier() {
        use crate::ui::motion::Tier;
        let mut app = app();
        for t in [Tier::Fast, Tier::Power, Tier::Temps] {
            assert!(app.motion_clock.last(t).is_none(), "fresh app: no stamps");
        }
        app.apply(Update::Fast(Box::new(tu::fast_at(0))));
        assert!(app.motion_clock.last(Tier::Fast).is_some());
        assert!(app.motion_clock.last(Tier::Power).is_none());
        app.apply(Update::Power(Box::new(tu::power_at(0))));
        assert!(app.motion_clock.last(Tier::Power).is_some());
        // A Slow update without temps (battery-only) must not stamp temps.
        app.apply(Update::Slow(Box::new(SlowSnapshot {
            temps: None,
            battery: Some(tu::battery()),
        })));
        assert!(app.motion_clock.last(Tier::Temps).is_none());
        app.apply(Update::Slow(Box::new(SlowSnapshot {
            temps: Some(tu::temps_at(0)),
            battery: None,
        })));
        assert!(app.motion_clock.last(Tier::Temps).is_some());
    }

    #[test]
    fn series_is_buckets_when_motion_is_off() {
        use crate::ui::motion::Tier;
        let mut app = tu::app(); // fixture: motion pinned off, rings full
        app.motion_clock.fast = Some(std::time::Instant::now());
        app.frame_now = std::time::Instant::now();
        let ring = &app.hist.cpu_total;
        assert_eq!(
            app.series(ring, 40, Agg::Max, Tier::Fast),
            ring.buckets(40, app.graph_k(), Agg::Max),
            "motion off is a pure buckets passthrough"
        );
        // Motion on but settled (stamp older than the interval) is also
        // exactly buckets — the honesty-at-rest contract end to end.
        app.config.motion = true;
        app.motion_clock.fast = app
            .frame_now
            .checked_sub(std::time::Duration::from_secs(60));
        assert_eq!(
            app.series(&app.hist.cpu_total, 40, Agg::Max, Tier::Fast),
            app.hist.cpu_total.buckets(40, app.graph_k(), Agg::Max),
        );
        // Mid-tick, the interpolated head differs only in the last slot at
        // most — and stays inside the ring's value range.
        app.motion_clock.fast = app
            .frame_now
            .checked_sub(std::time::Duration::from_millis(50));
        let eased = app.series(&app.hist.cpu_total, 40, Agg::Max, Tier::Fast);
        assert_eq!(eased.len(), 40);
    }

    #[test]
    fn series_span_is_the_raw_scale_basis() {
        let mut app = tu::app(); // fixture: motion pinned off
        let ring = &app.hist.cpu_total;
        let k = app.graph_k();
        // Motion off: identical to the drawn buckets — scale == old behavior.
        assert_eq!(
            app.series_span(ring, 40, Agg::Max),
            ring.buckets(40, k, Agg::Max)
        );
        // Motion on: the conveyor's full source window (one extra bucket),
        // independent of phase — so an axis built on it never moves between
        // ticks, and max(span) bounds every convex blend the drift draws.
        app.config.motion = true;
        let span = app.series_span(&app.hist.cpu_total, 40, Agg::Max);
        assert_eq!(span, app.hist.cpu_total.buckets(41, k, Agg::Max));
        app.motion_clock.fast = Some(std::time::Instant::now());
        app.frame_now = std::time::Instant::now();
        let drawn = app.series(
            &app.hist.cpu_total,
            40,
            Agg::Max,
            crate::ui::motion::Tier::Fast,
        );
        let bound = span.iter().copied().fold(f32::MIN, f32::max);
        assert!(
            drawn.iter().all(|&v| v <= bound + 1e-3),
            "span bounds blend"
        );
    }

    #[test]
    fn apply_power_pushes_rails() {
        let mut app = app();
        app.apply(Update::Power(Box::new(tu::power_at(1))));
        let h = &app.hist;
        for r in [
            &h.package_w,
            &h.cpu_w,
            &h.gpu_w,
            &h.ane_w,
            &h.dram_w,
            &h.disp_w,
            &h.ecpu_usage,
            &h.pcpu_usage,
        ] {
            assert!(r.latest().is_some());
        }
        assert!(app.power.is_some());
    }

    #[test]
    fn apply_slow_keeps_last_battery_and_bumps_temps_seq() {
        let mut app = app();
        app.apply(Update::Slow(Box::new(SlowSnapshot {
            temps: Some(tu::temps_at(0)),
            battery: Some(tu::battery()),
        })));
        assert_eq!(app.temps_seq, 1);
        assert!(app.battery.is_some());
        assert!(app.hist.sys_w.latest().is_some(), "PSTR feeds the SYS ring");
        // A temps-only tick must not drop the last battery reading…
        app.apply(Update::Slow(Box::new(SlowSnapshot {
            temps: Some(tu::temps_at(4)),
            battery: None,
        })));
        assert!(app.battery.is_some());
        assert_eq!(app.temps_seq, 2);
        // …and a temps-less tick must not bump the render-cache key.
        app.apply(Update::Slow(Box::new(SlowSnapshot {
            temps: None,
            battery: None,
        })));
        assert_eq!(app.temps_seq, 2);
    }

    #[test]
    fn apply_ping_miss_records_nan_gap() {
        let mut app = app();
        let mut miss = tu::ping_at(0);
        miss.rtt_ms = None;
        app.apply(Update::Ping(Box::new(miss)));
        assert!(app.hist.ping_ms.latest().unwrap().is_nan());
        app.apply(Update::Ping(Box::new(tu::ping_at(1))));
        assert!(!app.hist.ping_ms.latest().unwrap().is_nan());
    }

    #[test]
    fn apply_flows_resorts_only_under_net_sort() {
        let mut app = app();
        app.apply(Update::Procs(Box::new(tu::procs(10))));
        let before = app.visible_rows.clone();
        app.apply(Update::Flows(Box::new(tu::flows())));
        assert_eq!(app.visible_rows, before, "CPU sort ignores flow churn");
        app.sort = SortKey::Net;
        app.apply(Update::Flows(Box::new(tu::flows())));
        // The busiest flow among visible rows (Safari, pid 251) leads, and
        // the whole column is ordered by per-pid rx+tx.
        assert_eq!(app.procs.rows[app.visible_rows[0]].pid, 251);
        let net = |pid: i32| app.flows.by_pid.get(&pid).map_or(0, |&(rx, tx)| rx + tx);
        assert!(
            app.visible_rows
                .windows(2)
                .all(|w| net(app.procs.rows[w[0]].pid) >= net(app.procs.rows[w[1]].pid))
        );
    }

    #[test]
    fn apply_source_down_accumulates() {
        let mut app = app();
        app.apply(Update::SourceDown {
            source: "power",
            error: "no IOReport".into(),
        });
        app.apply(Update::SourceDown {
            source: "flows",
            error: "socket".into(),
        });
        assert_eq!(app.source_errors.len(), 2);
        assert_eq!(app.source_errors[0].0, "power");
    }

    #[test]
    fn refresh_visible_filters_name_pid_and_user() {
        let mut app = app();
        app.apply(Update::Procs(Box::new(tu::procs(32))));
        app.filter = "safari".into();
        app.refresh_visible();
        assert!(!app.visible_rows.is_empty());
        assert!(
            app.visible_rows
                .iter()
                .all(|&i| app.procs.rows[i].name == "Safari")
        );
        // Exact-pid match.
        app.filter = "200".into();
        app.refresh_visible();
        assert_eq!(app.visible_rows.len(), 1);
        assert_eq!(app.procs.rows[app.visible_rows[0]].pid, 200);
        // User substring, case-insensitive.
        app.filter = "_WindowServer".to_lowercase();
        app.refresh_visible();
        assert!(
            app.visible_rows
                .iter()
                .all(|&i| app.procs.rows[i].user == "_windowserver")
        );
        // No match: selection has nothing to sit on.
        app.filter = "zzz-no-such".into();
        app.refresh_visible();
        assert!(app.visible_rows.is_empty());
        assert!(app.selected_row().is_none());
    }

    #[test]
    fn refresh_visible_sorts_and_sinks_unreadable_rows() {
        let mut app = app();
        app.apply(Update::Procs(Box::new(tu::procs(24))));
        // Default: CPU descending, unreadable (None) rows sink to the bottom.
        let cpus: Vec<Option<f32>> = app
            .visible_rows
            .iter()
            .map(|&i| app.procs.rows[i].cpu.map(|c| c.0))
            .collect();
        let split = cpus.iter().position(Option::is_none).unwrap_or(cpus.len());
        assert!(split > 0, "fixture has readable rows");
        assert!(cpus[..split].windows(2).all(|w| w[0] >= w[1]));
        assert!(cpus[split..].iter().all(Option::is_none));

        let row = |app: &App, v: usize| app.procs.rows[app.visible_rows[v]].clone();
        // Name starts ascending (case-insensitive), Pid ascending.
        for key in [SortKey::Name, SortKey::User, SortKey::Pid] {
            app.sort = key;
            app.sort_desc = false;
            app.refresh_visible();
        }
        assert!(
            app.visible_rows
                .windows(2)
                .all(|w| app.procs.rows[w[0]].pid <= app.procs.rows[w[1]].pid)
        );
        // Memory, Power, Threads: descending with None/0 last.
        app.sort = SortKey::Memory;
        app.sort_desc = true;
        app.refresh_visible();
        let m0 = row(&app, 0).memory.unwrap();
        assert!(
            app.visible_rows
                .iter()
                .all(|&i| app.procs.rows[i].memory.unwrap_or_default() <= m0)
        );
        app.sort = SortKey::Threads;
        app.refresh_visible();
        let t0 = row(&app, 0).threads.unwrap();
        assert!(
            app.visible_rows
                .iter()
                .all(|&i| app.procs.rows[i].threads.unwrap_or(0) <= t0)
        );
    }

    #[test]
    fn selection_movement_clamps() {
        let mut app = app();
        app.move_selection(5); // empty list: no panic, stays put
        assert_eq!(app.selected, 0);
        app.apply(Update::Procs(Box::new(tu::procs(5))));
        app.move_selection(1000);
        assert_eq!(app.selected, 4);
        app.move_selection(-1000);
        assert_eq!(app.selected, 0);
        // A shrinking table clamps a stale selection.
        app.selected = 4;
        app.apply(Update::Procs(Box::new(tu::procs(2))));
        assert_eq!(app.selected, 1);
        assert_eq!(
            app.selected_row().unwrap().pid,
            app.procs.rows[app.visible_rows[1]].pid
        );
    }
}
