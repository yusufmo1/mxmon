//! Chassis schematic: the machine itself, etched under the thermal contours.
//!
//! A dim silkscreen blueprint of the open MacBook — fans, heat pipe, the SoC
//! package with die + on-package LPDDR, SSD, battery cells, speakers,
//! trackpad — drawn beneath the isotherm layer so every sensor label lands on
//! real hardware and the contour rings read as heat blooming off silicon.
//!
//! `Geometry` is the single source of truth for physical positions: the
//! thermal view derives every sensor anchor from it, so the silkscreen and
//! the sensors can never drift apart. All coordinates are normalized chassis
//! space (0,0 top-left at the hinge → 1,1 bottom-right at the palm rest);
//! the deck's 1.45:1 physical aspect is corrected at raster time, exactly as
//! the heat field does.
//!
//! Ink discipline: strokes use `theme.border` (the quietest structural color)
//! and etched text uses `theme.dim`, both well below the contour rings'
//! lifted ink — the blueprint stays a backdrop, the isotherms stay the story.

// Anchors take `&self` uniformly, even where a position is (today) a
// constant: the geometry is the API boundary, and fixed spots are free to
// become adaptive without touching every call site.
#![allow(clippy::unused_self)]

use std::collections::HashMap;
use std::f32::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::collect::battery::BatterySample;
use crate::collect::soc::SocInfo;
use crate::collect::temps::{SensorGroup, TempSample};
use crate::ui::theme::Theme;
use crate::ui::widgets::BRAILLE;

/// Physical deck aspect (width : height) — one unit of normalized y spans
/// 1.45× the distance of one unit of x. Matches `thermal::corner_field`.
const DECK_ASPECT: f32 = 1.45;

/// A rectangle in normalized chassis coordinates.
#[derive(Debug, Clone, Copy)]
pub struct NRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl NRect {
    const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
}

/// A fan: center + radius (radius in y-units; x extent is aspect-corrected).
#[derive(Debug, Clone, Copy)]
pub struct FanGeo {
    pub cx: f32,
    pub cy: f32,
    pub r: f32,
}

/// Sensor-zone rows inside the die (normalized y).
const E_ROW: f32 = 0.115;
const P_ROW0: f32 = 0.20;
const P_PITCH: f32 = 0.08;
const GPU_ROW0: f32 = 0.36;
const GPU_PITCH: f32 = 0.045;

/// Die-region sensor slots ("PMU tdie1-10"): columns just inside the die's
/// left/right edges, then a pair along its lower band.
const DIE_SLOTS: [(f32, f32); 10] = [
    (0.335, 0.16),
    (0.645, 0.16),
    (0.335, 0.25),
    (0.645, 0.25),
    (0.335, 0.34),
    (0.645, 0.34),
    (0.335, 0.43),
    (0.645, 0.43),
    (0.42, 0.51),
    (0.58, 0.51),
];

/// Board-level PMU / power-rail slots flanking the package on both sides
/// (kept off the heat-pipe latitude and clear of the fan bays).
const PMU_SLOTS: [(f32, f32); 8] = [
    (0.175, 0.19),
    (0.175, 0.30),
    (0.175, 0.41),
    (0.175, 0.52),
    (0.80, 0.19),
    (0.80, 0.30),
    (0.80, 0.41),
    (0.80, 0.52),
];

/// Everything the schematic draws and every anchor the sensors pin to.
pub struct Geometry {
    /// SoC package substrate (holds die + RAM, like the real part).
    pub package: NRect,
    /// The die itself (dashed outline; sensor zones live inside).
    pub die: NRect,
    /// On-package LPDDR stacks, two per side of the die.
    pub ram: Vec<NRect>,
    /// Fan circles at the hinge corners (empty on fanless machines).
    pub fans: Vec<FanGeo>,
    /// Battery cells on the lower deck (empty on desktops).
    pub battery: Vec<NRect>,
    /// Whole-pack outline used when the map is too small for six cells.
    pub battery_pack: NRect,
    /// Speaker grilles flanking the battery (empty on desktops).
    pub speakers: Vec<NRect>,
    pub trackpad: Option<NRect>,
    pub ssd: Option<NRect>,
    /// Etched package line, e.g. "APPLE M3 MAX · 40-CORE GPU · 48GB".
    chip_label: String,
    ram_label: &'static str,
    /// P-cores laid out this many per row (from the cluster topology).
    p_per_row: usize,
    p_rows: usize,
}

