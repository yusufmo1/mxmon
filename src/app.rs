//! Application state: latest snapshots, history rings, and UI state.

use std::collections::VecDeque;

use crate::collect::battery::BatterySample;
use crate::collect::flows::FlowSample;
use crate::collect::ping::PingSample;
use crate::collect::power::PowerSample;
use crate::collect::procs::{ProcRow, ProcSample};
use crate::collect::sampler::{FastSnapshot, Update};
use crate::collect::soc::SocInfo;
use crate::collect::temps::TempSample;
use crate::config::Config;

/// Fixed-capacity history ring for graph data.
#[derive(Debug, Clone)]
pub struct Ring {
    buf: VecDeque<f32>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub fn push(&mut self, v: f32) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
    }

    /// Latest `n` values, oldest first (fewer if not enough history yet).
    pub fn last_n(&self, n: usize) -> impl Iterator<Item = f32> + '_ {
        let skip = self.buf.len().saturating_sub(n);
        self.buf.iter().skip(skip).copied()
    }

    pub fn latest(&self) -> Option<f32> {
        self.buf.back().copied()
    }

    pub fn max(&self) -> f32 {
        self.buf.iter().copied().fold(0.0, f32::max)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

pub const HISTORY: usize = 600;

/// All history rings (each advances with its own tier's cadence).
pub struct Histories {
    pub cpu_total: Ring,
    pub per_core: Vec<Ring>,
    pub ecpu_usage: Ring,
    pub pcpu_usage: Ring,
    pub gpu: Ring,
    pub package_w: Ring,
    pub cpu_w: Ring,
    pub gpu_w: Ring,
    pub ane_w: Ring,
    pub dram_w: Ring,
    pub disp_w: Ring,
    pub sys_w: Ring,
    pub mem_used: Ring,
    pub net_rx: Ring,
    pub net_tx: Ring,
    /// Probe RTT in ms; a miss is stored as NaN (`Ring::max` ignores NaN).
    pub ping_ms: Ring,
    pub disk_rd: Ring,
    pub disk_wr: Ring,
    pub cpu_temp: Ring,
    pub gpu_temp: Ring,
}

impl Histories {
    fn new(cores: usize) -> Self {
        let r = || Ring::new(HISTORY);
        Self {
            cpu_total: r(),
            per_core: (0..cores).map(|_| Ring::new(HISTORY)).collect(),
            ecpu_usage: r(),
            pcpu_usage: r(),
            gpu: r(),
            package_w: r(),
            cpu_w: r(),
            gpu_w: r(),
            ane_w: r(),
            dram_w: r(),
            disp_w: r(),
            sys_w: r(),
            mem_used: r(),
            net_rx: r(),
            net_tx: r(),
            ping_ms: r(),
            disk_rd: r(),
            disk_wr: r(),
            cpu_temp: r(),
            gpu_temp: r(),
        }
    }
}

/// Which main view is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Overview,
    Processes,
    Thermal,
    Connections,
}

/// Process table sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Cpu,
    Memory,
    Power,
    Net,
    Pid,
    Name,
    User,
    Threads,
}

impl SortKey {
    pub fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU%",
            Self::Memory => "MEM",
            Self::Power => "PWR",
            Self::Net => "NET",
            Self::Pid => "PID",
            Self::Name => "NAME",
            Self::User => "USER",
            Self::Threads => "THR",
        }
    }
}

/// Active modal overlay, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    Help,
    Kill {
        pid: i32,
        name: String,
        selected: usize,
    },
    SortMenu {
        selected: usize,
    },
    Details {
        pid: i32,
    },
    Settings {
        selected: usize,
    },
}

/// Signals offered by the kill modal.
pub const KILL_SIGNALS: [(&str, i32); 4] = [
    ("SIGTERM  · graceful", libc::SIGTERM),
    ("SIGKILL  · force", libc::SIGKILL),
    ("SIGINT   · interrupt", libc::SIGINT),
    ("SIGHUP   · hangup", libc::SIGHUP),
];

