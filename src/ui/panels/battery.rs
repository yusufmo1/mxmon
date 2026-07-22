//! Battery & power-flow panel: charge state, health, and an AlDente-style
//! flow diagram — sources on the left, the system node in the middle,
//! consumers on the right.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::ui::theme::Theme;
use crate::ui::widgets::Meter;
use crate::units::Watts;

use super::{chrome, chrome_with, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(b) = &app.battery else {
        let inner = chrome(buf, area, "BATTERY · FLOW", th);
        line(
            buf,
            inner,
            0,
            vec![Span::styled("no battery (desktop)", dim)],
        );
        return;
    };

    // Headline: charge percent, promoted into the title bar.
    let pct = b.charge.as_percent();
    let charge_color = if pct < 20.0 {
        th.crit
    } else if pct < 40.0 {
        th.warn
    } else {
        th.ok
    };
    let headline = vec![Span::styled(format!("{pct:3.0}%"), bold(charge_color))];
    let inner = chrome_with(buf, area, "BATTERY · FLOW", headline, th);
    if inner.height == 0 {
        return;
    }

    // Row 0: charge meter + state chip.
    let state = if b.charging {
        ("⚡ charging", th.ok)
    } else if b.fully_charged {
        ("✓ full", th.ok)
    } else if b.external_power {
        ("~ AC hold", th.accent)
    } else {
        ("▽ battery", th.warn)
    };
    let meter_w = inner.width.saturating_sub(14).max(4);
    Meter {
        ratio: b.charge.0,
        gradient: crate::ui::theme::Gradient::Solid(charge_color),
        track: th.border,
    }
    .render(Rect::new(inner.x, inner.y, meter_w, 1), buf);
    line_right(buf, inner, 0, vec![Span::styled(state.0, bold(state.1))]);

    // Row 1: stats.
    if inner.height >= 2 {
        let mut spans = vec![
            Span::styled("health ", dim),
            Span::styled(
                format!("{:.0}%", b.health.as_percent()),
                Style::default().fg(th.text),
            ),
            Span::styled("  cycles ", dim),
            Span::styled(b.cycle_count.to_string(), Style::default().fg(th.text)),
            Span::styled("  temp ", dim),
            Span::styled(
                format!("{:>4}", b.temp),
                Style::default().fg(th.temp_color(b.temp.0)),
            ),
        ];
        if let Some(m) = b.minutes_remaining {
            spans.push(Span::styled(
                format!(
                    "  {}h{:02}m {}",
                    m / 60,
                    m % 60,
                    if b.charging { "to full" } else { "left" }
                ),
                dim,
            ));
        }
        if b.external_power
            && let Some(rated) = b.adapter_watts
        {
            spans.push(Span::styled(format!("  adapter {:.0}W", rated.0), dim));
        }
        line(buf, inner, 1, spans);
    }

    // Flow diagram: proportional Sankey when there's room to breathe,
    // otherwise the compact three-row line flow. The Sankey's fixed
    // columns total 34 cells, so 48 still leaves a 14-cell ribbon run —
    // below that the S-curves shred and the compact flow reads better.
    let flow_h = inner.height.saturating_sub(2).min(16);
    if flow_h >= 5 && inner.width >= 48 {
        let flow_area = Rect::new(inner.x, inner.y + 2, inner.width, flow_h);
        sankey(buf, flow_area, app, b, th);
    } else if inner.height >= 5 && inner.width >= 44 {
        let flow_area = Rect::new(inner.x, inner.y + 2, inner.width, 3);
        flow(buf, flow_area, app, b, th);
    }
}

/// The wattages every flow rendering shares. `ac` is actual delivery (SMC
/// PDTR, rated max as fallback), zeroed when unplugged; `bat` is signed
/// (positive = charging); `ram` is the DRAM rail once it earns its own
/// ribbon (see [`earned_sink`]); `bl` is the SMC backlight rail on the
/// same terms; `other` is the SMC system total minus the telemetry we can
/// attribute.
struct FlowW {
    ac: f32,
    bat: f32,
    sys: f32,
    soc: f32,
    disp: f32,
    ram: f32,
    bl: f32,
    other: f32,
}

