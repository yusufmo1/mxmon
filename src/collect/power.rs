//! Power rails and frequencies from IOReport energy counters and
//! performance-state residencies.

use crate::collect::soc::SocInfo;
use crate::ffi::ioreport::{DeltaItem, IoReport};
use crate::units::{Mhz, Ratio, Watts};

/// One core's readings.
#[derive(Debug, Clone, Copy, Default)]
pub struct CoreSample {
    pub freq: Mhz,
    pub usage: Ratio,
    /// The core's own energy rail, when the Energy Model publishes one for it
    /// (`EACC_CPU3`, `PACC1_CPU0`, …). `None` rather than zero, so a chip that
    /// simply doesn't report per-core power never reads as "idle".
    pub watts: Option<Watts>,
}

/// A cluster's aggregate frequency/usage plus its per-core breakdown.
#[derive(Debug, Clone, Default)]
pub struct ClusterSample {
    pub freq: Mhz,
    pub usage: Ratio,
    /// Per core, sorted by (die, cluster, core) id.
    pub cores: Vec<CoreSample>,
}

#[derive(Debug, Clone, Default)]
pub struct PowerSample {
    pub cpu: Watts,
    pub gpu: Watts,
    pub ane: Watts,
    pub dram: Watts,
    /// Both display pipelines summed; [`Self::display_ext`] is the external
    /// share of it.
    pub display: Watts,
    pub gpu_sram: Watts,
    pub ecpu: ClusterSample,
    pub pcpu: ClusterSample,
    pub gpu_freq: Mhz,
    /// Frequency-scaled GPU usage (from GPUPH residency).
    pub gpu_usage: Ratio,
    /// Fraction of the window the GPU was not powered off.
    pub gpu_active: Ratio,
    /// Memory-controller fabric (`AMCC*`) — a separate rail from the DRAM one,
    /// not a component of it.
    pub amcc: Watts,
    /// DRAM command scheduler / PHY (`DCS*`).
    pub dcs: Watts,
    /// Video encode/decode engine (`AVE*`) — non-zero while media is playing
    /// or transcoding.
    pub video: Watts,
    /// Camera image-signal processor (`ISP*`) — the internal camera's rail.
    pub isp: Watts,
    /// Media scaler (`MSR*`).
    pub scaler: Watts,
    /// GPU command/scheduler rails (`GPU CS*`).
    pub gpu_cs: Watts,
    /// External display pipeline (`DISPEXT*`), the external share of
    /// [`Self::display`].
    pub display_ext: Watts,
}

impl PowerSample {
    /// Package power: compute + neural rails (matches macmon's `all_power`).
    pub fn package(&self) -> Watts {
        Watts(self.cpu.0 + self.gpu.0 + self.ane.0)
    }
}

/// Keep only the channels we consume; fewer channels = cheaper samples.
fn channel_filter(group: &str, subgroup: &str, name: &str) -> bool {
    match group {
        "Energy Model" => {
            name == "GPU Energy"
                || name.ends_with("CPU Energy")
                || name.starts_with("ANE")
                || name.starts_with("DRAM")
                || name.starts_with("DISP")
                || name.starts_with("GPU SRAM")
                // Per-core rails, so a busy core can be told from a busy
                // cluster. The `_SRAM` siblings are deliberately left
                // unsubscribed: folding them into a core would invent an
                // attribution, and reporting them separately has no home yet.
                || parse_energy_core(name).is_some()
                // Blocks the group has always published and mxmon has never
                // read. Prefix-matched because multi-die parts index them.
                || BLOCK_PREFIXES.iter().any(|p| name.starts_with(p))
        }
        "CPU Stats" => subgroup == "CPU Core Performance States",
        "GPU Stats" => subgroup == "GPU Performance States",
        _ => false,
    }
}

