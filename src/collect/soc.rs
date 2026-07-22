//! Static SoC information gathered once at startup: chip identity, core
//! topology, DVFS frequency tables, GPU core count.

use std::io;

use crate::ffi::cf::{cfstr, cfstr_copy, data_bytes, dict_get};
use crate::ffi::iokit::{cf_number_i64, services};
use crate::units::Mhz;

/// Immutable machine facts.
#[derive(Debug, Clone)]
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
    /// Display letter for the lower CPU tier: 'E' on M1–M4; 'P' on M5
    /// Pro/Max, where the mid tier (MCPU in IOReport) fills the ecpu slot.
    pub tier_low: char,
    /// Display letter for the higher CPU tier: 'P' on M1–M4, 'S' (Super)
    /// on M5 Pro/Max.
    pub tier_high: char,
}

// Manual impl: the derived default would set NUL tier chars, and tests build
// `SocInfo::default()` directly for soc-less rendering paths.
impl Default for SocInfo {
    fn default() -> Self {
        Self {
            chip_name: String::new(),
            macos_version: String::new(),
            ecpu_count: 0,
            pcpu_count: 0,
            cores_per_pcluster: 0,
            gpu_core_count: None,
            memory_bytes: 0,
            ecpu_freqs: Vec::new(),
            pcpu_freqs: Vec::new(),
            gpu_freqs: Vec::new(),
            tier_low: 'E',
            tier_high: 'P',
        }
    }
}

