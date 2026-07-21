//! CPU panel: per-core cluster meters with frequencies, totals, and the main
//! utilization history graph.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::widgets::{BrailleGraph, core_bar};
use crate::units::Mhz;

use super::{chrome, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let inner = chrome(buf, area, "CPU", th);
    if inner.height == 0 {
        return;
    }
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);

    let total = app.hist.cpu_total.latest().unwrap_or(0.0);
    let cpu_temp = app.temps.as_ref().map(|t| (t.cpu_avg, t.cpu_max));

    // Summary row.
    let mut spans = vec![
        Span::styled(format!("{total:5.1}%"), bold(th.cpu.at(total / 100.0))),
        Span::styled("  ", dim),
    ];
    if let Some(p) = &app.power {
        // Fixed-width freqs: "748MHz" ↔ "1.03GHz" must not push " · P" around.
        spans.push(Span::styled(
            format!("E {:>7}", p.ecpu.freq),
            Style::default().fg(th.accent),
        ));
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("P {:>7}", p.pcpu.freq),
            Style::default().fg(th.accent),
        ));
    }
    line(buf, inner, 0, spans);
    if let Some((avg, max)) = cpu_temp {
        line_right(
            buf,
            inner,
            0,
            vec![
                Span::styled(
                    format!("{avg:>4}"),
                    Style::default().fg(th.temp_color(avg.0)),
                ),
                Span::styled(" avg · ", dim),
                Span::styled(format!("{max:>4}"), bold(th.temp_color(max.0))),
                Span::styled(" max", dim),
            ],
        );
    }

    // Cluster meter rows: E, P0, P1… (per-core bars from host_processor_info,
    // ordered E-first; frequencies from IOReport).
    let cores = app
        .fast
        .cpu
        .as_ref()
        .map_or(&[] as &[_], |c| c.per_core.as_slice());
    let e_count = app.soc.ecpu_count.min(cores.len());
    let per_cluster = app.soc.cores_per_pcluster.max(1);

    let mut row: u16 = 1;
    let mut draw_cluster =
        |row: u16, label: String, slice: &[crate::units::Ratio], freq: Option<Mhz>| {
            let mut spans = vec![Span::styled(format!("{label:<3}"), dim)];
            for r in slice {
                let (ch, color) = core_bar(r.0, th.cpu);
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            }
            let avg: f32 = slice.iter().map(|r| r.0).sum::<f32>() / slice.len().max(1) as f32;
            spans.push(Span::styled(
                format!(" {:>5.1}%", avg * 100.0),
                Style::default().fg(th.cpu.at(avg)),
            ));
            if let Some(f) = freq {
                spans.push(Span::styled(
                    format!(" @ {f:>7}"),
                    Style::default().fg(th.accent),
                ));
            }
            line(buf, inner, row, spans);
        };

    if !cores.is_empty() && inner.height > 2 {
        draw_cluster(
            row,
            "E".into(),
            &cores[..e_count],
            app.power.as_ref().map(|p| p.ecpu.freq),
        );
        row += 1;
        let pcores = &cores[e_count..];
        for (ci, chunk) in pcores.chunks(per_cluster).enumerate() {
            if row >= inner.height {
                break;
            }
            let freq = app.power.as_ref().map(|p| p.pcpu.freq);
            draw_cluster(row, format!("P{ci}"), chunk, freq);
            row += 1;
        }
    }

    // History graph fills the remainder.
    if row < inner.height {
        let graph_area = Rect::new(inner.x, inner.y + row, inner.width, inner.height - row);
        let data: Vec<f32> = app
            .hist
            .cpu_total
            .last_n(graph_area.width as usize * 2)
            .collect();
        BrailleGraph {
            data: &data,
            max: 100.0,
            gradient: th.cpu,
            baseline: th.border,
        }
        .render(graph_area, buf);
    }
}
