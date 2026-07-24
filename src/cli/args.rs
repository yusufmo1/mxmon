//! The command-line surface: an optional subcommand plus the preserved legacy
//! TUI flags. Bare `mxmon` (no subcommand) launches the TUI, so muscle memory
//! and existing scripts keep working while the headless verbs grow beside them.
//!
//! Help text here is a deliverable, not a courtesy. An agent reads `--help`
//! once and then trusts it, so an advertised flag that does nothing is a silent
//! wrong answer. Two tests defend that: [`tests::every_arg_is_documented`]
//! walks the whole command tree for empty help strings, and the golden in
//! `help.golden.txt` turns any wording change into a reviewable diff.

use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The long-form help blocks. Kept out of the derive so the attributes stay
/// readable and the prose stays diffable.
mod help {
    pub const ROOT: &str = "\
Bare `mxmon` opens the TUI. Every verb below is headless: it settles one sample,
prints, and exits, with machine output whenever stdout is not a terminal.

Examples:
  mxmon                                    open the TUI
  mxmon snapshot                           one settled report (JSON when piped)
  mxmon get power.package_w                pull a single value
  mxmon check 'thermal.throttling == false' && cargo build
  mxmon watch cpu.self_ratio --for 10s     bounded NDJSON stream
  mxmon top power                          rank processes by a resource
  mxmon health                             composite verdict, exit 1 if degraded
  mxmon schema --format compact            every queryable path and its type

Exit codes:
  0  success; check true; health ok
  1  check false; health warn or crit
  2  usage error: bad flag, unknown path, unknown group, malformed expression
  3  no usable data: every source down
  4  a control action was refused or failed
  5  check undecidable: a referenced source was null

Start with `mxmon schema --format compact` to learn the contract, then query
leaves by name. Full guide: AGENTS.md";

    pub const SNAPSHOT: &str = "\
Examples:
  mxmon snapshot                        human summary on a terminal, JSON when piped
  mxmon snapshot --only power,thermal   just those groups (meta is always kept)
  mxmon snapshot --format compact       one greppable line per leaf

Keys are never omitted. A domain that is null is either down (see
source_errors), disabled (see meta.features), or unsettled (see meta.settled).";

    pub const GET: &str = "\
Examples:
  mxmon get power.package_w
  mxmon get power.ecpu.cores[0].freq_mhz processes.top[0].name
  mxmon get thermal --format compact    flatten a whole group to key=value

Array indices work as [0] or .0. Descending into a null source yields null and
exits 0, because the metric is legitimately unavailable. A path the contract
does not have exits 2.";

    pub const WATCH: &str = "\
Examples:
  mxmon watch --count 3                       three whole-report frames
  mxmon watch power.package_w --for 10s       one field, ten seconds
  mxmon watch cpu.self_ratio power.package_w --format table

Every path is checked before the first frame, so a typo exits 2 instead of
streaming nulls. Piped output is NDJSON and `| head` exits cleanly.";

    pub const TOP: &str = "\
Examples:
  mxmon top power -c 5
  mxmon top mem --format json

Ranks the processes in the last sample. Note that cpu_ratio is not clamped: a
process saturating four cores reads 4.0.";

