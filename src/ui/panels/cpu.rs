//! CPU panel: per-core cluster meters with frequencies, totals, and the main
//! utilization history graph.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{Agg, App};
use crate::ui::theme::Theme;
use crate::ui::widgets::{BrailleGraph, CoreBands, core_bar, stacked_bands};
use crate::units::Mhz;

use super::{chrome_with, line, line_right};

/// Minimum text/graph width worth keeping beside the scaled-up core bands;
/// below it the card falls back to the inline single-row meters.
const STRIP_MIN_BODY: u16 = 40;
/// Label gutter left of the bands ("P0 ") and the gap between the bands and
/// the text/graph body.
const LABEL_W: u16 = 3;
const BAND_GAP: u16 = 2;

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);

    let total = app.hist.cpu_total.latest().unwrap_or(0.0);
    // Headline: total utilization, promoted into the title bar.
    let headline = vec![Span::styled(
        format!("{total:5.1}%"),
        bold(th.cpu.at(total / 100.0)),
    )];
    let inner = chrome_with(buf, area, "CPU", headline, th);
    if inner.height == 0 {
        return;
    }

    // Cluster groups, E-first (per-core loads from host_processor_info,
    // frequencies from IOReport).
    let cores = app
        .fast
        .cpu
        .as_ref()
        .map_or(&[] as &[_], |c| c.per_core.as_slice());
    let e_count = app.soc.ecpu_count.min(cores.len());
    let per_cluster = app.soc.cores_per_pcluster.max(1);
    let mut clusters: Vec<(String, Vec<f32>, Option<Mhz>)> = Vec::new();
    if !cores.is_empty() {
        clusters.push((
            app.soc.tier_low.to_string(),
            cores[..e_count].iter().map(|r| r.0).collect(),
            app.power.as_ref().map(|p| p.ecpu.freq),
        ));
        for (ci, chunk) in cores[e_count..].chunks(per_cluster).enumerate() {
            clusters.push((
                format!("{}{ci}", app.soc.tier_high),
                chunk.iter().map(|r| r.0).collect(),
                app.power.as_ref().map(|p| p.pcpu.freq),
            ));
        }
    }
    let groups: Vec<Vec<f32>> = clusters.iter().map(|(_, loads, _)| loads.clone()).collect();
    let bands = CoreBands {
        groups: &groups,
        gradient: th.cpu,
        baseline: th.border,
    };

    // The classic cluster meters scale up into stacked bands filling the
    // card's full height when it can afford them; text and history keep the
    // rest. Narrow or short cards fold the bars back into single rows.
    let cols = bands.cols();
    let n = clusters.len() as u16;
    let scaled = cols > 0
        && inner.height >= 2 * n
        && inner.width >= LABEL_W + cols + BAND_GAP + STRIP_MIN_BODY;
    let body = if scaled {
        bands.render(
            Rect::new(inner.x + LABEL_W, inner.y, cols, inner.height),
            buf,
        );
        Rect::new(
            inner.x + LABEL_W + cols + BAND_GAP,
            inner.y,
            inner.width - LABEL_W - cols - BAND_GAP,
            inner.height,
        )
    } else {
        inner
    };

    // The total-utilization history is the card's headline signal, so it
    // bleeds across the full inner height. It renders first and the text
    // rows paint over it: set_line only overwrites the cells its spans
    // cover, so the right-aligned fresh data flows around the text block
    // instead of being boxed under it.
    let data = app
        .hist
        .cpu_total
        .buckets(body.width as usize * 2, app.graph_k(), Agg::Max);
    BrailleGraph {
        data: &data,
        max: 100.0,
        gradient: th.cpu,
        baseline: th.border,
    }
    .render(body, buf);

    let cpu_temp = app.temps.as_ref().map(|t| (t.cpu_avg, t.cpu_max));

    // Summary row: cluster frequencies (the total lives in the title bar).
    let mut spans = Vec::new();
    if let Some(p) = &app.power {
        // Fixed-width freqs: "748MHz" ↔ "1.03GHz" must not push the second
        // tier around. Tier letters come from the SoC (E/P; P/S on M5).
        spans.push(Span::styled(
            format!("{} {:>7}", app.soc.tier_low, p.ecpu.freq),
            Style::default().fg(th.accent),
        ));
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("{} {:>7}", app.soc.tier_high, p.pcpu.freq),
            Style::default().fg(th.accent),
        ));
    }
    line(buf, body, 0, spans);
    if let Some((avg, max)) = cpu_temp {
        line_right(
            buf,
            body,
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

    let cluster_stats = |loads: &[f32], freq: Option<Mhz>| {
        let avg: f32 = loads.iter().sum::<f32>() / loads.len().max(1) as f32;
        let mut spans = vec![Span::styled(
            format!(" {:>5.1}%", avg * 100.0),
            Style::default().fg(th.cpu.at(avg)),
        )];
        if let Some(f) = freq {
            spans.push(Span::styled(
                format!(" @ {f:>7}"),
                Style::default().fg(th.accent),
            ));
        }
        spans
    };

    if scaled {
        // One label + stats row per band, on the band's base row — the
        // classic "P0 ▆████▅  53.0%" line, with the bars now towering
        // above it (bands ≥2 rows here, so band 0 clears the summary row).
        for ((y0, bh), (label, loads, freq)) in stacked_bands(inner.height, clusters.len())
            .into_iter()
            .zip(&clusters)
        {
            let row = y0 + bh - 1;
            line(
                buf,
                inner,
                row,
                vec![Span::styled(format!("{label:<3}"), dim)],
            );
            line(buf, body, row, cluster_stats(loads, *freq));
        }
    } else if inner.height > 2 {
        // Single-row cluster meters, exactly the pre-scaled layout.
        for (row, (label, loads, freq)) in (1..inner.height).zip(&clusters) {
            let mut spans = vec![Span::styled(format!("{label:<3}"), dim)];
            for &r in loads {
                let (ch, color) = core_bar(r, th.cpu);
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            }
            spans.extend(cluster_stats(loads, *freq));
            line(buf, inner, row, spans);
        }
    }
}
