//! Per-divergence classification.
//!
//! The classifier walks two canonical-JSON values in lockstep, recursing
//! into objects (key by key) and arrays (per the manifest's ordering
//! class), and emits a `Divergence` for each mismatch. Each divergence
//! carries a path (`abilities[0].triggers[0].condition`), the two raw
//! values, and a `Severity` indicating how much the parsers disagree.
//!
//! ## Severity model
//! - **`SemanticDivergence`** — Different discriminant tags. The parsers
//!   disagree about *what the rule does*. Highest priority for review.
//!   Detected by comparing `type` keys on tagged-union JSON objects.
//! - **`ScopeDivergence`** — Same discriminant, different content in a
//!   `TargetFilter` / `QuantityRef` / quantity / counter sub-tree. The
//!   rule has the right shape but the wrong scope (e.g., "creature" vs
//!   "creature you control").
//! - **`Cosmetic`** — Same shape and scope but a `description` /
//!   `target_prompt` / `parse_warnings` string differs, or some other
//!   non-rules-meaningful field.
//!
//! The classifier walks but does not panic on unexpected shapes — if it
//! can't decide, it emits `Severity::Cosmetic` with the raw values so a
//! reviewer can decide. Crashing the diff binary is worse than reporting
//! an under-classified divergence.

use std::collections::BTreeSet;

use serde_json::Value;

use super::ordering::{lookup_ordering, OrderingClass};

/// Classification of a single divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Severity {
    /// Different discriminant tag — parsers disagree about what the rule does.
    SemanticDivergence,
    /// Same discriminant, different scope (TargetFilter / QuantityRef contents).
    ScopeDivergence,
    /// Same shape & scope, but description / label differs.
    Cosmetic,
}

/// One structural divergence between two canonical JSON values.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Divergence {
    pub path: String,
    pub severity: Severity,
    pub native: Value,
    pub mtgish: Value,
}

/// Walk two canonical-JSON values and emit a list of divergences.
///
/// The `carrier_hint` argument names the parent struct/enum the current
/// value lives in, used to look up array ordering classes from the
/// manifest. Pass `""` for the top-level call; it propagates down the
/// recursion.
///
/// This is the public entry point used by the `mtgish-diff` binary.
pub fn classify_value(native: &Value, mtgish: &Value) -> Vec<Divergence> {
    let mut out = Vec::new();
    walk(native, mtgish, "", "", &mut out);
    out
}

fn walk(native: &Value, mtgish: &Value, path: &str, carrier_hint: &str, out: &mut Vec<Divergence>) {
    if native == mtgish {
        return;
    }

    match (native, mtgish) {
        (Value::Object(n), Value::Object(m)) => walk_object(n, m, path, carrier_hint, out),
        (Value::Array(n), Value::Array(m)) => walk_array(n, m, path, carrier_hint, out),
        _ => {
            // Primitives or shape-mismatched roots. Severity is decided by
            // the kind of the difference.
            out.push(Divergence {
                path: path.to_string(),
                severity: classify_primitive_diff(path, native, mtgish),
                native: native.clone(),
                mtgish: mtgish.clone(),
            });
        }
    }
}

fn walk_object(
    native: &serde_json::Map<String, Value>,
    mtgish: &serde_json::Map<String, Value>,
    path: &str,
    _carrier_hint: &str,
    out: &mut Vec<Divergence>,
) {
    // CR-style discriminated unions are emitted with `#[serde(tag = "type")]`
    // and sometimes `content = "data"`. A mismatch on `type` means the two
    // parsers chose different variants — that's a semantic divergence and
    // we don't recurse into the content (the children would all be noise).
    if let (Some(Value::String(nt)), Some(Value::String(mt))) =
        (native.get("type"), mtgish.get("type"))
    {
        if nt != mt {
            out.push(Divergence {
                path: path.to_string(),
                severity: Severity::SemanticDivergence,
                native: Value::Object(native.clone()),
                mtgish: Value::Object(mtgish.clone()),
            });
            return;
        }
    }

    // The carrier hint at the next level is best derived from the JSON
    // tag if present, else falls through. This lets the manifest lookup
    // resolve `(TargetFilter, filters)` even though the JSON only carries
    // a `type: "Or"` discriminator.
    let next_carrier = native
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut keys: BTreeSet<&str> = BTreeSet::new();
    keys.extend(native.keys().map(String::as_str));
    keys.extend(mtgish.keys().map(String::as_str));

    for key in keys {
        let n = native.get(key).cloned().unwrap_or(Value::Null);
        let m = mtgish.get(key).cloned().unwrap_or(Value::Null);
        if n == m {
            continue;
        }
        let child_path = if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        };
        walk(&n, &m, &child_path, &next_carrier, out);
    }
}