    pub const CHECK_LONG: &str = "\
Evaluate a boolean assertion over the report.

Grammar (paths use the same dot syntax as `get`):
  expr    := or
  or      := and (\"or\" and)*
  and     := cmp (\"and\" cmp)*
  cmp     := \"not\" cmp | \"(\" expr \")\" | operand (op operand)?
  op      := \"<\" | \"<=\" | \">\" | \">=\" | \"==\" | \"!=\"
  operand := path | number | string | true | false | null

Evaluation is tri-state. An operand that resolves to null (a source that is down
or disabled) makes the comparison undecidable rather than false, so
`thermal.cpu_max_c < 90` never passes just because the sensor was unavailable.
Test availability explicitly with `thermal == null`.";

    pub const CHECK: &str = "\
Examples:
  mxmon check 'thermal.throttling == false'
  mxmon check 'power.package_w < 40 and memory.pressure == \"normal\"'
  mxmon check 'not (battery.charging or battery.external_power)'
  mxmon check 'storage != null'         is the source up at all?

Exit: 0 true, 1 false, 2 malformed, 5 undecidable.";

    pub const HEALTH: &str = "\
Folds thermal pressure, SMART, controller throttle, memory pressure, battery
wear, and sleep blockers into one verdict. A domain whose source is null is
reported as unavailable and does not drive the status: you cannot fail a check
you could not run.

Exit: 0 ok, 1 warn or crit.";

    pub const EXPLAIN: &str = "\
Examples:
  mxmon explain thermal
  mxmon explain slow --format json

Deterministic and templated, with no model in the loop, so the same report
always yields the same diagnosis.";

    pub const SCHEMA: &str = "\
Examples:
  mxmon schema                      the full JSON Schema
  mxmon schema --format compact     path:type, one per line
  mxmon schema --format table       path, type, and description

The compact and table views emit exactly the paths `get`, `check`, and `watch`
accept, so the listing doubles as the query vocabulary. Every field carries its
unit in the description.";

    pub const KILL: &str = "\
Examples:
  mxmon kill --dry-run 431           resolve and print the plan, touch nothing
  mxmon kill -s KILL 431 512
  mxmon kill 431 --yes --format json act, then parse what happened

Confirms interactively by default. Off a terminal, --yes is required or the
command refuses and exits 4.";

    pub const SIGNAL: &str = "\
Examples:
  mxmon signal STOP 431              pause a process
  mxmon signal CONT 431              resume it
  mxmon signal 9 431 512 --yes

Same policy as `kill`: confirms interactively by default, and off a terminal
--yes is required or the command refuses and exits 4.";

    pub const RENICE: &str = "\
Examples:
  mxmon renice 10 431 --dry-run
  mxmon renice 5 431 --yes

Lowering priority (a higher number) works for your own processes. Raising it,
or touching another user's process, needs sudo.";

    /// Appended to the verbs that produce a fixed artifact, so the global
    /// output flags are not silently ignored on them.
    pub const NO_OUTPUT_FLAGS: &str =
        "This command emits a fixed artifact; the Output options do not apply to it.";
}

#[derive(Parser, Debug)]
#[command(
    name = "mxmon",
    version,
    about = "mxmon: a sudoless Apple Silicon monitor, as a TUI and as a headless tool",
    long_about = "mxmon reads live Apple Silicon telemetry (power, frequency, temperature, GPU, \
memory, network, per-process energy, SMART, thermal pressure) from macOS frameworks with no sudo, \
no kexts, and no daemons.\n\n\
Bare `mxmon` opens the interactive TUI. Every subcommand is headless and prints a versioned, \
self-describing contract that scripts and AI agents can rely on.",
    after_help = help::ROOT,
    after_long_help = help::ROOT
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub legacy: LegacyFlags,

    #[command(flatten)]
    pub global: GlobalArgs,
}

/// Flags that shape the TUI (bare invocation). Kept for backward compatibility.
/// Combined with a subcommand they are a usage error rather than a silent
/// no-op, so `mxmon --interval 100 watch` cannot look like it worked.
#[derive(Args, Debug)]
#[command(next_help_heading = "TUI options (bare `mxmon` only)")]
pub struct LegacyFlags {
    /// Print one JSON snapshot of every metric and exit (alias for `snapshot`).
    #[arg(long)]
    pub json: bool,

    /// Fast-tier sampling interval in milliseconds (100-2000); overrides config.
    #[arg(long, value_name = "MS")]
    pub interval: Option<u64>,

    /// Theme name (midnight, neon, gruvbox, and more); overrides config.
    #[arg(long, value_name = "NAME")]
    pub theme: Option<String>,

    /// Sub-cell glyph set for graphs; overrides config.
    #[arg(long, value_enum)]
    pub glyphs: Option<crate::config::Glyphs>,
}

/// Cross-cutting output controls, valid before or after any subcommand.
#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Output options")]
pub struct GlobalArgs {
    /// Output shape. `auto` prints a human summary on a terminal and machine
    /// output when piped; `compact` is flat `path=value` lines.
    #[arg(long, value_enum, default_value = "auto", global = true)]
    pub format: Format,

    /// Deadline for the settle a read verb waits on, e.g. `8s`, `500ms`, `2m`
    /// (read verbs only; the control verbs do not sample).
    #[arg(long, value_name = "DUR", value_parser = parse_duration, global = true)]
    pub timeout: Option<Duration>,

    /// Never emit ANSI color, even on a terminal (also honors $NO_COLOR).
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Drop headers, badges, and prose; print only data and errors.
    #[arg(long, short, global = true)]
    pub quiet: bool,
}

/// Output shape. `Auto` resolves to a human summary on a tty and machine output
/// when piped; the rest force a specific shape.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[value(rename_all = "lowercase")]
pub enum Format {
    /// Human summary on a terminal, machine output when piped.
    #[default]
    Auto,
    /// Pretty-printed JSON.
    Json,
    /// One JSON object per line.
    Ndjson,
    /// Flat `dotted.path=value` lines, one leaf per line.
    Compact,
    /// Aligned columns for a terminal.
    Table,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print one settled snapshot of every metric.
    #[command(after_help = help::SNAPSHOT, after_long_help = help::SNAPSHOT)]
    Snapshot(SnapshotArgs),

    /// Extract values by dot-path, e.g. `power.package_w`.
    #[command(after_help = help::GET, after_long_help = help::GET)]
    Get(GetArgs),

