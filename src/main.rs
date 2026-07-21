//! mxmon — a beautiful, lightning-fast terminal monitor for Apple Silicon.

mod app;
mod collect;
mod config;
mod event;
mod ffi;
mod trace;
mod ui;
mod units;

#[cfg(test)]
mod tests;

use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};

use app::App;
use collect::sampler::{self, Control, Update};
use config::Config;
use event::Outcome;
use ui::layout::RenderState;
use ui::widgets::HitMap;

#[derive(Parser, Debug)]
#[command(
    name = "mxmon",
    version,
    about = "mxmon — a sudoless Apple Silicon terminal monitor"
)]
struct Cli {
    /// Print one JSON snapshot of every metric and exit (for scripting/tests).
    #[arg(long)]
    json: bool,

    /// Fast-tier sampling interval in milliseconds (100–2000); overrides config.
    #[arg(long)]
    interval: Option<u64>,

    /// Theme name (neon, gruvbox, tokyonight, catppuccin, and more); overrides config.
    #[arg(long)]
    theme: Option<String>,

    /// Dump raw per-interface counters and exit (debugging aid).
    #[arg(long, hide = true)]
    net_debug: bool,

    /// Dump all readable float SMC power/voltage/current keys and exit.
    #[arg(long, hide = true)]
    smc_debug: bool,

    /// Time each collector's sample cost and exit (perf debugging aid).
    #[arg(long, hide = true)]
    bench: bool,

    /// Dump the raw ntstat message stream + self-calibrated field offsets.
    #[arg(long, hide = true)]
    flows_debug: bool,
}

fn main() -> color_eyre::Result<()> {
    trace::init();
    color_eyre::install()?;
    let cli = Cli::parse();

    let soc = collect::soc::load()?;
    trace::mark("soc facts loaded");

    if cli.net_debug {
        println!("{}", ffi::net::layout_report("en17"));
        for c in ffi::net::interface_counters()? {
            println!(
                "{:8} up={} run={} lo={} rx={:>15} tx={:>15} baud={} mac={}",
                c.name,
                c.up,
                c.running,
                c.loopback,
                c.rx_bytes,
                c.tx_bytes,
                c.baudrate,
                c.mac.as_deref().unwrap_or("-")
            );
        }
        return Ok(());
    }

    if cli.smc_debug {
        let smc = ffi::smc::Smc::open()?;
        let mut keys = smc.all_keys()?;
        keys.sort();
        for key in keys {
            if !key.starts_with(['P', 'V', 'I']) {
                continue;
            }
            let Ok(info) = smc.key_info(&key) else {
                continue;
            };
            if let Ok(v) = smc.read_f32(&key, info) {
                println!("{key} = {v:10.3}");
            }
        }
        return Ok(());
    }

    if cli.flows_debug {
        collect::flows::debug_dump()?;
        return Ok(());
    }

    if cli.bench {
        let mut temps = collect::temps::TempCollector::new(&soc.chip_name, &soc.macos_version)?;
        let battery = collect::battery::BatteryCollector::new();
        let time = |label: &str, f: &mut dyn FnMut()| {
            f(); // warm-up
            let start = std::time::Instant::now();
            for _ in 0..10 {
                f();
            }
            println!(
                "{label:10} {:>8.0}µs",
                start.elapsed().as_micros() as f64 / 10.0
            );
        };
        time("temps", &mut || {
            let _ = temps.sample(true);
        });
        time("battery", &mut || {
            let _ = battery.sample();
        });
        if let Ok(mut hid) = ffi::hid::HidTemps::new() {
            let raw = hid.read_all().len();
            time("hid raw", &mut || {
                let _ = hid.read_all();
            });
            // The collector sheds non-display channels once at startup; this
            // is the sweep cost the running app actually pays.
            hid.retain(|name| collect::temps::classify_hid(name).is_some());
            println!("hid sensors {:>4} raw / {} kept", raw, hid.read_all().len());
            time("hid kept", &mut || {
                let _ = hid.read_all();
            });
        }
        return Ok(());
    }

    if cli.json {
        return json_snapshot(&soc);
    }

    let mut config = Config::load();
    if let Some(interval) = cli.interval {
        config.interval_ms = interval.clamp(sampler::FAST_MS_MIN, sampler::FAST_MS_MAX);
    }
    if let Some(theme) = cli.theme {
        config.theme = theme;
    }

    run_tui(soc, config)
}

