//! Storage health: NVMe SMART, APFS per-volume cache behaviour, and the
//! controller-side throttle and flash-traffic counters.
//!
//! Everything here answers a question the DISK panel's throughput cannot:
//! whether the drive is wearing out, whether the file cache is absorbing the
//! reads, and whether the controller is thermally capping itself. All of it is
//! slow-moving, so it rides a tier of its own rather than the fast one.

use crate::ffi::cf::{CfOwned, cfstr, dict_get};
use crate::ffi::iokit::{cf_number_i64, service_iter};
use crate::ffi::ioreport::IoReport;
use crate::ffi::nvme::Smart;
use crate::units::{Bytes, Ratio};

/// One volume's cache and write behaviour, from its APFS `Statistics` table.
#[derive(Debug, Clone, Default)]
pub struct VolumeStats {
    pub name: String,
    /// Bytes userspace asked for vs bytes that reached the device. The gap is
    /// the unified buffer cache doing its job.
    pub user_read: Bytes,
    pub device_read: Bytes,
    pub user_write: Bytes,
    pub device_write: Bytes,
}

impl VolumeStats {
    /// Share of reads served without touching the device. `None` when nothing
    /// has been read — an idle volume has no hit rate, and rendering one as 0%
    /// would read as a catastrophically bad cache.
    pub fn cache_hit(&self) -> Option<Ratio> {
        (self.user_read.0 > 0).then(|| {
            let served = self.user_read.0.saturating_sub(self.device_read.0);
            Ratio(served as f32 / self.user_read.0 as f32).clamped()
        })
    }

    /// Device bytes written per byte userspace wrote. Above 1 means the file
    /// system is writing more than it was asked to (metadata, alignment).
    pub fn write_amplification(&self) -> Option<f32> {
        (self.user_write.0 > 0).then(|| self.device_write.0 as f32 / self.user_write.0 as f32)
    }
}

/// Controller-side counters that only IOReport publishes.
#[derive(Debug, Clone, Default)]
pub struct ControllerStats {
    /// Share of the window the drive spent in any thermal-throttle tier.
    pub throttled: Option<Ratio>,
    /// Bytes the controller moved to flash over the window — larger than the
    /// host wrote, which is the write amplification the host never sees.
    pub nand_written: Bytes,
}

#[derive(Debug, Clone, Default)]
pub struct StorageSample {
    pub smart: Option<Smart>,
    pub volumes: Vec<VolumeStats>,
    pub controller: ControllerStats,
}

/// Keep only the controller channels we read.
fn channel_filter(group: &str, subgroup: &str, _name: &str) -> bool {
    match group {
        // `BW Limits` is deliberately absent: its channels carry the current
        // cap as an absolute level, and IOReport only hands us deltas, so
        // differencing it yields 0 rather than "unrestricted".
        "NVMe" => subgroup == "Time weighted throttle statistics",
        "ANS2" => subgroup == "BW Monitor",
        _ => false,
    }
}

pub struct StorageCollector {
    /// `None` when the controller groups could not be subscribed; SMART and
    /// APFS still work, so this is a partial degrade, not a failure.
    report: Option<IoReport>,
    keys: Keys,
}

struct Keys {
    statistics: CfOwned,
    user_read: CfOwned,
    device_read: CfOwned,
    user_write: CfOwned,
    device_write: CfOwned,
    volume_name: CfOwned,
}

impl StorageCollector {
    pub fn new() -> Self {
        Self {
            report: IoReport::subscribe(channel_filter).ok(),
            keys: Keys {
                statistics: cfstr("Statistics"),
                user_read: cfstr("Bytes read by user"),
                device_read: cfstr("Bytes read from block device"),
                user_write: cfstr("Bytes written by user"),
                device_write: cfstr("Bytes written to block device"),
                volume_name: cfstr("APFS Volume Name"),
            },
        }
    }

    /// One health pass. Individually fallible: a volume that refuses to be
    /// read drops out of the list, and a missing SMART interface leaves
    /// `smart` as `None`, without affecting the rest.
    pub fn sample(&mut self) -> StorageSample {
        StorageSample {
            smart: crate::ffi::nvme::read(),
            volumes: self.volumes(),
            controller: self.controller(),
        }
    }

