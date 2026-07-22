//! Custom drawing primitives: filled braille graphs with vertical gradients,
//! zoomed-axis braille polylines, eighth-block meters, single-cell core bars,
//! and the mouse hit-map.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};

use crate::app::{SortKey, View};

use super::theme::Gradient;

/// Eighth-height blocks, empty → full (index 0 = blank).
const V_EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Eighth-width blocks for horizontal meters (index 0 = blank).
const H_EIGHTHS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Braille dot bits by (sub-column, dot-row top→bottom). Shared with the
/// schematic layer's dotted fan rings.
pub(crate) const BRAILLE: [[u16; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// Filled area graph rendered in braille, newest data on the right, colored
/// with a vertical gradient (btop-style). Idle or not-yet-filled columns
/// keep a dotted baseline, and any nonzero value paints at least one dot.
pub struct BrailleGraph<'a> {
    pub data: &'a [f32],
    pub max: f32,
    pub gradient: Gradient,
    /// Dim color for the dotted baseline through idle columns.
    pub baseline: Color,
}

impl BrailleGraph<'_> {
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let height = area.height as usize;
        let slots = width * 2;

        // Right-align: slot k ← data[len - slots + k].
        let offset = self.data.len() as i64 - slots as i64;
        let value_at = |slot: usize| -> Option<f32> {
            let idx = offset + slot as i64;
            (idx >= 0)
                .then(|| self.data.get(idx as usize).copied())
                .flatten()
        };

        let dot_rows = (height * 4) as i64;
        for x in 0..width {
            // Filled dot-height per sub-column.
            let heights: [i64; 2] =
                [0, 1].map(|c| graph_dots(value_at(x * 2 + c), self.max, dot_rows));
            if heights == [0, 0] {
                // Dotted baseline instead of a void where the series is idle.
                let cell = &mut buf[(area.x + x as u16, area.y + (height - 1) as u16)];
                cell.set_char('⣀');
                cell.set_fg(self.baseline);
                continue;
            }
            for row in 0..height {
                let mut bits: u16 = 0;
                for (c, &h) in heights.iter().enumerate() {
                    for (d, &bit) in BRAILLE[c].iter().enumerate() {
                        // Dot-row index from the bottom of the graph.
                        let from_bottom = ((height - 1 - row) * 4) as i64 + (3 - d as i64);
                        if h > from_bottom {
                            bits |= bit;
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }
                let ch = char::from_u32(0x2800 + u32::from(bits)).unwrap_or('⠀');
                let vertical = (height - row) as f32 / height as f32;
                let cell = &mut buf[(area.x + x as u16, area.y + row as u16)];
                cell.set_char(ch);
                cell.set_fg(self.gradient.at(vertical));
            }
        }
    }
}

/// Auto-ranged y-axis for a bounded-below series (temperature, memory): the
/// visible window's `[min, max]` snapped outward to whole `step`s, widened to
/// at least `min_span` (downward first — headroom above stays honest), and
/// kept inside `clamp`. `None` when the window holds no finite sample.
/// Snapping to steps is what keeps the axis calm: bounds only move when the
/// data actually crosses a step boundary, so the graph doesn't rescale every
/// tick.
pub(crate) fn axis_window(
    values: &[f32],
    step: f32,
    min_span: f32,
    clamp: (f32, f32),
) -> Option<(f32, f32)> {
    let step = step.max(f32::EPSILON);
    let (floor, ceil) = clamp;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for v in values.iter().copied().filter(|v| v.is_finite()) {
        min = min.min(v);
        max = max.max(v);
    }
    if min > max {
        return None;
    }
    let mut lo = ((min.clamp(floor, ceil) / step).floor() * step).max(floor);
    let mut hi = ((max.clamp(floor, ceil) / step).ceil() * step).min(ceil);
    // Widen to the minimum span one whole step at a time, alternating sides
    // and pinning at the clamp. The iteration cap keeps this total under
    // hostile step/min_span combinations (ring data is not hostile; fuzz
    // inputs are).
    let eps = step * 1e-3;
    let mut grow_down = true;
    for _ in 0..64 {
        if hi - lo >= min_span - eps {
            break;
        }
        let down_ok = lo - step >= floor - eps;
        let up_ok = hi + step <= ceil + eps;
        match (down_ok, up_ok) {
            (true, true) => {
                if grow_down {
                    lo -= step;
                } else {
                    hi += step;
                }
                grow_down = !grow_down;
            }
            (true, false) => lo -= step,
            (false, true) => hi += step,
            (false, false) => break,
        }
    }
    let (lo, hi) = (lo.max(floor), hi.min(ceil));
    (hi > lo).then_some((lo, hi))
}

/// Braille polyline over an explicit `[lo, hi]` value window (the zoomed axis
/// from [`axis_window`]), newest data on the right like [`BrailleGraph`]. One
/// dot per sub-column with vertical runs joining neighbors, colored per
/// *value* — absolute meaning (how hot, how full) survives the zoom. Dots
/// OR-merge with braille already in the buffer, so several series overlay in
/// one graph area: draw the dim series first and the bright one last (later
/// fg wins shared cells). Columns with no finite sample keep the dotted
/// baseline.
pub struct LineGraph<'a, F: Fn(f32) -> Color> {
    pub data: &'a [f32],
    pub lo: f32,
    pub hi: f32,
    /// Color for a given data value.
    pub color: F,
    /// Dim color for the dotted baseline through empty columns.
    pub baseline: Color,
}

