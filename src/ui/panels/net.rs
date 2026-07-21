//! Network panel: rates, session totals, a mirrored throughput history
//! (upload up / download down from a shared axis), a connectivity strip fed
//! by the ping prober, and link details. Flat and minimal, Stats-style.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::widgets::MirrorGraph;
use crate::units::Bytes;

use super::{chrome, format_link_speed, line, line_right, split_bits_per_sec};

/// Autoscale floor ≈ 64 Kb/s: idle chatter stays small instead of filling
/// the graph, while anything lighter still lands its minimum dot.
const SCALE_FLOOR: f32 = 8192.0;

/// Probes slower than this color the connectivity strip amber.
const SLOW_MS: f32 = 100.0;

/// Windowed autoscale with the network floor.
pub(crate) fn scale(visible: &[f32]) -> f32 {
    super::windowed_scale(visible, SCALE_FLOOR)
}

/// `16.2ms` / `104ms` style latency, em-dash when unknown.
fn fmt_ms(ms: Option<f32>) -> String {
    match ms {
        Some(v) if v >= 100.0 => format!("{v:.0}ms"),
        Some(v) => format!("{v:.1}ms"),
        None => "–".into(),
    }
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let inner = chrome(buf, area, "NETWORK", th);
    if inner.height == 0 {
        return;
    }
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(n) = &app.fast.net else {
        line(buf, inner, 0, vec![Span::styled("sampling…", dim)]);
        return;
    };

    // Row 0 — rates: bold value, dim unit; link chip (status dot · name ·
    // speed) on the right, dropped before it could overlap. Values pad to 5
    // ("999.9" worst case) and units to 4 ("Kb/s") so nothing shifts as
    // magnitudes change; squeezed panels (5-across mid row) drop the
    // value/unit gap — the row is a constant 26 cells spacious, 23 tight.
    let tight = inner.width < 27;
    let (gap, mid) = if tight {
        ("", " ↑ ")
    } else {
        (" ", "  ↑ ")
    };
    let (rx_v, rx_u) = split_bits_per_sec(n.rx_per_sec.0);
    let (tx_v, tx_u) = split_bits_per_sec(n.tx_per_sec.0);
    line(
        buf,
        inner,
        0,
        vec![
            Span::styled("↓ ", bold(th.net_rx)),
            Span::styled(format!("{rx_v:>5}"), bold(th.net_rx)),
            Span::styled(format!("{gap}{rx_u:<4}"), dim),
            Span::styled(mid, bold(th.net_tx)),
            Span::styled(format!("{tx_v:>5}"), bold(th.net_tx)),
            Span::styled(format!("{gap}{tx_u:<4}"), dim),
        ],
    );
    if inner.width >= 38
        && let Some(p) = &n.primary
    {
        let link = if p.running { th.ok } else { th.crit };
        let mut spans = vec![
            Span::styled("● ", Style::default().fg(link)),
            Span::styled(p.name.clone(), Style::default().fg(th.text)),
        ];
        if p.baudrate > 0 {
            spans.push(Span::styled(
                format!(" {}", format_link_speed(p.baudrate)),
                dim,
            ));
        }
        line_right(buf, inner, 0, spans);
    }

    // Row 1 — session totals left, IP right.
    if inner.height >= 2 {
        line(
            buf,
            inner,
            1,
            vec![
                Span::styled("Σ↓ ", dim),
                Span::styled(
                    format!("{:>5}", Bytes(n.rx_session.0)),
                    Style::default().fg(th.text),
                ),
                Span::styled("  Σ↑ ", dim),
                Span::styled(
                    format!("{:>5}", Bytes(n.tx_session.0)),
                    Style::default().fg(th.text),
                ),
            ],
        );
        if inner.width >= 40
            && let Some(ip) = n.primary.as_ref().and_then(|p| p.ipv4.as_ref())
        {
            line_right(
                buf,
                inner,
                1,
                vec![
                    Span::styled("ip ", dim),
                    Span::styled(ip.clone(), Style::default().fg(th.accent)),
                ],
            );
        }
    }

    // Bottom sections exist only while the prober reports (disabled or
    // failed probing simply hands the rows to the graph).
    let ping = app.ping.as_ref();
    let strip_row = ping.is_some() && inner.height >= 7;
    let stats_row = ping.is_some() && inner.height >= 8;
    let below = u16::from(strip_row) + u16::from(stats_row);

    // Mirrored history graph between the header rows and the ping section.
    if inner.height > 2 + below {
        let graph = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2 - below);
        if graph.height >= 2 {
            let slots = graph.width as usize * 2;
            let tx: Vec<f32> = app.hist.net_tx.last_n(slots).collect();
            let rx: Vec<f32> = app.hist.net_rx.last_n(slots).collect();
            let (tx_max, rx_max) = (scale(&tx), scale(&rx));
            MirrorGraph {
                tx: &tx,
                rx: &rx,
                tx_max,
                rx_max,
                up: th.net_tx,
                down: th.net_rx,
                baseline: th.border,
            }
            .render(graph, buf);
            // Per-side scale labels on the graph's edge rows (fixed-width
            // values so the unit doesn't wander as the autoscale moves).
            if graph.height >= 4 && inner.width >= 24 {
                let (tv, tu) = split_bits_per_sec(tx_max as u64);
                let (rv, ru) = split_bits_per_sec(rx_max as u64);
                line(
                    buf,
                    graph,
                    0,
                    vec![Span::styled(format!("↑ {tv:>5} {tu}"), dim)],
                );
                line(
                    buf,
                    graph,
                    graph.height - 1,
                    vec![Span::styled(format!("↓ {rv:>5} {ru}"), dim)],
                );
            }
        }
    }

    // Connectivity strip: one cell per probe, newest on the right.
    if strip_row {
        let row = inner.height - 1 - u16::from(stats_row);
        let width = inner.width as usize;
        let history: Vec<f32> = app.hist.ping_ms.last_n(width).collect();
        let pad = width - history.len();
        for x in 0..width {
            let color = match x.checked_sub(pad).and_then(|i| history.get(i)) {
                None => th.border, // no probe yet — dim track
                Some(v) if v.is_nan() => th.crit,
                Some(v) if *v > SLOW_MS => th.warn,
                Some(_) => th.ok,
            };
            let cell = &mut buf[(inner.x + x as u16, inner.y + row)];
            cell.set_char('▄');
            cell.set_fg(color);
        }
    }

    // Ping stats row: latency (+ jitter when it fits) on the left; the
    // right-aligned UP/DOWN chip can never be clipped off, with the MAC
    // joining it on genuinely wide panels.
    if stats_row && let Some(p) = ping {
        let row = inner.height - 1;
        let mut spans = vec![
            Span::styled("ping ", dim),
            Span::styled(
                format!("{:>6}", fmt_ms(p.latency_ms)),
                Style::default().fg(th.text),
            ),
        ];
        if inner.width >= 30 {
            spans.push(Span::styled("  jit ", dim));
            spans.push(Span::styled(
                format!("{:>6}", fmt_ms(p.jitter_ms)),
                Style::default().fg(th.text),
            ));
        }
        line(buf, inner, row, spans);

        let (chip, chip_color) = if p.up {
            ("UP", th.ok)
        } else {
            ("DOWN", th.crit)
        };
        let mut right = Vec::new();
        if inner.width >= 50
            && let Some(mac) = n.primary.as_ref().and_then(|p| p.mac.as_ref())
        {
            right.push(Span::styled(mac.clone(), dim));
            right.push(Span::styled("  ", dim));
        }
        right.push(Span::styled(chip, bold(chip_color)));
        line_right(buf, inner, row, right);
    }
}

// Exact scale values in, exact values out — lossless passthrough asserts.
#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::scale;
    use crate::ui::panels::windowed_scale;

    #[test]
    fn net_scale_is_windowed_and_floored() {
        // Empty and light-traffic windows sit on the floor (≈64 Kb/s)…
        assert_eq!(scale(&[]), 8192.0);
        assert_eq!(scale(&[100.0, 4500.0]), 8192.0);
        // …a burst raises the window's scale, and NaN misses are ignored.
        assert_eq!(scale(&[100.0, 9e6]), 9e6);
        assert_eq!(scale(&[f32::NAN, 5e5]), 5e5);
        // The shared helper honors per-panel floors (disk uses 1 MB/s).
        assert_eq!(windowed_scale(&[80_000.0], 1e6), 1e6);
        assert_eq!(windowed_scale(&[4.6e7], 1e6), 4.6e7);
    }
}
