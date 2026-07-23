//! `Inputs -> Report`: the one place internal collector samples become the v1
//! contract. Every conversion routes numbers through [`super::norm`], so the
//! unit convention lives in exactly one module and a renamed or dropped
//! internal field fails to compile here rather than silently changing the wire
//! format.

use super::model::{
    Battery, Cluster, Controller, Core, Cpu, DailySoc, Disk, Features, Filesystem, Flow, Flows, Fan,
    Gpu, InterruptSource, Interrupts, Kernel, KernelRates, Memory, Meta, Network, Ping, Power,
    PrimaryIf, Proc, Processes, Report, SampleWindow, Sensor, SleepBlocker, Smart, Soc, SourceError,
    Storage, Thermal, Volume,
};
use super::norm;
use crate::collect::battery::{BatterySample, cell_imbalance_mv};
use crate::collect::disk::DiskSample;
use crate::collect::gpu::GpuSample;
use crate::collect::kernel::{InterruptSource as HwInterrupt, KernelSnapshot};
use crate::collect::mem::MemSample;
use crate::collect::net::{NetSample, PrimaryIf as HwPrimaryIf};
use crate::collect::flows::{Flow as HwFlow, FlowSample};
use crate::collect::ping::PingSample;
use crate::collect::power::{ClusterSample, CoreSample, PowerSample};
use crate::collect::procs::{ProcRow, ProcSample};
use crate::collect::sampler::{self, FastSnapshot};
use crate::collect::soc::SocInfo;
use crate::collect::storage::StorageSample;
use crate::collect::temps::TempSample;
use crate::ffi::nvme::Smart as HwSmart;
use crate::units::Ratio;

/// Everything a report is built from: the machine facts plus the latest sample
/// per tier (each `None` when its source is down, disabled, or unsettled), the
/// startup errors, and the run's settle/feature context.
pub struct Inputs<'a> {
    pub soc: &'a SocInfo,
    pub fast: Option<&'a FastSnapshot>,
    pub power: Option<&'a PowerSample>,
    pub temps: Option<&'a TempSample>,
    pub battery: Option<&'a BatterySample>,
    pub procs: Option<&'a ProcSample>,
    pub flows: Option<&'a FlowSample>,
    pub ping: Option<&'a PingSample>,
    pub storage: Option<&'a StorageSample>,
    pub kernel: Option<&'a KernelSnapshot>,
    pub errors: &'a [(String, String)],
    pub fast_ms: u64,
    pub ping_on: bool,
    pub storage_health_on: bool,
    pub kernel_stats_on: bool,
    pub settled: bool,
}

