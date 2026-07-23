//! Dot-path selection over a serialized report value: powers `get`, `--only`,
//! and the `check` operand resolver. Working on the JSON value (not the typed
//! tree) keeps `mxmon get x.y` byte-identical to `jq .x.y` and needs no
//! per-field wiring when the contract grows.

use serde_json::Value;

/// One path segment: an object key or an array index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// Parse a dot-path. Bracket indices (`a.b[0]`) and bare numeric segments
/// (`a.b.0`) both address arrays, so `power.ecpu.cores[0]` and
/// `power.ecpu.cores.0` are equivalent.
pub fn parse_path(path: &str) -> Result<Vec<Seg>, String> {
    let normalized = path.replace('[', ".").replace(']', "");
    let mut segs = Vec::new();
    for part in normalized.split('.') {
        if part.is_empty() {
            continue;
        }
        if let Ok(i) = part.parse::<usize>() {
            segs.push(Seg::Index(i));
        } else {
            segs.push(Seg::Key(part.to_owned()));
        }
    }
    if segs.is_empty() {
        return Err(format!("empty path {path:?}"));
    }
    Ok(segs)
}

/// Resolve a path against a value. Descending into `null` yields `null` (the
/// metric is legitimately unavailable); a segment naming a field or index not
/// present in the schema is an error.
pub fn resolve<'a>(root: &'a Value, segs: &[Seg]) -> Result<&'a Value, String> {
    let mut cur = root;
    for (depth, seg) in segs.iter().enumerate() {
        if cur.is_null() {
            return Ok(cur);
        }
        let next = match seg {
            Seg::Key(k) => cur.get(k.as_str()),
            Seg::Index(i) => cur.get(*i),
        };
        cur = next.ok_or_else(|| {
            let at = render_prefix(&segs[..=depth]);
            match seg {
                Seg::Key(k) => format!("no field {k:?} at {at}"),
                Seg::Index(i) => format!("index {i} out of bounds at {at}"),
            }
        })?;
    }
    Ok(cur)
}

fn render_prefix(segs: &[Seg]) -> String {
    let mut s = String::new();
    for seg in segs {
        match seg {
            Seg::Key(k) => {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(k);
            }
            Seg::Index(i) => {
                s.push('[');
                s.push_str(&i.to_string());
                s.push(']');
            }
        }
    }
    s
}

/// Project a report value to the named top-level groups, always keeping `meta`.
pub fn only(root: &Value, groups: &[String]) -> Result<Value, String> {
    let obj = root.as_object().ok_or("report is not an object")?;
    let mut out = serde_json::Map::new();
    if let Some(m) = obj.get("meta") {
        out.insert("meta".to_owned(), m.clone());
    }
    for g in groups {
        let key = g.trim();
        if key == "meta" {
            continue;
        }
        let Some(v) = obj.get(key) else {
            let valid: Vec<&str> = obj.keys().map(String::as_str).collect();
            return Err(format!("unknown group {key:?}; valid: {}", valid.join(", ")));
        };
        out.insert(key.to_owned(), v.clone());
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "meta": {"schema_version": 1},
            "power": {"cpu_w": 4.2, "ecpu": {"cores": [{"freq_mhz": 1901}]}},
            "thermal": null,
        })
    }

    #[test]
    fn resolves_keys_and_indices() {
        let v = sample();
        assert_eq!(resolve(&v, &parse_path("power.cpu_w").unwrap()).unwrap(), &json!(4.2));
        assert_eq!(
            resolve(&v, &parse_path("power.ecpu.cores[0].freq_mhz").unwrap()).unwrap(),
            &json!(1901)
        );
        assert_eq!(
            resolve(&v, &parse_path("power.ecpu.cores.0.freq_mhz").unwrap()).unwrap(),
            &json!(1901)
        );
    }

    #[test]
    fn null_propagates_but_bad_field_errors() {
        let v = sample();
        assert!(resolve(&v, &parse_path("thermal.cpu_max_c").unwrap()).unwrap().is_null());
        assert!(resolve(&v, &parse_path("power.bogus").unwrap()).is_err());
        assert!(resolve(&v, &parse_path("power.ecpu.cores[9]").unwrap()).is_err());
    }

    #[test]
    fn only_keeps_meta_and_named_groups() {
        let v = sample();
        let out = only(&v, &["power".to_owned()]).unwrap();
        let obj = out.as_object().unwrap();
        assert!(obj.contains_key("meta") && obj.contains_key("power"));
        assert!(!obj.contains_key("thermal"));
        assert!(only(&v, &["nope".to_owned()]).is_err());
    }

    mod prop {
        use super::super::{parse_path, resolve};
        use proptest::prelude::*;
        use serde_json::json;

        proptest! {
            #[test]
            fn parse_and_resolve_never_panic(p in ".*") {
                if let Ok(segs) = parse_path(&p) {
                    let v = json!({"a": {"b": [1, 2, {"c": null}]}});
                    let _ = resolve(&v, &segs);
                }
            }
        }
    }
}
