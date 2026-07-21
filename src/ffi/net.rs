//! Network interface counters via `sysctl NET_RT_IFLIST2` (64-bit counters,
//! one syscall for all interfaces) and local addresses via `getifaddrs`.

use std::collections::HashMap;
use std::io;

const RTM_IFINFO2: u8 = 0x12;

/// `struct if_data64` (net/if_var.h) — 64-bit interface statistics.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct IfData64 {
    ifi_type: u8,
    ifi_typelen: u8,
    ifi_physical: u8,
    ifi_addrlen: u8,
    ifi_hdrlen: u8,
    ifi_recvquota: u8,
    ifi_xmitquota: u8,
    ifi_unused1: u8,
    ifi_mtu: u32,
    ifi_metric: u32,
    ifi_baudrate: u64,
    ifi_ipackets: u64,
    ifi_ierrors: u64,
    ifi_opackets: u64,
    ifi_oerrors: u64,
    ifi_collisions: u64,
    ifi_ibytes: u64,
    ifi_obytes: u64,
    ifi_imcasts: u64,
    ifi_omcasts: u64,
    ifi_iqdrops: u64,
    ifi_noproto: u64,
    ifi_recvtiming: u32,
    ifi_xmittiming: u32,
    /// `struct timeval32` — 8 bytes even on LP64 (verified against the wire).
    ifi_lastchange: [i32; 2],
}

/// The record header must be exactly 160 bytes (empirically verified against
/// the kernel's NET_RT_IFLIST2 stream) or every name/counter drifts.
const _: () = assert!(size_of::<IfMsghdr2>() == 160);

/// Field offsets used by the byte-level parser, tied to the struct above so
/// a definition change breaks the build rather than the numbers.
const OFF_DATA: usize = std::mem::offset_of!(IfMsghdr2, ifm_data);
const OFF_BAUDRATE: usize = std::mem::offset_of!(IfData64, ifi_baudrate);
const OFF_IBYTES: usize = std::mem::offset_of!(IfData64, ifi_ibytes);
const OFF_OBYTES: usize = std::mem::offset_of!(IfData64, ifi_obytes);
const _: () = assert!(OFF_DATA == 32 && OFF_BAUDRATE == 16 && OFF_IBYTES == 64 && OFF_OBYTES == 72);

/// `struct if_msghdr2` (net/if.h).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct IfMsghdr2 {
    ifm_msglen: u16,
    ifm_version: u8,
    ifm_type: u8,
    ifm_addrs: i32,
    ifm_flags: i32,
    ifm_index: u16,
    ifm_snd_len: i32,
    ifm_snd_maxlen: i32,
    ifm_snd_drops: i32,
    ifm_timer: i32,
    ifm_data: IfData64,
}

/// Cumulative counters for one interface.
#[derive(Debug, Clone, Default)]
pub struct IfCounters {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub baudrate: u64,
    pub up: bool,
    /// `IFF_RUNNING` — the link layer is actually up, not just configured.
    pub running: bool,
    pub loopback: bool,
    /// Hardware (MAC) address, when the link record carries one.
    pub mac: Option<String>,
}

/// Debug aid: actual compiled constants/offsets and a raw hexdump of the
/// first record whose link name matches `grep`.
pub fn layout_report(grep: &str) -> String {
    use std::fmt::Write as _;
    let mut report = format!(
        "CTL_NET={} PF_ROUTE={} NET_RT_IFLIST2={} msghdr2={} ibytes_off={}\n",
        libc::CTL_NET,
        libc::PF_ROUTE,
        libc::NET_RT_IFLIST2,
        size_of::<IfMsghdr2>(),
        OFF_DATA + OFF_IBYTES,
    );
    if let Ok(buf) = raw_iflist2() {
        let mut offset = 0usize;
        while offset + size_of::<IfMsghdr2>() <= buf.len() {
            let msglen = u16::from_ne_bytes([buf[offset], buf[offset + 1]]) as usize;
            if msglen == 0 {
                break;
            }
            let record = &buf[offset..(offset + msglen).min(buf.len())];
            if buf[offset + 3] == RTM_IFINFO2 && parse_link(record).0 == grep {
                let _ = write!(report, "record@{offset} len={msglen} bytes[88..112]=");
                for b in &record[88..112] {
                    let _ = write!(report, "{b:02x} ");
                }
            }
            offset += msglen;
        }
    }
    report
}

fn raw_iflist2() -> io::Result<Vec<u8>> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut len: usize = 0;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf: Vec<u8> = vec![0; len];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len);
    Ok(buf)
}

