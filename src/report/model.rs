//! The v1 agent-facing report: a stable, self-describing, consistently-unitted
//! view of every metric, decoupled from the internal collector structs.
//!
//! This module IS the public contract. Two rules keep it honest:
//!
//! 1. **No field is ever omitted.** A domain that could not be sampled
//!    serializes as JSON `null` with its key present, so a consumer can rely on
//!    the shape without probing. That means **no `skip_serializing_if`, no
//!    `skip`, and no omission-inducing `#[serde(default)]`** anywhere here.
//!    `super::tests` asserts an all-`None` report still carries every key.
//! 2. **Every unit is spelled in the key** (`_w`, `_bytes`, `_ratio`, `_c`,
//!    `_mhz`, `_ms`, `_per_sec`) and produced through [`super::norm`], the one
//!    normalization site.
//!
//! `null` has three distinct causes, disambiguated so an agent always knows
//! which: a matching entry in [`Report::source_errors`] (source failed), a
//! `false` in [`Meta::features`] (collector disabled), or [`Meta::settled`]
//! being `false` (the tier never reported before the deadline).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One settled snapshot of the machine.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    /// Provenance: contract version, capture time, settle state, and which
    /// optional collectors ran.
    pub meta: Meta,
    /// Immutable machine facts. Never null: loaded synchronously at startup.
    pub soc: Soc,
    /// CPU utilization and load. `null` if the fast tier did not settle.
    pub cpu: Option<Cpu>,
    /// GPU utilization and memory. `null` if the AGX source is down.
    pub gpu: Option<Gpu>,
    /// System memory and swap. `null` if the memory source is down.
    pub memory: Option<Memory>,
    /// Power rails and CPU/GPU frequency. `null` if IOReport is unavailable.
    pub power: Option<Power>,
    /// Temperatures, fans, and thermal pressure. `null` if the SMC sweep is down.
    pub thermal: Option<Thermal>,
    /// Network throughput and the primary interface. `null` if the source is down.
    pub network: Option<Network>,
    /// Disk I/O, capacity, and filesystems. `null` if the block-storage source is down.
    pub disk: Option<Disk>,
    /// Drive health (SMART, cache, throttle). `null` when `storage_health` is off.
    pub storage: Option<Storage>,
    /// Battery and adapter state. `null` on machines without a battery.
    pub battery: Option<Battery>,
    /// Process-table summary and top consumers. `null` if the process pass is down.
    pub processes: Option<Processes>,
    /// Per-connection network flows. `null` if the ntstat source is down.
    pub flows: Option<Flows>,
    /// Kernel activity: scheduler rates, interrupts, and sleep blockers.
    pub kernel: Option<Kernel>,
    /// Connectivity probe. `null` when `ping` is disabled.
    pub ping: Option<Ping>,
    /// One entry per collector that failed at startup. Always present, often
    /// empty. A key elsewhere being `null` with a matching entry here means
    /// "the source is down", as opposed to "disabled" or "not yet settled".
    pub source_errors: Vec<SourceError>,
}

/// Provenance for the snapshot: how to read it and how fresh it is.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Meta {
    /// Contract version. Bumped on any breaking schema change. Currently 1.
    pub schema_version: u32,
    /// The mxmon build that produced this report.
    pub mxmon_version: String,
    /// Wall-clock capture time, seconds since the Unix epoch.
    pub generated_unix: u64,
    /// True iff every settle-gated tier reported at least twice before the
    /// deadline. When false, some fresh-but-single-sample or absent values may
    /// be present, and delta-based rates may still be zero.
    pub settled: bool,
    /// Delta windows the printed rates average over, per tier, in milliseconds.
    pub sample_window: SampleWindow,
    /// Which optional collectors were enabled for this run. Disambiguates a
    /// `null` domain that is "disabled" from one that is "down" or "unsettled".
    pub features: Features,
}

/// Per-tier sampling intervals, in milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SampleWindow {
    /// Fast tier: cpu, gpu, memory, network, disk.
    pub fast_ms: u64,
    /// Power and frequency tier.
    pub power_ms: u64,
    /// Process-table tier.
    pub procs_ms: u64,
    /// Connection-flows tier.
    pub flows_ms: u64,
}

