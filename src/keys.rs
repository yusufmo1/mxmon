//! Key bindings: the one table every global command is dispatched from,
//! displayed from, and remapped through.
//!
//! Before this module the bindings lived as literals inside
//! `event::handle_key`, were described again in the help modal, and a third
//! time as the footer's key chips — three places to keep in step and nothing
//! the user could change. Here a [`Chord`] (a key plus its modifiers) resolves
//! to an [`Action`], the [`Keymap`] owns that relation, and everything else —
//! dispatch, the settings card's KEYS section, the footer chips — reads it.
//!
//! **The keymap governs global commands only.** Structural keys keep their
//! meaning everywhere and are never remappable: `esc` (close / cancel / clear),
//! `enter`, the arrows, `tab` inside the settings card, `backspace`, and any
//! character typed into the filter or a text field. `ctrl+c` always quits.
//! Without that floor a bad remap could lock the user out of their own config.

use std::collections::BTreeMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::de::{Deserializer, Error as _};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

/// One key press: a code plus the modifiers that must accompany it.
///
/// Comparison is exact, so `ctrl+r` never fires on a bare `r`. Construction
/// from a real event goes through [`Chord::from_event`], which normalizes the
/// one case terminals disagree on (see there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

/// Modifiers mxmon can bind. `SUPER`/`HYPER`/`META` are dropped: terminals on
/// macOS almost never deliver them, so a binding using one would look valid
/// and never fire.
const BINDABLE: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SHIFT);

impl Chord {
    /// The chord a key event should be dispatched as.
    ///
    /// A shifted character already carries its case (`G`), and terminals
    /// disagree about whether they also set `SHIFT` — keeping it would make
    /// `G` fire on one terminal and not another. It is dropped for characters
    /// and kept for named keys, where it is the only signal (`shift+f5`).
    pub fn from_event(key: KeyEvent) -> Self {
        let mut mods = key.modifiers & BINDABLE;
        if matches!(key.code, KeyCode::Char(_)) {
            mods.remove(KeyModifiers::SHIFT);
        }
        Self {
            code: key.code,
            mods,
        }
    }

    /// The canonical config spelling — `"q"`, `"ctrl+r"`, `"f10"`, `"space"`.
    /// Round-trips through [`Chord::parse`]; that is what makes `[keys]`
    /// hand-editable.
    pub fn name(self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("shift+");
        }
        s.push_str(&key_name(self.code));
        s
    }

    /// The pretty form for the UI: arrows and symbols where a glyph reads
    /// faster than a word. Display only — [`Chord::name`] is what persists.
    pub fn label(self) -> String {
        let key = match self.code {
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Enter => "⏎".into(),
            KeyCode::Tab => "⇥".into(),
            KeyCode::BackTab => "⇤".into(),
            KeyCode::Backspace => "⌫".into(),
            KeyCode::Char(' ') => "␣".into(),
            KeyCode::F(n) => format!("F{n}"),
            _ => key_name(self.code),
        };
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("shift+");
        }
        s.push_str(&key);
        s
    }

    /// Parse a config spelling. Total: any string at all either yields a
    /// chord or `None` — a hand-edited `[keys]` table can never panic or
    /// abort startup, it just loses the entries it got wrong.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        // `+` is both the separator and a bindable key ("faster" is bound to
        // it), so the key token is taken from the right: a doubled tail is
        // separator-then-plus ("ctrl++"), a bare "+" is the key alone, and a
        // *single* trailing `+` is a dangling modifier — a typo, not a
        // silent ctrl+plus.
        let (mods_str, key_str) = if s == "+" {
            ("", "+")
        } else if let Some(rest) = s.strip_suffix("++") {
            (rest, "+")
        } else {
            match s.rfind('+') {
                Some(i) => (&s[..i], &s[i + 1..]),
                None => ("", s),
            }
        };
        let mut mods = KeyModifiers::NONE;
        // Empty tokens ("++q", "ctrl+") fall through to the catch-all and
        // reject, so a malformed chord never half-parses.
        if !mods_str.is_empty() {
            for token in mods_str.split('+') {
                match token.trim().to_ascii_lowercase().as_str() {
                    "ctrl" | "control" => mods.insert(KeyModifiers::CONTROL),
                    "alt" | "opt" | "option" => mods.insert(KeyModifiers::ALT),
                    "shift" => mods.insert(KeyModifiers::SHIFT),
                    _ => return None,
                }
            }
        }
        let code = key_code(key_str)?;
        if matches!(code, KeyCode::Char(_)) {
            mods.remove(KeyModifiers::SHIFT);
        }
        Some(Self { code, mods })
    }

    /// Chords that keep their built-in meaning no matter what: rebinding them
    /// would cost the user the way out of a modal, a text field, or the app.
    pub fn reserved(self) -> bool {
        self.code == KeyCode::Esc
            || (self.code == KeyCode::Char('c') && self.mods.contains(KeyModifiers::CONTROL))
    }
}

