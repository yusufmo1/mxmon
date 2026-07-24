//! Process table: bulk BSD enumeration (all pids, one syscall) enriched with
//! per-pid task info where permissions allow. CPU% uses the unit-canceling
//! mach-tick ratio, immune to the Apple Silicon "ticks aren't nanoseconds" trap.

use std::collections::{HashMap, HashSet};
use std::io;

use crate::ffi::mach::now_ticks;
use crate::ffi::proc as fp;
use crate::units::{Bytes, Ratio, Watts};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    Idle,
    Running,
    Sleeping,
    Stopped,
    Zombie,
    Unknown,
}

impl ProcState {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Running => "R",
            Self::Sleeping => "S",
            Self::Idle => "I",
            Self::Stopped => "T",
            Self::Zombie => "Z",
            Self::Unknown => "?",
        }
    }

    /// Stable lowercase label for the v1 report contract (a word, not the
    /// single-letter glyph the TUI uses).
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Sleeping => "sleeping",
            Self::Idle => "idle",
            Self::Stopped => "stopped",
            Self::Zombie => "zombie",
            Self::Unknown => "unknown",
        }
    }
}

/// One row of the process table.
#[derive(Debug, Clone)]
pub struct ProcRow {
    pub pid: i32,
    pub ppid: i32,
    pub user: String,
    pub name: String,
    pub path: Option<String>,
    pub state: ProcState,
    /// CPU as a fraction of ONE core (can exceed 1.0 for multithreaded).
    /// `None` when unreadable without sudo.
    pub cpu: Option<Ratio>,
    /// Activity-Monitor-style memory footprint. `None` when unreadable.
    pub memory: Option<Bytes>,
    /// Average power over the last window (`ri_energy_nj` delta). `None` when
    /// unreadable, pre-v6 kernel, or first sighting (no delta yet).
    pub power: Option<Watts>,
    /// Instructions retired per cycle over the window.
    pub ipc: Option<f32>,
    /// Fraction of the window's cycles spent on P-cluster cores.
    pub p_share: Option<Ratio>,
    /// Disk bytes read / written per second over the window.
    pub disk_read_rate: Option<Bytes>,
    pub disk_write_rate: Option<Bytes>,
    pub threads: Option<i32>,
    /// Cumulative CPU time in seconds, when readable.
    pub cpu_time_secs: Option<u64>,
    pub start_sec: i64,
    /// Context switches per second over the window.
    pub csw_rate: Option<f64>,
    /// Syscalls (BSD + Mach) per second over the window.
    pub syscall_rate: Option<f64>,
    /// Interrupt-driven wakeups per second — Activity Monitor's "Energy
    /// Impact" is built on this; a high rate keeps the SoC out of idle.
    pub wakeup_rate: Option<f64>,
    /// Seconds spent runnable-but-not-running per second of wall clock.
    /// High means the process is waiting for a core, not for work.
    pub runnable: Option<f64>,
    /// Bytes written to the file system vs bytes that reached the device.
    /// A large ratio means the write cache is absorbing them.
    pub logical_write_rate: Option<Bytes>,
    /// Share of CPU time requested at each QoS band, coarsened to
    /// `(interactive, background)`. Explains why a process sits on E vs P
    /// cores better than any frequency reading can.
    pub qos_interactive: Option<Ratio>,
    pub qos_background: Option<Ratio>,
}

#[derive(Debug, Clone, Default)]
pub struct ProcSample {
    pub rows: Vec<ProcRow>,
    pub total: usize,
    pub running: usize,
    /// Threads across readable processes (lower bound without sudo).
    pub threads: usize,
    /// True when at least one process was unreadable (hint: run with sudo).
    pub restricted: bool,
    /// System-wide kernel activity over the window.
    pub kernel: KernelRates,
}

/// Cached immutable identity of a live process.
struct Identity {
    start_sec: i64,
    name: String,
    path: Option<String>,
}

impl Identity {
    fn resolve(pid: i32, start_sec: i64, comm: &str) -> Self {
        let path = fp::pid_path(pid);
        let name = path
            .as_deref()
            .and_then(|p| p.rsplit('/').next())
            .map_or_else(|| comm.to_owned(), str::to_owned);
        Self {
            start_sec,
            name,
            path,
        }
    }

