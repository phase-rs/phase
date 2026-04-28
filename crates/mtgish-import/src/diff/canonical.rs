//! Canonical-form normalizer for `serde_json::Value`.
//!
//! The native parser and mtgish converter both serialize via `serde`, but
//! their outputs differ in incidental ways:
//! - Field order in objects (HashMap-derived inserts can vary).
//! - `skip_serializing_if = "Vec::is_empty"` on some fields means an empty
//!   vec round-trips as "absent" on one side and "present and empty" on
//!   the other.
//! - Defaults (`#[serde(default)]`) similarly come and go.
//!
//! `canonicalize` reduces a `Value` to the smallest equivalent form so the
//! classifier compares only meaningful content:
//! - Object keys sorted (`BTreeMap`).
//! - Default-valued fields stripped (`null`, empty `""`, empty `[]`,
//!   empty `{}`, `false`).
//! - List ordering preserved — the diff layer applies set-vs-positional
//!   semantics from `ORDERING_MANIFEST`, so the canonicalizer must not
//!   pre-sort lists.
//!
//! Numbers are passed through verbatim. `0` is *not* stripped because
//! some engine fields (`u32`, `usize`) carry semantic zero.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// Reduce a `serde_json::Value` to canonical form for structural comparison.
///
/// See module-level docs for the normalization rules.
pub fn canonicalize(value: Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(_) | Value::Number(_) | Value::String(_) => value,
        Value::Array(items) => {
            let canonical: Vec<Value> = items.into_iter().map(canonicalize).collect();
            Value::Array(canonical)
        }
        Value::Object(map) => canonicalize_object(map),
    }
}

fn canonicalize_object(map: Map<String, Value>) -> Value {
    // BTreeMap → deterministic key ordering. See diff/mod.rs for why we
    // refuse HashMap throughout the diff path.
    let mut sorted: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in map {
        let canon = canonicalize(v);
        if is_default(&canon) {
            continue;
        }
        sorted.insert(k, canon);
    }
    // Re-emit as a serde_json::Map; serde_json::Map preserves insertion
    // order, and we insert in BTreeMap key order, so the resulting object
    // is canonically ordered.
    let mut out = Map::with_capacity(sorted.len());
    for (k, v) in sorted {
        out.insert(k, v);
    }
    Value::Object(out)
}

/// Whether a value is a serde-equivalent default that should be stripped.
fn is_default(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::String(s) => s.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Number(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_null_empty_string_empty_vec_empty_object_false() {
        let input = json!({
            "keep": "v",
            "null_field": null,
            "empty_string": "",
            "empty_vec": [],
            "empty_obj": {},
            "false_flag": false,
            "true_flag": true,
            "zero": 0,
        });
        let out = canonicalize(input);
        assert_eq!(out, json!({"keep": "v", "true_flag": true, "zero": 0}));
    }

    #[test]
    fn sorts_object_keys() {
        let input = json!({"z": 1, "a": 2, "m": 3});
        let out = canonicalize(input);
        // serde_json::Map preserves insertion order; iterate to verify.
        let obj = out.as_object().unwrap();
        let keys: Vec<&String> = obj.keys().collect();
        assert_eq!(keys, vec!["a", "m", "z"]);
    }

    #[test]
    fn preserves_array_order() {
        let input = json!([3, 1, 2]);
        let out = canonicalize(input);
        assert_eq!(out, json!([3, 1, 2]));
    }

    #[test]
    fn recurses_into_nested_objects_and_arrays() {
        let input = json!({
            "outer": {
                "inner_empty": [],
                "inner_keep": {"b": 1, "a": 2}
            },
            "list": [{"y": 0, "x": null, "z": "v"}]
        });
        let out = canonicalize(input);
        assert_eq!(
            out,
            json!({
                "list": [{"y": 0, "z": "v"}],
                "outer": {"inner_keep": {"a": 2, "b": 1}}
            })
        );
    }

    #[test]
    fn idempotent() {
        let input = json!({"a": [1, {"b": false, "c": "x"}], "d": null});
        let once = canonicalize(input.clone());
        let twice = canonicalize(once.clone());
        assert_eq!(once, twice);
    }
}