/// Message stream feeding the UI thread.
enum Msg {
    Data(Box<Update>),
    Input(ratatui::crossterm::event::Event),
}

/// A frame's diff is tens of KB of ANSI with no newlines; bare `Stdout` is
/// line-buffered (1 KiB), which turned every draw into dozens of small
/// `write(2)` calls — a third of render CPU. Buffer big enough that even a
/// full truecolor repaint flushes as one write.
type Term = ratatui::Terminal<CrosstermBackend<std::io::BufWriter<std::io::Stdout>>>;

/// `ratatui::init()` with the buffered writer swapped in; identical raw-mode,
/// alternate-screen, and panic-restore semantics.
fn init_terminal() -> std::io::Result<Term> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        hook(info);
    }));
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(std::io::BufWriter::with_capacity(
        512 * 1024,
        std::io::stdout(),
    ));
    ratatui::Terminal::new(backend)
}

fn run_tui(soc: collect::soc::SocInfo, config: Config) -> color_eyre::Result<()> {
    let control = Control::new();
    control
        .fast_ms
        .store(config.interval_ms, std::sync::atomic::Ordering::Relaxed);

    let (tx, rx) = mpsc::channel::<Msg>();

    // Sampler threads (they use their own channel, adapted below).
    let (data_tx, data_rx) = mpsc::channel::<Update>();
    let ping_host = config.ping.then(|| config.ping_host.clone());
    sampler::spawn(
        soc.clone(),
        std::sync::Arc::clone(&control),
        data_tx,
        ping_host,
    );
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("mxmon-data-pump".into())
            .spawn(move || {
                while let Ok(update) = data_rx.recv() {
                    if tx.send(Msg::Data(Box::new(update))).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn data pump");
    }
    // Input thread.
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("mxmon-input".into())
            .spawn(move || {
                while let Ok(ev) = ratatui::crossterm::event::read() {
                    if tx.send(Msg::Input(ev)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn input thread");
    }

    let mut terminal = init_terminal()?;
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    trace::mark("terminal ready");

    let mut app = App::new(soc, config);
    let mut hits = HitMap::default();
    let mut rs = RenderState::default();

    let result = ui_loop(&mut terminal, &mut app, &control, &rx, &mut hits, &mut rs);

    control
        .shutdown
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    app.config.save();
    result
}

fn ui_loop(
    terminal: &mut Term,
    app: &mut App,
    control: &Control,
    rx: &mpsc::Receiver<Msg>,
    hits: &mut HitMap,
    rs: &mut RenderState,
) -> color_eyre::Result<()> {
    // First paint before any data arrives.
    draw(terminal, app, hits, rs)?;
    trace::mark("first frame drawn");
    loop {
        // Block for the next message, then drain the queue so a burst of
        // updates costs one redraw.
        let first = rx.recv()?;
        let mut outcome = apply_msg(first, app, control, hits, rs);
        while let Ok(msg) = rx.try_recv() {
            match apply_msg(msg, app, control, hits, rs) {
                Outcome::Quit => outcome = Outcome::Quit,
                Outcome::Continue if outcome != Outcome::Quit => outcome = Outcome::Continue,
                _ => {}
            }
        }
        if outcome == Outcome::Quit {
            return Ok(());
        }
        // Expire stale toasts.
        if app
            .toast
            .as_ref()
            .is_some_and(|t| std::time::Instant::now() > t.until)
        {
            app.toast = None;
            outcome = Outcome::Continue;
        }
        // An all-idle batch (e.g. pointer motion under any-motion mouse
        // tracking) changed no state — repainting would emit an identical
        // frame, so skip it.
        if outcome == Outcome::Continue {
            draw(terminal, app, hits, rs)?;
        }
    }
}

fn apply_msg(
    msg: Msg,
    app: &mut App,
    control: &Control,
    hits: &mut HitMap,
    rs: &mut RenderState,
) -> Outcome {
    match msg {
        Msg::Data(update) => {
            app.apply(*update);
            Outcome::Continue
        }
        Msg::Input(ev) => event::handle(&ev, app, control, hits, rs),
    }
}

fn draw(
    terminal: &mut Term,
    app: &mut App,
    hits: &mut HitMap,
    rs: &mut RenderState,
) -> color_eyre::Result<()> {
    let started = std::time::Instant::now();
    let theme = ui::theme::by_name(&app.config.theme);
    terminal.draw(|f| ui::layout::draw(f, app, &theme, hits, rs))?;
    app.last_frame_us = started.elapsed().as_micros() as u64;
    app.frames += 1;
    Ok(())
}

/// Gather one settled snapshot of everything and print it as JSON.
fn json_snapshot(soc: &collect::soc::SocInfo) -> color_eyre::Result<()> {
    let control = Control::new();
    // One fast tick separates the warm-up emissions from the settled ones, so
    // this is the delta window the printed rates average over.
    control
        .fast_ms
        .store(250, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let config = Config::load();
    sampler::spawn(
        soc.clone(),
        std::sync::Arc::clone(&control),
        tx,
        config.ping.then(|| config.ping_host.clone()),
    );

    // Collect until every tier has reported at least twice (deltas settled).
    // Ping is deliberately not part of the settle gate — its first probe
    // fires immediately and simply rides along if it lands in time.
    let mut fast = None;
    let mut power = None;
    let mut slow = None;
    let mut procs = None;
    let mut ping = None;
    let mut flows = None;
    let mut battery: Option<collect::battery::BatterySample> = None;
    let mut errors: Vec<(String, String)> = Vec::new();
    let mut down: std::collections::HashSet<String> = std::collections::HashSet::new();
    let (mut n_fast, mut n_power, mut n_slow, mut n_procs, mut n_flows) = (0, 0, 0, 0, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Update::Fast(s)) => {
                n_fast += 1;
                fast = Some(s);
            }
            Ok(Update::Power(s)) => {
                n_power += 1;
                power = Some(s);
            }
            Ok(Update::Slow(s)) => {
                n_slow += 1;
                // Battery rides the slower cadence — don't lose a reading
                // to a later temps-only update.
                if s.battery.is_some() {
                    battery.clone_from(&s.battery);
                }
                slow = Some(s);
            }
            Ok(Update::Procs(s)) => {
                n_procs += 1;
                procs = Some(s);
            }
            Ok(Update::Ping(s)) => ping = Some(s),
            Ok(Update::Flows(s)) => {
                n_flows += 1;
                flows = Some(s);
            }
            Ok(Update::SourceDown { source, error }) => {
                down.insert(source.to_string());
                errors.push((source.into(), error));
            }
            Err(_) => {}
        }
        // A collector that reported SourceDown will never deliver twice —
        // don't let its absence pin the snapshot to the full deadline.
        let ok = |n: i32, src: &str| n >= 2 || down.contains(src);
        if n_fast >= 2
            && ok(n_power, "power")
            && n_slow >= 2
            && n_procs >= 2
            && ok(n_flows, "flows")
        {
            break;
        }
    }
    trace::mark("json settled");
    control
        .shutdown
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let mut top: Vec<_> = procs.as_ref().map(|p| p.rows.clone()).unwrap_or_default();
    top.sort_by(|a, b| {
        b.cpu
            .map_or(0.0, |r| r.0)
            .total_cmp(&a.cpu.map_or(0.0, |r| r.0))
    });
    top.truncate(12);

    let json = serde_json::json!({
        "soc": {
            "chip": soc.chip_name,
            "macos": soc.macos_version,
            "ecores": soc.ecpu_count,
            "pcores": soc.pcpu_count,
            "gpu_cores": soc.gpu_core_count,
            "memory_gb": soc.memory_bytes as f64 / 1_073_741_824.0,
            "ecpu_freqs_mhz": soc.ecpu_freqs.iter().map(|f| f.0).collect::<Vec<_>>(),
            "pcpu_freqs_mhz": soc.pcpu_freqs.iter().map(|f| f.0).collect::<Vec<_>>(),
            "gpu_freqs_mhz": soc.gpu_freqs.iter().map(|f| f.0).collect::<Vec<_>>(),
        },
        "cpu_per_core_pct": fast.as_ref().and_then(|f| f.cpu.as_ref())
            .map(|c| c.per_core.iter().map(|r| (r.as_percent() * 10.0).round() / 10.0).collect::<Vec<_>>()),
        "load": fast.as_ref().map(|f| f.load),
        "uptime_secs": fast.as_ref().map(|f| f.uptime_secs),
        "gpu": fast.as_ref().and_then(|f| f.gpu.as_ref()).map(|g| serde_json::json!({
            "device_pct": g.device.as_percent(),
            "renderer_pct": g.renderer.as_percent(),
            "tiler_pct": g.tiler.as_percent(),
            "used_memory_gb": g.used_memory.as_f64() / 1_073_741_824.0,
        })),
        "memory": fast.as_ref().and_then(|f| f.mem.as_ref()).map(|m| serde_json::json!({
            "used_gb": m.used.as_f64() / 1_073_741_824.0,
            "total_gb": m.total.as_f64() / 1_073_741_824.0,
            "app_gb": m.app.as_f64() / 1_073_741_824.0,
            "wired_gb": m.wired.as_f64() / 1_073_741_824.0,
            "compressed_gb": m.compressed.as_f64() / 1_073_741_824.0,
            "cached_gb": m.cached.as_f64() / 1_073_741_824.0,
            "swap_used_gb": m.swap_used.as_f64() / 1_073_741_824.0,
            "pressure": format!("{:?}", m.pressure),
        })),
        "network": fast.as_ref().and_then(|f| f.net.as_ref()).map(|n| serde_json::json!({
            "rx_mbps": n.rx_per_sec.as_f64() * 8.0 / 1e6,
            "tx_mbps": n.tx_per_sec.as_f64() * 8.0 / 1e6,
            "rx_session_gb": n.rx_session.as_f64() / 1e9,
            "tx_session_gb": n.tx_session.as_f64() / 1e9,
            "primary": n.primary.as_ref().map(|p| serde_json::json!({
                "name": p.name, "speed_mbps": p.baudrate / 1_000_000, "ipv4": p.ipv4,
                "mac": p.mac, "link_up": p.running,
            })),
        })),
        "ping": ping.as_ref().map(|p| serde_json::json!({
            "host": p.host,
            "rtt_ms": p.rtt_ms.map(|v| f64::from(v * 10.0).round() / 10.0),
            "latency_ms": p.latency_ms.map(|v| f64::from(v * 10.0).round() / 10.0),
            "jitter_ms": p.jitter_ms.map(|v| f64::from(v * 100.0).round() / 100.0),
            "up": p.up,
        })),
        "disk": fast.as_ref().and_then(|f| f.disk.as_ref()).map(|d| serde_json::json!({
            "read_mbs": (d.read_per_sec.as_f64() / 1e5).round() / 10.0,
            "write_mbs": (d.write_per_sec.as_f64() / 1e5).round() / 10.0,
            "read_iops": d.read_iops,
            "write_iops": d.write_iops,
            "read_lat_us": d.read_lat_us.map(|l| f64::from(l).round()),
            "write_lat_us": d.write_lat_us.map(|l| f64::from(l).round()),
            "read_session_gb": (d.read_session.as_f64() / 1e8).round() / 10.0,
            "write_session_gb": (d.write_session.as_f64() / 1e8).round() / 10.0,
            "devices": d.devices,
        })),
        "flows": flows.as_ref().map(|f| serde_json::json!({
            "count": f.count,
            "rx_total_kbs": (f.rx_total_rate as f64 / 100.0).round() / 10.0,
            "tx_total_kbs": (f.tx_total_rate as f64 / 100.0).round() / 10.0,
            "top": f.flows.iter().take(10).map(|fl| serde_json::json!({
                "pid": fl.pid, "name": fl.pname, "local": fl.local, "remote": fl.remote,
                "state": fl.state,
                "rx_kbs": (fl.rx_rate.as_f64() / 100.0).round() / 10.0,
                "tx_kbs": (fl.tx_rate.as_f64() / 100.0).round() / 10.0,
                "rtt_ms": fl.srtt_ms.map(|v| f64::from(v * 10.0).round() / 10.0),
                "retx_pct": fl.retx_pct.map(|v| f64::from(v * 10.0).round() / 10.0),
            })).collect::<Vec<_>>(),
        })),
        "power": power.as_ref().map(|p| serde_json::json!({
            "package_w": p.package().0,
            "cpu_w": p.cpu.0,
            "gpu_w": p.gpu.0,
            "ane_w": p.ane.0,
            "dram_w": p.dram.0,
            "display_w": p.display.0,
            "ecpu": { "freq_mhz": p.ecpu.freq.0, "usage_pct": p.ecpu.usage.as_percent(),
                      "cores": p.ecpu.cores.iter().map(|(f, u)| (f.0, (u.as_percent()*10.0).round()/10.0)).collect::<Vec<_>>() },
            "pcpu": { "freq_mhz": p.pcpu.freq.0, "usage_pct": p.pcpu.usage.as_percent(),
                      "cores": p.pcpu.cores.iter().map(|(f, u)| (f.0, (u.as_percent()*10.0).round()/10.0)).collect::<Vec<_>>() },
            "gpu_freq_mhz": p.gpu_freq.0,
            "gpu_usage_pct": p.gpu_usage.as_percent(),
        })),
        "temps": slow.as_ref().and_then(|s| s.temps.as_ref()).map(|t| serde_json::json!({
            "cpu_avg_c": t.cpu_avg.0,
            "cpu_max_c": t.cpu_max.0,
            "gpu_avg_c": t.gpu_avg.0,
            "sys_power_w": t.sys_power.map(|w| w.0),
            "adapter_power_w": t.adapter_power.map(|w| w.0),
            "fans": t.fans.iter().map(|f| serde_json::json!({"label": f.label, "rpm": f.rpm, "max": f.max_rpm})).collect::<Vec<_>>(),
            "sensor_count": t.sensors.len(),
            "sensors": t.sensors.iter().map(|s| serde_json::json!({"group": s.group.title(), "label": s.label, "c": (s.temp.0*10.0).round()/10.0})).collect::<Vec<_>>(),
        })),
        "battery": battery.as_ref().map(|b| serde_json::json!({
            "charge_pct": b.charge.as_percent(),
            "charging": b.charging,
            "external_power": b.external_power,
            "battery_w": b.battery_watts.0,
            "adapter_w": b.adapter_watts.map(|w| w.0),
            "adapter_name": b.adapter_name,
            "cycles": b.cycle_count,
            "health_pct": b.health.as_percent(),
            "temp_c": b.temp.0,
        })),
        "processes": procs.as_ref().map(|p| serde_json::json!({
            "total": p.total,
            "running": p.running,
            "threads_visible": p.threads,
            "restricted": p.restricted,
            "top_by_cpu": top.iter().map(|r| serde_json::json!({
                "pid": r.pid, "user": r.user, "name": r.name,
                "cpu_pct": r.cpu.map(|c| (c.as_percent()*10.0).round()/10.0),
                "mem_mb": r.memory.map(|m| (m.as_f64() / 1_048_576.0).round()),
                "power_mw": r.power.map(|w| f64::from(w.0 * 1000.0).round()),
                "ipc": r.ipc.map(|v| f64::from(v * 100.0).round() / 100.0),
                "p_share_pct": r.p_share.map(|p| f64::from(p.as_percent()).round()),
                "disk_r_kbs": r.disk_read_rate.map(|b| (b.as_f64() / 1000.0).round()),
                "disk_w_kbs": r.disk_write_rate.map(|b| (b.as_f64() / 1000.0).round()),
                "threads": r.threads, "state": r.state.glyph(),
            })).collect::<Vec<_>>(),
        })),
        "source_errors": errors,
    });
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}
