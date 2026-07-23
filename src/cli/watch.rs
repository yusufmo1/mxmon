//! Bounded metric stream. Spawns the sampler once, folds the live `Update`
//! stream, and emits one NDJSON object per interval until `--for` / `--count`
//! or a closed pipe. Because release builds are `panic = "abort"`, EPIPE
//! handling is load-bearing: `mxmon watch | head` must exit cleanly, so frames
//! go to a locked stdout and a write error (BrokenPipe) ends the loop.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::args::WatchArgs;
use super::collect::{Features, Latest};
use super::render;
use crate::collect::sampler::{self, Control, FAST_MS_MAX, FAST_MS_MIN};
use crate::collect::soc::SocInfo;
use crate::config::Config;
use crate::report::{self, select};

pub fn run(soc: &SocInfo, a: &WatchArgs) -> ExitCode {
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
    let deadline = a.r#for.map(|d| Instant::now() + d);
    let mut next = Instant::now() + Duration::from_millis(interval);
    let stdout = std::io::stdout();
    let mut w = stdout.lock();

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
        let value = frame_value(&report, &a.paths);
        if writeln!(w, "{}", render::ndjson_line(&value)).is_err() {
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

/// The whole report, or an object mapping each requested path to its value.
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
