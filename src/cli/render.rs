//! Headless renderers: the neon-styled human summary, an aligned table, and the
//! machine helpers (pretty JSON, one-line NDJSON). Human formatting wraps the
//! report's scalars back into [`crate::units`] newtypes so the terminal output
//! matches the TUI's typography exactly.

use crate::report::Report;
use crate::units::{Bytes, Celsius, Mhz, Ratio, Watts};

// ---- color ---------------------------------------------------------------

fn paint(color: bool, code: &str, s: &str) -> String {
    if color {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

pub fn title(c: bool, s: &str) -> String {
    paint(c, "1;38;2;255;45;149", s)
}
pub fn accent(c: bool, s: &str) -> String {
    paint(c, "38;2;0;229;255", s)
}
pub fn dim(c: bool, s: &str) -> String {
    paint(c, "38;2;105;105;130", s)
}
pub fn ok(c: bool, s: &str) -> String {
    paint(c, "38;2;0;230;118", s)
}
pub fn warn(c: bool, s: &str) -> String {
    paint(c, "38;2;255;179;0", s)
}
pub fn crit(c: bool, s: &str) -> String {
    paint(c, "38;2;255;82;82", s)
}

// ---- machine helpers -----------------------------------------------------

/// Serialize the report to a JSON value once; every selector and formatter
/// operates on this, so `mxmon get x.y` can never disagree with `jq .x.y`.
pub fn to_value(report: &Report) -> serde_json::Value {
    serde_json::to_value(report).unwrap_or(serde_json::Value::Null)
}

pub fn json_pretty(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".to_owned())
}

pub fn ndjson_line(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_owned())
}

// ---- human ---------------------------------------------------------------

fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn row(c: bool, label: &str, value: &str) -> String {
    format!("  {} {}\n", dim(c, &format!("{label:<7}")), value)
}

/// A dense one-screen summary of the whole report.
pub fn human_report(r: &Report, c: bool) -> String {
    let up = r.cpu.as_ref().map_or(0, |cpu| cpu.uptime_secs);
    let mut o = format!(
        "{} {} {} {} up {}\n",
        title(c, &r.soc.chip),
        dim(c, "·"),
        dim(c, &format!("macOS {}", r.soc.macos_version)),
        dim(c, "·"),
        uptime(up),
    );

    if let Some(cpu) = &r.cpu {
        let load = format!(
            "load {:.2} {:.2} {:.2}",
            cpu.load_avg[0], cpu.load_avg[1], cpu.load_avg[2]
        );
        let freq = r.power.as_ref().map_or_else(String::new, |p| {
            format!(
                "   {} {}  {} {}",
                r.soc.tier_low,
                Mhz(u32::try_from(p.ecpu.freq_mhz).unwrap_or(0)),
                r.soc.tier_high,
                Mhz(u32::try_from(p.pcpu.freq_mhz).unwrap_or(0)),
            )
        });
        o += &row(
            c,
            "CPU",
            &format!("{load}{freq}   self {}", Ratio(cpu.self_ratio as f32)),
        );
    }

    if let Some(p) = &r.power {
        o += &row(
            c,
            "Power",
            &format!(
                "{} pkg   cpu {}  gpu {}  ane {}",
                accent(c, &Watts(p.package_w as f32).to_string()),
                Watts(p.cpu_w as f32),
                Watts(p.gpu_w as f32),
                Watts(p.ane_w as f32),
            ),
        );
    }

    if let Some(t) = &r.thermal {
        let verdict = t.pressure.clone().unwrap_or_else(|| "unknown".to_owned());
        let painted = match t.throttling {
            Some(true) => crit(c, &format!("{verdict} (throttling)")),
            Some(false) => ok(c, &verdict),
            None => dim(c, &verdict),
        };
        o += &row(
            c,
            "Thermal",
            &format!("{} max   {painted}", Celsius(t.cpu_max_c as f32)),
        );
    }

    if let Some(m) = &r.memory {
        let pressure = match m.pressure.as_str() {
            "normal" => ok(c, "normal"),
            "warning" => warn(c, "warning"),
            other => crit(c, other),
        };
        o += &row(
            c,
            "Memory",
            &format!(
                "{} / {}  {}   pressure {pressure}",
                Bytes(m.used_bytes),
                Bytes(m.total_bytes),
                Ratio(m.used_ratio as f32),
            ),
        );
    }

    if let Some(d) = &r.disk {
        o += &row(
            c,
            "Disk",
            &format!(
                "rd {}/s wr {}/s   / {} used",
                Bytes(d.read_bytes_per_sec),
                Bytes(d.write_bytes_per_sec),
                Ratio(d.capacity_used_ratio as f32),
            ),
        );
    }

    if let Some(n) = &r.network {
        let iface = n
            .primary
            .as_ref()
            .map_or_else(String::new, |p| format!("   {} {}", p.name, if p.link_up { "up" } else { "down" }));
        o += &row(
            c,
            "Net",
            &format!(
                "rx {}/s tx {}/s{iface}",
                Bytes(n.rx_bytes_per_sec),
                Bytes(n.tx_bytes_per_sec),
            ),
        );
    }

    if let Some(procs) = &r.processes {
        let top: Vec<String> = procs
            .top
            .iter()
            .take(3)
            .map(|p| {
                format!(
                    "{} {}",
                    p.name,
                    Ratio(p.cpu_ratio.unwrap_or(0.0) as f32)
                )
            })
            .collect();
        if !top.is_empty() {
            o += &row(c, "Top", &top.join("  "));
        }
    }

    for e in &r.source_errors {
        o += &row(c, "Down", &dim(c, &format!("{}: {}", e.source, e.error)));
    }
    o
}

/// An aligned text table. Cells are plain (uncolored) so column widths stay
/// honest; the header is dimmed.
pub fn table(c: bool, headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut width = vec![0usize; cols];
    for (i, h) in headers.iter().enumerate() {
        width[i] = h.chars().count();
    }
    for r in rows {
        for (i, cell) in r.iter().enumerate().take(cols) {
            width[i] = width[i].max(cell.chars().count());
        }
    }
    let mut o = String::new();
    let head: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:<w$}", h, w = width[i]))
        .collect();
    o += &dim(c, &head.join("  "));
    o.push('\n');
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .take(cols)
            .map(|(i, cell)| format!("{:<w$}", cell, w = width[i]))
            .collect();
        o += line.join("  ").trim_end();
        o.push('\n');
    }
    o
}
