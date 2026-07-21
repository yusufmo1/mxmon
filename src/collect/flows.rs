//! Per-connection network flows over the ntstat kernel control: bytes, RTT,
//! and retransmits for every process's TCP/UDP flows — `nettop` data, no
//! sudo, rendered live.
//!
//! The protocol is private and its structs have drifted across macOS
//! releases, so nothing here transmutes a kernel message: every field is
//! read through bounds-checked offset readers gated on the advertised
//! message length. The offsets in `wire` were frozen from a `--flows-debug`
//! run on this kernel (macOS 26), which self-calibrates by locating mxmon's
//! own probe connections inside the raw bytes.

use std::collections::HashMap;
use std::io;
use std::time::Instant;

use crate::collect::net::counter_delta;
use crate::ffi::ntstat::NtstatSocket;
use crate::units::Bytes;

/// Wire-format encoding/decoding: pure byte-slice logic, unit-tested with
/// canned buffers.
pub(crate) mod wire {
    use std::net::{Ipv4Addr, Ipv6Addr};

    // ---- message types (bsd/net/ntstat.h) --------------------------------
    pub const MSG_ADD_ALL_SRCS: u32 = 1002;
    pub const MSG_GET_UPDATE: u32 = 1007;
    pub const MSG_SRC_REMOVED: u32 = 10002;
    pub const MSG_SRC_UPDATE: u32 = 10006;

    pub const PROVIDER_TCP_KERNEL: u32 = 2;
    pub const PROVIDER_UDP_KERNEL: u32 = 4;

    pub const SRC_REF_ALL: u64 = u64::MAX;

    /// `nstat_msg_hdr`: context, type, length, flags.
    pub const HDR_LEN: usize = 16;

    // ---- SRC_UPDATE offsets, verified on-device via --flows-debug --------
    /// `srcref` (u64) in any src-scoped message.
    pub const OFF_SRCREF: usize = 16;
    /// Inside the `nstat_counts` block at 32: rx/tx byte totals (u64),
    /// retransmitted bytes (u32), and RTT stats (u32, microseconds).
    pub const OFF_RXBYTES: usize = 40;
    pub const OFF_TXBYTES: usize = 56;
    pub const OFF_TXRETRANSMIT: usize = 120;
    pub const OFF_AVG_RTT: usize = 136;
    pub const OFF_VAR_RTT: usize = 140;
    /// `nstat_provider_id_t` after the counts.
    pub const OFF_PROVIDER: usize = 144;
    /// TCP descriptor (starts at 152): state, pid, endpoints, pname.
    pub const OFF_TCP_STATE: usize = 228;
    pub const OFF_TCP_PID: usize = 268;
    pub const OFF_TCP_LOCAL: usize = 276;
    pub const OFF_TCP_REMOTE: usize = 304;
    pub const OFF_TCP_PNAME: usize = 348;
    /// UDP descriptor: endpoints, pid, pname.
    pub const OFF_UDP_LOCAL: usize = 208;
    pub const OFF_UDP_REMOTE: usize = 236;
    pub const OFF_UDP_PID: usize = 280;
    pub const OFF_UDP_PNAME: usize = 284;
    /// `pname` is a fixed 64-byte C string in both descriptors.
    pub const PNAME_LEN: usize = 64;

    /// Parsed `nstat_msg_hdr` (bytes 0..8 are the request context — nothing
    /// reads it back because polling drains everything unconditionally).
    #[derive(Debug, Clone, Copy)]
    pub struct MsgHdr {
        pub typ: u32,
        pub length: u16,
    }

    pub fn u16_at(b: &[u8], o: usize) -> Option<u16> {
        Some(u16::from_ne_bytes(b.get(o..o + 2)?.try_into().ok()?))
    }
    pub fn u32_at(b: &[u8], o: usize) -> Option<u32> {
        Some(u32::from_ne_bytes(b.get(o..o + 4)?.try_into().ok()?))
    }
    pub fn u64_at(b: &[u8], o: usize) -> Option<u64> {
        Some(u64::from_ne_bytes(b.get(o..o + 8)?.try_into().ok()?))
    }
    pub fn i32_at(b: &[u8], o: usize) -> Option<i32> {
        Some(i32::from_ne_bytes(b.get(o..o + 4)?.try_into().ok()?))
    }

    pub fn parse_hdr(buf: &[u8]) -> Option<MsgHdr> {
        u64_at(buf, 0)?; // require a full 16-byte header, context included
        Some(MsgHdr {
            typ: u32_at(buf, 8)?,
            length: u16_at(buf, 12)?,
        })
    }

