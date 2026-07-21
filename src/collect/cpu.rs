//! Per-core CPU utilization from `host_processor_info` tick deltas.

use std::io;

use crate::ffi::mach;
use crate::units::Ratio;

/// Utilization of every logical core, in kernel enumeration order
/// (E-cluster first on Apple Silicon).
#[derive(Debug, Clone, Default)]
pub struct CpuSample {
    pub per_core: Vec<Ratio>,
}

pub struct CpuCollector {
    prev: Option<Vec<[u32; mach::CPU_STATE_MAX]>>,
}

impl CpuCollector {
    pub fn new() -> Self {
        Self { prev: None }
    }

    /// Busy ratio per core since the previous call (first call yields zeros).
    pub fn sample(&mut self) -> io::Result<CpuSample> {
        let curr = mach::per_core_ticks()?;
        let sample = match &self.prev {
            Some(prev) if prev.len() == curr.len() => CpuSample {
                per_core: prev
                    .iter()
                    .zip(curr.iter())
                    .map(|(p, c)| {
                        let busy = delta(c, p, mach::CPU_STATE_USER)
                            + delta(c, p, mach::CPU_STATE_SYSTEM)
                            + delta(c, p, mach::CPU_STATE_NICE);
                        let total = busy + delta(c, p, mach::CPU_STATE_IDLE);
                        if total == 0 {
                            Ratio(0.0)
                        } else {
                            Ratio(busy as f32 / total as f32)
                        }
                    })
                    .collect(),
            },
            _ => CpuSample {
                per_core: vec![Ratio(0.0); curr.len()],
            },
        };
        self.prev = Some(curr);
        Ok(sample)
    }
}

fn delta(
    curr: &[u32; mach::CPU_STATE_MAX],
    prev: &[u32; mach::CPU_STATE_MAX],
    state: usize,
) -> u64 {
    u64::from(curr[state].wrapping_sub(prev[state]))
}

// Re-exported so the sampler keeps one import site for fast-tier scalars.
pub use crate::ffi::sys::{load_avg, uptime_secs};
