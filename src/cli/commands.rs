//! Subcommand handlers for the read and analysis verbs. Each settles one report
//! through the shared spine, then renders it as JSON, NDJSON, or a human
//! summary depending on the resolved [`OutputCtx`]. Handlers return an
//! [`ExitCode`] directly; the dispatcher wraps it, since the fallible step
//! (loading SoC facts) already happened before they were called.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::Value;

use super::args::{
    CheckArgs, ExplainArgs, Format, GetArgs, GlobalArgs, SnapshotArgs, TopArgs, TopBy,
};
use super::collect::{self, Features, SettleOpts};
use super::flatten;
use super::output::{self, OutputCtx};
use super::render;
use crate::collect::soc::SocInfo;
use crate::config::Config;
use crate::report::model::Proc;
use crate::report::{Report, check, explain, health, select};

/// Settle one report through the shared spine, honoring an optional deadline.
fn settle_report(soc: &SocInfo, timeout: Option<Duration>) -> Report {
    let config = Config::load();
    let opts = SettleOpts {
        fast_ms: 250,
        deadline: timeout.unwrap_or(Duration::from_secs(8)),
        min_reports: 2,
        ping_on: config.ping,
        storage_health_on: config.storage_health,
        kernel_stats_on: config.kernel_stats,
        ping_host: config.ping_host.clone(),
    };
    let settled = collect::settle(soc, &opts);
    crate::trace::mark("settled");
    let features = Features {
        ping: config.ping,
        storage_health: config.storage_health,
        kernel_stats: config.kernel_stats,
    };
    Report::build(
        &settled
            .latest
            .inputs(soc, opts.fast_ms, features, !settled.timed_out),
    )
}

/// The legacy `--json` path: the v1 report as pretty JSON.
pub fn print_snapshot_json(soc: &SocInfo, timeout: Option<Duration>) {
    let report = settle_report(soc, timeout);
    println!("{}", render::json_pretty(&render::to_value(&report)));
}

pub fn snapshot(soc: &SocInfo, a: &SnapshotArgs, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Json);
    let report = settle_report(soc, g.timeout);
    if ctx.is_structured() {
        let mut v = render::to_value(&report);
        if !a.only.is_empty() {
            match select::only(&v, &a.only) {
                Ok(projected) => v = projected,
                Err(e) => {
                    eprintln!("mxmon snapshot: {e}");
                    return ExitCode::from(output::USAGE);
                }
            }
        }
        render::emit(ctx, &v);
    } else {
        print!("{}", render::human_report(&report, ctx));
    }
    ExitCode::SUCCESS
}

pub fn get(soc: &SocInfo, a: &GetArgs, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Table);
    let report = settle_report(soc, g.timeout);
    let root = render::to_value(&report);
    let mut code = ExitCode::SUCCESS;
    let mut out = String::new();
    for path in &a.paths {
        match select::parse_path(path).and_then(|segs| select::resolve(&root, &segs).cloned()) {
            // Under `compact`, a subtree flattens to `path=value` lines rooted
            // at the requested path, so the keys stay queryable as printed.
            Ok(v) if ctx.format == Format::Compact => {
                for (leaf, val) in flatten::value(&v) {
                    let full = if leaf.is_empty() {
                        path.clone()
                    } else if leaf.starts_with('[') {
                        format!("{path}{leaf}")
                    } else {
                        format!("{path}.{leaf}")
                    };
                    let _ = writeln!(out, "{full}={val}");
                }
            }
            Ok(v) => {
                out.push_str(&format_value(&v, a.raw, ctx.format));
                out.push('\n');
            }
            Err(e) => {
                eprintln!("mxmon get: {e}");
                code = ExitCode::from(output::USAGE);
            }
        }
    }
    print!("{out}");
    code
}

fn format_value(v: &Value, raw: bool, format: Format) -> String {
    match v {
        Value::String(s) if raw => s.clone(),
        Value::Object(_) | Value::Array(_) if format == Format::Ndjson => render::ndjson_line(v),
        Value::Object(_) | Value::Array(_) => render::json_pretty(v),
        _ => v.to_string(),
    }
}

