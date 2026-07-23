//! Subcommand handlers for the read and analysis verbs. Each settles one report
//! through the shared spine, then renders it as JSON, NDJSON, or a human
//! summary depending on the resolved [`OutputCtx`]. Handlers return an
//! [`ExitCode`] directly; the dispatcher wraps it, since the fallible step
//! (loading SoC facts) already happened before they were called.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::Value;

use super::args::{CheckArgs, ExplainArgs, Format, GetArgs, GlobalArgs, SnapshotArgs, TopArgs, TopBy};
use super::collect::{self, Features, SettleOpts};
use super::output::{self, OutputCtx};
use super::render;
use crate::collect::soc::SocInfo;
use crate::config::Config;
use crate::report::model::Proc;
use crate::report::{check, explain, health, select, Report};

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
    Report::build(&settled.latest.inputs(soc, opts.fast_ms, features, !settled.timed_out))
}

/// The legacy `--json` path: the v1 report as pretty JSON.
pub fn print_snapshot_json(soc: &SocInfo) {
    let report = settle_report(soc, None);
    println!("{}", render::json_pretty(&render::to_value(&report)));
}

pub fn snapshot(soc: &SocInfo, a: &SnapshotArgs, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Json);
    let report = settle_report(soc, g.timeout);
    match ctx.format {
        Format::Json | Format::Ndjson => {
            let mut v = render::to_value(&report);
            if !a.only.is_empty() {
                match select::only(&v, &a.only) {
                    Ok(projected) => v = projected,
                    Err(e) => {
                        eprintln!("mxmon snapshot: {e}");
                        return ExitCode::from(output::UNKNOWN);
                    }
                }
            }
            if ctx.format == Format::Ndjson {
                println!("{}", render::ndjson_line(&v));
            } else {
                println!("{}", render::json_pretty(&v));
            }
        }
        _ => print!("{}", render::human_report(&report, ctx.color)),
    }
    ExitCode::SUCCESS
}

pub fn get(soc: &SocInfo, a: &GetArgs, g: &GlobalArgs) -> ExitCode {
    let report = settle_report(soc, g.timeout);
    let root = render::to_value(&report);
    let mut code = ExitCode::SUCCESS;
    let mut out = String::new();
    for path in &a.paths {
        match select::parse_path(path).and_then(|segs| select::resolve(&root, &segs).cloned()) {
            Ok(v) => {
                out.push_str(&format_value(&v, a.raw));
                out.push('\n');
            }
            Err(e) => {
                eprintln!("mxmon get: {e}");
                code = ExitCode::from(output::NO_DATA);
            }
        }
    }
    print!("{out}");
    code
}

fn format_value(v: &Value, raw: bool) -> String {
    match v {
        Value::String(s) if raw => s.clone(),
        Value::Object(_) | Value::Array(_) => render::json_pretty(v),
        _ => v.to_string(),
    }
}

pub fn schema() {
    println!("{}", crate::report::schema::json_schema());
}

pub fn completions(shell: clap_complete::Shell) {
    use clap::CommandFactory as _;
    let mut cmd = super::args::Cli::command();
    clap_complete::generate(shell, &mut cmd, "mxmon", &mut std::io::stdout());
}

pub fn man() -> std::io::Result<()> {
    use clap::CommandFactory as _;
    clap_mangen::Man::new(super::args::Cli::command()).render(&mut std::io::stdout())
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

    match ctx.format {
        Format::Json | Format::Ndjson => {
            let arr = serde_json::to_value(&ranked).unwrap_or(Value::Null);
            if ctx.format == Format::Ndjson {
                println!("{}", render::ndjson_line(&arr));
            } else {
                println!("{}", render::json_pretty(&arr));
            }
        }
        _ => {
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
            print!("{}", render::table(ctx.color, &headers, &rows));
        }
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
        Ok(check::Verdict::Unknown(why)) => {
            if !g.quiet {
                eprintln!("unknown: {why}");
            }
            ExitCode::from(output::UNKNOWN)
        }
        Err(e) => {
            eprintln!("mxmon check: {e}");
            ExitCode::from(output::NO_DATA)
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
    match ctx.format {
        Format::Json | Format::Ndjson => {
            let v = serde_json::to_value(&verdict).unwrap_or(Value::Null);
            if ctx.format == Format::Ndjson {
                println!("{}", render::ndjson_line(&v));
            } else {
                println!("{}", render::json_pretty(&v));
            }
        }
        _ => print!("{}", health_human(&verdict, ctx.color)),
    }
    code
}

pub fn explain_cmd(soc: &SocInfo, a: &ExplainArgs, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Json);
    let report = settle_report(soc, g.timeout);
    let verdict = health::assess(&report);
    let e = explain::explain(a.topic.as_str(), &report, &verdict);
    match ctx.format {
        Format::Json | Format::Ndjson => {
            let v = serde_json::to_value(&e).unwrap_or(Value::Null);
            if ctx.format == Format::Ndjson {
                println!("{}", render::ndjson_line(&v));
            } else {
                println!("{}", render::json_pretty(&v));
            }
        }
        _ => print!("{}", explain_human(&e, ctx.color)),
    }
    ExitCode::SUCCESS
}

fn health_human(h: &health::Health, color: bool) -> String {
    use health::Status;
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
    let mut o = format!("{badge}{partial}\n");
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

fn explain_human(e: &explain::Explanation, color: bool) -> String {
    let mut o = format!(
        "{}\n{}\n",
        render::accent(color, &format!("[{}]", e.topic)),
        e.summary
    );
    for f in &e.findings {
        let _ = writeln!(o, "  {} {}", render::dim(color, &f.domain), f.summary);
    }
    o
}
