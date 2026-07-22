//! Persistent user preferences: `~/.config/mxmon/config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::collect::sampler::{FAST_MS_DEFAULT, FAST_MS_MAX, FAST_MS_MIN};

/// Sub-cell glyph set for the braille-drawn graphs. Rendering always happens
/// in braille; `ui::glyphs::octantize_buffer` upgrades the finished frame to
/// Unicode 16 octants (solid 2×4 blocks, no dot gaps) when this resolves on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Glyphs {
    /// Octants where the terminal is known to draw them (Ghostty, Kitty,
    /// WezTerm, foot), braille everywhere else.
    Auto,
    /// Force octants (needs a terminal or font with Symbols for Legacy
    /// Computing Supplement coverage).
    Octant,
    /// Force braille — safe in every terminal.
    Braille,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme name (any `ui::theme::THEMES` entry, e.g. "midnight", "neon",
    /// "gruvbox"); unknown names fall back to "midnight".
    pub theme: String,
    /// Panel frames, graph baselines, gauge tracks, schematic ink — the
    /// theme's `border` role. `"theme"` (default) defers to whichever theme
    /// is active; otherwise a `ui::theme::INKS` name or a `#rrggbb` literal.
    /// Set once, it holds across theme cycling. Unresolvable values fall
    /// back to the theme's own color.
    pub frames: String,
    /// Grey label / unit / hint / axis text — the theme's `dim` role. Same
    /// value grammar as [`Config::frames`].
    pub labels: String,
    /// Fast-tier sampling interval (ms); other tiers scale from it.
    pub interval_ms: u64,
    /// Sub-cell glyph set for graphs: `auto` (octants on terminals that draw
    /// them, braille elsewhere), `octant`, or `braille`.
    pub glyphs: Glyphs,
    /// Probe connectivity (latency/jitter/reachability) with ICMP echoes —
    /// the only thing mxmon ever sends on the network. `false` = fully passive.
    pub ping: bool,
    /// Ping target: an IPv4 literal or a hostname (resolved once at startup).
    pub ping_host: String,
    /// Max side-by-side process panes on wide layouts (1–4). At 1 (default)
    /// the table stays a single comfortable pane and the layout hands the
    /// freed width to the metric panels instead.
    pub procs_panes: u16,
    /// Draw the chassis blueprint (fans, SoC package, battery, …) beneath
    /// the thermal map's isotherm contours.
    pub schematic: bool,
    /// Draw the heat map's isotherm rings and hot-core fill. Off leaves the
    /// blueprint and every reading in place on a quiet deck — the field
    /// math is skipped entirely, not just hidden.
    pub contours: bool,
    /// Fast ticks aggregated into each graph dot column (1–8). Rings still
    /// fill every tick and the head column stays live; at ×4 the graph body
    /// advances a quarter as fast, showing 4× the history. The settings
    /// modal steps ×1/×2/×4/×8; hand-edited in-between values are honored.
    pub graph_window: u16,
    /// Fluid graphs: interpolate the live head and the bucket-shift between
    /// ticks (~30 fps while values move, zero frames at rest). Off renders
    /// exactly one frame per sample.
    pub motion: bool,
    /// Key bindings for every global command, remappable from the settings
    /// card's KEYS section. Absent (or partially specified) means the stock
    /// bindings — see [`crate::keys`].
    ///
    /// **Must stay the last field:** TOML demands every scalar before the
    /// first table, and this one serializes as `[keys]`. Moving it up makes
    /// `toml::to_string_pretty` fail and silently stops the config saving.
    /// Poll NVMe SMART, APFS volume statistics, and the drive controller's
    /// throttle counters on the slow health tier. The priciest thing mxmon
    /// asks the system for, so it is switchable.
    pub storage_health: bool,

    /// Poll per-device interrupt counts and wake assertions.
    pub kernel_stats: bool,

    /// Per-card visibility. Hiding a card only removes it from the layout —
    /// its collector keeps running, because other surfaces (the JSON snapshot,
    /// the flow diagram, the heat map) still read the same data. Sampling is
    /// governed by the collector toggles above.
    pub show_cpu: bool,
    pub show_gpu: bool,
    pub show_mem: bool,
    pub show_net: bool,
    pub show_disk: bool,
    pub show_power: bool,
    pub show_temps: bool,
    pub show_battery: bool,

    pub keys: crate::keys::Keymap,
}

