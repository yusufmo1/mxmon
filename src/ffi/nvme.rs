//! NVMe SMART health, read through the public `IONVMeSMARTInterface` plug-in.
//!
//! Nothing SMART-shaped exists as an IORegistry property — a grep of the whole
//! registry finds none of it — so the only way in is IOKit's COM-style plug-in
//! dance: `IOCreatePlugInInterfaceForService` for the user-client type, then
//! `QueryInterface` for the SMART interface, then one `SMARTReadData` call.
//! No entitlement and no root: verified reading a real drive as an ordinary
//! user.
//!
//! The interface hands back the NVMe SMART / Health Information log page
//! verbatim. That is wire data, so it is decoded here with bounds-checked
//! reads rather than transmuted onto a struct — the same rule `flows::wire`
//! follows, and for the same reason: a layout change must degrade to "no
//! reading", never to a crash or a garbage number.

use std::ffi::c_void;

use core_foundation::base::CFTypeRef;

use super::iokit::services;

// ---- COM plumbing --------------------------------------------------------

/// `CFUUIDBytes` — 16 bytes passed by value.
#[repr(C)]
#[derive(Clone, Copy)]
struct CFUUIDBytes {
    b: [u8; 16],
}

/// `IUNKNOWN_C_GUTS`, the head of every IOKit plug-in vtable.
#[repr(C)]
struct IUnknownVTbl {
    _reserved: *mut c_void,
    query_interface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    release: unsafe extern "C" fn(*mut c_void) -> u32,
}

/// `IOCFPlugInInterface`. Only the header is described: `repr(C)` reproduces
/// the same trailing padding after `version`/`revision` that the C compiler
/// inserts, and the methods past it are never called.
#[repr(C)]
struct PlugInVTbl {
    unknown: IUnknownVTbl,
    version: u16,
    revision: u16,
    _probe: *mut c_void,
    _start: *mut c_void,
    _stop: *mut c_void,
}

/// `IONVMeSMARTInterface`, truncated after the one method used.
#[repr(C)]
struct SmartVTbl {
    unknown: IUnknownVTbl,
    version: u16,
    revision: u16,
    smart_read_data: unsafe extern "C" fn(*mut c_void, *mut u8) -> i32,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDGetConstantUUIDWithBytes(
        alloc: CFTypeRef,
        b0: u8,
        b1: u8,
        b2: u8,
        b3: u8,
        b4: u8,
        b5: u8,
        b6: u8,
        b7: u8,
        b8: u8,
        b9: u8,
        b10: u8,
        b11: u8,
        b12: u8,
        b13: u8,
        b14: u8,
        b15: u8,
    ) -> CFTypeRef;
    fn CFUUIDGetUUIDBytes(uuid: CFTypeRef) -> CFUUIDBytes;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOCreatePlugInInterfaceForService(
        service: u32,
        plugin_type: CFTypeRef,
        interface_type: CFTypeRef,
        the_interface: *mut *mut *mut PlugInVTbl,
        score: *mut i32,
    ) -> i32;
    fn IODestroyPlugInInterface(interface: *mut *mut PlugInVTbl) -> i32;
}

/// `kIONVMeSMARTUserClientTypeID`.
fn user_client_type() -> CFTypeRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            0xAA,
            0x0F,
            0xA6,
            0xF9,
            0xC2,
            0xD6,
            0x45,
            0x7F,
            0xB1,
            0x0B,
            0x59,
            0xA1,
            0x32,
            0x53,
            0x29,
            0x2F,
        )
    }
}

/// `kIONVMeSMARTInterfaceID`.
fn smart_interface_id() -> CFTypeRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            0xCC,
            0xD1,
            0xDB,
            0x19,
            0xFD,
            0x9A,
            0x4D,
            0xAF,
            0xBF,
            0x95,
            0x12,
            0x45,
            0x4B,
            0x23,
            0x0A,
            0xB6,
        )
    }
}

/// `kIOCFPlugInInterfaceID`.
fn plugin_interface_id() -> CFTypeRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            0xC2,
            0x44,
            0xE8,
            0x58,
            0x10,
            0x9C,
            0x11,
            0xD4,
            0x91,
            0xD4,
            0x00,
            0x50,
            0xE4,
            0xC6,
            0x42,
            0x6F,
        )
    }
}

// ---- decoded data --------------------------------------------------------

/// The NVMe SMART log page is a fixed 512-byte structure.
const LOG_PAGE_LEN: usize = 512;

/// What we actually hand `SMARTReadData`. It takes a bare pointer with no
/// length, so the buffer must be at least as large as whatever the framework
/// decides to write — see the note at the call site.
const SMART_BUFFER_LEN: usize = 8192;

/// `DATA_UNITS_*` count 512 000-byte units, not blocks — a detail worth
/// getting right, since it is off by a factor of a thousand otherwise.
const DATA_UNIT_BYTES: u64 = 512_000;