impl Geometry {
    pub fn new(soc: &SocInfo, t: &TempSample, has_battery: bool) -> Self {
        let n_p = t
            .sensors
            .iter()
            .filter(|s| s.group == SensorGroup::CpuPCore)
            .count();
        let p_per_row = if soc.cores_per_pcluster == 0 {
            6
        } else {
            soc.cores_per_pcluster.clamp(1, 8)
        };
        let p_rows = n_p.div_ceil(p_per_row).max(1);

        let fan_slots = [(0.09, 0.16), (0.91, 0.16)];
        let fans = fan_slots
            .into_iter()
            .take(t.fans.len().min(2))
            .map(|(cx, cy)| FanGeo { cx, cy, r: 0.075 })
            .collect();

        let battery = if has_battery {
            vec![
                NRect::new(0.22, 0.655, 0.155, 0.135),
                NRect::new(0.22, 0.795, 0.155, 0.135),
                NRect::new(0.385, 0.655, 0.105, 0.275),
                NRect::new(0.51, 0.655, 0.105, 0.275),
                NRect::new(0.625, 0.655, 0.155, 0.135),
                NRect::new(0.625, 0.795, 0.155, 0.135),
            ]
        } else {
            Vec::new()
        };
        let speakers = if has_battery {
            vec![
                NRect::new(0.105, 0.68, 0.03, 0.24),
                NRect::new(0.865, 0.68, 0.03, 0.24),
            ]
        } else {
            Vec::new()
        };
        let trackpad = has_battery.then(|| NRect::new(0.38, 0.845, 0.24, 0.14));
        let ssd = t
            .sensors
            .iter()
            .any(|s| s.group == SensorGroup::Ssd)
            .then(|| NRect::new(0.845, 0.28, 0.08, 0.09));

        let chip_label = if soc.chip_name.is_empty() {
            String::new()
        } else {
            let mut parts = vec![soc.chip_name.to_uppercase()];
            if let Some(gpu) = soc.gpu_core_count {
                parts.push(format!("{gpu}-CORE GPU"));
            }
            let gib = soc.memory_bytes >> 30;
            if gib > 0 {
                parts.push(format!("{gib}GB"));
            }
            parts.join(" · ")
        };
        let ram_label = if soc.chip_name.contains("M1") {
            "LPDDR4X"
        } else if soc.chip_name.contains("M2") || soc.chip_name.contains("M3") {
            "LPDDR5"
        } else if soc.chip_name.is_empty() {
            "LPDDR"
        } else {
            "LPDDR5X"
        };

        Self {
            package: NRect::new(0.215, 0.065, 0.55, 0.535),
            die: NRect::new(0.30, 0.085, 0.38, 0.46),
            ram: vec![
                NRect::new(0.225, 0.10, 0.06, 0.17),
                NRect::new(0.225, 0.30, 0.06, 0.17),
                NRect::new(0.695, 0.10, 0.06, 0.17),
                NRect::new(0.695, 0.30, 0.06, 0.17),
            ],
            fans,
            battery,
            battery_pack: NRect::new(0.22, 0.655, 0.56, 0.275),
            speakers,
            trackpad,
            ssd,
            chip_label,
            ram_label,
            p_per_row,
            p_rows,
        }
    }

    fn spread(i: usize, n: usize, from: f32, to: f32) -> f32 {
        from + (to - from) * (i % n) as f32 / (n - 1).max(1) as f32
    }

    /// Y of P-core row `r`; rows past two compress to stay above the GPU zone.
    fn p_row_y(&self, r: usize) -> f32 {
        if self.p_rows <= 2 {
            P_ROW0 + P_PITCH * r as f32
        } else {
            0.19 + (0.145 / (self.p_rows - 1) as f32) * r as f32
        }
    }

    // ---- sensor anchors (the thermal view pins everything through these) ----

    pub fn ecore(&self, i: usize) -> (f32, f32) {
        (Self::spread(i, 4, 0.42, 0.57), E_ROW)
    }

    pub fn pcore(&self, i: usize) -> (f32, f32) {
        let row = (i / self.p_per_row).min(self.p_rows.saturating_sub(1));
        (
            Self::spread(i, self.p_per_row, 0.375, 0.645),
            self.p_row_y(row),
        )
    }

    /// SMC GPU-cluster sensors: an 8-per-row grid over the GPU zone.
    pub fn gpu_cluster(&self, i: usize) -> (f32, f32) {
        (
            Self::spread(i, 8, 0.375, 0.635),
            GPU_ROW0 + GPU_PITCH * ((i / 8) % 4) as f32,
        )
    }