pub const SORT_KEYS: [SortKey; 8] = [
    SortKey::Cpu,
    SortKey::Memory,
    SortKey::Power,
    SortKey::Net,
    SortKey::Pid,
    SortKey::Name,
    SortKey::User,
    SortKey::Threads,
];

/// Transient status message shown in the footer.
pub struct Toast {
    pub text: String,
    pub until: std::time::Instant,
    pub error: bool,
}

pub struct App {
    pub soc: SocInfo,
    pub config: Config,

    // Latest data per tier.
    pub fast: FastSnapshot,
    pub power: Option<PowerSample>,
    pub temps: Option<TempSample>,
    /// Bumped on every new `temps` sample; render caches key off it.
    pub temps_seq: u64,
    pub battery: Option<BatterySample>,
    pub ping: Option<PingSample>,
    pub procs: ProcSample,
    pub flows: FlowSample,
    pub source_errors: Vec<(&'static str, String)>,

    pub hist: Histories,

    // UI state.
    pub view: View,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub filter: String,
    pub filter_editing: bool,
    pub selected: usize,
    pub scroll: usize,
    pub modal: Option<Modal>,
    pub paused: bool,
    pub show_hud: bool,
    pub toast: Option<Toast>,

    /// Sorted + filtered view of `procs.rows` (indices into it).
    pub visible_rows: Vec<usize>,

    // Perf HUD numbers.
    pub last_frame_us: u64,
    pub frames: u64,
}

impl App {
    pub fn new(soc: SocInfo, config: Config) -> Self {
        let cores = soc.total_cores().max(1);
        Self {
            soc,
            config,
            fast: FastSnapshot::default(),
            power: None,
            temps: None,
            temps_seq: 0,
            battery: None,
            ping: None,
            procs: ProcSample::default(),
            flows: FlowSample::default(),
            source_errors: Vec::new(),
            hist: Histories::new(cores),
            view: View::Overview,
            sort: SortKey::Cpu,
            sort_desc: true,
            filter: String::new(),
            filter_editing: false,
            selected: 0,
            scroll: 0,
            modal: None,
            paused: false,
            show_hud: false,
            toast: None,
            visible_rows: Vec::new(),
            last_frame_us: 0,
            frames: 0,
        }
    }

