//! Kernel-side activity: which hardware is interrupting the CPU, and what is
//! holding the machine awake.
//!
//! Both are slow-moving facts that ride the health tier. The interrupt group
//! is large (over a thousand channels), so it is aggregated to a ranked
//! handful here rather than handed to the UI raw.

use crate::ffi::iopm::{self, Assertion};
use crate::ffi::ioreport::IoReport;

/// How many interrupt sources the UI ever shows. The group has ~1245
/// channels; past the top few the rest are noise.
const TOP_SOURCES: usize = 6;

/// One hardware block's interrupt activity over the window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InterruptSource {
    /// The device the interrupts came from, e.g. `gfx-asc`, `ans`, `usb-drd0`.
    pub device: String,
    pub per_sec: f64,
    /// Time the handlers spent servicing them, as a share of the window.
    /// This is the part that actually costs CPU.
    pub cpu_share: f64,
}

#[derive(Debug, Clone, Default)]
pub struct KernelSnapshot {
    /// Busiest interrupt sources, most active first.
    pub top_sources: Vec<InterruptSource>,
    pub total_per_sec: f64,
    /// Everything currently holding the machine awake.
    pub assertions: Vec<Assertion>,
}

impl KernelSnapshot {
    /// Assertions that actually prevent sleep, deduplicated by owner and kind
    /// — one process holding the same lock repeatedly is one reason, not six.
    pub fn sleep_blockers(&self) -> Vec<&Assertion> {
        let mut seen = std::collections::HashSet::new();
        self.assertions
            .iter()
            .filter(|a| a.prevents_sleep())
            .filter(|a| seen.insert((a.pid, a.kind.as_str())))
            .collect()
    }
}

fn channel_filter(group: &str, _subgroup: &str, _name: &str) -> bool {
    group == "Interrupt Statistics (by index)"
}

/// The `MATU` unit the handler-time channels are denominated in: the same
/// 24 MHz timebase every other IOReport residency uses.
const TICKS_PER_SEC: f64 = 24_000_000.0;

pub struct KernelCollector {
    report: Option<IoReport>,
}

impl KernelCollector {
    pub fn new() -> Self {
        Self {
            report: IoReport::subscribe(channel_filter).ok(),
        }
    }

    pub fn sample(&mut self) -> KernelSnapshot {
        let mut out = KernelSnapshot {
            assertions: iopm::assertions(),
            ..Default::default()
        };
        let Some(report) = self.report.as_mut() else {
            return out;
        };
        // device → (count, handler ticks)
        let mut by_device: std::collections::HashMap<String, (i64, i64)> =
            std::collections::HashMap::new();
        let mut window_ms = 0u64;
        let visited = report.visit_delta(|dt_ms, item| {
            window_ms = dt_ms;
            // The subgroup names the device; the channel says whether this is
            // a count or the time spent handling them.
            let device = item.id.subgroup.clone();
            if device.is_empty() {
                return;
            }
            let entry = by_device.entry(device).or_default();
            let value = item.integer_value();
            if item.id.name.contains("Count") {
                entry.0 = entry.0.saturating_add(value);
            } else if item.id.name.contains("Time") {
                entry.1 = entry.1.saturating_add(value);
            }
        });
        if !matches!(visited, Ok(Some(_))) {
            return out;
        }
        let (sources, total) = rank(by_device, window_ms);
        out.top_sources = sources;
        out.total_per_sec = total;
        out
    }
}

/// Turn raw per-device `(count, handler ticks)` totals into the ranked top
/// sources and the system-wide rate.
///
/// Pure, so the ranking rule is testable without an IOReport subscription:
/// order by **handler cost**, not raw count — a thousand cheap interrupts
/// matter less than a few expensive ones — and keep only the busiest few, since
/// the group publishes over a thousand channels.
fn rank(
    by_device: std::collections::HashMap<String, (i64, i64)>,
    window_ms: u64,
) -> (Vec<InterruptSource>, f64) {
    // A window of no length measured nothing. Guarding the divisor with a
    // tiny epsilon instead would turn any count into an astronomical rate —
    // absence has to stay absence.
    if window_ms == 0 {
        return (Vec::new(), 0.0);
    }
    let secs = window_ms as f64 / 1000.0;
    let mut sources: Vec<InterruptSource> = by_device
        .into_iter()
        .filter(|(_, (count, _))| *count > 0)
        .map(|(device, (count, ticks))| InterruptSource {
            device,
            per_sec: count as f64 / secs,
            cpu_share: (ticks as f64 / TICKS_PER_SEC) / secs,
        })
        .collect();
    let total = sources.iter().map(|s| s.per_sec).sum();
    sources.sort_by(|a, b| {
        b.cpu_share
            .total_cmp(&a.cpu_share)
            .then_with(|| b.per_sec.total_cmp(&a.per_sec))
    });
    sources.truncate(TOP_SOURCES);
    (sources, total)
}