impl Report {
    /// Build the v1 report. Pure: no sampling, no I/O beyond one cheap
    /// `getfsstat` for the filesystem list.
    pub fn build(i: &Inputs) -> Report {
        Report {
            meta: meta(i),
            soc: soc(i.soc),
            cpu: i.fast.map(cpu),
            gpu: i.fast.and_then(|f| f.gpu.as_ref()).map(gpu),
            memory: i.fast.and_then(|f| f.mem.as_ref()).map(memory),
            power: i.power.map(power),
            thermal: i.temps.map(|t| thermal(t, i.soc)),
            network: i.fast.and_then(|f| f.net.as_ref()).map(network),
            disk: i.fast.and_then(|f| f.disk.as_ref()).map(disk),
            storage: i.storage.map(storage),
            battery: i.battery.map(battery),
            processes: i.procs.map(processes),
            flows: i.flows.map(flows),
            kernel: kernel(i.procs, i.kernel),
            ping: i.ping.map(ping),
            source_errors: i
                .errors
                .iter()
                .map(|(source, error)| SourceError {
                    source: source.clone(),
                    error: error.clone(),
                })
                .collect(),
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn meta(i: &Inputs) -> Meta {
    Meta {
        schema_version: super::SCHEMA_VERSION,
        mxmon_version: env!("CARGO_PKG_VERSION").to_owned(),
        generated_unix: unix_now(),
        settled: i.settled,
        sample_window: SampleWindow {
            fast_ms: i.fast_ms,
            power_ms: i.fast_ms * sampler::POWER_EVERY,
            procs_ms: i.fast_ms * sampler::PROCS_EVERY,
            flows_ms: i.fast_ms * sampler::FLOWS_EVERY,
        },
        features: Features {
            ping: i.ping_on,
            storage_health: i.storage_health_on,
            kernel_stats: i.kernel_stats_on,
        },
    }
}

fn soc(s: &SocInfo) -> Soc {
    let mhz = |v: &[crate::units::Mhz]| v.iter().map(|f| norm::mhz(*f)).collect();
    Soc {
        chip: s.chip_name.clone(),
        macos_version: s.macos_version.clone(),
        ecpu_cores: s.ecpu_count as u32,
        pcpu_cores: s.pcpu_count as u32,
        cores_per_pcluster: s.cores_per_pcluster as u32,
        gpu_cores: s.gpu_core_count,
        memory_bytes: s.memory_bytes,
        ecpu_freqs_mhz: mhz(&s.ecpu_freqs),
        pcpu_freqs_mhz: mhz(&s.pcpu_freqs),
        gpu_freqs_mhz: mhz(&s.gpu_freqs),
        tier_low: s.tier_low.to_string(),
        tier_high: s.tier_high.to_string(),
    }
}

fn cpu(f: &FastSnapshot) -> Cpu {
    Cpu {
        per_core_ratio: f
            .cpu
            .as_ref()
            .map(|c| c.per_core.iter().map(|r| norm::ratio(*r)).collect())
            .unwrap_or_default(),
        load_avg: f.load,
        uptime_secs: f.uptime_secs,
        self_ratio: norm::ratio_f64(f64::from(f.self_cpu)),
    }
}

fn gpu(g: &GpuSample) -> Gpu {
    Gpu {
        device_ratio: norm::ratio(g.device),
        renderer_ratio: norm::ratio(g.renderer),
        tiler_ratio: norm::ratio(g.tiler),
        used_memory_bytes: norm::bytes(g.used_memory),
    }
}

fn memory(m: &MemSample) -> Memory {
    Memory {
        total_bytes: norm::bytes(m.total),
        used_bytes: norm::bytes(m.used),
        app_bytes: norm::bytes(m.app),
        wired_bytes: norm::bytes(m.wired),
        compressed_bytes: norm::bytes(m.compressed),
        cached_bytes: norm::bytes(m.cached),
        swap_used_bytes: norm::bytes(m.swap_used),
        swap_total_bytes: norm::bytes(m.swap_total),
        used_ratio: norm::ratio(m.used_ratio()),
        pressure: m.pressure.label().to_owned(),
    }
}

fn power(p: &PowerSample) -> Power {
    Power {
        package_w: norm::watts(p.package()),
        cpu_w: norm::watts(p.cpu),
        gpu_w: norm::watts(p.gpu),
        ane_w: norm::watts(p.ane),
        dram_w: norm::watts(p.dram),
        display_w: norm::watts(p.display),
        display_ext_w: norm::watts(p.display_ext),
        gpu_sram_w: norm::watts(p.gpu_sram),
        amcc_w: norm::watts(p.amcc),
        dcs_w: norm::watts(p.dcs),
        video_w: norm::watts(p.video),
        isp_w: norm::watts(p.isp),
        scaler_w: norm::watts(p.scaler),
        gpu_cs_w: norm::watts(p.gpu_cs),
        ecpu: cluster(&p.ecpu),
        pcpu: cluster(&p.pcpu),
        gpu_freq_mhz: norm::mhz(p.gpu_freq),
        gpu_usage_ratio: norm::ratio(p.gpu_usage),
        gpu_active_ratio: norm::ratio(p.gpu_active),
    }
}

fn cluster(c: &ClusterSample) -> Cluster {
    Cluster {
        freq_mhz: norm::mhz(c.freq),
        usage_ratio: norm::ratio(c.usage),
        cores: c.cores.iter().map(core).collect(),
    }
}

fn core(c: &CoreSample) -> Core {
    Core {
        freq_mhz: norm::mhz(c.freq),
        usage_ratio: norm::ratio(c.usage),
        power_w: c.watts.map(norm::watts),
    }
}

fn thermal(t: &TempSample, soc: &SocInfo) -> Thermal {
    Thermal {
        cpu_avg_c: norm::celsius(t.cpu_avg),
        cpu_max_c: norm::celsius(t.cpu_max),
        gpu_avg_c: norm::celsius(t.gpu_avg),
        gpu_max_c: norm::celsius(t.gpu_max),
        pressure: t.pressure.map(|p| p.label().to_owned()),
        throttling: t.pressure.map(crate::ffi::notify::Pressure::throttling),
        severity: t.pressure.map(|p| norm::ratio(Ratio(p.severity()))),
        sys_power_w: t.sys_power.map(norm::watts),
        adapter_power_w: t.adapter_power.map(norm::watts),
        backlight_power_w: t.backlight_power.map(norm::watts),
        sensors: t
            .sensors
            .iter()
            .map(|s| Sensor {
                label: s.label.clone(),
                group: s.group.title_with(soc.tier_low, soc.tier_high),
                temp_c: norm::celsius(s.temp),
            })
            .collect(),
        fans: t
            .fans
            .iter()
            .map(|f| Fan {
                label: f.label.clone(),
                rpm: f64::from(f.rpm),
                max_rpm: f64::from(f.max_rpm),
                ratio: (f.max_rpm > 0.0).then(|| norm::ratio(Ratio(f.rpm / f.max_rpm))),
            })
            .collect(),
    }
}

fn network(n: &NetSample) -> Network {
    Network {
        rx_bytes_per_sec: norm::bytes(n.rx_per_sec),
        tx_bytes_per_sec: norm::bytes(n.tx_per_sec),
        rx_session_bytes: norm::bytes(n.rx_session),
        tx_session_bytes: norm::bytes(n.tx_session),
        primary: n.primary.as_ref().map(primary_if),
    }
}

fn primary_if(p: &HwPrimaryIf) -> PrimaryIf {
    PrimaryIf {
        name: p.name.clone(),
        link_speed_bps: p.baudrate,
        ipv4: p.ipv4.clone(),
        mac: p.mac.clone(),
        link_up: p.running,
    }
}

fn disk(d: &DiskSample) -> Disk {
    let used = d.root_total.0.saturating_sub(d.root_available.0);
    let used_ratio = if d.root_total.0 > 0 {
        used as f64 / d.root_total.0 as f64
    } else {
        0.0
    };
    Disk {
        read_bytes_per_sec: norm::bytes(d.read_per_sec),
        write_bytes_per_sec: norm::bytes(d.write_per_sec),
        read_iops: u64::from(d.read_iops),
        write_iops: u64::from(d.write_iops),
        read_latency_us: d.read_lat_us.map(norm::us),
        write_latency_us: d.write_lat_us.map(norm::us),
        read_session_bytes: norm::bytes(d.read_session),
        write_session_bytes: norm::bytes(d.write_session),
        devices: d.devices as u64,
        capacity_total_bytes: norm::bytes(d.root_total),
        capacity_available_bytes: norm::bytes(d.root_available),
        capacity_used_ratio: norm::ratio_f64(used_ratio),
        filesystems: filesystems(),
    }
}

/// Real, sized filesystems only; the synthetic and nullfs mounts would drown
/// the useful ones (matches the set the TUI's inspector shows).
fn filesystems() -> Vec<Filesystem> {
    crate::ffi::sys::mounts()
        .into_iter()
        .filter(|m| {
            m.total > 0 && matches!(m.fs_type.as_str(), "apfs" | "hfs" | "exfat" | "msdos" | "ntfs")
        })
        .map(|m| {
            let used = m.total.saturating_sub(m.available);
            Filesystem {
                used_ratio: norm::ratio_f64(used as f64 / m.total as f64),
                mount: m.mount_point,
                fs_type: m.fs_type,
                total_bytes: m.total,
                available_bytes: m.available,
            }
        })
        .collect()
}

fn storage(s: &StorageSample) -> Storage {
    Storage {
        smart: s.smart.as_ref().map(smart),
        controller: Controller {
            throttled_ratio: s.controller.throttled.map(norm::ratio),
            nand_written_bytes: norm::bytes(s.controller.nand_written),
        },
        volumes: s
            .volumes
            .iter()
            .map(|v| Volume {
                name: v.name.clone(),
                user_read_bytes: norm::bytes(v.user_read),
                device_read_bytes: norm::bytes(v.device_read),
                user_write_bytes: norm::bytes(v.user_write),
                device_write_bytes: norm::bytes(v.device_write),
                cache_hit_ratio: v.cache_hit().map(norm::ratio),
                write_amplification: v.write_amplification().map(f64::from),
            })
            .collect(),
    }
}

fn smart(s: &HwSmart) -> Smart {
    Smart {
        critical_warning: s.critical_warning,
        temperature_c: s.temperature_c.map(i64::from),
        available_spare_ratio: norm::percent_to_ratio(f64::from(s.available_spare_pct)),
        available_spare_threshold_ratio: norm::percent_to_ratio(f64::from(
            s.available_spare_threshold_pct,
        )),
        used_ratio: norm::percent_to_ratio(f64::from(s.percentage_used)),
        bytes_read: norm::wide(s.bytes_read),
        bytes_written: norm::wide(s.bytes_written),
        power_cycles: norm::wide(s.power_cycles),
        power_on_hours: norm::wide(s.power_on_hours),
        unsafe_shutdowns: norm::wide(s.unsafe_shutdowns),
        media_errors: norm::wide(s.media_errors),
        error_log_entries: norm::wide(s.error_log_entries),
        unhealthy: s.unhealthy(),
    }
}

fn battery(b: &BatterySample) -> Battery {
    Battery {
        charge_ratio: norm::ratio(b.charge),
        charging: b.charging,
        external_power: b.external_power,
        fully_charged: b.fully_charged,
        battery_w: norm::watts(b.battery_watts),
        adapter_w: b.adapter_watts.map(norm::watts),
        adapter_name: b.adapter_name.clone(),
        cycle_count: u64::from(b.cycle_count),
        design_cycles: b.design_cycles.map(u64::from),
        cycle_ratio: b
            .design_cycles
            .filter(|&d| d > 0)
            .map(|d| norm::ratio_f64(f64::from(b.cycle_count) / f64::from(d))),
        health_ratio: norm::ratio(b.health),
        temp_c: norm::celsius(b.temp),
        minutes_remaining: b.minutes_remaining.map(u64::from),
        not_charging_reason: b.not_charging_reason,
        thermally_limited_secs: b.thermally_limited_secs,
        daily_soc: b.daily_soc.map(|(lo, hi)| DailySoc {
            min_ratio: norm::percent_to_ratio(f64::from(lo)),
            max_ratio: norm::percent_to_ratio(f64::from(hi)),
        }),
        lifetime_max_temp_c: b.lifetime_max_temp.map(norm::celsius),
        cell_voltages_mv: b.cell_voltages.iter().map(|&v| u64::from(v)).collect(),
        cell_imbalance_mv: cell_imbalance_mv(&b.cell_voltages).map(u64::from),
        raw_capacity_mah: b.raw_capacity_mah.map(u64::from),
        raw_max_capacity_mah: b.raw_max_capacity_mah.map(u64::from),
    }
}

fn processes(p: &ProcSample) -> Processes {
    let mut top: Vec<&ProcRow> = p.rows.iter().collect();
    top.sort_by(|a, b| {
        b.cpu
            .map_or(0.0, |r| r.0)
            .total_cmp(&a.cpu.map_or(0.0, |r| r.0))
    });
    top.truncate(12);
    Processes {
        total: p.total as u64,
        running: p.running as u64,
        threads_visible: p.threads as u64,
        restricted: p.restricted,
        top: top.into_iter().map(proc_row).collect(),
    }
}

fn proc_row(r: &ProcRow) -> Proc {
    Proc {
        pid: i64::from(r.pid),
        ppid: i64::from(r.ppid),
        user: r.user.clone(),
        name: r.name.clone(),
        path: r.path.clone(),
        state: r.state.label().to_owned(),
        cpu_ratio: r.cpu.map(norm::ratio),
        memory_bytes: r.memory.map(norm::bytes),
        power_w: r.power.map(norm::watts),
        ipc: r.ipc.map(|v| norm::small(f64::from(v))),
        p_share_ratio: r.p_share.map(norm::ratio),
        disk_read_bytes_per_sec: r.disk_read_rate.map(norm::bytes),
        disk_write_bytes_per_sec: r.disk_write_rate.map(norm::bytes),
        threads: r.threads.map(i64::from),
        cpu_time_secs: r.cpu_time_secs,
        csw_per_sec: r.csw_rate.map(norm::rate),
        syscalls_per_sec: r.syscall_rate.map(norm::rate),
        wakeups_per_sec: r.wakeup_rate.map(norm::rate),
        runnable: r.runnable.map(norm::small),
        qos_interactive_ratio: r.qos_interactive.map(norm::ratio),
        qos_background_ratio: r.qos_background.map(norm::ratio),
    }
}

fn flows(fl: &FlowSample) -> Flows {
    Flows {
        count: fl.count as u64,
        rx_bytes_per_sec: fl.rx_total_rate,
        tx_bytes_per_sec: fl.tx_total_rate,
        top: fl.flows.iter().take(10).map(flow).collect(),
    }
}

fn flow(f: &HwFlow) -> Flow {
    Flow {
        pid: i64::from(f.pid),
        name: f.pname.clone(),
        local: f.local.clone(),
        remote: f.remote.clone(),
        state: f.state.to_owned(),
        udp: f.udp,
        rx_bytes_per_sec: norm::bytes(f.rx_rate),
        tx_bytes_per_sec: norm::bytes(f.tx_rate),
        rx_total_bytes: norm::bytes(f.rx_total),
        tx_total_bytes: norm::bytes(f.tx_total),
        rtt_ms: f.srtt_ms.map(norm::ms),
        retransmit_ratio: f.retx_pct.map(|v| norm::percent_to_ratio(f64::from(v))),
    }
}

fn kernel(procs: Option<&ProcSample>, ks: Option<&KernelSnapshot>) -> Option<Kernel> {
    if procs.is_none() && ks.is_none() {
        return None;
    }
    Some(Kernel {
        rates: procs.map(|p| KernelRates {
            context_switches_per_sec: norm::rate(p.kernel.context_switches),
            syscalls_per_sec: norm::rate(p.kernel.syscalls),
            mach_messages_per_sec: norm::rate(p.kernel.mach_messages),
            interrupt_wakeups_per_sec: norm::rate(p.kernel.interrupt_wakeups),
            runnable_threads: norm::small(p.kernel.runnable),
        }),
        interrupts: ks.map(|k| Interrupts {
            total_per_sec: norm::rate(k.total_per_sec),
            top_sources: k.top_sources.iter().map(interrupt_source).collect(),
        }),
        sleep_blockers: ks.map(|k| {
            k.sleep_blockers()
                .into_iter()
                .map(|a| SleepBlocker {
                    pid: i64::from(a.pid),
                    kind: a.kind.clone(),
                    reason: a.name.clone(),
                })
                .collect()
        }),
    })
}

fn interrupt_source(s: &HwInterrupt) -> InterruptSource {
    InterruptSource {
        device: s.device.clone(),
        per_sec: norm::rate(s.per_sec),
        handler_cpu_ratio: norm::ratio_f64(s.cpu_share),
    }
}

fn ping(p: &PingSample) -> Ping {
    Ping {
        host: p.host.clone(),
        rtt_ms: p.rtt_ms.map(norm::ms),
        latency_ms: p.latency_ms.map(norm::ms),
        jitter_ms: p.jitter_ms.map(norm::ms),
        up: p.up,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{Bytes, Watts};

    #[test]
    fn network_throughput_stays_in_bytes_not_bits() {
        let n = NetSample {
            rx_per_sec: Bytes(1_000_000),
            ..Default::default()
        };
        let dto = network(&n);
        assert_eq!(dto.rx_bytes_per_sec, 1_000_000);
    }

    #[test]
    fn absent_per_core_rail_is_null_not_zero() {
        let with = core(&CoreSample {
            watts: Some(Watts(1.5)),
            ..Default::default()
        });
        let without = core(&CoreSample {
            watts: None,
            ..Default::default()
        });
        assert_eq!(with.power_w, Some(1.5));
        assert_eq!(without.power_w, None);
    }

    #[test]
    fn smart_percentages_become_ratios() {
        let s = HwSmart {
            available_spare_pct: 100,
            available_spare_threshold_pct: 10,
            percentage_used: 3,
            bytes_written: 717_000_000_000,
            ..Default::default()
        };
        let dto = smart(&s);
        assert!((dto.available_spare_ratio - 1.0).abs() < 1e-9);
        assert!((dto.used_ratio - 0.03).abs() < 1e-9);
        assert_eq!(dto.bytes_written, 717_000_000_000);
    }

    #[test]
    fn kernel_is_null_only_when_both_sources_absent() {
        assert!(kernel(None, None).is_none());
        let procs = ProcSample::default();
        let only_rates = kernel(Some(&procs), None).unwrap();
        assert!(only_rates.rates.is_some());
        assert!(only_rates.interrupts.is_none());
        assert!(only_rates.sleep_blockers.is_none());
    }
}
