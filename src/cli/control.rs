//! Process control: kill/signal/renice, with a dry-run plan, an interactive
//! confirmation by default, and a fail-closed policy off a terminal (an
//! automated caller must pass `--yes` before anything is signaled).
//!
//! The verbs honor the output flags like every other command: `--format json`
//! turns both the plan and the result into one object per target, so an agent
//! can act and then parse what happened rather than scraping prose.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use serde_json::json;

use super::args::{GlobalArgs, KillArgs, ReniceArgs, SignalArgs};
use super::output::{self, OutputCtx};
use super::render;
use crate::collect::procs;

struct Target {
    pid: i32,
    name: String,
}

fn resolve(pids: &[i32]) -> Vec<Target> {
    pids.iter()
        .map(|&pid| {
            let name = crate::ffi::proc::pid_path(pid)
                .and_then(|p| p.rsplit('/').next().map(str::to_owned))
                .unwrap_or_else(|| "?".to_owned());
            Target { pid, name }
        })
        .collect()
}

/// What the verb is about to do, in both voices: prose for the prompt, and the
/// fields a machine reader wants.
struct Action {
    /// Reads as an imperative phrase: "send SIGTERM to 431 (node)".
    verb: String,
    /// `"signal"` or `"renice"`.
    kind: &'static str,
    /// The signal name or the new niceness.
    detail: serde_json::Value,
}

pub fn kill(a: &KillArgs, g: &GlobalArgs) -> ExitCode {
    let sig = signal_name(a.signal);
    let action = Action {
        verb: format!("send {sig} to"),
        kind: "signal",
        detail: json!(sig),
    };
    act(&a.pids, a.dry_run, a.yes, &action, g, |pid| {
        procs::kill(pid, a.signal)
    })
}

pub fn signal(a: &SignalArgs, g: &GlobalArgs) -> ExitCode {
    let sig = signal_name(a.signal);
    let action = Action {
        verb: format!("send {sig} to"),
        kind: "signal",
        detail: json!(sig),
    };
    act(&a.pids, a.dry_run, a.yes, &action, g, |pid| {
        procs::kill(pid, a.signal)
    })
}

pub fn renice(a: &ReniceArgs, g: &GlobalArgs) -> ExitCode {
    let action = Action {
        verb: format!("set niceness {} on", a.nice),
        kind: "renice",
        detail: json!(a.nice),
    };
    act(&a.pids, a.dry_run, a.yes, &action, g, |pid| {
        procs::renice(pid, a.nice)
    })
}

fn act(
    pids: &[i32],
    dry_run: bool,
    yes: bool,
    action: &Action,
    g: &GlobalArgs,
    mut apply: impl FnMut(i32) -> Result<(), String>,
) -> ExitCode {
    let ctx = OutputCtx::resolve(g, super::args::Format::Table, super::args::Format::Table);
    let targets = resolve(pids);
    let plan: Vec<String> = targets
        .iter()
        .map(|t| format!("{} ({})", t.pid, t.name))
        .collect();

    if dry_run {
        if ctx.is_structured() {
            let rows: Vec<serde_json::Value> =
                targets.iter().map(|t| row(t, action, None, None)).collect();
            render::emit(ctx, &serde_json::Value::Array(rows));
        } else {
            println!("dry run: would {} {}", action.verb, plan.join(", "));
        }
        return ExitCode::SUCCESS;
    }

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if !yes {
        if !interactive {
            eprintln!(
                "mxmon: refusing to {} {} process(es) without --yes (not a terminal)",
                action.verb,
                pids.len()
            );
            return ExitCode::from(output::REFUSED);
        }
        eprint!("{} {} ? [y/N] ", action.verb, plan.join(", "));
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() || !line.trim().eq_ignore_ascii_case("y")
        {
            eprintln!("aborted.");
            return ExitCode::SUCCESS;
        }
    }

    let mut worst = ExitCode::SUCCESS;
    let mut rows = Vec::new();
    for t in &targets {
        match apply(t.pid) {
            Ok(()) => {
                if ctx.is_structured() {
                    rows.push(row(t, action, Some(true), None));
                } else if !ctx.quiet {
                    println!("{} ({}): ok", t.pid, t.name);
                }
            }
            Err(e) => {
                if ctx.is_structured() {
                    rows.push(row(t, action, Some(false), Some(&e)));
                } else {
                    // Failures print even under `--quiet`: an action that did
                    // not happen is not decoration.
                    eprintln!("{} ({}): {e}", t.pid, t.name);
                }
                worst = ExitCode::from(output::REFUSED);
            }
        }
    }
    if ctx.is_structured() {
        render::emit(ctx, &serde_json::Value::Array(rows));
    }
    worst
}

/// One machine-readable result. `ok` is tri-state on purpose: `None` is a
/// dry run, where nothing was attempted and neither `true` nor `false` is true.
fn row(t: &Target, action: &Action, ok: Option<bool>, error: Option<&str>) -> serde_json::Value {
    json!({
        "pid": t.pid,
        "name": t.name,
        "action": action.kind,
        action.kind: action.detail,
        "ok": ok,
        "error": error,
    })
}