fn walk_array(
    native: &[Value],
    mtgish: &[Value],
    path: &str,
    carrier_hint: &str,
    out: &mut Vec<Divergence>,
) {
    // Last `.<field>` segment of the path is the field name; pair it
    // with the carrier hint to look up ordering class.
    let field_name = path.rsplit('.').next().unwrap_or("");
    let class =
        lookup_ordering(carrier_hint, field_name).unwrap_or(OrderingClass::OrderSignificant);

    match class {
        OrderingClass::OrderSignificant | OrderingClass::ConditionallySignificant => {
            let max = native.len().max(mtgish.len());
            for i in 0..max {
                let n = native.get(i).cloned().unwrap_or(Value::Null);
                let m = mtgish.get(i).cloned().unwrap_or(Value::Null);
                if n == m {
                    continue;
                }
                let child_path = format!("{path}[{i}]");
                walk(&n, &m, &child_path, carrier_hint, out);
            }
        }
        OrderingClass::SetEquivalent => walk_array_as_set(native, mtgish, path, out),
    }
}

/// Multiset comparison: for each side, anything not present on the other
/// is recorded as a divergence. Element identity is structural-equality
/// of the canonical form. We do not attempt to pair "near-misses" — the
/// classifier is a measurement tool, not a fixer.
fn walk_array_as_set(native: &[Value], mtgish: &[Value], path: &str, out: &mut Vec<Divergence>) {
    let mut native_only: Vec<&Value> = Vec::new();
    let mut mtgish_remaining: Vec<&Value> = mtgish.iter().collect();

    for n in native {
        if let Some(pos) = mtgish_remaining.iter().position(|m| *m == n) {
            mtgish_remaining.remove(pos);
        } else {
            native_only.push(n);
        }
    }

    for (i, n) in native_only.iter().enumerate() {
        out.push(Divergence {
            path: format!("{path}[{{native-only #{i}}}]"),
            severity: Severity::SemanticDivergence,
            native: (*n).clone(),
            mtgish: Value::Null,
        });
    }
    for (i, m) in mtgish_remaining.iter().enumerate() {
        out.push(Divergence {
            path: format!("{path}[{{mtgish-only #{i}}}]"),
            severity: Severity::SemanticDivergence,
            native: Value::Null,
            mtgish: (*m).clone(),
        });
    }
}

/// Severity of a primitive-or-shape-mismatched divergence.
fn classify_primitive_diff(path: &str, native: &Value, mtgish: &Value) -> Severity {
    // Cosmetic-fields list. These carry display strings, not rules content.
    // TODO(classify): expand this list as more cosmetic field names are
    // identified during real-world diff runs.
    const COSMETIC_FIELDS: &[&str] = &["description", "target_prompt", "parse_warnings"];
    let leaf = path.rsplit('.').next().unwrap_or("");
    // Strip trailing `[N]` indexing if present.
    let leaf = leaf.split('[').next().unwrap_or(leaf);
    if COSMETIC_FIELDS.contains(&leaf) {
        return Severity::Cosmetic;
    }
    // Different primitive types (e.g., string vs number) → semantic.
    if std::mem::discriminant(native) != std::mem::discriminant(mtgish) {
        return Severity::SemanticDivergence;
    }
    // Same primitive type, different value → scope (a number, filter
    // value, etc. that affects rules behavior).
    Severity::ScopeDivergence
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn equal_values_produce_no_divergences() {
        let v = json!({"a": 1, "b": [1, 2, 3]});
        assert!(classify_value(&v, &v).is_empty());
    }

    #[test]
    fn type_tag_mismatch_is_semantic() {
        let n = json!({"type": "DealDamage", "amount": 3});
        let m = json!({"type": "Counter", "amount": 3});
        let divs = classify_value(&n, &m);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].severity, Severity::SemanticDivergence);
    }

    #[test]
    fn cosmetic_field_difference_is_cosmetic() {
        let n = json!({"description": "draws a card"});
        let m = json!({"description": "Draw a card"});
        let divs = classify_value(&n, &m);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].severity, Severity::Cosmetic);
        assert_eq!(divs[0].path, "description");
    }

    #[test]
    fn primitive_value_difference_is_scope() {
        let n = json!({"amount": 3});
        let m = json!({"amount": 5});
        let divs = classify_value(&n, &m);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].severity, Severity::ScopeDivergence);
    }

    #[test]
    fn set_equivalent_array_reorder_is_clean() {
        // TargetFilter::Or filters → SetEquivalent.
        let n = json!({"type": "Or", "filters": [1, 2, 3]});
        let m = json!({"type": "Or", "filters": [3, 1, 2]});
        let divs = classify_value(&n, &m);
        assert!(
            divs.is_empty(),
            "set-equivalent reorder should produce no divergences, got {divs:?}"
        );
    }

    #[test]
    fn order_significant_array_reorder_diverges() {
        // AbilityDefinition::mode_abilities → OrderSignificant.
        // Wrap so the carrier hint resolves: tag="" outer, but use the
        // direct lookup path. Use a synthetic structure simulating the
        // engine emit.
        let n = json!({"mode_abilities": [{"k": "a"}, {"k": "b"}]});
        // Without a carrier hint the default is OrderSignificant so this
        // also triggers a diff:
        let m = json!({"mode_abilities": [{"k": "b"}, {"k": "a"}]});
        let divs = classify_value(&n, &m);
        assert!(!divs.is_empty());
    }
}
