//! `mxmon schema`: the v1 report JSON Schema, derived from the same DTOs the
//! snapshot serializes (schemars), so the schema can never drift from the data.
//! A golden file pins it; a change becomes a reviewable diff and a prompt to
//! bump [`super::SCHEMA_VERSION`] when the change is breaking.

use super::Report;

/// The v1 report schema as pretty JSON. Every field's doc comment surfaces as
/// its property `description`, so the schema is self-documenting.
pub fn json_schema() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(Report)).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::json_schema;

    #[test]
    fn schema_matches_golden() {
        let current = json_schema();
        if std::env::var_os("MXMON_BLESS").is_some() {
            std::fs::write(
                concat!(env!("CARGO_MANIFEST_DIR"), "/src/report/schema.golden.json"),
                format!("{}\n", current.trim_end()),
            )
            .expect("write golden schema");
            return;
        }
        assert_eq!(
            current.trim(),
            include_str!("schema.golden.json").trim(),
            "Report schema changed. If intended, review the contract impact, bump \
             report::SCHEMA_VERSION on a breaking change, then re-bless with \
             `MXMON_BLESS=1 cargo test schema_matches_golden`."
        );
    }

    #[test]
    fn schema_version_is_current() {
        assert_eq!(super::super::SCHEMA_VERSION, 1);
    }
}
