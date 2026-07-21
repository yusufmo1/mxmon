//! Unprivileged ICMP echo via a datagram socket. macOS allows
//! `SOCK_DGRAM` + `IPPROTO_ICMP` without root (it's how /sbin/ping ships
//! non-setuid), which keeps mxmon sudoless.
//!
//! Only the socket lifecycle and send/recv live here; packet building and
//! parsing are pure byte-slice functions so they can be unit-tested.

use std::io;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

/// Echo payload size; 8 header bytes on top give the classic 64-byte ping.
const PAYLOAD: usize = 56;
const ECHO_REQUEST: u8 = 8;
pub const ECHO_REPLY: u8 = 0;

/// RFC 1071 internet checksum (ones'-complement sum of 16-bit words).
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for pair in data.chunks(2) {
        let word = u32::from(pair[0]) << 8 | u32::from(*pair.get(1).unwrap_or(&0));
        sum += word;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// A 64-byte ICMP echo request with a valid checksum.
pub fn build_echo(ident: u16, seq: u16) -> [u8; 8 + PAYLOAD] {
    let mut pkt = [0u8; 8 + PAYLOAD];
    pkt[0] = ECHO_REQUEST;
    pkt[4..6].copy_from_slice(&ident.to_be_bytes());
    pkt[6..8].copy_from_slice(&seq.to_be_bytes());
    for (i, b) in pkt[8..].iter_mut().enumerate() {
        *b = i as u8;
    }
    let ck = checksum(&pkt);
    pkt[2..4].copy_from_slice(&ck.to_be_bytes());
    pkt
}

/// Parse a received datagram into `(icmp_type, ident, seq)`. The kernel may
/// hand dgram-ICMP receivers the whole IPv4 packet, so a leading IP header
/// (version nibble 4) is skipped; an echo reply itself starts with type 0,
/// which can't be mistaken for a version nibble.
pub fn parse_reply(datagram: &[u8]) -> Option<(u8, u16, u16)> {
    let icmp = if datagram.first()? >> 4 == 4 {
        let ihl = usize::from(datagram[0] & 0xf) * 4;
        datagram.get(ihl..)?
    } else {
        datagram
    };
    if icmp.len() < 8 {
        return None;
    }
    let ident = u16::from_be_bytes([icmp[4], icmp[5]]);
    let seq = u16::from_be_bytes([icmp[6], icmp[7]]);
    Some((icmp[0], ident, seq))
}

/// A connected ICMP datagram socket aimed at one host.
pub struct Pinger {
    fd: libc::c_int,
}

impl Pinger {
    pub fn open(dest: Ipv4Addr) -> io::Result<Self> {
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let pinger = Self { fd }; // drops (closes) on any error below

        let addr = libc::sockaddr_in {
            sin_len: size_of::<libc::sockaddr_in>() as u8,
            sin_family: libc::AF_INET as u8,
            sin_port: 0,
            sin_addr: libc::in_addr {
                // Octet order in memory *is* network order.
                s_addr: u32::from_ne_bytes(dest.octets()),
            },
            sin_zero: [0; 8],
        };
        let rc = unsafe {
            libc::connect(
                fd,
                (&raw const addr).cast(),
                size_of::<libc::sockaddr_in>() as u32,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(pinger)
    }

    /// One echo round-trip; `Ok(None)` on timeout. Blocks up to `timeout`,
    /// so callers live on their own thread, never the metrics loop.
    pub fn ping(&self, ident: u16, seq: u16, timeout: Duration) -> io::Result<Option<Duration>> {
        let pkt = build_echo(ident, seq);
        let sent_at = Instant::now();
        let n = unsafe { libc::send(self.fd, pkt.as_ptr().cast(), pkt.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }

        // The socket is connected and the kernel demuxes echo replies per
        // socket, but stale replies from a previous (timed-out) sequence can
        // still surface — keep reading until our seq or the deadline.
        let mut buf = [0u8; 512];
        loop {
            let Some(remaining) = timeout.checked_sub(sent_at.elapsed()) else {
                return Ok(None);
            };
            self.set_recv_timeout(remaining.max(Duration::from_millis(1)));
            let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                let err = io::Error::last_os_error();
                return match err.kind() {
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Ok(None),
                    _ => Err(err),
                };
            }
            if let Some((kind, _ident, got_seq)) = parse_reply(&buf[..n as usize])
                && kind == ECHO_REPLY
                && got_seq == seq
            {
                return Ok(Some(sent_at.elapsed()));
            }
        }
    }

    fn set_recv_timeout(&self, timeout: Duration) {
        let tv = libc::timeval {
            tv_sec: timeout.as_secs() as libc::time_t,
            tv_usec: libc::suseconds_t::from(timeout.subsec_micros() as i32),
        };
        unsafe {
            libc::setsockopt(
                self.fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const tv).cast(),
                size_of::<libc::timeval>() as u32,
            );
        }
    }
}

impl Drop for Pinger {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
