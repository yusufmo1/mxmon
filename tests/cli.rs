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
fn json_snapshot_honors_the_source_contract() {
    let tmp = tempfile::tempdir().unwrap();
    // Fully passive run: the ping prober is the only thing that ever emits
    // network traffic, and this also proves the config override reaches the
    // spawned binary.
    std::fs::write(tmp.path().join("config.toml"), "ping = false\n").unwrap();

    let out = mxmon(&tmp).arg("--json").output().unwrap();
    assert!(
        out.status.success(),
        "--json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
