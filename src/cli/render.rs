//! Headless renderers: the neon-styled human summary, an aligned table, and the
//! machine helpers (pretty JSON, one-line NDJSON, flat `key=value`). Human
//! formatting wraps the report's scalars back into [`crate::units`] newtypes so
//! the terminal output matches the TUI's typography exactly.
//!
//! Every renderer takes the resolved [`OutputCtx`] rather than a loose `color`
//! flag, so `--quiet` and `--no-color` are decided once and honored the same
//! way everywhere.

use std::fmt::Write as _;

use super::flatten;
use super::output::OutputCtx;
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

/// Flat `dotted.path=value` lines, one leaf per line.
///
/// This is a line-oriented shape, not a smaller one: repeating the full path on
/// every leaf costs more than JSON's nesting once an array gets long (a whole
/// snapshot runs a few percent *larger*). What it buys is `grep`, `cut`, and a
/// line-diff between two runs, plus keys that are paths `mxmon get` accepts
/// verbatim. The schema is the case where it also wins on size, because a
/// property's JSON Schema envelope dwarfs one `path:type` line.
pub fn compact(v: &serde_json::Value) -> String {
    let mut o = String::new();
    for (path, val) in flatten::value(v) {
        let _ = writeln!(o, "{path}={val}");
    }
    o
}

