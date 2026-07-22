//! Thermal view: a live isotherm map of the MacBook chassis — marching-
//! squares contour rings at fixed °C levels drawn over the theme background,
//! built from inverse-distance-weighted interpolation over physically-
//! anchored sensors, plus the full grouped sensor list.
//!
//! Every sensor is pinned to its physical spot on the board (SoC die block
//! center, PMU rails flanking it, fans at the top corners, battery in the
//! lower deck, …) and — when the map is wide enough — labeled in place.
//! Color is ABSOLUTE (`theme::temp_ratio`: 25° ambient → throttle ceiling,
//! the same mapping as temperature text everywhere): an idle chassis is a
//! calm, near-empty deck with a couple of cool rings, and pockets past 85°
//! fill solid and bloom. Nothing glows unless the silicon actually does.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::collect::temps::{SensorGroup, TempSample, natural_key};
use crate::ui::layout::RenderState;
use crate::ui::panels::{chrome, chrome_with, line};
use crate::ui::schematic::{self, Geometry};
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target, fill_bg};

/// A sensor pinned to a normalized (x, y) position on the chassis
/// (0,0 = top-left at the hinge, 1,1 = bottom-right at the palm rest).
/// `tag` is the compact on-map label ("P7", "SSD"); untagged sensors
/// only shape the heat field.
struct Placed {
    x: f32,
    y: f32,
    temp: f32,
    tag: Option<String>,
}

/// Cached isotherm layer with temporal easing: displayed anchor
/// temperatures glide toward each new sample (exponential, τ ≈ 300 ms),
/// riding on the fast-tier redraws, and recomputation stops once settled —
/// so the rings crawl smoothly as heat moves, then cost nothing at rest.
pub struct HeatSurface {
    map: Rect,
    theme: &'static str,
    /// Whether the isotherm layer was computed (`config.contours`); a live
    /// toggle flips this and forces a rebuild.
    contours: bool,
    /// `temps_seq` of the sample the easing is currently chasing.
    seq: u64,
    /// Eased per-anchor temperatures actually on screen.
    disp: Vec<f32>,
    settled: bool,
    last_step: std::time::Instant,
    /// Contour layer: one optional (glyph, ink) per cell over the bg.
    cells: ContourCells,
    /// "45°" tags pinned onto their own rings (absolute buffer coords).
    ring_labels: RingLabels,
    /// Fan-blade display phase per fan bay, stepped by live RPM.
    fan_phase: [f32; 2],
    last_spin: std::time::Instant,
}

/// Per-row occupancy so on-map labels never overwrite each other; a label
/// that cannot fit near its anchor (±1 row) is dropped, not misplaced.
/// Also tracks contour-glyph occupancy so labels prefer the candidate row
/// that severs the fewest ring lines.
struct LabelField {
    map: Rect,
    rows: Vec<Vec<(u16, u16)>>,
    /// Row-major map-relative flags: true where a contour glyph sits.
    busy: Vec<bool>,
}

impl LabelField {
    fn new(map: Rect) -> Self {
        Self {
            map,
            rows: vec![Vec::new(); map.height as usize],
            busy: Vec::new(),
        }
    }

    /// A field that knows where the isotherm layer drew, so labels dodge it.
    fn with_busy(map: Rect, cells: &ContourCells) -> Self {
        let mut f = Self::new(map);
        f.busy = cells.iter().map(Option::is_some).collect();
        f
    }

    /// Pre-claim a span (the ring's own labels) so sensor labels nudge
    /// around it instead of butting up against or overwriting it.
    fn reserve(&mut self, x: u16, y: u16, w: u16) {
        if y >= self.map.y && y < self.map.bottom() {
            self.rows[(y - self.map.y) as usize].push((x, x + w));
        }
    }

    /// How many contour glyphs a span at (x, y) of width `w` would cut.
    fn ring_cost(&self, x: u16, y: u16, w: u16) -> usize {
        if self.busy.is_empty() {
            return 0;
        }
        let row = (y - self.map.y) as usize * self.map.width as usize;
        (x..x + w)
            .filter(|&xx| self.busy[row + (xx - self.map.x) as usize])
            .count()
    }

    /// Draw `text` centered on the normalized anchor, nudging one row up
    /// or down on collision with other labels and preferring the row that
    /// severs the fewest ring lines. Skips silently when nothing fits.
    fn draw(&mut self, buf: &mut Buffer, fx: f32, fy: f32, text: &str, fg: Color) {
        let w = text.chars().count() as u16;
        if w == 0 || w > self.map.width {
            return;
        }
        let cx = self.map.x + (fx * f32::from(self.map.width - 1)) as u16;
        let cy = self.map.y + (fy * f32::from(self.map.height - 1)) as u16;
        let x = cx
            .saturating_sub(w / 2)
            .clamp(self.map.x, self.map.right().saturating_sub(w));
        let mut best: Option<(usize, i32)> = None;
        for dy in [0i32, -1, 1] {
            let y = i32::from(cy) + dy;
            if y < i32::from(self.map.y) || y >= i32::from(self.map.bottom()) {
                continue;
            }
            let row = &self.rows[(y as u16 - self.map.y) as usize];
            // One column of breathing room between neighbors.
            if !row.iter().all(|&(s, e)| x + w < s || e < x) {
                continue;
            }
            let cost = self.ring_cost(x, y as u16, w);
            if cost == 0 {
                best = Some((0, dy));
                break;
            }
            if best.is_none_or(|(c, _)| cost < c) {
                best = Some((cost, dy));
            }
        }
        let Some((_, dy)) = best else { return };
        let y = (i32::from(cy) + dy) as u16;
        self.rows[(y - self.map.y) as usize].push((x, x + w));
        for (i, ch) in text.chars().enumerate() {
            let cell = &mut buf[(x + i as u16, y)];
            cell.set_char(ch);
            cell.set_fg(fg);
            cell.set_style(Style::new().add_modifier(Modifier::BOLD));
        }
    }
}

