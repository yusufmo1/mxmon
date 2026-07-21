//! Named temperature sensors via the IOHID event system (AppleSMC-backed HID
//! services expose human-readable names like "pACC MTR Temp Sensor4").

use core_foundation::array::CFArrayRef;
use core_foundation::base::{CFRelease, CFTypeRef, kCFAllocatorDefault};
use core_foundation::dictionary::{
    CFDictionaryCreate, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
};
use core_foundation::number::{CFNumberCreate, kCFNumberSInt32Type};
use core_foundation::string::CFStringRef;

use super::cf::{CfOwned, array_iter, cfstr, string_from_cf};

const PAGE_APPLE_VENDOR: i32 = 0xff00;
const USAGE_TEMP_SENSOR: i32 = 5;
const EVENT_TYPE_TEMPERATURE: i64 = 15;
const FIELD_TEMPERATURE_LEVEL: u32 = (EVENT_TYPE_TEMPERATURE as u32) << 16;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreate(allocator: *const std::ffi::c_void) -> CFTypeRef;
    fn IOHIDEventSystemClientSetMatching(client: CFTypeRef, matching: CFTypeRef) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: CFTypeRef) -> CFArrayRef;
    fn IOHIDServiceClientCopyProperty(service: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDServiceClientCopyEvent(service: CFTypeRef, kind: i64, a: i32, b: i32) -> CFTypeRef;
    fn IOHIDEventGetFloatValue(event: CFTypeRef, field: u32) -> f64;
}

/// A live handle to all Apple-vendor temperature HID services.
pub struct HidTemps {
    _client: CfOwned,
    /// Borrowed service pointers owned by `services` array.
    services: Vec<(*const std::ffi::c_void, String)>,
    _services_array: CfOwned,
}

impl HidTemps {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let client =
                CfOwned::from_create(IOHIDEventSystemClientCreate(kCFAllocatorDefault.cast()))
                    .ok_or("IOHIDEventSystemClientCreate returned null")?;

            let page = CFNumberCreate(
                kCFAllocatorDefault,
                kCFNumberSInt32Type,
                std::ptr::from_ref::<i32>(&PAGE_APPLE_VENDOR).cast(),
            );
            let usage = CFNumberCreate(
                kCFAllocatorDefault,
                kCFNumberSInt32Type,
                std::ptr::from_ref::<i32>(&USAGE_TEMP_SENSOR).cast(),
            );
            let keys = [cfstr("PrimaryUsagePage"), cfstr("PrimaryUsage")];
            let key_ptrs = [keys[0].as_ptr(), keys[1].as_ptr()];
            let val_ptrs: [*const std::ffi::c_void; 2] = [page.cast(), usage.cast()];
            let matching = CFDictionaryCreate(
                kCFAllocatorDefault,
                key_ptrs.as_ptr().cast_mut().cast(),
                val_ptrs.as_ptr().cast_mut().cast(),
                2,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            );
            IOHIDEventSystemClientSetMatching(client.as_ptr(), matching.cast());
            CFRelease(matching.cast());
            CFRelease(page.cast());
            CFRelease(usage.cast());

            let services_array =
                CfOwned::from_create(IOHIDEventSystemClientCopyServices(client.as_ptr()).cast())
                    .ok_or("no HID temperature services")?;

            let product_key = cfstr("Product");
            let mut services = Vec::new();
            for svc in array_iter(services_array.as_ptr().cast()) {
                let name_ref =
                    IOHIDServiceClientCopyProperty(svc.cast(), product_key.as_ptr().cast());
                if name_ref.is_null() {
                    continue;
                }
                let name = string_from_cf(name_ref.cast());
                CFRelease(name_ref);
                if !name.is_empty() {
                    services.push((svc, name));
                }
            }
            Ok(Self {
                _client: client,
                services,
                _services_array: services_array,
            })
        }
    }

    /// Drop services whose name fails `keep`. Each retained service costs one
    /// mach IPC per [`read_all`], so callers should shed sensors they never
    /// display (e.g. calibration channels) once, up front.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.services.retain(|(_, name)| keep(name));
    }

    /// Read every sensor: `(name, °C)`. Sensors that fail to read are skipped.
    pub fn read_all(&self) -> Vec<(&str, f32)> {
        let mut out = Vec::with_capacity(self.services.len());
        for (svc, name) in &self.services {
            let event =
                unsafe { IOHIDServiceClientCopyEvent(svc.cast(), EVENT_TYPE_TEMPERATURE, 0, 0) };
            if event.is_null() {
                continue;
            }
            let value = unsafe { IOHIDEventGetFloatValue(event, FIELD_TEMPERATURE_LEVEL) } as f32;
            unsafe { CFRelease(event) };
            out.push((name.as_str(), value));
        }
        out
    }
}

// Constructed and used on one sampler thread only; the raw service pointers
// are kept alive by the owned CFArray for the collector's lifetime.
unsafe impl Send for HidTemps {}
