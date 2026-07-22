//! The settings schema: what is configurable, how it reads, and how it
//! changes — one table, shared by the settings card and the dispatcher.
//!
//! Before this module a setting existed three times over: a row in the
//! overlay's array, an arm in `event::settings_step` keyed by that row's
//! *index*, and a hand-counted `SETTINGS_ROWS` const tying them together.
//! Adding one meant editing all three in the right order, and a mismatch
//! silently stepped the wrong value. Here [`ITEMS`] is the only list; the
//! renderer iterates it and the dispatcher acts on an [`Id`], so a row can
//! never mean two different things.
//!
//! Every mutator persists immediately (`config.save()`), matching the live
//! behavior the app has always had: nothing here is "apply on close".

use ratatui::style::Color;

use crate::app::App;
use crate::collect::sampler::{Control, FAST_MS_MAX, FAST_MS_MIN};
use crate::config::{Config, Glyphs};
use crate::ui::theme::{self, INKS, THEMES};

/// A page of the settings card. Order is the tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Graphs,
    Layout,
    Panels,
    Sampling,
    Network,
    Keys,
    About,
}

pub const SECTIONS: [Section; 8] = [
    Section::Appearance,
    Section::Graphs,
    Section::Layout,
    Section::Panels,
    Section::Sampling,
    Section::Network,
    Section::Keys,
    Section::About,
];

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Graphs => "graphs",
            Self::Layout => "layout",
            Self::Panels => "panels",
            Self::Sampling => "sampling",
            Self::Network => "network",
            Self::Keys => "keys",
            Self::About => "about",
        }
    }

    /// Shown under the tab strip — what this page is for.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Appearance => "theme and the chrome colors painted on top of it",
            Self::Graphs => "how the waveforms aggregate and move",
            Self::Layout => "how much room each surface gets",
            Self::Panels => "which cards appear on the dashboard, and where",
            Self::Sampling => "how often the collectors run",
            Self::Network => "the only thing mxmon ever sends",
            Self::Keys => "every command and the keys that fire it",
            Self::About => "this machine, this build, and where its files live",
        }
    }

    /// Index into [`SECTIONS`], clamped — cursors are hostile input.
    pub fn at(index: usize) -> Self {
        SECTIONS[index.min(SECTIONS.len() - 1)]
    }
}

/// One configurable value. Adding a `Config` field means adding an `Id` here;
/// `config_field_coverage` in this module's tests fails until you do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    Theme,
    Frames,
    Labels,
    Glyphs,
    GraphWindow,
    Motion,
    ProcsPanes,
    Schematic,
    Contours,
    Interval,
    Ping,
    PingHost,
    StorageHealth,
    KernelStats,
    ShowCpu,
    ShowGpu,
    ShowMem,
    ShowNet,
    ShowDisk,
    ShowPower,
    ShowTemps,
    ShowBattery,
    Arrangement,
}

/// How a value is presented and edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Two states, drawn as a pill. Its options are `on` / `off`, so clicks
    /// go through the same path as any other choice.
    Toggle,
    /// A closed set, drawn as directly clickable chips — picking "neon" is
    /// one click, not eighteen steps through the cycle.
    Choice,
    /// A number with a unit, stepped by the `‹ ›` arrows.
    Stepper,
    /// Free text, edited inline.
    Text,
    /// A value the card only *reports*: it is changed by direct manipulation
    /// somewhere else in the UI, so the row has no stepper and no chips — just
    /// the reading, and the `↺` chip every row gets once it drifts.
    Readout,
}

pub struct Item {
    pub id: Id,
    pub section: Section,
    pub label: &'static str,
    /// One line under the cursor explaining what the setting does.
    pub help: &'static str,
    pub kind: Kind,
}