impl<F: Fn(f32) -> Color> LineGraph<'_, F> {
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area);
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let height = area.height as usize;
        let slots = width * 2;
        let offset = self.data.len() as i64 - slots as i64;
        let value_at = |slot: usize| -> Option<f32> {
            let idx = offset + slot as i64;
            (idx >= 0)
                .then(|| self.data.get(idx as usize).copied())
                .flatten()
                .filter(|v| v.is_finite())
        };
        let span = (self.hi - self.lo).max(f32::EPSILON);
        let top_dot = (height * 4 - 1) as f32;
        let dot_of =
            |v: f32| -> i64 { (((v - self.lo) / span).clamp(0.0, 1.0) * top_dot).round() as i64 };

        let mut prev: Option<i64> = None;
        for x in 0..width {
            let mut drew = false;
            for (c, bits_col) in BRAILLE.iter().enumerate() {
                let Some(v) = value_at(x * 2 + c) else {
                    prev = None;
                    continue;
                };
                drew = true;
                let dot = dot_of(v);
                let (run_lo, run_hi) = match prev {
                    Some(p) => (dot.min(p), dot.max(p)),
                    None => (dot, dot),
                };
                prev = Some(dot);
                let color = (self.color)(v);
                for d in run_lo..=run_hi {
                    let row = height - 1 - (d / 4) as usize;
                    let bit = bits_col[(3 - (d % 4)) as usize];
                    merge_dots(
                        buf,
                        area.x + x as u16,
                        area.y + row as u16,
                        bit,
                        color,
                        true,
                    );
                }
            }
            if !drew {
                // Dotted baseline instead of a void where the series is idle.
                merge_dots(
                    buf,
                    area.x + x as u16,
                    area.y + (height - 1) as u16,
                    0xC0, // both bottom dots: '⣀'
                    self.baseline,
                    false,
                );
            }
        }
    }
}

/// OR a braille dot-pattern into a cell: existing braille in the cell is kept
/// (series overlay), anything else is overwritten. `own_fg` takes the cell's
/// color; the baseline passes `false` so it never dims a line already drawn
/// through the cell.
fn merge_dots(buf: &mut Buffer, x: u16, y: u16, bits: u16, color: Color, own_fg: bool) {
    let cell = &mut buf[(x, y)];
    let mut chars = cell.symbol().chars();
    let existing = match (chars.next(), chars.next()) {
        (Some(ch), None) if ('\u{2800}'..='\u{28FF}').contains(&ch) => (ch as u32 - 0x2800) as u16,
        _ => 0,
    };
    let merged = existing | bits;
    cell.set_char(char::from_u32(0x2800 + u32::from(merged)).unwrap_or('⠀'));
    if own_fg || existing == 0 {
        cell.set_fg(color);
    }
}