    fn parts(&self) -> (String, Option<String>) {
        (self.name.clone(), self.path.clone())
    }
}

/// Per-pid cumulative counters from the previous pass, deltas against which
/// yield CPU%, watts, IPC, P-core share, and disk rates in one map lookup.
struct PrevCounters {
    busy_ticks: u64,
    energy_nj: u64,
    cycles: u64,
    instructions: u64,
    pcycles: u64,
    diskio_r: u64,
    diskio_w: u64,
    at_ticks: u64,
    // Kernel-interaction counters. These ride along free: both syscalls that
    // supply them are already made for CPU%, power and memory.
    csw: u64,
    syscalls: u64,
    messages: u64,
    wakeups: u64,
    logical_writes: u64,
    runnable_ns: u64,
}

/// System-wide kernel activity, summed from **per-pid deltas**.
///
/// Not from totals: a total-minus-total would go negative every time a process
/// exits between passes, and saturating that to zero silently under-reports.
/// Only pids seen in both passes contribute, so the rate is honest about the
/// window it actually observed.
#[derive(Debug, Clone, Copy, Default)]
pub struct KernelRates {
    pub context_switches: f64,
    pub syscalls: f64,
    pub mach_messages: f64,
    pub interrupt_wakeups: f64,
    /// Runnable-but-not-running thread time per second — the scheduler
    /// contention signal. 1.0 means one thread was always waiting for a core.
    pub runnable: f64,
}

/// nJ over a window → average watts.
pub(crate) fn watts_from_energy(delta_nj: u64, dt_secs: f64) -> Watts {
    if dt_secs <= 0.0 {
        return Watts(0.0);
    }
    Watts((delta_nj as f64 / dt_secs / 1e9) as f32)
}

/// Instructions per cycle; `None` when the process ran zero cycles.
pub(crate) fn ipc(delta_instructions: u64, delta_cycles: u64) -> Option<f32> {
    (delta_cycles > 0).then(|| (delta_instructions as f64 / delta_cycles as f64) as f32)
}

/// P-cluster share of cycles, clamped (counters can skew across a window).
pub(crate) fn p_share(delta_pcycles: u64, delta_cycles: u64) -> Option<Ratio> {
    (delta_cycles > 0).then(|| Ratio((delta_pcycles as f64 / delta_cycles as f64) as f32).clamped())
}

/// `(interactive, background)` share of a process's lifetime CPU time, from
/// the `ri_cpu_time_qos_*` bands.
///
/// Lifetime rather than windowed on purpose: QoS is a property of what a
/// process *is*, not of the last 2 seconds, and a windowed version would read
/// as undefined every time a process went briefly idle. `None` when it has
/// requested no classified CPU time at all, so "unknown" never renders as
/// "100% background".
pub(crate) fn qos_mix(u: &crate::ffi::proc::RusageInfoV6) -> Option<(Ratio, Ratio)> {
    let interactive = u
        .ri_cpu_time_qos_user_interactive
        .saturating_add(u.ri_cpu_time_qos_user_initiated);
    let background = u
        .ri_cpu_time_qos_background
        .saturating_add(u.ri_cpu_time_qos_maintenance)
        .saturating_add(u.ri_cpu_time_qos_utility);
    let total = interactive
        .saturating_add(background)
        .saturating_add(u.ri_cpu_time_qos_default)
        .saturating_add(u.ri_cpu_time_qos_legacy);
    (total > 0).then(|| {
        (
            Ratio((interactive as f64 / total as f64) as f32).clamped(),
            Ratio((background as f64 / total as f64) as f32).clamped(),
        )
    })
}

pub struct ProcCollector {
    /// pid → cumulative counters from the previous pass.
    prev: HashMap<i32, PrevCounters>,
    users: HashMap<u32, String>,
    /// pid → resolved name/path, revalidated by start time (pid reuse).
    identity: HashMap<i32, Identity>,
    /// mach ticks → seconds factor.
    ticks_to_secs: f64,
    /// `RUSAGE_INFO_V6` where the kernel supports it, else the v4 fallback
    /// (probed once — the flavor can't change while we run).
    rusage_flavor: i32,
}