#[cfg(test)]
mod tests {
    use super::{Assertion, KernelSnapshot, TICKS_PER_SEC, TOP_SOURCES, rank};
    use std::collections::HashMap;

    fn totals(pairs: &[(&str, i64, i64)]) -> HashMap<String, (i64, i64)> {
        pairs
            .iter()
            .map(|&(d, c, t)| (d.to_owned(), (c, t)))
            .collect()
    }

    #[test]
    fn rates_are_per_second_over_the_window() {
        // 2000 interrupts in 500 ms is 4000/s; half a second of handler time
        // in a half-second window is a 100% share of one core.
        let (sources, total) = rank(
            totals(&[("ans 4", 2000, (TICKS_PER_SEC / 2.0) as i64)]),
            500,
        );
        assert_eq!(sources.len(), 1);
        assert!(
            (sources[0].per_sec - 4000.0).abs() < 1e-6,
            "{:?}",
            sources[0]
        );
        assert!(
            (sources[0].cpu_share - 1.0).abs() < 1e-6,
            "{:?}",
            sources[0]
        );
        assert!((total - 4000.0).abs() < 1e-6);
    }

    #[test]
    fn ranking_prefers_expensive_handlers_over_chatty_ones() {
        let (sources, _) = rank(
            totals(&[
                ("chatty", 100_000, 10),
                ("expensive", 5, 1_000_000),
                ("middling", 500, 500),
            ]),
            1000,
        );
        assert_eq!(sources[0].device, "expensive");
        assert_eq!(sources[1].device, "middling");
        assert_eq!(sources[2].device, "chatty");
    }

    #[test]
    fn silent_devices_are_dropped_and_the_list_is_capped() {
        let many: Vec<(&str, i64, i64)> = vec![
            ("a", 0, 0),
            ("b", 1, 1),
            ("c", 2, 2),
            ("d", 3, 3),
            ("e", 4, 4),
            ("f", 5, 5),
            ("g", 6, 6),
            ("h", 7, 7),
            ("i", 8, 8),
        ];
        let (sources, _) = rank(totals(&many), 1000);
        // "a" never fired, and the group is far too large to render whole.
        assert_eq!(sources.len(), TOP_SOURCES);
        assert!(!sources.iter().any(|s| s.device == "a"));
    }

    #[test]
    fn a_zero_length_window_reports_nothing_rather_than_infinity() {
        // Dividing a real count by an epsilon window yields ~1e308, which
        // renders as a nonsense rate. No window means no measurement.
        let (sources, total) = rank(totals(&[("ans", 10, 10)]), 0);
        assert!(sources.is_empty());
        assert!(total.abs() < f64::EPSILON);
    }

    #[test]
    fn no_devices_is_an_empty_ranking() {
        let (sources, total) = rank(HashMap::new(), 1000);
        assert!(sources.is_empty());
        assert!(total.abs() < f64::EPSILON);
    }

    fn assertion(pid: i32, kind: &str) -> Assertion {
        Assertion {
            pid,
            kind: kind.into(),
            name: None,
        }
    }

    #[test]
    fn sleep_blockers_ignore_assertions_that_do_not_block() {
        let snap = KernelSnapshot {
            assertions: vec![
                assertion(1, "UserIsActive"),
                assertion(42, "PreventUserIdleSystemSleep"),
            ],
            ..Default::default()
        };
        let blockers = snap.sleep_blockers();
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].pid, 42);
    }

    #[test]
    fn one_process_holding_the_same_lock_twice_is_one_reason() {
        let snap = KernelSnapshot {
            assertions: vec![
                assertion(7, "PreventSystemSleep"),
                assertion(7, "PreventSystemSleep"),
                assertion(7, "PreventUserIdleSystemSleep"),
                assertion(9, "PreventSystemSleep"),
            ],
            ..Default::default()
        };
        // Same pid + same kind collapses; a different kind or pid does not.
        assert_eq!(snap.sleep_blockers().len(), 3);
    }

    #[test]
    fn nothing_holding_the_machine_awake_is_an_empty_list() {
        assert!(KernelSnapshot::default().sleep_blockers().is_empty());
    }
}