pub fn render(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
    rs: &mut RenderState,
) {
    let Some(t) = &app.temps else {
        let inner = chrome(buf, area, "THERMAL", th);
        line(
            buf,
            inner,
            0,
            vec![Span::styled("sampling…", Style::default().fg(th.dim))],
        );
        return;
    };

    // Split: map on the left (60%), sensor list right.
    let list_w = (area.width / 3).clamp(26, 44);
    let map_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(list_w),
        area.height,
    );
    let list_area = Rect::new(map_area.right(), area.y, list_w, area.height);

    render_map(buf, map_area, t, app, th, rs);
    render_sensor_list(
        buf,
        list_area,
        t,
        th,
        hits,
        rs.sensor_scroll,
        (app.soc.tier_low, app.soc.tier_high),
    );
}

/// Standalone chassis-map panel for the overview (no sensor list) — shown
/// there whenever the terminal is large enough to spare the space.
pub fn map_panel(buf: &mut Buffer, area: Rect, app: &App, th: &Theme, rs: &mut RenderState) {
    let Some(t) = &app.temps else {
        let inner = chrome(buf, area, "CHASSIS HEAT MAP", th);
        line(
            buf,
            inner,
            0,
            vec![Span::styled("sampling…", Style::default().fg(th.dim))],
        );
        return;
    };
    render_map(buf, area, t, app, th, rs);
}