/// The report contract itself. `json`/`ndjson` give the JSON Schema; `table`
/// and `compact` give a flat path listing, which is the same information at a
/// fraction of the bytes and in the dialect `get` speaks.
pub fn schema(g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Json);
    let raw = crate::report::schema::json_schema();
    if matches!(ctx.format, Format::Json | Format::Ndjson) {
        // Already JSON; reprinting it verbatim keeps it byte-identical to the
        // golden, which is what a consumer diffs against.
        println!("{raw}");
        return ExitCode::SUCCESS;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        eprintln!("mxmon schema: the generated schema is not valid JSON");
        return ExitCode::from(output::NO_DATA);
    };
    let fields = flatten::schema(&parsed);
    if ctx.format == Format::Compact {
        let mut out = String::new();
        for f in &fields {
            let _ = writeln!(out, "{}:{}", f.path, f.ty);
        }
        print!("{out}");
    } else {
        let rows: Vec<Vec<String>> = fields
            .iter()
            .map(|f| vec![f.path.clone(), f.ty.clone(), f.description.clone()])
            .collect();
        print!(
            "{}",
            render::table(ctx, &["PATH", "TYPE", "DESCRIPTION"], &rows)
        );
    }
    ExitCode::SUCCESS
}

pub fn completions(shell: clap_complete::Shell) {
    use clap::CommandFactory as _;
    let mut cmd = super::args::Cli::command();
    clap_complete::generate(shell, &mut cmd, "mxmon", &mut std::io::stdout());
}

/// A complete roff page: the root, then one section per subcommand. Rendering
/// only the root (clap_mangen's default) would document none of the thirteen
/// verbs, which is most of what mxmon is.
pub fn man() -> std::io::Result<()> {
    use clap::CommandFactory as _;
    use std::io::Write as _;
    let root = super::args::Cli::command();
    let mut out = std::io::stdout().lock();
    clap_mangen::Man::new(root.clone())
        .title("mxmon")
        .section("1")
        .manual("mxmon manual")
        .render(&mut out)?;
    writeln!(out, ".SH SUBCOMMANDS")?;
    for sub in root.get_subcommands().filter(|s| !s.is_hide_set()) {
        // `help` is clap's own; it documents itself in the synopsis.
        if sub.get_name() == "help" {
            continue;
        }
        let page = clap_mangen::Man::new(sub.clone());
        writeln!(out, ".SS mxmon {}", sub.get_name())?;
        page.render_synopsis_section(&mut out)?;
        page.render_description_section(&mut out)?;
        page.render_options_section(&mut out)?;
    }
    Ok(())
}

pub fn top(soc: &SocInfo, a: &TopArgs, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Json);
    let report = settle_report(soc, g.timeout);
    let Some(procs) = report.processes.as_ref() else {
        eprintln!("mxmon top: process source unavailable");
        return ExitCode::from(output::NO_DATA);
    };
    let mut ranked: Vec<&Proc> = procs.top.iter().collect();
    ranked.sort_by(|x, y| top_key(a.by, y).total_cmp(&top_key(a.by, x)));
    ranked.truncate(a.count);

    if ctx.is_structured() {
        let arr = serde_json::to_value(&ranked).unwrap_or(Value::Null);
        render::emit(ctx, &arr);
    } else {
        let col = match a.by {
            TopBy::Cpu => "CPU",
            TopBy::Power => "POWER",
            TopBy::Mem => "MEM",
            TopBy::Disk => "DISK/s",
        };
        let headers = ["PID", "NAME", col];
        let rows: Vec<Vec<String>> = ranked
            .iter()
            .map(|p| vec![p.pid.to_string(), p.name.clone(), top_value(a.by, p)])
            .collect();
        print!("{}", render::table(ctx, &headers, &rows));
    }
    ExitCode::SUCCESS
}

fn top_key(by: TopBy, p: &Proc) -> f64 {
    match by {
        TopBy::Cpu => p.cpu_ratio.unwrap_or(0.0),
        TopBy::Power => p.power_w.unwrap_or(0.0),
        TopBy::Mem => p.memory_bytes.unwrap_or(0) as f64,
        TopBy::Disk => {
            p.disk_read_bytes_per_sec.unwrap_or(0) as f64
                + p.disk_write_bytes_per_sec.unwrap_or(0) as f64
        }
    }
}

fn top_value(by: TopBy, p: &Proc) -> String {
    use crate::units::{Bytes, Ratio, Watts};
    match by {
        TopBy::Cpu => Ratio(p.cpu_ratio.unwrap_or(0.0) as f32).to_string(),
        TopBy::Power => Watts(p.power_w.unwrap_or(0.0) as f32).to_string(),
        TopBy::Mem => Bytes(p.memory_bytes.unwrap_or(0)).to_string(),
        TopBy::Disk => format!(
            "{}/s",
            Bytes(p.disk_read_bytes_per_sec.unwrap_or(0) + p.disk_write_bytes_per_sec.unwrap_or(0))
        ),
    }
}

