//! Power rails and frequencies from IOReport energy counters and
//! performance-state residencies.

use crate::collect::soc::SocInfo;
use crate::ffi::ioreport::{DeltaItem, IoReport};
use crate::units::{Mhz, Ratio, Watts};

/// A cluster's aggregate frequency/usage plus its per-core breakdown.
#[derive(Debug, Clone, Default)]
pub struct ClusterSample {
    pub freq: Mhz,
    pub usage: Ratio,
    /// `(freq, effective_usage)` per core, sorted by (cluster, core) id.
    pub cores: Vec<(Mhz, Ratio)>,
}

#[derive(Debug, Clone, Default)]
pub struct PowerSample {
    pub cpu: Watts,
    pub gpu: Watts,
    pub ane: Watts,
    pub dram: Watts,
    pub display: Watts,
    pub gpu_sram: Watts,
    pub ecpu: ClusterSample,
    pub pcpu: ClusterSample,
    pub gpu_freq: Mhz,
    /// Frequency-scaled GPU usage (from GPUPH residency).
    pub gpu_usage: Ratio,
    /// Fraction of the window the GPU was not powered off.
    pub gpu_active: Ratio,
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
    let offset = residencies
        .iter()
        .position(|(name, _)| !matches!(name.as_str(), "IDLE" | "DOWN" | "OFF"))
        .unwrap_or(0);
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
}

impl PowerCollector {
    pub fn new(soc: SocInfo) -> Result<Self, String> {
        Ok(Self {
            report: IoReport::subscribe(channel_filter)?,
            soc,
        })
    }

    /// Delta since the previous call; `None` on the first (baseline) call.
    pub fn sample(&mut self) -> Result<Option<PowerSample>, String> {
        let mut out = PowerSample::default();
        // (sort_key, freq, usage) per core, split by kind.
        let mut ecores: Vec<(u64, Mhz, Ratio)> = Vec::new();
        let mut pcores: Vec<(u64, Mhz, Ratio)> = Vec::new();
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
                    } else if name.starts_with("GPU SRAM") {
                        out.gpu_sram.0 += watts.0;
                    }
                }
                "CPU Stats" => {
                    if let Some((kind, die, ord)) = parse_core_channel(name) {
                        let table = match kind {
                            CoreKind::Efficiency => &soc.ecpu_freqs,
                            CoreKind::Performance => &soc.pcpu_freqs,
                        };
                        let (freq, usage, _) = freq_from_residency(&item.residencies(), table);
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

        out.ecpu = aggregate(ecores);
        out.pcpu = aggregate(pcores);
        Ok(Some(out))
    }
}

/// Sort cores by (die, ordinal) and average their freq/usage into the cluster.
fn aggregate(mut cores: Vec<(u64, Mhz, Ratio)>) -> ClusterSample {
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
        cores: cores.into_iter().map(|(_, f, u)| (f, u)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{freq_from_residency, parse_core_channel};
    use crate::units::Mhz;

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
}