fn render_map(
    buf: &mut Buffer,
    area: Rect,
    t: &TempSample,
    app: &App,
    th: &Theme,
    rs: &mut RenderState,
) {
    // Headline: the hottest sensor on the board.
    let hottest = t
        .sensors
        .iter()
        .map(|s| s.temp.0)
        .filter(|v| v.is_finite())
        .fold(f32::NAN, f32::max);
    let headline = if hottest.is_finite() {
        vec![
            Span::styled(
                format!("{:>4}", crate::units::Celsius(hottest)),
                Style::default()
                    .fg(th.temp_color(hottest))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" max", Style::default().fg(th.dim)),
        ]
    } else {
        Vec::new()
    };
    let inner = chrome_with(buf, area, "CHASSIS HEAT MAP", headline, th);
    if inner.width < 20 || inner.height < 8 {
        return;
    }
    fill_bg(inner, buf, th.bg);

    // Chassis proportions: base ≈ 1.45:1 (w:h); cells ≈ 9×19 px, so
    // cols/rows ≈ 1.45 × 19/9 ≈ 3.06. Margin of 1 kept for the outline.
    const COLS_PER_ROW: f32 = 3.06;
    let max_w = f32::from(inner.width.saturating_sub(4));
    let max_h = f32::from(inner.height.saturating_sub(2));
    let (mut w, mut h) = (max_w, max_w / COLS_PER_ROW);
    if h > max_h {
        h = max_h;
        w = h * COLS_PER_ROW;
    }
    let (w, h) = ((w as u16).max(12), (h as u16).max(6));
    let map = Rect::new(
        inner.x + (inner.width - w) / 2,
        inner.y + (inner.height - h) / 2,
        w,
        h,
    );

    // The floorplan grid is the whole point of Full detail: when it fits
    // the die, every core/cluster/region reading gets its own drawn cell
    // and Full engages; when it doesn't, the view degrades to Mid.
    let geom = Geometry::new(&app.soc, t, app.battery.is_some());
    let eligible = schematic::detail(map, app.config.schematic);
    let mut plan = (eligible != schematic::Detail::Off)
        .then(|| schematic::Floorplan::layout(&geom, map, t))
        .flatten();
    let detail = match (&plan, eligible) {
        (Some(_), _) => schematic::Detail::Full,
        (None, schematic::Detail::Full) => schematic::Detail::Mid,
        (None, other) => other,
    };

    let placed = place_sensors(t, &geom, plan.as_mut());
    if placed.is_empty() {
        return;
    }

    // The silkscreen strokes go down first so the isotherm layer always
    // wins contested cells; the reading-grid walls and etched lettering
    // land after the blit.
    schematic::render_silkscreen(buf, map, &geom, detail, th);

    // Cache + easing: recompute only while the field is actually moving
    // (new sample arriving, resize, theme switch, or the contour toggle
    // flipping live); idle redraws are free.
    let contours = app.config.contours;
    let targets: Vec<f32> = placed.iter().map(|p| p.temp).collect();
    let rebuild = rs.heat.as_ref().is_none_or(|hs| {
        hs.map != map
            || hs.theme != th.name
            || hs.contours != contours
            || hs.disp.len() != targets.len()
    });
    if rebuild {
        let (cells, ring_labels) = contour_layer(contours, map, &placed, &targets, th);
        rs.heat = Some(HeatSurface {
            map,
            theme: th.name,
            contours,
            seq: app.temps_seq,
            cells,
            ring_labels,
            disp: targets,
            settled: true,
            last_step: std::time::Instant::now(),
            fan_phase: [0.0; 2],
            last_spin: std::time::Instant::now(),
        });
    } else if let Some(hs) = rs.heat.as_mut() {
        if hs.seq != app.temps_seq {
            hs.seq = app.temps_seq;
            // With the rings hidden nothing on screen animates: stay
            // settled and skip the easing (toggling back on rebuilds
            // fresh at the live temps).
            if hs.contours {
                hs.settled = false;
                hs.last_step = std::time::Instant::now();
            }
        }
        if !hs.settled {
            let dt = hs.last_step.elapsed().as_secs_f32();
            hs.last_step = std::time::Instant::now();
            let alpha = 1.0 - (-dt / 0.3).exp();
            let mut max_err = 0.0f32;
            for (d, &t) in hs.disp.iter_mut().zip(&targets) {
                *d += (t - *d) * alpha;
                max_err = max_err.max((t - *d).abs());
            }
            if max_err < 0.2 {
                hs.disp.copy_from_slice(&targets);
                hs.settled = true;
            }
            (hs.cells, hs.ring_labels) = contour_layer(hs.contours, map, &placed, &hs.disp, th);
        }
    }
    if let Some(hs) = rs.heat.as_mut() {
        let dt = hs.last_spin.elapsed().as_secs_f32();
        hs.last_spin = std::time::Instant::now();
        for (i, fan) in t.fans.iter().take(2).enumerate() {
            hs.fan_phase[i] = schematic::spin(hs.fan_phase[i], fan.rpm, dt);
        }
    }
    let surface = rs.heat.as_ref().expect("surface computed above");
    let cols = map.width as usize;
    for cy in 0..map.height as usize {
        for cx in 0..cols {
            if let Some((ch, ink)) = surface.cells[cy * cols + cx] {
                let cell = &mut buf[(map.x + cx as u16, map.y + cy as u16)];
                cell.set_char(ch);
                cell.set_fg(ink);
                cell.set_bg(th.bg);
            }
        }
    }
    // The reading-grid boxes are data containers, so they stay whole over
    // the rings — an isotherm crossing a zone table enters and exits around
    // it instead of shredding its borders into glyph soup.
    if let Some(plan) = &plan {
        plan.render_walls(buf, th);
    }
    for (x, y, text, ink) in &surface.ring_labels {
        for (i, ch) in text.chars().enumerate() {
            let cell = &mut buf[(x + i as u16, *y)];
            cell.set_char(ch);
            cell.set_fg(*ink);
            cell.set_style(Style::new().add_modifier(Modifier::BOLD));
        }
    }

    // Etched lettering, grid readings, and living accents (fan blades,
    // charge waterline) over the rings but under the sensor labels; the
    // accents skip contour cells so an isotherm is never severed.
    let mut etched = schematic::render_etches(buf, map, &geom, detail, th);
    if let Some(plan) = &plan {
        etched.extend(plan.render_readings(buf, th));
    }
    schematic::render_dynamic(
        buf,
        map,
        &geom,
        detail,
        th,
        t,
        app.battery.as_ref(),
        surface.fan_phase,
        &surface.cells,
    );

    draw_labels(
        buf,
        map,
        t,
        app,
        th,
        &placed,
        surface,
        &geom,
        &etched,
        plan.is_some(),
    );
    draw_outline(buf, inner, map, th, &surface.cells);
}

/// Isotherm levels drawn on the map (°C). Fixed and absolute: a ring means
/// that temperature exists on the deck, full stop.
const ISO_LEVELS: [f32; 7] = [35.0, 45.0, 55.0, 65.0, 75.0, 85.0, 95.0];

/// One optional (glyph, ink) per map cell — the drawn contour layer.
type ContourCells = Vec<Option<(char, Color)>>;
/// Ring labels: (x, y, "55°", ink) in absolute buffer coordinates.
type RingLabels = Vec<(u16, u16, String, Color)>;

/// Marching-squares case → box glyph. Corner bits: TL=8 TR=4 BL=2 BR=1,
/// set when that corner is above the level. Complementary cases share a
/// glyph (the contour between them is the same line).
fn contour_char(case: u8) -> Option<char> {
    match case {
        1 | 14 => Some('╭'),
        2 | 13 => Some('╮'),
        3 | 12 => Some('─'),
        4 | 11 => Some('╰'),
        5 | 10 => Some('│'),
        6 | 9 => Some('┼'), // saddle
        7 | 8 => Some('╯'),
        _ => None, // uniformly above or below
    }
}

