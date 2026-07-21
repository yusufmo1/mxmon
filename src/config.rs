//! Persistent user preferences: `~/.config/mxmon/config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::collect::sampler::{FAST_MS_DEFAULT, FAST_MS_MAX, FAST_MS_MIN};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme name (any `ui::theme::THEMES` entry, e.g. "neon", "gruvbox",
    /// "tokyonight"); unknown names fall back to "neon".
    pub theme: String,
    /// Fast-tier sampling interval (ms); other tiers scale from it.
    pub interval_ms: u64,
    /// Use octant characters for graphs (needs a font with legacy computing
    /// symbols); braille otherwise.
    pub octant_graphs: bool,
    /// Probe connectivity (latency/jitter/reachability) with ICMP echoes —
    /// the only thing mxmon ever sends on the network. `false` = fully passive.
    pub ping: bool,
    /// Ping target: an IPv4 literal or a hostname (resolved once at startup).
    pub ping_host: String,
    /// Max side-by-side process panes on wide layouts (1–4). At 1 (default)
    /// the table stays a single comfortable pane and the layout hands the
    /// freed width to the metric panels instead.
    pub procs_panes: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "neon".into(),
            interval_ms: FAST_MS_DEFAULT,
            octant_graphs: false,
            ping: true,
            ping_host: "1.1.1.1".into(),
            procs_panes: 1,
        }
    }
}

/// `~/.config/mxmon` — home of the config and the SMC sensor-discovery cache.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/mxmon"))
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
