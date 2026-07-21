//! Disk I/O from `IOBlockStorageDriver` statistics: throughput, IOPS, and
//! real per-op device latency, aggregated across every attached drive. The
//! registry keeps cumulative byte/op/busy-time counters per driver, so one
//! small property copy per device per tick is the entire cost.

use std::io;
use std::time::Instant;

use crate::collect::net::counter_delta;
use crate::ffi::cf::{CfOwned, cfstr, dict_get};
use crate::ffi::iokit::{IoObject, cf_number_i64, services};
use crate::units::Bytes;

/// Re-enumerate drivers this often (~30 s at defaults) so newly attached
/// devices show up; detached ones are caught by their reads failing.
const REENUM_TICKS: u64 = 120;

/// Cumulative counters summed across the current device set.
#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    bytes_r: u64,
    bytes_w: u64,
    ops_r: u64,
    ops_w: u64,
    time_r_ns: u64,
    time_w_ns: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DiskSample {
    pub read_per_sec: Bytes,
    pub write_per_sec: Bytes,
    pub read_iops: u32,
    pub write_iops: u32,
    /// Average device time per op over the most recent window that had ops
    /// (an idle window keeps the last measurement rather than flickering to
    /// nothing); `None` until the first op is ever seen.
    pub read_lat_us: Option<f32>,
    pub write_lat_us: Option<f32>,
    pub read_session: Bytes,
    pub write_session: Bytes,
    pub devices: usize,
}

/// Busy-time over an op-count window → average µs per operation.
pub(crate) fn avg_latency_us(delta_time_ns: u64, delta_ops: u64) -> Option<f32> {
    (delta_ops > 0).then(|| (delta_time_ns as f64 / delta_ops as f64 / 1000.0) as f32)
}

pub struct DiskCollector {
    drivers: Vec<IoObject>,
    stats_key: CfOwned,
    k_br: CfOwned,
    k_bw: CfOwned,
    k_or: CfOwned,
    k_ow: CfOwned,
    k_tr: CfOwned,
    k_tw: CfOwned,
    prev: Option<(Instant, Totals)>,
    session_r: u64,
    session_w: u64,
    ema_r: f64,
    ema_w: f64,
    ema_ior: f64,
    ema_iow: f64,
    last_lat_r: Option<f32>,
    last_lat_w: Option<f32>,
    reenumerate: bool,
    ticks: u64,
}

impl DiskCollector {
    pub fn new() -> io::Result<Self> {
        let drivers = services("IOBlockStorageDriver")?;
        if drivers.is_empty() {
            return Err(io::Error::other("no IOBlockStorageDriver services"));
        }
        Ok(Self {
            drivers,
            stats_key: cfstr("Statistics"),
            k_br: cfstr("Bytes (Read)"),
            k_bw: cfstr("Bytes (Write)"),
            k_or: cfstr("Operations (Read)"),
            k_ow: cfstr("Operations (Write)"),
            k_tr: cfstr("Total Time (Read)"),
            k_tw: cfstr("Total Time (Write)"),
            prev: None,
            session_r: 0,
            session_w: 0,
            ema_r: 0.0,
            ema_w: 0.0,
            ema_ior: 0.0,
            ema_iow: 0.0,
            last_lat_r: None,
            last_lat_w: None,
            reenumerate: false,
            ticks: 0,
        })
    }

    /// Sum the statistics dictionaries of every driver; `None` when any read
    /// fails (a device detached between enumeration and now).
    fn read_totals(&self) -> Option<Totals> {
        let mut t = Totals::default();
        for d in &self.drivers {
            let stats = d.property(&self.stats_key)?;
            let dict = stats.as_dict();
            let get = |key: &CfOwned| {
                dict_get(dict, key)
                    .and_then(cf_number_i64)
                    .map(|v| v as u64)
            };
            t.bytes_r += get(&self.k_br)?;
            t.bytes_w += get(&self.k_bw)?;
            t.ops_r += get(&self.k_or)?;
            t.ops_w += get(&self.k_ow)?;
            t.time_r_ns += get(&self.k_tr)?;
            t.time_w_ns += get(&self.k_tw)?;
        }
        Some(t)
    }

    pub fn sample(&mut self) -> io::Result<DiskSample> {
        self.ticks += 1;
        if self.reenumerate || self.ticks.is_multiple_of(REENUM_TICKS) {
            let drivers = services("IOBlockStorageDriver")?;
            // A different device set makes the aggregate counters
            // non-comparable — restart the delta baseline.
            if drivers.len() != self.drivers.len() {
                self.prev = None;
            }
            self.drivers = drivers;
            self.reenumerate = false;
            if self.drivers.is_empty() {
                return Err(io::Error::other("no IOBlockStorageDriver services"));
            }
        }

        let mut out = DiskSample {
            devices: self.drivers.len(),
            read_session: Bytes(self.session_r),
            write_session: Bytes(self.session_w),
            ..Default::default()
        };
        let Some(totals) = self.read_totals() else {
            // Mid-read detach: report one quiet tick and resync.
            self.reenumerate = true;
            self.prev = None;
            return Ok(out);
        };

        let now = Instant::now();
        if let Some((prev_at, prev)) = self.prev {
            let dt = now.duration_since(prev_at).as_secs_f64().max(0.001);
            let d_br = counter_delta(totals.bytes_r, prev.bytes_r);
            let d_bw = counter_delta(totals.bytes_w, prev.bytes_w);
            let d_or = counter_delta(totals.ops_r, prev.ops_r);
            let d_ow = counter_delta(totals.ops_w, prev.ops_w);
            let d_tr = counter_delta(totals.time_r_ns, prev.time_r_ns);
            let d_tw = counter_delta(totals.time_w_ns, prev.time_w_ns);
            self.session_r += d_br;
            self.session_w += d_bw;
            // Same light smoothing as the net collector; IOPS too, or the
            // number flickers 0↔burst at fast-tick granularity.
            let alpha = 0.6;
            let ema = |acc: &mut f64, v: f64| *acc = alpha * v + (1.0 - alpha) * *acc;
            ema(&mut self.ema_r, d_br as f64 / dt);
            ema(&mut self.ema_w, d_bw as f64 / dt);
            ema(&mut self.ema_ior, d_or as f64 / dt);
            ema(&mut self.ema_iow, d_ow as f64 / dt);
            out.read_per_sec = Bytes(self.ema_r as u64);
            out.write_per_sec = Bytes(self.ema_w as u64);
            out.read_iops = self.ema_ior.round() as u32;
            out.write_iops = self.ema_iow.round() as u32;
            if let Some(l) = avg_latency_us(d_tr, d_or) {
                self.last_lat_r = Some(l);
            }
            if let Some(l) = avg_latency_us(d_tw, d_ow) {
                self.last_lat_w = Some(l);
            }
            out.read_lat_us = self.last_lat_r;
            out.write_lat_us = self.last_lat_w;
            out.read_session = Bytes(self.session_r);
            out.write_session = Bytes(self.session_w);
        }
        self.prev = Some((now, totals));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::avg_latency_us;

    #[test]
    fn disk_latency_derivation() {
        // 2 ms of device time across 4 ops = 500 µs each.
        assert_eq!(avg_latency_us(2_000_000, 4), Some(500.0));
        // An idle window has no latency, not zero latency.
        assert_eq!(avg_latency_us(0, 0), None);
    }
}
