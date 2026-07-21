//! mxmon's own CPU footprint — the "look how little it costs" readout. One
//! `task_info` on our own pid per fast tick, reusing the same unit-canceling
//! mach-tick ratio as the process table (immune to the Apple Silicon
//! "ticks aren't nanoseconds" trap).

use crate::ffi::mach::now_ticks;
use crate::ffi::proc as fp;

/// Tracks this process's cumulative busy time between fast ticks.
pub struct SelfCpu {
    pid: i32,
    /// (busy mach-ticks, mach-time) from the previous sample.
    prev: Option<(u64, u64)>,
}

impl SelfCpu {
    pub fn new() -> Self {
        Self {
            pid: std::process::id() as i32,
            prev: None,
        }
    }

    /// CPU used since the last call, as a fraction of one core (all threads
    /// summed, so it can exceed 1.0). Reads 0.0 on the first call, before a
    /// delta window exists.
    pub fn sample(&mut self) -> f32 {
        let Some(t) = fp::task_info(self.pid) else {
            return 0.0;
        };
        let busy = t.pti_total_user + t.pti_total_system;
        let at = now_ticks();
        let frac = match self.prev {
            Some((prev_busy, prev_at)) if at > prev_at => {
                busy.saturating_sub(prev_busy) as f64 / (at - prev_at) as f64
            }
            _ => 0.0,
        };
        self.prev = Some((busy, at));
        frac as f32
    }
}
