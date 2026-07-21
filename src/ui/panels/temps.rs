//! Temperatures panel (overview): CPU/GPU aggregates, history, fans, and the
//! hottest sensors strip. The full sensor list + heat map live in the
//! Thermal view.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::widgets::{BrailleGraph, Meter};

use super::{chrome, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let inner = chrome(buf, area, "TEMPS · FANS", th);
    if inner.height == 0 {
        return;
    }
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(t) = &app.temps else {
        line(buf, inner, 0, vec![Span::styled("sampling…", dim)]);
        return;
    };

    // Narrow panels drop the avg/max detail so the fan block never
    // collides with the temperature text.
    let wide = inner.width >= 56;
    let mut spans = vec![
        Span::styled("CPU ", dim),
        Span::styled(
            format!("{:>4}", t.cpu_avg),
            bold(th.temp_color(t.cpu_avg.0)),
        ),
    ];
    if wide {
        spans.extend([
            Span::styled(" avg ", dim),
            Span::styled(
                format!("{:>4}", t.cpu_max),
                bold(th.temp_color(t.cpu_max.0)),
            ),
            Span::styled(" max", dim),
        ]);
    }
    spans.extend([
        Span::styled(if wide { "   GPU " } else { " GPU " }, dim),
        Span::styled(
            format!("{:>4}", t.gpu_avg),
            bold(th.temp_color(t.gpu_avg.0)),
        ),
    ]);
    line(buf, inner, 0, spans);

    // Fans on the right of row 0 / row 1 (skipped when they'd collide
    // with the temperature text).
    for (i, fan) in t.fans.iter().enumerate() {
        let row = i as u16;
        if row >= inner.height || inner.width < 38 {
            break;
        }
        let ratio = if fan.max_rpm > 0.0 {
            fan.rpm / fan.max_rpm
        } else {
            0.0
        };
        let label_w = 20u16;
        let x = inner.x + inner.width.saturating_sub(label_w);
        buf.set_span(
            x,
            inner.y + row,
            &Span::styled(format!("{:<5}", fan.label), dim),
            6,
        );
        Meter {
            ratio,
            gradient: th.power,
            track: th.border,
        }
        .render(Rect::new(x + 6, inner.y + row, 6, 1), buf);
        buf.set_span(
            x + 13,
            inner.y + row,
            &Span::styled(
                format!("{:>4.0}", fan.rpm),
                Style::default().fg(th.severity(ratio)),
            ),
            7,
        );
    }

    // Hottest non-core sensors strip on row 1 (clipped clear of the fan area).
    if inner.height >= 2 {
        let mut hot: Vec<_> = t
            .sensors
            .iter()
            .filter(|s| {
                use crate::collect::temps::SensorGroup as G;
                !matches!(s.group, G::CpuECore | G::CpuPCore | G::Gpu | G::Soc)
            })
            .collect();
        hot.sort_by(|a, b| b.temp.0.total_cmp(&a.temp.0));
        let budget = inner.width.saturating_sub(21) as usize; // fans live right
        let mut used = 0usize;
        let mut spans = Vec::new();
        for s in hot.iter().take(3) {
            let text_len = s.label.chars().count() + 7;
            if used + text_len > budget {
                break;
            }
            used += text_len;
            spans.push(Span::styled(format!("{} ", s.label), dim));
            spans.push(Span::styled(
                format!("{:>4}  ", s.temp),
                Style::default().fg(th.temp_color(s.temp.0)),
            ));
        }
        line(buf, inner, 1, spans);
    }

    // CPU temperature history (amber ramp: always visible, reads as heat).
    if inner.height > 2 {
        let graph = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
        let data: Vec<f32> = app.hist.cpu_temp.last_n(graph.width as usize * 2).collect();
        BrailleGraph {
            data: &data,
            max: 110.0,
            gradient: th.power,
            baseline: th.border,
        }
        .render(graph, buf);
        line_right(
            buf,
            graph,
            graph.height.saturating_sub(1),
            vec![Span::styled("cpu °c history", Style::default().fg(th.dim))],
        );
    }
}