/// `KeyCode` → its canonical config token.
fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Esc => "esc".into(),
        // Everything else (media keys, modifier-only presses, …) is
        // unbindable; naming it keeps `name()` total for display.
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

/// The inverse of [`key_name`], plus the aliases people actually type.
fn key_code(s: &str) -> Option<KeyCode> {
    let lower = s.trim().to_ascii_lowercase();
    Some(match lower.as_str() {
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "enter" | "return" => KeyCode::Enter,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "esc" | "escape" => KeyCode::Esc,
        _ => {
            if let Some(n) = lower.strip_prefix('f')
                && let Ok(n) = n.parse::<u8>()
                && (1..=24).contains(&n)
            {
                return Some(KeyCode::F(n));
            }
            // A single character binds as itself — cased, so `G` and `g` are
            // different bindings (they are on the keyboard, too).
            let mut chars = s.trim().chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}

/// Every remappable command. Declaration order is display order in the
/// settings card and tie-break order when two actions claim one chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    // Views
    ViewOverview,
    ViewProcesses,
    ViewThermal,
    ViewConnections,
    CycleView,
    // Process table
    Filter,
    SortMenu,
    Kill,
    Details,
    SelectDown,
    SelectUp,
    Top,
    Bottom,
    PageDown,
    PageUp,
    // App
    Settings,
    Inspect,
    Help,
    ThemeCycle,
    Pause,
    Hud,
    Faster,
    Slower,
    Quit,
}

/// Which block of the KEYS section an action is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Views,
    Table,
    App,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Self::Views => "views",
            Self::Table => "process table",
            Self::App => "app",
        }
    }
}

/// Every action, in display order.
pub const ACTIONS: [Action; 24] = [
    Action::ViewOverview,
    Action::ViewProcesses,
    Action::ViewThermal,
    Action::ViewConnections,
    Action::CycleView,
    Action::Filter,
    Action::SortMenu,
    Action::Kill,
    Action::Details,
    Action::SelectDown,
    Action::SelectUp,
    Action::Top,
    Action::Bottom,
    Action::PageDown,
    Action::PageUp,
    Action::Settings,
    Action::Inspect,
    Action::Help,
    Action::ThemeCycle,
    Action::Pause,
    Action::Hud,
    Action::Faster,
    Action::Slower,
    Action::Quit,
];

impl Action {
    /// The stable `[keys]` table name. Never rename one of these without a
    /// migration: an old config would silently lose that binding.
    pub fn name(self) -> &'static str {
        match self {
            Self::ViewOverview => "view_overview",
            Self::ViewProcesses => "view_processes",
            Self::ViewThermal => "view_thermal",
            Self::ViewConnections => "view_connections",
            Self::CycleView => "cycle_view",
            Self::Filter => "filter",
            Self::SortMenu => "sort_menu",
            Self::Kill => "kill",
            Self::Details => "details",
            Self::SelectDown => "select_down",
            Self::SelectUp => "select_up",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::PageDown => "page_down",
            Self::PageUp => "page_up",
            Self::Settings => "settings",
            Self::Inspect => "inspect",
            Self::Help => "help",
            Self::ThemeCycle => "theme_cycle",
            Self::Pause => "pause",
            Self::Hud => "hud",
            Self::Faster => "faster",
            Self::Slower => "slower",
            Self::Quit => "quit",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        ACTIONS.into_iter().find(|a| a.name() == name)
    }

