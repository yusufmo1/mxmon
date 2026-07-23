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
mod output;
mod render;
mod watch;

use std::process::ExitCode;

use args::{Cli, Command};

/// Route a parsed [`Cli`] to its handler. `schema` and `debug` never load SoC
/// facts, so they run on a machine without Apple Silicon telemetry.
pub fn dispatch(cli: Cli) -> color_eyre::Result<ExitCode> {
    match cli.command {
        Some(Command::Schema) => {
            commands::schema();
            Ok(ExitCode::SUCCESS)
        }
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
            Ok(watch::run(&soc, &a))
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
        Some(Command::Kill(a)) => Ok(control::kill(&a)),
        Some(Command::Signal(a)) => Ok(control::signal(&a)),
        Some(Command::Renice(a)) => Ok(control::renice(&a)),
        None => legacy(&cli),
    }
}

/// Bare invocation: the TUI, with the preserved legacy flags.
fn legacy(cli: &Cli) -> color_eyre::Result<ExitCode> {
    let soc = crate::collect::soc::load()?;
    crate::trace::mark("soc facts loaded");

    if cli.legacy.json {
        commands::print_snapshot_json(&soc);
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
