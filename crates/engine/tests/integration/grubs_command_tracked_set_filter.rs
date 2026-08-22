use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GRUBS_COMMAND_ORACLE: &str = "Choose two —\n\
    • Create a token that's a copy of target Goblin you control.\n\
    • Creatures target player controls get +1/+1 and gain haste until end of turn.\n\
    • Destroy target artifact or creature.\n\
    • Target player mills five cards, then puts each Goblin card milled this way into their hand.";

fn mark_goblin(runner: &mut GameRunner, id: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.card_types.core_types = vec![CoreType::Creature];
    object.card_types.subtypes = vec!["Goblin".to_string()];
    object.base_card_types = object.card_types.clone();
}

fn mark_non_goblin(runner: &mut GameRunner, id: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.card_types.core_types = vec![CoreType::Sorcery];
    object.card_types.subtypes.clear();
    object.base_card_types = object.card_types.clone();
}

#[test]
fn grubs_command_cast_moves_only_milled_goblins_to_hand() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // add_card_to_library_top inserts at index 0, so these calls seed the
    // library in the order Goblin, non-Goblin, Goblin, non-Goblin, non-Goblin.
    let non_goblin_c = scenario.add_card_to_library_top(P1, "Non-Goblin C");
    let non_goblin_b = scenario.add_card_to_library_top(P1, "Non-Goblin B");
    let goblin_b = scenario.add_card_to_library_top(P1, "Goblin B");
    let non_goblin_a = scenario.add_card_to_library_top(P1, "Non-Goblin A");
    let goblin_a = scenario.add_card_to_library_top(P1, "Goblin A");

    // This Goblin is a scope control for the tracked-set filter. Mode 1 also
    // pumps P1's creatures, so only its zone is relevant to this assertion.
    let battlefield_goblin = scenario
        .add_creature(P1, "Battlefield Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let grubs_command = scenario
        .add_spell_to_hand_from_oracle(P0, "Grub's Command", false, GRUBS_COMMAND_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Red],
            generic: 3,
        })
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    scenario.add_basic_land(P0, ManaColor::Red);

    let mut runner = scenario.build();
    for id in [goblin_a, goblin_b] {
        mark_goblin(&mut runner, id);
    }
    for id in [non_goblin_a, non_goblin_b, non_goblin_c] {
        mark_non_goblin(&mut runner, id);
    }

    let outcome = runner
        .cast(grubs_command)
        .modes(&[1, 3])
        .target_players(&[P1])
        .resolve();

    assert_eq!(outcome.state().players[1].hand.len(), 2);
    for id in [goblin_a, goblin_b] {
        assert_eq!(outcome.state().objects[&id].zone, Zone::Hand);
    }
    assert_eq!(outcome.state().players[1].graveyard.len(), 3);
    for id in [non_goblin_a, non_goblin_b, non_goblin_c] {
        assert_eq!(outcome.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(
        outcome.state().objects[&battlefield_goblin].zone,
        Zone::Battlefield
    );
}
