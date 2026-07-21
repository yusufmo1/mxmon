//! Temperatures, fans, and system power.
//!
//! Die-level sensors come from IOHID (human-named: "pACC MTR Temp Sensor4",
//! "NAND CH0 temp", …). SMC supplies fans, total system power (`PSTR`), and
//! chassis sensors that HID doesn't expose.

use std::io;

use serde::{Deserialize, Serialize};

use crate::ffi::hid::HidTemps;
use crate::ffi::smc::{KeyInfo, Smc};
use crate::units::{Celsius, Watts};

/// A named temperature reading, grouped for display and the thermal map.
#[derive(Debug, Clone)]
pub struct Sensor {
    pub label: String,
    pub group: SensorGroup,
    pub temp: Celsius,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SensorGroup {
    CpuECore,
    CpuPCore,
    Gpu,
    Soc,
    Ane,
    Ssd,
    Battery,
    Airflow,
    Charger,
    Ports,
    Wireless,
    Other,
}

impl SensorGroup {
    pub fn title(self) -> &'static str {
        match self {
            Self::CpuECore => "E-Cores",
            Self::CpuPCore => "P-Cores",
            Self::Gpu => "GPU",
            Self::Soc => "SoC",
            Self::Ane => "ANE",
            Self::Ssd => "SSD",
            Self::Battery => "Battery",
            Self::Airflow => "Airflow",
            Self::Charger => "Power",
            Self::Ports => "Ports",
            Self::Wireless => "Wireless",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Fan {
    pub label: String,
    pub rpm: f32,
    pub max_rpm: f32,
}

#[derive(Debug, Clone, Default)]
pub struct TempSample {
    pub cpu_avg: Celsius,
    pub cpu_max: Celsius,
    pub gpu_avg: Celsius,
    pub gpu_max: Celsius,
    pub sensors: Vec<Sensor>,
    pub fans: Vec<Fan>,
    /// SMC `PSTR`: total system power draw.
    pub sys_power: Option<Watts>,
    /// SMC `PDTR`: watts actually delivered by the adapter right now
    /// (the registry's `AdapterDetails.Watts` is only the rated maximum).
    pub adapter_power: Option<Watts>,
}

/// Classify an IOHID sensor by its product name; `None` = not a display
/// sensor (e.g. calibration channels).
pub(crate) fn classify_hid(name: &str) -> Option<(SensorGroup, String)> {
    let n = name;
    let classified = if n.starts_with("eACC") {
        (SensorGroup::CpuECore, pretty_ordinal(n, "E-Core"))
    } else if n.starts_with("pACC") {
        (SensorGroup::CpuPCore, pretty_ordinal(n, "P-Core"))
    } else if n.starts_with("GPU") {
        (SensorGroup::Gpu, pretty_ordinal(n, "GPU"))
    } else if n.starts_with("SOC") {
        (SensorGroup::Soc, pretty_ordinal(n, "SoC"))
    } else if n.starts_with("ANE") {
        (SensorGroup::Ane, pretty_ordinal(n, "ANE"))
    } else if n.contains("tdie") {
        // "PMU tdie7" → authoritative per-region die temperature.
        (SensorGroup::Soc, pretty_ordinal(n, "Die"))
    } else if n.contains("tcal") {
        return None; // calibration reference, not a temperature
    } else if n.contains("tdev") {
        (SensorGroup::Soc, pretty_ordinal(n, "PMU"))
    } else if n.starts_with("ISP") || n.starts_with("DISP") {
        (SensorGroup::Soc, n.to_owned())
    } else if n.contains("NAND") || n.to_lowercase().contains("ssd") {
        (SensorGroup::Ssd, n.replace("temp", "").trim().to_owned())
    } else if n.to_lowercase().contains("battery") || n.starts_with("gas gauge") {
        (SensorGroup::Battery, "Battery".to_owned())
    } else {
        (SensorGroup::Other, n.to_owned())
    };
    Some(classified)
}

/// Curated per-core SMC temperature keys by chip generation (the families mix
/// real die sensors with calibration channels, so exact keys matter — same
/// curation the open-source Stats app ships). Unknown generations fall back
/// to dynamic family discovery labeled as "regions".
pub(crate) struct CoreKeys {
    pub(crate) ecores: &'static [&'static str],
    pub(crate) pcores: &'static [&'static str],
}

pub(crate) fn curated_core_keys(chip_name: &str) -> Option<CoreKeys> {
    if chip_name.contains("M1") {
        Some(CoreKeys {
            ecores: &["Tp09", "Tp0T"],
            pcores: &[
                "Tp01", "Tp05", "Tp0D", "Tp0H", "Tp0L", "Tp0P", "Tp0X", "Tp0b",
            ],
        })
    } else if chip_name.contains("M2") {
        Some(CoreKeys {
            ecores: &["Tp1h", "Tp1t", "Tp1p", "Tp1l"],
            pcores: &[
                "Tp01", "Tp05", "Tp09", "Tp0D", "Tp0X", "Tp0b", "Tp0f", "Tp0j",
            ],
        })
    } else if chip_name.contains("M3") {
        Some(CoreKeys {
            ecores: &["Te05", "Te0L", "Te0P", "Te0S"],
            pcores: &[
                "Tf04", "Tf09", "Tf0A", "Tf0B", "Tf0D", "Tf0E", // P-cluster 0
                "Tf44", "Tf49", "Tf4A", "Tf4B", "Tf4D", "Tf4E", // P-cluster 1
            ],
        })
    } else {
        None
    }
}

/// Fallback family classification for chips without a curated map.
fn classify_smc_family(key: &str) -> Option<SensorGroup> {
    match key.get(..2)? {
        "Te" => Some(SensorGroup::CpuECore),
        "Tf" | "Tp" => Some(SensorGroup::CpuPCore),
        "Tg" => Some(SensorGroup::Gpu),
        _ => None,
    }
}

/// Die-region temps only plausibly sit in this band (idle ambient ≈ 25°C).
fn plausible_die(temp: f32) -> bool {
    (15.0..=125.0).contains(&temp)
}

/// "pACC MTR Temp Sensor4" → "P-Core 4".
fn pretty_ordinal(name: &str, prefix: &str) -> String {
    let digits: String = name
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        prefix.to_owned()
    } else {
        let n: String = digits.chars().rev().collect();
        format!("{prefix} {n}")
    }
}

