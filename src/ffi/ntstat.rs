//! Kernel-control socket to `com.apple.network.statistics` (ntstat) — the
//! interface `nettop` uses. It hands out per-flow byte counts, RTT, and
//! retransmit stats for *every* process, no privileges required.
//!
//! Only the socket lifecycle lives here. The message protocol is private and
//! drifts across macOS releases, so all encoding/parsing is done on plain
//! byte slices in `collect::flows` where it can be unit-tested.

use std::io;

/// `struct ctl_info` (sys/kern_control.h).
#[repr(C)]
struct CtlInfo {
    ctl_id: u32,
    ctl_name: [u8; 96],
}

/// `struct sockaddr_ctl` (sys/kern_control.h).
#[repr(C)]
struct SockaddrCtl {
    sc_len: u8,
    sc_family: u8,
    ss_sysaddr: u16,
    sc_id: u32,
    sc_unit: u32,
    sc_reserved: [u32; 5],
}

const CONTROL_NAME: &[u8] = b"com.apple.network.statistics";
/// `CTLIOCGINFO` — _IOWR('N', 3, struct ctl_info).
const CTLIOCGINFO: libc::c_ulong = 0xC064_4E03;
const AF_SYS_CONTROL: u16 = 2;

/// A connected, non-blocking ntstat kernel-control socket.
pub struct NtstatSocket {
    fd: libc::c_int,
}

impl NtstatSocket {
    pub fn open() -> io::Result<Self> {
        let fd = unsafe { libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let sock = Self { fd }; // drops (closes) on any error below

        let mut info = CtlInfo {
            ctl_id: 0,
            ctl_name: [0; 96],
        };
        info.ctl_name[..CONTROL_NAME.len()].copy_from_slice(CONTROL_NAME);
        if unsafe { libc::ioctl(fd, CTLIOCGINFO, &raw mut info) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let addr = SockaddrCtl {
            sc_len: size_of::<SockaddrCtl>() as u8,
            sc_family: libc::AF_SYSTEM as u8,
            ss_sysaddr: AF_SYS_CONTROL,
            sc_id: info.ctl_id,
            sc_unit: 0,
            sc_reserved: [0; 5],
        };
        let rc = unsafe {
            libc::connect(
                fd,
                (&raw const addr).cast(),
                size_of::<SockaddrCtl>() as u32,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }

        // Big receive buffer: one GET_UPDATE poll can fan out hundreds of
        // per-flow messages faster than we drain them.
        let rcvbuf: libc::c_int = 256 * 1024;
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&raw const rcvbuf).cast(),
                size_of::<libc::c_int>() as u32,
            );
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Ok(sock)
    }

    /// Write one message; short writes don't happen on datagram controls.
    pub fn send(&self, msg: &[u8]) -> io::Result<()> {
        let n = unsafe { libc::send(self.fd, msg.as_ptr().cast(), msg.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Receive one kernel datagram; `Ok(None)` when the queue is drained.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n >= 0 {
            return Ok(Some(n as usize));
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(err)
        }
    }
}

impl Drop for NtstatSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
