//! The kernel's thermal-pressure verdict, read through `libnotify`.
//!
//! This is the one sudoless source that says whether macOS *thinks* the machine
//! is thermally constrained. Raw °C cannot answer that: the throttle point
//! depends on chassis, workload, and headroom the OS never publishes.
//! `pmset -g therm` is empty on Apple Silicon and `pmset -g thermlog` streams
//! forever without exiting, so neither is usable as a source.
//!
//! Registration happens once; each read is a token lookup costing ~12 µs.

use std::ffi::{CString, c_char};

#[link(name = "System")]
unsafe extern "C" {
    fn notify_register_check(name: *const c_char, out_token: *mut i32) -> u32;
    fn notify_get_state(token: i32, state: *mut u64) -> u32;
    fn notify_cancel(token: i32) -> u32;
}

/// `kOSThermalNotificationPressureLevelName`.
const PRESSURE_KEY: &str = "com.apple.system.thermalpressurelevel";

const NOTIFY_STATUS_OK: u32 = 0;

/// The kernel's thermal-pressure ladder (`OSThermalPressureLevel`).
///
/// macOS publishes six levels where `NSProcessInfo.thermalState` collapses to
/// four — verified on-device: a value of 2 here reads as `.fair` there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    Nominal,
    Light,
    Moderate,
    Heavy,
    Trapping,
    Sleeping,
}

impl Pressure {
    /// Map a raw level. Unknown values above the documented ladder are treated
    /// as the most severe rather than ignored: a future OS adding a level
    /// should read as "worse than heavy", never as "fine".
    fn from_raw(raw: u64) -> Self {
        match raw {
            0 => Self::Nominal,
            1 => Self::Light,
            2 => Self::Moderate,
            3 => Self::Heavy,
            4 => Self::Trapping,
            _ => Self::Sleeping,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Light => "light",
            Self::Moderate => "moderate",
            Self::Heavy => "heavy",
            Self::Trapping => "trapping",
            Self::Sleeping => "sleeping",
        }
    }

    /// Whether the OS considers the machine thermally constrained — the point
    /// at which it starts trading performance for temperature.
    pub fn throttling(self) -> bool {
        self >= Self::Moderate
    }

    /// Severity in `0.0..=1.0`, for gauges and color ramps.
    pub fn severity(self) -> f32 {
        match self {
            Self::Nominal => 0.0,
            Self::Light => 0.2,
            Self::Moderate => 0.4,
            Self::Heavy => 0.6,
            Self::Trapping => 0.8,
            Self::Sleeping => 1.0,
        }
    }
}

/// A registered handle on the thermal-pressure notification.
pub struct ThermalPressure {
    token: i32,
}

impl ThermalPressure {
    /// Register once. `None` when the key is unavailable, which simply means
    /// the reading is absent — never a reason to fail the collector.
    pub fn new() -> Option<Self> {
        let name = CString::new(PRESSURE_KEY).ok()?;
        let mut token = 0i32;
        let status = unsafe { notify_register_check(name.as_ptr(), &raw mut token) };
        (status == NOTIFY_STATUS_OK).then_some(Self { token })
    }

    /// Current pressure level, or `None` if the check failed this time.
    pub fn read(&self) -> Option<Pressure> {
        let mut state = 0u64;
        let status = unsafe { notify_get_state(self.token, &raw mut state) };
        (status == NOTIFY_STATUS_OK).then(|| Pressure::from_raw(state))
    }
}

impl Drop for ThermalPressure {
    fn drop(&mut self) {
        unsafe { notify_cancel(self.token) };
    }
}

#[cfg(test)]
mod tests {
    use super::Pressure;

    #[test]
    fn raw_levels_map_to_the_documented_ladder() {
        assert_eq!(Pressure::from_raw(0), Pressure::Nominal);
        assert_eq!(Pressure::from_raw(2), Pressure::Moderate);
        assert_eq!(Pressure::from_raw(4), Pressure::Trapping);
    }

    #[test]
    fn unknown_levels_read_as_severe_not_as_fine() {
        // A future OS level must never be silently reported as nominal.
        assert_eq!(Pressure::from_raw(99), Pressure::Sleeping);
        assert!(Pressure::from_raw(u64::MAX).throttling());
    }

    #[test]
    fn throttling_starts_at_moderate() {
        assert!(!Pressure::Nominal.throttling());
        assert!(!Pressure::Light.throttling());
        assert!(Pressure::Moderate.throttling());
        assert!(Pressure::Heavy.throttling());
    }

    #[test]
    fn severity_is_ordered_and_bounded() {
        let ladder = [
            Pressure::Nominal,
            Pressure::Light,
            Pressure::Moderate,
            Pressure::Heavy,
            Pressure::Trapping,
            Pressure::Sleeping,
        ];
        for pair in ladder.windows(2) {
            assert!(pair[0].severity() < pair[1].severity());
        }
        assert!(ladder.iter().all(|p| (0.0..=1.0).contains(&p.severity())));
    }
}
