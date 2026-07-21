//! Network throughput: per-interface byte-counter deltas plus since-boot
//! totals, and the machine's primary interface details.

use std::collections::HashMap;
use std::io;
use std::time::Instant;

use crate::ffi::net::{interface_counters, ipv4_addresses};
use crate::units::Bytes;

#[derive(Debug, Clone, Default)]
pub struct NetSample {
    /// Aggregate over all physical interfaces.
    pub rx_per_sec: Bytes,
    pub tx_per_sec: Bytes,
    /// Accumulated since mxmon launched ("session" totals). Raw since-boot
    /// counters are unusable here: macOS quantizes (64 KiB) and 32-bit-wraps
    /// NET_RT_IFLIST2 byte counters for ad-hoc-signed binaries (verified
    /// empirically on macOS 26.5 — Apple-signed tools see real values).
    pub rx_session: Bytes,
    pub tx_session: Bytes,
    /// Busiest active interface (name, link speed bit/s, local IPv4).
    pub primary: Option<PrimaryIf>,
}

#[derive(Debug, Clone, Default)]
pub struct PrimaryIf {
    pub name: String,
    pub baudrate: u64,
    pub ipv4: Option<String>,
    pub mac: Option<String>,
    /// Link actually up (`IFF_RUNNING`), not merely configured.
    pub running: bool,
}

/// Per-interface `(rx_bytes, tx_bytes)` snapshot keyed by name.
type CounterMap = HashMap<String, (u64, u64)>;

pub struct NetCollector {
    prev: Option<(Instant, CounterMap)>,
    session_rx: u64,
    session_tx: u64,
    rate_rx_ema: f64,
    rate_tx_ema: f64,
}

/// Counter delta that survives the 32-bit wrap imposed on ad-hoc-signed
/// binaries (real 64-bit counters take the plain-subtraction path).
pub(crate) fn counter_delta(curr: u64, prev: u64) -> u64 {
    if curr >= prev {
        curr - prev
    } else if prev < (1 << 32) {
        curr + (1 << 32) - prev
    } else {
        0 // counter reset (interface bounced)
    }
}

impl NetCollector {
    pub fn new() -> Self {
        Self {
            prev: None,
            session_rx: 0,
            session_tx: 0,
            rate_rx_ema: 0.0,
            rate_tx_ema: 0.0,
        }
    }

    pub fn sample(&mut self) -> io::Result<NetSample> {
        let now = Instant::now();
        let counters = interface_counters()?;
        let mut out = NetSample::default();

        // Physical interfaces only: tunnels (utun) re-count traffic that also
        // crosses the underlying link, and awdl/llw/bridge/ap are chatter.
        let physical = |name: &str| {
            name.starts_with("en") && name.len() <= 5 // en0..en99
        };

        let mut current = CounterMap::new();
        for c in counters.iter().filter(|c| !c.loopback && physical(&c.name)) {
            current.insert(c.name.clone(), (c.rx_bytes, c.tx_bytes));
        }

        // Rates per interface; the busiest *right now* becomes primary.
        let mut busiest: Option<(u64, String)> = None;
        if let Some((prev_at, prev)) = &self.prev {
            let dt = now.duration_since(*prev_at).as_secs_f64().max(0.001);
            let (mut rx_d, mut tx_d) = (0u64, 0u64);
            for (name, &(rx, tx)) in &current {
                if let Some(&(prx, ptx)) = prev.get(name) {
                    let (r, t) = (counter_delta(rx, prx), counter_delta(tx, ptx));
                    rx_d += r;
                    tx_d += t;
                    if busiest.as_ref().is_none_or(|(b, _)| r + t > *b) {
                        busiest = Some((r + t, name.clone()));
                    }
                }
            }
            self.session_rx += rx_d;
            self.session_tx += tx_d;
            // Light smoothing hides the 64 KiB counter quantization.
            let alpha = 0.6;
            self.rate_rx_ema = alpha * (rx_d as f64 / dt) + (1.0 - alpha) * self.rate_rx_ema;
            self.rate_tx_ema = alpha * (tx_d as f64 / dt) + (1.0 - alpha) * self.rate_tx_ema;
            out.rx_per_sec = Bytes(self.rate_rx_ema as u64);
            out.tx_per_sec = Bytes(self.rate_tx_ema as u64);
        }
        out.rx_session = Bytes(self.session_rx);
        out.tx_session = Bytes(self.session_tx);

        // Before the first delta (or an idle network): fall back to the
        // interface with the largest cumulative traffic.
        if busiest.as_ref().is_none_or(|(b, _)| *b == 0) {
            busiest = counters
                .iter()
                .filter(|c| c.up && !c.loopback && physical(&c.name))
                .max_by_key(|c| c.rx_bytes + c.tx_bytes)
                .map(|c| (0, c.name.clone()));
        }

        if let Some(c) = busiest.and_then(|(_, name)| counters.iter().find(|c| c.name == name)) {
            // getifaddrs is cheap (~µs); resolve the primary's IPv4 fresh so
            // DHCP changes show up.
            let ips = ipv4_addresses();
            out.primary = Some(PrimaryIf {
                name: c.name.clone(),
                baudrate: c.baudrate,
                ipv4: ips.get(&c.name).cloned(),
                mac: c.mac.clone(),
                running: c.running,
            });
        }

        self.prev = Some((now, current));
        Ok(out)
    }
}
