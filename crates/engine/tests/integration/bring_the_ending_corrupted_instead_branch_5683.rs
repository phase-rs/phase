//! Issue #5683 (Class B regression anchor) — CR 614.1a + CR 614.6: a
//! "Counter that spell **instead** if <cond>" ability-word override is a BRANCH,
//! not a second sibling ability.
//!
//! CR 614.1a: "Effects that use the word 'instead' are replacement effects."
//! CR 614.6:  "If an event is replaced, it never happens…"
//!
//! Witness (Oracle read from the pool export):
//!   Bring the Ending —
//!     "Counter target spell unless its controller pays {2}.
//!      Corrupted — Counter that spell instead if its controller has three or
//!      more poison counters."
//!
//! The #5683 defect class is "instead" clauses lowering to an unconditional
//! `SequentialSibling` chain (both halves run, condition dropped). This card is
//! one of the four named regression anchors; it is already lowered CORRECTLY via
//! the cross-line self-replacement binder (the same path that fixed Anoint with
//! Affliction) — this test pins that so it cannot regress back to two
//! unconditional Counter siblings. Drives the real production card parser
//! (`parse_oracle_text`).

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect};

const BRING_THE_ENDING: &str = "Counter target spell unless its controller pays {2}.\nCorrupted — Counter that spell instead if its controller has three or more poison counters.";

#[test]
fn bring_the_ending_corrupted_instead_is_a_branch_not_a_sibling() {
    let parsed = parse_oracle_text(
        BRING_THE_ENDING,
        "Bring the Ending",
        &[],
        &["Instant".to_string()],
        &[],
    );

    // CR 614.6: the "instead" override is a conditional sub of the base spell,
    // NOT a second top-level ability. Two abilities here would mean the Corrupted
    // Counter runs as an independent unconditional sibling (the #5683 defect).
    assert_eq!(
        parsed.abilities.len(),
        1,
        "the Corrupted 'instead' override must be a conditional sub, not a second \
         sibling ability, got {:?}",
        parsed.abilities
    );

    let base = &parsed.abilities[0];
    assert!(
        matches!(&*base.effect, Effect::Counter { .. }),
        "base clause is Counter target spell, got {:?}",
        base.effect
    );
    // The base keeps its "unless its controller pays {2}" cost — not overwritten
    // by the override.
    assert!(
        base.unless_pay.is_some(),
        "base must retain the 'unless … pays {{2}}' cost"
    );

    let sub = base
        .sub_ability
        .as_deref()
        .expect("the Corrupted override must attach as the base's sub_ability");
    // CR 614.15: the override is gated by `ConditionInstead` (fires the swap only
    // when corrupted) — the whole point of the fix. A `None` / non-instead
    // condition would mean the counter runs unconditionally.
    assert!(
        matches!(
            sub.condition,
            Some(AbilityCondition::ConditionInstead { .. })
        ),
        "the override sub must be gated by ConditionInstead (branch), got {:?}",
        sub.condition
    );
    assert!(
        matches!(&*sub.effect, Effect::Counter { .. }),
        "the override effect is 'counter that spell' outright, got {:?}",
        sub.effect
    );
}