/// The optional collectors and whether each was enabled.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Features {
    /// The ICMP connectivity prober.
    pub ping: bool,
    /// NVMe SMART, APFS cache, and controller-throttle polling.
    pub storage_health: bool,
    /// Interrupt-rate and sleep-assertion polling.
    pub kernel_stats: bool,
}

/// A collector that failed at startup, with the reason.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceError {
    /// The collector that failed (matches a top-level domain key).
    pub source: String,
    /// Why it failed.
    pub error: String,
}

/// Immutable machine facts.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Soc {
    /// Marketing name, e.g. "Apple M3 Max".
    pub chip: String,
    /// macOS product version, e.g. "26.5.2".
    pub macos_version: String,
    /// Efficiency-tier core count.
    pub ecpu_cores: u32,
    /// Performance-tier core count.
    pub pcpu_cores: u32,
    /// Cores per performance cluster (Max parts run more than one cluster).
    pub cores_per_pcluster: u32,
    /// GPU core count, when the registry publishes it.
    pub gpu_cores: Option<u32>,
    /// Installed physical memory.
    pub memory_bytes: u64,
    /// E-tier DVFS steps, ascending, in MHz.
    pub ecpu_freqs_mhz: Vec<u64>,
    /// P-tier DVFS steps, ascending, in MHz.
    pub pcpu_freqs_mhz: Vec<u64>,
    /// GPU DVFS steps, ascending, in MHz.
    pub gpu_freqs_mhz: Vec<u64>,
    /// Display letter for the lower CPU tier ("E" on M1-M4, "P" on M5 Pro/Max).
    pub tier_low: String,
    /// Display letter for the upper CPU tier ("P" on M1-M4, "S" on M5 Pro/Max).
    pub tier_high: String,
}

/// CPU utilization and system load. Per-core frequency and power live under
/// [`Power`] (a different source: IOReport, not mach).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Cpu {
    /// Busy fraction per logical core, 0..1, in kernel order (E-cluster first).
    /// Empty if the per-core read failed.
    pub per_core_ratio: Vec<f64>,
    /// Load average over 1, 5, and 15 minutes.
    pub load_avg: [f64; 3],
    /// Seconds since boot.
    pub uptime_secs: u64,
    /// mxmon's own CPU use, as a fraction of one core.
    pub self_ratio: f64,
}

/// GPU utilization and memory. GPU frequency and active ratio live under
/// [`Power`] (the IOReport source).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Gpu {
    /// Overall device utilization, 0..1 (matches Activity Monitor's GPU meter).
    pub device_ratio: f64,
    /// Renderer (shader) utilization, 0..1.
    pub renderer_ratio: f64,
    /// Tiler (geometry) utilization, 0..1.
    pub tiler_ratio: f64,
    /// In-use system memory attributed to the GPU.
    pub used_memory_bytes: u64,
}

/// System memory (the Activity Monitor formula), swap, and pressure.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Memory {
    /// Installed physical memory.
    pub total_bytes: u64,
    /// App + wired + compressed (Activity Monitor's "Memory Used").
    pub used_bytes: u64,
    /// Anonymous, non-purgeable application memory.
    pub app_bytes: u64,
    /// Wired (non-pageable) memory.
    pub wired_bytes: u64,
    /// Memory held by the compressor.
    pub compressed_bytes: u64,
    /// File-backed and purgeable pages the OS can reclaim.
    pub cached_bytes: u64,
    /// Swap in use.
    pub swap_used_bytes: u64,
    /// Swap backing-store size.
    pub swap_total_bytes: u64,
    /// `used_bytes` / `total_bytes`, 0..1.
    pub used_ratio: f64,
    /// Kernel memory pressure: "normal", "warning", or "critical".
    pub pressure: String,
}