/// Snapshot every interface's 64-bit byte counters (single sysctl).
pub fn interface_counters() -> io::Result<Vec<IfCounters>> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut len: usize = 0;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf: Vec<u8> = vec![0; len];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + size_of::<IfMsghdr2>() <= len {
        // Route messages are variable-length records led by (msglen, version, type).
        let msglen = u16::from_ne_bytes([buf[offset], buf[offset + 1]]) as usize;
        if msglen == 0 {
            break;
        }
        let msg_type = buf[offset + 3];
        if msg_type == RTM_IFINFO2 && offset + size_of::<IfMsghdr2>() <= len {
            let record = &buf[offset..(offset + msglen).min(len)];
            let u64_at = |o: usize| {
                u64::from_ne_bytes(record[o..o + 8].try_into().expect("record bounds checked"))
            };
            let flags =
                i32::from_ne_bytes(record[8..12].try_into().expect("record bounds checked"));
            // if_data64 starts at 32; baudrate +16, ibytes +64, obytes +72
            // (offsets asserted against the struct definition below).
            let (name, mac) = parse_link(record);
            out.push(IfCounters {
                loopback: flags & libc::IFF_LOOPBACK != 0,
                up: flags & libc::IFF_UP != 0,
                running: flags & libc::IFF_RUNNING != 0,
                baudrate: u64_at(OFF_DATA + OFF_BAUDRATE),
                rx_bytes: u64_at(OFF_DATA + OFF_IBYTES),
                tx_bytes: u64_at(OFF_DATA + OFF_OBYTES),
                name,
                mac,
            });
        }
        offset += msglen;
    }
    Ok(out)
}

/// Extract the interface name and MAC address from the `sockaddr_dl`
/// trailing a `RTM_IFINFO2` record. Starts at the compiled header size and
/// scans forward defensively for the `[sdl_len, AF_LINK]` signature.
fn parse_link(record: &[u8]) -> (String, Option<String>) {
    let start = size_of::<IfMsghdr2>();
    for probe in start..record.len().saturating_sub(8) {
        let sdl_len = record[probe] as usize;
        let family = record[probe + 1];
        if family == libc::AF_LINK as u8 && sdl_len >= 8 && probe + sdl_len <= record.len() {
            let nlen = record[probe + 5] as usize;
            let alen = record[probe + 6] as usize;
            let name = &record[probe + 8..(probe + 8 + nlen).min(record.len())];
            if !name.is_empty() && name.iter().all(|&b| (0x21..0x7f).contains(&b)) {
                // The hardware address sits right after the name; 6 bytes on
                // anything Ethernet-shaped (an all-zero one means "none").
                let mac = record
                    .get(probe + 8 + nlen..probe + 8 + nlen + alen)
                    .filter(|m| m.len() == 6 && m.iter().any(|&b| b != 0))
                    .map(mac_string);
                return (String::from_utf8_lossy(name).into_owned(), mac);
            }
        }
        // Only scan a small window past the expected offset.
        if probe > start + 32 {
            break;
        }
    }
    (String::new(), None)
}

/// `b4:e9:b8:6d:3b:d6`-style rendering of a hardware address.
pub fn mac_string(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(':');
        }
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Map of interface name → first IPv4 address, via `getifaddrs`.
pub fn ipv4_addresses() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&raw mut ifap) } != 0 {
        return out;
    }
    let mut cursor = ifap;
    while !cursor.is_null() {
        let ifa = unsafe { &*cursor };
        cursor = ifa.ifa_next;
        if ifa.ifa_addr.is_null() {
            continue;
        }
        let family = unsafe { (*ifa.ifa_addr).sa_family };
        if i32::from(family) != libc::AF_INET {
            continue;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        // getifaddrs records aren't alignment-guaranteed; copy out unaligned.
        let sin = unsafe { std::ptr::read_unaligned(ifa.ifa_addr.cast::<libc::sockaddr_in>()) };
        let octets = sin.sin_addr.s_addr.to_ne_bytes();
        out.entry(name)
            .or_insert_with(|| format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]));
    }
    unsafe { libc::freeifaddrs(ifap) };
    out
}

#[cfg(test)]
mod tests {
    use super::mac_string;

    #[test]
    fn mac_formatting() {
        assert_eq!(
            mac_string(&[0xb4, 0xe9, 0xb8, 0x6d, 0x3b, 0xd6]),
            "b4:e9:b8:6d:3b:d6"
        );
        assert_eq!(mac_string(&[0x00, 0x0a, 0xff]), "00:0a:ff");
    }
}
