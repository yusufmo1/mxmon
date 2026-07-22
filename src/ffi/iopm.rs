//! Power assertions: what is currently keeping this Mac awake, and which
//! process is responsible.
//!
//! `IOPMCopyAssertionsByProcess` is sudoless and names both the owning process
//! and the reason, which is more than `pmset -g assertions` prints and far
//! more than any other macOS terminal monitor shows. Costs ~46 µs, so it rides
//! the slow health tier.

use std::ffi::c_void;

use core_foundation::array::CFArrayRef;
use core_foundation::dictionary::CFDictionaryRef;

use super::cf::{CfOwned, array_iter, as_dict, as_string, cfstr, dict_get};
use super::iokit::cf_number_i64;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMCopyAssertionsByProcess(assertions_by_pid: *mut CFDictionaryRef) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetCount(dict: CFDictionaryRef) -> isize;
    fn CFDictionaryGetKeysAndValues(
        dict: CFDictionaryRef,
        keys: *mut *const c_void,
        values: *mut *const c_void,
    );
}

/// One reason the machine is being held awake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    pub pid: i32,
    /// The assertion type, e.g. `PreventUserIdleSystemSleep`.
    pub kind: String,
    /// The human-readable reason the owner supplied, when it supplied one.
    pub name: Option<String>,
}

impl Assertion {
    /// Whether this assertion actually prevents sleep, as opposed to merely
    /// recording state. `UserIsActive` is a fact about the user, not a lock.
    pub fn prevents_sleep(&self) -> bool {
        self.kind.starts_with("Prevent") || self.kind.contains("NoIdleSleep")
    }
}

/// Every currently-held assertion, flattened across processes.
///
/// Returns an empty list rather than an error on any failure: "nothing is
/// holding the machine awake" and "we could not ask" both render the same way,
/// and neither is worth failing a collector over.
pub fn assertions() -> Vec<Assertion> {
    let mut dict: CFDictionaryRef = std::ptr::null();
    let kr = unsafe { IOPMCopyAssertionsByProcess(&raw mut dict) };
    if kr != 0 || dict.is_null() {
        return Vec::new();
    }
    // Owned from a Copy call — released when this drops.
    let Some(owned) = (unsafe { CfOwned::from_create(dict.cast()) }) else {
        return Vec::new();
    };

    let count = unsafe { CFDictionaryGetCount(owned.as_dict()) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut keys: Vec<*const c_void> = vec![std::ptr::null(); count];
    let mut values: Vec<*const c_void> = vec![std::ptr::null(); count];
    unsafe {
        CFDictionaryGetKeysAndValues(owned.as_dict(), keys.as_mut_ptr(), values.as_mut_ptr());
    }

    let type_key = cfstr("AssertType");
    let name_key = cfstr("AssertName");
    let mut out = Vec::new();
    for (key, value) in keys.into_iter().zip(values) {
        // The dictionary is keyed by pid, valued by that process's assertions.
        let Some(pid) = cf_number_i64(key) else {
            continue;
        };
        let list: CFArrayRef = value.cast();
        for entry in array_iter(list) {
            let Some(entry) = as_dict(entry) else {
                continue;
            };
            let Some(kind) = dict_get(entry, &type_key).and_then(as_string) else {
                continue;
            };
            out.push(Assertion {
                pid: pid.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                kind,
                name: dict_get(entry, &name_key)
                    .and_then(as_string)
                    .filter(|s| !s.is_empty()),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::Assertion;

    fn a(kind: &str) -> Assertion {
        Assertion {
            pid: 1,
            kind: kind.into(),
            name: None,
        }
    }

    #[test]
    fn only_preventing_assertions_count_as_holding_the_mac_awake() {
        assert!(a("PreventUserIdleSystemSleep").prevents_sleep());
        assert!(a("PreventSystemSleep").prevents_sleep());
        assert!(a("NoIdleSleepAssertion").prevents_sleep());
        // A statement about the user, not a lock on the machine.
        assert!(!a("UserIsActive").prevents_sleep());
        assert!(!a("ExternalMedia").prevents_sleep());
    }
}