/// Mirrored two-series braille graph: `tx` grows up from a shared axis, `rx`
/// hangs below it (Stats-style network history). Each side autoscales
/// independently, any nonzero value paints at least one dot so light traffic
/// never vanishes, and idle columns keep a dotted axis line.
pub struct MirrorGraph<'a> {
    pub tx: &'a [f32],
    pub rx: &'a [f32],
    pub tx_max: f32,
    pub rx_max: f32,
    pub up: Color,
    pub down: Color,
    pub baseline: Color,
}

/// Filled dot count for one value against a dot budget: ceil-scaled with a
/// 1-dot minimum for anything nonzero (NaN and ≤0 draw nothing). Shared by
/// every braille graph so light activity can never round to invisible.
pub(crate) fn graph_dots(value: Option<f32>, max: f32, budget: i64) -> i64 {
    let Some(v) = value else { return 0 };
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    let scaled = (v / max.max(f32::EPSILON)).clamp(0.0, 1.0) * budget as f32;
    (scaled.ceil() as i64).clamp(1, budget)
}

impl MirrorGraph<'_> {
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height < 2 {
            return;
        }
        let width = area.width as usize;
        let top_h = (area.height / 2) as usize; // upload rows; download gets the rest
        let bot_h = area.height as usize - top_h;
        let slots = width * 2;
        // Right-align both series on the same slot grid.
        let tx_off = self.tx.len() as i64 - slots as i64;
        let rx_off = self.rx.len() as i64 - slots as i64;
        let value = |data: &[f32], off: i64, slot: usize| -> Option<f32> {
            let idx = off + slot as i64;
            (idx >= 0)
                .then(|| data.get(idx as usize).copied())
                .flatten()
        };

        for x in 0..width {
            let tx_dots: [i64; 2] = [0, 1].map(|c| {
                graph_dots(
                    value(self.tx, tx_off, x * 2 + c),
                    self.tx_max,
                    (top_h * 4) as i64,
                )
            });
            let rx_dots: [i64; 2] = [0, 1].map(|c| {
                graph_dots(
                    value(self.rx, rx_off, x * 2 + c),
                    self.rx_max,
                    (bot_h * 4) as i64,
                )
            });

            // Upload: dots counted up from the axis (bottom of the top half).
            for row in 0..top_h {
                let mut bits: u16 = 0;
                for (c, &h) in tx_dots.iter().enumerate() {
                    for (d, &bit) in BRAILLE[c].iter().enumerate() {
                        let from_bottom = ((top_h - 1 - row) * 4) as i64 + (3 - d as i64);
                        if h > from_bottom {
                            bits |= bit;
                        }
                    }
                }
                let mut color = self.up;
                if bits == 0 {
                    // Keep a thin dotted axis where upload is idle, so the
                    // graph reads as a chart instead of a void.
                    if row + 1 == top_h && tx_dots == [0, 0] {
                        bits = 0xC0; // both bottom dots of the cell
                        color = self.baseline;
                    } else {
                        continue;
                    }
                }
                let cell = &mut buf[(area.x + x as u16, area.y + row as u16)];
                cell.set_char(char::from_u32(0x2800 + u32::from(bits)).unwrap_or('⠀'));
                cell.set_fg(color);
            }
            // Download: dots counted down from the axis (top of the bottom half).
            for row in 0..bot_h {
                let mut bits: u16 = 0;
                for (c, &h) in rx_dots.iter().enumerate() {
                    for (d, &bit) in BRAILLE[c].iter().enumerate() {
                        let from_top = (row * 4) as i64 + d as i64;
                        if h > from_top {
                            bits |= bit;
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }
                let cell = &mut buf[(area.x + x as u16, area.y + (top_h + row) as u16)];
                cell.set_char(char::from_u32(0x2800 + u32::from(bits)).unwrap_or('⠀'));
                cell.set_fg(self.down);
            }
        }
    }
}