    /// Short label for the KEYS section and the footer chips.
    pub fn title(self) -> &'static str {
        match self {
            Self::ViewOverview => "overview",
            Self::ViewProcesses => "processes",
            Self::ViewThermal => "thermal",
            Self::ViewConnections => "connections",
            Self::CycleView => "cycle views",
            Self::Filter => "filter",
            Self::SortMenu => "sort",
            Self::Kill => "kill",
            Self::Details => "details",
            Self::SelectDown => "select down",
            Self::SelectUp => "select up",
            Self::Top => "jump to top",
            Self::Bottom => "jump to bottom",
            Self::PageDown => "page down",
            Self::PageUp => "page up",
            Self::Settings => "settings",
            Self::Inspect => "inspect",
            Self::Help => "help",
            Self::ThemeCycle => "cycle theme",
            Self::Pause => "pause",
            Self::Hud => "debug hud",
            Self::Faster => "sample faster",
            Self::Slower => "sample slower",
            Self::Quit => "quit",
        }
    }

    /// One-line explainer — what the old help modal said, now attached to the
    /// action itself so the KEYS section and any future surface share it.
    pub fn help(self) -> &'static str {
        match self {
            Self::ViewOverview => "the dashboard: every metric card at once",
            Self::ViewProcesses => "full-screen process table",
            Self::ViewThermal => "chassis heat map + named sensors",
            Self::ViewConnections => "live connections by process",
            Self::CycleView => "step through the four views",
            Self::Filter => "filter processes by name (esc clears)",
            Self::SortMenu => "sort menu · column headers click too",
            Self::Kill => "signal picker for the selected process",
            Self::Details => "everything known about the selected process",
            Self::SelectDown => "move the selection down a row",
            Self::SelectUp => "move the selection up a row",
            Self::Top => "select the first row",
            Self::Bottom => "select the last row",
            Self::PageDown => "move the selection a screen down",
            Self::PageUp => "move the selection a screen up",
            Self::Settings => "this card — every setting in one place",
            Self::Inspect => "storage health, kernel activity, battery depth",
            Self::Help => "the key reference (this section)",
            Self::ThemeCycle => "next theme, applied and saved live",
            Self::Pause => "freeze sampling; the app keeps drawing",
            Self::Hud => "frame time, fps and tick in the footer",
            Self::Faster => "shorten the fast-tier interval by 50 ms",
            Self::Slower => "lengthen the fast-tier interval by 50 ms",
            Self::Quit => "save the config and exit",
        }
    }

    pub fn group(self) -> Group {
        match self {
            Self::ViewOverview
            | Self::ViewProcesses
            | Self::ViewThermal
            | Self::ViewConnections
            | Self::CycleView => Group::Views,
            Self::Filter
            | Self::SortMenu
            | Self::Kill
            | Self::Details
            | Self::SelectDown
            | Self::SelectUp
            | Self::Top
            | Self::Bottom
            | Self::PageDown
            | Self::PageUp => Group::Table,
            _ => Group::App,
        }
    }
}

/// The stock bindings — exactly what mxmon shipped before the keymap existed,
/// function-key aliases included. A config without a `[keys]` table behaves
/// identically to one that never had the feature.
const DEFAULTS: [(Action, &[&str]); 24] = [
    (Action::ViewOverview, &["1"]),
    (Action::ViewProcesses, &["2"]),
    (Action::ViewThermal, &["3"]),
    (Action::ViewConnections, &["4"]),
    (Action::CycleView, &["tab"]),
    (Action::Filter, &["/", "f3"]),
    (Action::SortMenu, &["s", "f6"]),
    (Action::Kill, &["x", "f9", "delete"]),
    (Action::Details, &["enter"]),
    (Action::SelectDown, &["j", "down"]),
    (Action::SelectUp, &["k", "up"]),
    (Action::Top, &["g", "home"]),
    (Action::Bottom, &["G", "end"]),
    (Action::PageDown, &["pagedown"]),
    (Action::PageUp, &["pageup"]),
    (Action::Settings, &["o"]),
    (Action::Inspect, &["i"]),
    (Action::Help, &["?", "f1"]),
    (Action::ThemeCycle, &["t"]),
    (Action::Pause, &["p"]),
    (Action::Hud, &["d"]),
    (Action::Faster, &["+", "="]),
    (Action::Slower, &["-"]),
    (Action::Quit, &["q", "f10"]),
];

