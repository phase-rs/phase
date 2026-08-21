use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility, TargetRef};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str =
    "Target player mills five cards, then puts each Goblin card milled this way into their hand.";

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
fn grubs_command_moves_only_milled_goblins_to_hand() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // add_card_to_library_top inserts at index 0, so these calls seed the
    // library in the order Goblin, non-Goblin, Goblin, non-Goblin, non-Goblin.
    let non_goblin_c = scenario.add_card_to_library_top(P1, "Non-Goblin C");
    let non_goblin_b = scenario.add_card_to_library_top(P1, "Non-Goblin B");
    let goblin_b = scenario.add_card_to_library_top(P1, "Goblin B");
    let non_goblin_a = scenario.add_card_to_library_top(P1, "Non-Goblin A");
    let goblin_a = scenario.add_card_to_library_top(P1, "Goblin A");

    // A battlefield Goblin is the negative control for an unscoped subtype
    // filter: it must not be moved by the follow-up effect.
    let battlefield_goblin = scenario
        .add_creature(P1, "Battlefield Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let source = scenario.add_basic_land(P0, ManaColor::Black).id();

    let mut runner = scenario.build();
    for id in [goblin_a, goblin_b] {
        mark_goblin(&mut runner, id);
    }
    for id in [non_goblin_a, non_goblin_b, non_goblin_c] {
        mark_non_goblin(&mut runner, id);
    }

    assert_eq!(runner.state().players[1].library.len(), 5);

    let definition = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Player(P1)],
        ..build_resolved_from_def(&definition, source, P0)
    };
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Grubs Command effect chain should resolve");

    assert_eq!(runner.state().players[1].hand.len(), 2);
    for id in [goblin_a, goblin_b] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
    }
    assert_eq!(runner.state().players[1].graveyard.len(), 3);
    for id in [non_goblin_a, non_goblin_b, non_goblin_c] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(
        runner.state().objects[&battlefield_goblin].zone,
        Zone::Battlefield
    );
}