pub const ITEMS: [Item; 23] = [
    Item {
        id: Id::Theme,
        section: Section::Appearance,
        label: "theme",
        help: "the whole palette — panels, graphs, gauges, heat map",
        kind: Kind::Choice,
    },
    Item {
        id: Id::Frames,
        section: Section::Appearance,
        label: "frames",
        help: "panel frames · graph baselines · gauge tracks · silkscreen",
        kind: Kind::Choice,
    },
    Item {
        id: Id::Labels,
        section: Section::Appearance,
        label: "labels",
        help: "grey labels, units, hints and axis text",
        kind: Kind::Choice,
    },
    Item {
        id: Id::Glyphs,
        section: Section::Appearance,
        label: "glyphs",
        help: "solid sub-cell graphs · octants need a modern terminal",
        kind: Kind::Choice,
    },
    Item {
        id: Id::GraphWindow,
        section: Section::Graphs,
        label: "graph window",
        help: "ticks folded into each graph dot · peaks kept, head stays live",
        kind: Kind::Choice,
    },
    Item {
        id: Id::Motion,
        section: Section::Graphs,
        label: "motion",
        help: "fluid graphs · ~30 fps while moving, zero frames at rest",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ProcsPanes,
        section: Section::Layout,
        label: "process panes",
        help: "how many table panes wide layouts may split into",
        kind: Kind::Choice,
    },
    Item {
        id: Id::Schematic,
        section: Section::Layout,
        label: "schematic",
        help: "chassis silkscreen beneath the thermal contours",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::Contours,
        section: Section::Layout,
        label: "contours",
        help: "the heat map's isotherm rings · the readings stay either way",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::Interval,
        section: Section::Sampling,
        label: "interval",
        help: "fast-tier period · every other tier is a multiple of it",
        kind: Kind::Stepper,
    },
    Item {
        id: Id::ShowCpu,
        section: Section::Panels,
        label: "cpu",
        help: "per-core cluster meters, frequencies and utilization",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ShowGpu,
        section: Section::Panels,
        label: "gpu",
        help: "GPU utilization, frequency and power",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ShowMem,
        section: Section::Panels,
        label: "memory",
        help: "the Activity Monitor formula, swap and pressure",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ShowNet,
        section: Section::Panels,
        label: "network",
        help: "throughput, interface and the latency strip",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ShowDisk,
        section: Section::Panels,
        label: "disk",
        help: "read/write throughput, IOPS, latency and capacity",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ShowPower,
        section: Section::Panels,
        label: "power",
        help: "package and per-rail watts",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ShowTemps,
        section: Section::Panels,
        label: "temps",
        help: "sensor groups and fan speeds",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::ShowBattery,
        section: Section::Panels,
        label: "battery",
        help: "charge, health and the power-flow diagram",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::Arrangement,
        section: Section::Panels,
        label: "arrangement",
        help: "drag a card onto another to swap them · a arranges by keyboard",
        kind: Kind::Readout,
    },
    Item {
        id: Id::StorageHealth,
        section: Section::Sampling,
        label: "storage health",
        help: "SMART · volume cache · controller throttle, every 10 s",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::KernelStats,
        section: Section::Sampling,
        label: "kernel stats",
        help: "per-device interrupt rates and what is holding the Mac awake",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::Ping,
        section: Section::Network,
        label: "ping probe",
        help: "ICMP connectivity strip · applies at next launch",
        kind: Kind::Toggle,
    },
    Item {
        id: Id::PingHost,
        section: Section::Network,
        label: "ping host",
        help: "IPv4 literal or hostname, resolved once at startup",
        kind: Kind::Text,
    },
];

/// The rows of one section, in declaration order.
pub fn items(section: Section) -> impl Iterator<Item = &'static Item> {
    ITEMS.iter().filter(move |i| i.section == section)
}

/// The two things the ABOUT page can *do* — everything else there is text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AboutAction {
    RescanSensors,
    ResetAll,
}

pub const ABOUT_ACTIONS: [AboutAction; 2] = [AboutAction::RescanSensors, AboutAction::ResetAll];

impl AboutAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::RescanSensors => "rescan SMC sensors",
            Self::ResetAll => "reset every setting",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::RescanSensors => "re-probes at next launch",
            Self::ResetAll => "settings and bindings back to stock",
        }
    }
}

/// How many selectable rows a section has — settings rows, key actions, or
/// about actions. Dispatch and the renderer both clamp against this, so a
/// cursor can never point past the page it is on.
pub fn row_count(section: Section) -> usize {
    match section {
        Section::Keys => crate::keys::ACTIONS.len(),
        Section::About => ABOUT_ACTIONS.len(),
        s => items(s).count(),
    }
}

/// The setting at `row` of a row-based section, if there is one.
pub fn item_at(section: Section, row: usize) -> Option<&'static Item> {
    items(section).nth(row)
}

pub fn item(id: Id) -> &'static Item {
    ITEMS.iter().find(|i| i.id == id).unwrap_or(&ITEMS[0]) // unreachable: every Id is in the table (tested)
}

/// The `config.toml` key an id owns — the link the coverage test walks.
pub fn config_key(id: Id) -> &'static str {
    match id {
        Id::Theme => "theme",
        Id::Frames => "frames",
        Id::Labels => "labels",
        Id::Glyphs => "glyphs",
        Id::GraphWindow => "graph_window",
        Id::Motion => "motion",
        Id::ProcsPanes => "procs_panes",
        Id::Schematic => "schematic",
        Id::Contours => "contours",
        Id::StorageHealth => "storage_health",
        Id::KernelStats => "kernel_stats",
        Id::ShowCpu => "show_cpu",
        Id::ShowGpu => "show_gpu",
        Id::ShowMem => "show_mem",
        Id::ShowNet => "show_net",
        Id::ShowDisk => "show_disk",
        Id::ShowPower => "show_power",
        Id::ShowTemps => "show_temps",
        Id::ShowBattery => "show_battery",
        Id::Arrangement => "arrangement",
        Id::Interval => "interval_ms",
        Id::Ping => "ping",
        Id::PingHost => "ping_host",
    }
}