/// Action → the chords that trigger it.
///
/// Keyed by [`Action`] rather than by chord because the action list is the
/// stable thing: it drives the KEYS section's rows, its display order, and
/// serialization. Lookup walks the table (23 actions, ~1.5 chords each) —
/// far cheaper than the redraw the keypress causes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    map: BTreeMap<Action, Vec<Chord>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Keymap {
    pub fn defaults() -> Self {
        let mut map = BTreeMap::new();
        for (action, names) in DEFAULTS {
            map.insert(
                action,
                names.iter().filter_map(|s| Chord::parse(s)).collect(),
            );
        }
        Self { map }
    }

    /// The command `chord` triggers, if any. Ties (only reachable by hand
    /// editing) go to the earliest action in [`ACTIONS`], so lookup is
    /// deterministic rather than map-order dependent.
    pub fn action(&self, chord: Chord) -> Option<Action> {
        ACTIONS
            .into_iter()
            .find(|a| self.chords(*a).contains(&chord))
    }

    pub fn chords(&self, action: Action) -> &[Chord] {
        self.map.get(&action).map_or(&[], Vec::as_slice)
    }

    /// Bind `chord` to `action`, taking it from whoever held it. Returns that
    /// previous owner so the caller can say so — a silent steal is how a user
    /// loses a key they still needed. Reserved chords are refused (`Err`).
    pub fn bind(&mut self, action: Action, chord: Chord) -> Result<Option<Action>, ()> {
        if chord.reserved() {
            return Err(());
        }
        let previous = self.action(chord).filter(|a| *a != action);
        if let Some(prev) = previous {
            self.unbind(prev, chord);
        }
        let list = self.map.entry(action).or_default();
        if !list.contains(&chord) {
            list.push(chord);
        }
        Ok(previous)
    }

    pub fn unbind(&mut self, action: Action, chord: Chord) {
        if let Some(list) = self.map.get_mut(&action) {
            list.retain(|c| *c != chord);
        }
    }

    pub fn reset(&mut self, action: Action) {
        if let Some((_, names)) = DEFAULTS.iter().find(|(a, _)| *a == action) {
            let restored: Vec<Chord> = names.iter().filter_map(|s| Chord::parse(s)).collect();
            // The defaults may be held by something the user rebound them to;
            // take them back so a reset always ends with the stock behavior.
            for chord in &restored {
                if let Some(owner) = self.action(*chord)
                    && owner != action
                {
                    self.unbind(owner, *chord);
                }
            }
            self.map.insert(action, restored);
        }
    }

    pub fn is_default(&self, action: Action) -> bool {
        DEFAULTS
            .iter()
            .find(|(a, _)| *a == action)
            .is_some_and(|(_, names)| {
                let stock: Vec<Chord> = names.iter().filter_map(|s| Chord::parse(s)).collect();
                self.chords(action) == stock.as_slice()
            })
    }

    pub fn all_default(&self) -> bool {
        ACTIONS.into_iter().all(|a| self.is_default(a))
    }
}

/// Serialized as a plain `action = ["chord", …]` table so `[keys]` reads and
/// edits like the rest of `config.toml`.
impl Serialize for Keymap {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut map = ser.serialize_map(Some(ACTIONS.len()))?;
        for action in ACTIONS {
            let names: Vec<String> = self.chords(action).iter().map(|c| c.name()).collect();
            map.serialize_entry(action.name(), &names)?;
        }
        map.end()
    }
}

/// Deserialization starts from the defaults and overrides only the actions the
/// file actually names, so a partial (or older) `[keys]` table keeps every
/// other binding alive. An empty list is honored as "unbound" — that is the
/// only way to say it. Unknown actions and unparsable chords are dropped, never
/// fatal: the config is hand-editable and must not be able to break startup.
impl<'de> Deserialize<'de> for Keymap {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, Vec<String>>::deserialize(de).map_err(D::Error::custom)?;
        let mut keymap = Self::defaults();
        for (name, chords) in raw {
            if let Some(action) = Action::from_name(&name) {
                keymap.map.insert(
                    action,
                    chords
                        .iter()
                        .filter_map(|s| Chord::parse(s))
                        .filter(|c| !c.reserved())
                        .collect(),
                );
            }
        }
        Ok(keymap)
    }
}

#[cfg(test)]
mod tests {
    use super::{ACTIONS, Action, Chord, Keymap};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn chord(s: &str) -> Chord {
        Chord::parse(s).unwrap_or_else(|| panic!("{s} must parse"))
    }

