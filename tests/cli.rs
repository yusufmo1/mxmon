//! End-to-end CLI checks against the real binary: flag handling and the
//! `--json` snapshot contract (the machine-readable verification path).
//!
//! Every spawn points `MXMON_CONFIG_DIR` at a tempdir, so runs never read or
//! write the real `~/.config/mxmon` — and with `ping = false` written there,
//! the run is fully passive on the network.

#![cfg(target_os = "macos")]

use std::process::Command;

/// The binary under test, sandboxed to a throwaway config dir.
fn mxmon(tmp: &tempfile::TempDir) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mxmon"));
    c.env("MXMON_CONFIG_DIR", tmp.path());
    c
}

/// Whether this host is real Apple Silicon rather than a VM.
///
/// The `--json` path drives Apple's private telemetry frameworks, and
/// IOReport has no providers under a hypervisor: the first call into it
/// traps (the process dies on SIGTRAP with no stdout, stderr, or panic log
/// — the trap is raised inside the framework, so there is nothing for
/// mxmon to catch or degrade). GitHub's macOS runners are VMs, so the
/// snapshot assertions below can only mean anything on real silicon.
///
/// Positive confirmation only: anything we can't read resolves to `false`
/// and skips, so an unknown environment reports "not verified" instead of
/// failing a test it was never able to run.
fn on_real_silicon() -> bool {
    Command::new("sysctl")
        .args(["-n", "kern.hv_vmm_present"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// Everything a failed spawn knows about itself. A bare "it failed" is
/// useless when the run only reproduces on another machine: a process that
/// dies on a signal has no stderr at all, and the exit code is the only
/// thing that distinguishes a crash from a clean non-zero exit.
fn diagnose(what: &str, out: &std::process::Output, tmp: &tempfile::TempDir) -> String {
    use std::fmt::Write;
    let mut s = format!("{what} failed\n  status: {:?}", out.status);
    if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&out.status) {
        let _ = write!(s, "\n  killed by signal: {sig}");
    }
    let _ = write!(
        s,
        "\n  stderr: {}\n  stdout (first 400): {}",
        String::from_utf8_lossy(&out.stderr).trim(),
        String::from_utf8_lossy(&out.stdout)
            .chars()
            .take(400)
            .collect::<String>()
    );
    // A panic in the TUI path is written here rather than to stderr.
    let log = tmp.path().join("last-panic.log");
    if let Ok(panic_log) = std::fs::read_to_string(&log) {
        let _ = write!(s, "\n  last-panic.log: {}", panic_log.trim());
    }
    s
}

#[test]
fn version_and_help_exit_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let out = mxmon(&tmp).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
    assert!(mxmon(&tmp).arg("--help").output().unwrap().status.success());
}

#[test]
fn unknown_flags_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let out = mxmon(&tmp).arg("--no-such-flag").output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn glyphs_flag_validates_its_value() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.toml"), "ping = false\n").unwrap();
    // A valid mode rides along with --json (the flag only shapes TUI frames,
    // so the snapshot path just proves it's accepted end-to-end). That half
    // needs real telemetry; the rejection half below is pure clap.
    if on_real_silicon() {
        let out = mxmon(&tmp)
            .args(["--json", "--glyphs", "braille"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            diagnose("--json --glyphs braille", &out, &tmp)
        );
    } else {
        eprintln!("SKIP: --json half needs real Apple Silicon (no IOReport under a hypervisor)");
    }
    // Clap rejects values outside the enum, not the code downstream.
    let out = mxmon(&tmp).args(["--glyphs", "sixel"]).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("possible values"),
        "clap should name the valid modes"
    );
}

#[test]
fn json_snapshot_honors_the_source_contract() {
    if !on_real_silicon() {
        eprintln!("SKIP: --json needs real Apple Silicon (no IOReport under a hypervisor)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Fully passive run: the ping prober is the only thing that ever emits
    // network traffic, and this also proves the config override reaches the
    // spawned binary.
    std::fs::write(tmp.path().join("config.toml"), "ping = false\n").unwrap();

    let out = mxmon(&tmp).arg("--json").output().unwrap();
    assert!(out.status.success(), "{}", diagnose("--json", &out, &tmp));
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    let obj = v.as_object().expect("top level is an object");

    // Every metric key is always present; a degraded source is null plus an
    // entry in source_errors — never a missing key (the SourceDown contract).
    for key in [
        "soc",
        "cpu_per_core_pct",
        "load",
        "uptime_secs",
        "gpu",
        "memory",
        "network",
        "ping",
        "disk",
        "flows",
        "power",
        "temps",
        "battery",
        "processes",
        "source_errors",
    ] {
        assert!(obj.contains_key(key), "missing top-level key {key}");
    }
    assert!(obj["source_errors"].is_array());
    assert!(
        !obj["soc"]["chip"].as_str().unwrap_or_default().is_empty(),
        "chip identity must always resolve"
    );
    // Tier letters are machine facts too: single uppercase letters (E/P on
    // two-tier chips, P/S on M5 Pro/Max).
    for tier in ["tier_low", "tier_high"] {
        let v = obj["soc"][tier].as_str().unwrap_or_default();
        assert!(
            v.len() == 1 && v.chars().all(|c| c.is_ascii_uppercase()),
            "{tier} must be one uppercase letter, got {v:?}"
        );
    }
    assert!(obj["uptime_secs"].as_u64().unwrap_or(0) > 0);
    // Ping was disabled — it must be absent-as-null, not fabricated.
    assert!(obj["ping"].is_null());
    // Numeric sanity wherever a source is up (nulls allowed when down).
    if let Some(mem) = obj["memory"].as_object() {
        assert!(mem["total_gb"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(mem["used_gb"].as_f64().unwrap_or(-1.0) >= 0.0);
    }
    if let Some(procs) = obj["processes"].as_object() {
        assert!(procs["total"].as_u64().unwrap_or(0) > 10);
        assert!(!procs["top_by_cpu"].as_array().unwrap().is_empty());
    }
    // The hermetic sandbox held: nothing else may appear in the tempdir
    // beyond what this test wrote and mxmon's own config-dir files.
    for entry in std::fs::read_dir(tmp.path()).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy().into_owned();
        assert!(
            ["config.toml", "sensors.toml", "last-panic.log"].contains(&name.as_str()),
            "unexpected file in sandbox: {name}"
        );
    }
}
