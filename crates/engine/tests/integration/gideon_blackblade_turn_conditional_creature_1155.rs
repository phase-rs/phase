//! Discriminating regression test for **issue #1155**: Gideon Blackblade's
//! turn-conditional animation static must parse and drive the layer system.
//!
//! Gideon Blackblade's static:
//!
//! > As long as it's your turn, Gideon Blackblade is a 4/4 Human Soldier
//! > creature with indestructible that's still a planeswalker.
//!
//! Before the fix the line failed to parse as a static and was dropped to an
//! `Effect::Unimplemented` ability, so the layer system never animated Gideon.
//! The root cause was two parser gaps: `parse_pronoun_becomes_type_static` only
//! accepted possessive-pronoun copulas (`it's a`, `~'s a`, `he's a`) and not the
//! bare `~ is a` copula produced when `parse_conditional_static` splits the
//! inverted "As long as it's your turn, ~ is a ..." line.
//!
//! After the fix the static parses to a `StaticCondition::DuringYourTurn` gate
//! carrying `AddType { Creature }` (additive — CR 205.1b retains Planeswalker),
//! `SetPower/SetToughness 4/4`, and `AddKeyword(Indestructible)`.
//!
//! CR 205.1b: "still a [type]" retains the object's prior card types — so
//!   `AddType` is non-replacing and Gideon stays a planeswalker while a creature.
//! CR 306: Planeswalkers.
//! CR 613.7: continuous type-change from a static ability (timestamp/layers).
//! CR 702.12: indestructible.

use std::sync::Arc;

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, StaticCondition};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;

const GIDEON_ORACLE: &str = "Indestructible\n\
As long as it's your turn, Gideon Blackblade is a 4/4 Human Soldier creature with indestructible that's still a planeswalker.\n\
Whenever Gideon Blackblade attacks, choose up to one target creature an opponent controls. Prevent all combat damage that creature would deal this turn.";

/// Returns true if the condition is `DuringYourTurn`, or an `And` that contains
/// `DuringYourTurn`. Robust to whichever composition the parser emits.
fn condition_gates_on_your_turn(cond: &StaticCondition) -> bool {
    match cond {
        StaticCondition::DuringYourTurn => true,
        StaticCondition::And { conditions } => conditions.iter().any(condition_gates_on_your_turn),
        _ => false,
    }
}

#[test]
fn gideon_blackblade_is_turn_conditional_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Build Gideon from his real Oracle text. Parsing of the animation static
    // happens here, at build time.
    let gideon = scenario
        .add_creature_from_oracle(P0, "Gideon Blackblade", 0, 0, GIDEON_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Reshape the printed baseline into a Legendary Planeswalker with no printed
    // P/T — the actual card. `GameScenario` exposes object mutation only after
    // `build()` (via `GameRunner::state_mut()`, mirroring the priest test). The
    // animation static must add the Creature type and 4/4 P/T on top of this
    // baseline, retaining the Planeswalker type (CR 205.1b). The reshape touches
    // only the type/P-T baseline, not `base_static_definitions` (already parsed).
    {
        let obj = runner.state_mut().objects.get_mut(&gideon).unwrap();
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.card_types.supertypes = vec![Supertype::Legendary];
        obj.card_types.subtypes = vec!["Gideon".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        obj.base_loyalty = Some(4);
        obj.loyalty = Some(4);
    }

    // ----- Assertion 3: the static parsed (not dropped to Unimplemented) -----
    let statics: &Arc<Vec<_>> = &runner.state().objects[&gideon].base_static_definitions;
    let animation = statics
        .iter()
        .find(|s| {
            s.modifications.contains(&ContinuousModification::AddType {
                core_type: CoreType::Creature,
            })
        })
        .expect(
            "Gideon's animation line must parse to a static carrying \
             AddType{Creature}, not drop to Effect::Unimplemented (#1155)",
        );
    let cond = animation
        .condition
        .as_ref()
        .expect("animation static must carry a turn condition");
    assert!(
        condition_gates_on_your_turn(cond),
        "animation static must gate on DuringYourTurn, got {cond:?}"
    );

    // ----- Assertion 1: on P0's turn, Gideon animates -----
    runner.state_mut().active_player = P0;
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&gideon];
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "On its controller's turn Gideon must be a Creature, got {:?}",
        obj.card_types.core_types
    );
    // CR 205.1b: "still a planeswalker" — Planeswalker type is retained.
    assert!(
        obj.card_types.core_types.contains(&CoreType::Planeswalker),
        "Gideon must remain a Planeswalker (CR 205.1b 'still a planeswalker'), got {:?}",
        obj.card_types.core_types
    );
    assert_eq!(obj.power, Some(4), "Gideon's animated power must be 4");
    assert_eq!(
        obj.toughness,
        Some(4),
        "Gideon's animated toughness must be 4"
    );
    assert!(
        obj.keywords
            .iter()
            .any(|k| k.kind() == KeywordKind::Indestructible),
        "Gideon must have Indestructible while animated (CR 702.12), got {:?}",
        obj.keywords
    );

    // ----- Assertion 2 (THE DISCRIMINATOR): on the opponent's turn, Gideon is
    // NOT a creature. This fails both before the fix (never animates → fails
    // Assertion 1) AND under a naive always-on fix that drops the turn gate. -----
    runner.state_mut().active_player = P1;
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&gideon];
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Creature),
        "On the OPPONENT's turn Gideon must NOT be a Creature (turn-conditional \
         static), got {:?}",
        obj.card_types.core_types
    );
    assert!(
        obj.card_types.core_types.contains(&CoreType::Planeswalker),
        "Gideon is always a Planeswalker, got {:?}",
        obj.card_types.core_types
    );
}
