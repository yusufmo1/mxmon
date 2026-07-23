//! Responsive layout: allocates rects to panels by terminal size.
//!
//! Breakpoints: ≥200 cols = ultrawide (4-across metric rows), ≥130 = wide
//! (3-across), ≥88 = two columns, below = single stacked column. Height
//! drives progressive disclosure — panels shrink before they disappear.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Span;

use crate::app::{App, View};
use crate::ui::panels;
use crate::ui::panels::nav;
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, PanelKind, Target, fill_bg};

use super::{overlays, thermal};

/// Extra UI state owned by the render layer (scroll positions, caches).
#[derive(Default)]
pub struct RenderState {
    pub sensor_scroll: usize,
    pub flows_scroll: usize,
    /// Cached thermal-map surface; recomputed only when temps/size/theme change.
    pub heat: Option<thermal::HeatSurface>,
    /// Cached chassis layout (geometry / floorplan / sensor placement); rebuilt
    /// only when the map size, `temps_seq`, or schematic / battery presence
    /// changes, so a 30 fps heat-map redraw doesn't re-lay-out every frame.
    pub geom: Option<thermal::GeomCache>,
    /// The last fully-composed frame. On a pure motion frame we restore this
    /// and re-render only the animating cards + header/footer, so every static
    /// cell stays byte-identical (see [`draw`]). `last_view` guards against
    /// reusing it across a view switch.
    pub last_frame: Option<ratatui::buffer::Buffer>,
    pub last_view: Option<View>,
}

/// Frame entry point. On a pure motion frame (`motion_frame` — a `recv`
/// timeout in the UI loop, so no new data or input) the animation only moved
/// the graph waveforms and the heat map; restore the previous frame and
/// re-render just those cards. Otherwise compose the whole frame. Both paths
/// leave an identical buffer — the `partial_repaint_matches_full` parity test
/// enforces it — and both refresh `last_frame` for the next motion frame.
pub fn draw(
    f: &mut Frame<'_>,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
    motion_frame: bool,
) {
    if motion_frame && can_partial(app, rs, f.area()) {
        draw_partial(f, app, th, hits, rs);
    } else {
        draw_full(f, app, th, hits, rs);
    }
    // Keep the finished frame so the next motion frame can restore it. This is
    // the load-bearing invariant: ratatui's diff compares against exactly the
    // buffer we drew last, so a partial repaint over this snapshot emits only
    // the cells the animation changed. Only a motion-on session ever takes the
    // partial path, so skip the per-frame clone entirely when motion is off —
    // and the input that toggles motion on is itself a full frame that primes
    // this snapshot before the first motion frame can use it.
    if app.config.motion {
        rs.last_frame = Some(f.buffer_mut().clone());
        rs.last_view = Some(app.view);
    }
}

/// A motion frame may reuse the last frame only when the layout is unchanged
/// and nothing overlays or dims it. Any of these take the full path instead
/// (which refreshes `last_frame`): motion off / paused, a modal or drag /
/// arrange overlay, a live toast, the Thermal view (its animating map is the
/// view body, not a `hits.panels()` card), a view switch, or a resize / first
/// frame.
fn can_partial(app: &App, rs: &RenderState, area: Rect) -> bool {
    app.config.motion
        && !app.paused
        && app.modal.is_none()
        && app.arrange.is_none()
        && app.toast.is_none()
        && app.view != View::Thermal
        && rs.last_view == Some(app.view)
        && rs.last_frame.as_ref().is_some_and(|b| b.area == area)
}

