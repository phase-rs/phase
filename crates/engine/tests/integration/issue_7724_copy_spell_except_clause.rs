//! Issue #7724 — runtime regression for the copy-spell `, except <body>` clause
//! (CR 707.9a/b) reaching the resolved copy.
//!
//! Iron Man, Bleeding Edge reads "Whenever you cast an artifact spell, you may
//! copy it, except the copy isn't legendary." The parser dropped that except
//! tail on every copy INSTRUCTION, so `Effect::CopySpell` was built with an
//! empty `additional_modifications` and the copy entered the battlefield still
//! legendary. Two legendary permanents with the same name under one controller
//! then trip the legend rule (CR 704.5j) and one is put into the graveyard —
//! the reported bug.
//!
//! These tests drive the real cast → trigger → stack → resolve pipeline through
//! the scenario runner, so they observe the copy as an actual battlefield
//! object rather than asserting a parsed AST shape. Reverting either half of the
//! fix (the except-tail routing in `parse_utility_imperative_ast`, or the
//! `the copy` subject arm in `become_copy_except::parse_copy_subject`) flips the
//! named assertions.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 205.4: supertypes (Legendary is a supertype).
//!   - CR 704.5j: the legend rule — two same-named legendary permanents under
//!     one controller, all but one are put into their owners' graveyards.
//!   - CR 707.9b: a copy effect may modify a characteristic as part of the
//!     copying process; the modified value becomes part of the copy's copiable
//!     values.
//!   - CR 707.10: copying a spell puts a copy of it onto the stack.

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::phase::Phase;

/// Verbatim Oracle text (from `client/public/card-data.json`). A paraphrase can
/// take a different parser branch than the printed card, so the card-test skill
/// requires the real string here.
const IRON_MAN: &str = "Flying\nWhenever you cast an artifact spell, you may copy it, except the \
                        copy isn't legendary. Do this only once each turn. (The copy becomes a \
                        token.)";

/// Tawnos, the Toymaker — the same `[,] except <body>` routing carrying a
/// TYPE-ADDITION exception instead of a supertype removal, proving the fix is
/// not legend-specific.
const TAWNOS: &str = "Whenever you cast a Beast or Bird creature spell, you may copy it, except \
                      the copy is an artifact in addition to its other types. (The copy becomes a \
                      token.)";

/// CR 707.9b + CR 704.5j: the copy Iron Man creates must NOT be legendary, so
/// it coexists with the original legendary artifact instead of one of the pair
/// being put into the graveyard by the legend rule.
///
/// REVERT-GUARD: with the except tail dropped, the copy keeps Legendary, the
/// legend rule fires on the same-named pair, and the "both permanents are on
/// the battlefield" assertion fails (the copy also fails the supertype
/// assertion directly).
#[test]
fn iron_man_bleeding_edge_copy_is_nonlegendary_and_survives_the_legend_rule() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Iron Man, Bleeding Edge", 3, 5, IRON_MAN)
        .as_legendary();

    // The cast spell is a LEGENDARY artifact — that is what makes the exception
    // observable. A nonlegendary artifact would land on the battlefield twice
    // whether or not the exception applied, so the test would not discriminate.
    let artifact = scenario
        .add_artifact_to_hand_from_oracle(P0, "Mendicant Core, Guidelight", "")
        .as_legendary()
        .id();

    let mut runner = scenario.build();
    // "you may copy it" — the trigger is optional, and the driver declines by
    // default (CR 608.2d).
    runner.cast(artifact).accept_optional().resolve();

    let state = runner.state();
    let landed: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.name == "Mendicant Core, Guidelight")
        .collect();

    // REVERT-GUARD (positive reach): both the original and the copy must be on
    // the battlefield at once. This is the user-visible symptom in issue #7724.
    assert_eq!(
        landed.len(),
        2,
        "CR 704.5j: a nonlegendary copy must coexist with the legendary original; \
         found {} permanent(s) named Mendicant Core, Guidelight",
        landed.len()
    );

    let copy = landed
        .iter()
        .find(|o| o.is_token)
        .expect("CR 707.10: the resolved spell copy becomes a token permanent");
    let original = landed
        .iter()
        .find(|o| !o.is_token)
        .expect("the originally cast card is still on the battlefield");

    // The exception applied to the copy...
    assert!(
        !copy.card_types.supertypes.contains(&Supertype::Legendary),
        "CR 707.9b: \"except the copy isn't legendary\" must strip Legendary from the copy; \
         supertypes were {:?}",
        copy.card_types.supertypes
    );
    // ...and ONLY to the copy — the original keeps its printed supertype
    // (CR 707.9b modifies the copy's copiable values, not the source's).
    assert!(
        original
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "the original cast card must remain legendary; supertypes were {:?}",
        original.card_types.supertypes
    );
}

/// CR 707.9b + CR 205.1b: the same routing must carry a type-ADDITION exception
/// ("except the copy is an artifact in addition to its other types") onto the
/// copy. Guards against a fix that special-cases the legendary supertype
/// instead of routing the whole except clause.
///
/// REVERT-GUARD: with the except tail dropped, the copy is a plain creature
/// token and the `CoreType::Artifact` assertion fails.
#[test]
fn tawnos_the_toymaker_copy_gains_artifact_type_in_addition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Tawnos, the Toymaker", 2, 3, TAWNOS)
        .as_legendary();

    let beast = scenario
        .add_creature_to_hand(P0, "Bristling Boar", 4, 3)
        .with_subtypes(vec!["Beast"])
        .id();

    let mut runner = scenario.build();
    runner.cast(beast).accept_optional().resolve();

    let state = runner.state();
    let copy = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|o| o.is_token && o.name == "Bristling Boar")
        .expect("CR 707.10: the copied creature spell resolves into a token permanent");

    assert!(
        copy.card_types.core_types.contains(&CoreType::Artifact),
        "CR 707.9b: \"except the copy is an artifact\" must add Artifact to the copy; \
         core types were {:?}",
        copy.card_types.core_types
    );
    // CR 205.1b: "in addition to its other types" RETAINS the copied types.
    assert!(
        copy.card_types.core_types.contains(&CoreType::Creature),
        "CR 205.1b: \"in addition to its other types\" must retain Creature; \
         core types were {:?}",
        copy.card_types.core_types
    );
}