/// Horizontal meter with eighth-block resolution and horizontal gradient fill.
pub struct Meter {
    pub ratio: f32,
    pub gradient: Gradient,
    pub track: Color,
}

impl Meter {
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        // Never index past the buffer: a caller that hands us an area wider than
        // the screen (e.g. a modal squeezed to ~10 cols) would otherwise panic
        // the whole app on the raw `buf[..]` write below. Render must be total.
        let area = area.intersection(buf.area);
        if area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let eighths = (self.ratio.clamp(0.0, 1.0) * width as f32 * 8.0).round() as usize;
        for x in 0..width {
            let cell_eighths = eighths.saturating_sub(x * 8).min(8);
            let cell = &mut buf[(area.x + x as u16, area.y)];
            if cell_eighths == 0 {
                cell.set_char('▏');
                cell.set_fg(self.track);
            } else {
                cell.set_char(H_EIGHTHS[cell_eighths]);
                cell.set_fg(self.gradient.at((x as f32 + 0.5) / width as f32));
            }
        }
    }
}

/// One-cell vertical bar for per-core meters.
pub fn core_bar(value: f32, gradient: Gradient) -> (char, Color) {
    let v = value.clamp(0.0, 1.0);
    let idx = ((v * 8.0).round() as usize).min(8);
    (V_EIGHTHS[idx], gradient.at(v))
}

/// Fill a rect with a background color (panels paint their own bg).
pub fn fill_bg(area: Rect, buf: &mut Buffer, bg: Color) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_style(Style::default().bg(bg));
        }
    }
}

/// Which metric card a [`Target::Panel`] refers to. Every card is a
/// navigation surface: clicking it jumps to the view where that metric
/// continues (the hover hint names the destination).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Cpu,
    Gpu,
    Mem,
    Net,
    Disk,
    Power,
    Temps,
    Battery,
    /// The inline chassis heat map on the overview.
    HeatMap,
}

/// Everything the mouse can interact with, rebuilt each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Tab(View),
    ProcHeader(SortKey),
    ProcRow(usize),
    /// Scrollable process list body.
    ProcList,
    /// Footer buttons.
    Help,
    Filter,
    Kill,
    Pause,
    ThemeCycle,
    Quit,
    /// Sensor list body (scrollable in thermal view).
    SensorList,
    /// Connections table body (scrollable in the connections view).
    FlowList,
    /// One connection row (click opens the owning process's details).
    FlowRow(usize),
    /// Kill-modal signal row.
    KillSignal(usize),
    /// Sort-menu row.
    SortOption(usize),
    /// Footer button opening the settings modal.
    Settings,
    /// Settings-modal row (click cycles the value forward).
    SettingRow(usize),
    /// Settings-modal `‹` arrow (steps the row's value back).
    SettingDec(usize),
    /// Settings-modal `›` arrow (steps the row's value forward).
    SettingInc(usize),
    /// Anywhere on a modal (consumes the click).
    ModalBody,
    /// The `✕` in a modal's top border.
    ModalClose,
    /// Kill button inside the details modal (carries the shown pid).
    KillPid(i32),
    /// A whole metric card — click navigates to its deep-dive view.
    Panel(PanelKind),
    /// Header `◉ mxmon cpu` chip — click toggles the perf HUD.
    Hud,
    /// Footer HUD chip — click opens settings at sampling; wheel tunes it.
    Tick,
    /// Footer toast — click dismisses it early.
    Toast,
}

#[derive(Default)]
pub struct HitMap {
    targets: Vec<(Rect, Target)>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.targets.clear();
    }

    pub fn push(&mut self, rect: Rect, target: Target) {
        self.targets.push((rect, target));
    }

    /// Topmost target under the cursor (later pushes win).
    pub fn hit(&self, x: u16, y: u16) -> Option<Target> {
        let pos = Position { x, y };
        self.targets
            .iter()
            .rev()
            .find(|(r, _)| r.contains(pos))
            .map(|&(_, t)| t)
    }
}