fn flow_watts(app: &App, b: &crate::collect::battery::BatterySample) -> FlowW {
    let sys = app
        .temps
        .as_ref()
        .and_then(|t| t.sys_power)
        .unwrap_or_else(|| {
            app.power
                .as_ref()
                .map(crate::collect::power::PowerSample::package)
                .unwrap_or_default()
        });
    let soc = app
        .power
        .as_ref()
        .map(crate::collect::power::PowerSample::package)
        .unwrap_or_default();
    let disp = app.power.as_ref().map(|p| p.display).unwrap_or_default();
    let adapter = app
        .temps
        .as_ref()
        .and_then(|t| t.adapter_power)
        .or(b.adapter_watts)
        .unwrap_or_default();
    let recent: Vec<f32> = app.hist.dram_w.last_n(SINK_GATE_WINDOW).collect();
    let ram = earned_sink(&recent);
    let recent: Vec<f32> = app.hist.backlight_w.last_n(SINK_GATE_WINDOW).collect();
    let bl = earned_sink(&recent);
    FlowW {
        ac: if b.external_power { adapter.0 } else { 0.0 },
        bat: b.battery_watts.0,
        sys: sys.0,
        soc: soc.0,
        disp: disp.0,
        ram,
        bl,
        other: (sys.0 - soc.0 - disp.0 - ram - bl).max(0.0),
    }
}

/// Sustained draw that earns an auxiliary rail (RAM, backlight) its own
/// ribbon; below it the rail stays folded into "other".
const SINK_GATE_W: f32 = 4.0;
/// Rail ticks averaged for the gate — enough smoothing that a single
/// spike (or dip) can't flap a ribbon in and out between frames.
const SINK_GATE_WINDOW: usize = 3;

/// An auxiliary rail earns its own ribbon only under sustained draw: the
/// recent ticks must *average* over the gate. Returns the latest wattage
/// to draw, `0.0` while the rail stays folded into "other". Non-finite
/// samples (absent SMC keys, fuzzed rings) are ignored.
fn earned_sink(recent: &[f32]) -> f32 {
    let finite: Vec<f32> = recent.iter().copied().filter(|v| v.is_finite()).collect();
    let Some(&latest) = finite.last() else {
        return 0.0;
    };
    let mean = finite.iter().sum::<f32>() / finite.len() as f32;
    if mean >= SINK_GATE_W {
        latest.max(0.0)
    } else {
        0.0
    }
}

/// Compact-flow column widths (no emoji — cell widths must be exact):
/// the source column, the classic sink column (arrow + label + watts),
/// and each extra sink column (gutter + label + watts). `FLOW_MID_MIN` is
/// the narrowest the SYS node and its run may become before an extra sink
/// column is worth taking.
const FLOW_LEFT_W: u16 = 11;
const FLOW_MID_MIN: u16 = 15;
const SINK_COL0_W: u16 = 16;
const SINK_COL_W: u16 = 14;
/// Rows in the compact flow — also the sink grid's column height.
const FLOW_ROWS: usize = 3;

/// The compact flow's sink-grid plan at `width`, given `rails` earned
/// rails (backlight, RAM) competing for space beyond the classic
/// SoC · DISP · other column. Returns `(extra_columns, rails_named)`:
/// rails past `rails_named` fold back into "other", so the drawn sinks
/// sum to SYS at every width instead of a rail silently vanishing.
fn sink_grid(width: u16, rails: usize) -> (usize, usize) {
    let base = FLOW_LEFT_W + 1 + FLOW_MID_MIN + 1 + SINK_COL0_W + 1;
    let cols = usize::from(width.saturating_sub(base) / SINK_COL_W).min(rails.div_ceil(FLOW_ROWS));
    (cols, (cols * FLOW_ROWS).min(rails))
}