impl ProcCollector {
    pub fn new() -> Self {
        let tb = crate::ffi::mach::timebase();
        let ticks_to_secs = f64::from(tb.numer) / f64::from(tb.denom) / 1e9;
        let own_pid = std::process::id() as i32;
        let rusage_flavor = if fp::rusage(own_pid, fp::RUSAGE_INFO_V6).is_some() {
            fp::RUSAGE_INFO_V6
        } else {
            fp::RUSAGE_INFO_V4
        };
        Self {
            prev: HashMap::new(),
            users: HashMap::new(),
            identity: HashMap::new(),
            ticks_to_secs,
            rusage_flavor,
        }
    }

    fn username(&mut self, uid: u32) -> String {
        self.users
            .entry(uid)
            .or_insert_with(|| crate::ffi::sys::username(uid).unwrap_or_else(|| uid.to_string()))
            .clone()
    }

    pub fn sample(&mut self) -> io::Result<ProcSample> {
        let bsd = fp::bsd_procs_all()?;
        let now_ticks = now_ticks();
        let mut kernel = KernelRates::default();
        let mut out = ProcSample {
            total: bsd.len(),
            ..Default::default()
        };
        let mut next_prev = HashMap::with_capacity(bsd.len());
        let mut seen: HashSet<i32> = HashSet::with_capacity(bsd.len());

        for p in bsd {
            seen.insert(p.pid);
            if p.pid == 0 {
                continue; // kernel_task pseudo-entry has no readable stats
            }
            let task = fp::task_info(p.pid);
            // macOS reports SRUN for almost everything; a process is genuinely
            // running only when it has runnable threads right now.
            let state = match p.status {
                fp::SZOMB => ProcState::Zombie,
                fp::SSTOP => ProcState::Stopped,
                fp::SIDL => ProcState::Idle,
                fp::SSLEEP => ProcState::Sleeping,
                fp::SRUN => match task {
                    Some(t) if t.pti_numrunning > 0 => ProcState::Running,
                    Some(_) => ProcState::Sleeping,
                    None => ProcState::Unknown,
                },
                _ => ProcState::Unknown,
            };
            // task_info failing means EPERM — rusage would fail identically,
            // so don't pay for a second refused syscall.
            let usage = if task.is_some() {
                fp::rusage(p.pid, self.rusage_flavor)
            } else {
                None
            };
            if task.is_none() {
                out.restricted = true;
            }

            // One cumulative-counter snapshot per readable pid; every rate
            // (CPU%, watts, IPC, P-share, disk) is a delta against last pass.
            let mut cpu = None;
            let mut power = None;
            let mut proc_ipc = None;
            let mut proc_p_share = None;
            let mut disk_read_rate = None;
            let mut disk_write_rate = None;
            let mut csw_rate = None;
            let mut syscall_rate = None;
            let mut wakeup_rate = None;
            let mut runnable = None;
            let mut logical_write_rate = None;
            if let Some(t) = task {
                let cur = PrevCounters {
                    busy_ticks: t.pti_total_user + t.pti_total_system,
                    energy_nj: usage.map_or(0, |u| u.ri_energy_nj),
                    cycles: usage.map_or(0, |u| u.ri_cycles),
                    instructions: usage.map_or(0, |u| u.ri_instructions),
                    pcycles: usage.map_or(0, |u| u.ri_pcycles),
                    diskio_r: usage.map_or(0, |u| u.ri_diskio_bytesread),
                    diskio_w: usage.map_or(0, |u| u.ri_diskio_byteswritten),
                    at_ticks: now_ticks,
                    csw: t.pti_csw.max(0) as u64,
                    syscalls: (t.pti_syscalls_unix.max(0) as u64)
                        + (t.pti_syscalls_mach.max(0) as u64),
                    messages: (t.pti_messages_sent.max(0) as u64)
                        + (t.pti_messages_received.max(0) as u64),
                    wakeups: usage.map_or(0, |u| u.ri_interrupt_wkups),
                    logical_writes: usage.map_or(0, |u| u.ri_logical_writes),
                    runnable_ns: usage.map_or(0, |u| u.ri_runnable_time),
                };
                // First sighting reports 0% CPU (no window yet) — keeps
                // `cpu.is_some()` meaning "readable", which the UI relies on.
                cpu = Some(Ratio(0.0));
                if let Some(prev) = self.prev.get(&p.pid)
                    && now_ticks > prev.at_ticks
                {
                    let dt_ticks = now_ticks - prev.at_ticks;
                    let busy = cur.busy_ticks.saturating_sub(prev.busy_ticks);
                    cpu = Some(Ratio((busy as f64 / dt_ticks as f64) as f32));
                    let window = dt_ticks as f64 * self.ticks_to_secs;
                    if window > 0.0 {
                        let per_sec = |d: u64| d as f64 / window;
                        let d_csw = cur.csw.saturating_sub(prev.csw);
                        let d_sys = cur.syscalls.saturating_sub(prev.syscalls);
                        let d_msg = cur.messages.saturating_sub(prev.messages);
                        csw_rate = Some(per_sec(d_csw));
                        syscall_rate = Some(per_sec(d_sys));
                        kernel.context_switches += per_sec(d_csw);
                        kernel.syscalls += per_sec(d_sys);
                        kernel.mach_messages += per_sec(d_msg);
                        if usage.is_some() && self.rusage_flavor == fp::RUSAGE_INFO_V6 {
                            let d_wake = cur.wakeups.saturating_sub(prev.wakeups);
                            wakeup_rate = Some(per_sec(d_wake));
                            kernel.interrupt_wakeups += per_sec(d_wake);
                            logical_write_rate = Some(Bytes(per_sec(
                                cur.logical_writes.saturating_sub(prev.logical_writes),
                            ) as u64));
                            let d_run = cur.runnable_ns.saturating_sub(prev.runnable_ns);
                            let run = d_run as f64 / 1e9 / window;
                            runnable = Some(run);
                            kernel.runnable += run;
                        }
                    }
                    // v4 fallback leaves the v6 tail zeroed — a delta of two
                    // zeros would fake a 0 W reading, so gate on the flavor.
                    if usage.is_some() && self.rusage_flavor == fp::RUSAGE_INFO_V6 {
                        let dt_secs = dt_ticks as f64 * self.ticks_to_secs;
                        let cycles = cur.cycles.saturating_sub(prev.cycles);
                        power = Some(watts_from_energy(
                            cur.energy_nj.saturating_sub(prev.energy_nj),
                            dt_secs,
                        ));
                        proc_ipc = ipc(cur.instructions.saturating_sub(prev.instructions), cycles);
                        proc_p_share = p_share(cur.pcycles.saturating_sub(prev.pcycles), cycles);
                        if dt_secs > 0.0 {
                            let rate = |delta: u64| Bytes((delta as f64 / dt_secs) as u64);
                            disk_read_rate = Some(rate(cur.diskio_r.saturating_sub(prev.diskio_r)));
                            disk_write_rate =
                                Some(rate(cur.diskio_w.saturating_sub(prev.diskio_w)));
                        }
                    }
                }
                next_prev.insert(p.pid, cur);
            }

            let qos = usage
                .filter(|_| self.rusage_flavor == fp::RUSAGE_INFO_V6)
                .and_then(|u| qos_mix(&u));

            let threads = task.map(|t| t.pti_threadnum);
            out.threads += threads.unwrap_or(0).max(0) as usize;
            if state == ProcState::Running {
                out.running += 1;
            }

            // Prefer the executable basename over the 16-char truncated comm.
            // Identity is immutable for a live process, so resolve the path
            // once per (pid, start-time) instead of ~1000 syscalls per pass.
            let (name, path) = self
                .identity
                .entry(p.pid)
                .and_modify(|cached| {
                    if cached.start_sec != p.start_sec {
                        *cached = Identity::resolve(p.pid, p.start_sec, &p.comm);
                    }
                })
                .or_insert_with(|| Identity::resolve(p.pid, p.start_sec, &p.comm))
                .parts();

            out.rows.push(ProcRow {
                pid: p.pid,
                ppid: p.ppid,
                user: self.username(p.uid),
                name,
                path,
                state,
                cpu,
                memory: usage.map(|u| Bytes(u.ri_phys_footprint)),
                power,
                ipc: proc_ipc,
                p_share: proc_p_share,
                disk_read_rate,
                disk_write_rate,
                threads,
                cpu_time_secs: task.map(|t| {
                    ((t.pti_total_user + t.pti_total_system) as f64 * self.ticks_to_secs) as u64
                }),
                start_sec: p.start_sec,
                csw_rate,
                syscall_rate,
                wakeup_rate,
                runnable,
                logical_write_rate,
                qos_interactive: qos.map(|q| q.0),
                qos_background: qos.map(|q| q.1),
            });
        }

        out.kernel = kernel;
        self.prev = next_prev;
        // Drop identities of exited processes so pid reuse can't go stale.
        self.identity.retain(|pid, _| seen.contains(pid));
        Ok(out)
    }
}

