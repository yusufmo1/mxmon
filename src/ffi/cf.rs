//! Minimal safe helpers over CoreFoundation objects.
//!
//! Everything returned to callers is either a plain Rust value or an owned
//! [`CfOwned`] that releases its CF object on drop. Raw `CFTypeRef`s never
//! escape the `ffi` layer.

use std::ffi::c_void;

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, CFTypeRef, kCFAllocatorDefault, kCFAllocatorNull};
use core_foundation::data::{CFDataGetBytePtr, CFDataGetLength, CFDataRef};
use core_foundation::dictionary::{CFDictionaryGetValue, CFDictionaryRef, CFMutableDictionaryRef};
use core_foundation::string::{
    CFStringCreateWithBytes, CFStringCreateWithBytesNoCopy, CFStringGetCString, CFStringRef,
    kCFStringEncodingUTF8,
};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
}

/// Whether a borrowed CF pointer really is the type we are about to treat it
/// as.
///
/// Registry property tables are external data: a key can appear with a type
/// nobody expected, and calling a `CFString` accessor on a `CFData` is not a
/// wrong answer but a crash. Every typed accessor below checks first, so a
/// surprising property degrades to `None`.
fn is_type(p: *const c_void, want: unsafe extern "C" fn() -> usize) -> bool {
    !p.is_null() && unsafe { CFGetTypeID(p.cast()) == want() }
}

/// A borrowed pointer as a `String`, or `None` if it is not a `CFString`.
pub fn as_string(p: *const c_void) -> Option<String> {
    is_type(p, CFStringGetTypeID).then(|| string_from_cf(p.cast()))
}

/// A borrowed pointer as a dictionary, or `None` if it is not one.
pub fn as_dict(p: *const c_void) -> Option<CFDictionaryRef> {
    is_type(p, CFDictionaryGetTypeID).then(|| p.cast())
}

/// A borrowed pointer as an array, or `None` if it is not one.
pub fn as_array(p: *const c_void) -> Option<CFArrayRef> {
    is_type(p, CFArrayGetTypeID).then(|| p.cast())
}

/// Whether a borrowed pointer is a `CFNumber`.
pub fn is_number(p: *const c_void) -> bool {
    is_type(p, CFNumberGetTypeID)
}

/// An owned CoreFoundation object, released on drop.
pub struct CfOwned(CFTypeRef);

impl CfOwned {
    /// Take ownership of a CF object the caller obtained from a Create/Copy
    /// function (i.e. we now hold its +1 retain).
    ///
    /// # Safety
    /// `ptr` must be a valid CF object with an unconsumed +1 retain count, or null.
    pub unsafe fn from_create(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then_some(Self(ptr))
    }

    pub fn as_ptr(&self) -> CFTypeRef {
        self.0
    }

    pub fn as_dict(&self) -> CFDictionaryRef {
        self.0.cast()
    }

    pub fn as_mut_dict(&self) -> CFMutableDictionaryRef {
        self.0.cast_mut().cast()
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

/// A borrowed CFString wrapper that only exists to build short-lived keys.
/// Uses `CFStringCreateWithBytesNoCopy` + a `'static` byte slice, avoiding the
/// known breakage of `CFString::from_static_string` for strings > 9 chars.
pub fn cfstr(s: &'static str) -> CfOwned {
    let ptr = unsafe {
        CFStringCreateWithBytesNoCopy(
            kCFAllocatorDefault,
            s.as_ptr(),
            s.len() as isize,
            kCFStringEncodingUTF8,
            0,
            kCFAllocatorNull,
        )
    };
    unsafe { CfOwned::from_create(ptr.cast()) }.expect("CFString allocation cannot fail")
}

/// [`cfstr`] for runtime-built keys (the M5+ pmgr table names resolved from
/// `acc-clusters`): the bytes are copied into the CFString, so any lifetime
/// works. Prefer [`cfstr`] for literal keys — it skips the copy.
pub fn cfstr_copy(s: &str) -> CfOwned {
    let ptr = unsafe {
        CFStringCreateWithBytes(
            kCFAllocatorDefault,
            s.as_ptr(),
            s.len() as isize,
            kCFStringEncodingUTF8,
            0,
        )
    };
    unsafe { CfOwned::from_create(ptr.cast()) }.expect("CFString allocation cannot fail")
}

/// Copy a borrowed `CFStringRef` into a Rust `String` (empty if null).
pub fn string_from_cf(s: CFStringRef) -> String {
    if s.is_null() {
        return String::new();
    }
    let mut buf = [0u8; 256];
    let ok = unsafe {
        CFStringGetCString(
            s,
            buf.as_mut_ptr().cast(),
            buf.len() as isize,
            kCFStringEncodingUTF8,
        )
    };
    if ok == 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Value of a borrowed CFBoolean pointer.
pub fn bool_from_cf(p: *const c_void) -> bool {
    unsafe { core_foundation::number::CFBooleanGetValue(p.cast()) }
}

/// Look up `key` in a borrowed dictionary; returns a *borrowed* pointer.
pub fn dict_get(dict: CFDictionaryRef, key: &CfOwned) -> Option<*const c_void> {
    if dict.is_null() {
        return None;
    }
    let v = unsafe { CFDictionaryGetValue(dict, key.as_ptr()) };
    (!v.is_null()).then_some(v)
}

/// Iterate a borrowed `CFArrayRef`, yielding borrowed element pointers.
/// Caller must keep the array alive for the duration of iteration.
pub fn array_iter(arr: CFArrayRef) -> impl Iterator<Item = *const c_void> {
    let count = if arr.is_null() {
        0
    } else {
        unsafe { CFArrayGetCount(arr) }
    };
    (0..count).map(move |i| unsafe { CFArrayGetValueAtIndex(arr, i) })
}

/// Copy a borrowed `CFDataRef`'s bytes into a Vec.
pub fn data_bytes(data: CFDataRef) -> Vec<u8> {
    if data.is_null() {
        return Vec::new();
    }
    unsafe {
        let len = CFDataGetLength(data) as usize;
        let ptr = CFDataGetBytePtr(data);
        std::slice::from_raw_parts(ptr, len).to_vec()
    }
}
