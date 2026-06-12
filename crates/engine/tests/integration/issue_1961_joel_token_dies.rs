//! Issue #1961 — Joel, Resolute Survivor must trigger when a creature token dies,
//! putting a +1/+1 counter on Joel and drawing a card (once per turn).

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
const JOEL_ORACLE: &str = "\
Menace\n\
Whenever a creature token dies, put a +1/+1 counter on Joel and draw a card. This ability triggers only once each turn.\n\
Partner—Survivors (You can have two commanders if both have this ability.)";

fn drain_to_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain exceeded bound; waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

fn destroy_with_lethal_damage(
    runner: &mut engine::game::scenario::GameRunner,
    object_id: ObjectId,
) {
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .unwrap()
        .damage_marked = 99;

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(runner);
}

fn hand_size(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .players
        .get(player.0 as usize)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

#[test]
fn joel_triggers_on_creature_token_death() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Forest");
    }

    let joel_id = scenario
        .add_creature_from_oracle(P0, "Joel, Resolute Survivor", 4, 4, JOEL_ORACLE)
        .id();
    let token_id = scenario.add_creature(P0, "Pest", 1, 1).id();

    let mut runner = scenario.build();
    let joel_triggers = runner
        .state()
        .objects
        .get(&joel_id)
        .expect("Joel on battlefield")
        .trigger_definitions
        .len();
    assert!(
        joel_triggers >= 1,
        "Joel must parse at least one trigger from oracle text, got {joel_triggers}"
    );
    runner
        .state_mut()
        .objects
        .get_mut(&token_id)
        .unwrap()
        .is_token = true;

    let hand_before = hand_size(&runner, P0);

    destroy_with_lethal_damage(&mut runner, token_id);

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "token death must not end the game; waiting_for={:?}",
        runner.state().waiting_for
    );

    let joel = runner
        .state()
        .objects
        .get(&joel_id)
        .expect("Joel still on battlefield");
    assert_eq!(
        joel.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "Joel must receive a +1/+1 counter when a creature token dies; waiting_for={:?}, stack={}",
        runner.state().waiting_for,
        runner.state().stack.len()
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_before + 1,
        "Joel's controller must draw a card"
    );
}

/// Tokens that die without `is_token` set must NOT satisfy Joel's Token filter.
#[test]
fn joel_ignores_nontoken_creature_death() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Forest");
    }

    let joel_id = scenario
        .add_creature_from_oracle(P0, "Joel, Resolute Survivor", 4, 4, JOEL_ORACLE)
        .id();
    let nontoken_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    let hand_before = hand_size(&runner, P0);

    destroy_with_lethal_damage(&mut runner, nontoken_id);

    let joel = runner.state().objects.get(&joel_id).unwrap();
    assert_eq!(
        joel.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "non-token creature death must not trigger Joel"
    );
    assert_eq!(hand_size(&runner, P0), hand_before, "must not draw");
}

/// End-to-end: token created by a spell (real `Effect::Token` pipeline) then dies.
#[test]
fn joel_triggers_when_spell_created_token_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Forest");
    }

    let joel_id = scenario
        .add_creature_from_oracle(P0, "Joel, Resolute Survivor", 4, 4, JOEL_ORACLE)
        .id();
    let token_maker = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Pest Summons",
            false,
            "Create a 1/1 black and green Pest creature token.",
        )
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(token_maker).resolve();
    let token_id = outcome
        .state()
        .battlefield
        .iter()
        .find(|id| {
            outcome
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.id != joel_id)
        })
        .copied()
        .expect("spell must create a Pest token on the battlefield");

    let hand_before = hand_size(&runner, P0);
    destroy_with_lethal_damage(&mut runner, token_id);

    let joel = runner.state().objects.get(&joel_id).unwrap();
    assert_eq!(
        joel.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "Joel must trigger when a spell-created creature token dies"
    );
    assert_eq!(hand_size(&runner, P0), hand_before + 1);
}

/// Joel's trigger is limited to once per turn (CR 603.2h).
#[test]
fn joel_triggers_only_once_per_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Forest");
    }

    let joel_id = scenario
        .add_creature_from_oracle(P0, "Joel, Resolute Survivor", 4, 4, JOEL_ORACLE)
        .id();
    let token_a = scenario.add_creature(P0, "Pest A", 1, 1).id();
    let token_b = scenario.add_creature(P0, "Pest B", 1, 1).id();

    let mut runner = scenario.build();
    for id in [token_a, token_b] {
        runner.state_mut().objects.get_mut(&id).unwrap().is_token = true;
    }

    let hand_before = hand_size(&runner, P0);

    destroy_with_lethal_damage(&mut runner, token_a);
    destroy_with_lethal_damage(&mut runner, token_b);

    let joel = runner.state().objects.get(&joel_id).unwrap();
    assert_eq!(
        joel.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "Joel must trigger only once per turn even if multiple tokens die"
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_before + 1,
        "Joel must draw at most one card per turn from this trigger"
    );
}
