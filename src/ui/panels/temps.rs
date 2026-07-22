//! Temperatures panel (overview): CPU/GPU aggregates, history, fans, and the
//! hottest sensors strip. The full sensor list + heat map live in the
//! Thermal view.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::widgets::{LineGraph, Meter, axis_window};
use crate::units::Celsius;

use super::{chrome, chrome_with, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(t) = &app.temps else {
        let inner = chrome(buf, area, "TEMPS · FANS", th);
        line(buf, inner, 0, vec![Span::styled("sampling…", dim)]);
        return;
    };

    // Headline: the hottest CPU sensor — the number people watch.
    let headline = vec![
        Span::styled(
            format!("{:>4}", t.cpu_max),
            bold(th.temp_color(t.cpu_max.0)),
        ),
        Span::styled(" max", dim),
    ];
    let inner = chrome_with(buf, area, "TEMPS · FANS", headline, th);
    if inner.height == 0 {
        return;
    }

    // Averages row (max lives in the title bar; the fan block sits right).
    let spans = vec![
        Span::styled("CPU ", dim),
        Span::styled(
            format!("{:>4}", t.cpu_avg),
            bold(th.temp_color(t.cpu_avg.0)),
        ),
        Span::styled(" GPU ", dim),
        Span::styled(
            format!("{:>4}", t.gpu_avg),
            bold(th.temp_color(t.gpu_avg.0)),
        ),
    ];
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

    // CPU + GPU temperature history as lines on an axis that hugs the data
    // (5° steps, ≥10° span). A die temp never nears 0, so a fixed 0–110°
    // axis renders as a featureless slab — the zoomed window is what makes
    // the movement visible. Line color = absolute temp through the shared
    // thermal ramp, so red still means hot regardless of zoom; the GPU
    // trace stays dim so the panels' lead series is unmistakable.
    if inner.height > 2 {
        let graph = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
        let slots = graph.width as usize * 2;
        let cpu: Vec<f32> = app.hist.cpu_temp.last_n(slots).collect();
        let gpu: Vec<f32> = app.hist.gpu_temp.last_n(slots).collect();
        // One window across both series so the lines share a scale.
        let both: Vec<f32> = cpu.iter().chain(gpu.iter()).copied().collect();
        let window = axis_window(&both, 5.0, 10.0, (0.0, 110.0));
        let (lo, hi) = window.unwrap_or((0.0, 1.0));
        LineGraph {
            data: &gpu,
            lo,
            hi,
            color: |_| th.dim,
            baseline: th.border,
        }
        .render(graph, buf);
        LineGraph {
            data: &cpu,
            lo,
            hi,
            color: |v| th.temp_color(v),
            baseline: th.border,
        }
        .render(graph, buf);
        if window.is_some() {
            line(
                buf,
                graph,
                0,
                vec![Span::styled(format!("{}", Celsius(hi)), dim)],
            );
            if graph.height >= 2 {
                let label = vec![Span::styled(format!("{}", Celsius(lo)), dim)];
                line(buf, graph, graph.height - 1, label);
            }
        }
        line_right(
            buf,
            graph,
            graph.height.saturating_sub(1),
            vec![
                Span::styled("cpu", Style::default().fg(th.temp_color(t.cpu_avg.0))),
                Span::styled(" · ", dim),
                Span::styled("gpu °c", dim),
            ],
        );
    }
}
