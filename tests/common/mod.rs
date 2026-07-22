//! Shared helpers for the end-to-end test binaries.

use std::process::Command;

/// Whether this host is real Apple Silicon rather than a VM.
///
/// Anything that boots the real binary past argument parsing drives Apple's
/// private telemetry frameworks, and IOReport has no providers under a
/// hypervisor: the first call into it raises SIGTRAP from inside the
/// framework, so the process dies with no stdout, stderr, or panic log —
/// there is nothing for mxmon to catch and nothing to degrade to. GitHub's
/// macOS runners are VMs, so those assertions can only mean something on
/// real silicon.
///
/// Positive confirmation only: anything we can't read resolves to `false`
/// and skips, so an unknown environment reports "not verified" instead of
/// failing a test it was never able to run.
pub fn on_real_silicon() -> bool {
    Command::new("sysctl")
        .args(["-n", "kern.hv_vmm_present"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// Print the standard skip line and return `true` when the host can't run a
/// telemetry-dependent test.
pub fn skip_without_hardware(what: &str) -> bool {
    if on_real_silicon() {
        return false;
    }
    eprintln!("SKIP: {what} needs real Apple Silicon (no IOReport under a hypervisor)");
    true
}
