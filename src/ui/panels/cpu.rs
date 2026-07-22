//! CPU panel: per-core cluster meters with frequencies, totals, and the main
//! utilization history graph.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{Agg, App};
use crate::ui::theme::Theme;
use crate::ui::widgets::{BrailleGraph, CoreBands, core_bar, stacked_bands};
use crate::units::{Mhz, Watts};

use super::{chrome_with, line, line_right};

/// Minimum text/graph width worth keeping beside the scaled-up core bands;
/// below it the card falls back to the inline single-row meters.
const STRIP_MIN_BODY: u16 = 40;
/// Body width below which the per-cluster watts column is dropped, so a narrow
/// card truncates nothing and keeps load + frequency intact.
const WATTS_MIN_BODY: u16 = 34;
/// Label gutter left of the bands ("P0 ") and the gap between the bands and
/// the text/graph body.
const LABEL_W: u16 = 3;
const BAND_GAP: u16 = 2;

/// One cluster's row: its label, per-core loads, and the cluster-wide
/// frequency and power when the Energy Model publishes them.
struct Cluster {
    label: String,
    loads: Vec<f32>,
    freq: Option<Mhz>,
    watts: Option<Watts>,
}

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
    // Per-cluster power is the sum of that cluster's own core rails. Only
    // attributed when the Energy Model published exactly as many cores as the
    // scheduler reports — a partial list would silently under-report a
    // cluster, and `None` renders as absent rather than as zero watts.
    let cluster_watts = |slice: &[crate::collect::power::CoreSample]| -> Option<Watts> {
        slice
            .iter()
            .map(|c| c.watts)
            .try_fold(0.0, |acc, w| Some(acc + w?.0))
            .map(Watts)
    };
    let mut clusters: Vec<Cluster> = Vec::new();
    if !cores.is_empty() {
        clusters.push(Cluster {
            label: app.soc.tier_low.to_string(),
            loads: cores[..e_count].iter().map(|r| r.0).collect(),
            freq: app.power.as_ref().map(|p| p.ecpu.freq),
            watts: app
                .power
                .as_ref()
                .and_then(|p| cluster_watts(&p.ecpu.cores)),
        });
        for (ci, chunk) in cores[e_count..].chunks(per_cluster).enumerate() {
            clusters.push(Cluster {
                label: format!("{}{ci}", app.soc.tier_high),
                loads: chunk.iter().map(|r| r.0).collect(),
                freq: app.power.as_ref().map(|p| p.pcpu.freq),
                watts: app.power.as_ref().and_then(|p| {
                    // Cluster ci owns cores [ci*per_cluster, +per_cluster) of
                    // the flat, identity-sorted P list.
                    p.pcpu
                        .cores
                        .get(ci * per_cluster..(ci + 1) * per_cluster)
                        .and_then(cluster_watts)
                }),
            });
        }
    }
    let groups: Vec<Vec<f32>> = clusters.iter().map(|c| c.loads.clone()).collect();
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

    // Watts are the last thing to earn room; a narrow card keeps load and
    // frequency rather than truncating all three.
    let show_watts = body.width >= WATTS_MIN_BODY;
    let cluster_stats = |loads: &[f32], freq: Option<Mhz>, watts: Option<Watts>| {
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
        if let Some(w) = watts.filter(|_| show_watts) {
            spans.push(Span::styled(
                format!(" {w:>6}"),
                Style::default().fg(th.warn),
            ));
        }
        spans
    };

    if scaled {
        // One label + stats row per band, on the band's base row — the
        // classic "P0 ▆████▅  53.0%" line, with the bars now towering
        // above it (bands ≥2 rows here, so band 0 clears the summary row).
        for ((y0, bh), c) in stacked_bands(inner.height, clusters.len())
            .into_iter()
            .zip(&clusters)
        {
            let row = y0 + bh - 1;
            line(
                buf,
                inner,
                row,
                vec![Span::styled(format!("{:<3}", c.label), dim)],
            );
            line(buf, body, row, cluster_stats(&c.loads, c.freq, c.watts));
        }
    } else if inner.height > 2 {
        // Single-row cluster meters, exactly the pre-scaled layout.
        for (row, c) in (1..inner.height).zip(&clusters) {
            let mut spans = vec![Span::styled(format!("{:<3}", c.label), dim)];
            for &r in &c.loads {
                let (ch, color) = core_bar(r, th.cpu);
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            }
            spans.extend(cluster_stats(&c.loads, c.freq, c.watts));
            line(buf, inner, row, spans);
        }
    }
}
