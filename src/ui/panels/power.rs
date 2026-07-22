//! Power panel: package watts with history, per-rail meters and peaks.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{Agg, App, Ring};
use crate::ui::theme::Theme;
use crate::ui::widgets::{BrailleGraph, Meter};
use crate::units::Watts;

use super::{chrome, chrome_with, line, line_right, windowed_scale};

/// Package-graph autoscale floor: idle draw (~2 W) reads low but visible,
/// and the scale relaxes again once a burst leaves the visible window.
const PKG_FLOOR: f32 = 15.0;
/// Rail meters normalize against the recent peak instead of the all-time
/// session max, so one old burst doesn't flatten them for the whole run.
const RAIL_FLOOR: f32 = 20.0;
/// Power-tier samples in the rail-peak window (≈ 60 s at default cadence).
const RAIL_PEAK_WINDOW: usize = 120;

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let dim = Style::default().fg(th.dim);
    let Some(p) = &app.power else {
        let inner = chrome(buf, area, "POWER", th);
        line(buf, inner, 0, vec![Span::styled("sampling…", dim)]);
        return;
    };

    let pkg = p.package();
    let sys = app.temps.as_ref().and_then(|t| t.sys_power);
    let pkg_peak = app.hist.package_w.max();

    // Headline: total system draw (the SMC wall number), package when the
    // SMC rail is unavailable — labeled so the two never read as each other.
    let (hl_label, hl_watts) = match sys {
        Some(sys) => ("SYS ", sys),
        None => ("PKG ", pkg),
    };
    let headline = vec![
        Span::styled(hl_label, dim),
        Span::styled(
            format!("{hl_watts:>6}"),
            Style::default().fg(th.warn).add_modifier(Modifier::BOLD),
        ),
    ];
    let inner = chrome_with(buf, area, "POWER", headline, th);
    if inner.height == 0 {
        return;
    }

    line(
        buf,
        inner,
        0,
        vec![
            Span::styled("PKG ", dim),
            Span::styled(
                format!("{pkg:>6}"),
                Style::default()
                    .fg(th.power.at((pkg.0 / 60.0).min(1.0)))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  peak {pkg_peak:>4.1}W"), dim),
        ],
    );

    // Package history graph (3 rows if space allows).
    let graph_h = if inner.height >= 10 {
        3
    } else if inner.height >= 8 {
        2
    } else {
        0
    };
    if graph_h > 0 {
        let graph = Rect::new(inner.x, inner.y + 1, inner.width, graph_h);
        let data = app
            .hist
            .package_w
            .buckets(graph.width as usize * 2, app.graph_k(), Agg::Max);
        // Scale to the visible window (the all-time session peak already
        // lives in the header text).
        BrailleGraph {
            data: &data,
            max: windowed_scale(&data, PKG_FLOOR),
            gradient: th.power,
            baseline: th.border,
        }
        .render(graph, buf);
    }

    // Rails.
    let rails: [(&str, Watts, &Ring); 5] = [
        ("CPU", p.cpu, &app.hist.cpu_w),
        ("GPU", p.gpu, &app.hist.gpu_w),
        ("ANE", p.ane, &app.hist.ane_w),
        ("RAM", p.dram, &app.hist.dram_w),
        ("DSP", p.display, &app.hist.disp_w),
    ];
    let base_row = 1 + graph_h;
    for (i, (label, watts, ring)) in rails.into_iter().enumerate() {
        let row = base_row + i as u16;
        if row >= inner.height {
            break;
        }
        let peak = ring.last_n(RAIL_PEAK_WINDOW).fold(RAIL_FLOOR, f32::max);
        let ratio = (watts.0 / peak).clamp(0.0, 1.0);
        let meter_w = inner.width.saturating_sub(18).max(4);
        line(
            buf,
            inner,
            row,
            vec![Span::styled(format!("{label:<4}"), dim)],
        );
        Meter {
            ratio,
            gradient: th.power,
            track: th.border,
        }
        .render(Rect::new(inner.x + 4, inner.y + row, meter_w, 1), buf);
        line_right(
            buf,
            inner,
            row,
            vec![Span::styled(
                format!("{watts:>6}"),
                Style::default()
                    .fg(th.power.at(ratio))
                    .add_modifier(Modifier::BOLD),
            )],
        );
    }
}