    /// Per-region HID GPU sensors (unlabeled, field-shaping only).
    pub fn gpu_region(&self, i: usize) -> (f32, f32) {
        (
            Self::spread(i, 6, 0.38, 0.63),
            GPU_ROW0 + 0.04 * ((i / 6) % 4) as f32,
        )
    }

    pub fn die_slot(&self, i: usize) -> (f32, f32) {
        DIE_SLOTS[i % DIE_SLOTS.len()]
    }

    pub fn pmu_slot(&self, i: usize) -> (f32, f32) {
        PMU_SLOTS[i % PMU_SLOTS.len()]
    }

    pub fn soc_misc(&self, i: usize) -> (f32, f32) {
        (Self::spread(i, 3, 0.44, 0.56), 0.49)
    }

    pub fn ane(&self) -> (f32, f32) {
        (0.625, E_ROW)
    }

    pub fn ssd_anchor(&self) -> (f32, f32) {
        self.ssd
            .map_or((0.885, 0.325), |r| (r.cx(), r.y + r.h / 2.0))
    }

    pub fn battery_anchor(&self) -> (f32, f32) {
        (0.50, 0.72)
    }

    /// Airflow sensors sit at the fan hubs (vents share the fan bays).
    pub fn airflow(&self, right: bool) -> (f32, f32) {
        let idx = usize::from(right && self.fans.len() > 1);
        self.fans
            .get(idx)
            .map_or_else(|| (if right { 0.90 } else { 0.10 }, 0.14), |f| (f.cx, f.cy))
    }

    pub fn charger(&self, supply: bool) -> (f32, f32) {
        if supply { (0.045, 0.46) } else { (0.045, 0.30) }
    }

    pub fn port(&self, right: bool) -> (f32, f32) {
        if right { (0.97, 0.40) } else { (0.03, 0.40) }
    }

    pub fn wifi(&self) -> (f32, f32) {
        (0.50, 0.05)
    }

    pub fn trackpad_anchor(&self) -> (f32, f32) {
        self.trackpad
            .map_or((0.50, 0.90), |r| (r.cx(), r.y + r.h / 2.0))
    }

    pub fn palm(&self) -> (f32, f32) {
        (0.20, 0.90)
    }

    pub fn other_misc(&self, i: usize) -> (f32, f32) {
        (0.30 + 0.40 * (i % 2) as f32, 0.85)
    }

    /// RPM readouts float just above their fan bays.
    pub fn fan_label(&self, i: usize) -> (f32, f32) {
        self.fans.get(i).map_or_else(
            || (if i == 0 { 0.10 } else { 0.90 }, 0.05),
            |f| (f.cx, 0.045),
        )
    }
}

/// How much of the schematic a map of this size can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Off,
    /// Package, die, RAM, fans, heat pipe, battery — outlines only.
    Mid,
    /// Everything, plus etched zone/chip/RAM labels and speaker grilles.
    Full,
}

pub fn detail(map: Rect, enabled: bool) -> Detail {
    if !enabled {
        Detail::Off
    } else if map.width >= 96 && map.height >= 26 {
        Detail::Full
    } else if map.width >= 54 && map.height >= 15 {
        Detail::Mid
    } else {
        Detail::Off
    }
}

/// Advance a fan's display phase. Decorative strobe, not physics: scaled so a
/// ~4000 rpm fan steps roughly one blade-glyph per fast tick.
pub fn spin(phase: f32, rpm: f32, dt: f32) -> f32 {
    (phase + rpm * dt * 0.000_8) % TAU
}

/// Rows of a battery cell's interior under the charge waterline.
fn fill_rows(interior_h: i32, charge: f32) -> i32 {
    if interior_h <= 0 {
        return 0;
    }
    (charge.clamp(0.0, 1.0) * interior_h as f32).round() as i32
}

/// Clipped cell painter over the map rect (never writes outside `map ∩ buf`).
struct Painter<'a> {
    buf: &'a mut Buffer,
    map: Rect,
    clip: Rect,
}

impl<'a> Painter<'a> {
    fn new(buf: &'a mut Buffer, map: Rect) -> Self {
        let clip = map.intersection(buf.area);
        Self { buf, map, clip }
    }

    /// Normalized x → absolute cell column (same convention as the labels).
    fn cx(&self, fx: f32) -> i32 {
        i32::from(self.map.x) + (fx * f32::from(self.map.width.saturating_sub(1))) as i32
    }