/// Three-row power flow, columns computed from the rect (no emoji — cell
/// widths must be exact):
/// ```text
/// AC  140.0W ─┐               ┌─▶ SoC    26.9W  BKLT    6.3W
///             ├─▶ SYS  63.0W ─┼─▶ DISP    1.2W  RAM     4.5W
/// BAT  +0.0W ─┘               └─▶ other  34.9W
/// ```
/// The sink side is a grid: the classic column always hangs off the
/// bracket, and each further column the width affords names three more
/// earned rails that would otherwise disappear into "other".
fn flow(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    b: &crate::collect::battery::BatterySample,
    th: &Theme,
) {
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);

    let fw = flow_watts(app, b);
    let (sys, soc, disp) = (Watts(fw.sys), Watts(fw.soc), Watts(fw.disp));
    let (adapter, batt) = (Watts(fw.ac), Watts(fw.bat));
    // Earned rails claim the extra sink columns in order; whatever the
    // width can't name folds back into "other" so the totals stay honest.
    let mut rails: Vec<(&str, f32)> = Vec::new();
    if fw.bl > 0.05 {
        rails.push(("BKLT", fw.bl));
    }
    if fw.ram > 0.05 {
        rails.push(("RAM", fw.ram));
    }
    let (extra_cols, named) = sink_grid(area.width, rails.len());
    let other = Watts(fw.other + rails[named..].iter().map(|r| r.1).sum::<f32>());

    let heat = |w: Watts, scale: f32| th.power.at((w.0 / scale).clamp(0.0, 1.0));
    let (y0, y1, y2) = (area.y, area.y + 1, area.y + 2);

    // Column plan: [left 11][junction 1][middle …][junction 1][sink grid]
    let left_w = FLOW_LEFT_W;
    let junction_l = area.x + left_w; // '┐', '├', '┘'
    let sinks_w = SINK_COL0_W + SINK_COL_W * extra_cols as u16;
    let junction_r = area.right().saturating_sub(sinks_w + 2); // '┌', '┼', '└'
    let mid_x = junction_l + 1;

    // Left column: sources, right-aligned watts.
    let adapter_text = if b.external_power {
        format!("{:>6}", adapter.to_string())
    } else {
        "    --".into()
    };
    buf.set_span(area.x, y0, &Span::styled("AC ", dim), 3);
    buf.set_span(
        area.x + 3,
        y0,
        &Span::styled(adapter_text, bold(heat(adapter, 90.0))),
        7,
    );
    buf.set_span(area.x + left_w - 1, y0, &Span::styled("─", dim), 1);
    buf.set_span(area.x, y2, &Span::styled("BAT", dim), 3);
    buf.set_span(
        area.x + 3,
        y2,
        &Span::styled(
            format!("{:>+6.1}W", batt.0),
            bold(if batt.0 < -0.5 { th.warn } else { th.ok }),
        ),
        7,
    );
    buf.set_span(area.x + left_w - 1, y2, &Span::styled("─", dim), 1);

    // Junctions.
    buf.set_span(junction_l, y0, &Span::styled("┐", dim), 1);
    buf.set_span(junction_l, y1, &Span::styled("├", dim), 1);
    buf.set_span(junction_l, y2, &Span::styled("┘", dim), 1);

    // Middle: the system node on row 1, then a run to the right junction.
    let sys_label = format!("─▶ SYS {:>6} ", sys.to_string());
    let sys_w = sys_label.chars().count() as u16;
    buf.set_span(mid_x, y1, &Span::styled("─▶ SYS ", dim), 7);
    buf.set_span(
        mid_x + 7,
        y1,
        &Span::styled(format!("{:>6}", sys.to_string()), bold(heat(sys, 90.0))),
        7,
    );
    for x in (mid_x + sys_w)..junction_r {
        buf.set_span(x, y1, &Span::styled("─", dim), 1);
    }

    // Right junctions + consumers.
    buf.set_span(junction_r, y0, &Span::styled("┌", dim), 1);
    buf.set_span(junction_r, y1, &Span::styled("┼", dim), 1);
    buf.set_span(junction_r, y2, &Span::styled("└", dim), 1);
    let mut sink = |x: u16, y: u16, head: String, head_w: u16, w: Watts, scale: f32| {
        buf.set_span(x, y, &Span::styled(head, dim), head_w);
        buf.set_span(
            x + head_w,
            y,
            &Span::styled(format!("{:>6}", w.to_string()), bold(heat(w, scale))),
            6,
        );
    };
    // The classic column hangs off the bracket…
    let col0 = junction_r + 1;
    for (y, label, w, scale) in [
        (y0, "SoC", soc, 45.0),
        (y1, "DISP", disp, 12.0),
        (y2, "other", other, 30.0),
    ] {
        sink(col0, y, format!("─▶ {label:<6}"), 10, w, scale);
    }
    // …and the earned rails continue the rows, filling column by column.
    for (i, (label, w)) in rails[..named].iter().enumerate() {
        let x = col0 + SINK_COL0_W + SINK_COL_W * (i / FLOW_ROWS) as u16;
        let y = area.y + (i % FLOW_ROWS) as u16;
        sink(x, y, format!("{label:<6}"), 8, Watts(*w), 12.0);
    }
}