    #[test]
    fn names_round_trip_through_parse() {
        // The property that makes `[keys]` hand-editable: whatever we write,
        // we read back identically.
        let samples = [
            "q",
            "G",
            "+",
            "=",
            "-",
            "/",
            "?",
            "1",
            "space",
            "enter",
            "esc",
            "tab",
            "backtab",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "pageup",
            "pagedown",
            "delete",
            "insert",
            "backspace",
            "f1",
            "f10",
            "f24",
            "ctrl+r",
            "alt+x",
            "ctrl+alt+p",
            "shift+f5",
            "ctrl++",
        ];
        for s in samples {
            let c = chord(s);
            assert_eq!(
                Chord::parse(&c.name()),
                Some(c),
                "{s} → {} must round-trip",
                c.name()
            );
        }
    }

    #[test]
    fn parse_rejects_nonsense_without_panicking() {
        for s in [
            "",
            "   ",
            "ctrl+",
            "ctrl",
            "hyper+q",
            "f0",
            "f25",
            "f99",
            "notakey",
            "ctrl+notakey",
            "++q",
            "qq",
        ] {
            assert_eq!(Chord::parse(s), None, "{s:?} must not parse");
        }
        // A bare "+" and "ctrl++" *are* legal — the separator is also a key.
        assert_eq!(chord("+").code, KeyCode::Char('+'));
        assert!(chord("ctrl++").mods.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn from_event_drops_redundant_shift_on_characters() {
        let ev = |code, mods| KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        // 'G' already says shift; terminals disagree about also setting the
        // flag, so the chord must be the same either way.
        assert_eq!(
            Chord::from_event(ev(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Chord::from_event(ev(KeyCode::Char('G'), KeyModifiers::NONE)),
        );
        assert_eq!(
            Chord::from_event(ev(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            chord("G")
        );
        // Named keys keep it — there it carries the whole meaning.
        assert_eq!(
            Chord::from_event(ev(KeyCode::F(5), KeyModifiers::SHIFT)),
            chord("shift+f5")
        );
        // Ctrl survives on characters.
        assert_eq!(
            Chord::from_event(ev(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            chord("ctrl+r")
        );
        // Unbindable modifiers are ignored rather than making the chord unmatchable.
        assert_eq!(
            Chord::from_event(ev(KeyCode::Char('q'), KeyModifiers::SUPER)),
            chord("q")
        );
    }

    #[test]
    fn defaults_reproduce_the_shipped_bindings() {
        let km = Keymap::defaults();
        for (keys, action) in [
            (["1"].as_slice(), Action::ViewOverview),
            (&["2"], Action::ViewProcesses),
            (&["3"], Action::ViewThermal),
            (&["4"], Action::ViewConnections),
            (&["tab"], Action::CycleView),
            (&["/", "f3"], Action::Filter),
            (&["s", "f6"], Action::SortMenu),
            (&["x", "f9", "delete"], Action::Kill),
            (&["enter"], Action::Details),
            (&["j", "down"], Action::SelectDown),
            (&["k", "up"], Action::SelectUp),
            (&["g", "home"], Action::Top),
            (&["G", "end"], Action::Bottom),
            (&["o"], Action::Settings),
            (&["?", "f1"], Action::Help),
            (&["t"], Action::ThemeCycle),
            (&["p"], Action::Pause),
            (&["d"], Action::Hud),
            (&["+", "="], Action::Faster),
            (&["-"], Action::Slower),
            (&["q", "f10"], Action::Quit),
        ] {
            for k in keys {
                assert_eq!(
                    km.action(chord(k)),
                    Some(action),
                    "{k} must fire {action:?}"
                );
            }
        }
        assert!(km.all_default());
        // Case matters: `g` and `G` are different rows.
        assert_ne!(km.action(chord("g")), km.action(chord("G")));
    }

    #[test]
    fn every_action_has_a_name_title_and_binding() {
        for action in ACTIONS {
            assert!(!action.title().is_empty());
            assert!(!action.help().is_empty());
            assert_eq!(Action::from_name(action.name()), Some(action));
            assert!(
                !Keymap::defaults().chords(action).is_empty(),
                "{action:?} ships unbound"
            );
        }
        // Names are unique — a duplicate would make one action unreachable
        // from the config file.
        let mut names: Vec<&str> = ACTIONS.iter().map(|a| a.name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate action names");
    }

    #[test]
    fn bind_steals_the_chord_and_names_the_loser() {
        let mut km = Keymap::defaults();
        // `t` cycles the theme; hand it to Pause and the theme loses it.
        assert_eq!(
            km.bind(Action::Pause, chord("t")),
            Ok(Some(Action::ThemeCycle))
        );
        assert_eq!(km.action(chord("t")), Some(Action::Pause));
        assert!(!km.chords(Action::ThemeCycle).contains(&chord("t")));
        assert!(!km.is_default(Action::Pause));
        // Re-binding the same chord to the same action is a no-op, not a
        // duplicate entry.
        assert_eq!(km.bind(Action::Pause, chord("t")), Ok(None));
        assert_eq!(
            km.chords(Action::Pause)
                .iter()
                .filter(|c| **c == chord("t"))
                .count(),
            1
        );
        // Reset pulls the default back even though something else holds it.
        km.reset(Action::ThemeCycle);
        assert_eq!(km.action(chord("t")), Some(Action::ThemeCycle));
        assert!(km.is_default(Action::ThemeCycle));
        // Resetting every action individually is the same as starting over —
        // the whole-config reset in `settings::reset_all` relies on it.
        for action in ACTIONS {
            km.reset(action);
        }
        assert_eq!(km, Keymap::defaults());
        assert!(km.all_default());
    }

    #[test]
    fn reserved_chords_cannot_be_rebound() {
        let mut km = Keymap::defaults();
        assert!(km.bind(Action::Quit, chord("esc")).is_err());
        assert!(km.bind(Action::Pause, chord("ctrl+c")).is_err());
        assert_eq!(km, Keymap::defaults(), "a refused bind changes nothing");
    }

    #[test]
    fn unbinding_leaves_the_action_reachable_only_by_its_other_chords() {
        let mut km = Keymap::defaults();
        km.unbind(Action::Quit, chord("q"));
        assert_eq!(km.action(chord("q")), None);
        assert_eq!(km.action(chord("f10")), Some(Action::Quit));
        km.unbind(Action::Quit, chord("f10"));
        assert!(km.chords(Action::Quit).is_empty(), "fully unbound is legal");
        assert!(!km.is_default(Action::Quit));
    }

    #[test]
    fn serde_round_trips_and_tolerates_a_hand_edited_table() {
        let mut km = Keymap::defaults();
        km.bind(Action::Quit, chord("ctrl+q")).unwrap();
        let toml = toml::to_string(&km).expect("serialize");
        let back: Keymap = toml::from_str(&toml).expect("deserialize");
        assert_eq!(back, km);

        // Partial table: only the named action changes, everything else keeps
        // its default — that is what makes the format forward-compatible.
        let partial: Keymap = toml::from_str("pause = [\"space\"]\n").expect("partial");
        assert_eq!(partial.action(chord("space")), Some(Action::Pause));
        assert_eq!(partial.action(chord("q")), Some(Action::Quit));

        // Garbage is dropped, not fatal: unknown action, unparsable chord,
        // and a reserved chord someone tried to claim.
        let messy: Keymap = toml::from_str(
            "not_an_action = [\"z\"]\nquit = [\"ctrl+q\", \"notakey\"]\npause = [\"esc\"]\n",
        )
        .expect("messy table still loads");
        assert_eq!(messy.action(chord("ctrl+q")), Some(Action::Quit));
        assert_eq!(messy.chords(Action::Quit), [chord("ctrl+q")]);
        assert!(messy.chords(Action::Pause).is_empty());
        assert_eq!(messy.action(chord("z")), None);

        // An explicit empty list means unbound (the only way to say it).
        let unbound: Keymap = toml::from_str("hud = []\n").expect("empty list");
        assert!(unbound.chords(Action::Hud).is_empty());
    }

    #[test]
    fn labels_are_display_only_but_never_empty() {
        for action in ACTIONS {
            for c in Keymap::defaults().chords(action) {
                assert!(!c.label().is_empty(), "{action:?} has a blank chord label");
            }
        }
        assert_eq!(chord("up").label(), "↑");
        assert_eq!(chord("enter").label(), "⏎");
        assert_eq!(chord("f10").label(), "F10");
        assert_eq!(chord("ctrl+r").label(), "ctrl+r");
    }

    proptest::proptest! {
        /// Any string at all: parse either yields a chord that round-trips or
        /// `None` — it never panics. `config.toml` is hand-editable, so this
        /// is the same never-panic contract the wire decoders hold.
        #[test]
        fn parse_never_panics(s in ".{0,24}") {
            if let Some(c) = Chord::parse(&s) {
                proptest::prop_assert_eq!(Chord::parse(&c.name()), Some(c));
                proptest::prop_assert!(!c.label().is_empty());
            }
        }
    }
}
