//! CR 107.3c — the where-X lowering pass must assert its OWN post-condition.
//!
//! `apply_where_x_effect_expression` rewrites the quantity slots of the `Effect`
//! variants it enumerates. That match is NOT exhaustive: of the 64 variants carrying a
//! `QuantityExpr`, only 22 are enumerated — the other 42 fall through a `_ => {}`
//! (task #95 binds the representable ones).
//!
//! Before the totality guard, the wildcard's failure mode was a FABRICATION, not a red:
//! the effect kept its bare `QuantityRef::Variable { name: "X" }`, which resolves to 0
//! at runtime (amass 0 / surveil 0 / discard 0) while the face still rendered as fully
//! supported. That lie is invisible to a red-count ledger (there is no `Unimplemented`
//! node to count) AND to the zero-raw-text invariant ("X" is the legitimate alias). A
//! control with an escape hatch is not a control.
//!
//! THE TRAP THIS FILE EXISTS TO GUARD: the guard must be keyed on the EXPRESSION, never
//! on tree-presence of `Variable("X")`. Two families legitimately bind X TO the
//! placeholder, and for them a residual `Variable("X")` is the CORRECT lowering:
//!
//!   Join Forces (CR 107.3i)   "where X is the total amount of mana paid this way"
//!                             -> resolved through the `chosen_x` machinery.
//!   Constraint tails (608.2g) "where X is less than or equal to <bound>"
//!                             -> BOUNDS the player's chosen X rather than defining it.
//!
//! A naive tree-presence guard would flip BOTH families from green to red. The control
//! tests below prove that: each asserts the face still carries a residual `Variable("X")`
//! (so a tree-presence guard WOULD fire on it) and that it nonetheless stays bound. The
//! control can fail, which is what makes it a control.
//!
//! Oracle text below is verbatim from MTGJSON, never paraphrased.

use engine::parser::parse_oracle_text;
use engine::types::ability::Effect;
use serde_json::Value;

/// Every `QuantityRef::Variable { name: "X" }` reachable in a parsed face.
///
/// This is deliberately the NAIVE predicate — tree-presence, exactly what the totality
/// guard must NOT key on. The preserve tests use it to prove a naive guard would fire.
fn retains_variable_x(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            let is_x = map.get("type").and_then(Value::as_str) == Some("Variable")
                && map
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.eq_ignore_ascii_case("X"));
            is_x || map.values().any(retains_variable_x)
        }
        Value::Array(items) => items.iter().any(retains_variable_x),
        _ => false,
    }
}

struct Parsed {
    tree: Value,
    has_where_x_gap: bool,
}

fn parse(oracle: &str, name: &str, types: &[&str]) -> Parsed {
    let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    let tree = serde_json::to_value(&parsed).expect("parse tree serializes");

    fn gap(v: &Value) -> bool {
        match v {
            Value::Object(m) => {
                let hit = m.get("type").and_then(Value::as_str) == Some("Unimplemented")
                    && m.get("name").and_then(Value::as_str) == Some("where_x_binding");
                hit || m.values().any(gap)
            }
            Value::Array(a) => a.iter().any(gap),
            _ => false,
        }
    }
    let has_where_x_gap = gap(&tree);
    Parsed {
        tree,
        has_where_x_gap,
    }
}

// ---------------------------------------------------------------------------
// PRESERVE CONTROLS — the naive guard MUST be able to fail these.
// ---------------------------------------------------------------------------

