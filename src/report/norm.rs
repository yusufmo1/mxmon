//! The single place a typed unit becomes a JSON scalar for the v1 report.
//!
//! Every field in the contract routes through here, the way [`crate::units`]'s
//! `Display` impls are the one formatting source of truth for the TUI. The
//! convention the agent-facing schema promises:
//!
//! - ratios emit as `0.0..=1.0` (never a percentage), and are not clamped:
//!   a process using more than one core, or an SSD past its rated endurance,
//!   is a real reading above 1.0, not an error to hide.
//! - byte counts emit as integers (never gigabyte floats).
//! - power emits as watts, temperature as Celsius, both floats.
//! - frequency emits as whole MHz (the source's native resolution; promoting
//!   to Hz would imply a precision the DVFS tables never carry).
//!
//! A non-finite input (a telemetry glitch) stays non-finite, which serde_json
//! renders as JSON `null`: "no reading", never a `NaN` or a panic.

use crate::units::{Bytes, Celsius, Mhz, Ratio, Watts};

/// Round to `dp` decimal places, leaving a non-finite value untouched so it
/// serializes to `null`.
fn round(v: f64, dp: i32) -> f64 {
    if !v.is_finite() {
        return v;
    }
    let f = 10f64.powi(dp);
    (v * f).round() / f
}

/// A ratio in `0.0..=1.0` (not clamped), rounded to 0.01%.
pub fn ratio(r: Ratio) -> f64 {
    round(f64::from(r.0), 4)
}

/// Power in watts, rounded to the milliwatt.
pub fn watts(w: Watts) -> f64 {
    round(f64::from(w.0), 3)
}

/// Temperature in Celsius, rounded to 0.1 degree.
pub fn celsius(c: Celsius) -> f64 {
    round(f64::from(c.0), 1)
}

/// Frequency in whole MHz.
pub fn mhz(m: Mhz) -> u64 {
    u64::from(m.0)
}

/// An exact byte count.
pub fn bytes(b: Bytes) -> u64 {
    b.0
}

/// A millisecond quantity (latency, rtt, jitter), rounded to 0.01 ms.
pub fn ms(v: f32) -> f64 {
    round(f64::from(v), 2)
}

/// A microsecond quantity (per-op device latency), rounded to whole µs.
pub fn us(v: f32) -> f64 {
    round(f64::from(v), 0)
}

/// A per-second rate, rounded to 0.1.
pub fn rate(v: f64) -> f64 {
    round(v, 1)
}

/// A ratio already held as an `f64` (0..1), rounded to 0.01%.
pub fn ratio_f64(v: f64) -> f64 {
    round(v, 4)
}

/// A small unitless quantity (IPC, runnable seconds), rounded to 0.01.
pub fn small(v: f64) -> f64 {
    round(v, 2)
}

/// A whole-percent value (`0..=100`) folded into a `0.0..=1.0` ratio.
pub fn percent_to_ratio(pct: f64) -> f64 {
    round(pct / 100.0, 4)
}

/// Narrow one of the SMART log page's 128-bit counters to `u64`, saturating.
/// `u64::MAX` is ~18 EB, far above any real endurance figure, so no drive
/// loses information and the value becomes numerically comparable in `check`.
pub fn wide(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_matches_the_documented_resolution() {
        assert!(
            (watts(Watts(5.234_5)) - 5.234).abs() < 1e-9
                || (watts(Watts(5.234_5)) - 5.235).abs() < 1e-9
        );
        assert!((ratio(Ratio(0.503_37)) - 0.5034).abs() < 1e-9);
        assert!((celsius(Celsius(83.44)) - 83.4).abs() < 1e-9);
        assert_eq!(mhz(Mhz(3152)), 3152);
        assert_eq!(bytes(Bytes(13_900_000_000)), 13_900_000_000);
        assert!((ms(1.234) - 1.23).abs() < 1e-9);
        assert!((percent_to_ratio(50.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn ratios_are_not_clamped() {
        // A process on four cores is 4.0, a legitimate reading, not 1.0.
        assert!((ratio(Ratio(4.0)) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn wide_saturates_instead_of_wrapping() {
        assert_eq!(wide(717_000_000_000), 717_000_000_000);
        assert_eq!(wide(u128::MAX), u64::MAX);
    }

    #[test]
    fn non_finite_stays_non_finite_and_serializes_to_null() {
        assert!(watts(Watts(f32::NAN)).is_nan());
        assert_eq!(
            serde_json::to_string(&watts(Watts(f32::INFINITY))).unwrap(),
            "null"
        );
        assert_eq!(
            serde_json::to_string(&celsius(Celsius(f32::NAN))).unwrap(),
            "null"
        );
    }
}