/// Blend `c` toward the theme background (ribbon fills stay dim; labels
/// carry the bright ink). Non-RGB colors pass through untouched.
fn mix(bg: Color, c: Color, t: f32) -> Color {
    let (Color::Rgb(r0, g0, b0), Color::Rgb(r1, g1, b1)) = (bg, c) else {
        return c;
    };
    let l = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) as u8;
    Color::Rgb(l(r0, r1), l(g0, g1), l(b0, b1))
}

/// Proportional ribbon thicknesses in half-rows: watts × scale, floored at
/// one half-row for any live flow so a 1 W display ribbon never vanishes,
/// zero for dead flows so they disappear entirely.
fn ribbon_half_rows(watts: &[f32], per_watt: f32) -> Vec<usize> {
    watts
        .iter()
        .map(|&v| {
            if v > 0.05 {
                ((v * per_watt).round() as usize).max(1)
            } else {
                0
            }
        })
        .collect()
}

/// Half-rows reserved between sink blocks when budgeting ribbon thickness:
/// 2 while every live sink can still average ≥2 half-rows of ink alongside
/// that spacing, else 1. On short cards the airy spacing otherwise eats the
/// whole budget and every ribbon collapses to its 1-half-row floor, erasing
/// proportionality — and visual separation comes from the segment spread,
/// not from this reservation.
fn sink_gap(h2: usize, n_sinks: usize) -> usize {
    if h2 >= n_sinks * 4 { 2 } else { 1 }
}