    /// Fold a sampler update into state + histories.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Fast(f) => {
                if let Some(cpu) = &f.cpu {
                    // Rings store percentages (0..100) — what the panels show.
                    let total: f32 = cpu.per_core.iter().map(|r| r.as_percent()).sum::<f32>()
                        / cpu.per_core.len().max(1) as f32;
                    self.hist.cpu_total.push(total);
                    for (i, r) in cpu.per_core.iter().enumerate() {
                        if let Some(ring) = self.hist.per_core.get_mut(i) {
                            ring.push(r.as_percent());
                        }
                    }
                }
                if let Some(gpu) = &f.gpu {
                    self.hist.gpu.push(gpu.device.0);
                }
                if let Some(mem) = &f.mem {
                    self.hist.mem_used.push(mem.used_ratio().0);
                }
                if let Some(net) = &f.net {
                    self.hist.net_rx.push(net.rx_per_sec.0 as f32);
                    self.hist.net_tx.push(net.tx_per_sec.0 as f32);
                }
                if let Some(disk) = &f.disk {
                    self.hist.disk_rd.push(disk.read_per_sec.0 as f32);
                    self.hist.disk_wr.push(disk.write_per_sec.0 as f32);
                }
                self.fast = *f;
            }
            Update::Power(p) => {
                self.hist.package_w.push(p.package().0);
                self.hist.cpu_w.push(p.cpu.0);
                self.hist.gpu_w.push(p.gpu.0);
                self.hist.ane_w.push(p.ane.0);
                self.hist.dram_w.push(p.dram.0);
                self.hist.disp_w.push(p.display.0);
                self.hist.ecpu_usage.push(p.ecpu.usage.0);
                self.hist.pcpu_usage.push(p.pcpu.usage.0);
                self.power = Some(*p);
            }
            Update::Slow(s) => {
                if let Some(t) = &s.temps {
                    self.hist.cpu_temp.push(t.cpu_avg.0);
                    self.hist.gpu_temp.push(t.gpu_avg.0);
                    if let Some(w) = t.sys_power {
                        self.hist.sys_w.push(w.0);
                    }
                }
                if let Some(t) = s.temps {
                    self.temps = Some(t);
                    self.temps_seq += 1;
                }
                // Battery arrives on a slower cadence than temps; a None
                // between readings means "unchanged", so keep the last one.
                if let Some(b) = s.battery {
                    self.battery = Some(b);
                }
            }
            Update::Procs(p) => {
                self.procs = *p;
                self.refresh_visible();
            }
            Update::Ping(p) => {
                self.hist.ping_ms.push(p.rtt_ms.unwrap_or(f32::NAN));
                self.ping = Some(*p);
            }
            Update::Flows(f) => {
                self.flows = *f;
                // Only the net sort key reads flow data — don't churn the
                // row order under other sorts between procs ticks.
                if self.sort == SortKey::Net {
                    self.refresh_visible();
                }
            }
            Update::SourceDown { source, error } => {
                self.source_errors.push((source, error));
            }
        }
    }

    /// Rebuild `visible_rows` after data, sort, or filter changes.
    pub fn refresh_visible(&mut self) {
        let filter = self.filter.to_lowercase();
        let rows = &self.procs.rows;
        self.visible_rows = (0..rows.len())
            .filter(|&i| {
                filter.is_empty()
                    || rows[i].name.to_lowercase().contains(&filter)
                    || rows[i].pid.to_string() == filter
                    || rows[i].user.to_lowercase().contains(&filter)
            })
            .collect();

        let key = self.sort;
        let desc = self.sort_desc;
        let net = &self.flows.by_pid;
        self.visible_rows.sort_by(|&a, &b| {
            let (ra, rb) = (&rows[a], &rows[b]);
            let ord = match key {
                SortKey::Cpu => ra
                    .cpu
                    .map_or(-1.0, |c| c.0)
                    .total_cmp(&rb.cpu.map_or(-1.0, |c| c.0)),
                SortKey::Memory => ra
                    .memory
                    .map_or(0, |m| m.0)
                    .cmp(&rb.memory.map_or(0, |m| m.0)),
                // Unreadable rows sink below a legitimate 0 W reading.
                SortKey::Power => ra
                    .power
                    .map_or(-1.0, |w| w.0)
                    .total_cmp(&rb.power.map_or(-1.0, |w| w.0)),
                SortKey::Net => {
                    let na = net.get(&ra.pid).map_or(0, |&(rx, tx)| rx + tx);
                    let nb = net.get(&rb.pid).map_or(0, |&(rx, tx)| rx + tx);
                    na.cmp(&nb)
                }
                SortKey::Pid => ra.pid.cmp(&rb.pid),
                SortKey::Name => ra.name.to_lowercase().cmp(&rb.name.to_lowercase()),
                SortKey::User => ra.user.cmp(&rb.user),
                SortKey::Threads => ra.threads.unwrap_or(0).cmp(&rb.threads.unwrap_or(0)),
            };
            if desc { ord.reverse() } else { ord }
        });

        self.selected = self.selected.min(self.visible_rows.len().saturating_sub(1));
    }

    /// The currently selected process row, if any.
    pub fn selected_row(&self) -> Option<&ProcRow> {
        self.visible_rows
            .get(self.selected)
            .map(|&i| &self.procs.rows[i])
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            until: std::time::Instant::now() + std::time::Duration::from_secs(4),
            error,
        });
    }

    /// Move the process selection by `delta`, clamped.
    pub fn move_selection(&mut self, delta: i64) {
        let len = self.visible_rows.len();
        if len == 0 {
            return;
        }
        let new = (self.selected as i64 + delta).clamp(0, len as i64 - 1);
        self.selected = new as usize;
    }
}
