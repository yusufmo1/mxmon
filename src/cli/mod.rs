//! The headless command surface: everything mxmon does that is not the
//! interactive TUI. Sits beside `ui/` and reads the same `collect/` layer; no
//! rendering here ever enters the ratatui path.
//!
//! [`dispatch`] is the single entry `main` calls. Bare `mxmon` (no subcommand)
//! runs the TUI; the legacy `--json`/`--interval`/`--theme`/`--glyphs` flags are
//! preserved on that path.

pub mod args;
pub mod collect;
mod commands;
mod control;
mod debug;
mod flatten;
mod output;
mod render;
mod watch;

use std::process::ExitCode;

use args::{Cli, Command};

/// Route a parsed [`Cli`] to its handler. `schema` and `debug` never load SoC
/// facts, so they run on a machine without Apple Silicon telemetry.
pub fn dispatch(cli: Cli) -> color_eyre::Result<ExitCode> {
    if let Some(code) = reject_legacy_with_subcommand(&cli) {
        return Ok(code);
    }
    match cli.command {
        Some(Command::Schema) => Ok(commands::schema(&cli.global)),
        Some(Command::Debug(d)) => {
            debug::run(d)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Completions { shell }) => {
            commands::completions(shell);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Man) => {
            commands::man()?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Snapshot(a)) => {
            let soc = crate::collect::soc::load()?;
            Ok(commands::snapshot(&soc, &a, &cli.global))
        }
        Some(Command::Get(a)) => {
            let soc = crate::collect::soc::load()?;
            Ok(commands::get(&soc, &a, &cli.global))
        }
        Some(Command::Watch(a)) => {
            let soc = crate::collect::soc::load()?;
            Ok(watch::run(&soc, &a, &cli.global))
        }
        Some(Command::Top(a)) => {
            let soc = crate::collect::soc::load()?;
            Ok(commands::top(&soc, &a, &cli.global))
        }
        Some(Command::Check(a)) => {
            let soc = crate::collect::soc::load()?;
            Ok(commands::check_cmd(&soc, &a, &cli.global))
        }
        Some(Command::Health) => {
            let soc = crate::collect::soc::load()?;
            Ok(commands::health_cmd(&soc, &cli.global))
        }
        Some(Command::Explain(a)) => {
            let soc = crate::collect::soc::load()?;
            Ok(commands::explain_cmd(&soc, &a, &cli.global))
        }
        Some(Command::Kill(a)) => Ok(control::kill(&a, &cli.global)),
        Some(Command::Signal(a)) => Ok(control::signal(&a, &cli.global)),
        Some(Command::Renice(a)) => Ok(control::renice(&a, &cli.global)),
        None => legacy(&cli),
    }
}

/// The four TUI flags shape the bare invocation and nothing else. Combined with
/// a subcommand they used to be dropped in silence, which is the worst outcome:
/// `mxmon --interval 100 watch` looked like it set the stream cadence and did
/// not. Refuse, and name the flag that does the job.
fn reject_legacy_with_subcommand(cli: &Cli) -> Option<ExitCode> {
    let Some(command) = &cli.command else {
        return None;
    };
    let name = command_name(command);
    let l = &cli.legacy;
    let (flag, hint) = if l.json {
        ("--json", "it is an alias for `mxmon snapshot`; drop it")
    } else if l.interval.is_some() {
        (
            "--interval",
            "use `mxmon watch --interval <dur>` for the stream cadence",
        )
    } else if l.theme.is_some() {
        ("--theme", "themes only apply to the TUI")
    } else if l.glyphs.is_some() {
        ("--glyphs", "glyph sets only apply to the TUI")
    } else {
        return None;
    };
    eprintln!("error: `{flag}` shapes the TUI and cannot be combined with `{name}`");
    eprintln!("  tip: {hint}");
    Some(ExitCode::from(output::USAGE))
}

fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Snapshot(_) => "snapshot",
        Command::Get(_) => "get",
        Command::Watch(_) => "watch",
        Command::Top(_) => "top",
        Command::Check(_) => "check",
        Command::Health => "health",
        Command::Explain(_) => "explain",
        Command::Schema => "schema",
        Command::Completions { .. } => "completions",
        Command::Man => "man",
        Command::Kill(_) => "kill",
        Command::Signal(_) => "signal",
        Command::Renice(_) => "renice",
        Command::Debug(_) => "debug",
    }
}

/// Bare invocation: the TUI, with the preserved legacy flags.
fn legacy(cli: &Cli) -> color_eyre::Result<ExitCode> {
    let soc = crate::collect::soc::load()?;
    crate::trace::mark("soc facts loaded");

    if cli.legacy.json {
        commands::print_snapshot_json(&soc, cli.global.timeout);
        return Ok(ExitCode::SUCCESS);
    }

    let mut config = crate::config::Config::load();
    if let Some(interval) = cli.legacy.interval {
        config.interval_ms = interval.clamp(
            crate::collect::sampler::FAST_MS_MIN,
            crate::collect::sampler::FAST_MS_MAX,
        );
    }
    if let Some(theme) = &cli.legacy.theme {
        config.theme.clone_from(theme);
    }
    if let Some(glyphs) = cli.legacy.glyphs {
        config.glyphs = glyphs;
    }
    crate::run_tui(soc, config)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("clap accepts it")
    }

    #[test]
    fn a_tui_flag_beside_a_subcommand_is_refused() {
        // Each of the four, against a verb that plausibly tempts it.
        for args in [
            vec!["mxmon", "--json", "snapshot"],
            vec!["mxmon", "--interval", "100", "watch"],
            vec!["mxmon", "--theme", "neon", "health"],
            vec!["mxmon", "--glyphs", "braille", "top"],
        ] {
            assert!(
                reject_legacy_with_subcommand(&parse(&args)).is_some(),
                "{args:?} should be refused, not silently ignored"
            );
        }
    }

    #[test]
    fn the_tui_flags_are_fine_on_a_bare_invocation() {
        for args in [
            vec!["mxmon"],
            vec!["mxmon", "--json"],
            vec!["mxmon", "--interval", "500"],
            vec!["mxmon", "--theme", "neon"],
            vec!["mxmon", "--glyphs", "octant"],
        ] {
            assert!(
                reject_legacy_with_subcommand(&parse(&args)).is_none(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn output_flags_are_never_confused_for_tui_flags() {
        // These are global and belong with any verb, before or after it.
        for args in [
            vec!["mxmon", "--format", "json", "snapshot"],
            vec!["mxmon", "snapshot", "--format", "json"],
            vec!["mxmon", "--no-color", "-q", "health"],
            vec!["mxmon", "--timeout", "2s", "get", "soc.chip"],
        ] {
            assert!(
                reject_legacy_with_subcommand(&parse(&args)).is_none(),
                "{args:?}"
            );
        }
    }

    /// Every variant must name itself, or the refusal message would read
    /// "cannot be combined with ``" for whichever one was forgotten.
    #[test]
    fn every_command_reports_its_own_name() {
        for (args, want) in [
            (vec!["mxmon", "snapshot"], "snapshot"),
            (vec!["mxmon", "get", "soc.chip"], "get"),
            (vec!["mxmon", "watch"], "watch"),
            (vec!["mxmon", "top"], "top"),
            (vec!["mxmon", "check", "true"], "check"),
            (vec!["mxmon", "health"], "health"),
            (vec!["mxmon", "explain", "thermal"], "explain"),
            (vec!["mxmon", "schema"], "schema"),
            (vec!["mxmon", "completions", "zsh"], "completions"),
            (vec!["mxmon", "man"], "man"),
            (vec!["mxmon", "kill", "1"], "kill"),
            (vec!["mxmon", "signal", "TERM", "1"], "signal"),
            (vec!["mxmon", "renice", "5", "1"], "renice"),
            (vec!["mxmon", "debug", "smc"], "debug"),
        ] {
            let cli = parse(&args);
            assert_eq!(command_name(cli.command.as_ref().unwrap()), want);
        }
    }
}
