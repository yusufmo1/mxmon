//! Memory panel: Activity-Monitor-style breakdown, swap, pressure, history.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::collect::mem::Pressure;
use crate::ui::theme::Theme;
use crate::ui::widgets::{BrailleGraph, Meter};

use super::{chrome, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let inner = chrome(buf, area, "MEMORY", th);
    if inner.height == 0 {
        return;
    }
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(m) = &app.fast.mem else {
        line(buf, inner, 0, vec![Span::styled("sampling…", dim)]);
        return;
    };

    // Left text is "used / total  pct" ≈ 20 cells; the pressure chip keeps
    // its "pressure" word only when both genuinely fit, and squeezed panels
    // drop the percent so the bare badge never overwrites the total.
    let ratio = m.used_ratio().0;
    let mut spans = vec![
        Span::styled(format!("{:>5}", m.used), bold(th.mem.at(ratio))),
        Span::styled(format!(" / {}", m.total), dim),
    ];
    if inner.width >= 27 {
        spans.push(Span::styled(
            format!("  {:>3.0}%", ratio * 100.0),
            Style::default().fg(th.mem.at(ratio)),
        ));
    }
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

    if inner.height > 4 {
        let graph = Rect::new(inner.x, inner.y + 4, inner.width, inner.height - 4);
        let data: Vec<f32> = app.hist.mem_used.last_n(graph.width as usize * 2).collect();
        BrailleGraph {
            data: &data,
            max: 1.0,
            gradient: th.mem,
            baseline: th.border,
        }
        .render(graph, buf);
    }
}