#[cfg(test)]
mod tests {
    use super::{BrailleGraph, MirrorGraph, graph_dots};
    use crate::ui::theme::Gradient;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    #[test]
    fn mirror_graph_dots_and_geometry() {
        // Any nonzero value paints at least one dot; NaN and gaps paint none.
        assert_eq!(graph_dots(Some(1.0), 1e9, 8), 1);
        assert_eq!(graph_dots(Some(1e9), 1e9, 8), 8);
        assert_eq!(graph_dots(Some(f32::NAN), 1e9, 8), 0);
        assert_eq!(graph_dots(None, 1e9, 8), 0);

        let area = Rect::new(0, 0, 4, 4); // 2 rows up, 2 rows down
        let render = |tx: &[f32], rx: &[f32]| {
            let mut buf = Buffer::empty(area);
            MirrorGraph {
                tx,
                rx,
                tx_max: 1e6,
                rx_max: 1e6,
                up: Color::Blue,
                down: Color::Green,
                baseline: Color::Gray,
            }
            .render(area, &mut buf);
            buf
        };

        // Saturated upload fills the top half; idle download leaves the bottom
        // empty — and vice versa (the mirror grows downward).
        let full = vec![1e6; 8];
        let buf = render(&full, &[]);
        assert_eq!(buf[(0, 0)].symbol(), "⣿");
        assert_eq!(buf[(3, 1)].symbol(), "⣿");
        assert_eq!(buf[(0, 2)].symbol(), " ");
        let buf = render(&[], &full);
        assert_eq!(buf[(0, 2)].symbol(), "⣿");
        assert_eq!(buf[(0, 0)].symbol(), " ", "upload half stays clear");
        // Idle columns keep a dotted axis on the boundary row, in the
        // baseline color — the graph never renders as a blank void.
        let buf = render(&[], &[]);
        assert_eq!(buf[(0, 1)].symbol(), "⣀");
        assert_eq!(buf[(0, 1)].fg, Color::Gray);
        // A tiny download hangs its minimum dot pair just below the axis.
        let buf = render(&[], &[1.0; 8]);
        assert_eq!(buf[(0, 2)].symbol(), "⠉");
        assert_eq!(buf[(0, 2)].fg, Color::Green);
    }

    #[test]
    fn braille_graph_baseline_and_min_dot() {
        let area = Rect::new(0, 0, 4, 2);
        let render = |data: &[f32], max: f32| {
            let mut buf = Buffer::empty(area);
            BrailleGraph {
                data,
                max,
                gradient: Gradient::Solid(Color::Red),
                baseline: Color::Gray,
            }
            .render(area, &mut buf);
            buf
        };
        // No data yet: a dotted baseline, not a void.
        let buf = render(&[], 100.0);
        assert_eq!(buf[(0, 1)].symbol(), "⣀");
        assert_eq!(buf[(3, 1)].fg, Color::Gray);
        // A sliver of activity still lands one dot, in series color.
        let buf = render(&[0.2; 8], 100.0);
        assert_eq!(buf[(0, 1)].symbol(), "⣀");
        assert_eq!(buf[(0, 1)].fg, Color::Red);
        // Saturation fills the column.
        let buf = render(&[100.0; 8], 100.0);
        assert_eq!(buf[(0, 0)].symbol(), "⣿");
    }

    use super::{LineGraph, axis_window};