/// A drive's SMART health.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Smart {
    /// Bit field; any bit set means the controller is reporting a fault.
    pub critical_warning: u8,
    /// Composite temperature in whole °C, converted from the log's Kelvin.
    pub temperature_c: Option<i32>,
    /// Spare capacity remaining, and the threshold below which the drive
    /// considers itself failing.
    pub available_spare_pct: u8,
    pub available_spare_threshold_pct: u8,
    /// Estimated share of rated write endurance consumed. Can exceed 100.
    pub percentage_used: u8,
    pub bytes_read: u128,
    pub bytes_written: u128,
    pub host_read_commands: u128,
    pub host_write_commands: u128,
    pub power_cycles: u128,
    pub power_on_hours: u128,
    /// Power losses without a clean shutdown.
    pub unsafe_shutdowns: u128,
    pub media_errors: u128,
    pub error_log_entries: u128,
}

impl Smart {
    /// Whether the controller is flagging a real problem.
    pub fn unhealthy(&self) -> bool {
        self.critical_warning != 0
            || self.media_errors > 0
            || self.available_spare_pct < self.available_spare_threshold_pct
    }

    /// Decode the SMART log page.
    ///
    /// Every read is bounds-checked against the slice, so a short, padded, or
    /// restructured page yields `None` instead of reading past the buffer.
    pub fn decode(page: &[u8]) -> Option<Self> {
        if page.len() < LOG_PAGE_LEN {
            return None;
        }
        let u16_at = |off: usize| -> Option<u16> {
            Some(u16::from_le_bytes(page.get(off..off + 2)?.try_into().ok()?))
        };
        // The log's wide counters are 128-bit little-endian.
        let u128_at = |off: usize| -> Option<u128> {
            Some(u128::from_le_bytes(
                page.get(off..off + 16)?.try_into().ok()?,
            ))
        };

        let kelvin = u16_at(1)?;
        Some(Self {
            critical_warning: *page.first()?,
            // 0 K is the controller saying "no reading", not absolute zero.
            temperature_c: (kelvin > 0).then(|| i32::from(kelvin) - 273),
            available_spare_pct: *page.get(3)?,
            available_spare_threshold_pct: *page.get(4)?,
            percentage_used: *page.get(5)?,
            bytes_read: u128_at(32)?.saturating_mul(u128::from(DATA_UNIT_BYTES)),
            bytes_written: u128_at(48)?.saturating_mul(u128::from(DATA_UNIT_BYTES)),
            host_read_commands: u128_at(64)?,
            host_write_commands: u128_at(80)?,
            power_cycles: u128_at(112)?,
            power_on_hours: u128_at(128)?,
            unsafe_shutdowns: u128_at(144)?,
            media_errors: u128_at(160)?,
            error_log_entries: u128_at(176)?,
        })
    }
}

/// Read SMART from the first NVMe device that offers the interface.
///
/// `None` covers every uninteresting case identically — no NVMe device, no
/// plug-in, a refused `QueryInterface`, or a page that failed to decode.
pub fn read() -> Option<Smart> {
    // The embedded (internal) drive first; a discrete controller otherwise.
    for class in ["IOEmbeddedNVMeBlockDevice", "IONVMeController"] {
        let Ok(devices) = services(class) else {
            continue;
        };
        for device in devices {
            if let Some(smart) = read_from(device.raw()) {
                return Some(smart);
            }
        }
    }
    None
}

/// The plug-in dance for one service. Every early return releases whatever
/// has been acquired so far.
fn read_from(service: u32) -> Option<Smart> {
    let mut plugin: *mut *mut PlugInVTbl = std::ptr::null_mut();
    let mut score: i32 = 0;
    let kr = unsafe {
        IOCreatePlugInInterfaceForService(
            service,
            user_client_type(),
            plugin_interface_id(),
            &raw mut plugin,
            &raw mut score,
        )
    };
    if kr != 0 || plugin.is_null() {
        return None;
    }

    let mut smart_ptr: *mut c_void = std::ptr::null_mut();
    let iid = unsafe { CFUUIDGetUUIDBytes(smart_interface_id()) };
    let query = unsafe { (**plugin).unknown.query_interface };
    let kr = unsafe { query(plugin.cast(), iid, &raw mut smart_ptr) };
    // The plug-in has served its purpose either way; the SMART interface holds
    // its own reference from here.
    if kr != 0 || smart_ptr.is_null() {
        unsafe { IODestroyPlugInInterface(plugin) };
        return None;
    }

    let smart_iface: *mut *mut SmartVTbl = smart_ptr.cast();
    // Heap, and far larger than the 512-byte log page we read back.
    //
    // `SMARTReadData` fills a `NVMeSMARTData` whose true size is defined by a
    // header we do not have, and it is handed a bare pointer with no length.
    // Sizing the buffer to the part we parse let it write past the end: the
    // decoded values were correct, and an unrelated CoreFoundation call
    // crashed later with a corrupted heap. Over-allocating costs one page and
    // removes the whole failure mode.
    let mut page = vec![0u8; SMART_BUFFER_LEN];
    let read_fn = unsafe { (**smart_iface).smart_read_data };
    let kr = unsafe { read_fn(smart_ptr, page.as_mut_ptr()) };
    let out = (kr == 0)
        .then(|| Smart::decode(&page[..LOG_PAGE_LEN]))
        .flatten();

    let release = unsafe { (**smart_iface).unknown.release };
    unsafe { release(smart_ptr) };
    unsafe { IODestroyPlugInInterface(plugin) };
    out
}

