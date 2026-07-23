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

mod common;
use common::{on_real_silicon, skip_without_hardware};

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
fn json_snapshot_honors_the_v1_contract() {
    if skip_without_hardware("--json") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Fully passive run: ping is the only network egress, and disabling it also
    // proves the config override reaches the spawned binary and drives the null
    // taxonomy checked below.
    std::fs::write(tmp.path().join("config.toml"), "ping = false\n").unwrap();

    let out = mxmon(&tmp).arg("--json").output().unwrap();
    assert!(out.status.success(), "{}", diagnose("--json", &out, &tmp));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    let obj = v.as_object().expect("top level is an object");

    // Every domain key is always present; a degraded source is null plus an
    // entry in source_errors, never a missing key (the SourceDown contract).
    for key in [
        "meta", "soc", "cpu", "gpu", "memory", "power", "thermal", "network", "disk", "storage",
        "battery", "processes", "flows", "kernel", "ping", "source_errors",
    ] {
        assert!(obj.contains_key(key), "missing top-level key {key}");
    }
    assert!(obj["source_errors"].is_array());

    // The v1 migration is complete: the old inconsistent shape is gone.
    for gone in [
        "cpu_per_core_pct",
        "load",
        "uptime_secs",
        "temps",
        "volumes",
        "kernel_activity",
    ] {
        assert!(!obj.contains_key(gone), "legacy top-level key {gone} still present");
    }
    for legacy in ["\"total_gb\"", "\"power_mw\"", "\"used_gb\"", "\"usage_pct\"", "\"cpu_pct\""] {
        assert!(!stdout.contains(legacy), "legacy unit key {legacy} still emitted");
    }

    // Meta carries the contract version and the feature flags that disambiguate
    // why a domain is null.
    assert_eq!(obj["meta"]["schema_version"].as_u64(), Some(1));
    assert_eq!(obj["meta"]["features"]["ping"].as_bool(), Some(false));

    // Machine facts always resolve.
    assert!(
        !obj["soc"]["chip"].as_str().unwrap_or_default().is_empty(),
        "chip identity must always resolve"
    );
    // Tier letters are machine facts too: single uppercase letters (E/P on
    // two-tier chips, P/S on M5 Pro/Max).
    for tier in ["tier_low", "tier_high"] {
        let t = obj["soc"][tier].as_str().unwrap_or_default();
        assert!(
            t.len() == 1 && t.chars().all(|c| c.is_ascii_uppercase()),
            "{tier} must be one uppercase letter, got {t:?}"
        );
    }

    // Null taxonomy: ping was disabled, so it is null (and features.ping is
    // false, asserted above) rather than fabricated or a missing key.
    assert!(obj["ping"].is_null(), "disabled ping must be null");

    // Numeric sanity and consistent units wherever a source is up.
    if let Some(cpu) = obj["cpu"].as_object() {
        assert!(cpu["uptime_secs"].as_u64().unwrap_or(0) > 0);
        assert_eq!(cpu["load_avg"].as_array().map(Vec::len), Some(3));
    }
    if let Some(mem) = obj["memory"].as_object() {
        assert!(mem["total_bytes"].as_u64().unwrap_or(0) > 0, "bytes are integers");
        assert!((0.0..=1.001).contains(&mem["used_ratio"].as_f64().unwrap_or(-1.0)));
    }
    if let Some(procs) = obj["processes"].as_object() {
        assert!(procs["total"].as_u64().unwrap_or(0) > 10);
        assert!(!procs["top"].as_array().unwrap().is_empty());
    }

    // The hermetic sandbox held: nothing else may appear in the tempdir beyond
    // what this test wrote and mxmon's own config-dir files.
    for entry in std::fs::read_dir(tmp.path()).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy().into_owned();
        assert!(
            ["config.toml", "sensors.toml", "last-panic.log"].contains(&name.as_str()),
            "unexpected file in sandbox: {name}"
        );
    }
}

#[test]
fn schema_runs_without_hardware() {
    // The one telemetry-free e2e: `schema` never samples, so it runs on the CI
    // VM and guards a code path the hardware suites cannot reach there.
    let tmp = tempfile::tempdir().unwrap();
    let out = mxmon(&tmp).arg("schema").output().unwrap();
    assert!(out.status.success(), "{}", diagnose("schema", &out, &tmp));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("schema is JSON");
    assert_eq!(v["title"], "Report");
    assert!(v["$defs"]["Meta"].is_object(), "schema defines Meta");
}

#[test]
fn kill_fails_closed_off_a_terminal() {
    // Control needs no telemetry, so this runs anywhere. Piped output is not a
    // tty, so without --yes the command must refuse (exit 4) and touch nothing.
    let tmp = tempfile::tempdir().unwrap();
    let refused = mxmon(&tmp).args(["kill", "2147483640"]).output().unwrap();
    assert_eq!(refused.status.code(), Some(4), "must fail closed without --yes off a tty");

    // --dry-run resolves and prints a plan without signaling anything.
    let dry = mxmon(&tmp).args(["kill", "--dry-run", "2147483640"]).output().unwrap();
    assert!(dry.status.success());
    assert!(String::from_utf8_lossy(&dry.stdout).contains("dry run"));

    // --yes on an impossible pid reaches the ESRCH branch (already exited).
    let yes = mxmon(&tmp).args(["kill", "--yes", "2147483640"]).output().unwrap();
    assert_eq!(yes.status.code(), Some(4));
}

#[test]
fn subcommands_over_the_v1_contract() {
    if skip_without_hardware("subcommands") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.toml"), "ping = false\n").unwrap();

    // snapshot --format json is the v1 report.
    let snap = mxmon(&tmp).args(["snapshot", "--format", "json"]).output().unwrap();
    assert!(snap.status.success(), "{}", diagnose("snapshot", &snap, &tmp));
    let v: serde_json::Value = serde_json::from_slice(&snap.stdout).unwrap();
    assert_eq!(v["meta"]["schema_version"], 1);

    // get extracts a scalar leaf; an unknown path exits nonzero.
    let chip = mxmon(&tmp).args(["get", "soc.chip"]).output().unwrap();
    assert!(chip.status.success());
    assert!(String::from_utf8_lossy(&chip.stdout).contains("Apple"));
    assert!(!mxmon(&tmp).args(["get", "power.nope"]).output().unwrap().status.success());

    // check exit codes: true=0, false=1, unknown (null source)=2.
    assert_eq!(
        mxmon(&tmp).args(["check", "meta.schema_version == 1"]).output().unwrap().status.code(),
        Some(0)
    );
    assert_eq!(
        mxmon(&tmp).args(["check", "meta.schema_version == 2"]).output().unwrap().status.code(),
        Some(1)
    );
    // ping is disabled, so ping.up is null and the comparison is undecidable.
    assert_eq!(
        mxmon(&tmp).args(["check", "ping.up == true"]).output().unwrap().status.code(),
        Some(2)
    );

    // health emits a status string.
    let h = mxmon(&tmp).args(["health", "--format", "json"]).output().unwrap();
    let hv: serde_json::Value = serde_json::from_slice(&h.stdout).unwrap();
    assert!(hv["status"].is_string());

    // watch is bounded: --count 2 emits exactly two NDJSON frames, then exits.
    let w = mxmon(&tmp)
        .args(["watch", "--count", "2", "--interval", "200ms"])
        .output()
        .unwrap();
    assert!(w.status.success());
    let frames = String::from_utf8_lossy(&w.stdout).lines().filter(|l| !l.is_empty()).count();
    assert_eq!(frames, 2, "watch --count 2 emits exactly two frames");
}
