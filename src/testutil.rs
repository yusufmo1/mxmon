//! Deterministic fixtures shared by unit, snapshot, and render-fuzz tests.
//!
//! Everything here is synthetic and machine-independent: no sockets, no
//! IOKit. An `App` built from these folds every update through the
//! production `App::apply` path and draws the same frame on any host —
//! which is exactly what the snapshot tests key on. The single wall-clock
//! read is `now_sec`, which pins process start times to a *fixed age*; see
//! [`procs`] for why an absolute epoch would make frames drift.

// Fixtures are a shared pool consumed piecemeal by test modules across the
// crate; any single build sees only a subset of them referenced.
#![allow(dead_code)]

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::App;
use crate::collect::battery::BatterySample;
use crate::collect::cpu::CpuSample;
use crate::collect::disk::DiskSample;
use crate::collect::flows::{Flow, FlowSample, aggregate_by_pid};
use crate::collect::gpu::GpuSample;
use crate::collect::mem::{MemSample, Pressure};
use crate::collect::net::{NetSample, PrimaryIf};
use crate::collect::ping::PingSample;
use crate::collect::power::{ClusterSample, PowerSample};
use crate::collect::procs::{ProcRow, ProcSample, ProcState};
use crate::collect::sampler::{FastSnapshot, SlowSnapshot, Update};
use crate::collect::soc::SocInfo;
use crate::collect::temps::{Fan, Sensor, SensorGroup, TempSample};
use crate::config::Config;
use crate::units::{Bytes, Celsius, Mhz, Ratio, Watts};

/// Deterministic triangle wave in `0.0..=1.0` (period `period` steps) — the
/// stand-in for "live-looking" variation without any randomness.
fn tri(i: usize, period: usize) -> f32 {
    let period = period.max(2);
    let p = i % period;
    let half = period / 2;
    let up = if p <= half { p } else { period - p };
    up as f32 / half as f32
}

/// M3-Max-like machine facts.
pub fn soc() -> SocInfo {
    SocInfo {
        chip_name: "Apple M3 Max".into(),
        macos_version: "26.5".into(),
        ecpu_count: 4,
        pcpu_count: 12,
        tier_low: 'E',
        tier_high: 'P',
        cores_per_pcluster: 6,
        gpu_core_count: Some(40),
        memory_bytes: 48 << 30,
        ecpu_freqs: [744, 1044, 1476, 1836, 2004, 2256, 2532, 2748]
            .map(Mhz)
            .to_vec(),
        pcpu_freqs: [
            1080, 1440, 1800, 2160, 2448, 2772, 3096, 3624, 3708, 3852, 3948, 4056,
        ]
        .map(Mhz)
        .to_vec(),
        gpu_freqs: [444, 612, 808, 968, 1110, 1236, 1338, 1398]
            .map(Mhz)
            .to_vec(),
    }
}

/// One fast-tier snapshot at synthetic tick `i` (every source present).
pub fn fast_at(i: usize) -> FastSnapshot {
    let per_core: Vec<Ratio> = (0..16)
        .map(|c| Ratio((0.08 + 0.72 * tri(i + c * 3, 24)).min(1.0)))
        .collect();
    FastSnapshot {
        cpu: Some(CpuSample { per_core }),
        gpu: Some(GpuSample {
            device: Ratio(0.12 + 0.38 * tri(i, 30)),
            renderer: Ratio(0.10 + 0.44 * tri(i + 4, 30)),
            tiler: Ratio(0.05 + 0.22 * tri(i + 9, 30)),
            used_memory: Bytes(3_517_245_440),
        }),
        mem: Some(MemSample {
            total: Bytes(48 << 30),
            used: Bytes((22 << 30) + ((i as u64 * 61_303_808) % (3 << 30))),
            app: Bytes(12 << 30),
            wired: Bytes(6 << 30),
            compressed: Bytes(4 << 30),
            cached: Bytes(9 << 30),
            swap_used: Bytes(1 << 29),
            swap_total: Bytes(2 << 30),
            pressure: Pressure::Normal,
        }),
        net: Some(NetSample {
            rx_per_sec: Bytes(140_000 + (i as u64 * 733_331) % 5_800_000),
            tx_per_sec: Bytes(38_000 + (i as u64 * 273_449) % 900_000),
            rx_session: Bytes(1_264_000_000 + i as u64 * 2_100_000),
            tx_session: Bytes(240_000_000 + i as u64 * 400_000),
            primary: Some(PrimaryIf {
                name: "en0".into(),
                baudrate: 1_000_000_000,
                ipv4: Some("192.168.1.24".into()),
                mac: Some("b4:e9:b8:6d:3b:d6".into()),
                running: true,
            }),
        }),
        disk: Some(DiskSample {
            read_per_sec: Bytes((9_400_000.0 * tri(i + 2, 16)) as u64),
            write_per_sec: Bytes((3_100_000.0 * tri(i + 11, 20)) as u64),
            read_iops: 220,
            write_iops: 95,
            read_lat_us: Some(284.0),
            write_lat_us: Some(451.0),
            read_session: Bytes(18_600_000_000 + i as u64 * 9_400_000),
            write_session: Bytes(4_100_000_000 + i as u64 * 3_100_000),
            devices: 2,
        }),
        load: [3.42, 2.87, 2.51],
        uptime_secs: 26 * 3600 + 14 * 60 + 9,
        self_cpu: 0.004,
    }
}