/// Curated SMC chassis keys (families that HID doesn't expose, with
/// human labels). Discovered dynamically — missing keys are skipped.
fn classify_smc(key: &str) -> Option<(SensorGroup, &'static str)> {
    Some(match key {
        "TaLP" => (SensorGroup::Airflow, "Airflow Left"),
        "TaRF" => (SensorGroup::Airflow, "Airflow Right"),
        "TB1T" | "TB2T" => (SensorGroup::Battery, "Battery"),
        "TCHP" => (SensorGroup::Charger, "Charger Proximity"),
        "TPSP" => (SensorGroup::Charger, "Power Supply"),
        "TW0P" => (SensorGroup::Wireless, "Wireless Proximity"),
        "TTLD" => (SensorGroup::Ports, "Thunderbolt Left"),
        "TTRD" => (SensorGroup::Ports, "Thunderbolt Right"),
        "Ts0P" => (SensorGroup::Other, "Palm Rest"),
        "Ts1P" => (SensorGroup::Other, "Trackpad"),
        _ => return None,
    })
}

fn plausible(temp: f32) -> bool {
    (1.0..=125.0).contains(&temp)
}

/// Sanity band for a group's readings (die sensors sit in a tighter band).
fn plausible_for(group: SensorGroup, temp: f32) -> bool {
    match group {
        SensorGroup::CpuECore | SensorGroup::CpuPCore | SensorGroup::Gpu => plausible_die(temp),
        _ => plausible(temp),
    }
}

/// A fan's current-RPM key plus its max RPM (a hardware constant, read once
/// at startup rather than over SMC IPC every sweep).
type FanKeys = (String, KeyInfo, f32);
/// A discovered sensor: SMC key, cached info, display group + label.
type SensorKey = (String, KeyInfo, SensorGroup, String);

