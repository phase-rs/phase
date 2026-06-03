use std::sync::Arc;

use engine::game::engine::apply_as_current;
use engine::game::scenario::GameScenario;
use engine::game::zones::{add_to_zone, create_object, remove_from_zone};
use engine::parser::parse_oracle_text;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::GameAction;

const P0: PlayerId = PlayerId(0);

const EVELYN_ORACLE: &str = "Flash\nWhenever Evelyn or another Vampire you control enters, exile the top card of each player's library with a collection counter on it.\nOnce each turn, you may play a card from exile with a collection counter on it if it was exiled by an ability you controlled, and mana of any type can be spent to cast that spell.";

fn make_library_land(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
    id
}

fn install_evelyn_static(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
) -> ObjectId {
    let parsed = parse_oracle_text(
        EVELYN_ORACLE,
        "Evelyn, the Covetous",
        &[],
        &["Creature".to_string()],
        &["Vampire".to_string()],
    );
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Evelyn, the Covetous".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push("Vampire".to_string());
    obj.base_card_types = obj.card_types.clone();
    for static_def in parsed.statics {
        obj.static_definitions.push(static_def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def);
    }
    id
}

#[test]
fn evelyn_play_permission_uses_live_static_and_exile_provenance() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source_a = scenario
        .add_creature_to_hand_from_oracle(P0, "Evelyn, the Covetous", 2, 5, EVELYN_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .unwrap()
        .library
        .clear();
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(1))
        .unwrap()
        .library
        .clear();
    let forest = make_library_land(runner.state_mut(), 100, P0, "Forest");
    let _opponent_top = make_library_land(runner.state_mut(), 101, PlayerId(1), "Island");

    // Cast Evelyn A; the cast driver resolves the spell and its ETB exile
    // trigger (no target/optional prompts), parking the live runner at the
    // post-resolution priority window.
    runner.cast(source_a).resolve();

    let exiled_forest = &runner.state().objects[&forest];
    assert_eq!(exiled_forest.zone, Zone::Exile);
    assert_eq!(
        exiled_forest
            .counters
            .get(&CounterType::Generic("collection".to_string())),
        Some(&1)
    );

    let source_b = install_evelyn_static(runner.state_mut(), 200);
    remove_from_zone(runner.state_mut(), source_a, Zone::Battlefield, P0);
    add_to_zone(runner.state_mut(), source_a, Zone::Graveyard, P0);
    runner.state_mut().objects.get_mut(&source_a).unwrap().zone = Zone::Graveyard;

    let forest_card_id = runner.state().objects[&forest].card_id;
    apply_as_current(
        runner.state_mut(),
        GameAction::PlayLand {
            object_id: forest,
            card_id: forest_card_id,
        },
    )
    .expect("Forest exiled by Evelyn A should be playable while Evelyn B's static is live");

    assert_eq!(runner.state().objects[&forest].zone, Zone::Battlefield);
    assert!(runner
        .state()
        .exile_play_permissions_used
        .contains(&source_b));
    assert!(!runner
        .state()
        .exile_play_permissions_used
        .contains(&source_a));
}