/// One power-tier sample at synthetic tick `i`.
pub fn power_at(i: usize) -> PowerSample {
    let e_cores: Vec<(Mhz, Ratio)> = (0..4)
        .map(|c| {
            (
                Mhz(1044 + 400 * c as u32 / 3),
                Ratio(0.2 + 0.6 * tri(i + c * 5, 18)),
            )
        })
        .collect();
    let p_cores: Vec<(Mhz, Ratio)> = (0..12)
        .map(|c| {
            (
                Mhz(1800 + 180 * c as u32),
                Ratio((0.05 + 0.85 * tri(i + c * 2, 22)).min(1.0)),
            )
        })
        .collect();
    PowerSample {
        cpu: Watts(2.4 + 6.5 * tri(i, 20)),
        gpu: Watts(1.1 + 3.2 * tri(i + 7, 26)),
        ane: Watts(0.02),
        dram: Watts(0.9 + 0.5 * tri(i + 3, 14)),
        display: Watts(3.1),
        gpu_sram: Watts(0.12),
        ecpu: ClusterSample {
            freq: Mhz(1476 + ((i as u32 * 97) % 900)),
            usage: Ratio(0.35 + 0.3 * tri(i, 18)),
            cores: e_cores,
        },
        pcpu: ClusterSample {
            freq: Mhz(2448 + ((i as u32 * 131) % 1400)),
            usage: Ratio(0.25 + 0.5 * tri(i + 6, 22)),
            cores: p_cores,
        },
        gpu_freq: Mhz(808 + ((i as u32 * 53) % 500)),
        gpu_usage: Ratio(0.18 + 0.4 * tri(i + 7, 26)),
        gpu_active: Ratio(0.4 + 0.4 * tri(i + 7, 26)),
    }
}

/// One temps sample at synthetic tick `i` — labels shaped exactly like the
/// classified HID/SMC output so the thermal map places and tags them.
pub fn temps_at(i: usize) -> TempSample {
    let s = |label: &str, group: SensorGroup, base: f32, k: usize| Sensor {
        label: label.into(),
        group,
        temp: Celsius(base + 6.0 * tri(i + k * 3, 12)),
    };
    let mut sensors = Vec::new();
    for c in 0..4 {
        sensors.push(s(
            &format!("E-Core {c}"),
            SensorGroup::CpuECore,
            52.0 + c as f32,
            c,
        ));
    }
    for c in 0..12 {
        sensors.push(s(
            &format!("P-Core {c}"),
            SensorGroup::CpuPCore,
            58.0 + (c % 5) as f32,
            c + 4,
        ));
    }
    for c in 1..=4 {
        sensors.push(s(
            &format!("GPU Cluster {c}"),
            SensorGroup::Gpu,
            54.0 + c as f32,
            c + 16,
        ));
    }
    for c in 0..6 {
        sensors.push(s(
            &format!("Die {c}"),
            SensorGroup::Soc,
            49.0 + (c % 4) as f32,
            c + 20,
        ));
    }
    sensors.push(s("PMU 2", SensorGroup::Soc, 44.0, 27));
    sensors.push(s("ANE 0", SensorGroup::Ane, 41.0, 28));
    sensors.push(s("NAND", SensorGroup::Ssd, 38.5, 29));
    sensors.push(s("Battery", SensorGroup::Battery, 31.2, 30));
    sensors.push(s("Airflow L", SensorGroup::Airflow, 33.0, 31));
    sensors.push(s("Charger", SensorGroup::Charger, 36.4, 32));
    sensors.push(s("Trackpad", SensorGroup::Other, 27.9, 33));
    let cpu_avg = 57.0 + 9.0 * tri(i, 12);
    TempSample {
        cpu_avg: Celsius(cpu_avg),
        cpu_max: Celsius(cpu_avg + 7.5),
        gpu_avg: Celsius(53.0 + 8.0 * tri(i + 5, 14)),
        gpu_max: Celsius(60.0 + 8.0 * tri(i + 5, 14)),
        sensors,
        fans: vec![
            Fan {
                label: "Left".into(),
                rpm: 1740.0 + 900.0 * tri(i, 16),
                max_rpm: 5900.0,
            },
            Fan {
                label: "Right".into(),
                rpm: 1610.0 + 850.0 * tri(i + 2, 16),
                max_rpm: 6100.0,
            },
        ],
        sys_power: Some(Watts(21.0 + 14.0 * tri(i, 20))),
        adapter_power: Some(Watts(64.8)),
    }
}

