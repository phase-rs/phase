//! GitHub issue #2890 — Reality Shift exiles the creature but the chained
//! manifest on its controller's top library card never happens.
//!
//! CR 701.34: Manifest — put the top card of a player's library onto the
//! battlefield face down as a 2/2 creature.
//! CR 608.2c: Chained instructions resolve in order; "its controller" anaphors
//! the exiled creature's controller.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const REALITY_SHIFT: &str =
    "Exile target creature. Its controller manifests the top card of their library.";

fn face_down_battlefield_count(state: &engine::types::game_state::GameState) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.face_down)
        .count()
}

#[test]
fn reality_shift_exiles_creature_and_manifests_for_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent's creature on the battlefield.
    let victim = scenario
        .add_creature_from_oracle(P1, "Victim Creature", 2, 2, "")
        .id();

    // Top card of each library — manifest should use P1's, not P0's.
    let opponent_top = scenario.add_card_to_library_top(P1, "Opponent Top Card");
    let _caster_top = scenario.add_card_to_library_top(P0, "Caster Top Card");

    let reality_shift = scenario
        .add_spell_to_hand_from_oracle(P0, "Reality Shift", true, REALITY_SHIFT)
        .id();

    let mut runner = scenario.build();
    let face_down_before = face_down_battlefield_count(runner.state());

    let outcome = runner
        .cast(reality_shift)
        .target_objects(&[victim])
        .resolve();

    outcome.assert_zone(&[victim], Zone::Exile);

    let state = outcome.state();
    let face_down_after = face_down_battlefield_count(state);
    assert_eq!(
        face_down_after,
        face_down_before + 1,
        "issue #2890: Reality Shift must manifest the exiled creature's controller's top card; \
         face_down_before={face_down_before} face_down_after={face_down_after}, \
         waiting_for={:?}",
        state.waiting_for
    );

    let manifested = state
        .objects
        .get(&opponent_top)
        .expect("opponent's top library card should be manifested");
    assert!(manifested.face_down);
    assert_eq!(manifested.zone, Zone::Battlefield);
    assert_eq!(manifested.controller, P1);
    assert_eq!(manifested.power, Some(2));
    assert_eq!(manifested.toughness, Some(2));
}

#[test]
fn reality_shift_manifests_when_exiled_target_is_a_token() {
    // CR 704.5d: Tokens in exile cease to exist at the next SBA, but CR 608.2h
    // last-known information must still bind "its controller" for the chained
    // manifest (issue #1582 class).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let token = scenario
        .add_creature_from_oracle(P1, "Goblin Token", 1, 1, "")
        .id();
    let opponent_top = scenario.add_card_to_library_top(P1, "Opponent Top Card");

    let reality_shift = scenario
        .add_spell_to_hand_from_oracle(P0, "Reality Shift", true, REALITY_SHIFT)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;

    let outcome = runner
        .cast(reality_shift)
        .target_objects(&[token])
        .resolve();

    let state = outcome.state();
    assert!(
        !state.objects.contains_key(&token),
        "CR 704.5d: exiled token must cease to exist before manifest resolves"
    );

    let manifested = state
        .objects
        .get(&opponent_top)
        .expect("token exile must not drop the chained manifest");
    assert!(manifested.face_down);
    assert_eq!(manifested.zone, Zone::Battlefield);
    assert_eq!(manifested.controller, P1);
}

#[test]
fn reality_shift_manifest_resolves_via_effect_context_object_snapshot() {
    // Regression for the reported failure mode: chained manifest with
    // ParentTargetController but no propagated object targets — only the
    // parent instruction's referent snapshot.
    use engine::game::effects::manifest;
    use engine::types::ability::{CostPaidObjectSnapshot, Effect, QuantityExpr, ResolvedAbility};
    use engine::types::game_state::LKISnapshot;
    use engine::types::identifiers::ObjectId;
    use std::collections::HashMap;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opponent_top = scenario.add_card_to_library_top(P1, "Opponent Top Card");
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::Manifest {
            target: engine::types::ability::TargetFilter::ParentTargetController,
            count: QuantityExpr::Fixed { value: 1 },
        },
        vec![],
        ObjectId(100),
        P0,
    );
    ability.effect_context_object = Some(CostPaidObjectSnapshot {
        object_id: ObjectId(404),
        lki: LKISnapshot {
            name: "Exiled Creature".to_string(),
            power: Some(2),
            toughness: Some(2),
            base_power: Some(2),
            base_toughness: Some(2),
            mana_value: 2,
            controller: P1,
            owner: P1,
            card_types: vec![engine::types::card_type::CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            chosen_attributes: Vec::new(),
            counters: HashMap::new(),
        },
    });

    let mut events = Vec::new();
    manifest::resolve(runner.state_mut(), &ability, &mut events).unwrap();

    let state = runner.state();
    let manifested = state
        .objects
        .get(&opponent_top)
        .expect("manifest must use effect_context_object controller's library");
    assert!(manifested.face_down);
    assert_eq!(manifested.zone, Zone::Battlefield);
    assert_eq!(manifested.controller, P1);
}
