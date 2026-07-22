//! Memory panel: Activity-Monitor-style breakdown, swap, pressure, history.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{Agg, App};
use crate::collect::mem::Pressure;
use crate::ui::motion::Tier;
use crate::ui::theme::Theme;
use crate::ui::widgets::{LineGraph, Meter, axis_window};

use super::{chrome, chrome_with, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(m) = &app.fast.mem else {
        let inner = chrome(buf, area, "MEMORY", th);
        line(buf, inner, 0, vec![Span::styled("sampling…", dim)]);
        return;
    };

    // Headline: used percent, promoted into the title bar.
    let ratio = m.used_ratio().0;
    let headline = vec![Span::styled(
        format!("{:>3.0}%", ratio * 100.0),
        bold(th.mem.at(ratio)),
    )];
    let inner = chrome_with(buf, area, "MEMORY", headline, th);
    if inner.height == 0 {
        return;
    }

    // Left text is "used / total" ≈ 14 cells; the pressure chip keeps its
    // "pressure" word only when both genuinely fit.
    let spans = vec![
        Span::styled(format!("{:>5}", m.used), bold(th.mem.at(ratio))),
        Span::styled(format!(" / {}", m.total), dim),
    ];
    line(buf, inner, 0, spans);
    let (badge, color) = match m.pressure {
        Pressure::Normal => (" OK ", th.ok),
        Pressure::Warning => (" WARN ", th.warn),
        Pressure::Critical => (" CRIT ", th.crit),
    };
    let mut chip = Vec::new();
    if inner.width >= 34 {
        chip.push(Span::styled("pressure", dim));
        chip.push(Span::raw(" "));
    }
    chip.push(Span::styled(
        badge,
        Style::default()
            .fg(th.bg)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    ));
    line_right(buf, inner, 0, chip);

    if inner.height >= 2 {
        Meter {
            ratio,
            gradient: th.mem,
            track: th.border,
        }
        .render(Rect::new(inner.x, inner.y + 1, inner.width, 1), buf);
    }

    if inner.height >= 3 {
        line(
            buf,
            inner,
            2,
            // Single-space label gaps: with 5-char padded values the row is
            // 44 cells — two-space gaps overflow the 46-cell 5-across panel.
            vec![
                Span::styled("app ", dim),
                Span::styled(format!("{:>5}", m.app), Style::default().fg(th.text)),
                Span::styled(" wired ", dim),
                Span::styled(format!("{:>5}", m.wired), Style::default().fg(th.text)),
                Span::styled(" cmpr ", dim),
                Span::styled(format!("{:>5}", m.compressed), Style::default().fg(th.text)),
                Span::styled(" cache ", dim),
                Span::styled(format!("{:>5}", m.cached), Style::default().fg(th.text)),
            ],
        );
    }
    if inner.height >= 4 {
        let mut spans = vec![
            Span::styled("swap ", dim),
            Span::styled(
                format!("{:>5}", m.swap_used),
                if m.swap_used.0 > 0 {
                    Style::default().fg(th.warn)
                } else {
                    Style::default().fg(th.text)
                },
            ),
        ];
        if m.swap_total.0 > 0 {
            spans.push(Span::styled(format!(" / {}", m.swap_total), dim));
        }
        if let Some(p) = &app.power {
            spans.push(Span::styled("   ram pwr ", dim));
            spans.push(Span::styled(
                format!("{:>5}", p.dram),
                Style::default().fg(th.warn),
            ));
        }
        line(buf, inner, 3, spans);
    }

    // Used-memory history as a line on an axis that hugs the data (5% steps,
    // ≥10% span). Usage sits high and moves by single points, so a filled
    // 0–100% graph is a featureless slab — the zoomed window shows the
    // drift. Line color = absolute used-ratio through the same ramp as the
    // meter above, so the hue still says how full.
    if inner.height > 4 {
        let graph = Rect::new(inner.x, inner.y + 4, inner.width, inner.height - 4);
        let data = app.series(
            &app.hist.mem_used,
            graph.width as usize * 2,
            Agg::Mean,
            Tier::Fast,
        );
        // Axis from the raw bucket span, not the drawn blend — the window
        // holds still while the line drifts (App::series_span).
        let span = app.series_span(&app.hist.mem_used, graph.width as usize * 2, Agg::Mean);
        let window = axis_window(&span, 0.05, 0.10, (0.0, 1.0));
        let (lo, hi) = window.unwrap_or((0.0, 1.0));
        LineGraph {
            data: &data,
            lo,
            hi,
            color: |v| th.mem.at(v),
            baseline: th.border,
        }
        .render(graph, buf);
        if window.is_some() {
            // Axis ticks match the header's whole-percent style.
            let tick = |r: f32| format!("{:.0}%", r * 100.0);
            line(buf, graph, 0, vec![Span::styled(tick(hi), dim)]);
            if graph.height >= 2 {
                line(
                    buf,
                    graph,
                    graph.height - 1,
                    vec![Span::styled(tick(lo), dim)],
                );
            }
        }
    }
}
