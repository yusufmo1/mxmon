//! Startup phase tracing: `SIMON_TRACE_STARTUP=1 mxmon 2>trace.log` prints
//! one stderr line per phase with milliseconds since launch. Zero cost when
//! the variable is unset (a single cached bool check per mark).

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Arm the clock; call first thing in `main`.
pub fn init() {
    START.get_or_init(Instant::now);
    enabled();
}

/// Whether tracing is on — gate any formatting work behind this.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("SIMON_TRACE_STARTUP").is_some_and(|v| v == "1"))
}

/// Print `label` stamped with milliseconds since [`init`].
pub fn mark(label: &str) {
    if enabled() {
        let ms = START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1e3);
        eprintln!("[mxmon {ms:7.1}ms] {label}");
    }
}
