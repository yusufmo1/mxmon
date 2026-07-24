//! The shared settle/collect spine: spin the real sampler, fold the tiered
//! `Update` stream into a latest-per-tier snapshot, gate until every settle-
//! tier has reported enough times (or a deadline passes), then stop the
//! threads. Every read subcommand samples through here, so the settle semantics
//! live in one place rather than being re-derived per command.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::collect::battery::BatterySample;
use crate::collect::flows::FlowSample;
use crate::collect::kernel::KernelSnapshot;
use crate::collect::ping::PingSample;
use crate::collect::power::PowerSample;
use crate::collect::procs::ProcSample;
use crate::collect::sampler::{self, Control, FastSnapshot, SlowSnapshot, Update};
use crate::collect::soc::SocInfo;
use crate::collect::storage::StorageSample;
use crate::report;

/// The latest sample per tier, folded from the `Update` stream. The same
/// Option-per-tier shape `App` holds, without the history rings the headless
/// surface never needs.
#[derive(Default)]
pub struct Latest {
    pub fast: Option<Box<FastSnapshot>>,
    pub power: Option<Box<PowerSample>>,
    pub slow: Option<Box<SlowSnapshot>>,
    pub procs: Option<Box<ProcSample>>,
    pub ping: Option<Box<PingSample>>,
    pub flows: Option<Box<FlowSample>>,
    /// Battery rides the slow tier alongside temps; preserved across a
    /// temps-only `Slow` update so a later one does not drop it.
    pub battery: Option<BatterySample>,
    pub health: Option<Box<StorageSample>>,
    pub kernel: Option<Box<KernelSnapshot>>,
    pub errors: Vec<(String, String)>,
    down: HashSet<String>,
    n_fast: u32,
    n_power: u32,
    n_slow: u32,
    n_procs: u32,
    n_flows: u32,
}

impl Latest {
    /// Fold one update in. Mirrors `App::apply`'s tier assignment, minus the
    /// history rings.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Fast(s) => {
                self.n_fast += 1;
                self.fast = Some(s);
            }
            Update::Power(s) => {
                self.n_power += 1;
                self.power = Some(s);
            }
            Update::Slow(s) => {
                self.n_slow += 1;
                if s.battery.is_some() {
                    self.battery.clone_from(&s.battery);
                }
                self.slow = Some(s);
            }
            Update::Procs(s) => {
                self.n_procs += 1;
                self.procs = Some(s);
            }
            Update::Ping(s) => self.ping = Some(s),
            Update::Health(s) => self.health = Some(s),
            Update::Kernel(s) => self.kernel = Some(s),
            Update::Flows(s) => {
                self.n_flows += 1;
                self.flows = Some(s);
            }
            Update::SourceDown { source, error } => {
                self.down.insert(source.to_string());
                self.errors.push((source.to_string(), error));
            }
        }
    }

    /// Whether every settle-gated tier has reported at least `n` times. A tier
    /// that reported `SourceDown` is exempt (it will never report twice). Ping,
    /// health, and kernel tiers are deliberately not gated: their intervals
    /// exceed the settle budget, so they ride along with whatever they produced.
    pub fn settled(&self, n: u32) -> bool {
        let ok = |count: u32, source: &str| count >= n || self.down.contains(source);
        self.n_fast >= n
            && ok(self.n_power, "power")
            && self.n_slow >= n
            && self.n_procs >= n
            && ok(self.n_flows, "flows")
    }

    /// Borrow the folded state as report [`Inputs`](report::Inputs).
    pub fn inputs<'a>(
        &'a self,
        soc: &'a SocInfo,
        fast_ms: u64,
        features: Features,
        settled: bool,
    ) -> report::Inputs<'a> {
        report::Inputs {
            soc,
            fast: self.fast.as_deref(),
            power: self.power.as_deref(),
            temps: self.slow.as_ref().and_then(|s| s.temps.as_ref()),
            battery: self.battery.as_ref(),
            procs: self.procs.as_deref(),
            flows: self.flows.as_deref(),
            ping: self.ping.as_deref(),
            storage: self.health.as_deref(),
            kernel: self.kernel.as_deref(),
            errors: &self.errors,
            fast_ms,
            ping_on: features.ping,
            storage_health_on: features.storage_health,
            kernel_stats_on: features.kernel_stats,
            settled,
        }
    }
}

/// Which optional collectors are enabled for a run.
#[derive(Debug, Clone, Copy)]
pub struct Features {
    pub ping: bool,
    pub storage_health: bool,
    pub kernel_stats: bool,
}

/// How to settle: the delta window, the deadline, how many reports per tier
/// count as settled, and which collectors to enable.
pub struct SettleOpts {
    pub fast_ms: u64,
    pub deadline: Duration,
    pub min_reports: u32,
    pub ping_on: bool,
    pub storage_health_on: bool,
    pub kernel_stats_on: bool,
    pub ping_host: String,
}

/// The outcome of a settle pass.
pub struct Settled {
    pub latest: Latest,
    /// True when the deadline was hit before every gated tier settled.
    pub timed_out: bool,
}

/// Spin the sampler, gate until settled or the deadline passes, stop the
/// threads, and return the folded snapshot.
pub fn settle(soc: &SocInfo, opts: &SettleOpts) -> Settled {
    let control = Control::new();
    control.fast_ms.store(opts.fast_ms, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    sampler::spawn(
        soc.clone(),
        Arc::clone(&control),
        tx,
        opts.ping_on.then(|| opts.ping_host.clone()),
        opts.storage_health_on,
        opts.kernel_stats_on,
    );

    let mut latest = Latest::default();
    let deadline = Instant::now() + opts.deadline;
    let mut timed_out = true;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(update) => latest.apply(update),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if latest.settled(opts.min_reports) {
            timed_out = false;
            break;
        }
    }
    control.shutdown.store(true, Ordering::Relaxed);
    Settled { latest, timed_out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_gate_exempts_downed_sources() {
        let mut l = Latest::default();
        // Fast, slow, and procs each reported twice; power and flows are down.
        for _ in 0..2 {
            l.apply(Update::Fast(Box::default()));
            l.apply(Update::Slow(Box::default()));
            l.apply(Update::Procs(Box::default()));
        }
        assert!(!l.settled(2), "not settled while power/flows are pending");
        l.apply(Update::SourceDown {
            source: "power",
            error: "no ioreport".into(),
        });
        l.apply(Update::SourceDown {
            source: "flows",
            error: "no ntstat".into(),
        });
        assert!(l.settled(2), "downed tiers are exempt from the gate");
        assert_eq!(l.errors.len(), 2);
    }
}
