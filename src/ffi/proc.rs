//! libproc + sysctl process FFI: bulk enumeration via `KERN_PROC_ALL` (one
//! syscall for every process's BSD info) and per-pid task/rusage detail.

use std::io;
use std::mem::MaybeUninit;

pub const PROC_PIDTASKINFO: i32 = 4;
pub const RUSAGE_INFO_V4: i32 = 4;
pub const RUSAGE_INFO_V6: i32 = 6;

/// Process states from `pbi_status` / `kinfo_proc.p_stat`.
pub const SIDL: u8 = 1;
pub const SRUN: u8 = 2;
pub const SSLEEP: u8 = 3;
pub const SSTOP: u8 = 4;
pub const SZOMB: u8 = 5;

/// `struct proc_taskinfo` (sys/proc_info.h).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcTaskInfo {
    pub pti_virtual_size: u64,
    pub pti_resident_size: u64,
    pub pti_total_user: u64,
    pub pti_total_system: u64,
    pub pti_threads_user: u64,
    pub pti_threads_system: u64,
    pub pti_policy: i32,
    pub pti_faults: i32,
    pub pti_pageins: i32,
    pub pti_cow_faults: i32,
    pub pti_messages_sent: i32,
    pub pti_messages_received: i32,
    pub pti_syscalls_mach: i32,
    pub pti_syscalls_unix: i32,
    pub pti_csw: i32,
    pub pti_threadnum: i32,
    pub pti_numrunning: i32,
    pub pti_priority: i32,
}

/// `struct rusage_info_v6` (sys/resource.h) — `ri_phys_footprint` is the
/// number Activity Monitor shows in its "Memory" column. The v6 tail adds
/// per-process retired instructions/cycles split by cluster and energy in
/// nanojoules — the only sudoless source of per-process watts on macOS.
///
/// The v4 prefix is layout-identical, so this one struct serves both flavors:
/// a v4 read simply leaves the tail fields untouched (see [`rusage`]).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct RusageInfoV6 {
    pub ri_uuid: [u8; 16],
    pub ri_user_time: u64,
    pub ri_system_time: u64,
    pub ri_pkg_idle_wkups: u64,
    pub ri_interrupt_wkups: u64,
    pub ri_pageins: u64,
    pub ri_wired_size: u64,
    pub ri_resident_size: u64,
    pub ri_phys_footprint: u64,
    pub ri_proc_start_abstime: u64,
    pub ri_proc_exit_abstime: u64,
    pub ri_child_user_time: u64,
    pub ri_child_system_time: u64,
    pub ri_child_pkg_idle_wkups: u64,
    pub ri_child_interrupt_wkups: u64,
    pub ri_child_pageins: u64,
    pub ri_child_elapsed_abstime: u64,
    pub ri_diskio_bytesread: u64,
    pub ri_diskio_byteswritten: u64,
    pub ri_cpu_time_qos_default: u64,
    pub ri_cpu_time_qos_maintenance: u64,
    pub ri_cpu_time_qos_background: u64,
    pub ri_cpu_time_qos_utility: u64,
    pub ri_cpu_time_qos_legacy: u64,
    pub ri_cpu_time_qos_user_initiated: u64,
    pub ri_cpu_time_qos_user_interactive: u64,
    pub ri_billed_system_time: u64,
    pub ri_serviced_system_time: u64,
    pub ri_logical_writes: u64,
    pub ri_lifetime_max_phys_footprint: u64,
    pub ri_instructions: u64,
    pub ri_cycles: u64,
    pub ri_billed_energy: u64,
    pub ri_serviced_energy: u64,
    pub ri_interval_max_phys_footprint: u64,
    pub ri_runnable_time: u64,
    pub ri_flags: u64,
    pub ri_user_ptime: u64,
    pub ri_system_ptime: u64,
    /// Instructions retired on P-cluster cores only.
    pub ri_pinstructions: u64,
    /// Cycles on P-cluster cores only (total minus this = E-cluster).
    pub ri_pcycles: u64,
    /// Total energy billed to the process, nanojoules.
    pub ri_energy_nj: u64,
    /// P-cluster share of `ri_energy_nj`, nanojoules.
    pub ri_penergy_nj: u64,
    pub ri_secure_time_in_system: u64,
    pub ri_secure_ptime_in_system: u64,
    pub ri_neural_footprint: u64,
    pub ri_lifetime_max_neural_footprint: u64,
    pub ri_interval_max_neural_footprint: u64,
    pub ri_reserved: [u64; 9],
}

