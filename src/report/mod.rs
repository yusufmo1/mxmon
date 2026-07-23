//! The agent-facing v1 report: a stable, self-describing, consistently-unitted
//! contract over every metric, decoupled from the internal collector structs.
//!
//! - [`model`] is the type tree and states the two contract invariants (keys
//!   are never omitted; every unit is spelled in its key).
//! - [`norm`] is the single place a typed unit becomes a JSON scalar.
//! - [`build`] converts the latest per-tier samples into a [`Report`].
//! - [`schema`] pins the derived JSON Schema against drift.

mod build;
pub mod model;
mod norm;
pub mod check;
pub mod explain;
pub mod health;
pub mod schema;
pub mod select;

pub use build::Inputs;
pub use model::Report;

/// The report contract version. Bump on any breaking schema change (a removed
/// or renamed field, or a tightened type). The golden-schema drift test flags
/// the change, and [`model::Meta::schema_version`] carries this at runtime.
pub const SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::soc::SocInfo;

    /// The strengthened SourceDown contract: with every source down, each
    /// top-level domain key is still present and `null` (never omitted), while
    /// `meta`, `soc`, and `source_errors` are always populated. Guards against
    /// a stray `skip_serializing_if` slipping into the model.
    #[test]
    fn all_sources_down_still_carries_every_key() {
        let soc = SocInfo::default();
        let inputs = Inputs {
            soc: &soc,
            fast: None,
            power: None,
            temps: None,
            battery: None,
            procs: None,
            flows: None,
            ping: None,
            storage: None,
            kernel: None,
            errors: &[],
            fast_ms: 250,
            ping_on: false,
            storage_health_on: false,
            kernel_stats_on: false,
            settled: false,
        };
        let value = serde_json::to_value(Report::build(&inputs)).unwrap();
        let obj = value.as_object().expect("report is a JSON object");
        for key in [
            "cpu", "gpu", "memory", "power", "thermal", "network", "disk", "storage", "battery",
            "processes", "flows", "kernel", "ping",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
            assert!(obj[key].is_null(), "{key} must be null when its source is down");
        }
        assert!(!obj["meta"].is_null());
        assert!(!obj["soc"].is_null());
        assert!(obj["source_errors"].is_array());
        assert_eq!(obj["meta"]["schema_version"], 1);
    }
}