/// AlDente-style proportional Sankey at half-row resolution:
///
/// sources (AC, battery while discharging) merge into the SYS node, SYS
/// splits into spread consumers (SoC, DISP, other, battery while charging).
/// Ribbon thickness scales to watts — the busier side of the graph fills
/// the panel height, so the diagram re-scales itself as load moves — and
/// each ribbon glides along a smoothstep S-curve between its endpoints.
fn sankey(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    b: &crate::collect::battery::BatterySample,
    th: &Theme,
) {
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);

    let w = flow_watts(app, b);
    let (bat_src, bat_sink) = if w.bat < -0.5 {
        (-w.bat, 0.0)
    } else if w.bat > 0.5 {
        (0.0, w.bat)
    } else {
        (0.0, 0.0)
    };
    // Fixed jewel-tone hue per flow (dimming the load-reactive power ramp
    // turned every ribbon the same muddy brown): wall power rides the
    // accent, battery is always green, and each consumer keeps one
    // recognizable color at any wattage.
    let srcs = [("AC", w.ac, th.accent), ("BAT", bat_src, th.ok)];
    let sinks = [
        ("SoC", w.soc, th.title),
        ("DISP", w.disp, th.cpu.at(0.55)),
        ("BKLT", w.bl, th.power.at(0.45)),
        ("RAM", w.ram, th.mem.at(0.65)),
        ("other", w.other, th.net_tx),
        ("BAT", bat_sink, th.ok),
    ];

    // Half-row budget: the busier side of the graph fills the panel.
    let h2 = area.height as usize * 2;
    let live = |v: f32| v > 0.05;
    let n_sinks = sinks.iter().filter(|s| live(s.1)).count().max(1);
    let usable = h2
        .saturating_sub((n_sinks - 1) * sink_gap(h2, n_sinks))
        .max(4);
    let src_total: f32 = srcs.iter().map(|s| s.1).sum();
    let sink_total: f32 = sinks.iter().map(|s| s.1).sum();
    let per_watt = usable as f32 / src_total.max(sink_total).max(1.0);
    let t_src = ribbon_half_rows(&[srcs[0].1, srcs[1].1], per_watt);
    let t_sink = ribbon_half_rows(
        &[
            sinks[0].1, sinks[1].1, sinks[2].1, sinks[3].1, sinks[4].1, sinks[5].1,
        ],
        per_watt,
    );

    // Columns: [src labels][run 1][SYS][run 2][sink labels].
    const SRC_LBL: u16 = 10;
    const SYS_W: u16 = 11;
    const SINK_LBL: u16 = 13;
    let flex = area.width.saturating_sub(SRC_LBL + SYS_W + SINK_LBL);
    let x_run1 = area.x + SRC_LBL;
    let x_sys = x_run1 + flex * 2 / 5;
    let x_run2 = x_sys + SYS_W;
    let x_sink = area.right().saturating_sub(SINK_LBL);

    // Vertical plan. Sinks spread across the height, one per segment;
    // their ribbons leave the SYS node contiguously, top to bottom.
    let sys_h = t_sink.iter().sum::<usize>().min(h2);
    let sys_top = (h2 - sys_h) / 2;
    let mut sink_top = [0usize; 6];
    {
        let seg = h2 / n_sinks;
        let mut si = 0;
        for (i, s) in sinks.iter().enumerate() {
            if live(s.1) {
                let t = t_sink[i];
                sink_top[i] = (si * seg + seg.saturating_sub(t) / 2).min(h2.saturating_sub(t));
                si += 1;
            }
        }
    }
    // Sources: AC rides the upper quarter, battery the lower; solo sources
    // center on the SYS node. Their ribbons arrive at SYS contiguously.
    let arr_h = t_src.iter().sum::<usize>().min(h2);
    let arr_top = (h2 - arr_h) / 2;
    let both = t_src[0] > 0 && t_src[1] > 0;
    let src_top = [
        if both {
            (h2 / 4).saturating_sub(t_src[0] / 2)
        } else {
            (h2 - t_src[0]) / 2
        },
        if both {
            (h2 * 3 / 4).saturating_sub(t_src[1] / 2)
        } else {
            (h2.saturating_sub(t_src[1])) / 2
        },
    ];

    // Rasterize into a half-row color grid, then blit as ▀▄█ pairs.
    let gw = area.width as usize;
    let mut grid: Vec<Option<Color>> = vec![None; gw * h2];
    let band = |grid: &mut Vec<Option<Color>>,
                x0: u16,
                x1: u16,
                top_a: usize,
                top_b: usize,
                t: usize,
                color: Color| {
        if t == 0 || x1 <= x0 {
            return;
        }
        let span = f32::from(x1 - x0 - 1).max(1.0);
        for x in x0..x1 {
            let k = f32::from(x - x0) / span;
            let s = k * k * (3.0 - 2.0 * k); // smoothstep
            let top = (top_a as f32 + (top_b as f32 - top_a as f32) * s).round() as usize;
            let top = top.min(h2.saturating_sub(t));
            for hy in top..(top + t).min(h2) {
                grid[hy * gw + (x - area.x) as usize] = Some(color);
            }
        }
    };

    // Thin ribbons (≤2 half-rows) route straight at their destination
    // height — S-curving a hairline across cell rows shreds it into
    // disconnected dashes.
    let mut off = arr_top;
    for (i, (_, _, ink)) in srcs.iter().enumerate() {
        let t = t_src[i];
        let arrive = if t <= 2 { src_top[i] } else { off };
        band(
            &mut grid,
            x_run1,
            x_sys,
            src_top[i],
            arrive,
            t,
            mix(th.bg, *ink, 0.30),
        );
        off += t;
    }
    let mut departs = [0usize; 6];
    let mut off = sys_top;
    for (i, (_, _, ink)) in sinks.iter().enumerate() {
        let t = t_sink[i];
        let depart = if t <= 2 { sink_top[i] } else { off };
        departs[i] = depart;
        band(
            &mut grid,
            x_run2,
            x_sink,
            depart,
            sink_top[i],
            t,
            mix(th.bg, *ink, 0.30),
        );
        off += t;
    }
    if sys_h > 0 {
        for x in x_sys..x_run2 {
            for hy in sys_top..(sys_top + sys_h).min(h2) {
                grid[hy * gw + (x - area.x) as usize] = Some(th.selection_bg);
            }
        }
    }
    // Bright node caps where ribbons land, so each flow visibly plugs
    // into its consumer instead of fading out before the label.
    for (i, (_, v, ink)) in sinks.iter().enumerate() {
        if !live(*v) {
            continue;
        }
        for x in x_sink.saturating_sub(2)..x_sink {
            for hy in sink_top[i]..(sink_top[i] + t_sink[i]).min(h2) {
                grid[hy * gw + (x - area.x) as usize] = Some(mix(th.bg, *ink, 0.85));
            }
        }
    }

    for cy in 0..area.height {
        for cx in 0..gw {
            let top = grid[(cy as usize * 2) * gw + cx];
            let bottom = grid[(cy as usize * 2 + 1) * gw + cx];
            let cell = &mut buf[(area.x + cx as u16, area.y + cy)];
            match (top, bottom) {
                (Some(a), Some(bt)) if a == bt => {
                    cell.set_char('█');
                    cell.set_fg(a);
                    cell.set_bg(bt);
                }
                (Some(a), Some(bt)) => {
                    cell.set_char('▀');
                    cell.set_fg(a);
                    cell.set_bg(bt);
                }
                (Some(a), None) => {
                    cell.set_char('▀');
                    cell.set_fg(a);
                    cell.set_bg(th.bg);
                }
                (None, Some(bt)) => {
                    cell.set_char('▄');
                    cell.set_fg(bt);
                    cell.set_bg(th.bg);
                }
                (None, None) => {}
            }
        }
    }

    // Labels over the ink. Sources left, SYS centered on its node, sinks
    // right — a label's row tracks its ribbon's vertical center.
    let row_of = |top: usize, t: usize| area.y + (usize::midpoint(top, t.max(1) / 2)) as u16;
    let src_label = |buf: &mut Buffer, y: u16, name: &str, text: String, ink: Color| {
        let s = format!("{name:<3} {text:>5} ");
        let x = area.x;
        buf.set_span(x, y, &Span::styled(format!("{name:<3} "), dim), 4);
        buf.set_span(x + 4, y, &Span::styled(s[4..].to_string(), bold(ink)), 6);
    };
    if w.ac > 0.05 {
        src_label(
            buf,
            row_of(src_top[0], t_src[0]),
            "AC",
            format!("{:.1}W", w.ac),
            th.warn,
        );
    } else if !b.external_power {
        buf.set_span(
            area.x,
            row_of(src_top[0], t_src[0]),
            &Span::styled("AC     --", dim),
            9,
        );
    }
    let bat_row = if t_src[1] > 0 {
        row_of(src_top[1], t_src[1])
    } else if both || t_src[0] == 0 {
        area.y + (area.height * 3 / 4).min(area.height - 1)
    } else {
        area.bottom() - 1
    };
    if bat_sink < 0.05 {
        src_label(
            buf,
            bat_row,
            "BAT",
            format!("{:+.1}W", w.bat),
            if w.bat < -0.5 { th.warn } else { th.ok },
        );
    }
    if sys_h > 0 {
        let y = area.y + usize::midpoint(sys_top, sys_h / 2) as u16;
        let text = format!("SYS {:.1}W", w.sys);
        let x = x_sys + (SYS_W.saturating_sub(text.chars().count() as u16)) / 2;
        buf.set_span(x, y, &Span::styled(text, bold(th.text)), SYS_W);
    }
    for (i, (name, v, ink)) in sinks.iter().enumerate() {
        if !live(*v) {
            continue;
        }
        let y = row_of(sink_top[i], t_sink[i]);
        let text = if *name == "BAT" {
            format!("{v:+.1}W")
        } else {
            format!("{v:.1}W")
        };
        // Thick ribbons carry their wattage mid-curve (the reference look);
        // the right column then only needs the name. Thin ones keep the
        // number in the column where it always fits.
        let run_w = x_sink.saturating_sub(x_run2);
        let on_ribbon = t_sink[i] >= 6 && run_w >= 18 && *name != "BAT";
        if on_ribbon {
            let x_mid = u16::midpoint(x_run2, x_sink);
            let top_mid = usize::midpoint(departs[i], sink_top[i]);
            let ry = area.y + (usize::midpoint(top_mid, t_sink[i] / 2)) as u16;
            let x = x_mid.saturating_sub(text.chars().count() as u16 / 2);
            for (k, ch) in text.chars().enumerate() {
                let cell = &mut buf[(x + k as u16, ry)];
                cell.set_char(ch);
                cell.set_fg(mix(th.bg, *ink, 0.95));
                cell.set_style(Style::new().add_modifier(Modifier::BOLD));
            }
        }
        buf.set_span(x_sink + 1, y, &Span::styled(format!("{name:<6}"), dim), 6);
        if !on_ribbon {
            buf.set_span(
                x_sink + 7,
                y,
                &Span::styled(format!("{text:>6}"), bold(*ink)),
                6,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ribbons_scale_floor_and_vanish() {
        // 40 half-rows over 40 W → 1 half-row per watt.
        let t = ribbon_half_rows(&[28.0, 2.82, 9.0, 0.0], 1.0);
        assert_eq!(t, vec![28, 3, 9, 0]);
        // A live sliver never vanishes; a dead flow always does.
        let t = ribbon_half_rows(&[0.4, 0.04], 1.0);
        assert_eq!(t, vec![1, 0]);
        // Proportionality holds under scaling.
        let t = ribbon_half_rows(&[30.0, 15.0], 0.5);
        assert_eq!(t, vec![15, 8]);
    }

    #[test]
    fn sink_grid_spends_spare_width_on_earned_rails() {
        // The classic column is all a narrow flow can hold: both rails
        // fold back into "other" rather than being dropped silently.
        assert_eq!(sink_grid(44, 2), (0, 0));
        assert_eq!(sink_grid(58, 2), (0, 0), "one cell short of a column");
        // One extra column names up to FLOW_ROWS rails.
        assert_eq!(sink_grid(59, 2), (1, 2));
        assert_eq!(sink_grid(59, 1), (1, 1));
        assert_eq!(sink_grid(200, 2), (1, 2), "no empty columns past need");
        // No rails means no extra columns, however wide the panel.
        assert_eq!(sink_grid(300, 0), (0, 0));
        // A fourth rail would need a second extra column, and only takes
        // one when the width is actually there.
        assert_eq!(sink_grid(72, 4), (1, 3));
        assert_eq!(sink_grid(73, 4), (2, 4));
        // Degenerate widths saturate instead of underflowing.
        assert_eq!(sink_grid(0, 2), (0, 0));
    }

    #[test]
    fn sink_gap_spends_spacing_on_ink_when_short() {
        assert_eq!(sink_gap(16, 4), 2, "tall flow keeps the airy spacing");
        assert_eq!(sink_gap(10, 4), 1, "short flow buys ribbon ink instead");
        assert_eq!(sink_gap(12, 3), 2);
        assert_eq!(sink_gap(10, 3), 1);
        assert_eq!(sink_gap(4, 1), 2, "a lone sink has no gaps to fund");
    }

    #[test]
    #[allow(clippy::float_cmp)] // gate outputs are exact pass-throughs
    fn earned_sink_gates_on_the_smoothed_window() {
        assert_eq!(earned_sink(&[]), 0.0);
        assert_eq!(earned_sink(&[5.0]), 5.0, "a single hot sample gates in");
        assert_eq!(earned_sink(&[1.0, 1.2, 0.9]), 0.0, "idle stays folded");
        // One spike can't flap the ribbon in…
        assert_eq!(earned_sink(&[1.0, 1.0, 9.0]), 0.0);
        // …and one dip can't flap it out (the mean holds the gate open;
        // the latest value is what's drawn).
        assert_eq!(earned_sink(&[6.0, 6.0, 3.0]), 3.0);
        // Non-finite ring data is ignored, never propagated — an absent
        // SMC key (desktops) fills the ring with NaN and never gates in.
        assert_eq!(earned_sink(&[f32::NAN, 5.0]), 5.0);
        assert_eq!(earned_sink(&[f32::NAN]), 0.0);
        assert_eq!(earned_sink(&[f32::INFINITY, 5.0]), 5.0);
        // Hostile negatives clamp instead of drawing an anti-ribbon.
        assert_eq!(earned_sink(&[9.0, 9.0, -2.0]), 0.0);
    }

    #[test]
    #[allow(clippy::float_cmp)] // an ungated rail is exactly 0.0
    fn flow_watts_carves_ram_out_of_other_when_gated() {
        use crate::collect::sampler::Update;

        let b = crate::testutil::battery();
        // Identical apps except for the DRAM rail: sustained pressure on
        // one, idle on the other.
        let mut hot = crate::testutil::app();
        let mut cold = crate::testutil::app();
        for _ in 0..SINK_GATE_WINDOW {
            let mut p = crate::testutil::power_at(0);
            p.dram = Watts(6.0);
            hot.apply(Update::Power(Box::new(p)));
            let mut p = crate::testutil::power_at(0);
            p.dram = Watts(1.0);
            cold.apply(Update::Power(Box::new(p)));
        }
        let (h, c) = (flow_watts(&hot, &b), flow_watts(&cold, &b));
        assert_eq!(c.ram, 0.0, "under the gate RAM stays inside other");
        assert!((h.ram - 6.0).abs() < 1e-3);
        // The carve-out comes exactly out of "other" — nothing double
        // counted, nothing lost (sys/soc/disp are identical between apps).
        assert!(((c.other - h.other) - 6.0).abs() < 1e-3);
    }

    #[test]
    #[allow(clippy::float_cmp)] // an ungated rail is exactly 0.0
    fn flow_watts_carves_backlight_out_of_other_when_gated() {
        use crate::collect::sampler::SlowSnapshot;
        use crate::collect::sampler::Update;

        let b = crate::testutil::battery();
        let mut hot = crate::testutil::app();
        let mut cold = crate::testutil::app();
        for i in 0..SINK_GATE_WINDOW {
            let mut t = crate::testutil::temps_at(i);
            t.backlight_power = Some(Watts(6.3));
            hot.apply(Update::Slow(Box::new(SlowSnapshot {
                temps: Some(t),
                battery: None,
            })));
            let mut t = crate::testutil::temps_at(i);
            t.backlight_power = None; // desktop: key absent, ring gets NaN
            cold.apply(Update::Slow(Box::new(SlowSnapshot {
                temps: Some(t),
                battery: None,
            })));
        }
        let (h, c) = (flow_watts(&hot, &b), flow_watts(&cold, &b));
        assert_eq!(c.bl, 0.0, "an absent rail never earns a ribbon");
        assert!((h.bl - 6.3).abs() < 1e-3);
        assert!(
            h.other < c.other,
            "the carve-out comes out of other, not thin air"
        );
    }

    #[test]
    fn mix_blends_toward_bg_and_passes_indexed() {
        let bg = Color::Rgb(10, 10, 15);
        let Color::Rgb(r, g, b) = mix(bg, Color::Rgb(210, 10, 115), 0.5) else {
            panic!("rgb in, rgb out");
        };
        assert_eq!((r, g, b), (110, 10, 65));
        assert_eq!(mix(bg, Color::Indexed(203), 0.5), Color::Indexed(203));
    }
}