/// Restore the previous frame, then re-render only the animating cards plus the
/// header/footer (which carry the clock and the HUD frame counter — the only
/// wall-clock cells). Every other cell is a byte copy of the last full frame.
fn draw_partial(
    f: &mut Frame<'_>,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
) {
    let screen = f.area();
    let [header, _body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(screen);

    // Snapshot the animating cards from the prior (still-valid) hit map before
    // borrowing the buffer; the layout is unchanged so those rects still hold.
    let regions: Vec<(Rect, PanelKind)> = hits.panels().filter(|(_, k)| k.animates()).collect();

    let buf = f.buffer_mut();
    buf.content.clone_from(
        &rs.last_frame
            .as_ref()
            .expect("can_partial checked last_frame")
            .content,
    );

    // Header + footer redraw every frame (clock, HUD counter). A scratch hit
    // map absorbs their target pushes so the real one — still correct for this
    // unchanged layout — is left intact for mouse hit-testing.
    let mut scratch = HitMap::default();
    panels::header::header(buf, header, app, th, &mut scratch);
    panels::header::footer(buf, footer, app, th, &mut scratch);

    // Each animating card, from the exact per-rect initial condition a full
    // frame leaves (a reset buffer under the whole-screen bg fill), then the
    // same panel + nav paint `card_capped` would run.
    for (rect, kind) in regions {
        reset_rect(buf, rect);
        fill_bg(rect, buf, th.bg);
        paint_panel(buf, rect, app, th, &mut scratch, rs, kind, 4);
    }

    // Idempotent on the restored (already-presented) cells, so a whole-buffer
    // scan reproduces a full frame's output over the re-rendered rects too.
    if super::glyphs::active(app.config.glyphs) {
        super::glyphs::octantize_buffer(buf);
    }
    if !super::theme::truecolor_supported() {
        super::theme::quantize_buffer(buf);
    }
}

/// Blank a rect to the state a fresh (post-swap) ratatui buffer starts in, so a
/// partial re-render begins from the same initial condition as a full frame.
fn reset_rect(buf: &mut ratatui::buffer::Buffer, rect: Rect) {
    let clip = rect.intersection(buf.area);
    for y in clip.top()..clip.bottom() {
        for x in clip.left()..clip.right() {
            buf[(x, y)].reset();
        }
    }
}

fn draw_full(
    f: &mut Frame<'_>,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
) {
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
        View::Processes => processes_view(buf, body, app, th, hits, rs),
        View::Thermal => thermal::render(buf, body, app, th, hits, rs),
        View::Connections => panels::flows::render(buf, body, app, th, hits, rs),
    }

    panels::header::footer(buf, footer, app, th, hits);
    overlays::render(buf, screen, app, th, hits);

    // Upgrade braille graphs to solid octant glyphs where the terminal (or
    // the user) says they render — chars first, colors second; the passes
    // are independent, both one buffer scan before ratatui's cell diff.
    if super::glyphs::active(app.config.glyphs) {
        super::glyphs::octantize_buffer(buf);
    }
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
    rs: &mut RenderState,
) {
    let cap = app.config.procs_panes;
    let fit = panels::procs::max_panes(body.width.saturating_sub(2));
    let table_w = panels::procs::preferred_width(cap);
    if fit > cap && body.width.saturating_sub(table_w) >= 40 {
        let [table, widgets] =
            Layout::horizontal([Constraint::Length(table_w), Constraint::Min(0)]).areas(body);
        panels::procs::render(buf, table, app, th, hits, cap);
        widget_grid(buf, widgets, app, th, hits, rs);
    } else {
        panels::procs::render(buf, body, app, th, hits, 4);
    }
}

