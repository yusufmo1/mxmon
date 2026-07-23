//! Deterministic, templated diagnosis of one topic. No model in the loop, so
//! the output is trustworthy and testable: it reads the settled report plus the
//! health verdict and names the specific offending metric or process.

use schemars::JsonSchema;
use serde::Serialize;

use super::health::{Finding, Health};
use super::model::{Proc, Report};

/// A topic diagnosis: a human summary plus the structured findings behind it.
#[derive(Serialize, JsonSchema, Debug, Clone)]
pub struct Explanation {
    pub topic: String,
    pub summary: String,
    pub findings: Vec<Finding>,
}

/// Explain `topic` (one of thermal, power, slow, battery, network, disk).
pub fn explain(topic: &str, r: &Report, h: &Health) -> Explanation {
    let summary = match topic {
        "thermal" => thermal(r),
        "power" => power(r),
        "slow" => slow(r),
        "battery" => battery(r),
        "network" => network(r),
        "disk" => disk(r),
        other => format!("unknown topic {other:?}"),
    };
    let findings = h
        .findings
        .iter()
        .filter(|f| relevant(topic, &f.domain))
        .cloned()
        .collect();
    Explanation {
        topic: topic.to_owned(),
        summary,
        findings,
    }
}

fn relevant(topic: &str, domain: &str) -> bool {
    match topic {
        "thermal" => matches!(domain, "thermal" | "controller"),
        "battery" => domain == "battery",
        "slow" => matches!(domain, "memory" | "thermal"),
        _ => false,
    }
}

/// The busiest process by a chosen metric, as `(name, formatted value)`.
fn top_by(r: &Report, key: impl Fn(&Proc) -> f64) -> Option<(String, f64)> {
    r.processes.as_ref().and_then(|p| {
        p.top
            .iter()
            .max_by(|a, b| key(a).total_cmp(&key(b)))
            .map(|proc| (proc.name.clone(), key(proc)))
    })
}

fn thermal(r: &Report) -> String {
    let Some(t) = &r.thermal else {
        return "thermal source unavailable.".to_owned();
    };
    let verdict = t.pressure.as_deref().unwrap_or("unknown");
    let hottest = t
        .sensors
        .iter()
        .max_by(|a, b| a.temp_c.total_cmp(&b.temp_c))
        .map(|s| format!("; hottest sensor {} at {:.0}C", s.label, s.temp_c))
        .unwrap_or_default();
    if t.throttling == Some(true) {
        format!(
            "The SoC is throttling: thermal pressure {verdict}, CPU max {:.0}C{hottest}. Reduce sustained load or improve cooling.",
            t.cpu_max_c
        )
    } else {
        format!(
            "Thermals are healthy: pressure {verdict}, CPU max {:.0}C{hottest}.",
            t.cpu_max_c
        )
    }
}

fn power(r: &Report) -> String {
    let Some(p) = &r.power else {
        return "power source unavailable.".to_owned();
    };
    let hog = top_by(r, |proc| proc.power_w.unwrap_or(0.0))
        .filter(|(_, w)| *w > 0.0)
        .map(|(name, w)| format!(" Top consumer: {name} at {w:.2}W."))
        .unwrap_or_default();
    format!(
        "Package draw {:.1}W (cpu {:.1}, gpu {:.1}, ane {:.2}).{hog}",
        p.package_w, p.cpu_w, p.gpu_w, p.ane_w
    )
}

fn slow(r: &Report) -> String {
    let load = r.cpu.as_ref().map_or_else(
        || "load unknown".to_owned(),
        |c| format!("load {:.2}", c.load_avg[0]),
    );
    let hog = top_by(r, |proc| proc.cpu_ratio.unwrap_or(0.0))
        .map(|(name, ratio)| format!("; busiest process {name} at {:.0}% of a core", ratio * 100.0))
        .unwrap_or_default();
    let contention = r
        .processes
        .as_ref()
        .and_then(|p| p.top.iter().filter_map(|x| x.runnable).reduce(f64::max))
        .filter(|&run| run > 0.5)
        .map(|run| format!(". A process is spending {run:.1}s/s runnable but not running, so the system is CPU-bound"))
        .unwrap_or_default();
    format!("{load}{hog}{contention}.")
}