    /// Watch metrics as a bounded stream, then exit.
    #[command(after_help = help::WATCH, after_long_help = help::WATCH)]
    Watch(WatchArgs),

    /// Rank the top consumers of a resource.
    #[command(after_help = help::TOP, after_long_help = help::TOP)]
    Top(TopArgs),

    /// Evaluate a boolean assertion over the report; exit 0 true, 1 false.
    #[command(
        long_about = help::CHECK_LONG,
        after_help = help::CHECK,
        after_long_help = help::CHECK
    )]
    Check(CheckArgs),

    /// Print a composite health verdict; exit 1 when degraded.
    #[command(after_help = help::HEALTH, after_long_help = help::HEALTH)]
    Health,

    /// Explain a topic (thermal, power, slow, battery, network, disk).
    #[command(after_help = help::EXPLAIN, after_long_help = help::EXPLAIN)]
    Explain(ExplainArgs),

    /// Print the report contract: JSON Schema, or a flat path listing.
    #[command(after_help = help::SCHEMA, after_long_help = help::SCHEMA)]
    Schema,

    /// Print a shell completion script (bash, zsh, fish, and more).
    #[command(after_help = help::NO_OUTPUT_FLAGS, after_long_help = help::NO_OUTPUT_FLAGS)]
    Completions {
        /// The shell to generate for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Print a roff man page covering mxmon and every subcommand.
    #[command(after_help = help::NO_OUTPUT_FLAGS, after_long_help = help::NO_OUTPUT_FLAGS)]
    Man,

    /// Send a signal to processes (default SIGTERM).
    #[command(after_help = help::KILL, after_long_help = help::KILL)]
    Kill(KillArgs),

    /// Send a named or numbered signal to processes.
    #[command(after_help = help::SIGNAL, after_long_help = help::SIGNAL)]
    Signal(SignalArgs),

    /// Change the scheduling niceness of processes.
    #[command(after_help = help::RENICE, after_long_help = help::RENICE)]
    Renice(ReniceArgs),

    /// Developer dumps of raw sources (hidden).
    #[command(hide = true, subcommand)]
    Debug(DebugCmd),
}

#[derive(Args, Debug)]
pub struct SnapshotArgs {
    /// Restrict to these top-level groups, comma-separated (e.g. `power,thermal`).
    #[arg(long, value_name = "GROUPS", value_delimiter = ',')]
    pub only: Vec<String>,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    /// One or more dot-paths, e.g. `power.package_w processes.top[0].pid`.
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
    #[arg(long, value_name = "DUR", value_parser = parse_duration)]
    pub interval: Option<Duration>,

    /// Stop after this long, e.g. `10s`.
    #[arg(long, value_name = "DUR", value_parser = parse_duration)]
    pub r#for: Option<Duration>,

    /// Stop after this many frames.
    #[arg(long, value_name = "N")]
    pub count: Option<u64>,
}

#[derive(Args, Debug)]
pub struct TopArgs {
    /// What to rank by.
    #[arg(value_enum, default_value = "cpu")]
    pub by: TopBy,