    fn cy(&self, fy: f32) -> i32 {
        i32::from(self.map.y) + (fy * f32::from(self.map.height.saturating_sub(1))) as i32
    }

    fn put(&mut self, x: i32, y: i32, ch: char, ink: Color) {
        if x >= i32::from(self.clip.x)
            && x < i32::from(self.clip.right())
            && y >= i32::from(self.clip.y)
            && y < i32::from(self.clip.bottom())
        {
            let cell = &mut self.buf[(x as u16, y as u16)];
            cell.set_char(ch);
            cell.set_fg(ink);
        }
    }

    /// Rounded rect; dashed edges for "inner" parts (the die). Degenerate
    /// rects (under 2×2 cells) draw nothing.
    fn stroke(&mut self, r: NRect, dashed: bool, ink: Color) {
        let (x0, y0) = (self.cx(r.x), self.cy(r.y));
        let (x1, y1) = (self.cx(r.x + r.w), self.cy(r.y + r.h));
        if x1 - x0 < 2 || y1 - y0 < 2 {
            return;
        }
        let (h, v) = if dashed {
            ('┈', '┊')
        } else {
            ('─', '│')
        };
        for x in (x0 + 1)..x1 {
            self.put(x, y0, h, ink);
            self.put(x, y1, h, ink);
        }
        for y in (y0 + 1)..y1 {
            self.put(x0, y, v, ink);
            self.put(x1, y, v, ink);
        }
        self.put(x0, y0, '╭', ink);
        self.put(x1, y0, '╮', ink);
        self.put(x0, y1, '╰', ink);
        self.put(x1, y1, '╯', ink);
    }

    /// Braille-dot ellipse: a thin, smooth ring in the same sub-cell idiom as
    /// the graphs. Radius is given in y-units and aspect-corrected for x.
    fn ellipse(&mut self, fan: FanGeo, ink: Color) {
        let cxf = f32::from(self.map.x) + fan.cx * f32::from(self.map.width.saturating_sub(1));
        let cyf = f32::from(self.map.y) + fan.cy * f32::from(self.map.height.saturating_sub(1));
        let ry = fan.r * f32::from(self.map.height.saturating_sub(1));
        let rx = fan.r / DECK_ASPECT * f32::from(self.map.width.saturating_sub(1));
        if ry < 1.0 || rx < 1.0 {
            return;
        }
        let mut dots: HashMap<(i32, i32), u16> = HashMap::new();
        for k in 0..128 {
            let a = k as f32 / 128.0 * TAU;
            let dx = ((cxf + rx * a.cos()) * 2.0).floor() as i32;
            let dy = ((cyf + ry * a.sin()) * 4.0).floor() as i32;
            let sub_x = dx.rem_euclid(2) as usize;
            let sub_y = dy.rem_euclid(4) as usize;
            *dots
                .entry((dx.div_euclid(2), dy.div_euclid(4)))
                .or_default() |= BRAILLE[sub_x][sub_y];
        }
        for ((x, y), bits) in dots {
            let ch = char::from_u32(0x2800 + u32::from(bits)).unwrap_or('⠀');
            self.put(x, y, ch, ink);
        }
    }

    /// Etched text centered on a normalized anchor; returns the claimed span
    /// so sensor labels can dodge it. Silently skipped when it can't fit.
    fn etch(&mut self, fx: f32, fy: f32, text: &str, ink: Color) -> Option<(u16, u16, u16)> {
        let w = text.chars().count() as i32;
        if w == 0 || w > i32::from(self.clip.width) {
            return None;
        }
        let y = self.cy(fy);
        if y < i32::from(self.clip.y) || y >= i32::from(self.clip.bottom()) {
            return None;
        }
        let x =
            (self.cx(fx) - w / 2).clamp(i32::from(self.clip.x), i32::from(self.clip.right()) - w);
        for (i, ch) in text.chars().enumerate() {
            self.put(x + i as i32, y, ch, ink);
        }
        Some((x as u16, y as u16, w as u16))
    }

    /// Left-anchored etch (zone labels hug the die's inner margin).
    fn etch_left(&mut self, fx: f32, fy: f32, text: &str, ink: Color) -> Option<(u16, u16, u16)> {
        let w = text.chars().count() as i32;
        let x = self.cx(fx);
        let y = self.cy(fy);
        if w == 0 || x + w > i32::from(self.clip.right()) {
            return None;
        }
        if y < i32::from(self.clip.y) || y >= i32::from(self.clip.bottom()) {
            return None;
        }
        for (i, ch) in text.chars().enumerate() {
            self.put(x + i as i32, y, ch, ink);
        }
        Some((x as u16, y as u16, w as u16))
    }

