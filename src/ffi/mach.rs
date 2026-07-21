//! Hand-rolled Mach host FFI: per-core CPU ticks, VM statistics, timebase.
//!
//! These live in `libSystem`, so no extra link attribute is needed.

use std::io;

pub const CPU_STATE_USER: usize = 0;
pub const CPU_STATE_SYSTEM: usize = 1;
pub const CPU_STATE_IDLE: usize = 2;
pub const CPU_STATE_NICE: usize = 3;
pub const CPU_STATE_MAX: usize = 4;

const PROCESSOR_CPU_LOAD_INFO: i32 = 2;
const HOST_VM_INFO64: i32 = 4;
/// `HOST_VM_INFO64_COUNT`: size of `vm_statistics64` in `u32` words.
const HOST_VM_INFO64_COUNT: u32 = (size_of::<VmStatistics64>() / size_of::<u32>()) as u32;

/// `struct vm_statistics64` (machine/vm_statistics.h), page-count fields.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct VmStatistics64 {
    pub free_count: u32,
    pub active_count: u32,
    pub inactive_count: u32,
    pub wire_count: u32,
    pub zero_fill_count: u64,
    pub reactivations: u64,
    pub pageins: u64,
    pub pageouts: u64,
    pub faults: u64,
    pub cow_faults: u64,
    pub lookups: u64,
    pub hits: u64,
    pub purges: u64,
    pub purgeable_count: u32,
    pub speculative_count: u32,
    pub decompressions: u64,
    pub compressions: u64,
    pub swapins: u64,
    pub swapouts: u64,
    pub compressor_page_count: u32,
    pub throttled_count: u32,
    pub external_page_count: u32,
    pub internal_page_count: u32,
    pub total_uncompressed_pages_in_compressor: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TimebaseInfo {
    pub numer: u32,
    pub denom: u32,
}

unsafe extern "C" {
    fn mach_host_self() -> u32;
    fn mach_task_self() -> u32;
    fn host_processor_info(
        host: u32,
        flavor: i32,
        out_count: *mut u32,
        out_info: *mut *mut i32,
        out_info_cnt: *mut u32,
    ) -> i32;
    fn host_statistics64(host: u32, flavor: i32, info: *mut VmStatistics64, count: *mut u32)
    -> i32;
    fn vm_deallocate(task: u32, address: usize, size: usize) -> i32;
    pub fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
}

fn mach_err(what: &str, kr: i32) -> io::Error {
    io::Error::other(format!("{what} failed: kern_return {kr}"))
}

/// Current mach absolute time (monotonic ticks).
pub fn now_ticks() -> u64 {
    unsafe { mach_absolute_time() }
}

/// Per-core cumulative `[user, system, idle, nice]` ticks for every logical CPU.
pub fn per_core_ticks() -> io::Result<Vec<[u32; CPU_STATE_MAX]>> {
    let mut ncpu = 0u32;
    let mut info: *mut i32 = std::ptr::null_mut();
    let mut info_cnt = 0u32;
    let kr = unsafe {
        host_processor_info(
            mach_host_self(),
            PROCESSOR_CPU_LOAD_INFO,
            &raw mut ncpu,
            &raw mut info,
            &raw mut info_cnt,
        )
    };
    if kr != 0 {
        return Err(mach_err("host_processor_info", kr));
    }
    let mut out = Vec::with_capacity(ncpu as usize);
    for core in 0..ncpu as usize {
        let base = core * CPU_STATE_MAX;
        let mut ticks = [0u32; CPU_STATE_MAX];
        for (state, tick) in ticks.iter_mut().enumerate() {
            *tick = unsafe { *info.add(base + state) } as u32;
        }
        out.push(ticks);
    }
    // The kernel vm_allocates the info array into our task; we must free it.
    unsafe {
        vm_deallocate(
            mach_task_self(),
            info as usize,
            info_cnt as usize * size_of::<i32>(),
        );
    }
    Ok(out)
}

/// System-wide VM statistics (page counts; multiply by the VM page size).
pub fn vm_stats() -> io::Result<VmStatistics64> {
    let mut stats = VmStatistics64::default();
    let mut count = HOST_VM_INFO64_COUNT;
    let kr = unsafe {
        host_statistics64(
            mach_host_self(),
            HOST_VM_INFO64,
            &raw mut stats,
            &raw mut count,
        )
    };
    if kr != 0 {
        return Err(mach_err("host_statistics64", kr));
    }
    Ok(stats)
}

/// Mach timebase (numer/denom); ticks × numer / denom = nanoseconds.
pub fn timebase() -> TimebaseInfo {
    let mut tb = TimebaseInfo::default();
    // Cannot fail on a live system; a zeroed denom would be a kernel bug.
    unsafe { mach_timebase_info(&raw mut tb) };
    tb
}
