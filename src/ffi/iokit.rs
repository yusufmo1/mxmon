//! IOKit registry helpers: service iteration, pmgr DVFS tables, and the
//! `AGXAccelerator` GPU performance statistics.

use std::ffi::c_void;
use std::io;

use core_foundation::base::kCFAllocatorDefault;
use core_foundation::dictionary::{CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation::number::{CFNumberGetValue, CFNumberRef, kCFNumberSInt64Type};

use super::cf::CfOwned;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const u8) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: CFDictionaryRef,
        iterator: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IORegistryEntryGetName(entry: u32, name: *mut u8) -> i32;
    fn IORegistryEntryCreateCFProperties(
        entry: u32,
        properties: *mut CFMutableDictionaryRef,
        allocator: *const c_void,
        options: u32,
    ) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: core_foundation::string::CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> *const c_void;
    fn IOObjectRelease(object: u32) -> u32;
    pub fn IOServiceOpen(device: u32, owning_task: u32, conn_type: u32, conn: *mut u32) -> i32;
    pub fn IOServiceClose(conn: u32) -> i32;
    pub fn IOConnectCallStructMethod(
        conn: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> i32;
    pub fn mach_task_self() -> u32;
}

/// An IOKit object handle released on drop.
pub struct IoObject(u32);

impl IoObject {
    pub fn raw(&self) -> u32 {
        self.0
    }

    /// The registry entry's class/instance name.
    pub fn name(&self) -> String {
        let mut buf = [0u8; 128];
        if unsafe { IORegistryEntryGetName(self.0, buf.as_mut_ptr()) } != 0 {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    /// Copy a single property — far cheaper than [`Self::properties`] when
    /// the entry's table is large (the AGX accelerator's spans hundreds of
    /// nested entries). `None` when the key is absent.
    pub fn property(&self, key: &CfOwned) -> Option<CfOwned> {
        let ptr = unsafe {
            IORegistryEntryCreateCFProperty(self.0, key.as_ptr().cast(), kCFAllocatorDefault, 0)
        };
        unsafe { CfOwned::from_create(ptr.cast()) }
    }

    /// Copy the entry's property table.
    pub fn properties(&self) -> io::Result<CfOwned> {
        let mut props: CFMutableDictionaryRef = std::ptr::null_mut();
        let kr = unsafe {
            IORegistryEntryCreateCFProperties(self.0, &raw mut props, kCFAllocatorDefault, 0)
        };
        if kr != 0 {
            return Err(io::Error::other(format!(
                "IORegistryEntryCreateCFProperties: {kr:#x}"
            )));
        }
        unsafe { CfOwned::from_create(props.cast()) }
            .ok_or_else(|| io::Error::other("null property dict"))
    }
}

impl Drop for IoObject {
    fn drop(&mut self) {
        unsafe { IOObjectRelease(self.0) };
    }
}

/// Iterate all services matching an IOKit class name, yielding live objects
/// paired with their registry names (objects stay valid until dropped).
pub fn service_iter(class: &str) -> io::Result<Vec<(IoObject, String)>> {
    services(class).map(|objs| {
        objs.into_iter()
            .map(|o| {
                let name = o.name();
                (o, name)
            })
            .collect()
    })
}

/// Owned service objects matching an IOKit class name.
pub fn services(class: &str) -> io::Result<Vec<IoObject>> {
    let mut class_c = class.as_bytes().to_vec();
    class_c.push(0);
    let matching = unsafe { IOServiceMatching(class_c.as_ptr()) };
    if matching.is_null() {
        return Err(io::Error::other(format!(
            "IOServiceMatching({class}) returned null"
        )));
    }
    let mut iter = 0u32;
    // IOServiceGetMatchingServices consumes `matching`.
    let kr = unsafe { IOServiceGetMatchingServices(0, matching.cast(), &raw mut iter) };
    if kr != 0 {
        return Err(io::Error::other(format!(
            "IOServiceGetMatchingServices: {kr:#x}"
        )));
    }
    let mut out = Vec::new();
    loop {
        let obj = unsafe { IOIteratorNext(iter) };
        if obj == 0 {
            break;
        }
        out.push(IoObject(obj));
    }
    unsafe { IOObjectRelease(iter) };
    Ok(out)
}

/// Read an i64 out of a borrowed CFNumber pointer.
pub fn cf_number_i64(ptr: *const c_void) -> Option<i64> {
    if ptr.is_null() {
        return None;
    }
    let mut value: i64 = 0;
    let ok = unsafe {
        CFNumberGetValue(
            ptr as CFNumberRef,
            kCFNumberSInt64Type,
            (&raw mut value).cast(),
        )
    };
    ok.then_some(value)
}