    /// Vertical etch reading downward from a top anchor (RAM part numbers,
    /// like the print on the real chips). Reserves one cell per row.
    fn etch_down(
        &mut self,
        fx: f32,
        fy: f32,
        text: &str,
        ink: Color,
        spans: &mut Vec<(u16, u16, u16)>,
    ) {
        let x = self.cx(fx);
        if x < i32::from(self.clip.x) || x >= i32::from(self.clip.right()) {
            return;
        }
        let y0 = self.cy(fy);
        for (i, ch) in text.chars().enumerate() {
            let y = y0 + i as i32;
            if y < i32::from(self.clip.y) || y >= i32::from(self.clip.bottom()) {
                return;
            }
            self.put(x, y, ch, ink);
            spans.push((x as u16, y as u16, 1));
        }
    }
}

/// Battery boxes actually drawable at this size: the six cells when each is
/// tall enough to read as a cell, the whole-pack outline otherwise, nothing
/// when even that collapses. Static stroke and dynamic fill share this.
fn battery_boxes(p: &Painter<'_>, geom: &Geometry) -> Vec<NRect> {
    if geom.battery.is_empty() {
        return Vec::new();
    }
    let cell = geom.battery[0];
    let rows = p.cy(cell.y + cell.h) - p.cy(cell.y);
    let cols = p.cx(cell.x + cell.w) - p.cx(cell.x);
    if rows >= 3 && cols >= 4 {
        geom.battery.clone()
    } else {
        let pack = geom.battery_pack;
        if p.cy(pack.y + pack.h) - p.cy(pack.y) >= 2 {
            vec![pack]
        } else {
            Vec::new()
        }
    }
}

/// The static silkscreen strokes. Drawn before the contour blit so the
/// isotherms always win contested cells; text goes down later via
/// [`render_etches`] so a ring can never sever a part number.
pub fn render_silkscreen(buf: &mut Buffer, map: Rect, geom: &Geometry, detail: Detail, th: &Theme) {
    if detail == Detail::Off {
        return;
    }
    let mut p = Painter::new(buf, map);
    let ink = th.border;

    // Package → die → RAM, the schematic's centerpiece.
    p.stroke(geom.package, false, ink);
    p.stroke(geom.die, true, ink);
    for chip in &geom.ram {
        p.stroke(*chip, false, ink);
    }

    // Heat-pipe cowling: one double-stroke run across the hinge connecting
    // the fan bays, passing behind the antenna label — only when it resolves
    // to its own row above the package.
    if geom.fans.len() == 2 {
        let y = p.cy(0.045);
        if y < p.cy(geom.package.y) {
            let rx =
                |f: &FanGeo| (f.r / DECK_ASPECT * f32::from(map.width.saturating_sub(1))) as i32;
            let x0 = p.cx(geom.fans[0].cx) + rx(&geom.fans[0]) + 2;
            let x1 = p.cx(geom.fans[1].cx) - rx(&geom.fans[1]) - 1;
            for x in x0..x1 {
                p.put(x, y, '═', ink);
            }
        }
    }
    for fan in &geom.fans {
        p.ellipse(*fan, ink);
        if detail == Detail::Mid {
            // Hub marker; at Full the airflow label claims the center.
            p.put(p.cx(fan.cx), p.cy(fan.cy), '◉', ink);
        }
    }

    for cell in battery_boxes(&p, geom) {
        p.stroke(cell, false, ink);
    }
    if let Some(tp) = geom.trackpad {
        p.stroke(tp, false, ink);
    }
    if let Some(ssd) = geom.ssd {
        p.stroke(ssd, false, ink);
    }

    // Speaker grilles: soft braille mesh flanking the battery.
    if detail == Detail::Full {
        for sp in &geom.speakers {
            let (x0, y0) = (p.cx(sp.x), p.cy(sp.y));
            let (x1, y1) = (p.cx(sp.x + sp.w), p.cy(sp.y + sp.h));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    p.put(x, y, '⣿', ink);
                }
            }
        }
    }
}

