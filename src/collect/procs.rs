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
            });
        }

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
