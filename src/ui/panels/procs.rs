//! Process table: clickable sortable headers, scrollable rows, gradient
//! value coloring, filter line, scrollbar. Wide panels split into
//! side-by-side panes — consecutive slices of one scrolled list — so
//! ultrawide layouts show 2–4× more processes instead of stretching the
//! NAME column into dead space.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, SortKey};
use crate::collect::procs::ProcState;
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target};

use super::{chrome_with, format_duration, line};

/// Fixed column widths (name flexes).
const W_PID: u16 = 7;
const W_USER: u16 = 11;
const W_CPU: u16 = 7;
const W_PWR: u16 = 7;
const W_MEM: u16 = 8;
const W_THR: u16 = 5;
const W_ST: u16 = 3;
const W_TIME: u16 = 9;
/// Per-process network rate columns (joined from the flows sample); only
/// drawn when a pane is wide enough to spare the room.
const W_NET: u16 = 8;

/// Fixed columns + the two 1-col gutters (after PID, after TIME).
const FIXED_W: u16 = W_PID + W_USER + W_CPU + W_PWR + W_MEM + W_THR + W_ST + W_TIME + 2;
/// Minimum pane width: the fixed columns plus a NAME that fits real
/// process names ("Code Helper (Renderer)" and friends).
const PANE_MIN: u16 = 96;
/// Gap between panes; a dim rule rides its center cell.
const PANE_GAP: u16 = 3;
/// Pane width at which the ↓/s ↑/s columns appear.
const PANE_NET_MIN: u16 = 118;
/// Comfortable single-pane width (net columns + a 51-cell NAME).
const PANE_COMFORT: u16 = 128;

/// How many panes an inner width can host.
pub(crate) fn max_panes(inner_width: u16) -> u16 {
    ((inner_width + PANE_GAP) / (PANE_MIN + PANE_GAP)).clamp(1, 4)
}

/// Outer panel width the layout should reserve for an `n`-pane table
/// (comfortable panes + gaps + the two border columns).
pub(crate) fn preferred_width(n: u16) -> u16 {
    n * PANE_COMFORT + n.saturating_sub(1) * PANE_GAP + 2
}

/// `cap` limits the side-by-side panes (from config, via the layout — call
/// sites that can hand freed width to other panels pass the user's setting;
/// the rest pass 4 so the table always uses the space it was given).
pub fn render(
    buf: &mut Buffer,
    area: Rect,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    cap: u16,
) {
    let dim = Style::default().fg(th.dim);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let headline = vec![
        Span::styled(app.procs.total.to_string(), bold(th.text)),
        Span::styled(" · ", dim),
        Span::styled(app.procs.running.to_string(), bold(th.ok)),
        Span::styled(" running · ", dim),
        Span::styled(app.procs.threads.to_string(), bold(th.text)),
        Span::styled(" thr", dim),
    ];
    let inner = chrome_with(buf, area, "PROCESSES", headline, th);
    if inner.height < 2 {
        return;
    }

    // Filter line (only when active or non-empty), spanning every pane.
    // Clicking it re-enters editing, same as `/`.
    let mut top = 0u16;
    if app.filter_editing || !app.filter.is_empty() {
        let cursor = if app.filter_editing { "▏" } else { "" };
        line(
            buf,
            inner,
            0,
            vec![
                Span::styled(" filter: ", dim),
                Span::styled(
                    format!("{}{cursor}", app.filter),
                    Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("   {} match(es) · esc clears", app.visible_rows.len()),
                    dim,
                ),
            ],
        );
        if inner.height > 0 {
            hits.push(Rect::new(inner.x, inner.y, inner.width, 1), Target::Filter);
        }
        top = 1;
    }

    // Pane geometry: as many side-by-side panes as keep each ≥ PANE_MIN,
    // bounded by the caller's cap.
    let panes = max_panes(inner.width).min(cap.max(1));
    let content_w = inner.width - (panes - 1) * PANE_GAP;
    let pane_w = content_w / panes;
    let wide_panes = content_w % panes; // leftover cells widen the first panes
    // One decision for every pane (gated on the narrowest), or remainder
    // cells would give some panes net columns and not others.
    let show_net = pane_w >= PANE_NET_MIN;

    let body_top = top + 1;
    let body_h = inner.height.saturating_sub(body_top) as usize;
    let capacity = body_h * panes as usize;
    let body = Rect::new(inner.x, inner.y + body_top, inner.width, body_h as u16);
    hits.push(body, Target::ProcList);

    // Keep selection inside the full multi-pane window.
    if app.selected < app.scroll {
        app.scroll = app.selected;
    } else if app.selected >= app.scroll + capacity {
        app.scroll = app.selected + 1 - capacity;
    }
    app.scroll = app
        .scroll
        .min(app.visible_rows.len().saturating_sub(capacity.max(1)));

    let own_uid = crate::ffi::sys::effective_uid();
    let mut x0 = inner.x;
    for p in 0..panes {
        let w = pane_w + u16::from(p < wide_panes);
        let rect = Rect::new(x0, inner.y + top, w, inner.height - top);
        draw_pane(
            buf,
            hits,
            rect,
            &Pane {
                app,
                th,
                own_uid,
                show_net,
                first: app.scroll + p as usize * body_h,
                rows_h: body_h,
            },
        );
        if p + 1 < panes {
            for i in 0..(inner.height - top) {
                buf.set_span(
                    x0 + w + 1,
                    inner.y + top + i,
                    &Span::styled("│", Style::default().fg(th.border)),
                    1,
                );
            }
        }
        x0 += w + PANE_GAP;
    }

    // Scrollbar: the accent thumb rides the panel's right border — the
    // border line itself is the track, so no second line floats inside the
    // table, misaligned with the frame corners.
    if app.visible_rows.len() > capacity && body_h > 0 {
        let total = app.visible_rows.len() as f32;
        let thumb_h = ((capacity as f32 / total) * body_h as f32).ceil().max(1.0) as usize;
        let thumb_top = ((app.scroll as f32 / total) * body_h as f32).round() as usize;
        for i in thumb_top..(thumb_top + thumb_h).min(body_h) {
            buf.set_span(
                inner.right(),
                body.y + i as u16,
                &Span::styled("┃", Style::default().fg(th.accent)),
                1,
            );
        }
    }
}