/// Etched lettering — die zones, RAM part numbers, the package line. Drawn
/// after the contour blit (text is legend, like the sensor labels, and must
/// stay whole); returns its spans so sensor labels dodge them.
pub fn render_etches(
    buf: &mut Buffer,
    map: Rect,
    geom: &Geometry,
    detail: Detail,
    th: &Theme,
) -> Vec<(u16, u16, u16)> {
    if detail != Detail::Full {
        return Vec::new();
    }
    let mut p = Painter::new(buf, map);
    let ink = th.dim;
    let mut etched = Vec::new();

    // Zone labels hugging the die's inner-left margin.
    let zx = geom.die.x + 0.008;
    etched.extend(p.etch_left(zx, E_ROW, "E-CPU", ink));
    etched.extend(p.etch_left(zx, geom.p_row_y(0), "P-CPU", ink));
    etched.extend(p.etch_left(zx, GPU_ROW0, "GPU", ink));

    // RAM part numbers printed down the chips, like the real silkscreen.
    for chip in [&geom.ram[0], &geom.ram[2]] {
        let rows = p.cy(chip.y + chip.h) - p.cy(chip.y);
        if (geom.ram_label.len() as i32) < rows {
            p.etch_down(chip.cx(), chip.y + 0.015, geom.ram_label, ink, &mut etched);
        }
    }

    // The part line, centered on the package margin under the die — only
    // when that band resolves to its own row at this size.
    if !geom.chip_label.is_empty() {
        let die_bottom = p.cy(geom.die.y + geom.die.h);
        let pkg_bottom = p.cy(geom.package.y + geom.package.h);
        let fy = f32::midpoint(geom.die.y + geom.die.h, geom.package.y + geom.package.h);
        let y = p.cy(fy);
        if y > die_bottom && y < pkg_bottom {
            etched.extend(p.etch(geom.package.cx(), fy, &geom.chip_label, ink));
        }
    }
    etched
}