/// A row-major grid of metric panels filling whatever rect the process
/// table left over: 1–2 columns, 2–4 rows, most-useful panels first.
fn widget_grid(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
) {
    let cols = (area.width / 100).clamp(1, 2);
    let rows = (area.height / 9).clamp(2, 4);
    let mut slots: Vec<PanelKind> = vec![
        PanelKind::Cpu,
        PanelKind::Net,
        PanelKind::Mem,
        PanelKind::Disk,
        PanelKind::Power,
        PanelKind::Gpu,
        PanelKind::Temps,
    ];
    if app.battery.is_some() {
        slots.push(PanelKind::Battery);
    }
    // The table is this view's own subject, so a slot that now resolves to it
    // yields to the next panel rather than drawing a second copy beside it.
    slots.retain(|&slot| app.config.arrangement.at(slot) != PanelKind::Procs);
    let (cw, rh) = (area.width / cols, area.height / rows);
    for (i, slot) in slots.into_iter().take((cols * rows) as usize).enumerate() {
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
        card(buf, rect, app, th, hits, rs, slot);
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

    // Panel heights adapt to total height. The top row earns two extra
    // rows on mid-height terminals: the battery card's Sankey needs the
    // vertical budget for proportional ribbons (and the CPU core bands
    // stretch to match).
    let cpu_h = match body.height {
        h if h >= 44 => 12,
        h if h >= 36 => 11,
        _ => 9,
    };
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
        card(buf, map_col, app, th, hits, rs, PanelKind::HeatMap);

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
        card(buf, cpu_a, app, th, hits, rs, PanelKind::Cpu);
        card(buf, power_a, app, th, hits, rs, PanelKind::Power);
        card(buf, battery_a, app, th, hits, rs, PanelKind::Battery);

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
            card(buf, gpu_a, app, th, hits, rs, PanelKind::Gpu);
            card(buf, mem_a, app, th, hits, rs, PanelKind::Mem);
            card(buf, net_a, app, th, hits, rs, PanelKind::Net);

            let [table, disk_a, temps_a] = Layout::horizontal([
                Constraint::Length(table_w),
                Constraint::Fill(1),
                Constraint::Fill(1),
            ])
            .areas(procs_area);
            card_capped(buf, table, app, th, hits, rs, PanelKind::Procs, cap);
            card(buf, disk_a, app, th, hits, rs, PanelKind::Disk);
            card(buf, temps_a, app, th, hits, rs, PanelKind::Temps);
        } else {
            let [gpu_a, mem_a, net_a, disk_a, temps_a] =
                Layout::horizontal([Constraint::Percentage(20); 5]).areas(mid);
            card(buf, gpu_a, app, th, hits, rs, PanelKind::Gpu);
            card(buf, mem_a, app, th, hits, rs, PanelKind::Mem);
            card(buf, net_a, app, th, hits, rs, PanelKind::Net);
            card(buf, disk_a, app, th, hits, rs, PanelKind::Disk);
            card(buf, temps_a, app, th, hits, rs, PanelKind::Temps);

            card(buf, procs_area, app, th, hits, rs, PanelKind::Procs);
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
        let mut slots: Vec<(u16, PanelKind)> = vec![(6, PanelKind::Cpu), (4, PanelKind::Power)];
        if card_present(app, PanelKind::Battery) {
            slots.push((7, PanelKind::Battery));
        }
        slots.extend([
            (4, PanelKind::Gpu),
            (4, PanelKind::Mem),
            (6, PanelKind::Net),
            (4, PanelKind::Disk),
            (5, PanelKind::Temps),
            (6, PanelKind::HeatMap),
            (18, PanelKind::Procs),
        ]);

        let widths: Vec<Constraint> = slots.iter().map(|&(w, _)| Constraint::Fill(w)).collect();
        let rects = Layout::horizontal(widths).split(body);
        // Rects are computed up front, so rendering is a plain sequence — each
        // call releases its borrow before the next.
        for (&(_, slot), &rect) in slots.iter().zip(rects.iter()) {
            card(buf, rect, app, th, hits, rs, slot);
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
        card(buf, cpu_a, app, th, hits, rs, PanelKind::Cpu);
        card(buf, power_a, app, th, hits, rs, PanelKind::Power);
        card(buf, battery_a, app, th, hits, rs, PanelKind::Battery);

        let [gpu_a, mem_a, net_a, disk_a, temps_a] =
            Layout::horizontal([Constraint::Percentage(20); 5]).areas(mid);
        card(buf, gpu_a, app, th, hits, rs, PanelKind::Gpu);
        card(buf, mem_a, app, th, hits, rs, PanelKind::Mem);
        card(buf, net_a, app, th, hits, rs, PanelKind::Net);
        card(buf, disk_a, app, th, hits, rs, PanelKind::Disk);
        card(buf, temps_a, app, th, hits, rs, PanelKind::Temps);

        // Big terminals get the chassis heat map inline, beside the
        // process table (sized to the chassis aspect, capped at 45%).
        if width >= 156 && procs_area.height >= 16 {
            let aspect_w = (f32::from(procs_area.height - 2) * 3.06) as u16 + 4;
            let map_w = aspect_w.min(procs_area.width * 45 / 100);
            let [procs_l, map_r] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(map_w)])
                    .areas(procs_area);
            card(buf, procs_l, app, th, hits, rs, PanelKind::Procs);
            card(buf, map_r, app, th, hits, rs, PanelKind::HeatMap);
        } else {
            card(buf, procs_area, app, th, hits, rs, PanelKind::Procs);
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
        card(buf, cpu_a, app, th, hits, rs, PanelKind::Cpu);
        card(buf, power_a, app, th, hits, rs, PanelKind::Power);

        let [mid_top, mid_bottom, mid_third] = Layout::vertical([
            Constraint::Length(mid_h),
            Constraint::Length(net2),
            Constraint::Min(0), // mid_h when tall, zero otherwise
        ])
        .areas(mid);
        let [gpu_a, mem_a] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(mid_top);
        card(buf, gpu_a, app, th, hits, rs, PanelKind::Gpu);
        card(buf, mem_a, app, th, hits, rs, PanelKind::Mem);
        let [net_a, right_a] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(mid_bottom);
        card(buf, net_a, app, th, hits, rs, PanelKind::Net);
        if tall {
            // Row 2 keeps battery (temps moves to row 3 beside disk); on
            // desktops without a battery, disk takes the full third row.
            if card_present(app, PanelKind::Battery) {
                card(buf, right_a, app, th, hits, rs, PanelKind::Battery);
                let [disk_a, temps_a] =
                    Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .areas(mid_third);
                card(buf, disk_a, app, th, hits, rs, PanelKind::Disk);
                card(buf, temps_a, app, th, hits, rs, PanelKind::Temps);
            } else {
                card(buf, right_a, app, th, hits, rs, PanelKind::Temps);
                card(buf, mid_third, app, th, hits, rs, PanelKind::Disk);
            }
        } else if card_present(app, PanelKind::Battery) && body.height < 40 {
            card(buf, right_a, app, th, hits, rs, PanelKind::Battery);
        } else {
            card(buf, right_a, app, th, hits, rs, PanelKind::Temps);
        }

        card(buf, procs_area, app, th, hits, rs, PanelKind::Procs);
    } else {
        // Narrow: single stacked column, priority order, whatever fits.
        let heights = [
            cpu_h.min(9),
            6,  // power
            6,  // gpu
            6,  // memory
            10, // network (mirrored graph + connectivity need the rows)
            6,  // disk
            if card_present(app, PanelKind::Battery) {
                7
            } else {
                0
            },
        ];
        let mut y = body.y;
        let panels_fns: [(u16, PanelKind); 7] = [
            (heights[0], PanelKind::Cpu),
            (heights[1], PanelKind::Power),
            (heights[2], PanelKind::Gpu),
            (heights[3], PanelKind::Mem),
            (heights[4], PanelKind::Net),
            (heights[5], PanelKind::Disk),
            (heights[6], PanelKind::Battery),
        ];
        for (h, slot) in panels_fns {
            if h == 0 {
                continue;
            }
            // Always leave room for the process list.
            if y + h + procs_min > body.bottom() {
                break;
            }
            let area = Rect::new(body.x, y, body.width, h);
            card(buf, area, app, th, hits, rs, slot);
            y += h;
        }
        if y < body.bottom() {
            let procs_area = Rect::new(body.x, y, body.width, body.bottom() - y);
            card(buf, procs_area, app, th, hits, rs, PanelKind::Procs);
        }
    }
}