/// The graph-window stops the card offers. Any 1–8 value from a hand-edited
/// config is honored; stepping snaps outward in the direction of travel.
pub const GRAPH_WINDOW_STOPS: [u16; 4] = [1, 2, 4, 8];

const GLYPH_MODES: [Glyphs; 3] = [Glyphs::Auto, Glyphs::Octant, Glyphs::Braille];

/// Every value a setting can be clicked straight to. Empty for steppers and
/// text fields, which have no closed set.
pub fn options(id: Id) -> Vec<String> {
    match id {
        Id::Theme => THEMES.iter().map(|t| t.name.to_owned()).collect(),
        Id::Frames | Id::Labels => INKS.iter().map(|&s| s.to_owned()).collect(),
        Id::Glyphs => vec!["auto".into(), "octant".into(), "braille".into()],
        Id::GraphWindow => GRAPH_WINDOW_STOPS.iter().map(|k| format!("×{k}")).collect(),
        Id::ProcsPanes => (1..=4).map(|n| n.to_string()).collect(),
        Id::Motion
        | Id::Schematic
        | Id::Contours
        | Id::Ping
        | Id::StorageHealth
        | Id::KernelStats
        | Id::ShowCpu
        | Id::ShowGpu
        | Id::ShowMem
        | Id::ShowNet
        | Id::ShowDisk
        | Id::ShowPower
        | Id::ShowTemps
        | Id::ShowBattery => vec!["on".into(), "off".into()],
        Id::Interval | Id::PingHost | Id::Arrangement => Vec::new(),
    }
}

/// What a setting reads as right now.
pub struct Current {
    /// The value itself — "midnight", "on", "250 ms".
    pub value: String,
    /// A dim clause saying what that means in this configuration.
    pub detail: String,
    /// Position in [`options`], when there is a closed set.
    pub index: Option<usize>,
}

pub fn current(app: &App, id: Id) -> Current {
    let c = &app.config;
    let (value, detail): (String, String) = match id {
        Id::Theme => (
            c.theme.clone(),
            format!("{} themes · t cycles", THEMES.len()),
        ),
        Id::Frames | Id::Labels => {
            let name = if id == Id::Frames {
                &c.frames
            } else {
                &c.labels
            };
            let detail = if name == "theme" {
                format!("from {}", c.theme)
            } else {
                "override · survives theme changes".into()
            };
            (name.clone(), detail)
        }
        Id::Glyphs => match c.glyphs {
            Glyphs::Auto => (
                "auto".into(),
                if crate::ui::glyphs::active(c.glyphs) {
                    "octants here".into()
                } else {
                    "braille here".into()
                },
            ),
            Glyphs::Octant => ("octant".into(), "forced".into()),
            Glyphs::Braille => ("braille".into(), "forced".into()),
        },
        Id::GraphWindow => {
            let k = c.graph_window.max(1);
            let detail = if k == 1 {
                "every tick is a dot".into()
            } else {
                format!("{} per dot", per_dot(k, c.interval_ms))
            };
            (format!("×{k}"), detail)
        }
        Id::Motion => on_off(c.motion, "graphs glide between ticks", "one frame per tick"),
        Id::ProcsPanes => (
            c.procs_panes.to_string(),
            if c.procs_panes == 1 {
                "widgets get the spare width".into()
            } else {
                "side-by-side process slices".into()
            },
        ),
        Id::Schematic => on_off(
            c.schematic,
            "blueprint under the heat map",
            "contours on a bare deck",
        ),
        Id::Contours => on_off(
            c.contours,
            "isotherm rings over the deck",
            "readings on a quiet deck",
        ),
        Id::ShowCpu => on_off(c.show_cpu, "cpu card shown", "cpu card hidden"),
        Id::ShowGpu => on_off(c.show_gpu, "gpu card shown", "gpu card hidden"),
        Id::ShowMem => on_off(c.show_mem, "memory card shown", "memory card hidden"),
        Id::ShowNet => on_off(c.show_net, "network card shown", "network card hidden"),
        Id::ShowDisk => on_off(c.show_disk, "disk card shown", "disk card hidden"),
        Id::ShowPower => on_off(c.show_power, "power card shown", "power card hidden"),
        Id::ShowTemps => on_off(c.show_temps, "temps card shown", "temps card hidden"),
        Id::ShowBattery => on_off(c.show_battery, "battery card shown", "battery card hidden"),
        Id::StorageHealth => on_off(
            c.storage_health,
            "SMART, cache and throttle polled",
            "storage health not sampled",
        ),
        Id::KernelStats => on_off(
            c.kernel_stats,
            "interrupts and wake locks polled",
            "kernel activity not sampled",
        ),
        Id::Interval => (
            format!("{} ms", c.interval_ms),
            // The tiers the fast interval drags along with it — the reason
            // this number matters more than it looks.
            format!(
                "power/temps {} ms · procs {}",
                c.interval_ms * 2,
                secs(c.interval_ms * 8)
            ),
        ),
        Id::Ping => on_off(c.ping, "one ICMP echo per tier tick", "fully passive"),
        Id::PingHost => (
            c.ping_host.clone(),
            if c.ping {
                "live target".into()
            } else {
                "unused while the probe is off".into()
            },
        ),
        Id::Arrangement => {
            let moved = c.arrangement.moved();
            if moved == 0 {
                ("default".into(), "every card in its shipped place".into())
            } else {
                ("custom".into(), format!("{moved} cards moved"))
            }
        }
    };
    let index = index_of(app, id);
    Current {
        value,
        detail,
        index,
    }
}