/// The living accents, drawn after the contour blit: fan blades stepped by
/// live RPM and the battery charge waterline. Both yield to contour cells so
/// an isotherm is never severed by decoration.
#[allow(clippy::too_many_arguments)]
pub fn render_dynamic(
    buf: &mut Buffer,
    map: Rect,
    geom: &Geometry,
    detail: Detail,
    th: &Theme,
    t: &TempSample,
    battery: Option<&BatterySample>,
    phases: [f32; 2],
    contours: &[Option<(char, Color)>],
) {
    if detail == Detail::Off {
        return;
    }
    let mut p = Painter::new(buf, map);
    let occupied = |x: i32, y: i32| -> bool {
        let (cx, cy) = (x - i32::from(map.x), y - i32::from(map.y));
        if cx < 0 || cy < 0 || cx >= i32::from(map.width) || cy >= i32::from(map.height) {
            return true;
        }
        contours
            .get(cy as usize * map.width as usize + cx as usize)
            .is_some_and(Option::is_some)
    };

    // Fan blades: three dots orbiting the hub, phase driven by live RPM.
    for (i, fan) in geom.fans.iter().enumerate() {
        let rpm = t.fans.get(i).map_or(0.0, |f| f.rpm);
        let ry = fan.r * f32::from(map.height.saturating_sub(1));
        if rpm < 1.0 || ry < 2.5 {
            continue;
        }
        let cxf = f32::from(map.x) + fan.cx * f32::from(map.width.saturating_sub(1));
        let cyf = f32::from(map.y) + fan.cy * f32::from(map.height.saturating_sub(1));
        let rx = fan.r / DECK_ASPECT * f32::from(map.width.saturating_sub(1));
        for k in 0..3 {
            let a = phases[i.min(1)] + k as f32 * TAU / 3.0;
            let x = (cxf + rx * 0.62 * a.cos()).round() as i32;
            let y = (cyf + ry * 0.62 * a.sin()).round() as i32;
            if !occupied(x, y) {
                p.put(x, y, '•', th.dim);
            }
        }
    }

    // Battery waterline: each cell fills from the bottom with the charge
    // fraction. Green while charging, amber when low, whisper-dim otherwise.
    let Some(b) = battery else { return };
    let charge = b.charge.0.clamp(0.0, 1.0);
    let ink = if b.charging {
        th.ok
    } else if charge < 0.20 {
        th.warn
    } else {
        th.dim
    };
    for cell in battery_boxes(&p, geom) {
        let (x0, y0) = (p.cx(cell.x), p.cy(cell.y));
        let (x1, y1) = (p.cx(cell.x + cell.w), p.cy(cell.y + cell.h));
        let filled = fill_rows(y1 - y0 - 1, charge);
        for y in (y1 - filled)..y1 {
            for x in (x0 + 1)..x1 {
                if !occupied(x, y) {
                    p.put(x, y, '░', ink);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::temps::Sensor;
    use crate::units::Celsius;

    fn sample(n_e: usize, n_p: usize, n_gpu: usize, n_fans: usize, ssd: bool) -> TempSample {
        let mut t = TempSample::default();
        let push = |t: &mut TempSample, group, label: String| {
            t.sensors.push(Sensor {
                label,
                group,
                temp: Celsius(50.0),
            });
        };
        for i in 0..n_e {
            push(&mut t, SensorGroup::CpuECore, format!("E-Core {}", i + 1));
        }
        for i in 0..n_p {
            push(&mut t, SensorGroup::CpuPCore, format!("P-Core {}", i + 1));
        }
        for i in 0..n_gpu {
            push(&mut t, SensorGroup::Gpu, format!("GPU Cluster {}", i + 1));
        }
        if ssd {
            push(&mut t, SensorGroup::Ssd, "NAND".into());
        }
        for i in 0..n_fans {
            t.fans.push(crate::collect::temps::Fan {
                label: format!("Fan {i}"),
                rpm: 1200.0,
                max_rpm: 6000.0,
            });
        }
        t
    }

    fn soc(e: usize, p: usize, cpl: usize) -> SocInfo {
        SocInfo {
            chip_name: "Apple M3 Max".into(),
            ecpu_count: e,
            pcpu_count: p,
            cores_per_pcluster: cpl,
            gpu_core_count: Some(40),
            memory_bytes: 48 << 30,
            ..Default::default()
        }
    }

    fn inside(rect: NRect, (x, y): (f32, f32)) -> bool {
        x > rect.x && x < rect.x + rect.w && y > rect.y && y < rect.y + rect.h
    }

    #[test]
    fn m3_max_geometry_contains_every_anchor() {
        let t = sample(4, 12, 32, 2, true);
        let g = Geometry::new(&soc(4, 12, 6), &t, true);
        for i in 0..4 {
            assert!(inside(g.die, g.ecore(i)), "E{i} outside die");
        }
        for i in 0..12 {
            assert!(inside(g.die, g.pcore(i)), "P{i} outside die");
        }
        for i in 0..32 {
            assert!(inside(g.die, g.gpu_cluster(i)), "G{i} outside die");
        }
        for i in 0..10 {
            assert!(inside(g.die, g.die_slot(i)), "die slot {i} outside die");
        }
        assert!(inside(g.die, g.ane()));
        // Die inside package; RAM inside package but clear of the die.
        assert!(inside(g.package, (g.die.x, g.die.y)));
        assert!(inside(g.package, (g.die.x + g.die.w, g.die.y + g.die.h)));
        for chip in &g.ram {
            assert!(inside(g.package, (chip.x, chip.y)));
            assert!(chip.x + chip.w < g.die.x || chip.x > g.die.x + g.die.w);
        }
        // PMU slots sit on the board, outside the package.
        for i in 0..8 {
            assert!(!inside(g.package, g.pmu_slot(i)), "PMU {i} inside package");
        }
        assert_eq!(g.fans.len(), 2);
        assert_eq!(g.battery.len(), 6);
        assert_eq!(g.p_rows, 2);
    }

    #[test]
    fn every_geometry_stays_on_the_deck() {
        let t = sample(4, 12, 32, 2, true);
        let g = Geometry::new(&soc(4, 12, 6), &t, true);
        let rects = g
            .ram
            .iter()
            .copied()
            .chain(g.battery.iter().copied())
            .chain(g.speakers.iter().copied())
            .chain([g.package, g.die, g.battery_pack])
            .chain(g.trackpad)
            .chain(g.ssd);
        for r in rects {
            assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= 1.0 && r.y + r.h <= 1.0);
        }
        let anchors: Vec<(f32, f32)> = (0..12)
            .flat_map(|i| {
                [
                    g.ecore(i),
                    g.pcore(i),
                    g.gpu_cluster(i),
                    g.gpu_region(i),
                    g.die_slot(i),
                    g.pmu_slot(i),
                    g.soc_misc(i),
                    g.other_misc(i),
                    g.fan_label(i),
                ]
            })
            .chain([
                g.ane(),
                g.ssd_anchor(),
                g.battery_anchor(),
                g.airflow(false),
                g.airflow(true),
                g.charger(false),
                g.charger(true),
                g.port(false),
                g.port(true),
                g.wifi(),
                g.trackpad_anchor(),
                g.palm(),
            ])
            .collect();
        for (x, y) in anchors {
            assert!((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y));
        }
    }

    #[test]
    fn m1_single_p_row_and_desktop_variants() {
        // M1: 4 P-cores in one cluster → one row, at the top P latitude.
        let t = sample(4, 4, 8, 2, true);
        let g = Geometry::new(&soc(4, 4, 4), &t, true);
        assert_eq!(g.p_rows, 1);
        assert!((g.pcore(3).1 - P_ROW0).abs() < 1e-6);

        // Desktop (no battery) drops the lower-deck furniture; fanless
        // machines drop the fan bays.
        let g = Geometry::new(&soc(4, 4, 4), &sample(4, 4, 8, 0, false), false);
        assert!(g.battery.is_empty() && g.speakers.is_empty());
        assert!(g.trackpad.is_none() && g.ssd.is_none());
        assert!(g.fans.is_empty());
    }

    #[test]
    fn detail_tiers_gate_by_size_and_toggle() {
        assert_eq!(detail(Rect::new(0, 0, 150, 48), true), Detail::Full);
        assert_eq!(detail(Rect::new(0, 0, 96, 26), true), Detail::Full);
        assert_eq!(detail(Rect::new(0, 0, 95, 26), true), Detail::Mid);
        assert_eq!(detail(Rect::new(0, 0, 60, 16), true), Detail::Mid);
        assert_eq!(detail(Rect::new(0, 0, 53, 16), true), Detail::Off);
        assert_eq!(detail(Rect::new(0, 0, 20, 8), true), Detail::Off);
        assert_eq!(detail(Rect::new(0, 0, 150, 48), false), Detail::Off);
    }

    #[test]
    fn spin_and_waterline_math() {
        // A stopped fan never advances; a spinning one does, and wraps.
        assert!((spin(1.0, 0.0, 0.25) - 1.0).abs() < f32::EPSILON);
        let mut phase = 0.0;
        for _ in 0..10_000 {
            phase = spin(phase, 6000.0, 0.25);
            assert!((0.0..TAU).contains(&phase));
        }
        assert!(spin(0.0, 4000.0, 0.25) > 0.5); // ~0.8 rad per tick at speed

        assert_eq!(fill_rows(10, 0.0), 0);
        assert_eq!(fill_rows(10, 1.0), 10);
        assert_eq!(fill_rows(10, 0.85), 9);
        assert_eq!(fill_rows(0, 0.5), 0);
        assert_eq!(fill_rows(-3, 0.5), 0);
    }

    #[test]
    fn silkscreen_renders_and_reserves_labels() {
        let map = Rect::new(0, 0, 150, 48);
        let mut buf = Buffer::empty(map);
        let t = sample(4, 12, 32, 2, true);
        let g = Geometry::new(&soc(4, 12, 6), &t, true);
        render_silkscreen(&mut buf, map, &g, Detail::Full, &crate::ui::theme::MIDNIGHT);
        let etched = render_etches(&mut buf, map, &g, Detail::Full, &crate::ui::theme::MIDNIGHT);
        assert!(etched.len() >= 4, "expected several etches, got {etched:?}");
        let row_major: String = (0..48)
            .flat_map(|y| (0..150).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect();
        for needle in ["E-CPU", "P-CPU", "GPU", "APPLE M3 MAX"] {
            assert!(row_major.contains(needle), "missing etched {needle}");
        }
        // RAM part numbers run vertically down the chips.
        let col_major: String = (0..150)
            .flat_map(|x| (0..48).map(move |y| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(col_major.contains("LPDDR5"), "missing vertical RAM label");

        // Degenerate maps must never panic, at any tier.
        for (w, h) in [(1, 1), (2, 2), (5, 3), (30, 4), (54, 15), (96, 26)] {
            let map = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(map);
            for d in [Detail::Off, Detail::Mid, Detail::Full] {
                render_silkscreen(&mut buf, map, &g, d, &crate::ui::theme::MIDNIGHT);
                render_etches(&mut buf, map, &g, d, &crate::ui::theme::MIDNIGHT);
                let cells = vec![None; w as usize * h as usize];
                render_dynamic(
                    &mut buf,
                    map,
                    &g,
                    d,
                    &crate::ui::theme::MIDNIGHT,
                    &t,
                    None,
                    [0.0, 0.0],
                    &cells,
                );
            }
        }
    }
}
