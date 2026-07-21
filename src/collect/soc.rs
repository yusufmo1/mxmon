//! Static SoC information gathered once at startup: chip identity, core
//! topology, DVFS frequency tables, GPU core count.

use std::io;

use crate::ffi::cf::{cfstr, data_bytes, dict_get};
use crate::ffi::iokit::{cf_number_i64, services};
use crate::units::Mhz;

/// Immutable machine facts.
#[derive(Debug, Clone, Default)]
pub struct SocInfo {
    pub chip_name: String,
    pub macos_version: String,
    pub ecpu_count: usize,
    pub pcpu_count: usize,
    /// Cores per P-cluster (6 on M3 Max → two clusters).
    pub cores_per_pcluster: usize,
    pub gpu_core_count: Option<u32>,
    pub memory_bytes: u64,
    /// DVFS tables in MHz, ascending.
    pub ecpu_freqs: Vec<Mhz>,
    pub pcpu_freqs: Vec<Mhz>,
    pub gpu_freqs: Vec<Mhz>,
}

impl SocInfo {
    pub fn total_cores(&self) -> usize {
        self.ecpu_count + self.pcpu_count
    }
}

use crate::ffi::sys::{sysctl_string, sysctl_u64};

/// Parse a pmgr `voltage-states*` CFData blob: 8-byte LE (freq, voltage)
/// pairs. Frequencies are Hz on M1–M3, kHz on M4+ — detected by magnitude.
pub(crate) fn parse_dvfs(bytes: &[u8]) -> Vec<Mhz> {
    let mut freqs: Vec<u64> = bytes
        .chunks_exact(8)
        .map(|c| u64::from(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
        .filter(|&f| f > 0)
        .collect();
    freqs.sort_unstable();
    let scale = match freqs.last() {
        // > 100 MHz when read as Hz ⇒ the table is in Hz; else kHz.
        Some(&max) if max > 100_000_000 => 1_000_000,
        _ => 1_000,
    };
    freqs.into_iter().map(|f| Mhz((f / scale) as u32)).collect()
}

/// Read the three DVFS tables from the `pmgr` registry entry.
fn dvfs_tables() -> io::Result<(Vec<Mhz>, Vec<Mhz>, Vec<Mhz>)> {
    for dev in services("AppleARMIODevice")? {
        if dev.name() != "pmgr" {
            continue;
        }
        let props = dev.properties()?;
        let read = |key| {
            dict_get(props.as_dict(), &key)
                .map(|ptr| parse_dvfs(&data_bytes(ptr.cast())))
                .unwrap_or_default()
        };
        return Ok((
            read(cfstr("voltage-states1-sram")),
            read(cfstr("voltage-states5-sram")),
            read(cfstr("voltage-states9")),
        ));
    }
    Err(io::Error::other("pmgr device not found"))
}

/// GPU core count from the AGXAccelerator registry entry (instant, unlike
/// `system_profiler`).
fn gpu_core_count() -> Option<u32> {
    let key = cfstr("gpu-core-count");
    for dev in services("AGXAccelerator").ok()? {
        if let Ok(props) = dev.properties()
            && let Some(v) = dict_get(props.as_dict(), &key).and_then(cf_number_i64)
        {
            return u32::try_from(v).ok();
        }
    }
    None
}

/// Gather all static SoC facts (fast: sysctls + two registry lookups).
pub fn load() -> io::Result<SocInfo> {
    let (ecpu_freqs, pcpu_freqs, gpu_freqs) = dvfs_tables()?;
    Ok(SocInfo {
        chip_name: sysctl_string("machdep.cpu.brand_string")
            .unwrap_or_else(|| "Apple Silicon".into()),
        macos_version: sysctl_string("kern.osproductversion").unwrap_or_default(),
        // perflevel0 = "Performance", perflevel1 = "Efficiency" — but logical
        // core indices start with the E-cluster (verified via IODeviceTree).
        ecpu_count: sysctl_u64("hw.perflevel1.logicalcpu").unwrap_or(0) as usize,
        pcpu_count: sysctl_u64("hw.perflevel0.logicalcpu").unwrap_or(0) as usize,
        cores_per_pcluster: sysctl_u64("hw.perflevel0.cpusperl2").unwrap_or(0) as usize,
        gpu_core_count: gpu_core_count(),
        memory_bytes: sysctl_u64("hw.memsize").unwrap_or(0),
        ecpu_freqs,
        pcpu_freqs,
        gpu_freqs,
    })
}
