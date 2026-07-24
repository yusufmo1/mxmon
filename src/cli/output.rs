//! Where format, terminal detection, and color are decided once, plus the
//! exit-code convention shared across subcommands.
//!
//! # Exit codes
//!
//! This table is the contract. `AGENTS.md`, the root `after_help`, and the
//! `tests/cli.rs` matrix all quote it, so change it here and follow the
//! references.
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | success; `check` true; `health` ok |
//! | 1 | `check` false; `health` warn or crit |
//! | 2 | usage error: a bad flag, an unknown path, an unknown group, a malformed expression |
//! | 3 | no usable data: every source down, or a settle that produced nothing |
//! | 4 | a control action was refused or failed |
//! | 5 | `check` was undecidable because a referenced source was null |
//!
//! Code 2 is deliberately clap's own usage-error code: "you named something
//! that does not exist" is one outcome whether clap or the selector caught it.
//! That is why an undecidable `check` needs its own code rather than sharing 2,
//! which would leave an agent unable to tell a typo from a dead sensor.

use std::io::IsTerminal;

use super::args::{Format, GlobalArgs};

/// Assertion held false, or a health verdict at warn/crit. A normal,
/// script-actionable outcome, not an error.
pub const ASSERT_FALSE: u8 = 1;
/// Something was named that does not exist: an unknown dot-path, an unknown
/// `--only` group, or a malformed `check` expression. Matches clap's own code
/// for a bad flag, since it is the same class of mistake.
pub const USAGE: u8 = 2;
/// No usable data: every source down, or a settle that produced nothing.
pub const NO_DATA: u8 = 3;
/// An operation was refused or failed (kill denied, non-tty without `--yes`).
pub const REFUSED: u8 = 4;
/// An expression could not be decided because a referenced source was null.
/// Distinct from [`ASSERT_FALSE`]: the answer is unknown, not no.
pub const UNDECIDABLE: u8 = 5;

/// Resolved output context: the concrete format, whether to color, and whether
/// to suppress decoration.
#[derive(Clone, Copy)]
pub struct OutputCtx {
    pub format: Format,
    pub color: bool,
    /// `--quiet`: drop headers, badges, and prose. Data and errors still print.
    pub quiet: bool,
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
        Self {
            format,
            color,
            quiet: g.quiet,
        }
    }

    /// True when the shape is data-only (JSON, NDJSON, or compact `key=value`).
    /// Those render through one shared emitter; the human shapes are
    /// per-command, so they stay at their call sites.
    pub fn is_structured(self) -> bool {
        matches!(self.format, Format::Json | Format::Ndjson | Format::Compact)
    }
}