/// Full SMC discovery: curated per-core keys, then a scan of every key on the
/// machine for chassis and family sensors. Expensive (one IOKit call per key)
/// — runs once per (chip, macOS, key-set) and is cached by [`save_sensor_cache`].
fn discover_sensors(smc: &Smc, chip_name: &str) -> Vec<SensorKey> {
    let mut smc_sensors = Vec::new();
    let curated = curated_core_keys(chip_name);
    // Try a curated key; returns the entry when it exists and reads sane.
    let probe = |key: &str, group: SensorGroup, label: String| {
        let info = smc.key_info(key).ok()?;
        smc.read_f32(key, info)
            .is_ok_and(plausible_die)
            .then(|| (key.to_owned(), info, group, label))
    };

    let mut have_cores = false;
    if let Some(ck) = &curated {
        let mut found = Vec::new();
        for (i, key) in ck.ecores.iter().enumerate() {
            found.extend(probe(
                key,
                SensorGroup::CpuECore,
                format!("E-Core {}", i + 1),
            ));
        }
        for (i, key) in ck.pcores.iter().enumerate() {
            found.extend(probe(
                key,
                SensorGroup::CpuPCore,
                format!("P-Core {}", i + 1),
            ));
        }
        // Trust the curated map only if it mostly matches this machine.
        if found.len() * 2 >= ck.ecores.len() + ck.pcores.len() {
            smc_sensors.extend(found);
            have_cores = true;
        }
    }

    if let Ok(mut keys) = smc.all_keys() {
        keys.sort();
        let mut family_counts: std::collections::HashMap<SensorGroup, u32> =
            std::collections::HashMap::new();
        for key in keys {
            // Curated chassis sensors (fixed labels).
            if let Some((group, label)) = classify_smc(&key) {
                let Ok(info) = smc.key_info(&key) else {
                    continue;
                };
                if smc.read_f32(&key, info).is_ok_and(plausible) {
                    smc_sensors.push((key, info, group, label.to_owned()));
                }
                continue;
            }
            match classify_smc_family(&key) {
                // GPU cluster sensors are discovered dynamically on
                // every generation (Tg count varies by GPU size).
                Some(SensorGroup::Gpu) => {
                    let Ok(info) = smc.key_info(&key) else {
                        continue;
                    };
                    if smc.read_f32(&key, info).is_ok_and(plausible_die) {
                        let n = family_counts.entry(SensorGroup::Gpu).or_insert(0);
                        *n += 1;
                        smc_sensors.push((key, info, SensorGroup::Gpu, format!("GPU Cluster {n}")));
                    }
                }
                // CPU families: only as a fallback for unknown chips,
                // honestly labeled as regions (not cores).
                Some(group) if !have_cores => {
                    let Ok(info) = smc.key_info(&key) else {
                        continue;
                    };
                    if smc.read_f32(&key, info).is_ok_and(plausible_die) {
                        let n = family_counts.entry(group).or_insert(0);
                        *n += 1;
                        let label = match group {
                            SensorGroup::CpuECore => format!("E Region {n}"),
                            _ => format!("P Region {n}"),
                        };
                        smc_sensors.push((key, info, group, label));
                    }
                }
                _ => {}
            }
        }
    }
    smc_sensors
}

/// On-disk record of one discovery pass: `~/.config/mxmon/sensors.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SensorCacheFile {
    chip: String,
    macos: String,
    /// Live `#KEY` count — firmware updates change the key set and this with it.
    key_count: u32,
    sensors: Vec<CachedSensor>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedSensor {
    key: String,
    group: SensorGroup,
    label: String,
}

fn sensor_cache_path() -> Option<std::path::PathBuf> {
    crate::config::dir().map(|d| d.join("sensors.toml"))
}

/// A cached discovery pass is only usable on the exact machine state that
/// produced it: same chip, same macOS build, same live `#KEY` count, and a
/// non-empty sensor list.
fn cache_fingerprint_matches(
    cached: &SensorCacheFile,
    chip: &str,
    macos: &str,
    key_count: u32,
) -> bool {
    cached.chip == chip
        && cached.macos == macos
        && cached.key_count == key_count
        && !cached.sensors.is_empty()
}

/// A mostly-dead cache is distrusted: fewer than 3/4 of its keys still
/// probing OK forces a fresh discovery scan.
fn cache_is_trustworthy(survived: usize, expected: usize) -> bool {
    survived * 4 >= expected * 3
}

/// Rebuild the sensor list from the cache when the machine fingerprint
/// matches. Every cached key is re-probed (2 cheap calls) so stale entries
/// drop out; if too few survive, the cache is distrusted and `None` forces a
/// fresh scan.
fn load_sensor_cache(smc: &Smc, chip: &str, macos: &str, key_count: u32) -> Option<Vec<SensorKey>> {
    let text = std::fs::read_to_string(sensor_cache_path()?).ok()?;
    let cached: SensorCacheFile = toml::from_str(&text).ok()?;
    if !cache_fingerprint_matches(&cached, chip, macos, key_count) {
        return None;
    }
    let expected = cached.sensors.len();
    let mut out = Vec::with_capacity(expected);
    for s in cached.sensors {
        let Ok(info) = smc.key_info(&s.key) else {
            continue;
        };
        if smc
            .read_f32(&s.key, info)
            .is_ok_and(|t| plausible_for(s.group, t))
        {
            out.push((s.key, info, s.group, s.label));
        }
    }
    cache_is_trustworthy(out.len(), expected).then_some(out)
}

