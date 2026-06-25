//! Issue #680 — Shalai and Hallar + Forgotten Ancient on the opponent's turn.
//!
//! Scenario: P0 controls Shalai and Hallar and Forgotten Ancient. On P1's
//! turn P1 casts a spell; Forgotten Ancient's optional trigger puts a +1/+1
//! counter on itself; Shalai and Hallar's CounterAdded trigger must deal that
//! much damage to a target opponent (CR 122.1 + CR 603.2).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const SHALAI_AND_HALLAR: &str = "Flying, vigilance\n\
Whenever one or more +1/+1 counters are put on a creature you control, \
Shalai and Hallar deals that much damage to target opponent.";

const FORGOTTEN_ANCIENT: &str = "Whenever a player casts a spell, you may put a \
+1/+1 counter on this creature.\n\
At the beginning of your upkeep, you may move any number of +1/+1 counters from \
this creature onto other creatures.";

fn p1p1(state: &engine::types::game_state::GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .unwrap()
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn zero_cost_spell(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand(player, "Zero Cantrip", true)
        .with_ability(Effect::NoOp)
        .id()
}

#[test]
fn issue_680_forgotten_ancient_counter_on_opponent_turn_triggers_shalai_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Shalai and Hallar", 3, 3, SHALAI_AND_HALLAR);
    let ancient = scenario
        .add_creature_from_oracle(P0, "Forgotten Ancient", 0, 0, FORGOTTEN_ANCIENT)
        .id();
    let spell = zero_cost_spell(&mut scenario, P1);

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let p1_life_before = runner.state().players[P1.0 as usize].life;

    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_player(P1)
        .resolve();

    assert_eq!(
        p1p1(outcome.state(), ancient),
        1,
        "Forgotten Ancient must receive exactly one +1/+1 counter"
    );
    assert_eq!(
        outcome.life_delta(P1),
        -1,
        "Shalai and Hallar must deal 1 damage to the chosen opponent on P1's turn"
    );
    assert_eq!(
        outcome.state().players[P1.0 as usize].life,
        p1_life_before - 1,
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { player: p } if *p == P1
        ),
        "resolution must finish on P1's priority with an empty stack, got {:?}",
        outcome.final_waiting_for()
    );
}
