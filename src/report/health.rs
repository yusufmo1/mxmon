//! A composite health verdict folded from the report's own signals: thermal
//! pressure, SMART, controller throttle, memory pressure, battery wear, and
//! sleep blockers. Deterministic and pure, so every rule is unit-testable. A
//! domain whose source is null is reported as `unavailable` and does not drive
//! the overall status: you cannot fail a check you could not run.

use schemars::JsonSchema;
use serde::Serialize;

use super::model::Report;

/// Named thresholds, in one auditable place.
mod thresholds {
    pub const ENDURANCE_WARN: f64 = 0.90; // SMART used_ratio
    pub const THROTTLE_WARN: f64 = 0.05; // controller throttled_ratio
    pub const THROTTLE_CRIT: f64 = 0.25;
    pub const BATTERY_HEALTH_WARN: f64 = 0.80;
    pub const CYCLE_WARN: f64 = 0.80; // cycle_ratio
    pub const IMBALANCE_MV: u64 = 100; // cell spread
}

#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct Health {
    /// The worst assessable domain.
    pub status: Status,
    /// True when at least one domain could not be assessed (a null source).
    pub partial: bool,
    /// Per-domain findings, worst first.
    pub findings: Vec<Finding>,
}

#[derive(Serialize, JsonSchema, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Crit,
}

#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct Finding {
    pub domain: String,
    pub status: Status,
    pub summary: String,
    pub detail: Option<String>,
    /// True when the source was null, so this finding does not affect `status`.
    pub unavailable: bool,
}

/// Internal 4-level severity; `Note` is surfaced but folds to `Ok` for the
/// top-line status so it does not cry wolf.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sev {
    Ok,
    Note,
    Warn,
    Crit,
}

impl Sev {
    fn rank(self) -> u8 {
        match self {
            Sev::Ok => 0,
            Sev::Note => 1,
            Sev::Warn => 2,
            Sev::Crit => 3,
        }
    }

    fn to_status(self) -> Status {
        match self {
            Sev::Ok | Sev::Note => Status::Ok,
            Sev::Warn => Status::Warn,
            Sev::Crit => Status::Crit,
        }
    }
}

fn finding(domain: &str, sev: Sev, summary: String, detail: Option<String>) -> (Sev, Finding) {
    (
        sev,
        Finding {
            domain: domain.to_owned(),
            status: sev.to_status(),
            summary,
            detail,
            unavailable: false,
        },
    )
}

fn unavailable(domain: &str) -> (Sev, Finding) {
    (
        Sev::Ok,
        Finding {
            domain: domain.to_owned(),
            status: Status::Ok,
            summary: format!("{domain} source unavailable"),
            detail: None,
            unavailable: true,
        },
    )
}