/// Best-effort persist of a discovery pass (failed scans aren't cached).
fn save_sensor_cache(chip: &str, macos: &str, key_count: u32, sensors: &[SensorKey]) {
    if sensors.is_empty() {
        return;
    }
    let Some(path) = sensor_cache_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = SensorCacheFile {
        chip: chip.to_owned(),
        macos: macos.to_owned(),
        key_count,
        sensors: sensors
            .iter()
            .map(|(key, _, group, label)| CachedSensor {
                key: key.clone(),
                group: *group,
                label: label.clone(),
            })
            .collect(),
    };
    if let Ok(s) = toml::to_string(&file) {
        let _ = std::fs::write(path, s);
    }
}

pub struct TempCollector {
    hid: Option<HidTemps>,
    smc: Option<Smc>,
    smc_sensors: Vec<SensorKey>,
    fans: Vec<FanKeys>,
    pstr: Option<KeyInfo>,
    pdtr: Option<KeyInfo>,
    /// Last HID readings (die/PMU sensors). The HID sweep costs ~40 ms of
    /// IOKit IPC wall time vs ~10 ms for all of SMC, so it refreshes at a
    /// slower cadence and is merged from this cache in between.
    hid_cache: Vec<Sensor>,
}

impl TempCollector {
    pub fn new(chip_name: &str, macos_version: &str) -> io::Result<Self> {
        let mut hid = HidTemps::new().ok();
        if let Some(h) = &mut hid {
            // Non-display channels (e.g. "tcal" calibration references) would
            // otherwise cost one mach IPC per sweep just to be classified away.
            h.retain(|name| classify_hid(name).is_some());
        }
        crate::trace::mark("temps: hid client ready");
        let smc = Smc::open().ok();
        let mut smc_sensors = Vec::new();
        let mut fans = Vec::new();
        let mut pstr = None;
        let mut pdtr = None;

        if let Some(smc) = &smc {
            // Enumerating every SMC key costs ~500 ms of IOKit IPC on an
            // M3 Max, but the key SET is fixed per machine + firmware — so
            // discovery is cached on disk and later launches only re-probe
            // the cached keys (~2 cheap calls each).
            let key_count = smc.key_count().unwrap_or(0);
            smc_sensors = if let Some(sensors) =
                load_sensor_cache(smc, chip_name, macos_version, key_count)
            {
                crate::trace::mark("temps: sensor cache hit");
                sensors
            } else {
                let discovered = discover_sensors(smc, chip_name);
                crate::trace::mark("temps: smc scan done");
                save_sensor_cache(chip_name, macos_version, key_count, &discovered);
                discovered
            };
            for n in 0..4 {
                let ac = format!("F{n}Ac");
                let Ok(info) = smc.key_info(&ac) else { break };
                let mx = format!("F{n}Mx");
                let max_rpm = smc
                    .key_info(&mx)
                    .ok()
                    .and_then(|mi| smc.read_f32(&mx, mi).ok())
                    .unwrap_or(0.0);
                fans.push((ac, info, max_rpm));
            }
            pstr = smc.key_info("PSTR").ok();
            pdtr = smc.key_info("PDTR").ok();
        }

        if hid.is_none() && smc.is_none() {
            return Err(io::Error::other(
                "no temperature sources (HID and SMC failed)",
            ));
        }
        Ok(Self {
            hid,
            smc,
            smc_sensors,
            fans,
            pstr,
            pdtr,
            hid_cache: Vec::new(),
        })
    }

