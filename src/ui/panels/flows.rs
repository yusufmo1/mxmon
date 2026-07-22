//! Connections view: every process's live TCP/UDP flows with per-flow
//! throughput, RTT, and retransmit share — nettop with taste. Rows arrive
//! pre-sorted by activity from the collector, so rendering is a plain
//! windowed slice.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::layout::RenderState;
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target};

use super::{chrome_with, format_bits_per_sec, line, split_bits_per_sec};

/// Base column widths; the identity columns absorb spare width so wide
/// terminals show full IPv6 endpoints instead of a dead right margin.
const W_PROC: u16 = 20; // name:pid
const W_LOCAL: u16 = 22;
const W_REMOTE: u16 = 24;
const W_STATE: u16 = 8;
const W_RTT: u16 = 8;
const W_RETX: u16 = 6;
const W_RX: u16 = 11;
const W_TX: u16 = 11;
const W_TOT: u16 = 12;

fn fmt_rtt(ms: Option<f32>) -> String {
    match ms {
        Some(v) if v >= 100.0 => format!("{v:.0}ms"),
        Some(v) => format!("{v:.1}ms"),
        None => "–".into(),
    }
}

pub fn render(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
) {
    let f = &app.flows;
    let (rv, ru) = split_bits_per_sec(f.rx_total_rate);
    let (tv, tu) = split_bits_per_sec(f.tx_total_rate);
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let headline = vec![
        Span::styled(f.count.to_string(), bold(th.text)),
        Span::styled(" · Σ↓ ", dim),
        Span::styled(format!("{rv:>5}"), bold(th.net_rx)),
        Span::styled(format!(" {ru}"), dim),
        Span::styled(" · Σ↑ ", dim),
        Span::styled(format!("{tv:>5}"), bold(th.net_tx)),
        Span::styled(format!(" {tu}"), dim),
    ];
    let inner = chrome_with(buf, area, "CONNECTIONS", headline, th);
    if inner.height < 2 {
        return;
    }
    if f.flows.is_empty() {
        line(
            buf,
            inner,
            0,
            vec![Span::styled("sampling…", Style::default().fg(th.dim))],
        );
        return;
    }

    // Optional columns claim space in priority order (retx is the headline
    // quality signal) — so the drawn set always fits, at every width. The
    // scrollbar rides the border and costs no content column.
    let avail = inner.width;
    let mut need = W_PROC + W_REMOTE + W_STATE + W_RTT + W_RX + W_TX;
    let mut claim = |w: u16| {
        let fits = avail >= need + w;
        if fits {
            need += w;
        }
        fits
    };
    let show_retx = claim(W_RETX);
    let show_local = claim(W_LOCAL);
    let show_tot = claim(W_TOT);

    // Spread spare width over the identity columns (full v6 addresses).
    let extra = avail.saturating_sub(need);
    let w_proc = W_PROC + (extra / 4).min(10);
    let w_local = if show_local {
        W_LOCAL + (extra / 4).min(24)
    } else {
        0
    };
    let w_remote = W_REMOTE + (extra / 2).min(24);

    // Header row.
    let header_style = Style::default()
        .fg(th.bg)
        .bg(th.accent)
        .add_modifier(Modifier::BOLD);
    for x in inner.left()..inner.right() {
        buf[(x, inner.y)].set_style(header_style);
        buf[(x, inner.y)].set_char(' ');
    }
    let mut x = inner.x;
    let mut col = |x: &mut u16, label: &str, width: u16, left: bool| {
        let text = if left {
            format!("{label:<w$}", w = width as usize)
        } else {
            format!("{label:>w$}", w = width as usize)
        };
        buf.set_span(*x, inner.y, &Span::styled(text, header_style), width);
        *x += width;
    };
    col(&mut x, "PROCESS", w_proc, true);
    if show_local {
        col(&mut x, "LOCAL", w_local, true);
    }
    col(&mut x, "REMOTE", w_remote, true);
    col(&mut x, "STATE", W_STATE, true);
    col(&mut x, "RTT", W_RTT, false);
    if show_retx {
        col(&mut x, "RETX", W_RETX, false);
    }
    col(&mut x, "↓/s", W_RX, false);
    col(&mut x, "↑/s", W_TX, false);
    if show_tot {
        col(&mut x, "TOTAL", W_TOT, false);
    }

    // Body.
    let body_h = inner.height.saturating_sub(1) as usize;
    let body = Rect::new(inner.x, inner.y + 1, inner.width, body_h as u16);
    hits.push(body, Target::FlowList);
    rs.flows_scroll = rs.flows_scroll.min(f.flows.len().saturating_sub(body_h));

    for (i, flow) in f
        .flows
        .iter()
        .skip(rs.flows_scroll)
        .take(body_h)
        .enumerate()
    {
        let y = body.y + i as u16;
        // Row targets carry the absolute flow index; a click opens the
        // owning process's details. Registered after the list body so the
        // rows win the overlap, and hover tints the row like the tables.
        let abs = rs.flows_scroll + i;
        hits.push(Rect::new(body.x, y, body.width, 1), Target::FlowRow(abs));
        let hovered = app.hover == Some(Target::FlowRow(abs));
        let active = flow.rx_rate.0 + flow.tx_rate.0 > 0;
        if hovered {
            for x in body.left()..body.right() {
                buf[(x, y)].set_style(Style::default().bg(th.panel_bg));
            }
        }
        let base = if active {
            Style::default().fg(th.text)
        } else {
            Style::default().fg(th.dim)
        };
        let mut x = inner.x;
        let mut cell = |x: &mut u16, text: String, width: u16, style: Style, left: bool| {
            let text = if left {
                format!("{text:<w$}", w = width as usize)
            } else {
                format!("{text:>w$}", w = width as usize)
            };
            buf.set_span(*x, y, &Span::styled(text, style), width);
            *x += width;
        };

        let proc_label = format!("{}:{}", flow.pname, flow.pid);
        cell(
            &mut x,
            truncate(&proc_label, w_proc as usize - 1),
            w_proc,
            if active {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            },
            true,
        );
        if show_local {
            cell(
                &mut x,
                truncate(&flow.local, w_local as usize - 1),
                w_local,
                Style::default().fg(th.dim),
                true,
            );
        }
        cell(
            &mut x,
            truncate(&flow.remote, w_remote as usize - 1),
            w_remote,
            base,
            true,
        );
        let state_color = if flow.udp {
            th.accent
        } else {
            match flow.state {
                "ESTAB" => th.ok,
                "LISTEN" => th.dim,
                _ => th.warn,
            }
        };
        cell(
            &mut x,
            flow.state.into(),
            W_STATE,
            Style::default().fg(state_color),
            true,
        );
        // RTT colored by quality: <20 ms fine, <80 ms noticeable, else red.
        let rtt_color = match flow.srtt_ms {
            Some(v) if v >= 80.0 => th.crit,
            Some(v) if v >= 20.0 => th.warn,
            Some(_) => th.ok,
            None => th.dim,
        };
        cell(
            &mut x,
            fmt_rtt(flow.srtt_ms),
            W_RTT,
            Style::default().fg(rtt_color),
            false,
        );
        if show_retx {
            // Retransmits are the "wifi is lying to you" signal.
            let (text, color) = match flow.retx_pct {
                Some(v) if v >= 3.0 => (format!("{v:.0}%"), th.crit),
                Some(v) if v >= 0.5 => (format!("{v:.1}%"), th.warn),
                Some(v) => (format!("{v:.1}%"), th.dim),
                None => ("–".into(), th.dim),
            };
            cell(&mut x, text, W_RETX, Style::default().fg(color), false);
        }
        let rate_style = |rate: u64, color| {
            if rate > 0 {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.dim)
            }
        };
        cell(
            &mut x,
            format_bits_per_sec(flow.rx_rate.0),
            W_RX,
            rate_style(flow.rx_rate.0, th.net_rx),
            false,
        );
        cell(
            &mut x,
            format_bits_per_sec(flow.tx_rate.0),
            W_TX,
            rate_style(flow.tx_rate.0, th.net_tx),
            false,
        );
        if show_tot {
            cell(
                &mut x,
                format!("{}/{}", flow.rx_total, flow.tx_total),
                W_TOT,
                Style::default().fg(th.dim),
                false,
            );
        }
    }

    // Scrollbar, same look as the process table: the accent thumb rides
    // the right border, the border line doubling as the track.
    if f.flows.len() > body_h && body_h > 0 {
        let total = f.flows.len() as f32;
        let thumb_h = ((body_h as f32 / total) * body_h as f32).ceil().max(1.0) as usize;
        let thumb_top = ((rs.flows_scroll as f32 / total) * body_h as f32).round() as usize;
        for i in thumb_top..(thumb_top + thumb_h).min(body_h) {
            buf.set_span(
                inner.right(),
                body.y + i as u16,
                &Span::styled("┃", Style::default().fg(th.accent)),
                1,
            );
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