fn signal_name(sig: i32) -> String {
    match sig {
        libc::SIGTERM => "SIGTERM".to_owned(),
        libc::SIGKILL => "SIGKILL".to_owned(),
        libc::SIGINT => "SIGINT".to_owned(),
        libc::SIGHUP => "SIGHUP".to_owned(),
        libc::SIGQUIT => "SIGQUIT".to_owned(),
        libc::SIGSTOP => "SIGSTOP".to_owned(),
        libc::SIGCONT => "SIGCONT".to_owned(),
        n => format!("signal {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_row(kind: &'static str, detail: serde_json::Value) -> serde_json::Value {
        let t = Target {
            pid: 431,
            name: "node".to_owned(),
        };
        let action = Action {
            verb: "send SIGTERM to".to_owned(),
            kind,
            detail,
        };
        row(&t, &action, Some(true), None)
    }

    use crate::cli::args::Format;

    fn globals(format: Format) -> GlobalArgs {
        GlobalArgs {
            format,
            timeout: None,
            no_color: true,
            quiet: false,
        }
    }

    fn term() -> Action {
        Action {
            verb: "send SIGTERM to".to_owned(),
            kind: "signal",
            detail: json!("SIGTERM"),
        }
    }

    /// An impossible pid is the one target that is safe to name in a test: it
    /// can never resolve to a real process, so nothing can be signaled by
    /// accident even if a guard regressed.
    const IMPOSSIBLE: i32 = 2_147_483_640;

    #[test]
    fn a_target_that_does_not_exist_still_resolves_to_a_row() {
        let targets = resolve(&[IMPOSSIBLE]);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].pid, IMPOSSIBLE);
        assert_eq!(
            targets[0].name, "?",
            "an unresolvable name is stated, not guessed"
        );
        // A pid that does exist resolves to its basename.
        let init = resolve(&[1]);
        assert!(!init[0].name.is_empty());
        assert!(!init[0].name.contains('/'), "the path is trimmed to a name");
    }

    #[test]
    fn a_dry_run_never_reaches_the_apply_closure() {
        for format in [Format::Table, Format::Json, Format::Ndjson] {
            let mut called = 0;
            let code = act(&[IMPOSSIBLE], true, true, &term(), &globals(format), |_| {
                called += 1;
                Ok(())
            });
            assert_eq!(called, 0, "dry run must not act ({format:?})");
            assert_eq!(
                format!("{code:?}"),
                format!("{:?}", ExitCode::SUCCESS),
                "a plan is a success"
            );
        }
    }

    /// Under `cargo test` stdout is a pipe, so this exercises the automation
    /// path: the guard that stops a piped or scripted caller from mass-signaling
    /// without saying so explicitly.
    #[test]
    fn off_a_terminal_it_fails_closed_without_yes() {
        let mut called = 0;
        let code = act(
            &[IMPOSSIBLE],
            false,
            false,
            &term(),
            &globals(Format::Table),
            |_| {
                called += 1;
                Ok(())
            },
        );
        assert_eq!(called, 0, "nothing may be signaled without --yes off a tty");
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::from(output::REFUSED))
        );
    }

    #[test]
    fn with_yes_every_target_is_attempted_and_a_failure_is_reported() {
        let mut seen = Vec::new();
        let code = act(
            &[IMPOSSIBLE, IMPOSSIBLE - 1],
            false,
            true,
            &term(),
            &globals(Format::Table),
            |pid| {
                seen.push(pid);
                if pid == IMPOSSIBLE {
                    Ok(())
                } else {
                    Err("no such process".to_owned())
                }
            },
        );
        assert_eq!(
            seen,
            vec![IMPOSSIBLE, IMPOSSIBLE - 1],
            "every target is tried"
        );
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::from(output::REFUSED)),
            "one failure makes the whole run a failure"
        );

        // All succeeding is a clean exit.
        let ok = act(
            &[IMPOSSIBLE],
            false,
            true,
            &term(),
            &globals(Format::Json),
            |_| Ok(()),
        );
        assert_eq!(format!("{ok:?}"), format!("{:?}", ExitCode::SUCCESS));
    }

    #[test]
    fn machine_rows_name_the_action_and_its_argument() {
        let v = ctx_row("signal", json!("SIGTERM"));
        assert_eq!(v["pid"], 431);
        assert_eq!(v["name"], "node");
        assert_eq!(v["action"], "signal");
        assert_eq!(
            v["signal"], "SIGTERM",
            "the argument is keyed by the action"
        );
        assert_eq!(v["ok"], true);
        assert!(v["error"].is_null());

        let r = ctx_row("renice", json!(5));
        assert_eq!(r["action"], "renice");
        assert_eq!(r["renice"], 5);
    }

    #[test]
    fn signal_names_cover_the_ones_the_parser_accepts() {
        // Every name `args::parse_signal` maps must round-trip back to a name,
        // or a dry-run plan would read "send signal 9 to" for a known signal.
        for name in ["HUP", "INT", "QUIT", "KILL", "TERM", "STOP", "CONT"] {
            let n = super::super::args::parse_signal(name).unwrap();
            assert_eq!(signal_name(n), format!("SIG{name}"));
        }
        // An unnamed one still renders, just numerically.
        assert_eq!(
            signal_name(libc::SIGUSR1),
            format!("signal {}", libc::SIGUSR1)
        );
    }
}
