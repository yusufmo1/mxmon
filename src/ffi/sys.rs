//! Safe wrappers over miscellaneous libc calls (sysctl, pwd, time). Keeps
//! the rest of the crate free of `unsafe` per the crate lint policy.

use std::ffi::CString;

/// Load averages (1/5/15 min).
pub fn load_avg() -> [f64; 3] {
    let mut avg = [0.0f64; 3];
    unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    avg
}

/// Seconds since boot (via `kern.boottime`).
pub fn uptime_secs() -> u64 {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = size_of::<libc::timeval>();
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut tv).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return 0;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    now.saturating_sub(tv.tv_sec as u64)
}

/// Swap (used, total) bytes via `vm.swapusage`.
pub fn swap_usage() -> (u64, u64) {
    let mut xsw = libc::xsw_usage {
        xsu_total: 0,
        xsu_avail: 0,
        xsu_used: 0,
        xsu_pagesize: 0,
        xsu_encrypted: 0,
    };
    let mut len = size_of::<libc::xsw_usage>();
    let mut mib = [libc::CTL_VM, libc::VM_SWAPUSAGE];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut xsw).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        (0, 0)
    } else {
        (xsw.xsu_used, xsw.xsu_total)
    }
}

/// Read a string sysctl by name.
pub fn sysctl_string(name: &str) -> Option<String> {
    let cname = CString::new(name).ok()?;
    let mut len: usize = 0;
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len.saturating_sub(1)); // trailing NUL
    String::from_utf8(buf).ok()
}

/// Read an integer sysctl by name (handles 32- and 64-bit values).
pub fn sysctl_u64(name: &str) -> Option<u64> {
    let cname = CString::new(name).ok()?;
    let mut value: u64 = 0;
    let mut len = size_of::<u64>();
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            (&raw mut value).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    // Some sysctls are 32-bit; the kernel writes only `len` bytes (LE).
    if len == 4 {
        value &= 0xffff_ffff;
    }
    Some(value)
}

/// Resolve a uid to its login name.
pub fn username(uid: u32) -> Option<String> {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Effective uid of this process.
pub fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

/// Local wall-clock (hour, minute, second) for a unix timestamp.
pub fn local_hms(now: u64) -> (i32, i32, i32) {
    let mut tm = libc::tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null_mut(),
    };
    let t = now as libc::time_t;
    unsafe { libc::localtime_r(&raw const t, &raw mut tm) };
    (tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// One mounted file system's capacity, from `getfsstat`.
#[derive(Debug, Clone)]
pub struct MountUsage {
    pub mount_point: String,
    pub fs_type: String,
    pub total: u64,
    pub available: u64,
}

/// Capacity of every mounted file system.
///
/// `MNT_NOWAIT` reads the kernel's cached figures instead of asking each file
/// system to update — the whole sweep costs a few microseconds, which is what
/// makes it affordable to poll at all.
pub fn mounts() -> Vec<MountUsage> {
    const MNT_NOWAIT: i32 = 2;
    // Ask for the count first, then size the buffer to it; a mount appearing
    // between the two calls is simply not reported this pass.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(count as usize);
    let bytes = std::mem::size_of::<libc::statfs>() as libc::c_int * count;
    let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, MNT_NOWAIT) };
    if got <= 0 {
        return Vec::new();
    }
    // Never trust the second count to be ≤ the capacity we allocated for.
    let got = (got as usize).min(count as usize);
    unsafe { buf.set_len(got) };

    buf.iter()
        .map(|fs| {
            let cstr = |p: &[libc::c_char]| {
                let end = p.iter().position(|&c| c == 0).unwrap_or(p.len());
                let raw: Vec<u8> = p[..end].iter().map(|&c| c as u8).collect();
                String::from_utf8_lossy(&raw).into_owned()
            };
            let block = u64::from(fs.f_bsize);
            MountUsage {
                mount_point: cstr(&fs.f_mntonname),
                fs_type: cstr(&fs.f_fstypename),
                total: fs.f_blocks.saturating_mul(block),
                // `bavail` is what a non-root process may actually use, which
                // is what a capacity bar should show — `bfree` includes the
                // reserve only root can touch.
                available: fs.f_bavail.saturating_mul(block),
            }
        })
        .collect()
}