    #[test]
    fn axis_window_hugs_and_snaps() {
        // A 71..79° band on 5° steps hugs the data: (70, 80).
        assert_eq!(
            axis_window(&[71.0, 74.5, 79.0], 5.0, 10.0, (0.0, 110.0)),
            Some((70.0, 80.0))
        );
        // Flat data still gets the minimum span, extended downward first.
        assert_eq!(
            axis_window(&[79.2; 8], 5.0, 10.0, (0.0, 110.0)),
            Some((70.0, 80.0))
        );
        // Non-finite samples are ignored; a window with none is None.
        assert_eq!(
            axis_window(&[f32::NAN, 42.0, f32::INFINITY], 5.0, 10.0, (0.0, 110.0)),
            Some((35.0, 45.0))
        );
        assert_eq!(axis_window(&[f32::NAN], 5.0, 10.0, (0.0, 110.0)), None);
        assert_eq!(axis_window(&[], 5.0, 10.0, (0.0, 110.0)), None);
        // Absurd values clamp into range instead of exploding the axis.
        assert_eq!(
            axis_window(&[1e30], 5.0, 10.0, (0.0, 110.0)),
            Some((100.0, 110.0))
        );

        // Ratio-scale windows pin at the clamp and widen the other way.
        let close = |got: Option<(f32, f32)>, want: (f32, f32)| {
            let (lo, hi) = got.expect("window");
            assert!(
                (lo - want.0).abs() < 1e-4 && (hi - want.1).abs() < 1e-4,
                "got ({lo}, {hi}), want {want:?}"
            );
        };
        close(
            axis_window(&[0.97, 1.0], 0.05, 0.10, (0.0, 1.0)),
            (0.90, 1.0),
        );
        close(axis_window(&[0.01], 0.05, 0.10, (0.0, 1.0)), (0.0, 0.10));
    }

