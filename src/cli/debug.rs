//! Developer dumps of raw sources, relocated verbatim from the old top-level
//! debug flags. Reachable as `mxmon debug <net|smc|bench|flows|smart>`.

use std::time::Duration;

use super::args::DebugCmd;

pub fn run(cmd: DebugCmd) -> color_eyre::Result<()> {
    match cmd {
        DebugCmd::Net => net(),
        DebugCmd::Smc => smc(),
        DebugCmd::Bench => bench(),
        DebugCmd::Flows => flows(),
        DebugCmd::Smart => {
            smart();
            Ok(())
        }
    }
}

fn net() -> color_eyre::Result<()> {
    println!("{}", crate::ffi::net::layout_report("en17"));
    for c in crate::ffi::net::interface_counters()? {
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
    Ok(())
}

fn smc() -> color_eyre::Result<()> {
    let smc = crate::ffi::smc::Smc::open()?;
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
    Ok(())
}

fn smart() {
    let mut storage = crate::collect::storage::StorageCollector::new();
    // Two passes: the controller counters are deltas and the first pass only
    // establishes their baseline.
    let _ = storage.sample();
    let s = storage.sample();
    println!("{:#?}", s.smart);
    println!("controller: {:?}", s.controller);
    let mut kc = crate::collect::kernel::KernelCollector::new();
    let _ = kc.sample();
    std::thread::sleep(Duration::from_millis(1500));
    let k = kc.sample();
    println!("interrupts {:.0}/s", k.total_per_sec);
    for src in &k.top_sources {
        println!(
            "  {:<22} {:>9.0}/s  handler {:>6.3}%",
            src.device,
            src.per_sec,
            src.cpu_share * 100.0
        );
    }
    for v in &s.volumes {
        println!(
            "{:<28} hit {:>6} amp {:>5}",
            v.name,
            v.cache_hit()
                .map_or("-".into(), |r| format!("{:.1}%", r.as_percent())),
            v.write_amplification()
                .map_or("-".into(), |a| format!("{a:.2}x")),
        );
    }
}

fn flows() -> color_eyre::Result<()> {
    crate::collect::flows::debug_dump()?;
    Ok(())
}

fn bench() -> color_eyre::Result<()> {
    let soc = crate::collect::soc::load()?;
    let mut temps = crate::collect::temps::TempCollector::new(&soc)?;
    let battery = crate::collect::battery::BatteryCollector::new();
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
    if let Ok(mut hid) = crate::ffi::hid::HidTemps::new() {
        let raw = hid.read_all().len();
        time("hid raw", &mut || {
            let _ = hid.read_all();
        });
        hid.retain(|name| crate::collect::temps::classify_hid(name).is_some());
        println!("hid sensors {:>4} raw / {} kept", raw, hid.read_all().len());
        time("hid kept", &mut || {
            let _ = hid.read_all();
        });
    }
    Ok(())
}
