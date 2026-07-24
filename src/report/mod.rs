//! The agent-facing v1 report: a stable, self-describing, consistently-unitted
//! contract over every metric, decoupled from the internal collector structs.
//!
//! - [`model`] is the type tree and states the two contract invariants (keys
//!   are never omitted; every unit is spelled in its key).
//! - [`norm`] is the single place a typed unit becomes a JSON scalar.
//! - [`build`] converts the latest per-tier samples into a [`Report`].
//! - [`schema`] pins the derived JSON Schema against drift.

mod build;
pub mod check;
pub mod explain;
pub mod health;
pub mod model;
mod norm;
pub mod schema;
pub mod select;

pub use build::Inputs;
pub use model::Report;

/// The report contract version. Bump on any breaking schema change (a removed
/// or renamed field, or a tightened type). The golden-schema drift test flags
/// the change, and [`model::Meta::schema_version`] carries this at runtime.
pub const SCHEMA_VERSION: u32 = 1;

/// A fully-populated report, built through the real [`Report::build`] from the
/// shared deterministic samples in [`crate::testutil`].
///
/// Every domain is live, so the consumers of a report (`health`, `explain`, the
/// headless renderers) can be tested against the same shape the binary emits,
/// rather than against hand-written JSON that could drift from the model.
#[cfg(test)]
pub fn populated() -> Report {
    use crate::testutil;
    let soc = testutil::soc();
    let fast = testutil::fast_at(1);
    let power = testutil::power_at(1);
    let temps = testutil::temps_at(1);
    let battery = testutil::battery();
    let procs = testutil::procs(8);
    let flows = testutil::flows();
    let ping = testutil::ping_at(1);
    let storage = testutil::storage();
    let kernel = testutil::kernel();
    Report::build(&Inputs {
        soc: &soc,
        fast: Some(&fast),
        power: Some(&power),
        temps: Some(&temps),
        battery: Some(&battery),
        procs: Some(&procs),
        flows: Some(&flows),
        ping: Some(&ping),
        storage: Some(&storage),
        kernel: Some(&kernel),
        errors: &[],
        fast_ms: 250,
        ping_on: true,
        storage_health_on: true,
        kernel_stats_on: true,
        settled: true,
    })
}

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
            "cpu",
            "gpu",
            "memory",
            "power",
            "thermal",
            "network",
            "disk",
            "storage",
            "battery",
            "processes",
            "flows",
            "kernel",
            "ping",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
            assert!(
                obj[key].is_null(),
                "{key} must be null when its source is down"
            );
        }
        assert!(!obj["meta"].is_null());
        assert!(!obj["soc"].is_null());
        assert!(obj["source_errors"].is_array());
        assert_eq!(obj["meta"]["schema_version"], 1);
    }
}
