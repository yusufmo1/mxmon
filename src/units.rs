//! Typed units with a single formatting source of truth.
//!
//! Every value that crosses a module boundary is wrapped, so a watt can never
//! be mistaken for a megahertz and formatting stays consistent everywhere.

use std::fmt;

use serde::Serialize;

/// Power in watts.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct Watts(pub f32);

/// Frequency in megahertz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
pub struct Mhz(pub u32);

/// Temperature in degrees Celsius.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct Celsius(pub f32);

/// A byte count (memory sizes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
pub struct Bytes(pub u64);

/// A ratio in `0.0..=1.0` (rendered as a percentage).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct Ratio(pub f32);

impl Ratio {
    /// Clamp into the displayable `0..=1` range.
    pub fn clamped(self) -> Self {
        Self(self.0.clamp(0.0, 1.0))
    }

    pub fn as_percent(self) -> f32 {
        self.0 * 100.0
    }
}

impl Bytes {
    pub const KIB: u64 = 1 << 10;
    pub const MIB: u64 = 1 << 20;
    pub const GIB: u64 = 1 << 30;

    pub fn as_f64(self) -> f64 {
        self.0 as f64
    }
}

// Every impl routes through `Formatter::pad` so callers can request a fixed
// width ("{:>6}") — auto-ranged units change string length as values move
// (748MHz ↔ 1.03GHz), and unpadded they make everything after them jitter.

impl fmt::Display for Watts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = if self.0 >= 10.0 {
            format!("{:.1}W", self.0)
        } else if self.0 >= 0.9995 {
            // The .9995 floor keeps "{:.2}" rounding from ever printing 1000mW.
            format!("{:.2}W", self.0)
        } else {
            format!("{:.0}mW", self.0 * 1000.0)
        };
        f.pad(&s)
    }
}

impl fmt::Display for Mhz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = if self.0 >= 1000 {
            format!("{:.2}GHz", f64::from(self.0) / 1000.0)
        } else {
            format!("{}MHz", self.0)
        };
        f.pad(&s)
    }
}

impl fmt::Display for Celsius {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&format!("{:.0}°C", self.0))
    }
}

impl fmt::Display for Ratio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&format!("{:.1}%", self.as_percent()))
    }
}

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0 as f64;
        let s = if self.0 >= Self::GIB {
            format!("{:.1}G", b / Self::GIB as f64)
        } else if self.0 >= Self::MIB {
            format!("{:.0}M", b / Self::MIB as f64)
        } else if self.0 >= Self::KIB {
            format!("{:.0}K", b / Self::KIB as f64)
        } else {
            format!("{}B", self.0)
        };
        f.pad(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_display_formats() {
        assert_eq!(Watts(5.234).to_string(), "5.23W");
        assert_eq!(Watts(28.91).to_string(), "28.9W");
        assert_eq!(Watts(0.18).to_string(), "180mW");
        assert_eq!(Watts(0.0084).to_string(), "8mW");
        assert_eq!(Watts(0.9999).to_string(), "1.00W");
        assert_eq!(Watts(1.5).to_string(), "1.50W");
        assert_eq!(Mhz(618).to_string(), "618MHz");
        assert_eq!(Mhz(3152).to_string(), "3.15GHz");
        assert_eq!(Celsius(83.4).to_string(), "83°C");
        assert_eq!(Ratio(0.503).to_string(), "50.3%");
        assert_eq!(Bytes(912 * 1024).to_string(), "912K");
        assert_eq!(Bytes(13_900_000_000).to_string(), "12.9G");
        assert_eq!(Bytes(120).to_string(), "120B");
    }

    #[test]
    fn unit_display_honors_width() {
        // Panels rely on `{:>N}` padding for jitter-free columns — the Display
        // impls must route through Formatter::pad, not raw write!.
        assert_eq!(format!("{:>6}", Watts(0.18)), " 180mW");
        assert_eq!(format!("{:>7}", Mhz(748)), " 748MHz");
        assert_eq!(format!("{:>7}", Mhz(1770)), "1.77GHz");
        assert_eq!(format!("{:>4}", Celsius(83.4)), "83°C");
        assert_eq!(format!("{:>7}", Ratio(0.503)), "  50.3%");
        assert_eq!(format!("{:>5}", Bytes(45 * Bytes::MIB)), "  45M");
        assert_eq!(format!("{:<5}|", Bytes(120)), "120B |");
    }

    mod prop {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            // Display is total — non-finite telemetry glitches must format,
            // never panic, and integer-unit widths stay column-friendly.
            #[test]
            fn displays_never_panic(v in proptest::num::f32::ANY, u in any::<u64>(), m in any::<u32>()) {
                let _ = Watts(v).to_string();
                let _ = Celsius(v).to_string();
                let _ = Ratio(v).to_string();
                prop_assert!(!Bytes(u).to_string().is_empty());
                prop_assert!(Mhz(m).to_string().len() <= 13);
            }
        }
    }
}