/// Power rails and CPU/GPU frequency from IOReport energy counters.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Power {
    /// Package power: CPU + GPU + ANE rails.
    pub package_w: f64,
    /// CPU compute rails.
    pub cpu_w: f64,
    /// GPU compute rails.
    pub gpu_w: f64,
    /// Apple Neural Engine.
    pub ane_w: f64,
    /// LPDDR (DRAM) rail.
    pub dram_w: f64,
    /// Both display pipelines summed.
    pub display_w: f64,
    /// External display pipeline (the external share of `display_w`).
    pub display_ext_w: f64,
    /// GPU SRAM rail.
    pub gpu_sram_w: f64,
    /// Memory-controller fabric (`AMCC`), separate from the DRAM rail.
    pub amcc_w: f64,
    /// DRAM command scheduler / PHY (`DCS`).
    pub dcs_w: f64,
    /// Video encode/decode engine (`AVE`).
    pub video_w: f64,
    /// Camera image-signal processor (`ISP`).
    pub isp_w: f64,
    /// Media scaler (`MSR`).
    pub scaler_w: f64,
    /// GPU command/scheduler rails.
    pub gpu_cs_w: f64,
    /// Efficiency cluster (the "E" tier; the mid tier on M5 Pro/Max).
    pub ecpu: Cluster,
    /// Performance cluster (the "P" tier; "S" on M5 Pro/Max).
    pub pcpu: Cluster,
    /// GPU frequency, residency-weighted.
    pub gpu_freq_mhz: u64,
    /// Frequency-scaled GPU usage.
    pub gpu_usage_ratio: f64,
    /// Fraction of the window the GPU was powered on.
    pub gpu_active_ratio: f64,
}

/// A CPU cluster's aggregate readings plus its per-core breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Cluster {
    /// Residency-weighted average frequency across the cluster.
    pub freq_mhz: u64,
    /// Average effective utilization across the cluster, 0..1.
    pub usage_ratio: f64,
    /// Per-core breakdown, sorted by (die, cluster, core).
    pub cores: Vec<Core>,
}

/// One core's frequency, usage, and (when the chip publishes a per-core rail)
/// power.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Core {
    /// This core's residency-weighted frequency.
    pub freq_mhz: u64,
    /// This core's effective utilization, 0..1.
    pub usage_ratio: f64,
    /// The core's own energy rail in watts. `null` on chips that publish no
    /// per-core rail: absent power is not zero power.
    pub power_w: Option<f64>,
}

/// Temperatures, fans, the kernel's thermal-pressure verdict, and SMC power.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Thermal {
    /// Mean CPU core temperature.
    pub cpu_avg_c: f64,
    /// Hottest CPU core temperature.
    pub cpu_max_c: f64,
    /// Mean GPU temperature.
    pub gpu_avg_c: f64,
    /// Hottest GPU temperature.
    pub gpu_max_c: f64,
    /// Kernel thermal-pressure level: "nominal" through "sleeping". `null` if
    /// the notification key is unavailable.
    pub pressure: Option<String>,
    /// Whether the OS considers the machine thermally constrained (pressure at
    /// "moderate" or above).
    pub throttling: Option<bool>,
    /// Pressure severity on a 0..1 ramp, for gauges.
    pub severity: Option<f64>,
    /// SMC total system power draw (`PSTR`).
    pub sys_power_w: Option<f64>,
    /// SMC watts delivered by the adapter right now (`PDTR`).
    pub adapter_power_w: Option<f64>,
    /// SMC display backlight rail (`PDBR`).
    pub backlight_power_w: Option<f64>,
    /// Every readable die and board sensor.
    pub sensors: Vec<Sensor>,
    /// Fans (empty on fanless machines).
    pub fans: Vec<Fan>,
}

/// One named temperature reading.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Sensor {
    /// Human-readable sensor name.
    pub label: String,
    /// Display group, tier-aware ("E-Cores"/"P-Cores", "GPU", "SSD", ...).
    pub group: String,
    /// Temperature.
    pub temp_c: f64,
}

/// One fan's current and maximum speed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Fan {
    /// Fan name.
    pub label: String,
    /// Current speed.
    pub rpm: f64,
    /// Rated maximum speed.
    pub max_rpm: f64,
    /// Current speed as a fraction of maximum, when the maximum is known.
    pub ratio: Option<f64>,
}

/// Network throughput and the primary interface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Network {
    /// Receive throughput, aggregated over physical interfaces.
    pub rx_bytes_per_sec: u64,
    /// Transmit throughput, aggregated over physical interfaces.
    pub tx_bytes_per_sec: u64,
    /// Received bytes accumulated since mxmon launched.
    pub rx_session_bytes: u64,
    /// Transmitted bytes accumulated since mxmon launched.
    pub tx_session_bytes: u64,
    /// The busiest active interface, when one is up.
    pub primary: Option<PrimaryIf>,
}