/// IDW heat field sampled at every cell corner (`(w+1)×(h+1)`), the grid
/// marching squares walks. Constant 25° ambient pull keeps the far field
/// at deck temperature instead of the coolest sensor's.
fn corner_field(map: Rect, placed: &[Placed], temps: &[f32]) -> Vec<f32> {
    const AMBIENT: f32 = 25.0;
    let nx = map.width as usize + 1;
    let ny = map.height as usize + 1;
    let ambient_w = placed.len() as f32 * 0.8;
    let mut out = vec![0.0f32; nx * ny];
    for gy in 0..ny {
        let fy = gy as f32 / (ny - 1) as f32;
        for gx in 0..nx {
            let fx = gx as f32 / (nx - 1) as f32;
            let mut num = 0.0f32;
            let mut den = 0.0f32;
            for (p, &t) in placed.iter().zip(temps) {
                let dx = fx - p.x;
                let dy = (fy - p.y) * 1.45; // physical aspect correction
                // Soft-core IDW: the +ε removes the hard plateau a clamped
                // 1/d² forms right on top of each anchor.
                let w = 1.0 / (dx * dx + dy * dy + 1.5e-3);
                num += w * t;
                den += w;
            }
            out[gy * nx + gx] = (num + AMBIENT * ambient_w) / (den + ambient_w);
        }
    }
    out
}

/// Ring ink for the `i`-th isotherm level. Levels are spread across the
/// theme's thermal ramp by *index*, topographic-map style, not by absolute
/// position — 35°/45°/55° all live on the ramp's navy floor and were
/// indistinguishable on screen. Ordering still tracks temperature and the
/// on-ring labels carry the exact numbers. Lifted toward white a touch
/// because thin lines need more ink than a filled surface would.
fn ring_ink(th: &Theme, i: usize) -> Color {
    let t = 0.18 + 0.82 * i as f32 / (ISO_LEVELS.len() - 1) as f32;
    if crate::ui::theme::truecolor_supported() {
        match th.thermal.at(t) {
            Color::Rgb(r, g, b) => {
                let lift = |v: u8| v + (f32::from(255 - v) * 0.15) as u8;
                Color::Rgb(lift(r), lift(g), lift(b))
            }
            other => other,
        }
    } else {
        let max = (th.thermal_indexed.len() - 1) as f32;
        Color::Indexed(th.thermal_indexed[(t * max).round() as usize])
    }
}

/// Solid-fill ink for hot cores (raw ramp — already bright up there).
fn fill_ink(th: &Theme, temp: f32) -> Color {
    let t = crate::ui::theme::temp_ratio(temp);
    if crate::ui::theme::truecolor_supported() {
        th.thermal.at(t)
    } else {
        let max = (th.thermal_indexed.len() - 1) as f32;
        Color::Indexed(th.thermal_indexed[(t * max).round() as usize])
    }
}

/// Build the isotherm layer: marching-squares rings at `ISO_LEVELS` over
/// the eased field, hot pockets past 85°/95° filled ▓/█ so a core reads
/// as a glow instead of a black hole, and each ring labeled with its
/// temperature on one of its own horizontal runs.
/// The isotherm layer for the frame — or, with the rings toggled off, a
/// blank layer of the same `w × h` shape. Every consumer (the blit, the
/// accent skip, label dodging, outline T-junctions) indexes the full map,
/// so the stand-in keeps them all total without special-casing, and the
/// field math is skipped rather than merely hidden.
fn contour_layer(
    enabled: bool,
    map: Rect,
    placed: &[Placed],
    temps: &[f32],
    th: &Theme,
) -> (ContourCells, RingLabels) {
    if enabled {
        compute_contours(map, placed, temps, th)
    } else {
        (
            vec![None; map.width as usize * map.height as usize],
            Vec::new(),
        )
    }
}

fn compute_contours(
    map: Rect,
    placed: &[Placed],
    temps: &[f32],
    th: &Theme,
) -> (ContourCells, RingLabels) {
    let f = corner_field(map, placed, temps);
    let (w, h) = (map.width as usize, map.height as usize);
    let nx = w + 1;
    let mut cells: ContourCells = vec![None; w * h];

    // Hot-core fill first; rings overwrite its fringe cleanly.
    for cy in 0..h {
        for cx in 0..w {
            let c = [
                f[cy * nx + cx],
                f[cy * nx + cx + 1],
                f[(cy + 1) * nx + cx],
                f[(cy + 1) * nx + cx + 1],
            ];
            let cmin = c.iter().copied().fold(f32::INFINITY, f32::min);
            if cmin >= 85.0 {
                let avg = c.iter().sum::<f32>() / 4.0;
                let glyph = if cmin >= 95.0 { '█' } else { '▓' };
                cells[cy * w + cx] = Some((glyph, fill_ink(th, avg)));
            }
        }
    }

    // Rings, cool→hot, so hotter rings win contested cells.
    let mut ring_runs: Vec<(usize, f32, Vec<usize>)> = Vec::new();
    for (li, &level) in ISO_LEVELS.iter().enumerate() {
        let ink = ring_ink(th, li);
        let mut runs = Vec::new();
        for cy in 0..h {
            for cx in 0..w {
                let bit = |v: f32| u8::from(v > level);
                let case = (bit(f[cy * nx + cx]) << 3)
                    | (bit(f[cy * nx + cx + 1]) << 2)
                    | (bit(f[(cy + 1) * nx + cx]) << 1)
                    | bit(f[(cy + 1) * nx + cx + 1]);
                if let Some(ch) = contour_char(case) {
                    cells[cy * w + cx] = Some((ch, ink));
                    if ch == '─' {
                        runs.push(cy * w + cx);
                    }
                }
            }
        }
        if !runs.is_empty() {
            ring_runs.push((li, level, runs));
        }
    }

    // Ring labels on wide maps: centered on the longest *straight* run of
    // their own contour so the line reads as flowing through the tag —
    // never stamped over corners, steps, or another ring's cells (that
    // severed lines mid-turn and left dangling fragments).
    let mut labels = Vec::new();
    if map.width >= 30 {
        for (li, level, runs) in &ring_runs {
            let text = format!("{level:.0}°");
            let tw = text.chars().count();
            let (mut best_at, mut best_len) = (0usize, 0usize);
            let (mut cur_at, mut cur_len) = (0usize, 0usize);
            for (k, &idx) in runs.iter().enumerate() {
                if k > 0 && idx == runs[k - 1] + 1 && idx % w != 0 {
                    cur_len += 1;
                } else {
                    (cur_at, cur_len) = (idx, 1);
                }
                if cur_len > best_len {
                    (best_at, best_len) = (cur_at, cur_len);
                }
            }
            // Two cells of visible line on both sides of the tag.
            if best_len >= tw + 4 {
                let start = best_at + (best_len - tw) / 2;
                labels.push((
                    map.x + (start % w) as u16,
                    map.y + (start / w) as u16,
                    text,
                    ring_ink(th, *li),
                ));
            }
        }
    }
    (cells, labels)
}