fn on_off(v: bool, yes: &str, no: &str) -> (String, String) {
    (
        if v { "on" } else { "off" }.into(),
        if v { yes } else { no }.to_owned(),
    )
}

/// Milliseconds as the dot cadence a graph column covers.
fn per_dot(k: u16, interval_ms: u64) -> String {
    let ms = u64::from(k) * interval_ms;
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        secs(ms)
    }
}

fn secs(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

/// Which option is selected, for choices and toggles.
fn index_of(app: &App, id: Id) -> Option<usize> {
    let c = &app.config;
    match id {
        Id::Theme => THEMES.iter().position(|t| t.name == c.theme),
        Id::Frames => INKS.iter().position(|n| *n == c.frames),
        Id::Labels => INKS.iter().position(|n| *n == c.labels),
        Id::Glyphs => GLYPH_MODES.iter().position(|g| *g == c.glyphs),
        Id::GraphWindow => GRAPH_WINDOW_STOPS.iter().position(|k| *k == c.graph_window),
        Id::ProcsPanes => Some(usize::from(c.procs_panes.clamp(1, 4)) - 1),
        Id::Motion => Some(usize::from(!c.motion)),
        Id::Schematic => Some(usize::from(!c.schematic)),
        Id::Contours => Some(usize::from(!c.contours)),
        Id::ShowCpu => Some(usize::from(!c.show_cpu)),
        Id::ShowGpu => Some(usize::from(!c.show_gpu)),
        Id::ShowMem => Some(usize::from(!c.show_mem)),
        Id::ShowNet => Some(usize::from(!c.show_net)),
        Id::ShowDisk => Some(usize::from(!c.show_disk)),
        Id::ShowPower => Some(usize::from(!c.show_power)),
        Id::ShowTemps => Some(usize::from(!c.show_temps)),
        Id::ShowBattery => Some(usize::from(!c.show_battery)),
        Id::StorageHealth => Some(usize::from(!c.storage_health)),
        Id::KernelStats => Some(usize::from(!c.kernel_stats)),
        Id::Ping => Some(usize::from(!c.ping)),
        Id::Interval | Id::PingHost | Id::Arrangement => None,
    }
}

/// Move a setting one place in `dir` (`+1` / `-1`), wrapping. Choices and
/// toggles walk their option list; the interval steps 50 ms; text does
/// nothing (it opens an editor instead).
pub fn step(app: &mut App, control: &Control, id: Id, dir: i64) {
    // The interval is the one stepper; a text field opens an editor instead
    // of stepping, so it has no option list and falls out below.
    if item(id).kind == Kind::Stepper {
        set_interval(app, control, step_ms(app.config.interval_ms, dir));
        return;
    }
    let len = options(id).len();
    if len == 0 {
        return;
    }
    // An off-list value (only reachable by hand-editing) has no index;
    // stepping forward snaps to the stop above it, backward to the one below,
    // so the cycle can never wedge.
    let next = match index_of(app, id) {
        Some(i) => (i as i64 + dir).rem_euclid(len as i64) as usize,
        None => snap(app, id, dir, len),
    };
    set(app, id, next);
}

/// Where an off-list value lands when stepped.
fn snap(app: &App, id: Id, dir: i64, len: usize) -> usize {
    if id == Id::GraphWindow {
        let cur = app.config.graph_window;
        let above = GRAPH_WINDOW_STOPS.iter().position(|s| *s > cur);
        return match (dir > 0, above) {
            (true, Some(i)) => i,
            (false, Some(i)) => i.saturating_sub(1),
            // Above every stop: either direction lands on the last one.
            (_, None) => len - 1,
        };
    }
    if dir > 0 { 0 } else { len - 1 }
}

fn step_ms(current: u64, dir: i64) -> u64 {
    // Right/up means *faster*, which is a shorter interval — the arrows
    // follow the value on screen, not the number behind it.
    let delta = if dir > 0 { -50 } else { 50 };
    (current as i64 + delta).clamp(FAST_MS_MIN as i64, FAST_MS_MAX as i64) as u64
}

/// Set a choice or toggle straight to `index` (a click on its chip). Out of
/// range is ignored rather than clamped: a stale hit rect must not quietly
/// write the nearest legal value.
///
/// No `Control`: nothing with a closed option set touches the sampler — the
/// interval is a stepper and goes through [`set_interval`].
pub fn set(app: &mut App, id: Id, index: usize) {
    if index >= options(id).len() {
        return;
    }
    match id {
        Id::Theme => {
            THEMES[index].name.clone_into(&mut app.config.theme);
            app.toast(format!("theme: {}", app.config.theme), false);
        }
        Id::Frames => INKS[index].clone_into(&mut app.config.frames),
        Id::Labels => INKS[index].clone_into(&mut app.config.labels),
        Id::Glyphs => app.config.glyphs = GLYPH_MODES[index],
        Id::GraphWindow => app.config.graph_window = GRAPH_WINDOW_STOPS[index],
        Id::ProcsPanes => app.config.procs_panes = index as u16 + 1,
        Id::Motion => app.config.motion = index == 0,
        Id::Schematic => app.config.schematic = index == 0,
        Id::Contours => app.config.contours = index == 0,
        Id::ShowCpu => app.config.show_cpu = index == 0,
        Id::ShowGpu => app.config.show_gpu = index == 0,
        Id::ShowMem => app.config.show_mem = index == 0,
        Id::ShowNet => app.config.show_net = index == 0,
        Id::ShowDisk => app.config.show_disk = index == 0,
        Id::ShowPower => app.config.show_power = index == 0,
        Id::ShowTemps => app.config.show_temps = index == 0,
        Id::ShowBattery => app.config.show_battery = index == 0,
        Id::StorageHealth => app.config.storage_health = index == 0,
        Id::KernelStats => app.config.kernel_stats = index == 0,
        Id::Ping => {
            app.config.ping = index == 0;
            app.toast("ping probe: applies at next launch", false);
        }
        // Not choices — reached only through `set_interval` / `set_text`.
        // Not choices — the interval steps, the host opens an editor, and
        // the arrangement is changed by dragging cards, not from this row.
        Id::Interval | Id::PingHost | Id::Arrangement => return,
    }
    app.config.save();
}

/// Apply a fast-tier interval: clamped, live-tuned on the sampler, saved.
/// The `+`/`-` keys, the footer wheel and the card's arrows all land here.
pub fn set_interval(app: &mut App, control: &Control, ms: u64) {
    let next = ms.clamp(FAST_MS_MIN, FAST_MS_MAX);
    app.config.interval_ms = next;
    control
        .fast_ms
        .store(next, std::sync::atomic::Ordering::Relaxed);
    app.config.save();
    app.toast(format!("tick {next}ms"), false);
}

/// Commit an edited text field. Blank input is rejected (it would leave the
/// probe pointed at nothing) — the old value stands.
pub fn set_text(app: &mut App, id: Id, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    match id {
        Id::PingHost => {
            text.clone_into(&mut app.config.ping_host);
            app.toast("ping host: applies at next launch", false);
        }
        _ => return,
    }
    app.config.save();
}

/// Whether a setting still holds its shipped value.
pub fn is_default(app: &App, id: Id) -> bool {
    let d = Config::default();
    let c = &app.config;
    match id {
        Id::Theme => c.theme == d.theme,
        Id::Frames => c.frames == d.frames,
        Id::Labels => c.labels == d.labels,
        Id::Glyphs => c.glyphs == d.glyphs,
        Id::GraphWindow => c.graph_window == d.graph_window,
        Id::Motion => c.motion == d.motion,
        Id::ProcsPanes => c.procs_panes == d.procs_panes,
        Id::Schematic => c.schematic == d.schematic,
        Id::Contours => c.contours == d.contours,
        Id::ShowCpu => c.show_cpu == d.show_cpu,
        Id::ShowGpu => c.show_gpu == d.show_gpu,
        Id::ShowMem => c.show_mem == d.show_mem,
        Id::ShowNet => c.show_net == d.show_net,
        Id::ShowDisk => c.show_disk == d.show_disk,
        Id::ShowPower => c.show_power == d.show_power,
        Id::ShowTemps => c.show_temps == d.show_temps,
        Id::ShowBattery => c.show_battery == d.show_battery,
        Id::StorageHealth => c.storage_health == d.storage_health,
        Id::KernelStats => c.kernel_stats == d.kernel_stats,
        Id::Interval => c.interval_ms == d.interval_ms,
        Id::Ping => c.ping == d.ping,
        Id::PingHost => c.ping_host == d.ping_host,
        Id::Arrangement => c.arrangement.is_default(),
    }
}

/// Put one setting back to its shipped value.
pub fn reset(app: &mut App, control: &Control, id: Id) {
    let d = Config::default();
    match id {
        Id::Theme => app.config.theme = d.theme,
        Id::Frames => app.config.frames = d.frames,
        Id::Labels => app.config.labels = d.labels,
        Id::Glyphs => app.config.glyphs = d.glyphs,
        Id::GraphWindow => app.config.graph_window = d.graph_window,
        Id::Motion => app.config.motion = d.motion,
        Id::ProcsPanes => app.config.procs_panes = d.procs_panes,
        Id::Schematic => app.config.schematic = d.schematic,
        Id::Contours => app.config.contours = d.contours,
        Id::ShowCpu => app.config.show_cpu = d.show_cpu,
        Id::ShowGpu => app.config.show_gpu = d.show_gpu,
        Id::ShowMem => app.config.show_mem = d.show_mem,
        Id::ShowNet => app.config.show_net = d.show_net,
        Id::ShowDisk => app.config.show_disk = d.show_disk,
        Id::ShowPower => app.config.show_power = d.show_power,
        Id::ShowTemps => app.config.show_temps = d.show_temps,
        Id::ShowBattery => app.config.show_battery = d.show_battery,
        Id::StorageHealth => app.config.storage_health = d.storage_health,
        Id::KernelStats => app.config.kernel_stats = d.kernel_stats,
        Id::Interval => {
            set_interval(app, control, d.interval_ms);
            return; // set_interval already saved and toasted
        }
        Id::Ping => app.config.ping = d.ping,
        Id::PingHost => app.config.ping_host = d.ping_host,
        Id::Arrangement => app.config.arrangement = d.arrangement,
    }
    app.config.save();
}

/// Whether the whole configuration — every row and every binding — is still
/// the shipped one. The ABOUT page says so rather than offering a reset that
/// would do nothing.
pub fn all_default(app: &App) -> bool {
    ITEMS.iter().all(|i| is_default(app, i.id)) && app.config.keys.all_default()
}

/// Put *everything* back, keymap included, and re-tune the live sampler.
pub fn reset_all(app: &mut App, control: &Control) {
    app.config = Config::default();
    control
        .fast_ms
        .store(app.config.interval_ms, std::sync::atomic::Ordering::Relaxed);
    app.config.save();
    app.toast("settings reset to defaults", false);
}

/// The color an option chip should be painted in, when the option *is* a
/// color: each theme in its own accent, each ink as it would actually
/// resolve against the active theme. Everything else has no swatch.
pub fn option_swatch(app: &App, id: Id, index: usize) -> Option<Color> {
    let base = theme::by_name(&app.config.theme);
    match id {
        Id::Theme => THEMES.get(index).map(|t| t.accent),
        Id::Frames => INKS.get(index).map(|n| theme::ink(n, &base, base.border)),
        Id::Labels => INKS.get(index).map(|n| theme::ink(n, &base, base.dim)),
        _ => None,
    }
}

/// The swatch for the *current* value of a setting.
pub fn current_swatch(app: &App, id: Id) -> Option<Color> {
    option_swatch(app, id, index_of(app, id)?)
}

/// Drop the SMC discovery cache so the next launch re-probes from scratch.
/// The one piece of state the app keeps that a user may need to invalidate
/// (a macOS update can change which keys read).
pub fn clear_sensor_cache() -> bool {
    crate::config::dir()
        .map(|d| d.join("sensors.toml"))
        .is_some_and(|p| std::fs::remove_file(p).is_ok())
}

#[cfg(test)]
mod tests {
    use super::{
        Current, ITEMS, Id, Kind, SECTIONS, Section, config_key, current, is_default, item, items,
        options, reset, reset_all, set, set_interval, set_text, step,
    };
    use crate::collect::sampler::{Control, FAST_MS_MAX, FAST_MS_MIN};
    use crate::config::{self, Config, Glyphs};
    use crate::testutil as tu;

    /// Fixture app + isolated config dir, so the save-on-change path in every
    /// mutator can never reach the real `~/.config/mxmon`.
    struct H {
        app: crate::app::App,
        control: std::sync::Arc<Control>,
        _tmp: tempfile::TempDir,
        _guard: config::TestDirGuard,
    }

    fn h() -> H {
        let tmp = tempfile::tempdir().expect("tempdir");
        let guard = config::test_dir(tmp.path().to_path_buf());
        H {
            app: tu::app(),
            control: Control::new(),
            _tmp: tmp,
            _guard: guard,
        }
    }

    /// **The guard that keeps "every setting is in one place" true.** Every
    /// `Config` field must be claimed by an `Id` (or explicitly owned by a
    /// section that isn't row-based). Add a field, and this fails until the
    /// card can edit it.
    #[test]
    fn every_config_field_is_reachable_from_the_card() {
        let toml = toml::to_string(&Config::default()).expect("serialize defaults");
        let table: toml::Table = toml.parse().expect("parse defaults");
        // `keys` is the KEYS section's whole subject, not a row.
        let owned_elsewhere = ["keys"];
        for key in table.keys() {
            let claimed = ITEMS.iter().any(|i| config_key(i.id) == key.as_str())
                || owned_elsewhere.contains(&key.as_str());
            assert!(
                claimed,
                "config field `{key}` has no settings row — add an Id for it"
            );
        }
        // …and no row points at a field that no longer exists.
        for i in &ITEMS {
            assert!(
                table.contains_key(config_key(i.id)),
                "settings row `{}` points at missing config field `{}`",
                i.label,
                config_key(i.id)
            );
        }
    }

    #[test]
    fn every_item_reads_writes_and_resets() {
        // A setting is four arms — `current`, `set`, `is_default`, `reset` —
        // and a missing one is invisible until someone touches that row. Walk
        // the whole table so adding an `Id` without wiring it up fails here.
        // A pristine config, not the snapshot fixture — that one deliberately
        // pins glyphs and motion so golden frames stay stable.
        let mut app = crate::app::App::new(crate::testutil::soc(), Config::default());
        let control = Control::new();
        for item in &ITEMS {
            let id = item.id;
            assert!(is_default(&app, id), "{id:?} starts at its default");
            let before = current(&app, id);
            let options = options(id);
            if options.len() > 1 {
                // Move off the default, then back.
                let now = super::index_of(&app, id).unwrap_or(0);
                let other = usize::from(now == 0);
                set(&mut app, id, other);
                assert!(
                    !is_default(&app, id) || options.len() == 1,
                    "{id:?} changed away from its default"
                );
            }
            reset(&mut app, &control, id);
            assert!(is_default(&app, id), "{id:?} resets");
            // Stepping in both directions must stay in range whatever the kind.
            step(&mut app, &control, id, 1);
            step(&mut app, &control, id, -1);
            let _ = current(&app, id);
            let _ = before;
        }
    }

    #[test]
    fn every_item_is_well_formed_and_sections_cover_the_table() {
        for i in &ITEMS {
            assert!(!i.label.is_empty() && !i.help.is_empty());
            assert_eq!(item(i.id).label, i.label, "item() must find every id");
            match i.kind {
                Kind::Toggle => assert_eq!(options(i.id), ["on", "off"]),
                Kind::Choice => assert!(options(i.id).len() >= 2, "{} has no choices", i.label),
                Kind::Stepper | Kind::Text | Kind::Readout => {
                    assert!(options(i.id).is_empty());
                }
            }
        }
        // Every row belongs to a section that exists, and the row-bearing
        // sections are non-empty.
        let listed: usize = SECTIONS.iter().map(|s| items(*s).count()).sum();
        assert_eq!(listed, ITEMS.len(), "a row belongs to no listed section");
        for s in [
            Section::Appearance,
            Section::Graphs,
            Section::Layout,
            Section::Sampling,
            Section::Network,
        ] {
            assert!(items(s).count() > 0, "{} is empty", s.title());
        }
        assert_eq!(items(Section::Keys).count(), 0, "KEYS is not row-based");
        assert_eq!(items(Section::About).count(), 0, "ABOUT is not row-based");
        assert_eq!(
            Section::at(usize::MAX),
            Section::About,
            "clamps, not panics"
        );
    }

    /// Every choice/toggle steps forward through its whole cycle and lands
    /// back where it started — the property the old index-keyed match had to
    /// be checked by hand, row by row.
    #[test]
    fn stepping_any_choice_cycles_and_returns() {
        let mut h = h();
        for i in &ITEMS {
            let len = options(i.id).len();
            if len == 0 {
                continue;
            }
            let before = current(&h.app, i.id).index;
            for _ in 0..len {
                step(&mut h.app, &h.control, i.id, 1);
            }
            assert_eq!(
                current(&h.app, i.id).index,
                before,
                "{} did not return after a full cycle",
                i.label
            );
            // …and backwards, from the same start.
            for _ in 0..len {
                step(&mut h.app, &h.control, i.id, -1);
            }
            assert_eq!(current(&h.app, i.id).index, before, "{} reverse", i.label);
        }
    }

    #[test]
    fn set_writes_the_value_a_chip_click_names() {
        let mut h = h();
        set(&mut h.app, Id::Theme, 1);
        assert_eq!(h.app.config.theme, crate::ui::theme::THEMES[1].name);
        set(&mut h.app, Id::Glyphs, 2);
        assert_eq!(h.app.config.glyphs, Glyphs::Braille);
        set(&mut h.app, Id::ProcsPanes, 3);
        assert_eq!(h.app.config.procs_panes, 4, "index 3 is pane count 4");
        set(&mut h.app, Id::Motion, 1);
        assert!(!h.app.config.motion, "index 1 of a toggle is off");
        set(&mut h.app, Id::Motion, 0);
        assert!(h.app.config.motion);
        // Out of range does nothing at all — a stale hit rect must not write
        // the nearest legal value.
        let before = h.app.config.theme.clone();
        set(&mut h.app, Id::Theme, usize::MAX);
        assert_eq!(h.app.config.theme, before);
        // Steppers and text fields ignore `set` outright.
        set(&mut h.app, Id::Interval, 0);
        set(&mut h.app, Id::PingHost, 0);
        assert_eq!(h.app.config.interval_ms, Config::default().interval_ms);
    }

    #[test]
    fn interval_steps_clamp_at_both_ends() {
        let mut h = h();
        // Right is faster: the interval shrinks.
        let start = h.app.config.interval_ms;
        step(&mut h.app, &h.control, Id::Interval, 1);
        assert_eq!(h.app.config.interval_ms, start - 50);
        for _ in 0..100 {
            step(&mut h.app, &h.control, Id::Interval, 1);
        }
        assert_eq!(h.app.config.interval_ms, FAST_MS_MIN);
        for _ in 0..100 {
            step(&mut h.app, &h.control, Id::Interval, -1);
        }
        assert_eq!(h.app.config.interval_ms, FAST_MS_MAX);
        // The live sampler knob follows every change.
        assert_eq!(
            h.control.fast_ms.load(std::sync::atomic::Ordering::Relaxed),
            FAST_MS_MAX
        );
        set_interval(&mut h.app, &h.control, 0);
        assert_eq!(h.app.config.interval_ms, FAST_MS_MIN, "clamped up");
    }

    #[test]
    fn graph_window_snaps_outward_from_a_hand_edited_value() {
        let mut h = h();
        // ×7 is off the ×1/2/4/8 stops but legal in a hand-edited config.
        h.app.config.graph_window = 7;
        step(&mut h.app, &h.control, Id::GraphWindow, 1);
        assert_eq!(h.app.config.graph_window, 8, "forward snaps up");
        h.app.config.graph_window = 7;
        step(&mut h.app, &h.control, Id::GraphWindow, -1);
        assert_eq!(h.app.config.graph_window, 4, "backward snaps down");
        // Above every stop, either direction lands on the last one.
        h.app.config.graph_window = 99;
        step(&mut h.app, &h.control, Id::GraphWindow, 1);
        assert_eq!(h.app.config.graph_window, 8);
    }

    #[test]
    fn text_fields_commit_and_refuse_blanks() {
        let mut h = h();
        set_text(&mut h.app, Id::PingHost, "  9.9.9.9  ");
        assert_eq!(h.app.config.ping_host, "9.9.9.9", "trimmed");
        set_text(&mut h.app, Id::PingHost, "   ");
        assert_eq!(h.app.config.ping_host, "9.9.9.9", "blank leaves it alone");
    }

    #[test]
    fn reset_restores_one_row_and_reset_all_restores_everything() {
        let mut h = h();
        // The fixture deliberately pins a couple of settings for determinism,
        // so start from a known-clean slate.
        reset_all(&mut h.app, &h.control);
        for i in &ITEMS {
            assert!(is_default(&h.app, i.id), "{} survived reset_all", i.label);
        }
        set(&mut h.app, Id::Theme, 4);
        set_text(&mut h.app, Id::PingHost, "9.9.9.9");
        set_interval(&mut h.app, &h.control, 1000);
        assert!(!is_default(&h.app, Id::Theme));
        reset(&mut h.app, &h.control, Id::Theme);
        assert!(is_default(&h.app, Id::Theme));
        assert!(!is_default(&h.app, Id::PingHost), "only that row moved");

        reset_all(&mut h.app, &h.control);
        for i in &ITEMS {
            assert!(is_default(&h.app, i.id), "{} survived reset_all", i.label);
        }
        assert!(h.app.config.keys.all_default(), "the keymap resets too");
        assert_eq!(
            h.control.fast_ms.load(std::sync::atomic::Ordering::Relaxed),
            Config::default().interval_ms,
            "the live sampler follows the reset"
        );
    }

    #[test]
    fn every_row_reads_back_with_a_value_and_an_explanation() {
        let mut h = h();
        // Off-stop and off-list values are legal (hand-edited config): the
        // card must still render them rather than index past the list.
        h.app.config.graph_window = 7;
        "#ff8800".clone_into(&mut h.app.config.frames);
        "not-a-theme".clone_into(&mut h.app.config.theme);
        for i in &ITEMS {
            let Current {
                value,
                detail,
                index,
            } = current(&h.app, i.id);
            assert!(!value.is_empty(), "{} has no value", i.label);
            assert!(!detail.is_empty(), "{} has no detail", i.label);
            if let Some(idx) = index {
                assert!(idx < options(i.id).len(), "{} index out of range", i.label);
            }
        }
        // The off-list values specifically report "no selection" rather than
        // pointing at some other option.
        assert_eq!(current(&h.app, Id::Theme).index, None);
        assert_eq!(current(&h.app, Id::Frames).index, None);
        assert_eq!(current(&h.app, Id::GraphWindow).index, None);
    }
}