    /// How many processes to list.
    #[arg(long, short, value_name = "N", default_value = "10")]
    pub count: usize,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum TopBy {
    /// Busiest by CPU share (not clamped; 4.0 means four saturated cores).
    Cpu,
    /// Highest estimated per-process power draw.
    Power,
    /// Largest resident memory footprint.
    Mem,
    /// Highest combined disk read plus write throughput.
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
    /// Heat, thermal pressure, and what is driving it.
    Thermal,
    /// Where the package watts are going.
    Power,
    /// Why the machine feels slow right now.
    Slow,
    /// Charge, wear, and what is draining it.
    Battery,
    /// Link state, throughput, and reachability.
    Network,
    /// Throughput, capacity, and drive health.
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

    /// Resolve and print the plan without signaling anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Proceed without the confirmation prompt (required when not a terminal).
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

    /// Resolve and print the plan without renicing anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Proceed without the confirmation prompt (required when not a terminal).
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
        Some(0) | None => {
            return Err(format!(
                "expected a number and unit in {s:?} (e.g. 8s, 500ms)"
            ));
        }
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
        return (1..=31)
            .contains(&n)
            .then_some(n)
            .ok_or_else(|| format!("signal number out of range: {n}"));
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

/// Render every help page into one document: the root, then each subcommand in
/// declaration order. This is what the golden pins, and what `--help` drift
/// shows up as.
#[cfg(test)]
pub fn help_document() -> String {
    use clap::CommandFactory as _;
    use std::fmt::Write as _;

    fn page(cmd: &mut clap::Command, path: &str, out: &mut String) {
        let _ = writeln!(out, "===== {path} =====");
        let _ = writeln!(out, "{}", cmd.render_long_help());
        // Subcommand pages come from clones, so rendering never mutates the
        // tree the next page is taken from.
        let subs: Vec<clap::Command> = cmd.get_subcommands().cloned().collect();
        for mut sub in subs {
            if sub.get_name() == "help" {
                continue;
            }
            let child = format!("{path} {}", sub.get_name());
            page(&mut sub, &child, out);
        }
    }

    let mut out = String::new();
    page(&mut Cli::command(), "mxmon", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_is_wellformed() {
        Cli::command().debug_assert();
    }

    /// The defect this guards against actually shipped: `signal --dry-run`,
    /// `signal -y`, `renice --dry-run`, `renice -y`, and the `completions`
    /// positional all rendered as bare flags with no description.
    #[test]
    fn every_arg_is_documented() {
        fn walk(cmd: &clap::Command, path: &str, gaps: &mut Vec<String>) {
            if cmd.get_about().is_none() && path != "mxmon" {
                gaps.push(format!("{path}: no about"));
            }
            for arg in cmd.get_arguments() {
                if arg.get_help().is_none() && arg.get_long_help().is_none() {
                    gaps.push(format!("{path} <{}>", arg.get_id()));
                }
            }
            for sub in cmd.get_subcommands() {
                if sub.get_name() == "help" {
                    continue;
                }
                walk(sub, &format!("{path} {}", sub.get_name()), gaps);
            }
        }
        let mut gaps = Vec::new();
        walk(&Cli::command(), "mxmon", &mut gaps);
        assert!(gaps.is_empty(), "undocumented CLI surface: {gaps:#?}");
    }

    /// The root help is where an agent learns the contract exists. These are
    /// the facts it cannot discover any other way.
    #[test]
    fn root_help_states_the_load_bearing_facts() {
        let doc = Cli::command().render_long_help().to_string();
        for fact in [
            "Bare `mxmon` opens the TUI",
            "Exit codes:",
            "AGENTS.md",
            "mxmon schema --format compact",
            "TUI options",
            "Output options",
        ] {
            assert!(doc.contains(fact), "root help never mentions {fact:?}");
        }
    }

    /// Golden help document. Re-bless with `MXMON_BLESS=1 cargo test help`.
    #[test]
    fn help_matches_the_golden() {
        let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/help.golden.txt");
        let rendered = help_document();
        if std::env::var_os("MXMON_BLESS").is_some() {
            std::fs::write(golden_path, &rendered).expect("write golden");
            return;
        }
        let golden = std::fs::read_to_string(golden_path).unwrap_or_default();
        assert_eq!(
            golden, rendered,
            "the help surface changed; review the diff, then re-bless with \
             MXMON_BLESS=1 cargo test help_matches_the_golden"
        );
    }

    #[test]
    fn bare_legacy_and_subcommands_parse() {
        assert!(Cli::try_parse_from(["mxmon"]).unwrap().command.is_none());
        assert!(
            Cli::try_parse_from(["mxmon", "--json"])
                .unwrap()
                .legacy
                .json
        );
        assert_eq!(
            Cli::try_parse_from(["mxmon", "--interval", "500"])
                .unwrap()
                .legacy
                .interval,
            Some(500)
        );
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "snapshot"]).unwrap().command,
            Some(Command::Snapshot(_))
        ));
        // The global `--format` must be accepted on either side of the verb.
        assert!(Cli::try_parse_from(["mxmon", "snapshot", "--format", "json"]).is_ok());
        assert!(Cli::try_parse_from(["mxmon", "--format", "json", "snapshot"]).is_ok());
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "snapshot", "--only", "power,thermal"])
                .unwrap()
                .command,
            Some(Command::Snapshot(_))
        ));
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "get", "power.package_w"])
                .unwrap()
                .command,
            Some(Command::Get(_))
        ));
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "check", "thermal.throttling", "==", "false"])
                .unwrap()
                .command,
            Some(Command::Check(_))
        ));
        assert!(matches!(
            Cli::try_parse_from(["mxmon", "debug", "smc"])
                .unwrap()
                .command,
            Some(Command::Debug(_))
        ));
        // Clap still validates enums and required positionals.
        assert!(Cli::try_parse_from(["mxmon", "--glyphs", "sixel"]).is_err());
        assert!(Cli::try_parse_from(["mxmon", "get"]).is_err());
        assert!(Cli::try_parse_from(["mxmon", "kill"]).is_err());
    }

    #[test]
    fn every_format_variant_is_reachable_by_name() {
        for (name, want) in [
            ("auto", Format::Auto),
            ("json", Format::Json),
            ("ndjson", Format::Ndjson),
            ("compact", Format::Compact),
            ("table", Format::Table),
        ] {
            let cli = Cli::try_parse_from(["mxmon", "snapshot", "--format", name])
                .unwrap_or_else(|e| panic!("--format {name} rejected: {e}"));
            assert_eq!(cli.global.format, want);
        }
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