    #[test]
    fn line_graph_zoomed_line_gaps_and_overlay_merge() {
        let area = Rect::new(0, 0, 4, 2); // 8 sub-columns × 8 dot-rows
        let mut buf = Buffer::empty(area);
        // A flat mid-window value sits mid-panel (top cell's bottom dots),
        // not on the floor — the whole point of the zoomed axis.
        LineGraph {
            data: &[4.0; 8],
            lo: 0.0,
            hi: 8.0,
            color: |_| Color::Red,
            baseline: Color::Gray,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "⣀");
        assert_eq!(buf[(0, 0)].fg, Color::Red);
        assert_eq!(buf[(0, 1)].symbol(), " ", "below the line stays empty");

        // A second series through the same cells ORs its dots in and, drawn
        // later, owns the shared cell's color.
        LineGraph {
            data: &[5.7; 8],
            lo: 0.0,
            hi: 8.0,
            color: |_| Color::Blue,
            baseline: Color::Gray,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "⣤", "both series' dots survive");
        assert_eq!(buf[(0, 0)].fg, Color::Blue, "later series wins the cell");

        // A steep move draws a connecting vertical run, not a gap.
        let mut buf = Buffer::empty(area);
        LineGraph {
            data: &[0.0, 0.0, 8.0, 8.0, 8.0, 8.0, 8.0, 8.0],
            lo: 0.0,
            hi: 8.0,
            color: |_| Color::Red,
            baseline: Color::Gray,
        }
        .render(area, &mut buf);
        assert_ne!(buf[(1, 0)].symbol(), " ");
        assert_ne!(buf[(1, 1)].symbol(), " ");

        // No data / all-NaN: dotted baseline, never a void — and the
        // baseline must not repaint cells a line already occupies.
        let mut buf = Buffer::empty(area);
        LineGraph {
            data: &[],
            lo: 0.0,
            hi: 1.0,
            color: |_| Color::Red,
            baseline: Color::Gray,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 1)].symbol(), "⣀");
        assert_eq!(buf[(0, 1)].fg, Color::Gray);
        let mut buf = Buffer::empty(area);
        LineGraph {
            data: &[0.05; 8], // line lives in the bottom cell
            lo: 0.0,
            hi: 1.0,
            color: |_| Color::Red,
            baseline: Color::Gray,
        }
        .render(area, &mut buf);
        LineGraph {
            data: &[f32::NAN; 8],
            lo: 0.0,
            hi: 1.0,
            color: |_| Color::Blue,
            baseline: Color::Gray,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 1)].fg, Color::Red, "baseline never dims a line");

        // Total under a hostile rect: area beyond the buffer clips, no panic.
        let mut buf = Buffer::empty(area);
        LineGraph {
            data: &[4.0; 64],
            lo: 0.0,
            hi: 8.0,
            color: |_| Color::Red,
            baseline: Color::Gray,
        }
        .render(Rect::new(2, 0, 40, 9), &mut buf);
    }

    use super::{HitMap, Meter, Target, core_bar, fill_bg};

    #[test]
    fn hitmap_topmost_wins_and_clears() {
        let mut hits = HitMap::default();
        hits.push(Rect::new(0, 0, 10, 10), Target::ProcList);
        hits.push(Rect::new(2, 2, 4, 1), Target::ProcRow(3));
        assert_eq!(hits.hit(3, 2), Some(Target::ProcRow(3)), "later push wins");
        assert_eq!(hits.hit(1, 1), Some(Target::ProcList));
        assert_eq!(hits.hit(50, 50), None);
        hits.clear();
        assert_eq!(hits.hit(3, 2), None);
    }

    #[test]
    fn meter_is_total_even_off_buffer() {
        // An area wider than the buffer (squeezed modal) must clamp, not
        // panic — render is total.
        let screen = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(screen);
        Meter {
            ratio: 1.0,
            gradient: Gradient::Solid(Color::Red),
            track: Color::Gray,
        }
        .render(Rect::new(2, 0, 40, 1), &mut buf);
        assert_eq!(buf[(3, 0)].symbol(), "█");
        assert_eq!(
            buf[(0, 0)].symbol(),
            " ",
            "cells outside the area untouched"
        );
        // Hostile ratios (overload, sign glitches, NaN) never panic.
        for ratio in [0.0, -3.0, 7.0, f32::NAN] {
            let mut buf = Buffer::empty(screen);
            Meter {
                ratio,
                gradient: Gradient::Solid(Color::Red),
                track: Color::Gray,
            }
            .render(screen, &mut buf);
        }
        // An idle meter keeps its groove instead of vanishing.
        let mut buf = Buffer::empty(screen);
        Meter {
            ratio: 0.0,
            gradient: Gradient::Solid(Color::Red),
            track: Color::Gray,
        }
        .render(screen, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "▏");
    }

    #[test]
    fn core_bar_clamps_and_scales() {
        let g = || Gradient::Solid(Color::Red);
        assert_eq!(core_bar(0.0, g()).0, ' ');
        assert_eq!(core_bar(1.0, g()).0, '█');
        assert_eq!(core_bar(55.0, g()).0, '█', "overload clamps");
        assert_eq!(core_bar(-9.0, g()).0, ' ');
        let _ = core_bar(f32::NAN, g()); // total
    }

    #[test]
    fn fill_bg_paints_every_cell() {
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::empty(area);
        fill_bg(area, &mut buf, Color::Blue);
        assert!(buf.content.iter().all(|c| c.bg == Color::Blue));
    }

    mod prop {
        use super::super::{axis_window, graph_dots};
        use proptest::prelude::*;

        proptest! {
            // Ring data reaches graphs unfiltered — any float must map into
            // the dot budget (this is what keeps panels panic-free).
            #[test]
            fn graph_dots_stays_in_budget(
                v in proptest::num::f32::ANY,
                max in proptest::num::f32::ANY,
                budget in 1i64..=256,
            ) {
                let d = graph_dots(Some(v), max, budget);
                prop_assert!((0..=budget).contains(&d));
            }

            // Any float soup (ring data, fuzzed step/span) must yield either
            // no window or an ordered one inside the clamp — LineGraph
            // divides by (hi - lo), so hi > lo is load-bearing.
            #[test]
            fn axis_window_is_total_and_ordered(
                values in proptest::collection::vec(proptest::num::f32::ANY, 0..64),
                step in proptest::num::f32::ANY,
                min_span in proptest::num::f32::ANY,
            ) {
                if let Some((lo, hi)) = axis_window(&values, step, min_span, (0.0, 110.0)) {
                    prop_assert!(lo < hi);
                    prop_assert!((0.0..=110.0).contains(&lo));
                    prop_assert!((0.0..=110.0).contains(&hi));
                }
            }
        }
    }
}