fn battery(r: &Report) -> String {
    let Some(b) = &r.battery else {
        return "No battery on this machine.".to_owned();
    };
    let state = if b.charging {
        "charging"
    } else if b.external_power {
        "on adapter"
    } else {
        "on battery"
    };
    let remaining = b
        .minutes_remaining
        .map(|m| format!(", {}h{:02}m remaining", m / 60, m % 60))
        .unwrap_or_default();
    format!(
        "Charge {:.0}% ({state}){remaining}. Health {:.0}% at {} cycles.",
        b.charge_ratio * 100.0,
        b.health_ratio * 100.0,
        b.cycle_count
    )
}

fn network(r: &Report) -> String {
    let link = r.network.as_ref().and_then(|n| n.primary.as_ref()).map_or_else(
        || "no primary interface".to_owned(),
        |p| format!("{} {}", p.name, if p.link_up { "up" } else { "down" }),
    );
    let reach = r
        .ping
        .as_ref()
        .map(|p| match p.latency_ms {
            Some(ms) if p.up => format!(", {} reachable at {ms:.0}ms", p.host),
            _ => format!(", {} unreachable", p.host),
        })
        .unwrap_or_default();
    let busiest = r
        .flows
        .as_ref()
        .and_then(|f| f.top.first())
        .map(|fl| format!(". Busiest flow: {} to {}", fl.name, fl.remote))
        .unwrap_or_default();
    format!("Link {link}{reach}{busiest}.")
}

fn disk(r: &Report) -> String {
    let Some(d) = &r.disk else {
        return "disk source unavailable.".to_owned();
    };
    let hog = top_by(r, |proc| {
        proc.disk_read_bytes_per_sec.unwrap_or(0) as f64
            + proc.disk_write_bytes_per_sec.unwrap_or(0) as f64
    })
    .filter(|(_, b)| *b > 0.0)
    .map(|(name, b)| format!(" Busiest: {name} at {:.0} KB/s.", b / 1000.0))
    .unwrap_or_default();
    format!(
        "Read {:.1} MB/s, write {:.1} MB/s, {:.0}% of the boot volume used.{hog}",
        d.read_bytes_per_sec as f64 / 1e6,
        d.write_bytes_per_sec as f64 / 1e6,
        d.capacity_used_ratio * 100.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_health() -> Health {
        Health {
            status: super::super::health::Status::Ok,
            partial: false,
            findings: vec![],
        }
    }

    #[test]
    fn unknown_topic_is_reported_not_panicked() {
        let r: Report = serde_json::from_value(serde_json::json!({
            "meta": {"schema_version": 1, "mxmon_version": "0", "generated_unix": 0, "settled": true,
                     "sample_window": {"fast_ms": 250, "power_ms": 500, "procs_ms": 2000, "flows_ms": 1000},
                     "features": {"ping": false, "storage_health": false, "kernel_stats": false}},
            "soc": {"chip": "x", "macos_version": "1", "ecpu_cores": 0, "pcpu_cores": 0,
                    "cores_per_pcluster": 0, "gpu_cores": null, "memory_bytes": 0,
                    "ecpu_freqs_mhz": [], "pcpu_freqs_mhz": [], "gpu_freqs_mhz": [],
                    "tier_low": "E", "tier_high": "P"},
            "cpu": null, "gpu": null, "memory": null, "power": null, "thermal": null,
            "network": null, "disk": null, "storage": null, "battery": null,
            "processes": null, "flows": null, "kernel": null, "ping": null, "source_errors": []
        })).unwrap();
        assert!(explain("thermal", &r, &empty_health()).summary.contains("unavailable"));
        assert!(explain("nope", &r, &empty_health()).summary.contains("unknown topic"));
    }
}
