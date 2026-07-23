//! Process control: kill/signal/renice, with a dry-run plan, an interactive
//! confirmation by default, and a fail-closed policy off a terminal (an
//! automated caller must pass `--yes` before anything is signaled).

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use super::args::{KillArgs, ReniceArgs, SignalArgs};
use super::output;
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

pub fn kill(a: &KillArgs) -> ExitCode {
    act(&a.pids, a.dry_run, a.yes, &format!("send {} to", signal_name(a.signal)), |pid| {
        procs::kill(pid, a.signal)
    })
}

pub fn signal(a: &SignalArgs) -> ExitCode {
    act(&a.pids, a.dry_run, a.yes, &format!("send {} to", signal_name(a.signal)), |pid| {
        procs::kill(pid, a.signal)
    })
}

pub fn renice(a: &ReniceArgs) -> ExitCode {
    act(&a.pids, a.dry_run, a.yes, &format!("set niceness {} on", a.nice), |pid| {
        procs::renice(pid, a.nice)
    })
}

fn act(
    pids: &[i32],
    dry_run: bool,
    yes: bool,
    verb: &str,
    mut apply: impl FnMut(i32) -> Result<(), String>,
) -> ExitCode {
    let targets = resolve(pids);
    let plan: Vec<String> = targets.iter().map(|t| format!("{} ({})", t.pid, t.name)).collect();

    if dry_run {
        println!("dry run: would {verb} {}", plan.join(", "));
        return ExitCode::SUCCESS;
    }

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if !yes {
        if !interactive {
            eprintln!(
                "mxmon: refusing to {verb} {} process(es) without --yes (not a terminal)",
                pids.len()
            );
            return ExitCode::from(output::REFUSED);
        }
        eprint!("{verb} {} ? [y/N] ", plan.join(", "));
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() || !line.trim().eq_ignore_ascii_case("y") {
            eprintln!("aborted.");
            return ExitCode::SUCCESS;
        }
    }

    let mut worst = ExitCode::SUCCESS;
    for t in &targets {
        match apply(t.pid) {
            Ok(()) => println!("{} ({}): ok", t.pid, t.name),
            Err(e) => {
                eprintln!("{} ({}): {e}", t.pid, t.name);
                worst = ExitCode::from(output::REFUSED);
            }
        }
    }
    worst
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