/// CR 107.3i — Join Forces. "where X is the total amount of mana paid this way" binds X
/// to the PLACEHOLDER on purpose: the value arrives through `chosen_x` after the
/// pay-any-amount loop, so `Variable("X")` IS the correct lowering.
///
/// This is one of exactly 6 faces in the pool (5 Join Forces + Well of Lost Dreams) for
/// which a residual `Variable("X")` is legitimate. A tree-presence guard flips it to red.
#[test]
fn join_forces_placeholder_bind_survives_the_totality_guard() {
    let parsed = parse(
        "Join forces — Starting with you, each player may pay any amount of mana. Each player \
         searches their library for up to X basic land cards, where X is the total amount of \
         mana paid this way, puts them onto the battlefield tapped, then shuffles.",
        "Collective Voyage",
        &["Sorcery"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "CR 107.3i: Join Forces binds X to the placeholder deliberately (the value arrives via \
         chosen_x after the pay-any-amount loop). The totality guard is keyed on the EXPRESSION, \
         so it must leave this face bound. A where_x_binding gap here means the guard regressed \
         to NAIVE tree-presence — which is exactly the failure this control exists to catch. \
         Tree: {:?}",
        parsed.tree
    );

    // NON-VACUITY: the face genuinely still carries a residual Variable("X"), which is what a
    // naive tree-presence guard would have fired on. Without this, "no gap" could pass for the
    // wrong reason (e.g. the clause never parsed) and the control would prove nothing.
    assert!(
        retains_variable_x(&parsed.tree),
        "control is vacuous: Collective Voyage no longer carries a residual Variable(\"X\"), so \
         it is no longer a witness that a tree-presence guard would misfire. Tree: {:?}",
        parsed.tree
    );
}

/// CR 608.2g — a comparator-shaped tail CONSTRAINS the player's chosen X rather than
/// defining it. Well of Lost Dreams pays {X} and draws X; the bound only limits what may
/// be chosen, and the drawn count is the amount actually paid (via `chosen_x`).
#[test]
fn constraint_tail_placeholder_bind_survives_the_totality_guard() {
    let parsed = parse(
        "Whenever you gain life, you may pay {X}, where X is less than or equal to the amount \
         of life you gained. If you do, draw X cards.",
        "Well of Lost Dreams",
        &["Artifact"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "CR 608.2g: the comparator tail BOUNDS the chosen X, it does not define it, so \
         Variable(\"X\") is the correct lowering and the face must stay bound. A gap here means \
         the guard regressed to NAIVE tree-presence. Tree: {:?}",
        parsed.tree
    );

    // NON-VACUITY, as above: this face really is a witness a tree-presence guard would misfire on.
    assert!(
        retains_variable_x(&parsed.tree),
        "control is vacuous: Well of Lost Dreams no longer carries a residual Variable(\"X\"). \
         Tree: {:?}",
        parsed.tree
    );
}

// ---------------------------------------------------------------------------
// THE GUARD ITSELF — an unenumerated variant must RED, never fabricate.
// ---------------------------------------------------------------------------

/// CR 107.3c — `Effect::Surveil` is one of the 42 `QuantityExpr` carriers the pass does
/// NOT enumerate. Its where-X expression is perfectly representable, but until #95 binds
/// it the pass cannot rewrite the slot — so the totality guard must turn it into an
/// honest gap rather than leave `surveil Variable("X")`, which surveils 0 while reading
/// as fully supported.
///
/// This is the positive witness for the guard: without it, this face is a lying green.
#[test]
fn unenumerated_effect_variant_reds_instead_of_fabricating() {
    let parsed = parse(
        "Flying\nWhenever you attack, surveil X, where X is the number of opponents being \
         attacked.",
        "Dimir Strandcatcher",
        &["Creature"],
    );

    assert!(
        parsed.has_where_x_gap,
        "CR 107.3c: Surveil is NOT among the effect variants apply_where_x_effect_expression \
         enumerates, so the where-X slot was never rewritten. The totality guard must report \
         the gap. Leaving Variable(\"X\") here surveils 0 at runtime while rendering as fully \
         supported — a fabrication invisible to both the red-count ledger and the \
         zero-raw-text invariant. Tree: {:?}",
        parsed.tree
    );

    // And the fabrication is genuinely gone, not merely accompanied by a gap.
    assert!(
        !retains_variable_x(&parsed.tree),
        "the bare Variable(\"X\") must be REPLACED by the gap node, not left beside it. Tree: {:?}",
        parsed.tree
    );
}

/// NEGATIVE CONTROL for the guard — an ENUMERATED variant whose expression IS
/// representable must still bind to green. Without this, a guard that simply redded every
/// where-X face would pass every test above.
#[test]
fn enumerated_bindable_where_x_still_binds_green() {
    let parsed = parse(
        "You gain X life, where X is the number of creatures you control.",
        "~",
        &["Sorcery"],
    );

    assert!(
        !parsed.has_where_x_gap,
        "an enumerated variant (GainLife) with a representable expression must still BIND. A \
         gap here means the totality guard is over-firing and redding good faces. Tree: {:?}",
        parsed.tree
    );

    let abilities = parse_oracle_text(
        "You gain X life, where X is the number of creatures you control.",
        "~",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        abilities
            .abilities
            .iter()
            .any(|d| matches!(&*d.effect, Effect::GainLife { .. })),
        "reach-guard: the face must actually parse to GainLife, so the assertion above is not \
         passing because the clause failed to parse at all. Parsed: {:?}",
        abilities.abilities
    );
}