    fn volumes(&self) -> Vec<VolumeStats> {
        let Ok(entries) = service_iter("AppleAPFSVolume") else {
            return Vec::new();
        };
        let k = &self.keys;
        entries
            .iter()
            .filter_map(|(obj, entry_name)| {
                // Copy the whole property table once and read keys out of it,
                // rather than asking for each key individually: it is one IPC
                // round trip instead of several, and it is the same path the
                // battery collector has always used.
                let props = obj.properties().ok()?;
                let table = crate::ffi::cf::as_dict(props.as_ptr().cast())?;
                let stats = crate::ffi::cf::as_dict(dict_get(table, &k.statistics)?)?;
                let num = |key: &CfOwned| {
                    dict_get(stats, key)
                        .and_then(cf_number_i64)
                        .map_or(0, |v| v.max(0) as u64)
                };
                let name = dict_get(table, &k.volume_name)
                    .and_then(crate::ffi::cf::as_string)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| entry_name.clone());
                let v = VolumeStats {
                    name,
                    user_read: Bytes(num(&k.user_read)),
                    device_read: Bytes(num(&k.device_read)),
                    user_write: Bytes(num(&k.user_write)),
                    device_write: Bytes(num(&k.device_write)),
                };
                // Volumes that have never been touched carry no information.
                (v.user_read.0 > 0 || v.user_write.0 > 0).then_some(v)
            })
            .collect()
    }

    fn controller(&mut self) -> ControllerStats {
        let mut out = ControllerStats::default();
        let Some(report) = self.report.as_mut() else {
            return out;
        };
        let mut throttle_ticks: i64 = 0;
        let mut elapsed_ticks: i64 = 0;
        let mut nand: u64 = 0;
        let visited = report.visit_delta(|_dt, item| {
            let name = item.id.name.as_str();
            match item.id.group.as_str() {
                "NVMe" if item.id.subgroup == "Time weighted throttle statistics" => {
                    if name == "Total time elapsed" {
                        elapsed_ticks = elapsed_ticks.saturating_add(item.integer_value());
                    } else if name.contains("Throttle Time") {
                        throttle_ticks = throttle_ticks.saturating_add(item.integer_value());
                    }
                }
                "ANS2" if name.contains("Write") => {
                    nand = nand.saturating_add(item.integer_value().max(0) as u64);
                }
                _ => {}
            }
        });
        if !matches!(visited, Ok(Some(_))) {
            return out;
        }
        out.throttled = throttled_share(throttle_ticks, elapsed_ticks);
        out.nand_written = Bytes(nand);
        out
    }
}

/// Throttle time as a share of the window it was measured over.
///
/// `None` when the window is empty: a drive that reported no elapsed time was
/// not "0% throttled", it simply was not measured, and the two must not render
/// the same.
fn throttled_share(throttle_ticks: i64, elapsed_ticks: i64) -> Option<Ratio> {
    (elapsed_ticks > 0).then(|| Ratio(throttle_ticks as f32 / elapsed_ticks as f32).clamped())
}

#[cfg(test)]
mod tests {
    use super::{VolumeStats, throttled_share};
    use crate::units::Bytes;

    #[test]
    fn throttle_share_is_a_fraction_of_the_measured_window() {
        let share = throttled_share(250, 1000).expect("window measured");
        assert!((share.as_percent() - 25.0).abs() < 0.01);
        // Counters can skew across a window; the ratio stays displayable.
        assert!(throttled_share(5000, 1000).unwrap().as_percent() <= 100.0);
    }

    #[test]
    fn an_unmeasured_window_is_absent_not_zero_percent() {
        assert!(throttled_share(0, 0).is_none());
        assert!(throttled_share(10, -5).is_none());
    }

    fn vol(ur: u64, dr: u64, uw: u64, dw: u64) -> VolumeStats {
        VolumeStats {
            name: "Data".into(),
            user_read: Bytes(ur),
            device_read: Bytes(dr),
            user_write: Bytes(uw),
            device_write: Bytes(dw),
        }
    }

    #[test]
    fn cache_hit_is_the_share_never_reaching_the_device() {
        // The surveyed volume: 1.00 GB served from 14.8 MB of device reads.
        let hit = vol(1_001_565_691, 14_807_040, 0, 0)
            .cache_hit()
            .expect("reads happened");
        assert!((hit.as_percent() - 98.5).abs() < 0.1, "{hit:?}");
    }

    #[test]
    fn an_idle_volume_has_no_hit_rate_rather_than_zero() {
        // 0% would read as a catastrophically bad cache; the truth is that
        // nothing has been read at all.
        assert!(vol(0, 0, 0, 0).cache_hit().is_none());
        assert!(vol(0, 0, 5, 5).cache_hit().is_none());
    }

    #[test]
    fn a_device_reading_more_than_the_user_asked_cannot_go_negative() {
        // Read-ahead can pull more from the device than userspace requested.
        let hit = vol(100, 400, 0, 0).cache_hit().expect("reads happened");
        assert!(hit.0.abs() < f32::EPSILON);
    }

    #[test]
    fn write_amplification_compares_device_to_user_bytes() {
        let amp = vol(0, 0, 583_133, 1_179_648)
            .write_amplification()
            .expect("writes happened");
        assert!((amp - 2.0).abs() < 0.05, "{amp}");
        assert!(vol(0, 0, 0, 4096).write_amplification().is_none());
    }
}
