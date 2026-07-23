//! Where format, terminal detection, and color are decided once, plus the
//! exit-code convention shared across subcommands.

use std::io::IsTerminal;

use super::args::{Format, GlobalArgs};

/// Assertion held false, or a health verdict at warn/crit. A normal,
/// script-actionable outcome, not an error.
pub const ASSERT_FALSE: u8 = 1;
/// A referenced source resolved to null (an expression could not be decided).
pub const UNKNOWN: u8 = 2;
/// No usable data (every source down, a timeout with nothing, or a bad path).
pub const NO_DATA: u8 = 3;
/// An operation was refused or failed (kill denied, non-tty without `--yes`).
pub const REFUSED: u8 = 4;

/// Resolved output context: the concrete format, whether to color, and whether
/// to stay quiet.
#[derive(Clone, Copy)]
pub struct OutputCtx {
    pub format: Format,
    pub color: bool,
}

impl OutputCtx {
    /// Resolve `--format auto` to `human` on a terminal or `machine` when piped;
    /// an explicit `--format` always wins. Color follows the terminal and the
    /// `NO_COLOR` convention.
    pub fn resolve(g: &GlobalArgs, human: Format, machine: Format) -> Self {
        let tty = std::io::stdout().is_terminal();
        let format = match g.format {
            Format::Auto => {
                if tty {
                    human
                } else {
                    machine
                }
            }
            explicit => explicit,
        };
        let color = !g.no_color && tty && std::env::var_os("NO_COLOR").is_none();
        Self { format, color }
    }
}