/// The busiest active interface.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrimaryIf {
    /// Interface name, e.g. "en0".
    pub name: String,
    /// Link speed in bits per second (a bit-rate by definition).
    pub link_speed_bps: u64,
    /// Local IPv4 address, when assigned.
    pub ipv4: Option<String>,
    /// Hardware (MAC) address.
    pub mac: Option<String>,
    /// Link actually up (`IFF_RUNNING`), not merely configured.
    pub link_up: bool,
}

/// Disk throughput, latency, capacity, and mounted filesystems.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Disk {
    /// Read throughput, aggregated across drives.
    pub read_bytes_per_sec: u64,
    /// Write throughput, aggregated across drives.
    pub write_bytes_per_sec: u64,
    /// Read operations per second.
    pub read_iops: u64,
    /// Write operations per second.
    pub write_iops: u64,
    /// Average per-op read latency, when any op has been seen.
    pub read_latency_us: Option<f64>,
    /// Average per-op write latency, when any op has been seen.
    pub write_latency_us: Option<f64>,
    /// Read bytes accumulated since mxmon launched.
    pub read_session_bytes: u64,
    /// Write bytes accumulated since mxmon launched.
    pub write_session_bytes: u64,
    /// Number of block-storage drivers aggregated.
    pub devices: u64,
    /// Capacity of the data volume the system booted from.
    pub capacity_total_bytes: u64,
    /// Free space on the data volume.
    pub capacity_available_bytes: u64,
    /// Used fraction of the data volume, 0..1.
    pub capacity_used_ratio: f64,
    /// Every real, sized filesystem currently mounted.
    pub filesystems: Vec<Filesystem>,
}

/// One mounted filesystem's capacity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Filesystem {
    /// Mount point.
    pub mount: String,
    /// Filesystem type (apfs, hfs, ...).
    pub fs_type: String,
    /// Capacity.
    pub total_bytes: u64,
    /// Free space.
    pub available_bytes: u64,
    /// Used fraction, 0..1.
    pub used_ratio: f64,
}

/// Drive health: NVMe SMART, APFS per-volume cache behaviour, and controller
/// throttle counters. The health tier; `null` when `storage_health` is off.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Storage {
    /// The NVMe SMART log page, when the drive exposes it.
    pub smart: Option<Smart>,
    /// Controller-side throttle and flash-traffic counters.
    pub controller: Controller,
    /// APFS per-volume cache and write behaviour (distinct from
    /// [`Disk::filesystems`], which is capacity).
    pub volumes: Vec<Volume>,
}

/// The NVMe SMART / Health Information log page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Smart {
    /// Fault bit field; any bit set is a controller-reported problem.
    pub critical_warning: u8,
    /// Composite drive temperature, when reported.
    pub temperature_c: Option<i64>,
    /// Spare capacity remaining, 0..1.
    pub available_spare_ratio: f64,
    /// Threshold below which the drive considers itself failing, 0..1.
    pub available_spare_threshold_ratio: f64,
    /// Rated write endurance consumed, 0..1 (can exceed 1).
    pub used_ratio: f64,
    /// Bytes read over the drive's life. Narrowed from the log's 128-bit
    /// counter to u64 (saturating); values past 2^53 lose precision in JSON
    /// parsers that use doubles.
    pub bytes_read: u64,
    /// Bytes written over the drive's life (same u64 narrowing as `bytes_read`).
    pub bytes_written: u64,
    /// Power-cycle count over the drive's life.
    pub power_cycles: u64,
    /// Powered-on hours over the drive's life.
    pub power_on_hours: u64,
    /// Power losses without a clean shutdown.
    pub unsafe_shutdowns: u64,
    /// Unrecovered media errors.
    pub media_errors: u64,
    /// Entries in the controller error log.
    pub error_log_entries: u64,
    /// True when the controller is flagging a real problem.
    pub unhealthy: bool,
}

/// Controller-side counters only IOReport publishes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Controller {
    /// Share of the window the drive spent thermally throttled.
    pub throttled_ratio: Option<f64>,
    /// Bytes the controller moved to flash over the window.
    pub nand_written_bytes: u64,
}