/// Compile-time guard against the SDK header: a wrong stride here would let
/// the kernel copyout scribble past (or short of) the buffer.
const _: () = assert!(size_of::<RusageInfoV6>() == 464);

unsafe extern "C" {
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut ProcTaskInfo, size: i32) -> i32;
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV6) -> i32;
    fn proc_pidpath(pid: i32, buffer: *mut u8, size: u32) -> i32;
}

// ---- kinfo_proc layout (sys/sysctl.h, LP64) ------------------------------
// libc does not export these; definitions mirror the kernel headers and match
// the proven bindings used by heim/bottom. We only read a handful of fields,
// but the full layout must be exact for the array stride (648 bytes) to hold.

#[repr(C)]
struct ExternProc {
    // Union of two pointers / timeval — same 16-byte size; we only use starttime.
    p_starttime: libc::timeval,
    p_vmspace: *mut libc::c_void,
    p_sigacts: *mut libc::c_void,
    p_flag: i32,
    p_stat: i8,
    p_pid: i32,
    p_oppid: i32,
    p_dupfd: i32,
    user_stack: *mut libc::c_void,
    exit_thread: *mut libc::c_void,
    p_debugger: i32,
    sigwait: i32,
    p_estcpu: u32,
    p_cpticks: i32,
    p_pctcpu: u32,
    p_wchan: *mut libc::c_void,
    p_wmesg: *mut libc::c_char,
    p_swtime: u32,
    p_slptime: u32,
    p_realtimer: libc::itimerval,
    p_rtime: libc::timeval,
    p_uticks: u64,
    p_sticks: u64,
    p_iticks: u64,
    p_traceflag: i32,
    p_tracep: *mut libc::c_void,
    p_siglist: i32,
    p_textvp: *mut libc::c_void,
    p_holdcnt: i32,
    p_sigmask: libc::sigset_t,
    p_sigignore: libc::sigset_t,
    p_sigcatch: libc::sigset_t,
    p_priority: u8,
    p_usrpri: u8,
    p_nice: i8,
    p_comm: [libc::c_char; 17],
    p_pgrp: *mut libc::c_void,
    p_addr: *mut libc::c_void,
    p_xstat: u16,
    p_acflag: u16,
    p_ru: *mut libc::c_void,
}

#[repr(C)]
struct Pcred {
    pc_lock: [i8; 72],
    pc_ucred: *mut libc::c_void,
    p_ruid: u32,
    p_svuid: u32,
    p_rgid: u32,
    p_svgid: u32,
    p_refcnt: i32,
}

#[repr(C)]
struct Ucred {
    cr_ref: i32,
    cr_uid: u32,
    cr_ngroups: i16,
    cr_groups: [u32; 16],
}

#[repr(C)]
struct VmspaceExtern {
    dummy: i32,
    dummy2: *mut libc::c_void,
    dummy3: [i32; 5],
    dummy4: [*mut libc::c_void; 3],
}

