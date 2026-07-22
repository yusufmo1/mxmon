//! Responsive layout: allocates rects to panels by terminal size.
//!
//! Breakpoints: ≥200 cols = ultrawide (4-across metric rows), ≥130 = wide
//! (3-across), ≥88 = two columns, below = single stacked column. Height
//! drives progressive disclosure — panels shrink before they disappear.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, View};
use crate::ui::panels;
use crate::ui::panels::nav;
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, PanelKind, fill_bg};

use super::{overlays, thermal};

/// Extra UI state owned by the render layer (scroll positions, caches).
#[derive(Default)]
pub struct RenderState {
    pub sensor_scroll: usize,
    pub flows_scroll: usize,
    /// Cached thermal-map surface; recomputed only when temps/size/theme change.
    pub heat: Option<thermal::HeatSurface>,
}

pub fn draw(f: &mut Frame<'_>, app: &mut App, th: &Theme, hits: &mut HitMap, rs: &mut RenderState) {
    hits.clear();
    let screen = f.area();
    let buf = f.buffer_mut();
    fill_bg(screen, buf, th.bg);

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(screen);

    panels::header::header(buf, header, app, th, hits);

    match app.view {
        View::Overview => overview(buf, body, app, th, hits, rs),
        View::Processes => processes_view(buf, body, app, th, hits),
        View::Thermal => thermal::render(buf, body, app, th, hits, rs),
        View::Connections => panels::flows::render(buf, body, app, th, hits, rs),
    }

    panels::header::footer(buf, footer, app, th, hits);
    overlays::render(buf, screen, app, th, hits);

    // Degrade to the 256-color palette on terminals without 24-bit SGR
    // (Terminal.app) — one pass over the buffer, before ratatui's cell diff.
    if !super::theme::truecolor_supported() {
        super::theme::quantize_buffer(buf);
    }
}

/// Full-screen Processes view. When the configured pane cap leaves width
/// the table doesn't want (`procs_panes`, default 1), the freed columns
/// become a metric-widget grid instead of a stretched NAME column — raise
/// the cap in settings (`o`) for the wall-of-processes look.
fn processes_view(
    buf: &mut ratatui::buffer::Buffer,
    body: Rect,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
) {
    let cap = app.config.procs_panes;
    let fit = panels::procs::max_panes(body.width.saturating_sub(2));
    let table_w = panels::procs::preferred_width(cap);
    if fit > cap && body.width.saturating_sub(table_w) >= 40 {
        let [table, widgets] =
            Layout::horizontal([Constraint::Length(table_w), Constraint::Min(0)]).areas(body);
        panels::procs::render(buf, table, app, th, hits, cap);
        widget_grid(buf, widgets, app, th, hits);
    } else {
        panels::procs::render(buf, body, app, th, hits, 4);
    }
}

/// A row-major grid of metric panels filling whatever rect the process
/// table left over: 1–2 columns, 2–4 rows, most-useful panels first.
fn widget_grid(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
) {
    let cols = (area.width / 100).clamp(1, 2);
    let rows = (area.height / 9).clamp(2, 4);
    let mut fns: Vec<(PanelFn, PanelKind)> = vec![
        (panels::cpu::render, PanelKind::Cpu),
        (panels::net::render, PanelKind::Net),
        (panels::mem::render, PanelKind::Mem),
        (panels::disk::render, PanelKind::Disk),
        (panels::power::render, PanelKind::Power),
        (panels::gpu::render, PanelKind::Gpu),
        (panels::temps::render, PanelKind::Temps),
    ];
    if app.battery.is_some() {
        fns.push((panels::battery::render, PanelKind::Battery));
    }
    let (cw, rh) = (area.width / cols, area.height / rows);
    for (i, (f, kind)) in fns.into_iter().take((cols * rows) as usize).enumerate() {
        let (row, col) = (i as u16 / cols, i as u16 % cols);
        // The last column/row absorb the division remainders.
        let w = if col + 1 == cols {
            area.width - col * cw
        } else {
            cw
        };
        let h = if row + 1 == rows {
            area.height - row * rh
        } else {
            rh
        };
        let rect = Rect::new(area.x + col * cw, area.y + row * rh, w, h);
        f(buf, rect, app, th);
        nav(buf, rect, app, th, hits, kind);
    }
}