type PanelFn = fn(&mut ratatui::buffer::Buffer, Rect, &App, &Theme);

/// Whether the card that resolves for `slot` has anything to show. Only
/// BATTERY is conditional (desktops have none), and after a rearrangement
/// that condition has to follow the *panel*, not the position it landed in —
/// otherwise swapping battery away from its home slot would blank whichever
/// card took its place.
fn card_present(app: &App, slot: PanelKind) -> bool {
    app.config.arrangement.at(slot) != PanelKind::Battery || app.battery.is_some()
}

/// Render whichever card the user has assigned to `slot`'s home position,
/// register it as a nav + drag target, and paint its affordance.
///
/// Geometry belongs to the slot: the rect was chosen for this position, and a
/// rearrangement only changes which panel draws into it (see
/// [`crate::arrange`]). A panel switched off on the PANELS page leaves the
/// slot empty — these layouts allocate rects by percentage, so a hidden card
/// leaves a hole rather than reflowing the rows around it.
fn card(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
    slot: PanelKind,
) {
    card_capped(buf, area, app, th, hits, rs, slot, 4);
}

/// [`card`] for a slot whose geometry is the process table's, carrying the
/// pane cap that slot was measured with.
#[allow(clippy::too_many_arguments)]
fn card_capped(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
    slot: PanelKind,
    cap: u16,
) {
    let kind = app.config.arrangement.at(slot);
    if !app.config.panel_visible(kind) || !card_present(app, slot) {
        // While a rearrangement is in flight an empty slot is still a place
        // a card can be dropped — otherwise hiding a panel would make its
        // (often roomier) position permanently unreachable.
        if app.arrange.is_some() {
            empty_slot(buf, area, app, th, hits, kind);
        }
        return;
    }
    // The target goes down *before* the panel renders: push order is z-order,
    // and the process table's rows must win the hit test over the card they
    // sit on. Registering the backdrop first is what lets the table drag by
    // its title bar while a click on a row still selects that process.
    hits.push(area, Target::Panel(kind));
    paint_panel(buf, area, app, th, hits, rs, kind, cap);
}

