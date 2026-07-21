//! Disk panel: mirrored R/W throughput history (writes up / reads down from
//! a shared axis, like the network panel), IOPS, and real per-op device
//! latency (the number iostat calls "ms/t") from the block-storage drivers.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::widgets::MirrorGraph;

use super::{chrome, line, line_right, split_bytes_per_sec, windowed_scale};

/// Autoscale floor (1 MB/s): background chatter stays low instead of
/// filling the graph, while a light trickle still lands its minimum dot.
const SCALE_FLOOR: f32 = 1e6;

/// Microseconds shown compactly: "95µ", "1.2m", "40m".
fn fmt_lat(us: Option<f32>) -> String {
    match us {
        None => "–".into(),
        Some(us) if us >= 10_000.0 => format!("{:.0}m", us / 1000.0),
        Some(us) if us >= 1000.0 => format!("{:.1}m", us / 1000.0),
        Some(us) => format!("{us:.0}µ"),
    }
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let inner = chrome(buf, area, "DISK", th);
    if inner.height == 0 {
        return;
    }
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(d) = &app.fast.disk else {
        line(buf, inner, 0, vec![Span::styled("sampling…", dim)]);
        return;
    };

    // Fixed-width values ("999.9" worst case) so the W half, the Σ chip and
    // the graph edge never shift as rates cross unit boundaries. Squeezed
    // panels drop the value/unit gap: 25 cells spacious, 23 tight.
    let gap = if inner.width < 26 { "" } else { " " };
    let (rv, ru) = split_bytes_per_sec(d.read_per_sec.0);
    let (wv, wu) = split_bytes_per_sec(d.write_per_sec.0);
    line(
        buf,
        inner,
        0,
        vec![
            Span::styled("R ", bold(th.accent)),
            Span::styled(format!("{rv:>5}"), bold(th.accent)),
            Span::styled(format!("{gap}{ru:<4}"), dim),
            Span::styled(" W ", bold(th.warn)),
            Span::styled(format!("{wv:>5}"), bold(th.warn)),
            Span::styled(format!("{gap}{wu:<4}"), dim),
        ],
    );
    // Session totals fit only on roomier panels — the rates row is a fixed
    // 25 cells, the Σ suffix ~13.
    if inner.width >= 38 {
        line_right(
            buf,
            inner,
            0,
            vec![
                Span::styled("Σ ", dim),
                Span::styled(
                    format!("{:>5}/{:>5}", d.read_session, d.write_session),
                    Style::default().fg(th.text),
                ),
            ],
        );
    }

    if inner.height >= 2 {
        let mut spans = vec![
            Span::styled("lat ", dim),
            Span::styled(
                format!(
                    "{:>4}/{:>4}",
                    fmt_lat(d.read_lat_us),
                    fmt_lat(d.write_lat_us)
                ),
                Style::default().fg(th.text),
            ),
        ];
        // lat is 13 cells, "  iops " + two padded counts is 16 more.
        if inner.width >= 30 {
            spans.push(Span::styled("  iops ", dim));
            spans.push(Span::styled(
                format!("{:>4}/{:>4}", d.read_iops, d.write_iops),
                Style::default().fg(th.text),
            ));
        }
        line(buf, inner, 1, spans);
    }

    // Mirrored history: writes grow up (outbound, like network upload),
    // reads hang down, each side autoscaled to its visible window.
    if inner.height > 2 {
        let graph = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
        if graph.height >= 2 {
            let slots = graph.width as usize * 2;
            let wr: Vec<f32> = app.hist.disk_wr.last_n(slots).collect();
            let rd: Vec<f32> = app.hist.disk_rd.last_n(slots).collect();
            let (wr_max, rd_max) = (
                windowed_scale(&wr, SCALE_FLOOR),
                windowed_scale(&rd, SCALE_FLOOR),
            );
            MirrorGraph {
                tx: &wr,
                rx: &rd,
                tx_max: wr_max,
                rx_max: rd_max,
                up: th.warn,
                down: th.accent,
                baseline: th.border,
            }
            .render(graph, buf);
            // Per-side scale labels in the panel's R/W vocabulary.
            if graph.height >= 4 && inner.width >= 24 {
                let (wv, wu) = split_bytes_per_sec(wr_max as u64);
                let (rv, ru) = split_bytes_per_sec(rd_max as u64);
                line(
                    buf,
                    graph,
                    0,
                    vec![Span::styled(format!("W {wv:>5} {wu}"), dim)],
                );
                line(
                    buf,
                    graph,
                    graph.height - 1,
                    vec![Span::styled(format!("R {rv:>5} {ru}"), dim)],
                );
            }
        }
    }
}
