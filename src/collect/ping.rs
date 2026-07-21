//! Active connectivity probe: ICMP echo RTT, smoothed latency and jitter,
//! and reachability against a configurable host (default 1.1.1.1). The only
//! collector that emits packets; `ping = false` in config.toml disables it.

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use crate::ffi::icmp::Pinger;

#[derive(Debug, Clone, Default)]
pub struct PingSample {
    /// This probe's round trip; `None` = timed out.
    pub rtt_ms: Option<f32>,
    /// Smoothed (EMA) round-trip latency.
    pub latency_ms: Option<f32>,
    /// Smoothed mean |Δrtt| between consecutive probes (RFC 3550-style).
    pub jitter_ms: Option<f32>,
    /// Reachability, debounced by one miss so a single lost packet doesn't
    /// flap the UI.
    pub up: bool,
    pub host: String,
}

/// Pure smoothing state, separated from the socket so it can be unit-tested.
#[derive(Debug, Clone, Default)]
pub struct PingStats {
    ema: Option<f32>,
    jitter: Option<f32>,
    last: Option<f32>,
    misses: u32,
}

const ALPHA: f32 = 0.25;

impl PingStats {
    /// Fold one probe result; returns `(latency_ms, jitter_ms, up)`.
    pub fn update(&mut self, rtt_ms: Option<f32>) -> (Option<f32>, Option<f32>, bool) {
        match rtt_ms {
            Some(rtt) => {
                self.misses = 0;
                if let Some(prev) = self.last {
                    let delta = (rtt - prev).abs();
                    self.jitter = Some(self.jitter.map_or(delta, |j| j + ALPHA * (delta - j)));
                }
                self.last = Some(rtt);
                self.ema = Some(self.ema.map_or(rtt, |e| e + ALPHA * (rtt - e)));
            }
            None => self.misses = self.misses.saturating_add(1),
        }
        (self.ema, self.jitter, self.misses < 2)
    }
}

pub struct PingCollector {
    pinger: Pinger,
    stats: PingStats,
    host: String,
    ident: u16,
    seq: u16,
}

impl PingCollector {
    pub fn new(host: &str) -> Result<Self, String> {
        let addr = resolve_v4(host).ok_or_else(|| format!("cannot resolve {host}"))?;
        let pinger = Pinger::open(addr).map_err(|e| format!("icmp socket: {e}"))?;
        Ok(Self {
            pinger,
            stats: PingStats::default(),
            host: host.to_owned(),
            ident: std::process::id() as u16,
            seq: 0,
        })
    }

    /// One probe (blocks up to `timeout`); socket errors count as misses so a
    /// dropped link degrades to "down", never kills the thread.
    pub fn sample(&mut self, timeout: Duration) -> PingSample {
        self.seq = self.seq.wrapping_add(1);
        let rtt = self
            .pinger
            .ping(self.ident, self.seq, timeout)
            .ok()
            .flatten();
        let rtt_ms = rtt.map(|d| d.as_secs_f32() * 1000.0);
        let (latency_ms, jitter_ms, up) = self.stats.update(rtt_ms);
        PingSample {
            rtt_ms,
            latency_ms,
            jitter_ms,
            up,
            host: self.host.clone(),
        }
    }
}

/// Literal IPv4 fast path, one-shot DNS otherwise (no IPv6: the ICMP socket
/// is v4).
fn resolve_v4(host: &str) -> Option<Ipv4Addr> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Some(ip);
    }
    (host, 0).to_socket_addrs().ok()?.find_map(|a| match a {
        SocketAddr::V4(v4) => Some(*v4.ip()),
        SocketAddr::V6(_) => None,
    })
}