/// One APFS volume's cache and write behaviour.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Volume {
    /// APFS volume name.
    pub name: String,
    /// Bytes userspace asked to read.
    pub user_read_bytes: u64,
    /// Bytes that actually reached the device (the gap is the cache).
    pub device_read_bytes: u64,
    /// Bytes userspace asked to write.
    pub user_write_bytes: u64,
    /// Bytes that actually reached the device.
    pub device_write_bytes: u64,
    /// Share of reads served without touching the device, when anything was
    /// read.
    pub cache_hit_ratio: Option<f64>,
    /// Device bytes written per byte userspace wrote.
    pub write_amplification: Option<f64>,
}

/// Battery and adapter state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Battery {
    /// Current charge, 0..1.
    pub charge_ratio: f64,
    /// Whether the pack is charging.
    pub charging: bool,
    /// Whether an adapter is connected.
    pub external_power: bool,
    /// Whether the OS considers the pack full.
    pub fully_charged: bool,
    /// Signed: positive while charging, negative while discharging.
    pub battery_w: f64,
    /// Adapter wattage, when connected.
    pub adapter_w: Option<f64>,
    /// Adapter name, when reported.
    pub adapter_name: Option<String>,
    /// Charge cycles the pack has been through.
    pub cycle_count: u64,
    /// Cycles the pack is rated for.
    pub design_cycles: Option<u64>,
    /// Cycles used as a fraction of the pack's rated cycle life.
    pub cycle_ratio: Option<f64>,
    /// Current full-charge capacity vs design capacity, 0..1.
    pub health_ratio: f64,
    /// Pack temperature.
    pub temp_c: f64,
    /// Minutes to full (charging) or empty (discharging), when the OS knows.
    pub minutes_remaining: Option<u64>,
    /// Non-zero reason code when on AC but not charging.
    pub not_charging_reason: Option<u64>,
    /// Seconds of charging the pack has spent thermally limited.
    pub thermally_limited_secs: Option<u64>,
    /// The optimized-charging band the pack has lived in recently.
    pub daily_soc: Option<DailySoc>,
    /// Highest pack temperature ever recorded.
    pub lifetime_max_temp_c: Option<f64>,
    /// Per-cell voltages.
    pub cell_voltages_mv: Vec<u64>,
    /// Spread between the highest and lowest cell, the earliest sign of a
    /// failing pack.
    pub cell_imbalance_mv: Option<u64>,
    /// Present charge in mAh.
    pub raw_capacity_mah: Option<u64>,
    /// Full-charge capacity in mAh.
    pub raw_max_capacity_mah: Option<u64>,
}

/// The optimized-charging state-of-charge band, as ratios.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DailySoc {
    /// Low end of the optimized-charging band, 0..1.
    pub min_ratio: f64,
    /// High end of the optimized-charging band, 0..1.
    pub max_ratio: f64,
}

/// The process table summary and the top consumers by CPU.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Processes {
    /// Total processes on the system.
    pub total: u64,
    /// Processes currently running (not sleeping or idle).
    pub running: u64,
    /// Threads across readable processes (a lower bound without sudo).
    pub threads_visible: u64,
    /// True when at least one process was unreadable.
    pub restricted: bool,
    /// Busiest processes by CPU, most-active first, capped at 12.
    pub top: Vec<Proc>,
}

/// One process, enriched where permissions allowed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Proc {
    /// Process ID.
    pub pid: i64,
    /// Parent process ID.
    pub ppid: i64,
    /// Owning username.
    pub user: String,
    /// Executable name.
    pub name: String,
    /// Executable path, when readable.
    pub path: Option<String>,
    /// "running", "sleeping", "idle", "stopped", "zombie", or "unknown".
    pub state: String,
    /// CPU as a fraction of one core; can exceed 1 for multithreaded work.
    pub cpu_ratio: Option<f64>,
    /// Physical footprint (Activity Monitor's memory column).
    pub memory_bytes: Option<u64>,
    /// Average power over the window, from per-process energy counters.
    pub power_w: Option<f64>,
    /// Instructions retired per cycle.
    pub ipc: Option<f64>,
    /// Fraction of the window's cycles spent on P-cluster cores.
    pub p_share_ratio: Option<f64>,
    /// Disk read rate.
    pub disk_read_bytes_per_sec: Option<u64>,
    /// Disk write rate.
    pub disk_write_bytes_per_sec: Option<u64>,
    /// Thread count, when readable.
    pub threads: Option<i64>,
    /// Cumulative CPU time.
    pub cpu_time_secs: Option<u64>,
    /// Context switches per second.
    pub csw_per_sec: Option<f64>,
    /// System calls per second.
    pub syscalls_per_sec: Option<f64>,
    /// Interrupt-driven wakeups per second.
    pub wakeups_per_sec: Option<f64>,
    /// Seconds runnable-but-not-running per wall second (scheduler contention).
    pub runnable: Option<f64>,
    /// Share of CPU time requested at interactive QoS, 0..1.
    pub qos_interactive_ratio: Option<f64>,
    /// Share of CPU time requested at background QoS, 0..1.
    pub qos_background_ratio: Option<f64>,
}