/// On-map labels. Wide maps label every placed sensor individually;
/// narrow maps fall back to CPU/GPU/SSD aggregates. Seeded with the
/// isotherm layer and the schematic's etched text so labels nudge away
/// from ring lines, ring tags, and silkscreen lettering alike.
#[allow(clippy::too_many_arguments)]
fn draw_labels(
    buf: &mut Buffer,
    map: Rect,
    t: &TempSample,
    app: &App,
    th: &Theme,
    placed: &[Placed],
    surface: &HeatSurface,
    geom: &Geometry,
    etched: &[(u16, u16, u16)],
    grid: bool,
) {
    let mut field = LabelField::with_busy(map, &surface.cells);
    for (x, y, text, _) in &surface.ring_labels {
        field.reserve(*x, *y, text.chars().count() as u16);
    }
    for &(x, y, w) in etched {
        field.reserve(x, y, w);
    }
    let group_avg = |group: SensorGroup| -> Option<f32> {
        let matching: Vec<f32> = t
            .sensors
            .iter()
            .filter(|s| s.group == group)
            .map(|s| s.temp.0)
            .collect();
        (!matching.is_empty()).then(|| matching.iter().sum::<f32>() / matching.len() as f32)
    };

    // Fans above their bays, battery (with charge) on the pack — drawn
    // first so they always win placement.
    for (i, fan) in t.fans.iter().enumerate() {
        let (x, y) = geom.fan_label(i);
        field.draw(buf, x, y, &format!("{:.0}rpm", fan.rpm), th.text);
    }
    if let Some(v) = group_avg(SensorGroup::Battery) {
        let charge = app
            .battery
            .as_ref()
            .map(|b| format!(" · {:.0}%", b.charge.as_percent()))
            .unwrap_or_default();
        let (x, y) = geom.battery_anchor();
        field.draw(buf, x, y, &format!("BATT{charge} {v:.0}°"), th.text);
    }

    if grid {
        // Die readings live in the floorplan's cells; what remains tagged
        // here is the board furniture (PMUs, SSD, airflow, ports, …).
        for p in placed {
            if let Some(tag) = &p.tag {
                field.draw(buf, p.x, p.y, &format!("{tag} {:.0}°", p.temp), th.text);
            }
        }
    } else {
        // No grid at this size — clean aggregates only, never a freeform
        // scatter of per-sensor labels fighting for rows.
        if t.cpu_avg.0 > 0.0 {
            field.draw(
                buf,
                0.49,
                0.24,
                &format!("CPU {:.0}°", t.cpu_avg.0),
                th.text,
            );
        }
        if t.gpu_avg.0 > 0.0 {
            field.draw(
                buf,
                0.49,
                0.43,
                &format!("GPU {:.0}°", t.gpu_avg.0),
                th.text,
            );
        }
        if let Some(v) = group_avg(SensorGroup::Ssd) {
            let (x, y) = geom.ssd_anchor();
            field.draw(buf, x, y, &format!("SSD {v:.0}°"), th.text);
        }
    }
}