pub fn check_cmd(soc: &SocInfo, a: &CheckArgs, g: &GlobalArgs) -> ExitCode {
    let expr = a.expr.join(" ");
    let report = settle_report(soc, g.timeout);
    let root = render::to_value(&report);
    match check::evaluate(&root, &expr) {
        Ok(check::Verdict::True) => {
            if !g.quiet {
                println!("true");
            }
            ExitCode::SUCCESS
        }
        Ok(check::Verdict::False) => {
            if !g.quiet {
                println!("false");
            }
            ExitCode::from(output::ASSERT_FALSE)
        }
        // Undecidable, not false: a referenced source was null. Its own exit
        // code, so a caller can retry or degrade instead of believing a "no".
        Ok(check::Verdict::Unknown(why)) => {
            if !g.quiet {
                eprintln!("unknown: {why}");
            }
            ExitCode::from(output::UNDECIDABLE)
        }
        // A malformed expression is a mistake in the command, not a fact about
        // the machine, so it exits like any other usage error.
        Err(e) => {
            eprintln!("mxmon check: {e}");
            ExitCode::from(output::USAGE)
        }
    }
}

pub fn health_cmd(soc: &SocInfo, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Json);
    let report = settle_report(soc, g.timeout);
    let verdict = health::assess(&report);
    let code = match verdict.status {
        health::Status::Ok => ExitCode::SUCCESS,
        _ => ExitCode::from(output::ASSERT_FALSE),
    };
    if ctx.is_structured() {
        let v = serde_json::to_value(&verdict).unwrap_or(Value::Null);
        render::emit(ctx, &v);
    } else {
        print!("{}", health_human(&verdict, ctx));
    }
    code
}

pub fn explain_cmd(soc: &SocInfo, a: &ExplainArgs, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Json);
    let report = settle_report(soc, g.timeout);
    let verdict = health::assess(&report);
    let e = explain::explain(a.topic.as_str(), &report, &verdict);
    if ctx.is_structured() {
        let v = serde_json::to_value(&e).unwrap_or(Value::Null);
        render::emit(ctx, &v);
    } else {
        print!("{}", explain_human(&e, ctx));
    }
    ExitCode::SUCCESS
}

fn health_human(h: &health::Health, ctx: OutputCtx) -> String {
    use health::Status;
    let color = ctx.color;
    // Quiet keeps the findings (they are the data) and drops the verdict
    // badge, which the exit code already carries.
    let mut o = if ctx.quiet {
        String::new()
    } else {
        let badge = match h.status {
            Status::Ok => render::ok(color, "OK"),
            Status::Warn => render::warn(color, "WARN"),
            Status::Crit => render::crit(color, "CRIT"),
        };
        let partial = if h.partial {
            render::dim(color, "  (partial: some sources unavailable)")
        } else {
            String::new()
        };
        format!("{badge}{partial}\n")
    };
    for f in &h.findings {
        let dot = if f.unavailable {
            render::dim(color, "·")
        } else {
            match f.status {
                Status::Ok => render::ok(color, "●"),
                Status::Warn => render::warn(color, "●"),
                Status::Crit => render::crit(color, "●"),
            }
        };
        let _ = writeln!(o, "  {dot} {:<11} {}", f.domain, f.summary);
        if let Some(d) = &f.detail {
            let _ = writeln!(o, "       {}", render::dim(color, d));
        }
    }
    o
}

