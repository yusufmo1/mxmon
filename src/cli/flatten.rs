//! Flattening a JSON tree into dotted paths, for the `compact` and `table`
//! output shapes.
//!
//! Two walkers, one idiom. [`value`] flattens a *report* into `path=value`
//! lines; [`schema`] flattens the *JSON Schema* of that report into
//! `path / type / description` rows. Both emit paths in exactly the dialect
//! [`crate::report::select`] parses, so every line of either output names a
//! path you can hand straight back to `mxmon get`.
//!
//! That reciprocity is the point: an agent runs `mxmon schema --format compact`
//! once to learn the shape, then queries leaves by name, and never has to
//! reconcile two spellings of the same address.

use std::collections::BTreeSet;

use serde_json::Value;

// ---- report values -------------------------------------------------------

/// Flatten a value into `(path, rendered)` pairs, depth-first.
///
/// Scalars render bare (a string without quotes, a number in its shortest
/// round-tripping form). Containers recurse; an *empty* container still emits
/// one row, because the v1 contract never omits a key and `compact` must not
/// either.
pub fn value(root: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_value(String::new(), root, &mut out);
    out
}

fn walk_value(path: String, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(map) if !map.is_empty() => {
            for (k, child) in map {
                let next = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk_value(next, child, out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (i, child) in items.iter().enumerate() {
                walk_value(format!("{path}[{i}]"), child, out);
            }
        }
        // Leaves, plus empty containers (which have no children to stand in
        // for them and would otherwise silently vanish from the dump).
        _ => out.push((path, scalar(v))),
    }
}

/// Render one scalar for a `key=value` line.
///
/// Strings are bare so `mxmon get soc.chip` and a compact dump agree
/// character for character. A string carrying a control byte would break the
/// one-pair-per-line frame, so that case alone falls back to JSON quoting.
fn scalar(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::String(s) => {
            if s.chars().any(char::is_control) {
                v.to_string()
            } else {
                s.clone()
            }
        }
        Value::Array(_) => "[]".to_owned(),
        Value::Object(_) => "{}".to_owned(),
        // serde_json renders floats through ryu: the shortest form that reads
        // back identically, so no 1.0000000000000002 reaches the output.
        _ => v.to_string(),
    }
}

// ---- schema --------------------------------------------------------------

/// One row of the flattened schema.
pub struct Field {
    pub path: String,
    pub ty: String,
    pub description: String,
}

/// Flatten a schemars-produced JSON Schema into one row per addressable path.
///
/// Resolves `$ref` through `$defs`, unwraps the `anyOf: [T, null]` shape
/// schemars emits for an `Option<T>` into a `?` suffix, and collapses an array
/// of scalars into a single `T[]` row (an array of objects keeps its `[]`
/// segment and recurses, since its fields are addressable).
pub fn schema(root: &Value) -> Vec<Field> {
    let defs = root.get("$defs").and_then(Value::as_object);
    let mut out = Vec::new();
    if let Some(props) = root.get("properties").and_then(Value::as_object) {
        for (k, child) in props {
            walk_schema(k, child, defs, &mut BTreeSet::new(), &mut out);
        }
    }
    out
}

/// Follow a `$ref` into `$defs`. Returns the target plus the name it was
/// reached by, so the caller can guard against a recursive definition.
fn deref<'a>(
    s: &'a Value,
    defs: Option<&'a serde_json::Map<String, Value>>,
) -> Option<(&'a Value, String)> {
    let r = s.get("$ref")?.as_str()?;
    let name = r.rsplit('/').next()?;
    let target = defs?.get(name)?;
    Some((target, name.to_owned()))
}

/// Strip an `Option<T>` wrapper. schemars emits `anyOf: [T, {"type":"null"}]`,
/// so an `anyOf` whose only non-null arm is a single schema is that schema,
/// made nullable.
fn unwrap_nullable(s: &Value) -> Option<&Value> {
    let arms = s.get("anyOf")?.as_array()?;
    let mut real = arms
        .iter()
        .filter(|a| a.get("type").and_then(Value::as_str) != Some("null"));
    let first = real.next()?;
    real.next().is_none().then_some(first)
}