/// Chassis frame drawn just outside the heat field: rounded box-drawing in
/// the same glyph family as the rings, so sides meet corners and any
/// isotherm that runs off the measured deck T-junctions into it cleanly.
/// (The old eighth-block frame — ▁▔▕▏ — stroked cell *edges* while every
/// line on the map strokes cell *midlines*: in most fonts it rendered as
/// dashed, torn seams that never met their corners, and rings ended
/// floating a half-cell short of it.)
fn draw_outline(buf: &mut Buffer, inner: Rect, map: Rect, th: &Theme, cells: &ContourCells) {
    let ox0 = map.x.saturating_sub(1);
    let ox1 = map.right();
    let oy0 = map.y.saturating_sub(1);
    let oy1 = map.bottom();
    if oy0 < inner.y || oy1 >= inner.bottom() || ox0 < inner.x || ox1 >= inner.right() {
        return;
    }
    let edge = Style::default().fg(th.border);
    for x in map.left()..map.right() {
        buf.set_span(x, oy0, &Span::styled("─", edge), 1);
        buf.set_span(x, oy1, &Span::styled("─", edge), 1);
    }
    for y in map.top()..map.bottom() {
        buf.set_span(ox0, y, &Span::styled("│", edge), 1);
        buf.set_span(ox1, y, &Span::styled("│", edge), 1);
    }
    buf.set_span(ox0, oy0, &Span::styled("╭", edge), 1);
    buf.set_span(ox1, oy0, &Span::styled("╮", edge), 1);
    buf.set_span(ox0, oy1, &Span::styled("╰", edge), 1);
    buf.set_span(ox1, oy1, &Span::styled("╯", edge), 1);
    // Hinge marker along the top edge for orientation.
    let hinge_w = (map.width / 3).max(8);
    let hinge = "━".repeat(hinge_w as usize);
    buf.set_span(
        map.x + (map.width - hinge_w) / 2,
        oy0,
        &Span::styled(hinge, Style::default().fg(th.dim)),
        hinge_w,
    );

    // Isotherms that exit the deck plug into the frame as T-junctions, in
    // their own ink, instead of ending a half-cell short of it.
    let (w, h) = (map.width as usize, map.height as usize);
    let junction = |buf: &mut Buffer, x: u16, y: u16, ch: char, ink: Color| {
        let cell = &mut buf[(x, y)];
        cell.set_char(ch);
        cell.set_fg(ink);
        cell.set_bg(th.bg);
    };
    for cy in 0..h {
        let y = map.y + cy as u16;
        if let Some((ch, ink)) = cells[cy * w]
            && matches!(ch, '─' | '╮' | '╯' | '┼')
        {
            junction(buf, ox0, y, '├', ink);
        }
        if let Some((ch, ink)) = cells[cy * w + w - 1]
            && matches!(ch, '─' | '╭' | '╰' | '┼')
        {
            junction(buf, ox1, y, '┤', ink);
        }
    }
    for cx in 0..w {
        let x = map.x + cx as u16;
        if let Some((ch, ink)) = cells[cx]
            && matches!(ch, '│' | '╰' | '╯' | '┼')
        {
            junction(buf, x, oy0, '┬', ink);
        }
        if let Some((ch, ink)) = cells[(h - 1) * w + cx]
            && matches!(ch, '│' | '╭' | '╮' | '┼')
        {
            junction(buf, x, oy1, '┴', ink);
        }
    }
}

