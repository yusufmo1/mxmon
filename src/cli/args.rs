//! The command-line surface: an optional subcommand plus the preserved legacy
//! TUI flags. Bare `mxmon` (no subcommand) launches the TUI, so muscle memory
//! and existing scripts keep working while the headless verbs grow beside them.

use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "mxmon",
    version,
    about = "mxmon — a sudoless Apple Silicon terminal monitor"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub legacy: LegacyFlags,

    #[command(flatten)]
    pub global: GlobalArgs,
}

/// Flags that shape the TUI (bare invocation). Kept for backward compatibility;
/// a subcommand takes precedence over them.
#[derive(Args, Debug)]
pub struct LegacyFlags {
    /// Print one JSON snapshot of every metric and exit (alias for `snapshot`).
    #[arg(long)]
    pub json: bool,

    /// Fast-tier sampling interval in milliseconds (100-2000); overrides config.
    #[arg(long)]
    pub interval: Option<u64>,

    /// Theme name (midnight, neon, gruvbox, and more); overrides config.
    #[arg(long)]
    pub theme: Option<String>,

    /// Sub-cell glyph set for graphs; overrides config.
    #[arg(long, value_enum)]
    pub glyphs: Option<crate::config::Glyphs>,
}

/// Cross-cutting output controls, valid before or after any subcommand.
#[derive(Args, Debug, Clone)]
pub struct GlobalArgs {
    /// Output format. `auto` prints a human summary on a terminal and machine
    /// output when piped.
    #[arg(long, value_enum, default_value = "auto", global = true)]
    pub format: Format,

    /// Deadline for read commands, e.g. `8s`, `500ms`, `2m`.
    #[arg(long, value_parser = parse_duration, global = true)]
    pub timeout: Option<Duration>,

    /// Never emit ANSI color, even on a terminal (also honors $NO_COLOR).
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Suppress human notes; print only the data.
    #[arg(long, short, global = true)]
    pub quiet: bool,
}

/// Output shape. `Auto` resolves to a human summary on a tty and machine output
/// when piped; the rest force a specific shape.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[value(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Auto,
    Json,
    Ndjson,
    Compact,
    Table,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print one settled snapshot of every metric.
    Snapshot(SnapshotArgs),

    /// Extract values by dot-path, e.g. `power.package_w`.
    Get(GetArgs),

    /// Watch metrics as a bounded stream (NDJSON), then exit.
    Watch(WatchArgs),

    /// Rank the top consumers of a resource.
    Top(TopArgs),

    /// Evaluate a boolean assertion over the report; exit 0 true, 1 false.
    Check(CheckArgs),

    /// Print a composite health verdict.
    Health,

    /// Explain a topic (thermal, power, slow, battery, network, disk).
    Explain(ExplainArgs),

    /// Print the JSON Schema of the report contract (no hardware needed).
    Schema,

    /// Print a shell completion script (bash, zsh, fish, and more).
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Print a roff man page for mxmon.
    Man,

    /// Send a signal to processes (default SIGTERM).
    Kill(KillArgs),

    /// Send a named or numbered signal to processes.
    Signal(SignalArgs),

    /// Change the scheduling niceness of processes.
    Renice(ReniceArgs),

    /// Developer dumps of raw sources (hidden).
    #[command(hide = true, subcommand)]
    Debug(DebugCmd),
}

#[derive(Args, Debug)]
pub struct SnapshotArgs {
    /// Restrict to these top-level groups, comma-separated (e.g. `power,thermal`).
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<String>,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// One or more dot-paths, e.g. `power.package_w processes.top.0.pid`.
    #[arg(required = true)]
    pub paths: Vec<String>,

    /// Print string values unquoted, for shell interpolation.
    #[arg(long)]
    pub raw: bool,
}

#[derive(Args, Debug)]
pub struct WatchArgs {
    /// Dot-paths to include in each frame; omit for the whole report.
    pub paths: Vec<String>,

    /// Per-frame interval, e.g. `500ms` (100-2000ms). Defaults to config.
    #[arg(long, value_parser = parse_duration)]
    pub interval: Option<Duration>,

    /// Stop after this long, e.g. `10s`.
    #[arg(long, value_parser = parse_duration)]
    pub r#for: Option<Duration>,

    /// Stop after this many frames.
    #[arg(long)]
    pub count: Option<u64>,
}

#[derive(Args, Debug)]
pub struct TopArgs {
    /// What to rank by.
    #[arg(value_enum, default_value = "cpu")]
    pub by: TopBy,