/// Everything a pane needs besides its rect.
struct Pane<'a> {
    app: &'a App,
    th: &'a Theme,
    own_uid: u32,
    /// Whether the ↓/s ↑/s columns are drawn (uniform across panes).
    show_net: bool,
    /// Visible-row index of this pane's first row.
    first: usize,
    /// Body rows below the header.
    rows_h: usize,
}

/// One pane: a header row plus `rows_h` process rows starting at visible
/// index `first`. Every pane repeats the full column set (headers stay
/// clickable everywhere); NAME takes whatever the pane has left.
fn draw_pane(buf: &mut Buffer, hits: &mut HitMap, rect: Rect, p: &Pane) {
    let (app, th) = (p.app, p.th);
    let header_y = rect.y;
    let show_net = p.show_net;
    let net_w = if show_net { 2 * W_NET } else { 0 };
    let name_w = rect.width.saturating_sub(FIXED_W + net_w);
    let header_style = Style::default()
        .fg(th.bg)
        .bg(th.accent)
        .add_modifier(Modifier::BOLD);
    for x in rect.left()..rect.right() {
        buf[(x, header_y)].set_style(header_style);
        buf[(x, header_y)].set_char(' ');
    }
    let mut x = rect.x;
    let mut draw_col = |x: &mut u16, key: Option<SortKey>, label: &str, width: u16, left: bool| {
        let arrow = match key {
            Some(k) if k == app.sort => {
                if app.sort_desc {
                    "▼"
                } else {
                    "▲"
                }
            }
            _ => "",
        };
        let text = format!("{label}{arrow}");
        let text = if left {
            format!("{text:<w$}", w = width as usize)
        } else {
            format!("{text:>w$}", w = width as usize)
        };
        buf.set_span(*x, header_y, &Span::styled(text, header_style), width);
        if let Some(k) = key {
            hits.push(Rect::new(*x, header_y, width, 1), Target::ProcHeader(k));
        }
        *x += width;
    };
    draw_col(&mut x, Some(SortKey::Pid), "PID", W_PID, false);
    x += 1;
    draw_col(&mut x, Some(SortKey::User), "USER", W_USER, true);
    draw_col(&mut x, Some(SortKey::Cpu), "CPU%", W_CPU, false);
    draw_col(&mut x, Some(SortKey::Power), "PWR", W_PWR, false);
    draw_col(&mut x, Some(SortKey::Memory), "MEM", W_MEM, false);
    if show_net {
        draw_col(&mut x, Some(SortKey::Net), "↓/s", W_NET, false);
        draw_col(&mut x, Some(SortKey::Net), "↑/s", W_NET, false);
    }
    draw_col(&mut x, Some(SortKey::Threads), "THR", W_THR, false);
    draw_col(&mut x, None, " S", W_ST, true);
    draw_col(&mut x, None, "TIME", W_TIME, false);
    x += 1;
    draw_col(&mut x, Some(SortKey::Name), "NAME", name_w, true);

    for (vis_i, &row_i) in app
        .visible_rows
        .iter()
        .enumerate()
        .skip(p.first)
        .take(p.rows_h)
    {
        let r = &app.procs.rows[row_i];
        let y = rect.y + 1 + (vis_i - p.first) as u16;
        let selected = vis_i == app.selected;
        let hovered = app.hover == Some(Target::ProcRow(vis_i));

        let row_bg = if selected {
            th.selection_bg
        } else if hovered {
            th.panel_bg
        } else {
            th.bg
        };
        for cx in rect.left()..rect.right() {
            buf[(cx, y)].set_style(Style::default().bg(row_bg));
        }

        let base = if r.state == ProcState::Zombie {
            Style::default().fg(th.crit).bg(row_bg)
        } else if is_own(r, p.own_uid) {
            Style::default().fg(th.text).bg(row_bg)
        } else {
            Style::default().fg(th.dim).bg(row_bg)
        };
        let value_or_dash = |v: Option<String>| v.unwrap_or_else(|| "–".into());

        let cpu_frac = r.cpu.map_or(0.0, |c| c.0);
        let cpu_style = if r.cpu.is_some() {
            Style::default()
                .fg(th.cpu.at((cpu_frac / 4.0).min(1.0)))
                .bg(row_bg)
        } else {
            Style::default().fg(th.dim).bg(row_bg)
        };
        // 5 W ≈ a single process working a P-cluster hard — top of the ramp.
        let pwr_style = if let Some(w) = r.power {
            Style::default()
                .fg(th.power.at((w.0 / 5.0).min(1.0)))
                .bg(row_bg)
        } else {
            Style::default().fg(th.dim).bg(row_bg)
        };
        let mem_frac = r
            .memory
            .map_or(0.0, |m| m.0 as f32 / (8.0 * 1024.0 * 1024.0 * 1024.0));
        let mem_style = if r.memory.is_some() {
            Style::default().fg(th.mem.at(mem_frac.min(1.0))).bg(row_bg)
        } else {
            Style::default().fg(th.dim).bg(row_bg)
        };
        let state_color = match r.state {
            ProcState::Running => th.ok,
            ProcState::Zombie => th.crit,
            ProcState::Stopped => th.warn,
            _ => th.dim,
        };

        let mut x = rect.x;
        let mut cell = |x: &mut u16, text: String, width: u16, style: Style, left: bool| {
            let text = if left {
                format!("{text:<w$}", w = width as usize)
            } else {
                format!("{text:>w$}", w = width as usize)
            };
            buf.set_span(*x, y, &Span::styled(text, style), width);
            *x += width;
        };
        cell(&mut x, r.pid.to_string(), W_PID, base, false);
        x += 1;
        cell(
            &mut x,
            truncate(&r.user, W_USER as usize - 1),
            W_USER,
            base,
            true,
        );
        cell(
            &mut x,
            value_or_dash(r.cpu.map(|c| format!("{:.1}", c.as_percent()))),
            W_CPU,
            cpu_style.add_modifier(Modifier::BOLD),
            false,
        );
        cell(
            &mut x,
            value_or_dash(r.power.map(|w| w.to_string())),
            W_PWR,
            pwr_style,
            false,
        );
        cell(
            &mut x,
            value_or_dash(r.memory.map(|m| m.to_string())),
            W_MEM,
            mem_style,
            false,
        );
        if show_net {
            let rates = app.flows.by_pid.get(&r.pid).copied();
            let net_cell =
                |x: &mut u16,
                 v: Option<u64>,
                 color,
                 cell: &mut dyn FnMut(&mut u16, String, u16, Style, bool)| {
                    let (text, style) = match v {
                        Some(n) if n > 0 => (
                            format!("{}", crate::units::Bytes(n)),
                            Style::default().fg(color).bg(row_bg),
                        ),
                        Some(_) => ("0".into(), Style::default().fg(th.dim).bg(row_bg)),
                        None => ("–".into(), Style::default().fg(th.dim).bg(row_bg)),
                    };
                    cell(x, text, W_NET, style, false);
                };
            net_cell(&mut x, rates.map(|(rx, _)| rx), th.net_rx, &mut cell);
            net_cell(&mut x, rates.map(|(_, tx)| tx), th.net_tx, &mut cell);
        }
        cell(
            &mut x,
            value_or_dash(r.threads.map(|t| t.to_string())),
            W_THR,
            base,
            false,
        );
        cell(
            &mut x,
            format!(" {}", r.state.glyph()),
            W_ST,
            Style::default().fg(state_color).bg(row_bg),
            true,
        );
        cell(
            &mut x,
            value_or_dash(r.cpu_time_secs.map(format_duration)),
            W_TIME,
            base,
            false,
        );
        x += 1;
        let name_style = if selected {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        cell(
            &mut x,
            truncate(&r.name, name_w as usize),
            name_w,
            name_style,
            true,
        );

        hits.push(Rect::new(rect.x, y, rect.width, 1), Target::ProcRow(vis_i));
    }
}

fn is_own(r: &crate::collect::procs::ProcRow, _own_uid: u32) -> bool {
    r.cpu.is_some()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::{max_panes, preferred_width};

    #[test]
    fn procs_pane_geometry() {
        // One pane below the 2-pane threshold, scaling up to the cap of 4.
        assert_eq!(max_panes(96), 1);
        assert_eq!(max_panes(194), 1);
        assert_eq!(max_panes(195), 2);
        assert_eq!(max_panes(297), 3);
        assert_eq!(max_panes(600), 4);
        // A reserved n-pane table must actually host n panes inside its borders.
        for n in 1..=4 {
            assert_eq!(max_panes(preferred_width(n) - 2), n);
        }
    }
}