impl Config {
    /// Whether a metric card is currently shown.
    pub fn panel_visible(&self, kind: crate::ui::widgets::PanelKind) -> bool {
        use crate::ui::widgets::PanelKind as P;
        match kind {
            P::Cpu => self.show_cpu,
            P::Gpu => self.show_gpu,
            P::Mem => self.show_mem,
            P::Net => self.show_net,
            P::Disk => self.show_disk,
            P::Power => self.show_power,
            P::Temps => self.show_temps,
            P::Battery => self.show_battery,
            // The heat map is a view, not a card in the strip.
            P::HeatMap => true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "midnight".into(),
            frames: "theme".into(),
            labels: "theme".into(),
            interval_ms: FAST_MS_DEFAULT,
            glyphs: Glyphs::Auto,
            ping: true,
            ping_host: "1.1.1.1".into(),
            procs_panes: 1,
            schematic: true,
            contours: true,
            graph_window: 4,
            motion: true,
            storage_health: true,
            kernel_stats: true,
            show_cpu: true,
            show_gpu: true,
            show_mem: true,
            show_net: true,
            show_disk: true,
            show_power: true,
            show_temps: true,
            show_battery: true,
            keys: crate::keys::Keymap::defaults(),
        }
    }
}

/// `~/.config/mxmon` — home of the config and the SMC sensor-discovery cache.
/// `MXMON_CONFIG_DIR` relocates it wholesale (the e2e tests point spawned
/// binaries at a tempdir so runs stay hermetic).
#[cfg(not(test))]
pub fn dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MXMON_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/mxmon"))
}

/// Test builds never see the real `~/.config/mxmon`: only a per-thread
/// tempdir installed via [`test_dir`] resolves, so no test — present or
/// future — can read or clobber the user's actual config or sensor cache.
#[cfg(test)]
pub fn dir() -> Option<PathBuf> {
    TEST_DIR.with(|d| d.borrow().clone())
}