fn overview(
    buf: &mut ratatui::buffer::Buffer,
    body: Rect,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
) {
    let width = body.width;
    let tall = body.height >= 44;

    // Panel heights adapt to total height.
    let cpu_h = if tall { 12 } else { 9 };
    let mid_h = if tall { 9 } else { 7 };
    // The network panel earns roughly double its old height (mirrored
    // graph, connectivity strip, link details) and the whole metric row
    // grows with it — every panel in that row stretches its graph. Graded
    // so the process list (and the ≥16-row inline heat map) keep their
    // space on shorter terminals.
    let net_h = match body.height {
        h if h >= 50 => 18,
        h if h >= 44 => 16,
        h if h >= 38 => 14,
        h if h >= 32 => 12,
        _ => mid_h,
    };
    let procs_min = 6;

    if width >= 300 && body.height >= 30 {
        // Ultrawide: a full-height thermal column on the right stops the
        // metric panels from stretching into acres of dead space, and the
        // heat map finally gets the real estate it deserves.
        let aspect_w = (f32::from(body.height.saturating_sub(2)) * 3.06) as u16 + 4;
        let map_w = aspect_w.min(width * 38 / 100);
        let [left, map_col] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(map_w)]).areas(body);
        thermal::map_panel(buf, map_col, app, th, rs);
        nav(buf, map_col, app, th, hits, PanelKind::HeatMap);

        let [top, mid, procs_area] = Layout::vertical([
            Constraint::Length(cpu_h),
            Constraint::Length(net_h),
            Constraint::Min(procs_min),
        ])
        .areas(left);

        let [cpu_a, power_a, battery_a] = Layout::horizontal([
            Constraint::Percentage(40),
            Constraint::Percentage(28),
            Constraint::Percentage(32),
        ])
        .areas(top);
        panels::cpu::render(buf, cpu_a, app, th);
        nav(buf, cpu_a, app, th, hits, PanelKind::Cpu);
        panels::power::render(buf, power_a, app, th);
        nav(buf, power_a, app, th, hits, PanelKind::Power);
        panels::battery::render(buf, battery_a, app, th);
        nav(buf, battery_a, app, th, hits, PanelKind::Battery);

        // With the pane cap (default 1) leaving width the process table
        // doesn't want, DISK and TEMPS move down beside it as tall panels
        // and the metric row relaxes from five-across to three.
        let cap = app.config.procs_panes;
        let fit = panels::procs::max_panes(left.width.saturating_sub(2));
        let table_w = panels::procs::preferred_width(cap);
        if fit > cap && left.width.saturating_sub(table_w) >= 84 {
            let [gpu_a, mem_a, net_a] = Layout::horizontal([
                Constraint::Percentage(33),
                Constraint::Percentage(33),
                Constraint::Percentage(34),
            ])
            .areas(mid);
            panels::gpu::render(buf, gpu_a, app, th);
            nav(buf, gpu_a, app, th, hits, PanelKind::Gpu);
            panels::mem::render(buf, mem_a, app, th);
            nav(buf, mem_a, app, th, hits, PanelKind::Mem);
            panels::net::render(buf, net_a, app, th);
            nav(buf, net_a, app, th, hits, PanelKind::Net);

            let [table, disk_a, temps_a] = Layout::horizontal([
                Constraint::Length(table_w),
                Constraint::Fill(1),
                Constraint::Fill(1),
            ])
            .areas(procs_area);
            panels::procs::render(buf, table, app, th, hits, cap);
            panels::disk::render(buf, disk_a, app, th);
            nav(buf, disk_a, app, th, hits, PanelKind::Disk);
            panels::temps::render(buf, temps_a, app, th);
            nav(buf, temps_a, app, th, hits, PanelKind::Temps);
        } else {
            let [gpu_a, mem_a, net_a, disk_a, temps_a] =
                Layout::horizontal([Constraint::Percentage(20); 5]).areas(mid);
            panels::gpu::render(buf, gpu_a, app, th);
            nav(buf, gpu_a, app, th, hits, PanelKind::Gpu);
            panels::mem::render(buf, mem_a, app, th);
            nav(buf, mem_a, app, th, hits, PanelKind::Mem);
            panels::net::render(buf, net_a, app, th);
            nav(buf, net_a, app, th, hits, PanelKind::Net);
            panels::disk::render(buf, disk_a, app, th);
            nav(buf, disk_a, app, th, hits, PanelKind::Disk);
            panels::temps::render(buf, temps_a, app, th);
            nav(buf, temps_a, app, th, hits, PanelKind::Temps);

            panels::procs::render(buf, procs_area, app, th, hits, 4);
        }
    } else if width >= 300 {
        // Hyper-wide but short (the ultrawide branch above needs ≥30 rows): a
        // wide, shallow strip can't afford stacked metric rows — two rows plus
        // the process list squeeze every panel to a few lines and the heat map
        // is dropped entirely. Here the strip becomes ONE line of full-height
        // cards instead: each panel, the chassis heat map, and the process
        // table (double-width) sit side by side, so nothing is squished and
        // the width finally earns its keep. Width-hungry cards carry heavier
        // weights; naturally-slim ones stay thin.
        #[derive(Clone, Copy)]
        enum Card {
            Metric(PanelFn, PanelKind),
            Heat,
            Procs,
        }
        let mut cards: Vec<(u16, Card)> = vec![
            (6, Card::Metric(panels::cpu::render, PanelKind::Cpu)),
            (4, Card::Metric(panels::power::render, PanelKind::Power)),
        ];
        if app.battery.is_some() {
            cards.push((7, Card::Metric(panels::battery::render, PanelKind::Battery)));
        }
        cards.extend([
            (4, Card::Metric(panels::gpu::render, PanelKind::Gpu)),
            (4, Card::Metric(panels::mem::render, PanelKind::Mem)),
            (6, Card::Metric(panels::net::render, PanelKind::Net)),
            (4, Card::Metric(panels::disk::render, PanelKind::Disk)),
            (5, Card::Metric(panels::temps::render, PanelKind::Temps)),
            (6, Card::Heat),
            (18, Card::Procs),
        ]);

        let widths: Vec<Constraint> = cards.iter().map(|&(w, _)| Constraint::Fill(w)).collect();
        let rects = Layout::horizontal(widths).split(body);
        // Rects are computed up front, so rendering is a plain sequence — each
        // call releases its borrow before the next, letting the `&App` metric
        // panels sit beside the process card's `&mut App` and the heat card's
        // `&mut RenderState`.
        for (&(_, card), &rect) in cards.iter().zip(rects.iter()) {
            match card {
                Card::Metric(f, kind) => {
                    f(buf, rect, app, th);
                    nav(buf, rect, app, th, hits, kind);
                }
                Card::Heat => {
                    thermal::map_panel(buf, rect, app, th, rs);
                    nav(buf, rect, app, th, hits, PanelKind::HeatMap);
                }
                Card::Procs => panels::procs::render(buf, rect, app, th, hits, 4),
            }
        }
    } else if width >= 130 {
        // Wide/ultrawide: CPU+POWER on top, then metric row(s), procs bottom.
        let [top, mid, procs_area] = Layout::vertical([
            Constraint::Length(cpu_h),
            Constraint::Length(net_h),
            Constraint::Min(procs_min),
        ])
        .areas(body);

        let [cpu_a, power_a, battery_a] = Layout::horizontal([
            Constraint::Percentage(44),
            Constraint::Percentage(26),
            Constraint::Percentage(30),
        ])
        .areas(top);
        panels::cpu::render(buf, cpu_a, app, th);
        nav(buf, cpu_a, app, th, hits, PanelKind::Cpu);
        panels::power::render(buf, power_a, app, th);
        nav(buf, power_a, app, th, hits, PanelKind::Power);
        panels::battery::render(buf, battery_a, app, th);
        nav(buf, battery_a, app, th, hits, PanelKind::Battery);

        let [gpu_a, mem_a, net_a, disk_a, temps_a] =
            Layout::horizontal([Constraint::Percentage(20); 5]).areas(mid);
        panels::gpu::render(buf, gpu_a, app, th);
        nav(buf, gpu_a, app, th, hits, PanelKind::Gpu);
        panels::mem::render(buf, mem_a, app, th);
        nav(buf, mem_a, app, th, hits, PanelKind::Mem);
        panels::net::render(buf, net_a, app, th);
        nav(buf, net_a, app, th, hits, PanelKind::Net);
        panels::disk::render(buf, disk_a, app, th);
        nav(buf, disk_a, app, th, hits, PanelKind::Disk);
        panels::temps::render(buf, temps_a, app, th);
        nav(buf, temps_a, app, th, hits, PanelKind::Temps);

        // Big terminals get the chassis heat map inline, beside the
        // process table (sized to the chassis aspect, capped at 45%).
        if width >= 156 && procs_area.height >= 16 {
            let aspect_w = (f32::from(procs_area.height - 2) * 3.06) as u16 + 4;
            let map_w = aspect_w.min(procs_area.width * 45 / 100);
            let [procs_l, map_r] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(map_w)])
                    .areas(procs_area);
            panels::procs::render(buf, procs_l, app, th, hits, 4);
            thermal::map_panel(buf, map_r, app, th, rs);
            nav(buf, map_r, app, th, hits, PanelKind::HeatMap);
        } else {
            panels::procs::render(buf, procs_area, app, th, hits, 4);
        }
    } else if width >= 88 {
        // Two columns; a third metric row (disk + temps) appears when tall.
        // The network row takes the taller grade only when there's room for
        // the extra rows on top of the (possibly three) metric rows.
        let net2 = match body.height {
            h if h >= 54 => 14,
            h if h >= 48 => 12,
            _ => mid_h,
        };
        let mid_total = mid_h + net2 + if tall { mid_h } else { 0 };
        let [top, mid, procs_area] = Layout::vertical([
            Constraint::Length(cpu_h),
            Constraint::Length(mid_total),
            Constraint::Min(procs_min),
        ])
        .areas(body);

        let [cpu_a, power_a] =
            Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(top);
        panels::cpu::render(buf, cpu_a, app, th);
        nav(buf, cpu_a, app, th, hits, PanelKind::Cpu);
        panels::power::render(buf, power_a, app, th);
        nav(buf, power_a, app, th, hits, PanelKind::Power);

        let [mid_top, mid_bottom, mid_third] = Layout::vertical([
            Constraint::Length(mid_h),
            Constraint::Length(net2),
            Constraint::Min(0), // mid_h when tall, zero otherwise
        ])
        .areas(mid);
        let [gpu_a, mem_a] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(mid_top);
        panels::gpu::render(buf, gpu_a, app, th);
        nav(buf, gpu_a, app, th, hits, PanelKind::Gpu);
        panels::mem::render(buf, mem_a, app, th);
        nav(buf, mem_a, app, th, hits, PanelKind::Mem);
        let [net_a, right_a] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(mid_bottom);
        panels::net::render(buf, net_a, app, th);
        nav(buf, net_a, app, th, hits, PanelKind::Net);
        if tall {
            // Row 2 keeps battery (temps moves to row 3 beside disk); on
            // desktops without a battery, disk takes the full third row.
            if app.battery.is_some() {
                panels::battery::render(buf, right_a, app, th);
                nav(buf, right_a, app, th, hits, PanelKind::Battery);
                let [disk_a, temps_a] =
                    Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .areas(mid_third);
                panels::disk::render(buf, disk_a, app, th);
                nav(buf, disk_a, app, th, hits, PanelKind::Disk);
                panels::temps::render(buf, temps_a, app, th);
                nav(buf, temps_a, app, th, hits, PanelKind::Temps);
            } else {
                panels::temps::render(buf, right_a, app, th);
                nav(buf, right_a, app, th, hits, PanelKind::Temps);
                panels::disk::render(buf, mid_third, app, th);
                nav(buf, mid_third, app, th, hits, PanelKind::Disk);
            }
        } else if app.battery.is_some() && body.height < 40 {
            panels::battery::render(buf, right_a, app, th);
            nav(buf, right_a, app, th, hits, PanelKind::Battery);
        } else {
            panels::temps::render(buf, right_a, app, th);
            nav(buf, right_a, app, th, hits, PanelKind::Temps);
        }

        panels::procs::render(buf, procs_area, app, th, hits, 4);
    } else {
        // Narrow: single stacked column, priority order, whatever fits.
        let heights = [
            cpu_h.min(9),
            6,  // power
            6,  // gpu
            6,  // memory
            10, // network (mirrored graph + connectivity need the rows)
            6,  // disk
            if app.battery.is_some() { 7 } else { 0 },
        ];
        let mut y = body.y;
        let panels_fns: [(u16, PanelFn, PanelKind); 7] = [
            (heights[0], panels::cpu::render, PanelKind::Cpu),
            (heights[1], panels::power::render, PanelKind::Power),
            (heights[2], panels::gpu::render, PanelKind::Gpu),
            (heights[3], panels::mem::render, PanelKind::Mem),
            (heights[4], panels::net::render, PanelKind::Net),
            (heights[5], panels::disk::render, PanelKind::Disk),
            (heights[6], panels::battery::render, PanelKind::Battery),
        ];
        for (h, render, kind) in panels_fns {
            if h == 0 {
                continue;
            }
            // Always leave room for the process list.
            if y + h + procs_min > body.bottom() {
                break;
            }
            let area = Rect::new(body.x, y, body.width, h);
            render(buf, area, app, th);
            nav(buf, area, app, th, hits, kind);
            y += h;
        }
        if y < body.bottom() {
            let procs_area = Rect::new(body.x, y, body.width, body.bottom() - y);
            panels::procs::render(buf, procs_area, app, th, hits, 4);
        }
    }
}