/// Render one card's body + nav affordance for the given DISPLAYED `kind`.
/// Shared by [`card_capped`] (full frame, after it registers the hit target)
/// and [`draw_partial`] (motion frame), so an animating card repaints
/// identically on either path.
#[allow(clippy::too_many_arguments)]
fn paint_panel(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &mut App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
    kind: PanelKind,
    cap: u16,
) {
    let render: PanelFn = match kind {
        PanelKind::Cpu => panels::cpu::render,
        PanelKind::Gpu => panels::gpu::render,
        PanelKind::Mem => panels::mem::render,
        PanelKind::Net => panels::net::render,
        PanelKind::Disk => panels::disk::render,
        PanelKind::Power => panels::power::render,
        PanelKind::Temps => panels::temps::render,
        PanelKind::Battery => panels::battery::render,
        // These two need more than `&App`, so they dispatch here rather than
        // through a plain fn pointer.
        PanelKind::HeatMap => {
            thermal::map_panel(buf, area, app, th, rs);
            nav(buf, area, app, th, kind);
            return;
        }
        PanelKind::Procs => {
            panels::procs::render(buf, area, app, th, hits, cap);
            nav(buf, area, app, th, kind);
            return;
        }
    };
    render(buf, area, app, th);
    nav(buf, area, app, th, kind);
}

/// The placeholder a switched-off card leaves behind while something is being
/// dragged: a dashed frame naming the panel that lives here, so the hole reads
/// as a destination rather than as damage.
fn empty_slot(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
    kind: PanelKind,
) {
    hits.push(area, Target::Panel(kind));
    let clipped = area.intersection(buf.area);
    if clipped.width < 2 || clipped.height < 2 {
        return;
    }
    let dashed = Style::default().fg(th.dim).bg(th.bg);
    let (top, bottom) = (clipped.top(), clipped.bottom() - 1);
    for x in clipped.left()..clipped.right() {
        buf[(x, top)].set_char('╌').set_style(dashed);
        buf[(x, bottom)].set_char('╌').set_style(dashed);
    }
    for y in clipped.top()..clipped.bottom() {
        buf[(clipped.left(), y)].set_char('╎').set_style(dashed);
        buf[(clipped.right() - 1, y)]
            .set_char('╎')
            .set_style(dashed);
    }
    let label = format!(" {} hidden ", kind.title());
    let w = label.chars().count() as u16;
    if clipped.width > w + 2 {
        buf.set_span(clipped.left() + 1, top, &Span::styled(label, dashed), w);
    }
    nav(buf, area, app, th, kind);
}