/// Emit a value in whichever data-only shape was resolved. Callers reach this
/// only when [`OutputCtx::is_structured`] holds.
pub fn emit(ctx: OutputCtx, v: &serde_json::Value) {
    match ctx.format {
        super::args::Format::Ndjson => println!("{}", ndjson_line(v)),
        super::args::Format::Compact => print!("{}", compact(v)),
        _ => println!("{}", json_pretty(v)),
    }
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

/// A dense one-screen summary of the whole report. Under `--quiet` the
/// identity banner is dropped and only the metric rows remain.
pub fn human_report(r: &Report, ctx: OutputCtx) -> String {
    let c = ctx.color;
    let up = r.cpu.as_ref().map_or(0, |cpu| cpu.uptime_secs);
    let mut o = if ctx.quiet {
        String::new()
    } else {
        format!(
            "{} {} {} {} up {}\n",
            title(c, &r.soc.chip),
            dim(c, "·"),
            dim(c, &format!("macOS {}", r.soc.macos_version)),
            dim(c, "·"),
            uptime(up),
        )
    };

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
        let iface = n.primary.as_ref().map_or_else(String::new, |p| {
            format!("   {} {}", p.name, if p.link_up { "up" } else { "down" })
        });
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
            .map(|p| format!("{} {}", p.name, Ratio(p.cpu_ratio.unwrap_or(0.0) as f32)))
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
/// honest; the header is dimmed, and `--quiet` drops it entirely so the output
/// is pure data for `cut`/`awk`.
pub fn table(ctx: OutputCtx, headers: &[&str], rows: &[Vec<String>]) -> String {
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
    if !ctx.quiet {
        let head: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:<w$}", h, w = width[i]))
            .collect();
        // Trimmed like the rows: a header padded out to a long final column
        // (a schema description, say) would trail dozens of spaces.
        o += &dim(ctx.color, head.join("  ").trim_end());
        o.push('\n');
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::{Format, GlobalArgs};

    fn ctx(format: Format, color: bool, quiet: bool) -> OutputCtx {
        OutputCtx {
            format,
            color,
            quiet,
        }
    }

    fn plain(format: Format) -> OutputCtx {
        ctx(format, false, false)
    }

    #[test]
    fn uptime_reads_in_the_largest_useful_unit() {
        assert_eq!(uptime(0), "0m");
        assert_eq!(uptime(59), "0m");
        assert_eq!(uptime(60), "1m");
        assert_eq!(uptime(3600), "1h 0m");
        assert_eq!(uptime(3600 + 25 * 60), "1h 25m");
        assert_eq!(uptime(86_400), "1d 0h");
        assert_eq!(uptime(86_400 * 3 + 3600 * 7), "3d 7h");
    }

    #[test]
    fn the_human_summary_covers_every_live_domain() {
        let r = crate::report::populated();
        let out = human_report(&r, plain(Format::Table));
        for label in ["CPU", "Power", "Thermal", "Memory", "Disk", "Net", "Top"] {
            assert!(out.contains(label), "the summary omits {label}:\n{out}");
        }
        assert!(
            out.starts_with(&r.soc.chip),
            "it leads with the machine identity"
        );
        assert!(out.contains("up "), "and its uptime");
    }

    #[test]
    fn quiet_drops_the_banner_and_nothing_else() {
        let r = crate::report::populated();
        let loud = human_report(&r, plain(Format::Table));
        let hushed = human_report(&r, ctx(Format::Table, false, true));
        assert_eq!(loud.lines().count(), hushed.lines().count() + 1);
        assert!(!hushed.contains(&r.soc.chip));
        assert!(hushed.contains("Power"), "the data survives");
    }

    #[test]
    fn a_downed_source_is_named_rather_than_omitted() {
        let mut r = crate::report::populated();
        r.power = None;
        r.network = None;
        r.source_errors.push(crate::report::model::SourceError {
            source: "power".to_owned(),
            error: "no ioreport subscription".to_owned(),
        });
        let out = human_report(&r, plain(Format::Table));
        assert!(!out.contains("Power  "), "a null domain draws no row");
        assert!(out.contains("Down"), "but the reason is surfaced");
        assert!(out.contains("no ioreport subscription"));
    }

    #[test]
    fn color_wraps_only_when_asked() {
        let r = crate::report::populated();
        assert!(!human_report(&r, plain(Format::Table)).contains('\x1b'));
        assert!(human_report(&r, ctx(Format::Table, true, false)).contains('\x1b'));
    }

    #[test]
    fn tables_align_to_the_widest_cell_and_never_trail_space() {
        let rows = vec![
            vec!["1".to_owned(), "a-very-long-process-name".to_owned()],
            vec!["4310".to_owned(), "node".to_owned()],
        ];
        let out = table(plain(Format::Table), &["PID", "NAME"], &rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "header plus two rows");
        assert!(lines.iter().all(|l| !l.ends_with(' ')), "no trailing pad");
        // The PID column is padded to its widest cell ("4310"), plus a
        // two-space gutter, so the second column starts at 6 on every line.
        assert_eq!(lines[0].find("NAME"), Some(6));
        assert_eq!(lines[1].find("a-very-long-process-name"), Some(6));
        assert_eq!(lines[2].find("node"), Some(6));

        let quiet = table(ctx(Format::Table, false, true), &["PID", "NAME"], &rows);
        assert_eq!(quiet.lines().count(), 2, "quiet drops the header");
        assert!(!quiet.starts_with("PID"));
    }

    #[test]
    fn a_short_row_does_not_panic_on_a_wide_header() {
        // Rendering is total: a ragged row is a legal input, not a crash.
        let rows = vec![vec!["only".to_owned()]];
        let out = table(plain(Format::Table), &["A", "B", "C"], &rows);
        assert!(out.contains("only"));
    }

    #[test]
    fn compact_emits_one_queryable_pair_per_leaf() {
        let r = crate::report::populated();
        let v = to_value(&r);
        let out = compact(&v);
        assert!(out.lines().all(|l| l.contains('=')));
        assert!(out.contains("meta.schema_version=1"));
        // Same value, two shapes, one truth.
        let pkg = out
            .lines()
            .find(|l| l.starts_with("power.package_w="))
            .expect("power is live in the fixture");
        let from_json = v["power"]["package_w"].to_string();
        assert_eq!(pkg, format!("power.package_w={from_json}"));
    }

    #[test]
    fn emit_picks_the_shape_the_context_resolved() {
        let v = serde_json::json!({"a": {"b": 1}});
        // json_pretty and ndjson_line are the two machine spellings; compact is
        // the flat one. Assert the strings rather than the side effect.
        assert!(json_pretty(&v).contains('\n'));
        assert_eq!(ndjson_line(&v), r#"{"a":{"b":1}}"#);
        assert_eq!(compact(&v), "a.b=1\n");
    }

    #[test]
    fn resolve_honors_an_explicit_format_over_auto_detection() {
        let g = |format: Format, no_color: bool, quiet: bool| GlobalArgs {
            format,
            timeout: None,
            no_color,
            quiet,
        };
        // Not a tty under `cargo test`, so `auto` takes the machine shape.
        let auto = OutputCtx::resolve(&g(Format::Auto, false, false), Format::Table, Format::Json);
        assert_eq!(auto.format, Format::Json);
        assert!(!auto.color, "no color off a terminal");
        // An explicit choice always wins.
        let forced = OutputCtx::resolve(
            &g(Format::Compact, false, false),
            Format::Table,
            Format::Json,
        );
        assert_eq!(forced.format, Format::Compact);
        assert!(forced.is_structured());
        assert!(
            !OutputCtx::resolve(&g(Format::Table, false, false), Format::Table, Format::Json)
                .is_structured()
        );
        assert!(
            OutputCtx::resolve(&g(Format::Auto, false, true), Format::Table, Format::Json).quiet
        );
    }
}