pub fn battery() -> BatterySample {
    BatterySample {
        charge: Ratio(0.78),
        charging: true,
        external_power: true,
        fully_charged: false,
        battery_watts: Watts(12.4),
        adapter_watts: Some(Watts(96.0)),
        adapter_name: Some("96W USB-C Power Adapter".into()),
        cycle_count: 312,
        health: Ratio(0.91),
        temp: Celsius(31.2),
        minutes_remaining: Some(94),
    }
}

pub fn ping_at(i: usize) -> PingSample {
    PingSample {
        rtt_ms: Some(10.8 + 7.0 * tri(i, 10)),
        latency_ms: Some(12.3 + 2.0 * tri(i, 10)),
        jitter_ms: Some(0.8),
        up: true,
        host: "1.1.1.1".into(),
    }
}

/// Seconds since the Unix epoch, saturating to 0 on a pre-epoch clock.
fn now_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// `n` process rows with a deterministic spread of values; every ~7th row is
/// "restricted" (unreadable counters → `None`s) to exercise sink-sorting.
pub fn procs(n: usize) -> ProcSample {
    const NAMES: [&str; 16] = [
        "mxmon",
        "WindowServer",
        "kernel_task",
        "Safari",
        "claude",
        "cargo",
        "rust-analyzer",
        "Music",
        "Finder",
        "Terminal",
        "mds_stores",
        "bird",
        "Xcode",
        "node",
        "postgres",
        "nginx",
    ];
    const USERS: [&str; 3] = ["yusuf", "root", "_windowserver"];
    const STATES: [ProcState; 4] = [
        ProcState::Running,
        ProcState::Sleeping,
        ProcState::Idle,
        ProcState::Sleeping,
    ];
    // Process start times are the one fixture anchored to the wall clock,
    // because the details modal renders them as a *relative age*: a fixed
    // epoch would drift across format_duration's width boundaries
    // ("3d 7h" → "3d 13h") and silently rewrite every frame it appears in.
    // Fixing the age instead makes that string constant. The offset sits
    // half an hour off the hour boundary, so the ~ms between building the
    // fixture and rendering it can never round differently.
    let started = now_sec() - (3 * 86_400 + 7 * 3_600 + 1_800);
    let rows: Vec<ProcRow> = (0..n)
        .map(|i| {
            let restricted = i % 7 == 3;
            let name = NAMES[i % NAMES.len()];
            ProcRow {
                pid: 200 + (i as i32) * 17,
                ppid: 1,
                user: USERS[i % USERS.len()].into(),
                name: name.into(),
                path: (!restricted).then(|| format!("/usr/bin/{name}")),
                state: STATES[i % STATES.len()],
                cpu: (!restricted).then(|| Ratio(((i * 13) % 180) as f32 / 100.0)),
                memory: (!restricted).then(|| Bytes(((50 + (i * 37) % 3900) as u64) << 20)),
                power: (!restricted).then(|| Watts(((i * 29) % 900) as f32 / 1000.0)),
                ipc: (!restricted).then(|| 1.2 + ((i * 11) % 30) as f32 / 10.0),
                p_share: (!restricted).then(|| Ratio(((i * 19) % 100) as f32 / 100.0)),
                disk_read_rate: (!restricted).then(|| Bytes((i as u64 * 91_000) % 8_000_000)),
                disk_write_rate: (!restricted).then(|| Bytes((i as u64 * 47_000) % 3_000_000)),
                threads: Some(4 + (i as i32) % 23),
                cpu_time_secs: Some(120 + (i as u64) * 37),
                start_sec: started + i as i64,
            }
        })
        .collect();
    let running = rows
        .iter()
        .filter(|r| r.state == ProcState::Running)
        .count();
    let threads = rows.iter().filter_map(|r| r.threads).sum::<i32>() as usize;
    ProcSample {
        total: rows.len() + 120,
        running,
        threads,
        restricted: true,
        rows,
    }
}