/// Send a signal to a process; returns a user-presentable error on failure.
pub fn kill(pid: i32, signal: i32) -> Result<(), String> {
    fp::kill(pid, signal).map_err(|e| match e.raw_os_error() {
        Some(libc::EPERM) => "permission denied — run with sudo".into(),
        Some(libc::ESRCH) => "process already exited".into(),
        _ => e.to_string(),
    })
}

/// Renice `pid`, mapping the common errno cases to friendly messages.
pub fn renice(pid: i32, nice: i32) -> Result<(), String> {
    fp::setpriority(pid, nice).map_err(|e| match e.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => {
            "permission denied — raising priority or renicing another user's process needs sudo"
                .into()
        }
        Some(libc::ESRCH) => "process already exited".into(),
        _ => e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{ipc, p_share, qos_mix, watts_from_energy};
    use crate::ffi::proc::RusageInfoV6;
    use crate::units::{Ratio, Watts};

    fn usage_with_qos(
        interactive: u64,
        initiated: u64,
        background: u64,
        default: u64,
    ) -> RusageInfoV6 {
        RusageInfoV6 {
            ri_cpu_time_qos_user_interactive: interactive,
            ri_cpu_time_qos_user_initiated: initiated,
            ri_cpu_time_qos_background: background,
            ri_cpu_time_qos_default: default,
            ..Default::default()
        }
    }

    #[test]
    fn qos_mix_splits_interactive_from_background() {
        // `user_initiated` counts as interactive, `default` as neither.
        let u = usage_with_qos(30, 10, 40, 20);
        let (i, b) = qos_mix(&u).expect("classified time exists");
        assert!((i.0 - 0.4).abs() < 1e-6, "{i:?}");
        assert!((b.0 - 0.4).abs() < 1e-6, "{b:?}");
    }

    #[test]
    fn qos_mix_is_absent_rather_than_zero_when_nothing_is_classified() {
        // A process with no classified CPU time is unknown, not "100%
        // background" — the panel must render a dash, not a confident 0%.
        assert!(qos_mix(&usage_with_qos(0, 0, 0, 0)).is_none());
    }

    #[test]
    fn qos_mix_saturates_instead_of_overflowing() {
        // Counters are cumulative u64s; a bogus kernel read must not panic.
        let u = usage_with_qos(u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        let (i, b) = qos_mix(&u).expect("saturated total is still positive");
        assert!((0.0..=1.0).contains(&i.0));
        assert!((0.0..=1.0).contains(&b.0));
    }

    #[test]
    fn proc_rate_derivation() {
        // 500 mJ over one second = 0.5 W.
        assert_eq!(watts_from_energy(500_000_000, 1.0), Watts(0.5));
        // A zero (or negative) window can't produce a rate.
        assert_eq!(watts_from_energy(1_000_000, 0.0), Watts(0.0));
        // IPC needs cycles to divide by.
        assert_eq!(ipc(30, 10), Some(3.0));
        assert_eq!(ipc(0, 0), None);
        // P-share clamps counter skew instead of reporting >100%.
        assert_eq!(p_share(50, 100).map(Ratio::as_percent), Some(50.0));
        assert_eq!(p_share(120, 100).map(Ratio::as_percent), Some(100.0));
        assert_eq!(p_share(1, 0), None);
    }
}
