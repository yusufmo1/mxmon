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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "midnight".into(),
            interval_ms: FAST_MS_DEFAULT,
            glyphs: Glyphs::Auto,
            ping: true,
            ping_host: "1.1.1.1".into(),
            procs_panes: 1,
            schematic: true,
            contours: true,
            graph_window: 4,
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
            theme: "gruvbox".into(),
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
        };
        c.save();
        let l = Config::load();
        assert_eq!(l.theme, "gruvbox");
        assert_eq!(l.interval_ms, 750);
        assert_eq!(l.glyphs, Glyphs::Octant);
        assert!(!l.ping);
        assert_eq!(l.ping_host, "9.9.9.9");
        assert_eq!(l.procs_panes, 3);
        assert!(!l.schematic);
        assert!(!l.contours);
        assert_eq!(l.graph_window, 7);
    }
}
