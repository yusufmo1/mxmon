//! Bounded metric stream. Spawns the sampler once, folds the live `Update`
//! stream, and emits one frame per interval until `--for` / `--count` /
//! `--timeout` or a closed pipe. Because release builds are `panic = "abort"`,
//! EPIPE handling is load-bearing: `mxmon watch | head` must exit cleanly, so
//! frames go to a locked stdout and a write error (BrokenPipe) ends the loop.
//!
//! Paths are validated once, before the first frame, against a report built
//! from whatever has arrived. A typo is a usage error here exactly as it is in
//! `get`: streaming `{"nope":null}` forever would look like a dead sensor
//! rather than a mistyped command.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::args::{Format, GlobalArgs, WatchArgs};
use super::collect::{Features, Latest};
use super::output::{self, OutputCtx};
use super::render;
use crate::collect::sampler::{self, Control, FAST_MS_MAX, FAST_MS_MIN};
use crate::collect::soc::SocInfo;
use crate::config::Config;
use crate::report::{self, select};

pub fn run(soc: &SocInfo, a: &WatchArgs, g: &GlobalArgs) -> ExitCode {
    let ctx = OutputCtx::resolve(g, Format::Table, Format::Ndjson);
    let config = Config::load();
    let interval = a
        .interval
        .map_or(config.interval_ms, |d| d.as_millis() as u64)
        .clamp(FAST_MS_MIN, FAST_MS_MAX);

    let control = Control::new();
    control.fast_ms.store(interval, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    sampler::spawn(
        soc.clone(),
        Arc::clone(&control),
        tx,
        config.ping.then(|| config.ping_host.clone()),
        config.storage_health,
        config.kernel_stats,
    );

    let features = Features {
        ping: config.ping,
        storage_health: config.storage_health,
        kernel_stats: config.kernel_stats,
    };
    let mut latest = Latest::default();
    let mut frames = 0u64;
    // `--for` bounds the stream; `--timeout` bounds the whole command. Whichever
    // is nearer wins, so neither flag can be silently outlived by the other.
    let deadline = [a.r#for, g.timeout]
        .into_iter()
        .flatten()
        .map(|d| Instant::now() + d)
        .min();
    let mut next = Instant::now() + Duration::from_millis(interval);
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    let mut header_done = false;

    let code = loop {
        let wait = next.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(update) => {
                latest.apply(update);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break ExitCode::SUCCESS,
        }

        let report = report::Report::build(&latest.inputs(soc, interval, features, true));

        // First frame only: reject a path the contract does not have, before a
        // single line of output commits the caller to a bad stream.
        if frames == 0
            && let Err(e) = validate(&report, &a.paths)
        {
            eprintln!("mxmon watch: {e}");
            control.shutdown.store(true, Ordering::Relaxed);
            return ExitCode::from(output::USAGE);
        }

        let value = frame_value(&report, &a.paths);
        if write_frame(&mut w, ctx, &value, &mut header_done).is_err() {
            // Downstream closed the pipe (e.g. `| head`): a clean exit.
            break ExitCode::SUCCESS;
        }
        let _ = w.flush();

        frames += 1;
        if a.count.is_some_and(|c| frames >= c) {
            break ExitCode::SUCCESS;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break ExitCode::SUCCESS;
        }
        next += Duration::from_millis(interval);
    };

    control.shutdown.store(true, Ordering::Relaxed);
    code
}

/// Every requested path must parse and resolve against the live contract.
fn validate(report: &report::Report, paths: &[String]) -> Result<(), String> {
    let root = render::to_value(report);
    for p in paths {
        let segs = select::parse_path(p)?;
        select::resolve(&root, &segs)?;
    }
    Ok(())
}

/// One frame in the resolved shape. `table` prints its header once and then a
/// row per frame, so the stream stays column-aligned for `awk` and for eyes.
fn write_frame(
    w: &mut impl Write,
    ctx: OutputCtx,
    value: &serde_json::Value,
    header_done: &mut bool,
) -> std::io::Result<()> {
    match ctx.format {
        Format::Json => writeln!(w, "{}", render::json_pretty(value)),
        Format::Compact => write!(w, "{}", render::compact(value)),
        Format::Table => {
            let pairs = super::flatten::value(value);
            if !*header_done {
                *header_done = true;
                if !ctx.quiet {
                    let head: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
                    writeln!(w, "{}", render::dim(ctx.color, &head.join("  ")))?;
                }
            }
            let cells: Vec<String> = pairs
                .iter()
                .map(|(k, v)| format!("{:<w$}", v, w = k.chars().count()))
                .collect();
            writeln!(w, "{}", cells.join("  ").trim_end())
        }
        _ => writeln!(w, "{}", render::ndjson_line(value)),
    }
}

/// The whole report, or an object mapping each requested path to its value.
/// Paths were validated before the first frame, so an unresolvable one here
/// means the source went null mid-stream, which is legitimately `null`.
fn frame_value(report: &report::Report, paths: &[String]) -> serde_json::Value {
    let root = render::to_value(report);
    if paths.is_empty() {
        return root;
    }
    let mut obj = serde_json::Map::new();
    for p in paths {
        let v = select::parse_path(p)
            .and_then(|segs| select::resolve(&root, &segs).cloned())
            .unwrap_or(serde_json::Value::Null);
        obj.insert(p.clone(), v);
    }
    serde_json::Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Format;

    fn ctx(format: Format, quiet: bool) -> OutputCtx {
        OutputCtx {
            format,
            color: false,
            quiet,
        }
    }

    fn frame(paths: &[&str]) -> serde_json::Value {
        let owned: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        frame_value(&crate::report::populated(), &owned)
    }

    #[test]
    fn no_paths_streams_the_whole_report() {
        let v = frame(&[]);
        let obj = v.as_object().expect("a report object");
        assert!(obj.contains_key("meta") && obj.contains_key("power"));
    }

    #[test]
    fn requested_paths_key_the_frame_by_the_path_as_typed() {
        // Keying by the literal argument is what lets a caller line a frame up
        // with the request that produced it, even across both index spellings.
        let v = frame(&[
            "power.package_w",
            "processes.top[0].pid",
            "processes.top.0.pid",
        ]);
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert!(obj["power.package_w"].is_number());
        assert_eq!(
            obj["processes.top[0].pid"], obj["processes.top.0.pid"],
            "[0] and .0 address the same leaf"
        );
    }

    #[test]
    fn validation_accepts_the_contract_and_rejects_a_typo() {
        let r = crate::report::populated();
        assert!(validate(&r, &["power.package_w".to_owned()]).is_ok());
        assert!(validate(&r, &[]).is_ok());
        // A null source is a legitimate value, not an invalid path.
        let mut down = r.clone();
        down.thermal = None;
        assert!(validate(&down, &["thermal.cpu_max_c".to_owned()]).is_ok());
        // A name the contract does not have is the error.
        let err = validate(&r, &["power.nope".to_owned()]).unwrap_err();
        assert!(err.contains("no field"), "{err}");
        assert!(validate(&r, &["processes.top[9999]".to_owned()]).is_err());
    }

    #[test]
    fn a_source_that_dies_mid_stream_becomes_null_not_an_error() {
        // Paths are validated once up front; after that the stream must keep
        // running, because a collector going down is data, not a mistake.
        let mut r = crate::report::populated();
        r.thermal = None;
        let v = frame_value(&r, &["thermal.cpu_max_c".to_owned()]);
        assert!(v["thermal.cpu_max_c"].is_null());
    }

    #[test]
    fn table_frames_print_one_header_then_aligned_rows() {
        let v = frame(&["cpu.self_ratio", "power.package_w"]);
        let mut buf: Vec<u8> = Vec::new();
        let mut header_done = false;
        for _ in 0..2 {
            write_frame(&mut buf, ctx(Format::Table, false), &v, &mut header_done).unwrap();
        }
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "one header, then a row per frame");
        assert!(lines[0].contains("cpu.self_ratio") && lines[0].contains("power.package_w"));
        assert_eq!(lines[1], lines[2], "same data renders identically");
        assert!(lines.iter().all(|l| !l.ends_with(' ')));
    }

    #[test]
    fn quiet_table_frames_skip_the_header_entirely() {
        let v = frame(&["power.package_w"]);
        let mut buf: Vec<u8> = Vec::new();
        let mut header_done = false;
        write_frame(&mut buf, ctx(Format::Table, true), &v, &mut header_done).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains("power.package_w"));
    }

    #[test]
    fn each_format_writes_its_own_shape() {
        let v = frame(&["power.package_w"]);
        let render = |format| {
            let mut buf: Vec<u8> = Vec::new();
            let mut done = false;
            write_frame(&mut buf, ctx(format, false), &v, &mut done).unwrap();
            String::from_utf8(buf).unwrap()
        };
        assert_eq!(render(Format::Ndjson).lines().count(), 1);
        assert!(render(Format::Ndjson).starts_with('{'));
        assert!(
            render(Format::Json).lines().count() > 1,
            "pretty spans lines"
        );
        assert!(render(Format::Compact).starts_with("power.package_w="));
    }

    #[test]
    fn a_closed_pipe_surfaces_as_an_error_rather_than_a_panic() {
        // Release builds are panic = "abort", so `mxmon watch | head` depends
        // on the write error propagating instead of println! unwinding.
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let v = frame(&["power.package_w"]);
        for format in [Format::Ndjson, Format::Json, Format::Compact, Format::Table] {
            let mut done = false;
            let e = write_frame(&mut Closed, ctx(format, false), &v, &mut done).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{format:?}");
        }
    }
}