pub fn flows() -> FlowSample {
    let f = |pid: i32,
             pname: &str,
             local: &str,
             remote: &str,
             state: &'static str,
             udp: bool,
             rx: u64,
             tx: u64,
             srtt: Option<f32>| Flow {
        pid,
        pname: pname.into(),
        local: local.into(),
        remote: remote.into(),
        state,
        udp,
        rx_rate: Bytes(rx),
        tx_rate: Bytes(tx),
        rx_total: Bytes(rx * 340),
        tx_total: Bytes(tx * 290),
        srtt_ms: srtt,
        retx_pct: srtt.map(|s| s / 40.0),
    };
    let flows = vec![
        f(
            251,
            "Safari",
            "192.168.1.24:52344",
            "151.101.1.140:443",
            "ESTAB",
            false,
            812_000,
            64_000,
            Some(18.2),
        ),
        f(
            268,
            "claude",
            "192.168.1.24:52901",
            "160.79.104.10:443",
            "ESTAB",
            false,
            96_400,
            22_800,
            Some(41.7),
        ),
        f(
            268,
            "claude",
            "192.168.1.24:52902",
            "160.79.104.10:443",
            "ESTAB",
            false,
            11_200,
            8_100,
            Some(39.9),
        ),
        f(
            438,
            "postgres",
            "127.0.0.1:5432",
            "127.0.0.1:60112",
            "ESTAB",
            false,
            4_300,
            51_000,
            Some(0.3),
        ),
        f(
            200,
            "mxmon",
            "0.0.0.0:5353",
            "*",
            "UDP",
            true,
            900,
            350,
            None,
        ),
        f(
            310,
            "Music",
            "192.168.1.24:53555",
            "17.253.31.14:443",
            "TIMEWT",
            false,
            0,
            0,
            Some(22.5),
        ),
    ];
    let by_pid = aggregate_by_pid(&flows);
    let (rx_total_rate, tx_total_rate) = by_pid
        .values()
        .fold((0, 0), |(rx, tx), &(r, t)| (rx + r, tx + t));
    FlowSample {
        count: flows.len() + 14,
        rx_total_rate,
        tx_total_rate,
        by_pid,
        flows,
    }
}

/// A populated `App` built the production way: every update folded through
/// `App::apply`, rings advanced far enough that every graph has a window.
pub fn app() -> App {
    let mut app = App::new(soc(), Config::default());
    for i in 0..72 {
        app.apply(Update::Fast(Box::new(fast_at(i))));
        if i % 2 == 0 {
            app.apply(Update::Power(Box::new(power_at(i))));
        }
        if i % 4 == 0 {
            app.apply(Update::Slow(Box::new(SlowSnapshot {
                temps: Some(temps_at(i)),
                battery: Some(battery()),
            })));
            app.apply(Update::Ping(Box::new(ping_at(i))));
        }
    }
    app.apply(Update::Flows(Box::new(flows())));
    app.apply(Update::Procs(Box::new(procs(40))));
    app
}

// ---- input-event constructors for `event::handle` tests -------------------

pub fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub fn key_with(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

pub fn click(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

pub fn moved(column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

pub fn scroll(column: u16, row: u16, down: bool) -> Event {
    Event::Mouse(MouseEvent {
        kind: if down {
            MouseEventKind::ScrollDown
        } else {
            MouseEventKind::ScrollUp
        },
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}
