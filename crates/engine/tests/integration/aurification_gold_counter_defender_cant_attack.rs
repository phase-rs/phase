//! Aurification (backlog root-cause #14) — production-path proof that the parser
//! fix reaches runtime combat legality, not just AST shape.
//!
//! Oracle (verbatim): "Each creature with a gold counter on it is a Wall in
//! addition to its other creature types and has defender. (Those creatures can't
//! attack.)"
//!
//! CR 702.3b: a creature with defender can't attack. Before this PR the static
//! parsed to `add subtype Wall` alone — the trailing "and has defender" conjunct
//! was silently dropped, so a gold-countered creature stayed a legal attacker.
//! The sibling parser test only asserts the parsed `StaticDefinition` shape, and
//! the generic layer test hand-constructs `AddKeyword`, so neither proves that
//! THIS parser seam feeds combat. This test parses Aurification's actual Oracle
//! text onto a battlefield enchantment, places a gold counter on a creature,
//! runs the real `evaluate_layers` pipeline, and asserts the creature is excluded
//! from `get_valid_attacker_ids` and rejected as an attacker declaration. It
//! fails if the parser change is reverted — the dropped defender re-admits the
//! creature.

use engine::game::combat::{get_valid_attacker_ids, validate_attackers};
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

// The full, verbatim Oracle text (Scryfall) so the fix is exercised against the
// real multi-line card — the static must be extracted from between the two
// triggered abilities, not parsed in isolation.
const AURIFICATION: &str = "Whenever a creature deals damage to you, put a gold counter on it.\n\
Each creature with a gold counter on it is a Wall in addition to its other creature types and has defender. (Those creatures can't attack.)\n\
When this enchantment leaves the battlefield, remove all gold counters from all creatures.";

#[test]
fn aurification_gold_counter_creature_cannot_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    // Aurification on the battlefield, its static parsed from real Oracle text.
    scenario
        .add_creature_from_oracle(P0, "Aurification", 0, 0, AURIFICATION)
        .as_enchantment();

    // Two creatures P0 controls; only one carries a gold counter.
    let gold_creature = scenario.add_creature(P0, "Gilded Bear", 2, 2).id();
    let plain_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // CR 122.1: a "gold" counter is an open-named Generic counter.
    scenario.with_counter(gold_creature, CounterType::Generic("gold".to_string()), 1);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;

    // CR 613: recompute the layer system, then read effective (post-layer) state.
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Reach-guard (non-vacuous): the parsed static actually applied. The
    // gold-countered creature gained BOTH the Wall subtype (already worked
    // pre-PR) AND the defender keyword (the conjunct this PR restores). Without
    // this, the "can't attack" assertions below could pass for the wrong reason
    // (e.g. the static never matched the filter at all).
    let gold_obj = &runner.state().objects[&gold_creature];
    assert!(
        gold_obj.card_types.subtypes.iter().any(|s| s == "Wall"),
        "gold-countered creature must become a Wall: {:?}",
        gold_obj.card_types.subtypes
    );
    // CR 702.3b: defender.
    assert!(
        has_keyword(gold_obj, &Keyword::Defender),
        "gold-countered creature must gain defender from Aurification's static (the restored conjunct)"
    );
    // Differential: no gold counter -> no static match -> no defender.
    assert!(
        !has_keyword(&runner.state().objects[&plain_creature], &Keyword::Defender),
        "a creature without a gold counter must not gain defender"
    );

    // (a) QUERY: the gold-countered creature is excluded from valid attackers;
    // the plain creature stays a legal attacker. REVERT-FAIL: if the parser drops
    // "and has defender", the gold creature re-enters this list.
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        !valid.contains(&gold_creature),
        "CR 702.3b: a defender creature can't attack — must be excluded from valid attackers: {valid:?}"
    );
    assert!(
        valid.contains(&plain_creature),
        "the creature without a gold counter is still a legal attacker: {valid:?}"
    );

    // (b) ENFORCEMENT: declaring the gold-countered creature as an attacker is
    // rejected; declaring the plain creature is legal.
    assert!(
        validate_attackers(runner.state(), &[gold_creature]).is_err(),
        "CR 508.1a + CR 702.3b: declaring a defender creature as an attacker must be illegal"
    );
    assert!(
        validate_attackers(runner.state(), &[plain_creature]).is_ok(),
        "the plain creature is a legal attacker declaration"
    );
}