type PanelFn = fn(&mut ratatui::buffer::Buffer, Rect, &App, &Theme);

/// Golden-frame snapshots: the fixture `App` rendered through the real
/// `draw` entry point. Glyphs only (colors are covered by theme unit tests),
/// so the snapshots are identical across truecolor and 256-color hosts.
/// After an intentional redesign: `cargo insta review`.
#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{RenderState, draw};
    use crate::app::{App, Modal, View};
    use crate::testutil as tu;
    use crate::ui::theme;
    use crate::ui::widgets::HitMap;

    /// One frame at `w`×`h`, right-trimmed, one line per row.
    fn frame(app: &mut App, w: u16, h: u16) -> String {
        let th = theme::by_name(&app.config.theme);
        let mut term = Terminal::new(TestBackend::new(w, h)).expect("backend");
        let mut hits = HitMap::default();
        let mut rs = RenderState::default();
        term.draw(|f| draw(f, app, &th, &mut hits, &mut rs))
            .expect("draw");
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in buf.area.top()..buf.area.bottom() {
            let line: String = (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect();
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    fn snap(name: &str, app: &mut App, w: u16, h: u16) {
        insta::with_settings!({
            // The header clock is the only wall-clock artifact left in a
            // frame. The details modal's "started … ago" used to need a
            // filter too, but redacting it only normalized the text — the
            // padding after it still tracked the real width. The fixture
            // pins a fixed age instead (`testutil::procs`), so the string
            // is constant and the frame can be asserted verbatim.
            filters => vec![(r"\d{2}:\d{2}:\d{2}", "HH:MM:SS")],
            omit_expression => true,
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(name, frame(app, w, h));
        });
    }

    #[test]
    fn views_render_stably() {
        let mut app = tu::app();
        for (view, tag) in [
            (View::Overview, "overview"),
            (View::Processes, "processes"),
            (View::Thermal, "thermal"),
            (View::Connections, "connections"),
        ] {
            app.view = view;
            snap(&format!("{tag}_80x24"), &mut app, 80, 24);
            snap(&format!("{tag}_160x45"), &mut app, 160, 45);
        }
        // The ≥300-column overview takes the single full-height row branch.
        app.view = View::Overview;
        snap("overview_320x60", &mut app, 320, 60);
    }

    #[test]
    fn hover_affordances_render_stably() {
        use crate::ui::widgets::{PanelKind, Target};
        // Colors are filtered from snapshots, so this locks the *glyph*
        // affordances: the card's "▸ destination" tag in its bottom border
        // and unchanged neighbors. One card per destination flavor.
        let mut app = tu::app();
        app.view = View::Overview;
        app.hover = Some(Target::Panel(PanelKind::Cpu));
        snap("overview_hover_cpu_160x45", &mut app, 160, 45);
        app.hover = Some(Target::Panel(PanelKind::Net));
        snap("overview_hover_net_160x45", &mut app, 160, 45);
    }

    #[test]
    fn modals_render_stably() {
        let mut app = tu::app();
        app.view = View::Processes;
        let pid = app.selected_row().map_or(0, |r| r.pid);
        let name = app
            .selected_row()
            .map_or_else(String::new, |r| r.name.clone());
        let modals: [(&str, Modal); 5] = [
            ("help", Modal::Help),
            ("sort", Modal::SortMenu { selected: 2 }),
            ("settings", Modal::Settings { selected: 1 }),
            (
                "kill",
                Modal::Kill {
                    pid,
                    name,
                    selected: 1,
                },
            ),
            ("details", Modal::Details { pid }),
        ];
        for (tag, modal) in modals {
            app.modal = Some(modal);
            snap(&format!("modal_{tag}_120x36"), &mut app, 120, 36);
        }
    }
}
