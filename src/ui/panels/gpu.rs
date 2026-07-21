//! GPU panel: device utilization graph, frequency, temperature, sub-units.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::widgets::{BrailleGraph, Meter};

use super::{chrome, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let inner = chrome(buf, area, "GPU", th);
    if inner.height == 0 {
        return;
    }
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);

    let device = app.fast.gpu.as_ref().map_or(0.0, |g| g.device.0);
    let mut spans = vec![Span::styled(
        format!("{:5.1}%", device * 100.0),
        bold(th.gpu.at(device)),
    )];
    if let Some(p) = &app.power {
        spans.push(Span::styled(
            format!("  @ {:>7}", p.gpu_freq),
            Style::default().fg(th.accent),
        ));
        spans.push(Span::styled(
            format!("  {:>5}", p.gpu),
            Style::default().fg(th.warn),
        ));
    }
    line(buf, inner, 0, spans);
    // The left text is a fixed 24 cells (pct 6 + freq 11 + watts 7); the
    // right temp needs 5 more or it overwrites the wattage.
    if inner.width >= 30
        && let Some(t) = &app.temps
        && t.gpu_avg.0 > 0.0
    {
        line_right(
            buf,
            inner,
            0,
            vec![Span::styled(
                format!("{:>4}", t.gpu_avg),
                bold(th.temp_color(t.gpu_avg.0)),
            )],
        );
    }

    // Sub-unit meters + memory line (bottom rows), graph in between.
    let mut detail_rows: u16 = 0;
    if let Some(g) = &app.fast.gpu {
        detail_rows = if inner.height >= 6 {
            2
        } else {
            u16::from(inner.height >= 4)
        };
        if detail_rows >= 1 {
            let row = inner.height - detail_rows;
            let half = inner.width / 2;
            let meter_w = half.saturating_sub(10).max(3);
            line(buf, inner, row, vec![Span::styled("rend ", dim)]);
            Meter {
                ratio: g.renderer.0,
                gradient: th.gpu,
                track: th.border,
            }
            .render(Rect::new(inner.x + 5, inner.y + row, meter_w, 1), buf);
            let tiler_x = inner.x + half + 1;
            buf.set_span(tiler_x, inner.y + row, &Span::styled("tile ", dim), 5);
            Meter {
                ratio: g.tiler.0,
                gradient: th.gpu,
                track: th.border,
            }
            .render(Rect::new(tiler_x + 5, inner.y + row, meter_w, 1), buf);
        }
        if detail_rows == 2 {
            let row = inner.height - 1;
            line(
                buf,
                inner,
                row,
                vec![
                    Span::styled("mem ", dim),
                    Span::styled(
                        format!("{:>5}", g.used_memory),
                        Style::default().fg(th.text),
                    ),
                ],
            );
            if let Some(p) = &app.power {
                line_right(
                    buf,
                    inner,
                    row,
                    vec![Span::styled(
                        format!("active {:>3.0}%", p.gpu_active.as_percent()),
                        dim,
                    )],
                );
            }
        }
    }

    let graph_top: u16 = 1;
    if inner.height > graph_top + detail_rows {
        let graph = Rect::new(
            inner.x,
            inner.y + graph_top,
            inner.width,
            inner.height - graph_top - detail_rows,
        );
        let data: Vec<f32> = app.hist.gpu.last_n(graph.width as usize * 2).collect();
        BrailleGraph {
            data: &data,
            max: 1.0,
            gradient: th.gpu,
            baseline: th.border,
        }
        .render(graph, buf);
    }
}
