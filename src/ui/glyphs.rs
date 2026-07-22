//! Octant upgrade pass: braille render-space, octant presentation.
//!
//! Every graph draws in braille (`widgets::BRAILLE`, `0x2800 + bits`), and
//! braille and Unicode 16 octants share the same 2×4 sub-cell grid — so the
//! upgrade is a lossless bit-for-bit char remap over the finished frame,
//! the glyph sibling of [`crate::ui::theme::quantize_buffer`]. Terminals
//! that synthesize Symbols for Legacy Computing Supplement glyphs (Ghostty,
//! Kitty, WezTerm, foot) get solid contiguous fills with no dot gaps;
//! everything else keeps braille. The `glyphs` config key picks the mode.

use std::sync::OnceLock;

use ratatui::buffer::Buffer;
use ratatui::symbols::pixel::OCTANTS;

use crate::config::Glyphs;

/// Braille dot-bit `k` (`1 << k`) → row-major octant bit index. Braille
/// numbers dots column-major with dots 7/8 appended (0x01,0x02,0x04 down the
/// left column, 0x08,0x10,0x20 down the right, 0x40/0x80 the bottom row);
/// octants number sub-cells row-major (`bit = row * 2 + col`).
const OCT_BIT: [u32; 8] = [0, 2, 4, 1, 3, 5, 6, 7];

/// Braille dot-pattern → octant char, for all 256 patterns. Built at compile
/// time from ratatui's row-major [`OCTANTS`] table.
static LUT: [char; 256] = {
    let mut lut = [' '; 256];
    let mut bits = 0usize;
    while bits < 256 {
        let mut oct = 0usize;
        let mut k = 0;
        while k < 8 {
            if bits & (1 << k) != 0 {
                oct |= 1 << OCT_BIT[k];
            }
            k += 1;
        }
        lut[bits] = OCTANTS[oct];
        bits += 1;
    }
    lut
};

/// Should the octant pass run for this configured mode?
pub fn active(mode: Glyphs) -> bool {
    match mode {
        Glyphs::Auto => octants_supported(),
        Glyphs::Octant => true,
        Glyphs::Braille => false,
    }
}

/// Whether the hosting terminal is known to draw octant glyphs itself
/// (font-independent). Conservative allowlist — anything unrecognized
/// (Apple Terminal, iTerm2, unknown emulators) stays on braille; `--glyphs
/// octant` or the settings modal forces the upgrade where detection can't
/// see (e.g. inside tmux).
fn octants_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        probe(
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var_os("KITTY_WINDOW_ID").is_some(),
            std::env::var_os("WEZTERM_EXECUTABLE").is_some(),
        )
    })
}

/// The pure decision behind [`octants_supported`], testable with injected
/// environment values.
fn probe(term_program: Option<&str>, term: Option<&str>, kitty: bool, wezterm: bool) -> bool {
    if kitty || wezterm {
        return true;
    }
    if matches!(term_program, Some("ghostty" | "WezTerm" | "kitty")) {
        return true;
    }
    term.is_some_and(|t| t.contains("ghostty") || t.contains("kitty") || t.starts_with("foot"))
}