    fn hdr_bytes(context: u64, typ: u32, length: u16) -> Vec<u8> {
        let mut v = Vec::with_capacity(length as usize);
        v.extend_from_slice(&context.to_ne_bytes());
        v.extend_from_slice(&typ.to_ne_bytes());
        v.extend_from_slice(&length.to_ne_bytes());
        v.extend_from_slice(&0u16.to_ne_bytes()); // flags
        v
    }

    /// `nstat_msg_add_all_srcs`: hdr, filter u64, events u64, provider u32,
    /// target pid i32, target uuid [16]. 56 bytes.
    pub fn encode_add_all_srcs(context: u64, provider: u32) -> Vec<u8> {
        let mut v = hdr_bytes(context, MSG_ADD_ALL_SRCS, 56);
        v.extend_from_slice(&0u64.to_ne_bytes()); // filter: no restriction
        v.extend_from_slice(&0u64.to_ne_bytes()); // events: none (we poll)
        v.extend_from_slice(&provider.to_ne_bytes());
        v.extend_from_slice(&0i32.to_ne_bytes()); // pid 0 = all
        v.extend_from_slice(&[0u8; 16]); // uuid: all
        v
    }

    /// `nstat_msg_query_src_req`: hdr + srcref. GET_UPDATE with REF_ALL
    /// makes the kernel emit one SRC_UPDATE (counts + descriptor) per source.
    pub fn encode_get_update(context: u64) -> Vec<u8> {
        let mut v = hdr_bytes(context, MSG_GET_UPDATE, 24);
        v.extend_from_slice(&SRC_REF_ALL.to_ne_bytes());
        v
    }

    /// Walk a recv buffer that may hold several back-to-back messages, each
    /// starting with an `nstat_msg_hdr` whose `length` gives the stride.
    pub fn for_each_msg(buf: &[u8], mut f: impl FnMut(&MsgHdr, &[u8])) {
        let mut off = 0;
        while off < buf.len() {
            let Some(hdr) = parse_hdr(&buf[off..]) else {
                return; // trailing fragment shorter than a header
            };
            let len = (hdr.length as usize).max(HDR_LEN);
            let end = (off + len).min(buf.len());
            f(&hdr, &buf[off..end]);
            off += len;
        }
    }

    /// Per-flow counters extracted from one SRC_UPDATE.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Counts {
        pub rx_bytes: u64,
        pub tx_bytes: u64,
        pub retx_bytes: u32,
        pub avg_rtt_us: u32,
        pub var_rtt_us: u32,
    }

    /// Identity parsed from a provider descriptor.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DescInfo {
        pub pid: i32,
        pub pname: String,
        pub local: String,
        pub remote: String,
        pub tcp_state: u32,
        pub udp: bool,
    }

    #[derive(Debug, Clone)]
    pub struct SrcUpdate {
        pub srcref: u64,
        pub counts: Counts,
        pub desc: Option<DescInfo>,
    }

    /// Render one 28-byte sockaddr union slot ("ip:port"). `Some("*")` for a
    /// zeroed slot (unbound), `None` when out of bounds / unknown family.
    fn sockaddr_at(msg: &[u8], off: usize) -> Option<String> {
        let len = *msg.get(off)?;
        let family = *msg.get(off + 1)?;
        if len == 0 {
            return Some("*".into());
        }
        let port = u16::from_be_bytes([*msg.get(off + 2)?, *msg.get(off + 3)?]);
        match i32::from(family) {
            libc::AF_INET => {
                let ip: [u8; 4] = msg.get(off + 4..off + 8)?.try_into().ok()?;
                Some(format!("{}:{port}", Ipv4Addr::from(ip)))
            }
            libc::AF_INET6 => {
                let ip: [u8; 16] = msg.get(off + 8..off + 24)?.try_into().ok()?;
                Some(format!("[{}]:{port}", Ipv6Addr::from(ip)))
            }
            _ => None,
        }
    }

    fn pname_at(msg: &[u8], off: usize) -> Option<String> {
        let raw = msg.get(off..off + PNAME_LEN)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        Some(String::from_utf8_lossy(&raw[..end]).into_owned())
    }

    /// Parse one SRC_UPDATE message. Counts parse whenever present; the
    /// descriptor parses only when the message is long enough for its
    /// provider's layout (shorter = kernel drift → counts-only, never junk).
    pub fn parse_src_update(msg: &[u8]) -> Option<SrcUpdate> {
        let srcref = u64_at(msg, OFF_SRCREF)?;
        let counts = Counts {
            rx_bytes: u64_at(msg, OFF_RXBYTES)?,
            tx_bytes: u64_at(msg, OFF_TXBYTES)?,
            retx_bytes: u32_at(msg, OFF_TXRETRANSMIT)?,
            avg_rtt_us: u32_at(msg, OFF_AVG_RTT)?,
            var_rtt_us: u32_at(msg, OFF_VAR_RTT)?,
        };
        let desc = match u32_at(msg, OFF_PROVIDER)? {
            PROVIDER_TCP_KERNEL => Some(DescInfo {
                pid: i32_at(msg, OFF_TCP_PID)?,
                pname: pname_at(msg, OFF_TCP_PNAME)?,
                local: sockaddr_at(msg, OFF_TCP_LOCAL)?,
                remote: sockaddr_at(msg, OFF_TCP_REMOTE)?,
                tcp_state: u32_at(msg, OFF_TCP_STATE)?,
                udp: false,
            }),
            PROVIDER_UDP_KERNEL => Some(DescInfo {
                pid: i32_at(msg, OFF_UDP_PID)?,
                pname: pname_at(msg, OFF_UDP_PNAME)?,
                local: sockaddr_at(msg, OFF_UDP_LOCAL)?,
                remote: sockaddr_at(msg, OFF_UDP_REMOTE)?,
                tcp_state: 0,
                udp: true,
            }),
            _ => None,
        };
        Some(SrcUpdate {
            srcref,
            counts,
            desc,
        })
    }

    /// TCP FSM state names (netinet/tcp_fsm.h ordering).
    pub fn tcp_state_name(state: u32) -> &'static str {
        match state {
            0 => "CLOSED",
            1 => "LISTEN",
            2 => "SYNSENT",
            3 => "SYNRCVD",
            4 => "ESTAB",
            5 => "CLOSEWT",
            6 => "FINW1",
            7 => "CLOSING",
            8 => "LASTACK",
            9 => "FINW2",
            10 => "TIMEWT",
            _ => "?",
        }
    }
}