#[cfg(test)]
thread_local! {
    static TEST_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Point this thread's config dir at `path` until the guard drops (tests).
#[cfg(test)]
#[must_use]
pub fn test_dir(path: PathBuf) -> TestDirGuard {
    TEST_DIR.with(|d| *d.borrow_mut() = Some(path));
    TestDirGuard
}

/// Clears the per-thread config-dir override on drop.
#[cfg(test)]
pub struct TestDirGuard;

#[cfg(test)]
impl Drop for TestDirGuard {
    fn drop(&mut self) {
        TEST_DIR.with(|d| *d.borrow_mut() = None);
    }
}

fn config_path() -> Option<PathBuf> {
    dir().map(|d| d.join("config.toml"))
}

impl Config {
    pub fn load() -> Self {
        let mut config: Self = config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        config.interval_ms = config.interval_ms.clamp(FAST_MS_MIN, FAST_MS_MAX);
        config.procs_panes = config.procs_panes.clamp(1, 4);
        config.graph_window = config.graph_window.clamp(1, 8);
        config
    }

    /// Best-effort persist (a read-only home dir shouldn't break the app).
    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::widgets::PanelKind;

    #[test]
    fn panel_visibility_maps_each_card_to_its_own_switch() {
        let mut c = Config::default();
        // Everything is on by default — hiding is opt-in.
        for kind in [
            PanelKind::Cpu,
            PanelKind::Gpu,
            PanelKind::Mem,
            PanelKind::Net,
            PanelKind::Disk,
            PanelKind::Power,
            PanelKind::Temps,
            PanelKind::Battery,
        ] {
            assert!(c.panel_visible(kind), "{kind:?} defaults to shown");
        }
        // Each switch governs exactly one card.
        c.show_gpu = false;
        assert!(!c.panel_visible(PanelKind::Gpu));
        assert!(c.panel_visible(PanelKind::Cpu));
    }

    #[test]
    fn the_heat_map_is_a_view_not_a_hideable_card() {
        let c = Config::default();
        assert!(c.panel_visible(PanelKind::HeatMap));
    }

    use super::{Config, Glyphs, dir, test_dir};
    use crate::collect::sampler::FAST_MS_DEFAULT;

    #[test]
    fn without_override_tests_see_no_config_dir() {
        // The hermetic guarantee itself: no override → no path → load yields
        // defaults and save has nowhere to write.
        assert!(dir().is_none());
        let c = Config::load();
        assert_eq!(c.theme, "midnight");
        assert_eq!(c.interval_ms, FAST_MS_DEFAULT);
        c.save(); // must be a silent no-op
    }

    #[test]
    fn load_clamps_and_tolerates_unknown_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = test_dir(tmp.path().to_path_buf());
        std::fs::write(
            tmp.path().join("config.toml"),
            "interval_ms = 5\nprocs_panes = 99\ntheme = \"neon\"\nfuture_option = true\n",
        )
        .unwrap();
        let c = Config::load();
        assert_eq!(c.interval_ms, 100, "clamped up to FAST_MS_MIN");
        assert_eq!(c.procs_panes, 4, "clamped down to the pane cap");
        assert_eq!(c.theme, "neon");
        assert!(c.ping, "absent keys keep their defaults");
        assert_eq!(c.glyphs, Glyphs::Auto, "absent glyphs key stays auto");
        assert_eq!(
            (c.frames.as_str(), c.labels.as_str()),
            ("theme", "theme"),
            "no chrome override until the user sets one"
        );
        assert_eq!(c.graph_window, 4, "absent graph_window keeps the default");
        std::fs::write(tmp.path().join("config.toml"), "graph_window = 99\n").unwrap();
        assert_eq!(Config::load().graph_window, 8, "clamped down to ×8");
        std::fs::write(tmp.path().join("config.toml"), "graph_window = 0\n").unwrap();
        assert_eq!(Config::load().graph_window, 1, "clamped up to ×1");
    }

    #[test]
    fn load_falls_back_on_missing_or_corrupt_file() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = test_dir(tmp.path().to_path_buf());
        assert_eq!(Config::load().interval_ms, FAST_MS_DEFAULT, "no file yet");
        std::fs::write(tmp.path().join("config.toml"), "not = [valid").unwrap();
        assert_eq!(Config::load().theme, "midnight", "corrupt file → defaults");
    }

    #[test]
    fn save_round_trips_every_field() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = test_dir(tmp.path().to_path_buf());
        let c = Config {
            storage_health: false,
            kernel_stats: false,
            show_cpu: false,
            show_gpu: false,
            show_mem: false,
            show_net: false,
            show_disk: false,
            show_power: false,
            show_temps: false,
            show_battery: false,
            theme: "gruvbox".into(),
            frames: "white".into(),
            // Off the modal's cycle on purpose: the hex escape hatch must
            // survive a round trip untouched.
            labels: "#ff8800".into(),
            interval_ms: 750,
            glyphs: Glyphs::Octant,
            ping: false,
            ping_host: "9.9.9.9".into(),
            procs_panes: 3,
            schematic: false,
            contours: false,
            // Odd on purpose: only the modal is limited to the ×1/2/4/8
            // stops — a hand-tuned value must survive the round trip.
            graph_window: 7,
            motion: false,
            keys: {
                let mut km = crate::keys::Keymap::defaults();
                km.bind(
                    crate::keys::Action::Quit,
                    crate::keys::Chord::parse("ctrl+q").unwrap(),
                )
                .unwrap();
                km
            },
        };
        c.save();
        let l = Config::load();
        assert_eq!(l.theme, "gruvbox");
        assert_eq!(l.frames, "white");
        assert_eq!(l.labels, "#ff8800");
        assert_eq!(l.interval_ms, 750);
        assert_eq!(l.glyphs, Glyphs::Octant);
        assert!(!l.ping);
        assert_eq!(l.ping_host, "9.9.9.9");
        assert_eq!(l.procs_panes, 3);
        assert!(!l.schematic);
        assert!(!l.contours);
        assert_eq!(l.graph_window, 7);
        assert!(!l.motion);
        // The keymap is a table, so it also proves the field order still
        // serializes: a scalar declared after it would break `save` outright.
        assert_eq!(l.keys, c.keys);
        assert_eq!(
            l.keys.action(crate::keys::Chord::parse("ctrl+q").unwrap()),
            Some(crate::keys::Action::Quit)
        );
    }
}