    /// How many rows to show.
    #[arg(long, short, default_value = "10")]
    pub count: usize,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum TopBy {
    Cpu,
    Power,
    Mem,
    Disk,
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// The expression, e.g. `thermal.throttling == false and power.package_w < 40`.
    #[arg(required = true)]
    pub expr: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ExplainArgs {
    /// The topic to diagnose.
    #[arg(value_enum)]
    pub topic: Topic,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum Topic {
    Thermal,
    Power,
    Slow,
    Battery,
    Network,
    Disk,
}

impl Topic {
    pub fn as_str(self) -> &'static str {
        match self {
            Topic::Thermal => "thermal",
            Topic::Power => "power",
            Topic::Slow => "slow",
            Topic::Battery => "battery",
            Topic::Network => "network",
            Topic::Disk => "disk",
        }
    }
}

#[derive(Args, Debug)]
pub struct KillArgs {
    /// Process IDs to signal.
    #[arg(required = true)]
    pub pids: Vec<i32>,

    /// Signal by name or number: TERM, KILL, 9, HUP, INT (default TERM).
    #[arg(long, short, default_value = "TERM", value_parser = parse_signal)]
    pub signal: i32,

    /// Resolve and print the plan without signaling anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Proceed without the confirmation prompt (required when not a terminal).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct SignalArgs {
    /// Signal by name or number: TERM, KILL, 9, HUP, INT, STOP, CONT.
    #[arg(value_parser = parse_signal)]
    pub signal: i32,

    /// Process IDs to signal.
    #[arg(required = true)]
    pub pids: Vec<i32>,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct ReniceArgs {
    /// New niceness (-20 highest priority to 19 lowest; raising priority or
    /// renicing another user's process needs privilege).
    pub nice: i32,

    /// Process IDs to renice.
    #[arg(required = true)]
    pub pids: Vec<i32>,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum DebugCmd {
    /// Raw per-interface network counters.
    Net,
    /// Readable float SMC power/voltage/current keys.
    Smc,
    /// Time each collector's sample cost.
    Bench,
    /// Raw ntstat stream and calibrated offsets.
    Flows,
    /// NVMe SMART page, APFS cache stats, interrupt rates.
    Smart,
}

/// Parse a human duration like `500ms`, `8s`, or `2m` into a [`Duration`].
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit() && c != '.');
    let (num, unit) = match split {
        Some(0) | None => return Err(format!("expected a number and unit in {s:?} (e.g. 8s, 500ms)")),
        Some(i) => t.split_at(i),
    };
    let n: f64 = num.parse().map_err(|_| format!("bad number in {s:?}"))?;
    if !n.is_finite() || n < 0.0 {
        return Err(format!("duration must be finite and non-negative in {s:?}"));
    }
    let secs = match unit.trim() {
        "ms" => n / 1000.0,
        "s" => n,
        "m" => n * 60.0,
        other => return Err(format!("unknown unit {other:?} in {s:?} (use ms, s, or m)")),
    };
    Ok(Duration::from_secs_f64(secs))
}

/// Parse a signal name (`TERM`, `SIGKILL`) or number (`9`) into its number.
pub fn parse_signal(s: &str) -> Result<i32, String> {
    let t = s.trim();
    if let Ok(n) = t.parse::<i32>() {
        return (1..=31).contains(&n).then_some(n).ok_or_else(|| format!("signal number out of range: {n}"));
    }
    let name = t.strip_prefix("SIG").unwrap_or(t).to_ascii_uppercase();
    let n = match name.as_str() {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "KILL" => libc::SIGKILL,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "TERM" => libc::SIGTERM,
        "STOP" => libc::SIGSTOP,
        "CONT" => libc::SIGCONT,
        other => return Err(format!("unknown signal {other:?}")),
    };
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_is_wellformed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bare_legacy_and_subcommands_parse() {
        assert!(Cli::try_parse_from(["mxmon"]).unwrap().command.is_none());
        assert!(Cli::try_parse_from(["mxmon", "--json"]).unwrap().legacy.json);
        assert_eq!(
            Cli::try_parse_from(["mxmon", "--interval", "500"]).unwrap().legacy.interval,
            Some(500)
        );
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "snapshot"]).unwrap().command,
            Some(Command::Snapshot(_))
        ));
        // The global `--format` must be accepted after a subcommand.
        assert!(Cli::try_parse_from(["mxmon", "snapshot", "--format", "json"]).is_ok());
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "snapshot", "--only", "power,thermal"]).unwrap().command,
            Some(Command::Snapshot(_))
        ));
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "get", "power.package_w"]).unwrap().command,
            Some(Command::Get(_))
        ));
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "check", "thermal.throttling", "==", "false"]).unwrap().command,
            Some(Command::Check(_))
        ));
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "debug", "smc"]).unwrap().command,
            Some(Command::Debug(_))
        ));
        // Clap still validates enums and required positionals.
        assert!(Cli::try_parse_from(["mxmon", "--glyphs", "sixel"]).is_err());
        assert!(Cli::try_parse_from(["mxmon", "get"]).is_err());
        assert!(Cli::try_parse_from(["mxmon", "kill"]).is_err());
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("8s").unwrap(), Duration::from_secs(8));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("-3s").is_err());
        assert!(parse_duration("ms").is_err());
    }

    #[test]
    fn signal_parsing() {
        assert_eq!(parse_signal("TERM").unwrap(), libc::SIGTERM);
        assert_eq!(parse_signal("SIGKILL").unwrap(), libc::SIGKILL);
        assert_eq!(parse_signal("9").unwrap(), 9);
        assert!(parse_signal("0").is_err());
        assert!(parse_signal("99").is_err());
        assert!(parse_signal("NOPE").is_err());
    }

    mod prop {
        use super::super::{parse_duration, parse_signal};
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn parsers_never_panic(s in ".*") {
                let _ = parse_duration(&s);
                let _ = parse_signal(&s);
            }
        }
    }
}