/// Per-connection network flows.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Flows {
    /// All live sources, including ones not shown in `top`.
    pub count: u64,
    /// Total receive rate across all connections.
    pub rx_bytes_per_sec: u64,
    /// Total transmit rate across all connections.
    pub tx_bytes_per_sec: u64,
    /// Busiest connections, most-active first, capped at 10.
    pub top: Vec<Flow>,
}

/// One connection.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Flow {
    /// Owning process ID.
    pub pid: i64,
    /// Owning process name.
    pub name: String,
    /// Local address:port.
    pub local: String,
    /// Remote address:port.
    pub remote: String,
    /// TCP state name, or empty for UDP.
    pub state: String,
    /// True for UDP, false for TCP.
    pub udp: bool,
    /// Receive rate.
    pub rx_bytes_per_sec: u64,
    /// Transmit rate.
    pub tx_bytes_per_sec: u64,
    /// Bytes received over the connection's life.
    pub rx_total_bytes: u64,
    /// Bytes sent over the connection's life.
    pub tx_total_bytes: u64,
    /// Smoothed round-trip time; `null` for UDP or before any measurement.
    pub rtt_ms: Option<f64>,
    /// Lifetime retransmitted share of transmitted bytes, 0..1.
    pub retransmit_ratio: Option<f64>,
}

/// Kernel activity: scheduler rates (from the process pass), interrupt sources,
/// and sleep blockers (from the `kernel_stats` collector).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Kernel {
    /// System-wide scheduler rates. `null` if the process pass has not settled.
    pub rates: Option<KernelRates>,
    /// Interrupt sources. `null` when `kernel_stats` is off.
    pub interrupts: Option<Interrupts>,
    /// What is currently holding the machine awake. `null` when `kernel_stats`
    /// is off.
    pub sleep_blockers: Option<Vec<SleepBlocker>>,
}

/// System-wide kernel activity rates.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KernelRates {
    /// System-wide context switches per second.
    pub context_switches_per_sec: f64,
    /// System-wide system calls per second.
    pub syscalls_per_sec: f64,
    /// System-wide Mach messages per second.
    pub mach_messages_per_sec: f64,
    /// System-wide interrupt-driven wakeups per second.
    pub interrupt_wakeups_per_sec: f64,
    /// Runnable-but-not-running thread-seconds per wall second.
    pub runnable_threads: f64,
}

/// Interrupt activity, aggregated to the busiest sources.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Interrupts {
    /// Total interrupts per second across all sources.
    pub total_per_sec: f64,
    /// Busiest interrupt sources, most-active first.
    pub top_sources: Vec<InterruptSource>,
}

/// One hardware block's interrupt activity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InterruptSource {
    /// Hardware block, e.g. "gfx-asc", "ans", "usb-drd0".
    pub device: String,
    /// Interrupts per second from this device.
    pub per_sec: f64,
    /// Handler time as a share of the window (the part that costs CPU).
    pub handler_cpu_ratio: f64,
}

/// One reason the machine is being held awake.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SleepBlocker {
    /// Process holding the assertion.
    pub pid: i64,
    /// Assertion type, e.g. "PreventUserIdleSystemSleep".
    pub kind: String,
    /// Human-readable reason the owner supplied, when any.
    pub reason: Option<String>,
}

/// Active connectivity probe (ICMP echo).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Ping {
    /// The probed host.
    pub host: String,
    /// This probe's round trip; `null` on timeout.
    pub rtt_ms: Option<f64>,
    /// Smoothed (EMA) latency.
    pub latency_ms: Option<f64>,
    /// Smoothed mean inter-probe variation (jitter).
    pub jitter_ms: Option<f64>,
    /// Reachability, debounced by one miss.
    pub up: bool,
}