/// Pin every sensor to its physical spot on the deck. Positions come from
/// the schematic [`Geometry`] — the single source of truth shared with the
/// silkscreen layer — so every anchor lands on drawn hardware: cores and
/// clusters inside the die, die regions along its inner edges, PMUs on the
/// board flanking the package, storage in its module, battery on the pack,
/// airflow at the fan hubs, ports and antenna on the chassis edges.
///
/// With an active [`schematic::Floorplan`], the die-resident groups claim
/// grid cells instead: the plan draws their readings in strict cells (never
/// nudged), so those `Placed` carry no tag and only shape the heat field.
fn place_sensors(
    t: &TempSample,
    geom: &Geometry,
    mut plan: Option<&mut schematic::Floorplan>,
) -> Vec<Placed> {
    let mut counts: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(t.sensors.len());

    for s in &t.sensors {
        let mut bump = |family: u8| -> usize {
            let c = counts.entry(family).or_insert(0);
            let i = *c;
            *c += 1;
            i
        };
        // A die-grid claim wins over the freeform anchor + label.
        let cell = |plan: &mut Option<&mut schematic::Floorplan>,
                    zone: schematic::Zone,
                    i: usize,
                    tag: &str,
                    temp: f32|
         -> Option<(f32, f32)> {
            plan.as_deref_mut()
                .and_then(|p| p.claim(zone, i, tag, temp))
        };
        // Prefer the number in the label ("Die 7" → 7); ordinal fallback.
        let n = natural_key(&s.label).1 as usize;
        let ((x, y), tag) = match s.group {
            SensorGroup::CpuECore => {
                let i = bump(0);
                let tag = format!("{}{}", geom.tier_low, n.max(i + 1));
                match cell(&mut plan, schematic::Zone::ECpu, i, &tag, s.temp.0) {
                    Some(at) => (at, None),
                    None => (geom.ecore(i), plan.is_none().then_some(tag)),
                }
            }
            SensorGroup::CpuPCore => {
                let i = bump(1);
                let tag = format!("{}{}", geom.tier_high, n.max(i + 1));
                match cell(&mut plan, schematic::Zone::PCpu, i, &tag, s.temp.0) {
                    Some(at) => (at, None),
                    None => (geom.pcore(i), plan.is_none().then_some(tag)),
                }
            }
            // GPU clusters grid 8-per-row over the GPU zone; big GPUs have
            // dozens (32 on M3 Max), so off-grid only the first row keeps tags.
            SensorGroup::Gpu if s.label.contains("Cluster") => {
                let i = bump(2);
                let num = n.max(i + 1);
                let tag = format!("G{num}");
                match cell(&mut plan, schematic::Zone::Gpu, i, &tag, s.temp.0) {
                    Some(at) => (at, None),
                    None => (
                        geom.gpu_cluster(i),
                        (plan.is_none() && num <= 8).then_some(tag),
                    ),
                }
            }
            // Per-region HID GPU sensors shape the field without labels.
            SensorGroup::Gpu => (geom.gpu_region(bump(3)), None),
            SensorGroup::Soc if s.label.starts_with("Die") => {
                let i = bump(4);
                let tag = format!("D{}", n.max(i + 1));
                match cell(&mut plan, schematic::Zone::Die, i, &tag, s.temp.0) {
                    Some(at) => (at, None),
                    // With the grid active but this band dropped/full, the
                    // sensor still shapes the field — unlabeled, so nothing
                    // freeform ever fights the grid for cells.
                    None => (geom.die_slot(i), plan.is_none().then_some(tag)),
                }
            }
            SensorGroup::Soc if s.label.starts_with("PMU") => {
                let i = bump(5);
                (geom.pmu_slot(i), Some(format!("PMU{}", n.max(i + 1))))
            }
            SensorGroup::Soc => (geom.soc_misc(bump(6)), None),
            SensorGroup::Ane => match cell(&mut plan, schematic::Zone::Ane, 0, "ANE", s.temp.0) {
                Some(at) => (at, None),
                None => (geom.ane(), plan.is_none().then(|| "ANE".into())),
            },
            SensorGroup::Ssd => (geom.ssd_anchor(), Some("SSD".into())),
            // The aggregate BATT · charge label covers the battery spot.
            SensorGroup::Battery => (geom.battery_anchor(), None),
            SensorGroup::Airflow => {
                let right = s.label.contains("Right");
                let tag = if right { "AIR R" } else { "AIR L" };
                (geom.airflow(right), Some(tag.into()))
            }
            SensorGroup::Charger => {
                let supply = s.label.contains("Supply");
                let tag = if supply { "PSU" } else { "CHG" };
                (geom.charger(supply), Some(tag.into()))
            }
            SensorGroup::Ports => {
                let right = s.label.contains("Right");
                let tag = if right { "TB R" } else { "TB L" };
                (geom.port(right), Some(tag.into()))
            }
            SensorGroup::Wireless => (geom.wifi(), Some("WIFI".into())),
            SensorGroup::Other => {
                if s.label.contains("Trackpad") {
                    (geom.trackpad_anchor(), Some("TPAD".into()))
                } else if s.label.contains("Palm") {
                    (geom.palm(), Some("PALM".into()))
                } else {
                    (geom.other_misc(bump(7)), None)
                }
            }
        };
        out.push(Placed {
            x,
            y,
            temp: s.temp.0,
            tag,
        });
    }
    out
}

