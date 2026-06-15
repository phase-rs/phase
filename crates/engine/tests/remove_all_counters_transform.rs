//! Regression: "remove all of them and transform it" (level-up / incubate class).
//!
//! CR 122.1 + CR 608.2k + CR 603.4 + CR 701.27: the trigger
//! "When ~ has N or more <type> counters on it, [if ...,] remove all of them
//! and transform it" must parse the anaphoric "remove all of them" / "remove
//! them" clause into `Effect::RemoveCounter` (remove-all sentinel) chained to
//! `Effect::Transform`. Previously the imperative dispatch gate required the
//! literal word "counter" in the clause, so the bare-pronoun anaphor dropped to
//! `Effect::Unimplemented`. Covers Ludevic's Test Subject, Smoldering Egg, and
//! the level-up/incubate class generally.

use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{AbilityDefinition, Effect, QuantityExpr};

fn parse(name: &str, text: &str) -> ParsedAbilities {
    parse_oracle_text(
        text,
        name,
        &[],
        &["Creature".to_string()],
        &["Homunculus".to_string()],
    )
}

/// Walk an ability + its sub_ability chain collecting every effect.
fn collect_effects(def: &AbilityDefinition, out: &mut Vec<Effect>) {
    out.push((*def.effect).clone());
    if let Some(sub) = &def.sub_ability {
        collect_effects(sub, out);
    }
}

/// Returns the flattened list of every effect across all triggers' execute
/// chains, so callers need not assume which trigger carries the remove/transform
/// (Smoldering Egg has a put-counter primary plus a "Then if ..." continuation).
fn trigger_effects(parsed: &ParsedAbilities) -> Vec<Effect> {
    let mut effects = Vec::new();
    for trigger in &parsed.triggers {
        if let Some(execute) = &trigger.execute {
            collect_effects(execute, &mut effects);
        }
    }
    effects
}

/// Asserts the in-scope clause for this fix: the anaphoric counter removal
/// (CR 122.1 + CR 608.2k, remove-all sentinel `count = -1`) chained to a
/// transform (CR 701.27), and that neither of those two effects degraded to
/// `Unimplemented`. Unrelated clauses in the same card (e.g. a dynamic
/// put-counter quantity) are intentionally not asserted here.
fn assert_remove_all_then_transform(effects: &[Effect]) {
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RemoveCounter {
                counter_type: None,
                count: QuantityExpr::Fixed { value: -1 },
                ..
            }
        )),
        "expected RemoveCounter remove-all sentinel; got {effects:#?}"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Transform { .. })),
        "expected Transform; got {effects:#?}"
    );
    // The anaphoric remove clause must no longer surface as Unimplemented.
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::Unimplemented { name, .. } if name == "remove"
        )),
        "remove clause should not be Unimplemented; got {effects:#?}"
    );
}

#[test]
fn ludevic_test_subject_remove_all_and_transform() {
    // Ludevic's Test Subject parses cleanly end to end — no Unimplemented at all.
    let parsed = parse(
        "Ludevic's Test Subject",
        "When ~ has six or more level counters on it, if it isn't a copy of \
another creature, remove all of them and transform it.",
    );
    let effects = trigger_effects(&parsed);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Unimplemented { .. })),
        "Ludevic: clause should not contain Unimplemented; got {effects:#?}"
    );
    assert_remove_all_then_transform(&effects);
}

#[test]
fn smoldering_egg_remove_them_and_transform() {
    // Smoldering Egg's "Then if it has seven or more ember counters on it,
    // remove them and transform it" continuation parses; the unrelated dynamic
    // put-counter ("equal to the amount of mana spent") is a separate gap and
    // is not asserted here.
    let parsed = parse(
        "Smoldering Egg",
        "Whenever you cast an instant or sorcery spell, put a number of ember \
counters on ~ equal to the amount of mana spent to cast that spell. Then if it \
has seven or more ember counters on it, remove them and transform it.",
    );
    let effects = trigger_effects(&parsed);
    assert_remove_all_then_transform(&effects);
}
