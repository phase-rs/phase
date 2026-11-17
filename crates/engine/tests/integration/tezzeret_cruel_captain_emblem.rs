//! Tezzeret, Cruel Captain's −7 emblem (issue #7901): "At the beginning of combat on your turn,
//! put three +1/+1 counters on target artifact you control. If it's not a creature, it becomes a
//! 0/0 Robot artifact creature." The animation has no duration (CR 611.2a: a continuous effect
//! from a resolving ability with no stated duration lasts until the game ends), so the targeted
//! artifact stays a creature that can attack, on that turn and on later turns.

use crate::support::shared_card_db;
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle text.
const TEZZERET: &str = "Whenever an artifact you control enters, put a loyalty counter on Tezzeret.\n0: Untap target artifact or creature. If it's an artifact creature, put a +1/+1 counter on it.\n\u{2212}3: Search your library for an artifact card with mana value 1 or less, reveal it, put it into your hand, then shuffle.\n\u{2212}7: You get an emblem with \"At the beginning of combat on your turn, put three +1/+1 counters on target artifact you control. If it's not a creature, it becomes a 0/0 Robot artifact creature.\"";
const EXPEDITION_MAP: &str = "{2}, {T}, Sacrifice this artifact: Search your library for a land card, reveal it, put it into your hand, then shuffle.";

/// Pass priority until the emblem trigger asks for its target; the first prompt belongs to P0.
fn pass_until_trigger_target(runner: &mut GameRunner) {
    for _ in 0..80 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority can be passed");
    }
    panic!("the emblem trigger never asked for a target");
}

/// Choose `target` for the pending emblem trigger and let it resolve.
fn resolve_emblem_trigger(runner: &mut GameRunner, target: ObjectId) {
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("the artifact is a legal target");
    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority can be passed");
    }
    panic!("the emblem trigger never resolved");
}

fn assert_robot(runner: &GameRunner, id: ObjectId) {
    let object = &runner.state().objects[&id];
    assert!(
        object.card_types.core_types.contains(&CoreType::Artifact)
            && object.card_types.core_types.contains(&CoreType::Creature),
        "{}: {:?}",
        object.name,
        object.card_types
    );
    assert_eq!(object.card_types.subtypes, vec!["Robot".to_string()]);
    assert_eq!((object.power, object.toughness), (Some(3), Some(3)));
    assert_eq!(object.counters.get(&CounterType::Plus1Plus1), Some(&3));
}

#[test]
fn emblem_animates_each_targeted_artifact_and_it_can_attack() {
    let db = shared_card_db().expect("the checked-in fixture contains Pentad Prism");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tezzeret = scenario
        .add_planeswalker_from_oracle(P0, "Tezzeret, Cruel Captain", "Tezzeret", 8, TEZZERET)
        .id();
    // Pentad Prism with no charge counters, as in the report.
    let prism = scenario.add_real_card(P0, "Pentad Prism", Zone::Battlefield, db);
    let map = scenario
        .add_artifact_from_oracle(P0, "Expedition Map", EXPEDITION_MAP)
        .id();
    // Libraries deep enough that the turn cycle doesn't deck either player.
    for _ in 0..6 {
        scenario.add_card_to_library_top(P0, "Library Card");
        scenario.add_card_to_library_top(P1, "Library Card");
    }
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    // The −7 is the third loyalty ability.
    runner.activate(tezzeret, 2).resolve();
    assert!(
        runner
            .state()
            .command_zone
            .iter()
            .any(|id| runner.state().objects[id].is_emblem),
        "reach: the −7 created the emblem"
    );
    assert!(!runner.state().objects[&prism]
        .card_types
        .core_types
        .contains(&CoreType::Creature));

    // First combat: Pentad Prism.
    runner.advance_to_phase(Phase::BeginCombat);
    pass_until_trigger_target(&mut runner);
    resolve_emblem_trigger(&mut runner, prism);
    assert_robot(&runner, prism);
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .act(GameAction::PassPriority)
            .expect("priority can be passed");
    }
    let WaitingFor::DeclareAttackers {
        valid_attacker_ids, ..
    } = &runner.state().waiting_for
    else {
        unreachable!()
    };
    assert_eq!(valid_attacker_ids, &vec![prism]);

    // The next combat on P0's turn: Expedition Map, while Pentad Prism stays a Robot.
    let turn = runner.state().turn_number;
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .ok();
    loop {
        pass_until_trigger_target(&mut runner);
        // The emblem's only trigger is "at the beginning of combat on your turn" (CR 603.2b), so
        // a prompt belonging to P1 must not satisfy this test.
        assert_eq!(runner.state().active_player, P0);
        if runner.state().turn_number > turn {
            break;
        }
    }
    resolve_emblem_trigger(&mut runner, map);
    assert_robot(&runner, map);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().objects[&prism]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
}
