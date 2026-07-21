//! System memory: the Activity Monitor formula, swap, and pressure level.

use std::io;
use std::sync::OnceLock;

use crate::ffi::mach;
use crate::ffi::sys::{swap_usage, sysctl_u64};
use crate::units::{Bytes, Ratio};

/// Memory pressure levels as reported by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Default)]
pub struct MemSample {
    pub total: Bytes,
    /// App (anonymous, non-purgeable) + wired + compressed — matches Activity Monitor.
    pub used: Bytes,
    pub app: Bytes,
    pub wired: Bytes,
    pub compressed: Bytes,
    pub cached: Bytes,
    pub swap_used: Bytes,
    pub swap_total: Bytes,
    pub pressure: Pressure,
}

impl MemSample {
    pub fn used_ratio(&self) -> Ratio {
        if self.total.0 == 0 {
            return Ratio(0.0);
        }
        Ratio(self.used.0 as f32 / self.total.0 as f32)
    }
}

fn page_size() -> u64 {
    static PAGE: OnceLock<u64> = OnceLock::new();
    *PAGE.get_or_init(|| sysctl_u64("hw.pagesize").unwrap_or(16384))
}

fn total_memory() -> u64 {
    static TOTAL: OnceLock<u64> = OnceLock::new();
    *TOTAL.get_or_init(|| sysctl_u64("hw.memsize").unwrap_or(0))
}

fn pressure_level() -> Pressure {
    match sysctl_u64("kern.memorystatus_vm_pressure_level") {
        Some(2) => Pressure::Warning,
        Some(4) => Pressure::Critical,
        _ => Pressure::Normal,
    }
}

pub fn sample() -> io::Result<MemSample> {
    let vm = mach::vm_stats()?;
    let page = page_size();
    let app_pages = u64::from(vm.internal_page_count).saturating_sub(u64::from(vm.purgeable_count));
    let app = app_pages * page;
    let wired = u64::from(vm.wire_count) * page;
    let compressed = u64::from(vm.compressor_page_count) * page;
    let cached = (u64::from(vm.external_page_count) + u64::from(vm.purgeable_count)) * page;
    let (swap_used, swap_total) = swap_usage();
    Ok(MemSample {
        total: Bytes(total_memory()),
        used: Bytes(app + wired + compressed),
        app: Bytes(app),
        wired: Bytes(wired),
        compressed: Bytes(compressed),
        cached: Bytes(cached),
        swap_used: Bytes(swap_used),
        swap_total: Bytes(swap_total),
        pressure: pressure_level(),
    })
}