// ---- collector -----------------------------------------------------------

/// One rendered connection.
#[derive(Debug, Clone)]
pub struct Flow {
    pub pid: i32,
    pub pname: String,
    pub local: String,
    pub remote: String,
    pub state: &'static str,
    pub udp: bool,
    pub rx_rate: Bytes,
    pub tx_rate: Bytes,
    pub rx_total: Bytes,
    pub tx_total: Bytes,
    /// Smoothed round-trip time; `None` for UDP / no measurement yet.
    pub srtt_ms: Option<f32>,
    /// Lifetime retransmitted share of transmitted bytes.
    pub retx_pct: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct FlowSample {
    /// Most-active first (rate, then lifetime bytes), capped at 512.
    pub flows: Vec<Flow>,
    /// pid → (rx bytes/s, tx bytes/s) across all its flows.
    pub by_pid: HashMap<i32, (u64, u64)>,
    pub rx_total_rate: u64,
    pub tx_total_rate: u64,
    /// All live sources, including ones not shown.
    pub count: usize,
}

/// Aggregate per-pid rates from rendered flows.
pub(crate) fn aggregate_by_pid(flows: &[Flow]) -> HashMap<i32, (u64, u64)> {
    let mut map: HashMap<i32, (u64, u64)> = HashMap::new();
    for f in flows {
        let e = map.entry(f.pid).or_default();
        e.0 += f.rx_rate.0;
        e.1 += f.tx_rate.0;
    }
    map
}

struct FlowState {
    prev: wire::Counts,
    at: Instant,
    desc: Option<wire::DescInfo>,
    ema_rx: f64,
    ema_tx: f64,
    last_seen: u64,
}

/// Drop flow state not refreshed for this many polls (srcref-reuse safety;
/// SRC_REMOVED normally gets there first).
const GC_POLLS: u64 = 30;
const MAX_FLOWS: usize = 512;

/// Fold one parsed `SRC_UPDATE` into per-flow state: EMA'd rates over the
/// real elapsed window (sub-50 ms gaps are folded without a rate update so a
/// burst of queued sweeps can't divide by ~zero), descriptor retention, and
/// a liveness stamp for GC.
fn apply_update(states: &mut HashMap<u64, FlowState>, up: wire::SrcUpdate, now: Instant, seq: u64) {
    let st = states.entry(up.srcref).or_insert_with(|| FlowState {
        prev: up.counts,
        at: now,
        desc: None,
        ema_rx: 0.0,
        ema_tx: 0.0,
        last_seen: seq,
    });
    let dt = now.duration_since(st.at).as_secs_f64();
    if dt > 0.05 {
        let rx = counter_delta(up.counts.rx_bytes, st.prev.rx_bytes);
        let tx = counter_delta(up.counts.tx_bytes, st.prev.tx_bytes);
        let alpha = 0.6;
        st.ema_rx = alpha * (rx as f64 / dt) + (1.0 - alpha) * st.ema_rx;
        st.ema_tx = alpha * (tx as f64 / dt) + (1.0 - alpha) * st.ema_tx;
        st.at = now;
    }
    st.prev = up.counts;
    if up.desc.is_some() {
        st.desc = up.desc;
    }
    st.last_seen = seq;
}

/// Drop flows not refreshed within [`GC_POLLS`] polls of `seq`.
fn gc_stale(states: &mut HashMap<u64, FlowState>, seq: u64) {
    states.retain(|_, st| seq.saturating_sub(st.last_seen) < GC_POLLS);
}

/// Render current per-flow state into a sample: described flows only, most
/// active first (rate, then lifetime bytes), capped at [`MAX_FLOWS`].
fn build_sample(states: &HashMap<u64, FlowState>) -> FlowSample {
    let mut flows: Vec<Flow> = states
        .values()
        .filter_map(|st| {
            let d = st.desc.as_ref()?;
            Some(Flow {
                pid: d.pid,
                pname: d.pname.clone(),
                local: d.local.clone(),
                remote: d.remote.clone(),
                state: if d.udp {
                    "UDP"
                } else {
                    wire::tcp_state_name(d.tcp_state)
                },
                udp: d.udp,
                rx_rate: Bytes(st.ema_rx as u64),
                tx_rate: Bytes(st.ema_tx as u64),
                rx_total: Bytes(st.prev.rx_bytes),
                tx_total: Bytes(st.prev.tx_bytes),
                srtt_ms: (!d.udp && st.prev.avg_rtt_us > 0)
                    .then(|| st.prev.avg_rtt_us as f32 / 1000.0),
                retx_pct: (!d.udp && st.prev.tx_bytes > 0).then(|| {
                    (f64::from(st.prev.retx_bytes) * 100.0 / st.prev.tx_bytes as f64) as f32
                }),
            })
        })
        .collect();
    flows.sort_by(|a, b| {
        let ra = a.rx_rate.0 + a.tx_rate.0;
        let rb = b.rx_rate.0 + b.tx_rate.0;
        rb.cmp(&ra)
            .then_with(|| (b.rx_total.0 + b.tx_total.0).cmp(&(a.rx_total.0 + a.tx_total.0)))
    });
    flows.truncate(MAX_FLOWS);

    let by_pid = aggregate_by_pid(&flows);
    let (rx_total_rate, tx_total_rate) = by_pid
        .values()
        .fold((0, 0), |(rx, tx), &(r, t)| (rx + r, tx + t));
    FlowSample {
        count: states.len(),
        rx_total_rate,
        tx_total_rate,
        by_pid,
        flows,
    }
}

pub struct FlowsCollector {
    sock: NtstatSocket,
    recv_buf: Vec<u8>,
    states: HashMap<u64, FlowState>,
    ctx: u64,
    poll_seq: u64,
    /// One transparent reopen is allowed; a second consecutive failure
    /// surfaces as an error (→ SourceDown).
    errored: bool,
}

impl FlowsCollector {
    pub fn new() -> io::Result<Self> {
        let sock = Self::open_subscribed()?;
        Ok(Self {
            sock,
            recv_buf: vec![0u8; 64 * 1024],
            states: HashMap::new(),
            ctx: 10,
            poll_seq: 0,
            errored: false,
        })
    }

