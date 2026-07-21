//! Safe wrapper over the private `IOReport` framework — the sudoless source of
//! energy counters and performance-state residencies on Apple Silicon.
//!
//! Flow (mirrors the approach proven by MIT-licensed `vladkens/macmon`):
//! copy all channels → filter to the ones we need → subscribe → take sample
//! pairs → diff them → read energies / residencies from the delta.

use std::ffi::c_void;
use std::marker::{PhantomData, PhantomPinned};

use core_foundation::array::{
    CFArrayAppendValue, CFArrayCreateMutable, CFArrayGetCount, CFArrayRef, kCFTypeArrayCallBacks,
};
use core_foundation::base::{CFRelease, CFTypeRef, kCFAllocatorDefault};
use core_foundation::dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryRef, CFDictionarySetValue,
    CFMutableDictionaryRef,
};
use core_foundation::string::CFStringRef;

use super::cf::{CfOwned, array_iter, cfstr, dict_get, string_from_cf};

#[repr(C)]
struct IOReportSubscriptionOpaque {
    _data: [u8; 0],
    _phantom: PhantomData<(*mut u8, PhantomPinned)>,
}
type IOReportSubscriptionRef = *const IOReportSubscriptionOpaque;

#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
    fn IOReportCopyAllChannels(a: u64, b: u64) -> CFDictionaryRef;
    fn IOReportCreateSubscription(
        allocator: *const c_void,
        channels: CFMutableDictionaryRef,
        subscribed: *mut CFMutableDictionaryRef,
        channel_id: u64,
        options: CFTypeRef,
    ) -> IOReportSubscriptionRef;
    fn IOReportCreateSamples(
        subscription: IOReportSubscriptionRef,
        channels: CFMutableDictionaryRef,
        options: CFTypeRef,
    ) -> CFDictionaryRef;
    fn IOReportCreateSamplesDelta(
        prev: CFDictionaryRef,
        curr: CFDictionaryRef,
        options: CFTypeRef,
    ) -> CFDictionaryRef;
    fn IOReportChannelGetGroup(item: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetSubGroup(item: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetChannelName(item: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetUnitLabel(item: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(item: CFDictionaryRef, index: i32) -> i64;
    fn IOReportStateGetCount(item: CFDictionaryRef) -> i32;
    fn IOReportStateGetNameForIndex(item: CFDictionaryRef, index: i32) -> CFStringRef;
    fn IOReportStateGetResidency(item: CFDictionaryRef, index: i32) -> i64;
}

/// Identity of one subscribed channel, cached once at subscription time.
#[derive(Debug, Clone)]
pub struct ChannelId {
    pub group: String,
    pub name: String,
    pub unit: String,
}

/// One channel's data within a delta sample.
pub struct DeltaItem<'a> {
    item: CFDictionaryRef,
    pub id: &'a ChannelId,
}

impl DeltaItem<'_> {
    /// Accumulated simple counter value over the delta window (e.g. energy).
    pub fn integer_value(&self) -> i64 {
        unsafe { IOReportSimpleGetIntegerValue(self.item, 0) }
    }

    /// `(state_name, residency_ticks)` pairs for state-format channels.
    pub fn residencies(&self) -> Vec<(String, i64)> {
        let count = unsafe { IOReportStateGetCount(self.item) };
        (0..count)
            .map(|i| {
                let name = string_from_cf(unsafe { IOReportStateGetNameForIndex(self.item, i) });
                let name = if name.is_empty() {
                    format!("S{i}")
                } else {
                    name
                };
                (name, unsafe { IOReportStateGetResidency(self.item, i) })
            })
            .collect()
    }
}

/// An active IOReport subscription over a filtered channel set.
pub struct IoReport {
    subscription: IOReportSubscriptionRef,
    channels: CfOwned,
    ids: Vec<ChannelId>,
    prev: Option<Sample>,
}

/// One raw sample (opaque snapshot of all counters).
pub struct Sample {
    dict: CfOwned,
    pub taken_at: std::time::Instant,
}

impl IoReport {
    /// Subscribe to every channel for which `keep(group, subgroup, name)` is true.
    pub fn subscribe(keep: impl Fn(&str, &str, &str) -> bool) -> Result<Self, String> {
        unsafe {
            let all = CfOwned::from_create(IOReportCopyAllChannels(0, 0).cast())
                .ok_or("IOReportCopyAllChannels returned null")?;

            let size = CFDictionaryGetCount(all.as_dict());
            let subset = CfOwned::from_create(
                CFDictionaryCreateMutableCopy(kCFAllocatorDefault, size, all.as_dict()).cast(),
            )
            .ok_or("CFDictionaryCreateMutableCopy returned null")?;

            let key = cfstr("IOReportChannels");
            let arr: CFArrayRef = dict_get(all.as_dict(), &key)
                .ok_or("channel dict lacks IOReportChannels")?
                .cast();

            // Retaining callbacks: `picked` must keep the channel items alive
            // after the all-channels dictionary is released.
            let picked = CFArrayCreateMutable(
                kCFAllocatorDefault,
                CFArrayGetCount(arr),
                &raw const kCFTypeArrayCallBacks,
            );
            let mut ids = Vec::new();
            for item in array_iter(arr) {
                let item: CFDictionaryRef = item.cast();
                let group = string_from_cf(IOReportChannelGetGroup(item));
                let subgroup = string_from_cf(IOReportChannelGetSubGroup(item));
                let name = string_from_cf(IOReportChannelGetChannelName(item));
                if keep(&group, &subgroup, &name) {
                    let unit = string_from_cf(IOReportChannelGetUnitLabel(item))
                        .trim()
                        .to_owned();
                    ids.push(ChannelId { group, name, unit });
                    CFArrayAppendValue(picked, item.cast());
                }
            }
            if ids.is_empty() {
                CFRelease(picked.cast());
                return Err("no IOReport channels matched the filter".into());
            }
            CFDictionarySetValue(subset.as_mut_dict(), key.as_ptr(), picked.cast());
            CFRelease(picked.cast());

            let mut subscribed: CFMutableDictionaryRef = std::ptr::null_mut();
            let subscription = IOReportCreateSubscription(
                std::ptr::null(),
                subset.as_mut_dict(),
                &raw mut subscribed,
                0,
                std::ptr::null(),
            );
            if subscription.is_null() {
                return Err("IOReportCreateSubscription returned null".into());
            }
            // `subscribed` is an extra +1 dict we don't need.
            if !subscribed.is_null() {
                CFRelease(subscribed.cast());
            }

            Ok(Self {
                subscription,
                channels: subset,
                ids,
                prev: None,
            })
        }
    }

    /// Take a raw counter snapshot now.
    pub fn sample_now(&self) -> Result<Sample, String> {
        let dict = unsafe {
            IOReportCreateSamples(
                self.subscription,
                self.channels.as_mut_dict(),
                std::ptr::null(),
            )
        };
        let dict = unsafe { CfOwned::from_create(dict.cast()) }
            .ok_or("IOReportCreateSamples returned null")?;
        Ok(Sample {
            dict,
            taken_at: std::time::Instant::now(),
        })
    }

    /// Diff the previous snapshot against a fresh one, invoke
    /// `visit(dt_ms, item)` per channel, and store the fresh snapshot as the
    /// new baseline. `dt_ms` is the real elapsed window.
    ///
    /// Returns the window length, or `None` on the very first call
    /// (baseline only, no data yet).
    pub fn visit_delta(
        &mut self,
        visit: impl FnMut(u64, DeltaItem<'_>),
    ) -> Result<Option<u64>, String> {
        let curr = self.sample_now()?;
        let Some(prev) = self.prev.replace(curr) else {
            return Ok(None);
        };
        // `prev` was replaced by `curr`; re-borrow the new baseline for the diff.
        let curr = self.prev.as_ref().expect("baseline was just stored");
        let dt_ms = curr
            .taken_at
            .duration_since(prev.taken_at)
            .as_millis()
            .max(1) as u64;

        let delta = unsafe {
            IOReportCreateSamplesDelta(prev.dict.as_dict(), curr.dict.as_dict(), std::ptr::null())
        };
        let delta =
            unsafe { CfOwned::from_create(delta.cast()) }.ok_or("samples delta returned null")?;

        let key = cfstr("IOReportChannels");
        let arr: CFArrayRef = dict_get(delta.as_dict(), &key)
            .ok_or("delta lacks IOReportChannels")?
            .cast();

        let mut visit = visit;
        // Delta preserves subscription order → zip with cached ids by index.
        for (item, id) in array_iter(arr).zip(self.ids.iter()) {
            visit(
                dt_ms,
                DeltaItem {
                    item: item.cast(),
                    id,
                },
            );
        }
        Ok(Some(dt_ms))
    }
}

impl Drop for IoReport {
    fn drop(&mut self) {
        unsafe { CFRelease(self.subscription.cast()) };
    }
}

// The subscription handle and CF dictionaries are not thread-bound; IOReport is
// documented (by long-standing community use) as safe to sample from one thread
// at a time, which our sampler guarantees.
unsafe impl Send for IoReport {}
unsafe impl Send for Sample {}
