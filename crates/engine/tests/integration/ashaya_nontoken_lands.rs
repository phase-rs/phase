//! Issue #436 — Ashaya, Soul of the Wild.
//!
//! Ashaya's second static ability reads "Nontoken creatures you control are
//! Forest lands in addition to their other types." The parser previously
//! misclassified the "Nontoken" descriptor as a creature *subtype*
//! (`Subtype("Nontoken")`) — a subtype that does not exist — so the affected
//! set was empty at runtime and the `AddType`/`AddSubtype` layer modifications
//! applied to nothing.
//!
//! These tests drive the real layer-recompute pipeline (`PassPriority` triggers
//! state-based actions + `evaluate_layers`) and assert the *post-layer*
//! `core_types` / `subtypes` of affected and unaffected objects, exercising
//! parser → `StaticDefinition` → layer system → `game/filter.rs` end to end.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::phase::Phase;

/// Ashaya's "Nontoken creatures you control are Forest lands" line.
const ASHAYA: &str = "Ashaya, Soul of the Wild's power and toughness are each \
equal to the number of lands you control.\nNontoken creatures you control are \
Forest lands in addition to their other types.";

/// CR 205.1b / CR 707.9d: Ashaya adds the Land type and Forest subtype "in
/// addition to" the creature's existing types — the `AddType`/`AddSubtype`
/// layer handlers append rather than replace.
#[test]
fn ashaya_grants_land_forest_to_other_nontoken_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ashaya_id = scenario
        .add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA)
        .id();
    let bears_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    // Pass priority so SBAs run and the layer system recomputes.
    runner.act(GameAction::PassPriority).ok();

    let state = runner.state();

    // The other nontoken creature gains the Land type and Forest subtype.
    let bears = &state.objects[&bears_id];
    assert!(
        bears.card_types.core_types.contains(&CoreType::Land),
        "Grizzly Bears should gain the Land type, got {:?}",
        bears.card_types.core_types
    );
    assert!(
        bears.card_types.core_types.contains(&CoreType::Creature),
        "Grizzly Bears should retain the Creature type (additive), got {:?}",
        bears.card_types.core_types
    );
    assert!(
        bears.card_types.subtypes.iter().any(|s| s == "Forest"),
        "Grizzly Bears should gain the Forest subtype, got {:?}",
        bears.card_types.subtypes
    );

    // Ashaya is itself a nontoken creature you control — it is also affected.
    let ashaya = &state.objects[&ashaya_id];
    assert!(
        ashaya.card_types.core_types.contains(&CoreType::Land),
        "Ashaya should gain the Land type (its own filter includes it), got {:?}",
        ashaya.card_types.core_types
    );
    assert!(
        ashaya.card_types.subtypes.iter().any(|s| s == "Forest"),
        "Ashaya should gain the Forest subtype, got {:?}",
        ashaya.card_types.subtypes
    );
}

/// CR 111.1: A token is a marker representing a permanent not represented by a
/// card. Ashaya's `nontoken` negation excludes tokens — validates the
/// `FilterProp::NonToken` exclusion through the real filter evaluator.
#[test]
fn ashaya_does_not_affect_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA)
        .id();
    let token_id = scenario.add_creature(P0, "Saproling", 1, 1).id();

    let mut runner = scenario.build();
    // Mark the Saproling as a token before the layer recompute.
    runner
        .state_mut()
        .objects
        .get_mut(&token_id)
        .unwrap()
        .is_token = true;
    runner.act(GameAction::PassPriority).ok();

    let token = &runner.state().objects[&token_id];
    assert!(
        !token.card_types.core_types.contains(&CoreType::Land),
        "Token creature must NOT gain the Land type, got {:?}",
        token.card_types.core_types
    );
    assert!(
        !token.card_types.subtypes.iter().any(|s| s == "Forest"),
        "Token creature must NOT gain the Forest subtype, got {:?}",
        token.card_types.subtypes
    );
}

/// CR 109.4: A battlefield object has a controller; Ashaya's filter carries
/// `controller: You`, so an opponent-controlled nontoken creature is not
/// affected.
#[test]
fn ashaya_does_not_affect_opponent_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA)
        .id();
    let opponent_id = scenario.add_creature(P1, "Hostile Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.act(GameAction::PassPriority).ok();

    let opponent = &runner.state().objects[&opponent_id];
    assert!(
        !opponent.card_types.core_types.contains(&CoreType::Land),
        "Opponent's creature must NOT gain the Land type, got {:?}",
        opponent.card_types.core_types
    );
    assert!(
        !opponent.card_types.subtypes.iter().any(|s| s == "Forest"),
        "Opponent's creature must NOT gain the Forest subtype, got {:?}",
        opponent.card_types.subtypes
    );
}