    fn open_subscribed() -> io::Result<NtstatSocket> {
        let sock = NtstatSocket::open()?;
        sock.send(&wire::encode_add_all_srcs(1, wire::PROVIDER_TCP_KERNEL))?;
        sock.send(&wire::encode_add_all_srcs(2, wire::PROVIDER_UDP_KERNEL))?;
        Ok(sock)
    }

    /// Poll cycle: drain everything queued (the previous tick's update sweep
    /// plus async add/remove events), fold into per-flow state, then request
    /// the next sweep — each call renders a full ~one-interval window
    /// without ever sleeping on the sampler thread.
    pub fn sample(&mut self) -> io::Result<FlowSample> {
        self.poll_seq += 1;
        match self.poll_once() {
            Ok(sample) => {
                self.errored = false;
                Ok(sample)
            }
            Err(first) => {
                if self.errored {
                    return Err(first);
                }
                // One silent recovery: reopen + resubscribe, blank window.
                self.errored = true;
                self.states.clear();
                self.sock = Self::open_subscribed()?;
                Ok(FlowSample::default())
            }
        }
    }

    fn poll_once(&mut self) -> io::Result<FlowSample> {
        let now = Instant::now();
        let states = &mut self.states;
        let seq = self.poll_seq;
        while let Some(n) = self.sock.recv(&mut self.recv_buf)? {
            wire::for_each_msg(&self.recv_buf[..n], |hdr, msg| match hdr.typ {
                wire::MSG_SRC_UPDATE => {
                    if let Some(up) = wire::parse_src_update(msg) {
                        apply_update(states, up, now, seq);
                    }
                }
                wire::MSG_SRC_REMOVED => {
                    if let Some(srcref) = wire::u64_at(msg, wire::OFF_SRCREF) {
                        states.remove(&srcref);
                    }
                }
                _ => {}
            });
        }
        gc_stale(states, seq);

        // Ask for the sweep the *next* poll will fold in.
        self.ctx += 1;
        self.sock.send(&wire::encode_get_update(self.ctx))?;

        Ok(build_sample(states))
    }
}

// ---- self-calibrating protocol probe (--flows-debug) ---------------------

/// Dump the live message stream, auto-locate descriptor field offsets by
/// finding our own probe connections in the raw bytes, and run the real
/// collector end-to-end for comparison against `nettop`.
pub fn debug_dump() -> io::Result<()> {
    use wire::{
        MSG_SRC_UPDATE, PROVIDER_TCP_KERNEL, PROVIDER_UDP_KERNEL, encode_add_all_srcs,
        encode_get_update, u32_at, u64_at,
    };

    let sock = NtstatSocket::open()?;
    sock.send(&encode_add_all_srcs(1, PROVIDER_TCP_KERNEL))?;
    sock.send(&encode_add_all_srcs(2, PROVIDER_UDP_KERNEL))?;
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Connections we fully control: loopback TCP pair + a UDP socket, so
    // known ports, a known pid (ours), and a known pname ("mxmon").
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let lport = listener.local_addr()?.port();
    let client = std::net::TcpStream::connect(("127.0.0.1", lport))?;
    let cport = client.local_addr()?.port();
    let _server = listener.accept()?;
    let udp = std::net::UdpSocket::bind("127.0.0.1:0")?;
    let uport = udp.local_addr()?.port();
    udp.send_to(b"probe", ("127.0.0.1", uport))?;
    let own_pid = std::process::id();
    println!("probe: tcp 127.0.0.1:{cport}->{lport}, udp :{uport}, pid {own_pid}");

    let mut buf = vec![0u8; 65536];
    std::thread::sleep(std::time::Duration::from_millis(300));
    sock.send(&encode_get_update(3))?;
    std::thread::sleep(std::time::Duration::from_millis(300));

    let calibrate = |label: &str, msg: &[u8], ports: &[u16]| {
        println!("\n{label} flow found in a {}-byte SRC_UPDATE:", msg.len());
        let pid_bytes = (own_pid as i32).to_ne_bytes();
        for (i, w) in msg.windows(4).enumerate() {
            if w == pid_bytes {
                println!("  pid         @ {i}");
            }
            for &p in ports {
                if w[0] == 0x10 && w[1] == 0x02 && w[2..4] == p.to_be_bytes() {
                    println!("  sockaddr_in @ {i} (port {p})");
                }
            }
        }
        for i in 0..msg.len().saturating_sub(5) {
            if &msg[i..i + 5] == b"mxmon" {
                println!("  pname       @ {i}");
            }
        }
    };

    let mut histogram: HashMap<u32, (usize, u16)> = HashMap::new();
    let mut tcp_done = false;
    let mut udp_done = false;
    while let Some(n) = sock.recv(&mut buf)? {
        wire::for_each_msg(&buf[..n], |hdr, msg| {
            let e = histogram.entry(hdr.typ).or_insert((0, hdr.length));
            e.0 += 1;
            e.1 = e.1.max(hdr.length);
            if hdr.typ != MSG_SRC_UPDATE {
                return;
            }
            let pid_bytes = (own_pid as i32).to_ne_bytes();
            if !msg.windows(4).any(|w| w == pid_bytes) {
                return;
            }
            if u32_at(msg, wire::OFF_PROVIDER) == Some(PROVIDER_UDP_KERNEL) {
                if !udp_done {
                    udp_done = true;
                    calibrate("UDP", msg, &[uport]);
                }
            } else if !tcp_done {
                tcp_done = true;
                calibrate("TCP", msg, &[cport, lport]);
                println!(
                    "  srcref      = {:#x}, state = {}",
                    u64_at(msg, wire::OFF_SRCREF).unwrap_or(0),
                    u32_at(msg, wire::OFF_TCP_STATE).unwrap_or(0)
                );
            }
        });
    }

    println!("\nmessage histogram (type -> count, max len):");
    let mut types: Vec<_> = histogram.iter().collect();
    types.sort();
    for (t, (n, len)) in types {
        println!("  {t:6} -> {n:4} msgs, max {len} bytes");
    }

    // Parsed end-to-end pass through the real collector for nettop-diffing.
    drop(sock);
    let mut collector = FlowsCollector::new()?;
    let _ = collector.sample();
    std::thread::sleep(std::time::Duration::from_millis(600));
    let sample = collector.sample()?;
    println!(
        "\ncollector: {} sources, {} rendered, Σ↓ {}/s Σ↑ {}/s",
        sample.count,
        sample.flows.len(),
        Bytes(sample.rx_total_rate),
        Bytes(sample.tx_total_rate)
    );
    for f in sample.flows.iter().take(20) {
        println!(
            "  {:>7} {:16} {:24} -> {:24} {:7} rtt={:?} retx={:?} Σ{}/{}",
            f.pid,
            f.pname.chars().take(16).collect::<String>(),
            f.local,
            f.remote,
            f.state,
            f.srtt_ms.map(|v| (v * 10.0).round() / 10.0),
            f.retx_pct.map(|v| (v * 10.0).round() / 10.0),
            f.rx_total,
            f.tx_total
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Flow, aggregate_by_pid, wire};
    use crate::units::Bytes;

    /// Build a synthetic SRC_UPDATE message with known values at the frozen
    /// offsets (the layout --flows-debug verified on this kernel).
    fn canned_src_update(udp: bool) -> Vec<u8> {
        let total = if udp { 432 } else { 496 };
        let mut m = vec![0u8; total];
        m[8..12].copy_from_slice(&wire::MSG_SRC_UPDATE.to_ne_bytes());
        m[12..14].copy_from_slice(&(total as u16).to_ne_bytes());
        m[wire::OFF_SRCREF..wire::OFF_SRCREF + 8].copy_from_slice(&0xfau64.to_ne_bytes());
        m[wire::OFF_RXBYTES..wire::OFF_RXBYTES + 8].copy_from_slice(&1000u64.to_ne_bytes());
        m[wire::OFF_TXBYTES..wire::OFF_TXBYTES + 8].copy_from_slice(&2000u64.to_ne_bytes());
        m[wire::OFF_TXRETRANSMIT..wire::OFF_TXRETRANSMIT + 4].copy_from_slice(&40u32.to_ne_bytes());
        m[wire::OFF_AVG_RTT..wire::OFF_AVG_RTT + 4].copy_from_slice(&3800u32.to_ne_bytes());
        let provider = if udp { 4u32 } else { 2u32 };
        m[wire::OFF_PROVIDER..wire::OFF_PROVIDER + 4].copy_from_slice(&provider.to_ne_bytes());
        let (pid_off, pname_off, local_off) = if udp {
            (wire::OFF_UDP_PID, wire::OFF_UDP_PNAME, wire::OFF_UDP_LOCAL)
        } else {
            (wire::OFF_TCP_PID, wire::OFF_TCP_PNAME, wire::OFF_TCP_LOCAL)
        };
        m[pid_off..pid_off + 4].copy_from_slice(&4242i32.to_ne_bytes());
        m[pname_off..pname_off + 5].copy_from_slice(b"mxmon");
        // sockaddr_in: len 16, AF_INET, port 443 BE, 10.0.0.1.
        m[local_off] = 16;
        m[local_off + 1] = 2;
        m[local_off + 2..local_off + 4].copy_from_slice(&443u16.to_be_bytes());
        m[local_off + 4..local_off + 8].copy_from_slice(&[10, 0, 0, 1]);
        if !udp {
            m[wire::OFF_TCP_STATE..wire::OFF_TCP_STATE + 4].copy_from_slice(&4u32.to_ne_bytes());
        }
        m
    }

    #[test]
    fn ntstat_parse_src_update() {
        let m = canned_src_update(false);
        let up = wire::parse_src_update(&m).expect("tcp update parses");
        assert_eq!(up.srcref, 0xfa);
        assert_eq!(up.counts.rx_bytes, 1000);
        assert_eq!(up.counts.tx_bytes, 2000);
        assert_eq!(up.counts.retx_bytes, 40);
        assert_eq!(up.counts.avg_rtt_us, 3800);
        let d = up.desc.expect("descriptor");
        assert_eq!(d.pid, 4242);
        assert_eq!(d.pname, "mxmon");
        assert_eq!(d.local, "10.0.0.1:443");
        assert_eq!(d.remote, "*"); // zeroed slot
        assert_eq!(wire::tcp_state_name(d.tcp_state), "ESTAB");
        assert!(!d.udp);

        let u = wire::parse_src_update(&canned_src_update(true)).expect("udp update parses");
        let d = u.desc.expect("descriptor");
        assert!(d.udp);
        assert_eq!(d.pid, 4242);
        assert_eq!(d.local, "10.0.0.1:443");
    }

    #[test]
    fn ntstat_parse_survives_truncation_and_growth() {
        let m = canned_src_update(false);
        // Every truncation length must parse to None or Some — never panic —
        // and a descriptor must never materialize from a short message.
        for cut in 0..m.len() {
            let up = wire::parse_src_update(&m[..cut]);
            if cut < 412 {
                assert!(up.is_none() || up.as_ref().unwrap().desc.is_none());
            }
        }
        // Longer-than-expected messages (a newer kernel appending fields) parse.
        let mut grown = m.clone();
        grown.extend_from_slice(&[0xAA; 64]);
        assert!(wire::parse_src_update(&grown).unwrap().desc.is_some());
    }

    #[test]
    fn ntstat_msg_walk_and_encode() {
        // Encoders produce self-consistent headers.
        let add = wire::encode_add_all_srcs(7, wire::PROVIDER_TCP_KERNEL);
        assert_eq!(add.len(), 56);
        let hdr = wire::parse_hdr(&add).unwrap();
        assert_eq!((hdr.typ, hdr.length), (1002, 56));
        let q = wire::encode_get_update(9);
        assert_eq!(q.len(), 24);

        // Two packed messages walk as two; a trailing fragment is ignored.
        let mut buf = add.clone();
        buf.extend_from_slice(&q);
        buf.extend_from_slice(&[1, 2, 3]);
        let mut seen = Vec::new();
        wire::for_each_msg(&buf, |h, m| seen.push((h.typ, m.len())));
        assert_eq!(seen, vec![(1002, 56), (1007, 24)]);
    }

    #[test]
    fn flow_pid_aggregation() {
        let f = |pid, rx, tx| Flow {
            pid,
            pname: String::new(),
            local: String::new(),
            remote: String::new(),
            state: "ESTAB",
            udp: false,
            rx_rate: Bytes(rx),
            tx_rate: Bytes(tx),
            rx_total: Bytes(0),
            tx_total: Bytes(0),
            srtt_ms: None,
            retx_pct: None,
        };
        let map = aggregate_by_pid(&[f(1, 100, 10), f(1, 50, 5), f(2, 7, 3)]);
        assert_eq!(map[&1], (150, 15));
        assert_eq!(map[&2], (7, 3));
    }

    // ---- state fold (the poll_once core, minus the socket) ---------------

    use super::{GC_POLLS, apply_update, build_sample, gc_stale};
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    fn upd(srcref: u64, rx: u64, tx: u64, desc: Option<wire::DescInfo>) -> wire::SrcUpdate {
        wire::SrcUpdate {
            srcref,
            counts: wire::Counts {
                rx_bytes: rx,
                tx_bytes: tx,
                retx_bytes: 10,
                avg_rtt_us: 42_000,
                var_rtt_us: 0,
            },
            desc,
        }
    }

    fn desc(pid: i32, udp: bool) -> wire::DescInfo {
        wire::DescInfo {
            pid,
            pname: "proc".into(),
            local: "10.0.0.1:1".into(),
            remote: "10.0.0.2:2".into(),
            tcp_state: 4,
            udp,
        }
    }

    #[test]
    fn flow_fold_ema_and_dt_gate() {
        let mut states = HashMap::new();
        let t0 = Instant::now();
        apply_update(&mut states, upd(1, 0, 0, None), t0, 1);
        assert!(states[&1].ema_rx.abs() < f64::EPSILON, "seed pass: no rate");
        // One second, 1000 rx / 500 tx bytes → EMA moves 60% of the way.
        apply_update(
            &mut states,
            upd(1, 1000, 500, None),
            t0 + Duration::from_secs(1),
            2,
        );
        assert!((states[&1].ema_rx - 600.0).abs() < 1e-6);
        assert!((states[&1].ema_tx - 300.0).abs() < 1e-6);
        // A queued sweep 10 ms later folds counters WITHOUT a rate update —
        // the dt gate stops a near-zero window from exploding the rate.
        apply_update(
            &mut states,
            upd(1, 1400, 700, None),
            t0 + Duration::from_millis(1010),
            3,
        );
        assert!((states[&1].ema_rx - 600.0).abs() < 1e-6, "sub-50ms skipped");
        assert_eq!(states[&1].prev.rx_bytes, 1400, "counters still fold");
        // The next full window rates against the folded counters.
        apply_update(
            &mut states,
            upd(1, 2400, 1200, None),
            t0 + Duration::from_secs(2),
            4,
        );
        assert!(
            (states[&1].ema_rx - 840.0).abs() < 40.0,
            "0.6·1000 + 0.4·600 ≈ 840, got {}",
            states[&1].ema_rx
        );
    }

    #[test]
    fn flow_fold_keeps_descriptor_and_reseeds_after_removal() {
        let mut states = HashMap::new();
        let t0 = Instant::now();
        apply_update(
            &mut states,
            upd(7, 5000, 5000, Some(desc(42, false))),
            t0,
            1,
        );
        // Sweeps without a descriptor keep the identity.
        apply_update(
            &mut states,
            upd(7, 6000, 6000, None),
            t0 + Duration::from_secs(1),
            2,
        );
        assert_eq!(states[&7].desc.as_ref().unwrap().pid, 42);
        // SRC_REMOVED then srcref reuse: fresh state, no rate carry-over.
        states.remove(&7);
        apply_update(
            &mut states,
            upd(7, 10, 10, Some(desc(43, true))),
            t0 + Duration::from_secs(2),
            3,
        );
        assert!(states[&7].ema_rx.abs() < f64::EPSILON);
        assert_eq!(states[&7].desc.as_ref().unwrap().pid, 43);
    }

    #[test]
    fn flow_gc_drops_only_stale_entries() {
        let mut states = HashMap::new();
        let t0 = Instant::now();
        apply_update(&mut states, upd(1, 0, 0, None), t0, 1);
        apply_update(&mut states, upd(2, 0, 0, None), t0, GC_POLLS + 1);
        gc_stale(&mut states, GC_POLLS + 1);
        assert!(!states.contains_key(&1), "unseen for GC_POLLS polls");
        assert!(states.contains_key(&2));
    }

    #[test]
    fn flow_sample_orders_hides_and_derives() {
        let mut states = HashMap::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(1);
        // A slow TCP flow, a fast UDP flow, and a source never described.
        apply_update(&mut states, upd(1, 0, 0, Some(desc(10, false))), t0, 1);
        apply_update(&mut states, upd(1, 10_000, 2_000, None), t1, 2);
        apply_update(&mut states, upd(2, 0, 0, Some(desc(20, true))), t0, 1);
        apply_update(&mut states, upd(2, 100_000, 4_000, None), t1, 2);
        apply_update(&mut states, upd(3, 0, 0, None), t0, 1);

        let s = build_sample(&states);
        assert_eq!(s.count, 3, "count includes undescribed sources");
        assert_eq!(s.flows.len(), 2, "only described flows render");
        assert_eq!(s.flows[0].pid, 20, "most active first");
        // TCP derives srtt (µs → ms) and retransmit share; UDP gets neither.
        let tcp = s.flows.iter().find(|f| !f.udp).unwrap();
        assert_eq!(tcp.state, "ESTAB");
        assert!((tcp.srtt_ms.unwrap() - 42.0).abs() < 1e-3);
        assert!(
            (tcp.retx_pct.unwrap() - 0.5).abs() < 1e-3,
            "10 of 2000 bytes"
        );
        let udp = s.flows.iter().find(|f| f.udp).unwrap();
        assert_eq!(udp.state, "UDP");
        assert!(udp.srtt_ms.is_none() && udp.retx_pct.is_none());
        // Footer totals equal the per-pid aggregation.
        assert_eq!(
            s.rx_total_rate,
            s.by_pid.values().map(|&(r, _)| r).sum::<u64>()
        );
    }

    mod prop {
        use super::{canned_src_update, wire};
        use proptest::prelude::*;

        proptest! {
            // The kernel owns this wire format and has changed it across
            // releases — every parser must be total over arbitrary bytes.
            #[test]
            fn wire_parsers_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..640)) {
                let _ = wire::parse_hdr(&bytes);
                let _ = wire::parse_src_update(&bytes);
                wire::for_each_msg(&bytes, |_, _| {});
            }

            #[test]
            fn wire_bitflips_of_real_messages_never_panic(idx in 0usize..496, bit in 0u32..8) {
                let mut m = canned_src_update(false);
                m[idx] ^= 1 << bit;
                let _ = wire::parse_src_update(&m);
                wire::for_each_msg(&m, |_, _| {});
            }
        }
    }
}