fn explain_human(e: &explain::Explanation, ctx: OutputCtx) -> String {
    let color = ctx.color;
    let mut o = if ctx.quiet {
        format!("{}\n", e.summary)
    } else {
        format!(
            "{}\n{}\n",
            render::accent(color, &format!("[{}]", e.topic)),
            e.summary
        )
    };
    for f in &e.findings {
        let _ = writeln!(o, "  {} {}", render::dim(color, &f.domain), f.summary);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Format;
    use crate::cli::output::OutputCtx;

    fn ctx(format: Format, quiet: bool) -> OutputCtx {
        OutputCtx {
            format,
            color: false,
            quiet,
        }
    }

    fn procs() -> Vec<Proc> {
        let r = crate::report::populated();
        r.processes.expect("fixture has processes").top
    }

    /// `top_key` is a pure passthrough, so exact equality is the right
    /// assertion; `total_cmp` states that without tripping the float-cmp lint.
    fn same(a: f64, b: f64) -> bool {
        a.total_cmp(&b).is_eq()
    }

    #[test]
    fn each_ranking_key_reads_its_own_metric() {
        let p = &procs()[0];
        assert!(same(top_key(TopBy::Cpu, p), p.cpu_ratio.unwrap_or(0.0)));
        assert!(same(top_key(TopBy::Power, p), p.power_w.unwrap_or(0.0)));
        assert!(same(
            top_key(TopBy::Mem, p),
            p.memory_bytes.unwrap_or(0) as f64
        ));
        // Disk is the sum of both directions, so a write-only hog still ranks.
        assert!(same(
            top_key(TopBy::Disk, p),
            (p.disk_read_bytes_per_sec.unwrap_or(0) + p.disk_write_bytes_per_sec.unwrap_or(0))
                as f64
        ));
    }

    #[test]
    fn a_null_metric_ranks_as_zero_rather_than_panicking() {
        let mut p = procs()[0].clone();
        p.cpu_ratio = None;
        p.power_w = None;
        p.memory_bytes = None;
        p.disk_read_bytes_per_sec = None;
        p.disk_write_bytes_per_sec = None;
        for by in [TopBy::Cpu, TopBy::Power, TopBy::Mem, TopBy::Disk] {
            assert!(same(top_key(by, &p), 0.0));
            assert!(!top_value(by, &p).is_empty(), "it still renders a cell");
        }
    }

    #[test]
    fn ranked_values_carry_their_unit() {
        let p = &procs()[0];
        assert!(top_value(TopBy::Power, p).ends_with('W'));
        assert!(top_value(TopBy::Disk, p).ends_with("/s"));
        assert!(top_value(TopBy::Cpu, p).contains('%'));
    }

    #[test]
    fn scalars_print_bare_and_subtrees_print_as_json() {
        let s = Value::String("Apple M3 Max".to_owned());
        assert_eq!(format_value(&s, false, Format::Table), r#""Apple M3 Max""#);
        assert_eq!(
            format_value(&s, true, Format::Table),
            "Apple M3 Max",
            "--raw unquotes"
        );
        assert_eq!(
            format_value(&Value::from(4.25), false, Format::Table),
            "4.25"
        );
        assert_eq!(format_value(&Value::Null, false, Format::Table), "null");

        let tree = serde_json::json!({"a": 1});
        assert!(
            format_value(&tree, false, Format::Table).contains('\n'),
            "pretty by default"
        );
        assert_eq!(
            format_value(&tree, false, Format::Ndjson),
            r#"{"a":1}"#,
            "ndjson keeps a subtree on one line"
        );
    }

    #[test]
    fn the_health_summary_shows_a_badge_and_every_finding() {
        let r = crate::report::populated();
        let h = health::assess(&r);
        let out = health_human(&h, ctx(Format::Table, false));
        let first = out.lines().next().unwrap();
        assert!(
            ["OK", "WARN", "CRIT"].iter().any(|b| first.contains(b)),
            "leads with a verdict badge: {first}"
        );
        for f in &h.findings {
            assert!(out.contains(&f.summary), "finding {:?} not shown", f.domain);
        }

        // Quiet drops the badge; the exit code already carries it.
        let hushed = health_human(&h, ctx(Format::Table, true));
        assert!(
            !["OK", "WARN", "CRIT"]
                .iter()
                .any(|b| hushed.lines().next().unwrap().contains(b))
        );
        assert!(
            hushed.contains(&h.findings[0].summary),
            "findings are data, they stay"
        );
    }

    #[test]
    fn a_partial_verdict_says_so_and_marks_the_domain() {
        let mut r = crate::report::populated();
        r.storage = None;
        let h = health::assess(&r);
        assert!(h.partial);
        let out = health_human(&h, ctx(Format::Table, false));
        assert!(out.contains("partial"), "the header admits it: {out}");
        assert!(out.contains("storage source unavailable"));
    }

    #[test]
    fn the_explanation_summary_leads_with_its_topic() {
        let r = crate::report::populated();
        let h = health::assess(&r);
        let e = explain::explain("thermal", &r, &h);
        let out = explain_human(&e, ctx(Format::Table, false));
        assert!(out.starts_with("[thermal]"), "{out}");
        assert!(out.contains(&e.summary));

        let hushed = explain_human(&e, ctx(Format::Table, true));
        assert!(
            hushed.starts_with(&e.summary),
            "quiet drops the topic tag: {hushed}"
        );
    }
}