/// Assess the machine's health from a settled report.
pub fn assess(r: &Report) -> Health {
    use thresholds::{
        BATTERY_HEALTH_WARN, CYCLE_WARN, ENDURANCE_WARN, IMBALANCE_MV, THROTTLE_CRIT, THROTTLE_WARN,
    };
    let mut items: Vec<(Sev, Finding)> = Vec::new();

    // Thermal.
    if let Some(t) = &r.thermal {
        let p = t.pressure.as_deref().unwrap_or("unknown");
        let (sev, summary) = match p {
            "nominal" | "light" => (Sev::Ok, format!("thermal pressure {p}")),
            "moderate" | "heavy" => (Sev::Warn, format!("thermal pressure {p}: the SoC is throttling")),
            "trapping" | "sleeping" => (Sev::Crit, format!("thermal pressure {p}: severe throttling")),
            other => (Sev::Note, format!("thermal pressure {other}")),
        };
        let detail = Some(format!(
            "cpu_max {:.1}C, throttling {}",
            t.cpu_max_c,
            t.throttling.unwrap_or(false)
        ));
        items.push(finding("thermal", sev, summary, detail));
    } else {
        items.push(unavailable("thermal"));
    }

    // Storage / SMART.
    if let Some(st) = &r.storage {
        if let Some(sm) = &st.smart {
            let (sev, summary) = if sm.unhealthy {
                (
                    Sev::Crit,
                    "SMART fault: critical warning, media errors, or spare below threshold".to_owned(),
                )
            } else if sm.used_ratio >= ENDURANCE_WARN {
                (
                    Sev::Warn,
                    format!("drive at {:.0}% of rated write endurance", sm.used_ratio * 100.0),
                )
            } else {
                (Sev::Ok, "drive healthy".to_owned())
            };
            items.push(finding(
                "storage",
                sev,
                summary,
                Some(format!(
                    "used {:.0}%, spare {:.0}%",
                    sm.used_ratio * 100.0,
                    sm.available_spare_ratio * 100.0
                )),
            ));
        }
        if let Some(thr) = st.controller.throttled_ratio {
            let sev = if thr > THROTTLE_CRIT {
                Sev::Crit
            } else if thr > THROTTLE_WARN {
                Sev::Warn
            } else {
                Sev::Ok
            };
            if sev != Sev::Ok {
                items.push(finding(
                    "controller",
                    sev,
                    format!("SSD controller throttled {:.0}% of the window", thr * 100.0),
                    None,
                ));
            }
        }
    } else {
        items.push(unavailable("storage"));
    }

    // Memory.
    if let Some(m) = &r.memory {
        let (sev, summary) = match m.pressure.as_str() {
            "normal" => (Sev::Ok, "memory pressure normal".to_owned()),
            "warning" => (Sev::Warn, "memory pressure warning".to_owned()),
            "critical" => (Sev::Crit, "memory pressure critical".to_owned()),
            other => (Sev::Note, format!("memory pressure {other}")),
        };
        items.push(finding(
            "memory",
            sev,
            summary,
            Some(format!("used {:.0}%", m.used_ratio * 100.0)),
        ));
    } else {
        items.push(unavailable("memory"));
    }

    // Battery wear (absent on desktops; no finding then, not "unavailable").
    if let Some(b) = &r.battery {
        let mut sev = Sev::Ok;
        let mut notes: Vec<String> = Vec::new();
        if b.health_ratio < BATTERY_HEALTH_WARN {
            sev = Sev::Warn;
            notes.push(format!("health {:.0}%", b.health_ratio * 100.0));
        }
        if b.cycle_ratio.is_some_and(|cr| cr > CYCLE_WARN) {
            sev = Sev::Warn;
            notes.push(format!("{} cycles", b.cycle_count));
        }
        if b.cell_imbalance_mv.is_some_and(|mv| mv > IMBALANCE_MV) {
            sev = Sev::Warn;
            notes.push(format!("cell imbalance {}mV", b.cell_imbalance_mv.unwrap_or(0)));
        }
        let summary = if sev == Sev::Ok {
            "battery healthy".to_owned()
        } else {
            format!("battery wear: {}", notes.join(", "))
        };
        items.push(finding("battery", sev, summary, None));
    }

    // Sleep blockers (a note; escalates to warn only while on battery).
    if let Some(k) = &r.kernel
        && let Some(blockers) = &k.sleep_blockers
        && !blockers.is_empty()
    {
        let on_battery = r.battery.as_ref().is_some_and(|b| !b.external_power);
        let sev = if on_battery { Sev::Warn } else { Sev::Note };
        let summary = if on_battery {
            format!("{} process(es) keeping the machine awake on battery", blockers.len())
        } else {
            format!("{} sleep blocker(s)", blockers.len())
        };
        let mut kinds: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for b in blockers {
            *kinds.entry(b.kind.as_str()).or_default() += 1;
        }
        let who = kinds
            .iter()
            .map(|(kind, n)| if *n > 1 { format!("{kind} x{n}") } else { (*kind).to_owned() })
            .collect::<Vec<_>>()
            .join(", ");
        items.push(finding("kernel", sev, summary, Some(who)));
    }

    let overall = items
        .iter()
        .filter(|(_, f)| !f.unavailable)
        .map(|(sev, _)| *sev)
        .max_by_key(|s| s.rank())
        .unwrap_or(Sev::Ok)
        .to_status();
    let partial = items.iter().any(|(_, f)| f.unavailable);

    let mut findings: Vec<Finding> = items.into_iter().map(|(_, f)| f).collect();
    findings.sort_by_key(|f| std::cmp::Reverse(status_rank(f.status)));

    Health {
        status: overall,
        partial,
        findings,
    }
}

fn status_rank(s: Status) -> u8 {
    match s {
        Status::Ok => 0,
        Status::Warn => 1,
        Status::Crit => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report_with(thermal_pressure: &str, mem_pressure: &str) -> Report {
        let v = json!({
            "meta": {"schema_version": 1, "mxmon_version": "0", "generated_unix": 0, "settled": true,
                     "sample_window": {"fast_ms": 250, "power_ms": 500, "procs_ms": 2000, "flows_ms": 1000},
                     "features": {"ping": false, "storage_health": true, "kernel_stats": true}},
            "soc": {"chip": "x", "macos_version": "1", "ecpu_cores": 0, "pcpu_cores": 0,
                    "cores_per_pcluster": 0, "gpu_cores": null, "memory_bytes": 0,
                    "ecpu_freqs_mhz": [], "pcpu_freqs_mhz": [], "gpu_freqs_mhz": [],
                    "tier_low": "E", "tier_high": "P"},
            "cpu": null, "gpu": null,
            "memory": {"total_bytes": 1, "used_bytes": 0, "app_bytes": 0, "wired_bytes": 0,
                       "compressed_bytes": 0, "cached_bytes": 0, "swap_used_bytes": 0,
                       "swap_total_bytes": 0, "used_ratio": 0.5, "pressure": mem_pressure},
            "power": null,
            "thermal": {"cpu_avg_c": 40.0, "cpu_max_c": 60.0, "gpu_avg_c": 40.0, "gpu_max_c": 40.0,
                        "pressure": thermal_pressure, "throttling": thermal_pressure != "nominal",
                        "severity": 0.0, "sys_power_w": null, "adapter_power_w": null,
                        "backlight_power_w": null, "sensors": [], "fans": []},
            "network": null, "disk": null, "storage": null, "battery": null,
            "processes": null, "flows": null, "kernel": null, "ping": null,
            "source_errors": []
        });
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn nominal_is_ok_moderate_is_warn_critical_mem_is_crit() {
        assert_eq!(assess(&report_with("nominal", "normal")).status, Status::Ok);
        assert_eq!(assess(&report_with("moderate", "normal")).status, Status::Warn);
        assert_eq!(assess(&report_with("nominal", "critical")).status, Status::Crit);
    }

    #[test]
    fn null_storage_is_partial_not_a_failure() {
        let h = assess(&report_with("nominal", "normal"));
        assert!(h.partial, "storage was null");
        assert_eq!(h.status, Status::Ok, "an unavailable domain must not drive status");
        assert!(h.findings.iter().any(|f| f.domain == "storage" && f.unavailable));
    }
}