/// Apple Silicon generation parsed from the brand string ("Apple M3 Max" →
/// 3). Digit-bounded, so an eventual "M14" can never read as generation 1
/// the way substring matching would.
pub(crate) fn generation(chip_name: &str) -> Option<u32> {
    let mut chars = chip_name.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == 'M'
            && let Some(&(_, d)) = chars.peek()
            && d.is_ascii_digit()
        {
            let digits: String = chip_name[i + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            return digits.parse().ok();
        }
    }
    None
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

/// Parse the pmgr `acc-clusters` table (published on M5+): 8-byte entries of
/// (voltage-states index, cluster type, …padding), mapping each CPU cluster
/// to its DVFS table. Returns the `(low, high)` tier key names. The highest
/// type is the top tier; the low tier is the *second*-highest, because the
/// type-0 slot can be a coreless placeholder (field-verified on M5 Max,
/// where the live tiers are types 1 and 2).
pub(crate) fn parse_acc_clusters(bytes: &[u8]) -> Option<(String, String)> {
    // (type, voltage-states index), sorted so ties break deterministically.
    let mut clusters: Vec<(u8, u8)> = bytes.chunks_exact(8).map(|c| (c[1], c[0])).collect();
    clusters.sort_unstable();
    if clusters.len() < 2 {
        return None;
    }
    let key = |idx: u8| format!("voltage-states{idx}-sram");
    Some((
        key(clusters[clusters.len() - 2].1),
        key(clusters[clusters.len() - 1].1),
    ))
}

/// Read the three DVFS tables from the `pmgr` registry entry. A machine with
/// no such entry — virtualized macOS, where the whole IODeviceTree power
/// domain is absent — yields empty tables instead of an error: the tables
/// only map residency indices to MHz, so losing them costs the frequency
/// readouts (`freq_from_residency` returns 0 MHz against an empty table),
/// not the process. Everything else mxmon reads is still there.
fn dvfs_tables() -> io::Result<(Vec<Mhz>, Vec<Mhz>, Vec<Mhz>)> {
    for dev in services("AppleARMIODevice")? {
        if dev.name() != "pmgr" {
            continue;
        }
        let props = dev.properties()?;
        // `cfstr_copy`: the M5+ fallback below reads runtime-built key names.
        let read = |key: &str| {
            dict_get(props.as_dict(), &cfstr_copy(key))
                .map(|ptr| parse_dvfs(&data_bytes(ptr.cast())))
                .unwrap_or_default()
        };
        let mut ecpu = read("voltage-states1-sram");
        let mut pcpu = read("voltage-states5-sram");
        let gpu = read("voltage-states9");
        // M5+ moved the CPU tables: the tier→table map lives in
        // `acc-clusters` and the fixed keys can come back empty (on M5 Max
        // the low tier resolves to voltage-states23-sram).
        if (ecpu.is_empty() || pcpu.is_empty())
            && let Some(acc) = dict_get(props.as_dict(), &cfstr("acc-clusters"))
            && let Some((low_key, high_key)) = parse_acc_clusters(&data_bytes(acc.cast()))
        {
            if ecpu.is_empty() {
                ecpu = read(&low_key);
            }
            if pcpu.is_empty() {
                pcpu = read(&high_key);
            }
        }
        return Ok((ecpu, pcpu, gpu));
    }
    Ok((Vec::new(), Vec::new(), Vec::new()))
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

/// Choose the (high, low) CPU tiers from the perflevel table, as
/// `((cores, letter), (cores, letter))`. perflevel0 is always the top tier;
/// the low tier is the *last* level that actually has cores — M5 Pro/Max
/// publish Super + Performance with an empty Efficiency level, base M5 and
/// M1–M4 end on a populated Efficiency level. Letters come from the level
/// names (Efficiency/Performance/Super → E/P/S).
pub(crate) fn pick_tiers(levels: &[(u64, String)]) -> Option<((usize, char), (usize, char))> {
    let letter = |name: &str, fallback: char| {
        name.chars()
            .next()
            .map_or(fallback, |c| c.to_ascii_uppercase())
    };
    let (high_n, high_name) = levels.first()?;
    let high = (*high_n as usize, letter(high_name, 'P'));
    let low = levels
        .iter()
        .skip(1)
        .rev()
        .find(|&&(n, _)| n > 0)
        .map_or((0, 'E'), |(n, name)| (*n as usize, letter(name, 'E')));
    Some((high, low))
}

/// Enumerate `hw.perflevel{i}.*` into [`pick_tiers`] input. `None` when the
/// perflevel sysctls are unavailable (callers fall back to the fixed 0/1
/// reads that every two-tier chip satisfies).
fn cpu_tiers() -> Option<((usize, char), (usize, char))> {
    let n = sysctl_u64("hw.nperflevels")?.min(8);
    let levels: Vec<(u64, String)> = (0..n)
        .map(|i| {
            (
                sysctl_u64(&format!("hw.perflevel{i}.logicalcpu")).unwrap_or(0),
                sysctl_string(&format!("hw.perflevel{i}.name")).unwrap_or_default(),
            )
        })
        .collect();
    pick_tiers(&levels)
}

/// Gather all static SoC facts (fast: sysctls + two registry lookups).
pub fn load() -> io::Result<SocInfo> {
    let (ecpu_freqs, pcpu_freqs, gpu_freqs) = dvfs_tables()?;
    // Tiers by name and population, not fixed indices: M5 Pro/Max have three
    // perflevels (Super / Performance / empty Efficiency). Logical core
    // indices still start with the low tier's cluster (verified via
    // IODeviceTree on two-tier chips; IOReport keys per-core data anyway).
    let ((pcpu_count, tier_high), (ecpu_count, tier_low)) = cpu_tiers().unwrap_or_else(|| {
        (
            (
                sysctl_u64("hw.perflevel0.logicalcpu").unwrap_or(0) as usize,
                'P',
            ),
            (
                sysctl_u64("hw.perflevel1.logicalcpu").unwrap_or(0) as usize,
                'E',
            ),
        )
    });
    Ok(SocInfo {
        chip_name: sysctl_string("machdep.cpu.brand_string")
            .unwrap_or_else(|| "Apple Silicon".into()),
        macos_version: sysctl_string("kern.osproductversion").unwrap_or_default(),
        ecpu_count,
        pcpu_count,
        cores_per_pcluster: sysctl_u64("hw.perflevel0.cpusperl2").unwrap_or(0) as usize,
        gpu_core_count: gpu_core_count(),
        memory_bytes: sysctl_u64("hw.memsize").unwrap_or(0),
        ecpu_freqs,
        pcpu_freqs,
        gpu_freqs,
        tier_low,
        tier_high,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_dvfs;
    use crate::units::Mhz;

    fn blob(pairs: &[(u32, u32)]) -> Vec<u8> {
        let mut v = Vec::new();
        for &(freq, volt) in pairs {
            v.extend_from_slice(&freq.to_le_bytes());
            v.extend_from_slice(&volt.to_le_bytes());
        }
        v
    }

    #[test]
    fn dvfs_hz_scale_m1_to_m3() {
        // M1–M3 publish Hz; zero rows are placeholder slots; output is sorted.
        let b = blob(&[(2_064_000_000, 5), (0, 0), (600_000_000, 3)]);
        assert_eq!(parse_dvfs(&b), vec![Mhz(600), Mhz(2064)]);
    }

    #[test]
    fn dvfs_khz_scale_m4_plus() {
        // M4+ publish kHz — detected by magnitude, not chip name.
        let b = blob(&[(4_512_000, 7), (1_080_000, 2)]);
        assert_eq!(parse_dvfs(&b), vec![Mhz(1080), Mhz(4512)]);
    }

    #[test]
    fn dvfs_tolerates_empty_and_truncated_blobs() {
        assert!(parse_dvfs(&[]).is_empty());
        let mut b = blob(&[(600_000_000, 1)]);
        b.extend_from_slice(&[1, 2, 3]); // partial trailing pair is ignored
        assert_eq!(parse_dvfs(&b), vec![Mhz(600)]);
    }

    use super::{generation, parse_acc_clusters, pick_tiers};

    #[test]
    fn generation_is_digit_bounded() {
        assert_eq!(generation("Apple M1"), Some(1));
        assert_eq!(generation("Apple M3 Max"), Some(3));
        assert_eq!(generation("Apple M5 Pro"), Some(5));
        // Substring matching would call this generation 1.
        assert_eq!(generation("Apple M14 Ultra"), Some(14));
        assert_eq!(generation("Apple Silicon"), None);
        assert_eq!(generation(""), None);
    }

    #[test]
    fn tiers_two_level_matches_legacy_reads() {
        // M3 Max shape: perflevel0 = Performance, perflevel1 = Efficiency.
        let levels = [(12, "Performance".to_owned()), (4, "Efficiency".to_owned())];
        let ((p, hi), (e, lo)) = pick_tiers(&levels).expect("two tiers");
        assert_eq!((p, hi, e, lo), (12, 'P', 4, 'E'));
    }

    #[test]
    fn tiers_m5_max_skips_empty_efficiency_level() {
        // M5 Max: 6 Super + 12 Performance + a coreless Efficiency level.
        let levels = [
            (6, "Super".to_owned()),
            (12, "Performance".to_owned()),
            (0, "Efficiency".to_owned()),
        ];
        let ((p, hi), (e, lo)) = pick_tiers(&levels).expect("tiers");
        assert_eq!((p, hi, e, lo), (6, 'S', 12, 'P'));
    }

    #[test]
    fn tiers_degenerate_tables() {
        assert!(pick_tiers(&[]).is_none());
        // A single populated level keeps the low tier empty, like the old
        // fixed perflevel1 read on a machine without one.
        let ((p, hi), (e, lo)) = pick_tiers(&[(8, "Performance".to_owned())]).expect("one tier");
        assert_eq!((p, hi, e, lo), (8, 'P', 0, 'E'));
        // Missing names fall back to positional letters.
        let ((_, hi), (_, lo)) =
            pick_tiers(&[(4, String::new()), (4, String::new())]).expect("tiers");
        assert_eq!((hi, lo), ('P', 'E'));
    }

    #[test]
    fn acc_clusters_m5_max_capture() {
        // Real acc-clusters bytes from an M5 Max (via macmon's ioreg
        // capture): type 0 is a coreless placeholder; the live tiers are
        // type 1 (index 23) and type 2 (index 5).
        #[rustfmt::skip]
        let data = [
            0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x05, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let (low, high) = parse_acc_clusters(&data).expect("two clusters");
        assert_eq!(low, "voltage-states23-sram");
        assert_eq!(high, "voltage-states5-sram");
    }

    #[test]
    fn acc_clusters_rejects_short_tables() {
        assert!(parse_acc_clusters(&[]).is_none());
        assert!(parse_acc_clusters(&[1, 0, 0, 0, 0, 0, 0, 0]).is_none());
        // A truncated trailing entry is ignored, not misread.
        let mut data = vec![3, 0, 0, 0, 0, 0, 0, 0, 5, 1, 0, 0, 0, 0, 0, 0];
        data.extend_from_slice(&[9, 9, 9]);
        let (low, high) = parse_acc_clusters(&data).expect("two whole entries");
        assert_eq!(
            (low.as_str(), high.as_str()),
            ("voltage-states3-sram", "voltage-states5-sram",)
        );
    }

    mod prop {
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn dvfs_total_and_sorted(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
                let freqs = super::super::parse_dvfs(&bytes);
                prop_assert!(freqs.windows(2).all(|w| w[0] <= w[1]));
            }

            // Any acc-clusters blob must parse totally: no panics, and when
            // it parses, both keys are well-formed voltage-states names.
            #[test]
            fn acc_clusters_total(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
                if let Some((low, high)) = super::super::parse_acc_clusters(&bytes) {
                    prop_assert!(low.starts_with("voltage-states"));
                    prop_assert!(high.starts_with("voltage-states"));
                }
            }
        }
    }
}