/// Upgrade every braille cell in a finished frame to its octant twin — one
/// pass, chars only (colors are the quantize pass's business). Non-braille
/// cells (box drawing, eighth blocks, text, wide graphemes) pass through
/// untouched, so the pass is total over any buffer.
pub fn octantize_buffer(buf: &mut Buffer) {
    for cell in &mut buf.content {
        let mut chars = cell.symbol().chars();
        if let (Some(ch), None) = (chars.next(), chars.next())
            && ('\u{2800}'..='\u{28FF}').contains(&ch)
        {
            cell.set_char(LUT[(ch as u32 - 0x2800) as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LUT, active, octantize_buffer, probe};
    use crate::config::Glyphs;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn lut_maps_known_patterns() {
        // Empty, full, and the idle-baseline glyph land on their block twins.
        assert_eq!(LUT[0], ' ', "blank braille clears to a space");
        assert_eq!(LUT[0xFF], '█', "all eight dots fill the cell");
        assert_eq!(LUT[0xC0], '▂', "the ⣀ baseline becomes a solid hairline");
        // Dot 1 (top-left) is row-major octant bit 0.
        assert_eq!(LUT[0x01], ratatui::symbols::pixel::OCTANTS[1]);
        // Dot 4 (top-right, 0x08) is row-major bit 1.
        assert_eq!(LUT[0x08], ratatui::symbols::pixel::OCTANTS[2]);
        // The upper half: dots 1,2,4,5 (0x1B) → top two rows (0x0F).
        assert_eq!(LUT[0x1B], '▀');
        // The remap is a permutation: 256 distinct patterns, 256 distinct
        // glyphs (OCTANTS itself is injective).
        let mut seen: Vec<char> = LUT.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 256);
    }

    #[test]
    fn octantize_upgrades_braille_and_leaves_the_rest() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        buf[(0, 0)].set_char('⣿');
        buf[(1, 0)].set_char('⣀');
        buf[(2, 0)].set_char('▄'); // eighth-block meter glyph: not braille
        buf[(3, 0)].set_char('┈'); // schematic silkscreen
        buf[(4, 0)].set_symbol("a\u{300}"); // multi-char grapheme
        buf[(0, 0)].set_fg(ratatui::style::Color::Red);
        octantize_buffer(&mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "█");
        assert_eq!(buf[(1, 0)].symbol(), "▂");
        assert_eq!(buf[(2, 0)].symbol(), "▄", "non-braille untouched");
        assert_eq!(buf[(3, 0)].symbol(), "┈", "non-braille untouched");
        assert_eq!(buf[(4, 0)].symbol(), "a\u{300}", "graphemes untouched");
        assert_eq!(buf[(5, 0)].symbol(), " ", "blank cells untouched");
        assert_eq!(
            buf[(0, 0)].fg,
            ratatui::style::Color::Red,
            "colors are not this pass's business"
        );
    }

    #[test]
    fn active_resolves_modes() {
        assert!(active(Glyphs::Octant));
        assert!(!active(Glyphs::Braille));
        // Auto consults the process environment (OnceLock) — only assert it
        // is stable, not its value, so the test passes in any terminal.
        assert_eq!(active(Glyphs::Auto), active(Glyphs::Auto));
    }

    #[test]
    fn probe_allowlists_known_synthesizers_only() {
        assert!(probe(Some("ghostty"), None, false, false));
        assert!(probe(Some("WezTerm"), None, false, false));
        assert!(probe(Some("kitty"), None, false, false));
        assert!(probe(None, Some("xterm-ghostty"), false, false));
        assert!(probe(None, Some("xterm-kitty"), false, false));
        assert!(probe(None, Some("foot-extra"), false, false));
        assert!(probe(None, None, true, false), "KITTY_WINDOW_ID");
        assert!(probe(None, None, false, true), "WEZTERM_EXECUTABLE");
        // The conservative side: stock and unknown terminals stay braille.
        assert!(!probe(
            Some("Apple_Terminal"),
            Some("xterm-256color"),
            false,
            false
        ));
        assert!(!probe(
            Some("iTerm.app"),
            Some("xterm-256color"),
            false,
            false
        ));
        assert!(!probe(None, Some("tmux-256color"), false, false));
        assert!(!probe(None, None, false, false));
    }

    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            // The pass must be total over arbitrary cell contents — any
            // string a widget (or future code) leaves in a cell.
            #[test]
            fn octantize_never_panics(s in "\\PC{0,4}") {
                let area = Rect::new(0, 0, 2, 1);
                let mut buf = Buffer::empty(area);
                buf[(0, 0)].set_symbol(&s);
                octantize_buffer(&mut buf);
                // Braille in → octant/block out; anything else unchanged.
                let mut chars = s.chars();
                if let (Some(ch), None) = (chars.next(), chars.next())
                    && ('\u{2800}'..='\u{28FF}').contains(&ch)
                {
                    prop_assert_eq!(
                        buf[(0, 0)].symbol(),
                        LUT[(ch as u32 - 0x2800) as usize].to_string()
                    );
                } else if !s.is_empty() {
                    prop_assert_eq!(buf[(0, 0)].symbol(), s);
                }
            }
        }
    }
}
