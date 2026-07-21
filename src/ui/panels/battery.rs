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

use super::{chrome, line, line_right};

pub fn render(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    let inner = chrome(buf, area, "BATTERY · FLOW", th);
    if inner.height == 0 {
        return;
    }
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let Some(b) = &app.battery else {
        line(
            buf,
            inner,
            0,
            vec![Span::styled("no battery (desktop)", dim)],
        );
        return;
    };

    // Row 0: charge meter + %.
    let pct = b.charge.as_percent();
    let charge_color = if pct < 20.0 {
        th.crit
    } else if pct < 40.0 {
        th.warn
    } else {
        th.ok
    };
    let state = if b.charging {
        ("⚡ charging", th.ok)
    } else if b.fully_charged {
        ("✓ full", th.ok)
    } else if b.external_power {
        ("~ AC hold", th.accent)
    } else {
        ("▽ battery", th.warn)
    };
    line(
        buf,
        inner,
        0,
        vec![Span::styled(format!("{pct:3.0}%"), bold(charge_color))],
    );
    let meter_w = inner.width.saturating_sub(18).max(4);
    Meter {
        ratio: b.charge.0,
        gradient: crate::ui::theme::Gradient::Solid(charge_color),
        track: th.border,
    }
    .render(Rect::new(inner.x + 5, inner.y, meter_w, 1), buf);
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
    // otherwise the compact three-row line flow.
    let flow_h = inner.height.saturating_sub(2).min(16);
    if flow_h >= 5 && inner.width >= 56 {
        let flow_area = Rect::new(inner.x, inner.y + 2, inner.width, flow_h);
        sankey(buf, flow_area, app, b, th);
    } else if inner.height >= 5 && inner.width >= 44 {
        let flow_area = Rect::new(inner.x, inner.y + 2, inner.width, 3);
        flow(buf, flow_area, app, b, th);
    }
}

/// The wattages every flow rendering shares. `ac` is actual delivery (SMC
/// PDTR, rated max as fallback), zeroed when unplugged; `bat` is signed
/// (positive = charging); `other` is the SMC system total minus the
/// telemetry we can attribute.
struct FlowW {
    ac: f32,
    bat: f32,
    sys: f32,
    soc: f32,
    disp: f32,
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
    FlowW {
        ac: if b.external_power { adapter.0 } else { 0.0 },
        bat: b.battery_watts.0,
        sys: sys.0,
        soc: soc.0,
        disp: disp.0,
        other: (sys.0 - soc.0 - disp.0).max(0.0),
    }
}

/// Three-row power flow, columns computed from the rect (no emoji — cell
/// widths must be exact):
/// ```text
/// AC  140.0W ─┐               ┌─▶ SoC    26.9W
///             ├─▶ SYS  63.0W ─┼─▶ DISP    1.2W
/// BAT  +0.0W ─┘               └─▶ other  34.9W
/// ```
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
    let (other, adapter, batt) = (Watts(fw.other), Watts(fw.ac), Watts(fw.bat));

    let heat = |w: Watts, scale: f32| th.power.at((w.0 / scale).clamp(0.0, 1.0));
    let (y0, y1, y2) = (area.y, area.y + 1, area.y + 2);

    // Column plan: [left 11][junction 1][middle …][junction 1][right 17]
    let left_w: u16 = 11;
    let junction_l = area.x + left_w; // '┐', '├', '┘'
    let right_w: u16 = 17;
    let junction_r = area.right().saturating_sub(right_w + 1); // '┌', '┼', '└'
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
    let mut consumer = |y: u16, label: &str, w: Watts, scale: f32| {
        buf.set_span(
            junction_r + 1,
            y,
            &Span::styled(format!("─▶ {label:<6}"), dim),
            10,
        );
        buf.set_span(
            junction_r + 11,
            y,
            &Span::styled(format!("{:>6}", w.to_string()), bold(heat(w, scale))),
            6,
        );
    };
    consumer(y0, "SoC", soc, 45.0);
    consumer(y1, "DISP", disp, 12.0);
    consumer(y2, "other", other, 30.0);
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
    const GAP: usize = 2; // half-rows kept clear between sink blocks
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
        ("other", w.other, th.net_tx),
        ("BAT", bat_sink, th.ok),
    ];

    // Half-row budget: the busier side of the graph fills the panel.
    let h2 = area.height as usize * 2;
    let live = |v: f32| v > 0.05;
    let n_sinks = sinks.iter().filter(|s| live(s.1)).count().max(1);
    let usable = h2.saturating_sub((n_sinks - 1) * GAP).max(4);
    let src_total: f32 = srcs.iter().map(|s| s.1).sum();
    let sink_total: f32 = sinks.iter().map(|s| s.1).sum();
    let per_watt = usable as f32 / src_total.max(sink_total).max(1.0);
    let t_src = ribbon_half_rows(&[srcs[0].1, srcs[1].1], per_watt);
    let t_sink = ribbon_half_rows(&[sinks[0].1, sinks[1].1, sinks[2].1, sinks[3].1], per_watt);

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
    let mut sink_top = [0usize; 4];
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
    let mut departs = [0usize; 4];
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
    fn mix_blends_toward_bg_and_passes_indexed() {
        let bg = Color::Rgb(10, 10, 15);
        let Color::Rgb(r, g, b) = mix(bg, Color::Rgb(210, 10, 115), 0.5) else {
            panic!("rgb in, rgb out");
        };
        assert_eq!((r, g, b), (110, 10, 65));
        assert_eq!(mix(bg, Color::Indexed(203), 0.5), Color::Indexed(203));
    }
}
