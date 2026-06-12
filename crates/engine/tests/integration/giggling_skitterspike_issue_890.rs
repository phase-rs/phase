//! Regression coverage for issue #890.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

use super::rules::AttackTarget;
use engine::types::game_state::CastPaymentMode;

const SKITTERSPIKE_ORACLE: &str = "Indestructible\n\
Whenever this creature attacks, blocks, or becomes the target of a spell, it \
deals damage equal to its power to each opponent.\n\
{5}: Monstrosity 5.";

fn p1_life(runner: &engine::game::scenario::GameRunner) -> i32 {
    runner.life(P1)
}

#[test]
fn issue_890_attack_trigger_deals_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let skitterspike = scenario
        .add_creature_from_oracle(P0, "Giggling Skitterspike", 1, 1, SKITTERSPIKE_ORACLE)
        .id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![skitterspike],
            valid_attack_targets: vec![AttackTarget::Player(P1)],
        };
    }

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(skitterspike, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declaring Giggling Skitterspike as attacker should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(p1_life(&runner), 19);
}

#[test]
fn issue_890_block_trigger_deals_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let skitterspike = scenario
        .add_creature_from_oracle(P0, "Giggling Skitterspike", 1, 1, SKITTERSPIKE_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 2;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P1,
            valid_attacker_ids: vec![attacker],
            valid_attack_targets: vec![AttackTarget::Player(P0)],
        };
    }

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("declaring P1 attacker should succeed");
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(skitterspike, attacker)],
        })
        .expect("blocking with Giggling Skitterspike should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(p1_life(&runner), 19);
}

#[test]
fn issue_890_targeted_by_spell_trigger_deals_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let skitterspike = scenario
        .add_creature_from_oracle(P0, "Giggling Skitterspike", 1, 1, SKITTERSPIKE_ORACLE)
        .id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Lightning Bolt should succeed");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(skitterspike)),
            })
            .expect("targeting Giggling Skitterspike should succeed");
    }

    runner.advance_until_stack_empty();

    assert_eq!(p1_life(&runner), 19);
}