#[cfg(test)]
mod tests {
    use super::{DATA_UNIT_BYTES, LOG_PAGE_LEN, Smart};

    /// A log page built the way the controller lays one out.
    fn page(f: impl FnOnce(&mut [u8])) -> Vec<u8> {
        let mut p = vec![0u8; LOG_PAGE_LEN];
        f(&mut p);
        p
    }

    fn put128(p: &mut [u8], off: usize, v: u128) {
        p[off..off + 16].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn decodes_a_realistic_page() {
        // Values captured from a real drive during the on-device survey.
        let p = page(|p| {
            p[0] = 0; // no critical warning
            p[1..3].copy_from_slice(&308u16.to_le_bytes()); // 308 K
            p[3] = 100; // spare
            p[4] = 99; // threshold
            p[5] = 6; // 6% of endurance used
            put128(p, 32, 579_472_699); // data units read
            put128(p, 48, 1_401_775_626); // data units written
            put128(p, 64, 18_302_504_183);
            put128(p, 80, 24_497_644_723);
            put128(p, 112, 441); // power cycles
            put128(p, 128, 4383); // power-on hours
            put128(p, 144, 62); // unsafe shutdowns
        });
        let s = Smart::decode(&p).expect("well-formed page decodes");
        assert_eq!(s.temperature_c, Some(35));
        assert_eq!(s.percentage_used, 6);
        assert_eq!(s.available_spare_pct, 100);
        assert_eq!(s.power_on_hours, 4383);
        assert_eq!(s.power_cycles, 441);
        assert_eq!(s.unsafe_shutdowns, 62);
        // 1.4e9 units × 512 kB ≈ 717 TB, matching the surveyed drive.
        assert_eq!(s.bytes_written, 1_401_775_626 * u128::from(DATA_UNIT_BYTES));
        assert_eq!(s.bytes_written / 1_000_000_000_000, 717);
        assert!(!s.unhealthy());
    }

    #[test]
    fn a_short_page_decodes_to_nothing_rather_than_reading_past_it() {
        assert_eq!(Smart::decode(&[]), None);
        assert_eq!(Smart::decode(&[0u8; LOG_PAGE_LEN - 1]), None);
        assert!(Smart::decode(&[0u8; LOG_PAGE_LEN]).is_some());
        // A longer page is fine — later revisions may append fields.
        assert!(Smart::decode(&[0u8; LOG_PAGE_LEN * 2]).is_some());
    }

    #[test]
    fn a_zero_kelvin_reading_is_absent_not_freezing() {
        // 0 K means the controller reported nothing; -273 °C would be a lie.
        let s = Smart::decode(&page(|_| {})).expect("zeroed page decodes");
        assert_eq!(s.temperature_c, None);
    }

    #[test]
    fn health_flags_every_way_a_drive_can_be_failing() {
        let warn = Smart::decode(&page(|p| p[0] = 0x01)).unwrap();
        assert!(warn.unhealthy());

        let errors = Smart::decode(&page(|p| put128(p, 160, 3))).unwrap();
        assert!(errors.unhealthy());

        // Spare fallen below the controller's own threshold.
        let spare = Smart::decode(&page(|p| {
            p[3] = 4;
            p[4] = 10;
        }))
        .unwrap();
        assert!(spare.unhealthy());

        let healthy = Smart::decode(&page(|p| {
            p[3] = 100;
            p[4] = 10;
        }))
        .unwrap();
        assert!(!healthy.unhealthy());
    }

    #[test]
    fn saturating_byte_totals_never_overflow() {
        // A garbage 128-bit unit count must not wrap when scaled to bytes.
        let s = Smart::decode(&page(|p| put128(p, 48, u128::MAX))).unwrap();
        assert_eq!(s.bytes_written, u128::MAX);
    }

    mod prop {
        use super::super::Smart;
        use proptest::prelude::*;

        proptest! {
            // The log page is wire data from firmware: any bytes at any length
            // must decode or decline, never panic.
            #[test]
            fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..1200)) {
                let _ = Smart::decode(&bytes);
            }

            // Full-length pages exercise every field extraction.
            #[test]
            fn full_length_pages_always_decode(
                bytes in proptest::collection::vec(any::<u8>(), 512..600)
            ) {
                let decoded = Smart::decode(&bytes);
                prop_assert!(decoded.is_some());
                if let Some(s) = decoded {
                    let _ = s.unhealthy();
                }
            }
        }
    }
}