fn render_sensor_list(
    buf: &mut Buffer,
    area: Rect,
    t: &TempSample,
    th: &Theme,
    hits: &mut HitMap,
    scroll: usize,
    tiers: (char, char),
) {
    let inner = chrome(buf, area, "SENSORS", th);
    if inner.height == 0 {
        return;
    }
    hits.push(inner, Target::SensorList);
    let dim = Style::default().fg(th.dim);

    // Build display lines: group headers + sensors (GPU capped to 8).
    let mut lines: Vec<(String, Option<f32>, bool)> = Vec::new();
    let mut last_group: Option<SensorGroup> = None;
    let mut gpu_shown = 0;
    for s in &t.sensors {
        if s.group == SensorGroup::Gpu {
            gpu_shown += 1;
            if gpu_shown > 8 {
                continue;
            }
        }
        if last_group != Some(s.group) {
            lines.push((s.group.title_with(tiers.0, tiers.1), None, true));
            last_group = Some(s.group);
        }
        lines.push((s.label.clone(), Some(s.temp.0), false));
    }
    if gpu_shown > 8 {
        lines.push((format!("… {} more GPU sensors", gpu_shown - 8), None, false));
    }

    let visible = inner.height as usize;
    let scroll = scroll.min(lines.len().saturating_sub(visible));
    for (row, (label, temp, is_header)) in lines.iter().skip(scroll).take(visible).enumerate() {
        let y = inner.y + row as u16;
        if *is_header {
            buf.set_span(
                inner.x,
                y,
                &Span::styled(
                    format!("─ {label} "),
                    Style::default().fg(th.title).add_modifier(Modifier::BOLD),
                ),
                inner.width,
            );
        } else {
            buf.set_span(
                inner.x + 1,
                y,
                &Span::styled(label.clone(), Style::default().fg(th.text)),
                inner.width.saturating_sub(8),
            );
            if let Some(v) = temp {
                let text = format!("{v:5.1}°");
                buf.set_span(
                    inner.right().saturating_sub(7),
                    y,
                    &Span::styled(
                        text,
                        Style::default()
                            .fg(th.temp_color(*v))
                            .add_modifier(Modifier::BOLD),
                    ),
                    7,
                );
            }
        }
    }
    if lines.len() > visible {
        buf.set_span(
            inner.right().saturating_sub(12),
            inner.bottom().saturating_sub(1),
            &Span::styled("scroll ↑↓", dim),
            10,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::temps::Sensor;
    use crate::units::Celsius;

    fn sensor(label: &str, group: SensorGroup, temp: f32) -> Sensor {
        Sensor {
            label: label.into(),
            group,
            temp: Celsius(temp),
        }
    }

    #[test]
    fn placement_tags_and_slots() {
        let t = TempSample {
            sensors: vec![
                sensor("E-Core 2", SensorGroup::CpuECore, 70.0),
                sensor("P-Core 12", SensorGroup::CpuPCore, 68.0),
                sensor("GPU Cluster 3", SensorGroup::Gpu, 64.0),
                sensor("GPU Cluster 12", SensorGroup::Gpu, 63.0),
                sensor("Die 7", SensorGroup::Soc, 59.0),
                sensor("PMU 8", SensorGroup::Soc, 41.0),
                sensor("Trackpad", SensorGroup::Other, 26.0),
            ],
            ..Default::default()
        };
        let geom = Geometry::new(&crate::collect::soc::SocInfo::default(), &t, true);
        let placed = place_sensors(&t, &geom, None);
        let tags: Vec<Option<&str>> = placed.iter().map(|p| p.tag.as_deref()).collect();
        assert_eq!(
            tags,
            [
                Some("E2"),
                Some("P12"),
                Some("G3"),
                None, // clusters past 8 shape the field unlabeled
                Some("D7"),
                Some("PMU8"),
                Some("TPAD"),
            ]
        );
        // Every anchor stays on the normalized chassis.
        assert!(
            placed
                .iter()
                .all(|p| (0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y))
        );
    }

    #[test]
    fn contour_case_table_symmetric() {
        // Complementary cases describe the same separating line.
        for k in 0..=15u8 {
            assert_eq!(contour_char(k), contour_char(15 - k), "case {k}");
        }
        assert_eq!(contour_char(0), None);
        assert_eq!(contour_char(15), None);
        assert_eq!(contour_char(1), Some('╭')); // lone BR corner above
        assert_eq!(contour_char(3), Some('─')); // bottom row above
        assert_eq!(contour_char(5), Some('│')); // right column above
        assert_eq!(contour_char(6), Some('┼')); // saddle
    }

    #[test]
    fn hotspot_rings_and_idle_stays_empty() {
        let map = Rect::new(0, 0, 48, 14);
        let hot = vec![Placed {
            x: 0.5,
            y: 0.4,
            temp: 96.0,
            tag: None,
        }];
        let (cells, _) = compute_contours(map, &hot, &[96.0], &crate::ui::theme::NEON);
        let drawn = cells.iter().flatten().count();
        assert!(drawn > 8, "a 96° hotspot must ring, drew {drawn} cells");

        // Absolute scale: a deck at ambient draws nothing at all.
        let cool = vec![Placed {
            x: 0.5,
            y: 0.5,
            temp: 25.0,
            tag: None,
        }];
        let (cells, labels) = compute_contours(map, &cool, &[25.0], &crate::ui::theme::NEON);
        assert!(cells.iter().all(Option::is_none));
        assert!(labels.is_empty());
    }

    #[test]
    fn contour_toggle_off_yields_blank_but_shaped_layer() {
        let map = Rect::new(0, 0, 48, 14);
        let hot = vec![Placed {
            x: 0.5,
            y: 0.4,
            temp: 96.0,
            tag: None,
        }];
        // Off: no glyphs, no ring labels — but the full w×h shape, because
        // every consumer (blit, accents, labels, outline) indexes the map.
        let (cells, labels) = contour_layer(false, map, &hot, &[96.0], &crate::ui::theme::NEON);
        assert_eq!(cells.len(), 48 * 14);
        assert!(cells.iter().all(Option::is_none));
        assert!(labels.is_empty());
        // On: identical inputs still ring — the toggle is the only gate.
        let (on, _) = contour_layer(true, map, &hot, &[96.0], &crate::ui::theme::NEON);
        assert!(on.iter().flatten().count() > 8);
    }

    #[test]
    fn label_field_nudges_instead_of_overwriting() {
        let map = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(map);
        let mut field = LabelField::new(map);
        field.draw(&mut buf, 0.5, 0.5, "AAAA", Color::White);
        field.draw(&mut buf, 0.5, 0.5, "BBBB", Color::White);
        let row = |y: u16| -> String { (0..40).map(|x| buf[(x, y)].symbol()).collect() };
        // Both labels rendered, on different rows (second nudged).
        let all: String = (0..10).map(row).collect();
        assert!(all.contains("AAAA") && all.contains("BBBB"));
        for y in 0..10 {
            let r = row(y);
            assert!(
                !(r.contains("AAAA") && r.contains("BBBB")) || r.find("AAAA") != r.find("BBBB")
            );
        }
        // Occupancy never records an overlap.
        for spans in &field.rows {
            for (i, &(s0, e0)) in spans.iter().enumerate() {
                for &(s1, _e1) in spans.iter().skip(i + 1) {
                    assert!(s1 > e0 || s0 > s1);
                }
            }
        }
    }
}