/// The declared type of a schema node as a display string, and whether it is
/// nullable. `type` is either a bare string or a `["number", "null"]` union.
fn type_of(s: &Value) -> (String, bool) {
    match s.get("type") {
        Some(Value::String(t)) => (t.clone(), false),
        Some(Value::Array(ts)) => {
            let nullable = ts.iter().any(|t| t.as_str() == Some("null"));
            let named = ts
                .iter()
                .filter_map(Value::as_str)
                .find(|t| *t != "null")
                .unwrap_or("any");
            (named.to_owned(), nullable)
        }
        _ => ("any".to_owned(), false),
    }
}

fn walk_schema(
    path: &str,
    node: &Value,
    defs: Option<&serde_json::Map<String, Value>>,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<Field>,
) {
    // Peel Option<T>, then $ref, recording nullability as we go.
    let mut target = node;
    let mut nullable = false;
    if let Some(inner) = unwrap_nullable(target) {
        target = inner;
        nullable = true;
    }
    let mut def_name = None;
    if let Some((resolved, name)) = deref(target, defs) {
        target = resolved;
        def_name = Some(name);
    }

    // A description on the property wins over the one on the type it points at
    // ("null if the fast tier did not settle" says more here than the struct's
    // own summary), but an array's items node carries no description of its
    // own, so falling back to the definition is what keeps rows like
    // `processes.top[]` documented.
    let description = node
        .get("description")
        .or_else(|| target.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let (ty, ty_nullable) = type_of(target);
    let nullable = nullable || ty_nullable;
    let suffix = if nullable { "?" } else { "" };

    let describe = |t: &str| Field {
        path: path.to_owned(),
        ty: format!("{t}{suffix}"),
        description: description.clone(),
    };

    match ty.as_str() {
        "object" => {
            out.push(describe("object"));
            // A definition that reaches itself would recurse forever. The v1
            // model is a tree, so this only ever fires if the contract grows a
            // cycle; better a truncated row than a hung command.
            if let Some(name) = &def_name
                && !seen.insert(name.clone())
            {
                return;
            }
            if let Some(props) = target.get("properties").and_then(Value::as_object) {
                for (k, child) in props {
                    walk_schema(&format!("{path}.{k}"), child, defs, seen, out);
                }
            }
            if let Some(name) = &def_name {
                seen.remove(name);
            }
        }
        "array" => {
            let items = target.get("items");
            // An array of scalars is one fact, not two rows: `integer[]`.
            let scalar_items = items.is_some_and(|it| {
                unwrap_nullable(it).unwrap_or(it).get("$ref").is_none()
                    && !matches!(type_of(it).0.as_str(), "object" | "array")
            });
            if let Some(it) = items
                && scalar_items
            {
                let (item_ty, item_nullable) = type_of(it);
                let item_suffix = if item_nullable { "?" } else { "" };
                out.push(describe(&format!("{item_ty}{item_suffix}[]")));
            } else {
                out.push(describe("array"));
                if let Some(it) = items {
                    walk_schema(&format!("{path}[]"), it, defs, seen, out);
                }
            }
        }
        other => out.push(describe(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pairs(v: &Value) -> Vec<String> {
        value(v)
            .into_iter()
            .map(|(k, val)| format!("{k}={val}"))
            .collect()
    }

    #[test]
    fn flattens_leaves_with_the_selector_dialect() {
        let v = json!({
            "power": {"cpu_w": 4.25, "ecpu": {"cores": [{"freq_mhz": 1901}, {"freq_mhz": 2004}]}},
            "thermal": null,
        });
        assert_eq!(
            pairs(&v),
            [
                "power.cpu_w=4.25",
                "power.ecpu.cores[0].freq_mhz=1901",
                "power.ecpu.cores[1].freq_mhz=2004",
                "thermal=null",
            ]
        );
    }

    #[test]
    fn every_emitted_path_round_trips_through_the_selector() {
        use crate::report::select;
        let v = json!({"a": {"b": [{"c": 1}]}, "d": "x", "e": null});
        for (path, _) in value(&v) {
            let segs = select::parse_path(&path).expect("emitted path must parse");
            assert!(
                select::resolve(&v, &segs).is_ok(),
                "compact emitted {path:?}, which `get` cannot resolve"
            );
        }
    }

    #[test]
    fn empty_containers_still_claim_a_line() {
        // The contract never omits a key; a group that happens to be empty
        // must not disappear from a compact dump either.
        assert_eq!(
            pairs(&json!({"errs": [], "opts": {}})),
            ["errs=[]", "opts={}"]
        );
    }

    #[test]
    fn strings_are_bare_unless_they_would_break_the_frame() {
        assert_eq!(
            pairs(&json!({"chip": "Apple M3 Max"})),
            ["chip=Apple M3 Max"]
        );
        assert_eq!(pairs(&json!({"odd": "a\nb"})), [r#"odd="a\nb""#]);
    }

    fn demo_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "cpu": {
                    "description": "CPU state; null if the tier did not settle.",
                    "anyOf": [{"$ref": "#/$defs/Cpu"}, {"type": "null"}]
                },
                "soc": {"$ref": "#/$defs/Soc"}
            },
            "$defs": {
                "Cpu": {
                    "type": "object",
                    "properties": {
                        "used_ratio": {"description": "Busy share, 0..1.", "type": "number"},
                        "clusters": {
                            "description": "Per cluster.",
                            "type": "array",
                            "items": {"$ref": "#/$defs/Cluster"}
                        }
                    }
                },
                "Cluster": {
                    "type": "object",
                    "properties": {"freq_mhz": {"description": "Whole MHz.", "type": "integer"}}
                },
                "Soc": {
                    "type": "object",
                    "properties": {
                        "chip": {"description": "Marketing name.", "type": "string"},
                        "gpu_cores": {"description": "GPU cores.", "type": ["integer", "null"]},
                        "freqs_mhz": {
                            "description": "DVFS table.",
                            "type": "array",
                            "items": {"type": "integer"}
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn schema_walk_resolves_refs_options_and_arrays() {
        let rows: Vec<String> = schema(&demo_schema())
            .into_iter()
            .map(|f| format!("{} {}", f.path, f.ty))
            .collect();
        assert_eq!(
            rows,
            [
                "cpu object?",           // anyOf [T, null] unwrapped to a `?`
                "cpu.clusters array",    // array of objects keeps its segment
                "cpu.clusters[] object", // the element is addressable too
                "cpu.clusters[].freq_mhz integer",
                "cpu.used_ratio number",
                "soc object",
                "soc.chip string",
                "soc.freqs_mhz integer[]", // array of scalars collapses to one row
                "soc.gpu_cores integer?",  // ["integer","null"] union
            ]
        );
    }

    #[test]
    fn schema_walk_prefers_the_property_description() {
        let rows = schema(&demo_schema());
        let cpu = rows.iter().find(|f| f.path == "cpu").unwrap();
        assert_eq!(
            cpu.description,
            "CPU state; null if the tier did not settle."
        );
    }

    #[test]
    fn schema_walk_survives_a_self_referential_definition() {
        let cyclic = json!({
            "type": "object",
            "properties": {"node": {"$ref": "#/$defs/Node"}},
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {"child": {"$ref": "#/$defs/Node"}}
                }
            }
        });
        // Terminates, and says so rather than hanging.
        let rows = schema(&cyclic);
        assert!(rows.len() < 10, "cycle guard should stop the descent");
    }

    #[test]
    fn the_real_schema_flattens_and_documents_every_row() {
        let s: Value = serde_json::from_str(&crate::report::schema::json_schema()).unwrap();
        let rows = schema(&s);
        assert!(
            rows.len() > 200,
            "expected the full contract, got {}",
            rows.len()
        );
        let undocumented: Vec<&str> = rows
            .iter()
            .filter(|f| f.description.is_empty())
            .map(|f| f.path.as_str())
            .collect();
        assert!(
            undocumented.is_empty(),
            "undocumented schema paths: {undocumented:?}"
        );
    }

    mod prop {
        use super::super::{schema, value};
        use proptest::prelude::*;

        /// An arbitrary JSON value, nested a few levels deep.
        fn any_json() -> impl Strategy<Value = serde_json::Value> {
            let leaf = prop_oneof![
                Just(serde_json::Value::Null),
                any::<bool>().prop_map(serde_json::Value::from),
                any::<i64>().prop_map(serde_json::Value::from),
                ".*".prop_map(serde_json::Value::from),
            ];
            leaf.prop_recursive(4, 32, 4, |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
                    prop::collection::hash_map(".*", inner, 0..4)
                        .prop_map(|m| { serde_json::Value::Object(m.into_iter().collect()) }),
                ]
            })
        }

        proptest! {
            #[test]
            fn walkers_never_panic(v in any_json()) {
                let _ = value(&v);
                let _ = schema(&v);
            }
        }
    }
}