/// `(cluster_kind, die, cluster, core)` parsed from a per-core channel name
/// like `ECPU030`, `PCPU120`, or `DIE_1_PCPU040` (Ultra).
pub(crate) fn parse_core_channel(name: &str) -> Option<(CoreKind, u32, u64)> {
    let kind = if name.contains("ECPU") || name.contains("MCPU") {
        CoreKind::Efficiency
    } else if name.contains("PCPU") {
        CoreKind::Performance
    } else {
        return None;
    };
    let die = name
        .strip_prefix("DIE_")
        .and_then(|rest| rest.split('_').next())
        .and_then(|d| d.parse::<u32>().ok())
        .unwrap_or(0);
    let digits: String = name
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let ord = digits.parse::<u64>().ok()?;
    Some((kind, die, ord))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreKind {
    Efficiency,
    Performance,
}

/// SoC blocks the `Energy Model` group publishes that mxmon never read. Prefix
/// matched: multi-die parts suffix an index (`AMCC0`, `AMCC1`).
const BLOCK_PREFIXES: [&str; 6] = ["AMCC", "DCS", "AVE", "ISP", "MSR", "GPU CS"];

/// `(kind, die, ord)` for a per-core **energy** channel, keyed to match
/// [`parse_core_channel`] so watts join to frequency by identity, never by
/// position in either list.
///
/// The two families spell the same core differently, verified against both
/// channel lists on an M3 Max:
///
/// | core | CPU Stats | Energy Model |
/// |------|-----------|--------------|
/// | E cluster 0, core 2 | `ECPU020` | `EACC_CPU2` |
/// | P cluster 1, core 3 | `PCPU130` | `PACC1_CPU3` |
///
/// CPU Stats writes `<cluster><core>0`, so the shared ordinal is
/// `cluster * 100 + core * 10`. Cluster totals (`EACC_CPU`, `PACC0_CPM`) and
/// the `_SRAM` rails are not cores and return `None`.
pub(crate) fn parse_energy_core(name: &str) -> Option<(CoreKind, u32, u64)> {
    let die = name
        .strip_prefix("DIE_")
        .and_then(|rest| rest.split('_').next())
        .and_then(|d| d.parse::<u32>().ok())
        .unwrap_or(0);
    // `EACC_CPU3`, or the tail of `DIE_1_PACC0_CPU3`.
    let rest = name.rsplit("DIE_").next().unwrap_or(name);
    let (acc, core) = rest.split_once("_CPU")?;
    let acc = acc.rsplit('_').next()?;
    // `_SRAM` rails and the `_CPM` cluster totals are not per-core.
    if core.is_empty() || !core.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let kind = match acc.as_bytes().first()? {
        // `MACC` mirrors CPU Stats' `MCPU` mid tier on M5.
        b'E' | b'M' => CoreKind::Efficiency,
        b'P' => CoreKind::Performance,
        _ => return None,
    };
    // Index rather than slice: channel names are hostile input like any other
    // wire format, and a short one must not panic.
    let cluster_digits = acc.get(1..).and_then(|tail| tail.strip_prefix("ACC"))?;
    let cluster: u64 = if cluster_digits.is_empty() {
        0
    } else {
        cluster_digits.parse().ok()?
    };
    let core: u64 = core.parse().ok()?;
    Some((kind, die, cluster * 100 + core * 10))
}

/// A core whose whole window sat in powered-down states: some DOWN/OFF
/// residency and zero everywhere else. Genuinely idle cores still accrue
/// IDLE ticks and an all-zero window matches nothing, so neither is dropped.
/// M5 Max publishes an entire phantom cluster of such cores (`MCPU0*`),
/// which would otherwise render as ghost 0 MHz rows.
pub(crate) fn parked(residencies: &[(String, i64)]) -> bool {
    let (mut down, mut alive) = (0i64, 0i64);
    for (name, r) in residencies {
        match name.as_str() {
            "DOWN" | "OFF" => down = down.saturating_add(*r),
            _ => alive = alive.saturating_add(*r),
        }
    }
    down > 0 && alive == 0
}

/// Energy counter → watts using the channel's unit label and real elapsed time.
fn to_watts(item: &DeltaItem<'_>, dt_ms: u64) -> Watts {
    let per_second = item.integer_value() as f32 / (dt_ms as f32 / 1000.0);
    let scale = match item.id.unit.as_str() {
        "mJ" => 1e3,
        "uJ" => 1e6,
        "nJ" => 1e9,
        _ => return Watts(0.0),
    };
    Watts(per_second / scale)
}

/// Residency buckets + DVFS table → (weighted MHz, effective usage, active ratio).
pub(crate) fn freq_from_residency(
    residencies: &[(String, i64)],
    freqs: &[Mhz],
) -> (Mhz, Ratio, Ratio) {
    if freqs.is_empty() || residencies.len() <= freqs.len() {
        return Default::default();
    }
    // Leading buckets are idle-ish states (IDLE / DOWN / OFF); data starts after.
    // Clamped so a malformed shape (more leading idle buckets than spare
    // slots) degrades to skewed numbers instead of indexing out of bounds.
    let offset = residencies
        .iter()
        .position(|(name, _)| !matches!(name.as_str(), "IDLE" | "DOWN" | "OFF"))
        .unwrap_or(0)
        .min(residencies.len() - freqs.len());
    let active: f64 = residencies[offset..].iter().map(|&(_, r)| r as f64).sum();
    let total: f64 = residencies.iter().map(|&(_, r)| r as f64).sum();
    if active <= 0.0 || total <= 0.0 {
        return Default::default();
    }
    let mut avg_freq = 0.0f64;
    for (i, freq) in freqs.iter().enumerate() {
        let share = residencies[i + offset].1 as f64 / active;
        avg_freq += share * f64::from(freq.0);
    }
    let active_ratio = active / total;
    let max_freq = f64::from(freqs.last().expect("freqs non-empty").0);
    let min_freq = f64::from(freqs[0].0);
    let effective = (avg_freq.max(min_freq) * active_ratio) / max_freq;
    (
        Mhz(avg_freq as u32),
        Ratio(effective as f32),
        Ratio(active_ratio as f32),
    )
}

pub struct PowerCollector {
    report: IoReport,
    soc: SocInfo,
    /// Unserved channels are a startup fact, reported on the first delta only.
    reported_unserved: bool,
}

impl PowerCollector {
    pub fn new(soc: SocInfo) -> Result<Self, String> {
        Ok(Self {
            report: IoReport::subscribe(channel_filter)?,
            soc,
            reported_unserved: false,
        })
    }

    /// Delta since the previous call; `None` on the first (baseline) call.
    pub fn sample(&mut self) -> Result<Option<PowerSample>, String> {
        let mut out = PowerSample::default();
        // (sort_key, freq, usage) per core, split by kind.
        let mut ecores: Vec<(u64, Mhz, Ratio)> = Vec::new();
        let mut pcores: Vec<(u64, Mhz, Ratio)> = Vec::new();
        // (sort_key, watts) from the Energy Model, joined to the above by key.
        let mut ewatts: Vec<(u64, Watts)> = Vec::new();
        let mut pwatts: Vec<(u64, Watts)> = Vec::new();
        // Disjoint field borrows: the closure reads `soc` while `report`
        // is sampled mutably — no per-tick clone of the DVFS tables.
        let soc = &self.soc;
        let window = self.report.visit_delta(|dt_ms, item| {
            let name = item.id.name.as_str();
            match item.id.group.as_str() {
                "Energy Model" => {
                    let watts = to_watts(&item, dt_ms);
                    if name == "GPU Energy" {
                        out.gpu = watts;
                    } else if name.ends_with("CPU Energy") {
                        // "CPU Energy", or "DIE_n_CPU Energy" on Ultra: sum dies.
                        out.cpu.0 += watts.0;
                    } else if name.starts_with("ANE") {
                        out.ane.0 += watts.0;
                    } else if name.starts_with("DRAM") {
                        out.dram.0 += watts.0;
                    } else if name.starts_with("DISP") {
                        out.display.0 += watts.0;
                        if name.starts_with("DISPEXT") {
                            out.display_ext.0 += watts.0;
                        }
                    } else if name.starts_with("GPU SRAM") {
                        out.gpu_sram.0 += watts.0;
                    } else if let Some((kind, die, ord)) = parse_energy_core(name) {
                        let key = (u64::from(die) << 32) | ord;
                        match kind {
                            CoreKind::Efficiency => ewatts.push((key, watts)),
                            CoreKind::Performance => pwatts.push((key, watts)),
                        }
                    } else if name.starts_with("AMCC") {
                        out.amcc.0 += watts.0;
                    } else if name.starts_with("DCS") {
                        out.dcs.0 += watts.0;
                    } else if name.starts_with("AVE") {
                        out.video.0 += watts.0;
                    } else if name.starts_with("ISP") {
                        out.isp.0 += watts.0;
                    } else if name.starts_with("MSR") {
                        out.scaler.0 += watts.0;
                    } else if name.starts_with("GPU CS") {
                        out.gpu_cs.0 += watts.0;
                    }
                }
                "CPU Stats" => {
                    if let Some((kind, die, ord)) = parse_core_channel(name) {
                        let residencies = item.residencies();
                        // Parked phantom cores (all-DOWN cluster on M5 Max)
                        // never reach the panel.
                        if parked(&residencies) {
                            return;
                        }
                        let table = match kind {
                            CoreKind::Efficiency => &soc.ecpu_freqs,
                            CoreKind::Performance => &soc.pcpu_freqs,
                        };
                        let (freq, usage, _) = freq_from_residency(&residencies, table);
                        let key = (u64::from(die) << 32) | ord;
                        match kind {
                            CoreKind::Efficiency => ecores.push((key, freq, usage)),
                            CoreKind::Performance => pcores.push((key, freq, usage)),
                        }
                    }
                }
                "GPU Stats" if name == "GPUPH" && soc.gpu_freqs.len() > 1 => {
                    let (freq, usage, active) =
                        freq_from_residency(&item.residencies(), &soc.gpu_freqs[1..]);
                    out.gpu_freq = freq;
                    out.gpu_usage = usage;
                    out.gpu_active = active;
                }
                _ => {}
            }
        })?;
        if window.is_none() {
            return Ok(None);
        }
        // Subscribing is a request, not a promise — IOReport serves what the
        // process is allowed to read and silently omits the rest. Say so once,
        // on the first real delta, rather than leaving a short list to look
        // like a complete one.
        if !self.reported_unserved {
            self.reported_unserved = true;
            let unserved = self.report.unserved();
            if !unserved.is_empty() && crate::trace::enabled() {
                crate::trace::mark(&format!("power: {} channels unserved", unserved.len()));
                for channel in &unserved {
                    crate::trace::mark(&format!("power:   unserved {channel}"));
                }
            }
        }

        out.ecpu = aggregate(ecores, &ewatts);
        out.pcpu = aggregate(pcores, &pwatts);
        Ok(Some(out))
    }
}

/// Sort cores by (die, ordinal) and average their freq/usage into the cluster.
///
/// `watts` carries the per-core energy rails keyed the same way; cores are
/// matched by that key, so a chip that publishes power for only some cores (or
/// none) still lines every reading up with the right core.
fn aggregate(mut cores: Vec<(u64, Mhz, Ratio)>, watts: &[(u64, Watts)]) -> ClusterSample {
    if cores.is_empty() {
        return ClusterSample::default();
    }
    cores.sort_unstable_by_key(|&(key, ..)| key);
    let n = cores.len() as f32;
    let freq = Mhz(
        (cores.iter().map(|&(_, f, _)| u64::from(f.0)).sum::<u64>() / cores.len() as u64) as u32,
    );
    let usage = Ratio(cores.iter().map(|&(.., u)| u.0).sum::<f32>() / n);
    ClusterSample {
        freq,
        usage,
        cores: cores
            .into_iter()
            .map(|(key, freq, usage)| CoreSample {
                freq,
                usage,
                watts: watts.iter().find(|&&(k, _)| k == key).map(|&(_, w)| w),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CoreKind, freq_from_residency, parse_core_channel, parse_energy_core};
    use crate::units::Mhz;

    /// The whole point of `parse_energy_core`: an energy channel and the CPU
    /// Stats channel for the same core must produce the same key, or per-core
    /// watts land on the wrong core. Both name lists were captured from an
    /// M3 Max (4 E-cores, two 6-core P clusters).
    #[test]
    fn energy_and_stats_channels_agree_on_every_core() {
        let pairs = [
            ("ECPU000", "EACC_CPU0"),
            ("ECPU010", "EACC_CPU1"),
            ("ECPU020", "EACC_CPU2"),
            ("ECPU030", "EACC_CPU3"),
            ("PCPU000", "PACC0_CPU0"),
            ("PCPU050", "PACC0_CPU5"),
            ("PCPU100", "PACC1_CPU0"),
            ("PCPU130", "PACC1_CPU3"),
            ("PCPU150", "PACC1_CPU5"),
        ];
        for (stats, energy) in pairs {
            assert_eq!(
                parse_core_channel(stats),
                parse_energy_core(energy),
                "{stats} vs {energy}"
            );
        }
    }

    #[test]
    fn energy_core_reads_kind_and_die() {
        assert_eq!(
            parse_energy_core("EACC_CPU1"),
            Some((CoreKind::Efficiency, 0, 10))
        );
        assert_eq!(
            parse_energy_core("PACC1_CPU2"),
            Some((CoreKind::Performance, 0, 120))
        );
        // M5's mid tier mirrors CPU Stats' `MCPU`, which parses as efficiency.
        assert_eq!(
            parse_energy_core("MACC_CPU0"),
            Some((CoreKind::Efficiency, 0, 0))
        );
        // Ultra parts prefix the die.
        assert_eq!(
            parse_energy_core("DIE_1_PACC0_CPU3"),
            Some((CoreKind::Performance, 1, 30))
        );
    }

    #[test]
    fn energy_core_rejects_everything_that_is_not_a_core() {
        // Cluster totals and their SRAM siblings share the prefix but are not
        // cores; counting them would double a cluster's power.
        for name in [
            "EACC_CPU",
            "PACC0_CPU",
            "EACC_CPM",
            "PACC1_CPM",
            "EACC_CPU0_SRAM",
            "PACC1_CPU5_SRAM",
            "EACC_CPM_SRAM",
            "AMCC0",
            "DCS0",
            "GPU Energy",
            "DRAM0",
            "",
            "_CPU0",
            "XACC_CPU0",
            "E_CPU0",
        ] {
            assert_eq!(parse_energy_core(name), None, "{name}");
        }
    }

    #[test]
    fn freq_from_residency_weighted_mean() {
        let freqs = [Mhz(1000), Mhz(2000), Mhz(3000)];
        // One leading idle bucket (as on M3: IDLE/DOWN), then residencies.
        let residencies = vec![
            ("IDLE".to_owned(), 100),
            ("P1".to_owned(), 0),
            ("P2".to_owned(), 100),
            ("P3".to_owned(), 100),
        ];
        let (freq, usage, active) = freq_from_residency(&residencies, &freqs);
        assert_eq!(freq, Mhz(2500)); // (2000+3000)/2 weighted
        assert!((active.0 - 2.0 / 3.0).abs() < 1e-4);
        // effective = max(2500,1000)*active / 3000
        assert!((usage.0 - (2500.0 * (2.0 / 3.0)) / 3000.0).abs() < 1e-4);
    }

    #[test]
    fn freq_from_residency_rejects_bad_shapes() {
        let freqs = [Mhz(1000)];
        // Residency array must be longer than the table.
        let (f, u, a) = freq_from_residency(&[("IDLE".into(), 5)], &freqs);
        assert_eq!((f, u.0, a.0), (Mhz(0), 0.0, 0.0));
    }

    #[test]
    fn parked_cores_detected_without_dropping_idle_ones() {
        use super::parked;
        let r = |pairs: &[(&str, i64)]| -> Vec<(String, i64)> {
            pairs.iter().map(|&(n, v)| (n.to_owned(), v)).collect()
        };
        // M5 Max phantom cluster: the entire window in DOWN.
        assert!(parked(&r(&[("IDLE", 0), ("DOWN", 900), ("P1", 0)])));
        assert!(parked(&r(&[("OFF", 42)])));
        // A real idle core accrues IDLE — kept.
        assert!(!parked(&r(&[("IDLE", 900), ("DOWN", 100), ("P1", 0)])));
        // Active cores and degenerate all-zero windows are kept too.
        assert!(!parked(&r(&[("IDLE", 10), ("DOWN", 0), ("P1", 50)])));
        assert!(!parked(&r(&[("IDLE", 0), ("DOWN", 0), ("P1", 0)])));
        assert!(!parked(&[]));
    }

    #[test]
    fn core_channel_parsing() {
        let (kind, die, ord) = parse_core_channel("ECPU030").expect("parses");
        assert_eq!(
            (format!("{kind:?}"), die, ord),
            ("Efficiency".into(), 0, 30)
        );
        let (kind, die, ord) = parse_core_channel("DIE_1_PCPU040").expect("parses");
        assert_eq!(
            (format!("{kind:?}"), die, ord),
            ("Performance".into(), 1, 40)
        );
        // M5 rename: MCPU is an efficiency-tier channel.
        assert!(parse_core_channel("MCPU010").is_some());
        assert!(parse_core_channel("GPUPH").is_none());
    }

    #[test]
    fn freq_from_residency_survives_excess_idle_buckets() {
        // Two leading idle buckets but only one spare slot: the offset clamp
        // must keep every index inside the residency array.
        let freqs = [Mhz(1000), Mhz(2000)];
        let residencies = vec![
            ("IDLE".to_owned(), 10),
            ("DOWN".to_owned(), 10),
            ("P1".to_owned(), 10),
        ];
        let (_, _, active) = freq_from_residency(&residencies, &freqs);
        assert!(active.0 > 0.0);
    }

    mod prop {
        use super::super::{freq_from_residency, parse_energy_core};
        use crate::units::Mhz;
        use proptest::prelude::*;

        proptest! {
            // Channel names are wire data like any other: the parser slices
            // and indexes, so it must be total for arbitrary input, not only
            // for the shapes Apple happens to ship today.
            #[test]
            fn parse_energy_core_never_panics(s in ".*") {
                let _ = parse_energy_core(&s);
            }

            // Fuzz around the real grammar, where the edges actually live.
            #[test]
            fn parse_energy_core_never_panics_near_the_grammar(
                s in "(DIE_[0-9]{0,3}_)?[EPMX]?ACC[0-9]{0,3}(_CPU[0-9]{0,3})?(_SRAM)?"
            ) {
                let _ = parse_energy_core(&s);
            }
        }

        proptest! {
            // Kernel residency shapes drift across macOS releases — any
            // shape must degrade gracefully, never panic, and keep the
            // derived ratios in display range.
            #[test]
            fn freq_from_residency_total(
                names in proptest::collection::vec(
                    proptest::sample::select(vec!["IDLE", "DOWN", "OFF", "P1", "V2"]),
                    0..8,
                ),
                counts in proptest::collection::vec(0i64..1_000_000, 0..8),
                freqs in proptest::collection::vec(1u32..5000, 0..6),
            ) {
                let residencies: Vec<(String, i64)> = names
                    .iter()
                    .zip(&counts)
                    .map(|(n, &c)| ((*n).to_owned(), c))
                    .collect();
                // DVFS tables arrive ascending (parse_dvfs sorts) — that is
                // the input contract the ratio bounds rely on.
                let mut freqs = freqs;
                freqs.sort_unstable();
                let freqs: Vec<Mhz> = freqs.into_iter().map(Mhz).collect();
                let (f, u, a) = freq_from_residency(&residencies, &freqs);
                prop_assert!((0.0..=1.0 + 1e-6).contains(&a.0));
                prop_assert!((0.0..=1.0 + 1e-6).contains(&u.0));
                prop_assert!(f.0 < 10_000);
            }
        }
    }
}
