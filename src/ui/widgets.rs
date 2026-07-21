//! Custom drawing primitives: filled braille graphs with vertical gradients,
//! eighth-block meters, single-cell core bars, and the mouse hit-map.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};

use crate::app::{SortKey, View};

use super::theme::Gradient;

/// Eighth-height blocks, empty → full (index 0 = blank).
const V_EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Eighth-width blocks for horizontal meters (index 0 = blank).
const H_EIGHTHS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Braille dot bits by (sub-column, dot-row top→bottom).
const BRAILLE: [[u16; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

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
    /// Kill-modal signal row.
    KillSignal(usize),
    /// Sort-menu row.
    SortOption(usize),
    /// Footer button opening the settings modal.
    Settings,
    /// Settings-modal row (click cycles the value forward).
    SettingRow(usize),
    /// Anywhere on a modal (consumes the click).
    ModalBody,
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