#[repr(C)]
struct Eproc {
    e_paddr: *mut libc::c_void,
    e_sess: *mut libc::c_void,
    e_pcred: Pcred,
    e_ucred: Ucred,
    e_vm: VmspaceExtern,
    e_ppid: i32,
    e_pgid: i32,
    e_jobc: i16,
    e_tdev: i32,
    e_tpgid: i32,
    e_tsess: *mut libc::c_void,
    e_wmesg: [i8; 8],
    e_xsize: i32,
    e_xrssize: i16,
    e_xccount: i16,
    e_xswrss: i16,
    e_flag: i32,
    e_login: [i8; 12],
    e_spare: [i32; 4],
}

#[repr(C)]
struct KinfoProc {
    kp_proc: ExternProc,
    kp_eproc: Eproc,
}

/// A trimmed view of one `kinfo_proc` entry (BSD info, visible for ALL
/// processes without sudo).
#[derive(Debug, Clone)]
pub struct BsdProc {
    pub pid: i32,
    pub ppid: i32,
    pub uid: u32,
    pub status: u8,
    pub start_sec: i64,
    pub comm: String,
}

/// Bulk-fetch BSD info for every process via `sysctl KERN_PROC_ALL`, using
/// libc's `kinfo_proc` definition (single syscall, no per-pid cost).
pub fn bsd_procs_all() -> io::Result<Vec<BsdProc>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL, 0];
    let mut len: usize = 0;
    // First call: size probe.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // Headroom: processes can spawn between the two calls.
    len += 32 * size_of::<KinfoProc>();
    let mut buf: Vec<u8> = vec![0; len];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let count = len / size_of::<KinfoProc>();

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let kp: &KinfoProc = unsafe { &*buf.as_ptr().add(i * size_of::<KinfoProc>()).cast() };
        let comm_bytes = unsafe {
            std::slice::from_raw_parts(
                kp.kp_proc.p_comm.as_ptr().cast::<u8>(),
                kp.kp_proc.p_comm.len(),
            )
        };
        let end = comm_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(comm_bytes.len());
        out.push(BsdProc {
            pid: kp.kp_proc.p_pid,
            ppid: kp.kp_eproc.e_ppid,
            uid: kp.kp_eproc.e_ucred.cr_uid,
            status: kp.kp_proc.p_stat as u8,
            start_sec: kp.kp_proc.p_starttime.tv_sec,
            comm: String::from_utf8_lossy(&comm_bytes[..end]).into_owned(),
        });
    }
    Ok(out)
}

/// Compile-time guard: the LP64 `kinfo_proc` stride must be exactly 648 bytes
/// or every record after the first would be misread.
const _: () = assert!(size_of::<KinfoProc>() == 648);

/// Task-level info (CPU times in mach ticks, thread count). Fails with EPERM
/// for other users' processes when not root.
pub fn task_info(pid: i32) -> Option<ProcTaskInfo> {
    let mut info = MaybeUninit::<ProcTaskInfo>::uninit();
    let size = size_of::<ProcTaskInfo>() as i32;
    let n = unsafe { proc_pidinfo(pid, PROC_PIDTASKINFO, 0, info.as_mut_ptr(), size) };
    (n == size).then(|| unsafe { info.assume_init() })
}

/// Rusage at the requested flavor (`RUSAGE_INFO_V6` or the `_V4` fallback for
/// kernels that reject v6). Same permission rules as [`task_info`].
///
/// The buffer is zero-initialized rather than `MaybeUninit` on purpose: a
/// kernel whose copyout is shorter than our struct (older release, v4 flavor)
/// leaves the tail fields at 0 instead of garbage, and 0 reads as "absent"
/// downstream.
pub fn rusage(pid: i32, flavor: i32) -> Option<RusageInfoV6> {
    let mut info = RusageInfoV6::default();
    let rc = unsafe { proc_pid_rusage(pid, flavor, &raw mut info) };
    (rc == 0).then_some(info)
}

/// Full executable path (may fail for system daemons without sudo).
pub fn pid_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

/// Send a signal; distinguishes permission failures for UI messaging.
pub fn kill(pid: i32, signal: i32) -> io::Result<()> {
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