/// Golden-frame snapshots: the fixture `App` rendered through the real
/// `draw` entry point. Glyphs only (colors are covered by theme unit tests),
/// so the snapshots are identical across truecolor and 256-color hosts.
/// After an intentional redesign: `cargo insta review`.
#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{RenderState, can_partial, draw};
    use crate::app::{App, Modal, View};
    use crate::testutil as tu;
    use crate::ui::theme;
    use crate::ui::widgets::HitMap;

    /// The pixel-parity contract for the motion optimization: a partial repaint
    /// (restore the previous frame, re-render only the animating cards) must
    /// produce the same body a full compose would at the same instant. The one
    /// wall-clock-driven accent (heat-map fan spin) is neutralized so the frame
    /// is a pure function of the pinned `frame_now`; header/footer carry the
    /// live clock, so the body — everything between them — is what must match.
    #[test]
    fn partial_repaint_matches_full() {
        use std::time::{Duration, Instant};
        let make = || {
            let mut app = tu::app();
            app.config.motion = true;
            app.paused = false;
            app.modal = None;
            app.arrange = None;
            app.toast = None;
            if let Some(t) = app.temps.as_mut() {
                for f in &mut t.fans {
                    f.rpm = 0.0;
                }
            }
            app
        };
        // Fixed relative timeline: tiers stamped at `base`, frames near the
        // start and end of the tick so the conveyor has genuinely advanced.
        let base = Instant::now();
        let t0 = base + Duration::from_millis(25);
        let t1 = base + Duration::from_millis(215);
        let arm = |app: &mut App, view: View, fnow: Instant| {
            app.view = view;
            app.motion_clock.fast = Some(base);
            app.motion_clock.power = Some(base);
            app.motion_clock.temps = Some(base);
            app.frame_now = fnow;
        };
        let th = theme::resolve(&make().config);

        for view in [View::Overview, View::Processes, View::Connections] {
            for (w, h) in [(80u16, 24u16), (120, 36), (160, 45), (200, 50)] {
                let area = ratatui::layout::Rect::new(0, 0, w, h);

                // Partial path: a full frame at t0 primes `last_frame`, then a
                // motion frame at t1 on the SAME RenderState.
                let mut app_p = make();
                let mut term_p = Terminal::new(TestBackend::new(w, h)).expect("backend");
                let mut hits_p = HitMap::default();
                let mut rs_p = RenderState::default();
                arm(&mut app_p, view, t0);
                term_p
                    .draw(|f| draw(f, &mut app_p, &th, &mut hits_p, &mut rs_p, false))
                    .expect("draw");
                arm(&mut app_p, view, t1);
                assert!(
                    can_partial(&app_p, &rs_p, area),
                    "partial branch must be taken ({view:?} {w}x{h}) or the test is vacuous"
                );
                term_p
                    .draw(|f| draw(f, &mut app_p, &th, &mut hits_p, &mut rs_p, true))
                    .expect("draw");
                let partial = term_p.backend().buffer().clone();

                // Full path: a fresh render straight at t1.
                let mut app_f = make();
                let mut term_f = Terminal::new(TestBackend::new(w, h)).expect("backend");
                let mut hits_f = HitMap::default();
                let mut rs_f = RenderState::default();
                arm(&mut app_f, view, t1);
                term_f
                    .draw(|f| draw(f, &mut app_f, &th, &mut hits_f, &mut rs_f, false))
                    .expect("draw");
                let full = term_f.backend().buffer().clone();

                // The body (between header row 0 and footer row h-1) must be
                // byte-identical — symbol AND style — to a full compose.
                for y in 1..h.saturating_sub(1) {
                    for x in 0..w {
                        assert_eq!(
                            partial[(x, y)],
                            full[(x, y)],
                            "cell ({x},{y}) differs partial vs full ({view:?} {w}x{h})"
                        );
                    }
                }
            }
        }
    }

    /// Per-frame render cost, full compose vs partial motion repaint. Not an
    /// assertion — run with `cargo test bench_partial_vs_full -- --ignored
    /// --nocapture` to quantify the motion-frame speedup on this machine.
    #[test]
    #[ignore = "timing measurement; run with --ignored --nocapture"]
    fn bench_partial_vs_full() {
        use std::time::{Duration, Instant};
        let mut app = tu::app();
        app.config.motion = true;
        app.paused = false;
        app.modal = None;
        app.arrange = None;
        app.toast = None;
        let th = theme::resolve(&app.config);
        let (w, h) = (200u16, 55u16);
        let base = Instant::now();
        app.motion_clock.fast = Some(base);
        app.motion_clock.power = Some(base);
        app.motion_clock.temps = Some(base);
        let n = 4000u32;
        let phase = |i: u32| base + Duration::from_millis(10 + u64::from(i % 240));

        println!("\nrender @{w}x{h}, full compose vs partial motion repaint:");
        for view in [View::Overview, View::Processes, View::Connections] {
            app.view = view;
            let mut term = Terminal::new(TestBackend::new(w, h)).expect("backend");
            let mut hits = HitMap::default();
            let mut rs = RenderState::default();

            // Prime last_frame, then time full composes.
            app.frame_now = phase(0);
            term.draw(|f| draw(f, &mut app, &th, &mut hits, &mut rs, false))
                .expect("draw");
            let t = Instant::now();
            for i in 0..n {
                app.frame_now = phase(i);
                term.draw(|f| draw(f, &mut app, &th, &mut hits, &mut rs, false))
                    .expect("draw");
            }
            let full_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(n);

            // Re-prime, then time partial motion repaints.
            term.draw(|f| draw(f, &mut app, &th, &mut hits, &mut rs, false))
                .expect("draw");
            assert!(can_partial(
                &app,
                &rs,
                ratatui::layout::Rect::new(0, 0, w, h)
            ));
            let t = Instant::now();
            for i in 0..n {
                app.frame_now = phase(i);
                term.draw(|f| draw(f, &mut app, &th, &mut hits, &mut rs, true))
                    .expect("draw");
            }
            let partial_us = t.elapsed().as_secs_f64() * 1e6 / f64::from(n);

            let name = format!("{view:?}");
            println!(
                "  {name:12}: full={full_us:6.1}µs  partial={partial_us:6.1}µs  \
                 {:.2}x  saved={:.1}µs",
                full_us / partial_us,
                full_us - partial_us,
            );
        }
        println!();
    }

    /// One frame at `w`×`h`, right-trimmed, one line per row.
    fn frame(app: &mut App, w: u16, h: u16) -> String {
        let th = theme::resolve(&app.config);
        let mut term = Terminal::new(TestBackend::new(w, h)).expect("backend");
        let mut hits = HitMap::default();
        let mut rs = RenderState::default();
        term.draw(|f| draw(f, app, &th, &mut hits, &mut rs, false))
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
            // Two build/wall-clock artifacts get normalized so the frames stay
            // stable across runs and releases: the header clock, and the about
            // page's crate version (`mxmon X.Y.Z`), which changes every release.
            // The details modal's "started … ago" used to need a filter too, but
            // redacting it only normalized the text; the padding after it still
            // tracked the real width. The fixture pins a fixed age instead
            // (`testutil::procs`), so the string is constant and the frame can
            // be asserted verbatim.
            filters => vec![
                (r"\d{2}:\d{2}:\d{2}", "HH:MM:SS"),
                (r"mxmon \d+\.\d+\.\d+", "mxmon X.Y.Z"),
            ],
            omit_expression => true,
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(name, frame(app, w, h));
        });
    }

    #[test]
    fn zzz_scratch_dump() {
        let Ok(dir) = std::env::var("MXMON_DUMP_DIR") else {
            return;
        };
        let mut app = tu::app();
        app.view = View::Overview;
        for spec in std::env::var("MXMON_DUMP_SIZES")
            .unwrap_or_default()
            .split(',')
        {
            let Some((w, h)) = spec.split_once('x') else {
                continue;
            };
            let (w, h): (u16, u16) = (w.parse().unwrap(), h.parse().unwrap());
            std::fs::write(format!("{dir}/{w}x{h}.txt"), frame(&mut app, w, h)).unwrap();
        }
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
    fn graph_window_extremes_render_stably() {
        // ×1 locks the exact-passthrough contract (every tick is a dot,
        // pre-zoom behavior, bit for bit); ×8 locks the deepest aggregation
        // through full rings — together they bracket the default ×4 the
        // other snapshots exercise.
        let mut app = tu::app();
        app.view = View::Overview;
        app.config.graph_window = 1;
        snap("overview_x1_160x45", &mut app, 160, 45);
        app.config.graph_window = 8;
        snap("overview_zoom8_160x45", &mut app, 160, 45);
    }

    #[test]
    fn octant_pass_renders_stably() {
        // The full pipeline with the octant upgrade forced: every braille
        // graph in the frame comes out as solid octant/block glyphs. Locks
        // the pass end-to-end (env-independent — `Octant` never probes).
        let mut app = tu::app();
        app.config.glyphs = crate::config::Glyphs::Octant;
        app.view = View::Overview;
        snap("overview_octants_160x45", &mut app, 160, 45);
    }

    /// The process table drags by its title bar, and its rows keep winning
    /// the hit test over the card they sit on. Push order is z-order, so the
    /// card's target has to go down *before* the table renders — register it
    /// after (as the affordance pass once did) and a click would land on the
    /// card instead of the process, silently breaking selection and kill.
    #[test]
    fn process_rows_outrank_the_card_they_sit_on() {
        use crate::ui::widgets::{PanelKind, Target};
        let mut app = tu::app();
        app.view = View::Overview;
        let th = theme::resolve(&app.config);
        let mut term = Terminal::new(TestBackend::new(160, 45)).expect("backend");
        let mut hits = HitMap::default();
        let mut rs = RenderState::default();
        term.draw(|f| draw(f, &mut app, &th, &mut hits, &mut rs, false))
            .expect("draw");

        let (table, _) = hits
            .panels()
            .find(|&(_, k)| k == PanelKind::Procs)
            .expect("the table is a card");
        // Its title bar is the drag handle…
        assert_eq!(
            hits.hit(table.x + 2, table.y),
            Some(Target::Panel(PanelKind::Procs)),
        );
        // …while the body belongs to the rows and the list beneath them.
        let body = hits.hit(table.x + 2, table.y + 3);
        assert!(
            matches!(body, Some(Target::ProcRow(_) | Target::ProcHeader(_))),
            "the table body must not read as the card: {body:?}"
        );
    }

    /// Rearranged cards, and the affordances shown while rearranging them.
    /// The point of the first frame is that the *rects* are identical to
    /// `overview_160x45` — only their tenants differ, which is the whole
    /// contract of [`crate::arrange`].
    #[test]
    fn rearranged_cards_render_stably() {
        use crate::app::Arranging;
        use crate::ui::widgets::PanelKind;
        let mut app = tu::app();
        app.view = View::Overview;
        // The process table into the CPU slot (and CPU into the table's), plus
        // the heat map traded with GPU — a metric card, a table, and a
        // `&mut RenderState` panel all landing somewhere new.
        app.config
            .arrangement
            .swap(PanelKind::Cpu, PanelKind::Procs);
        app.config
            .arrangement
            .swap(PanelKind::Gpu, PanelKind::HeatMap);
        snap("overview_arranged_160x45", &mut app, 160, 45);

        // Mid-rearrangement: one card held, the cursor on where it would land,
        // and a switched-off panel offering its dashed placeholder as a
        // destination — the only time an empty slot is a drop target.
        app.config.show_temps = false;
        app.arrange = Some(Arranging::Mode {
            cursor: PanelKind::Mem,
            held: Some(PanelKind::Net),
        });
        snap("overview_arranging_160x45", &mut app, 160, 45);
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
        let modals: [(&str, Modal); 3] = [
            ("sort", Modal::SortMenu { selected: 2 }),
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

    /// The inspector, tab by tab — the home of every slow-tier fact that has
    /// no room on a card, so each page is worth a golden frame.
    #[test]
    fn inspector_renders_stably() {
        let mut app = tu::app();
        app.view = View::Overview;
        for (i, tab) in crate::app::INSPECT_TABS.into_iter().enumerate() {
            app.modal = Some(Modal::Inspect { tab: i });
            snap(
                &format!("inspect_{}_120x36", tab.title()),
                &mut app,
                120,
                36,
            );
        }
        // A tab index past the end is hostile input, not a panic.
        app.modal = Some(Modal::Inspect { tab: 99 });
        snap("inspect_clamped_120x36", &mut app, 120, 36);
    }

    /// Hiding cards must reshape the dashboard without leaving a hole — and
    /// hiding all of them is a legal configuration, not a crash.
    #[test]
    fn hidden_panels_render_stably() {
        let mut app = tu::app();
        app.view = View::Overview;
        app.config.show_gpu = false;
        app.config.show_temps = false;
        app.config.show_battery = false;
        snap("overview_hidden_panels_160x45", &mut app, 160, 45);
        for f in [
            &mut app.config.show_cpu,
            &mut app.config.show_mem,
            &mut app.config.show_net,
            &mut app.config.show_disk,
            &mut app.config.show_power,
        ] {
            *f = false;
        }
        snap("overview_no_panels_160x45", &mut app, 160, 45);
    }

    /// The inspector before the slow tier has reported, and on a machine with
    /// no battery: every page must say so rather than render confident zeros.
    #[test]
    fn inspector_reports_absence_rather_than_zeros() {
        let mut app = tu::app();
        app.storage = None;
        app.kernel = None;
        app.battery = None;
        for tab in 0..crate::app::INSPECT_TABS.len() {
            app.modal = Some(Modal::Inspect { tab });
            let rendered = frame(&mut app, 120, 36);
            assert!(
                rendered.contains("sampling…") || rendered.contains("no battery"),
                "tab {tab} must admit it has nothing yet"
            );
        }
    }

    /// A failing drive has to be legible as failing.
    #[test]
    fn a_failing_drive_reads_as_failing() {
        let mut app = tu::app();
        if let Some(s) = app.storage.as_mut()
            && let Some(m) = s.smart.as_mut()
        {
            m.critical_warning = 0x01;
            m.media_errors = 7;
            m.available_spare_pct = 3;
        }
        app.modal = Some(Modal::Inspect { tab: 0 });
        assert!(frame(&mut app, 120, 36).contains("FAILING"));
    }

    /// The settings card, page by page: the surface that replaced both the
    /// old settings modal and the help overlay, so every page — including the
    /// two capture modes — is worth a golden frame.
    #[test]
    fn settings_card_renders_stably() {
        let mut app = tu::app();
        app.view = View::Overview;
        app.modal = Some(Modal::Settings);
        for (i, section) in crate::settings::SECTIONS.into_iter().enumerate() {
            app.settings = crate::app::SettingsUi {
                section: i,
                row: 0,
                edit: None,
            };
            snap(
                &format!("settings_{}_120x36", section.title()),
                &mut app,
                120,
                36,
            );
        }
        // A small terminal: the card fills what it can and the rows that fit
        // still line up.
        app.settings.section = 0;
        snap("settings_appearance_80x24", &mut app, 80, 24);
        // Both capture modes.
        app.settings = crate::app::SettingsUi {
            section: 4, // network
            row: 1,     // ping host
            edit: Some(crate::app::Edit::Text {
                id: crate::settings::Id::PingHost,
                buf: "9.9.9.9".into(),
            }),
        };
        snap("settings_editing_120x36", &mut app, 120, 36);
        app.settings = crate::app::SettingsUi {
            section: 5, // keys
            row: 2,
            edit: Some(crate::app::Edit::Capture {
                action: crate::keys::Action::ViewThermal,
            }),
        };
        snap("settings_capture_120x36", &mut app, 120, 36);
    }
}