    /// One merged sample. `refresh_hid` re-reads the slow HID sensors;
    /// otherwise their cached values fill in (SMC is always fresh).
    pub fn sample(&mut self, refresh_hid: bool) -> TempSample {
        if (refresh_hid || self.hid_cache.is_empty())
            && let Some(hid) = &self.hid
        {
            self.hid_cache.clear();
            for (name, temp) in hid.read_all() {
                if !plausible(temp) {
                    continue;
                }
                let Some((group, label)) = classify_hid(name) else {
                    continue;
                };
                self.hid_cache.push(Sensor {
                    label,
                    group,
                    temp: Celsius(temp),
                });
            }
        }

        let mut out = TempSample::default();
        let (mut cpu_sum, mut cpu_n) = (0.0f32, 0u32);
        let (mut gpu_sum, mut gpu_n) = (0.0f32, 0u32);

        let (mut cpu_max, mut gpu_max) = (0.0f32, 0.0f32);
        // CPU/GPU averages use core/cluster sensors only (Mx-Power-Gadget
        // "CORE AVG" semantics); Die/PMU/chassis sensors are list-only.
        let mut tally = |group: SensorGroup, temp: f32| match group {
            SensorGroup::CpuECore | SensorGroup::CpuPCore => {
                cpu_sum += temp;
                cpu_n += 1;
                cpu_max = cpu_max.max(temp);
            }
            SensorGroup::Gpu => {
                gpu_sum += temp;
                gpu_n += 1;
                gpu_max = gpu_max.max(temp);
            }
            _ => {}
        };

        for s in &self.hid_cache {
            tally(s.group, s.temp.0);
            out.sensors.push(s.clone());
        }

        if let Some(smc) = &self.smc {
            for (key, info, group, label) in &self.smc_sensors {
                let Ok(temp) = smc.read_f32(key, *info) else {
                    continue;
                };
                if plausible_for(*group, temp) {
                    tally(*group, temp);
                    out.sensors.push(Sensor {
                        label: label.clone(),
                        group: *group,
                        temp: Celsius(temp),
                    });
                }
            }
            for (i, (ac, info, max_rpm)) in self.fans.iter().enumerate() {
                let rpm = smc.read_f32(ac, *info).unwrap_or(0.0);
                let max_rpm = *max_rpm;
                let label = match (i, self.fans.len()) {
                    (0, 2) => "Left".into(),
                    (1, 2) => "Right".into(),
                    (n, _) => format!("Fan {}", n + 1),
                };
                out.fans.push(Fan {
                    label,
                    rpm,
                    max_rpm,
                });
            }
            out.sys_power = self
                .pstr
                .and_then(|info| smc.read_f32("PSTR", info).ok())
                .map(Watts);
            out.adapter_power = self
                .pdtr
                .and_then(|info| smc.read_f32("PDTR", info).ok())
                .map(Watts);
        }

        // Deduplicate identical labels (e.g. two battery sensors) by averaging.
        dedup_labels(&mut out.sensors);
        // Cached keys: `natural_key` allocates, so once per element beats
        // twice per comparison.
        out.sensors
            .sort_by_cached_key(|s| (s.group, natural_key(&s.label)));

        if cpu_n > 0 {
            out.cpu_avg = Celsius(cpu_sum / cpu_n as f32);
            out.cpu_max = Celsius(cpu_max);
        }
        if gpu_n > 0 {
            out.gpu_avg = Celsius(gpu_sum / gpu_n as f32);
            out.gpu_max = Celsius(gpu_max);
        }
        out
    }
}

/// Sort key that orders "Die 2" before "Die 10" (trailing-number aware).
pub(crate) fn natural_key(label: &str) -> (String, u32) {
    let digits: String = label
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        (label.to_owned(), 0)
    } else {
        let n: u32 = digits
            .chars()
            .rev()
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        (label[..label.len() - digits.len()].trim_end().to_owned(), n)
    }
}

/// Merge sensors sharing a label into one averaged entry (stable order).
fn dedup_labels(sensors: &mut Vec<Sensor>) {
    let mut merged: Vec<Sensor> = Vec::with_capacity(sensors.len());
    for s in sensors.drain(..) {
        if let Some(existing) = merged.iter_mut().find(|m| m.label == s.label) {
            existing.temp = Celsius(f32::midpoint(existing.temp.0, s.temp.0));
        } else {
            merged.push(s);
        }
    }
    *sensors = merged;
}

#[cfg(test)]
mod tests {
    use super::{SensorGroup, classify_hid, curated_core_keys, natural_key};

    #[test]
    fn sensor_classification() {
        let (group, label) = classify_hid("pACC MTR Temp Sensor4").expect("classified");
        assert_eq!((group, label.as_str()), (SensorGroup::CpuPCore, "P-Core 4"));
        let (group, label) = classify_hid("PMU tdie7").expect("classified");
        assert_eq!((group, label.as_str()), (SensorGroup::Soc, "Die 7"));
        assert!(
            classify_hid("PMU tcal").is_none(),
            "calibration channels dropped"
        );
        let (group, _) = classify_hid("NAND CH0 temp").expect("classified");
        assert_eq!(group, SensorGroup::Ssd);
        let (group, _) = classify_hid("gas gauge battery").expect("classified");
        assert_eq!(group, SensorGroup::Battery);
    }

    #[test]
    fn m3_curated_keys_present() {
        let keys = curated_core_keys("Apple M3 Max").expect("M3 curated");
        assert_eq!(keys.ecores.len(), 4);
        assert_eq!(keys.pcores.len(), 12);
        assert!(
            curated_core_keys("Apple M9 Ultra").is_none(),
            "unknown chips fall back"
        );
    }

    #[test]
    fn natural_sort_key_orders_numerically() {
        assert!(natural_key("Die 2") < natural_key("Die 10"));
        assert!(natural_key("P-Core 9") < natural_key("P-Core 12"));
    }
}
