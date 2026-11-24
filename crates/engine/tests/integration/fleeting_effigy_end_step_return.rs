//! Fleeting Effigy is a {R} 2/2 Elemental with "Haste", "At the beginning of
//! your end step, return this creature to its owner's hand. (Return it only if
//! it's on the battlefield.)", and "{2}{R}: This creature gets +2/+0 until end
//! of turn."
//!
//! These tests guard Effigy's end-step self-return (issue #693). Viashino
//! Sandstalker, Glitterfang, and Archwing Dragon share the self-return effect
//! with "the end step" timing. Reinforced Ronin shares the "your end step"
//! timing checked by the second test.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const NAME: &str = "Fleeting Effigy";
const ORACLE: &str = "Haste\n\
At the beginning of your end step, return this creature to its owner's hand. (Return it only if it's on the battlefield.)\n\
{2}{R}: This creature gets +2/+0 until end of turn.";

fn pass_priority_until_end_step(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().phase == Phase::End {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority on the way to the end step");
    }
    assert_eq!(runner.state().phase, Phase::End);
}

fn assert_effigy_trigger(runner: &GameRunner, effigy: ObjectId, controller: PlayerId) {
    let state = runner.state();
    assert_eq!(state.phase, Phase::End);
    assert_eq!(state.active_player, controller);
    // CR 513.1: The active player gets priority once the end step begins.
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: controller }
    );
    // CR 603.2b: Beginning-of-step abilities trigger as the step begins.
    assert_eq!(state.stack.len(), 1, "expected one end-step trigger");
    assert_eq!(state.stack[0].source_id, effigy);
    assert_eq!(state.stack[0].controller, controller);
}

fn assert_resolution_complete(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn fleeting_effigy_cast_with_haste_returns_at_its_controllers_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let effigy = scenario
        .add_creature_to_hand_from_oracle(P0, NAME, 2, 2, ORACLE)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();

    runner.cast(effigy).resolve();
    assert_eq!(runner.state().objects[&effigy].zone, Zone::Battlefield);

    // CR 702.10b: Haste lets the Effigy attack on the turn it entered.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(effigy, AttackTarget::Player(P1))])
        .expect("attack with the newly cast Effigy");
    pass_priority_until_end_step(&mut runner);
    assert_eq!(runner.life(P1), 18);

    assert_effigy_trigger(&runner, effigy, P0);
    runner.advance_until_stack_empty();
    assert_resolution_complete(&runner);
    assert_eq!(runner.state().objects[&effigy].zone, Zone::Hand);
    assert!(runner.state().players[0].hand.contains(&effigy));
}

#[test]
fn fleeting_effigy_waits_for_its_controllers_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let effigy = scenario
        .add_creature_from_oracle(P1, NAME, 2, 2, ORACLE)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .id();
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Deck Card", "Deck Card"]);
    }
    let mut runner = scenario.build();

    // P0 controls no creatures, so their turn has no declare-attackers prompt.
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&effigy].zone, Zone::Battlefield);

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes at the end of their turn");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes at the end of P0's turn");
    runner.advance_to_combat();
    assert_eq!(runner.state().active_player, P1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    runner
        .declare_attackers(&[])
        .expect("declare no attackers for P1");
    runner.advance_to_end_step();

    assert_effigy_trigger(&runner, effigy, P1);
    runner.advance_until_stack_empty();
    assert_resolution_complete(&runner);
    assert_eq!(runner.state().objects[&effigy].zone, Zone::Hand);
    assert!(runner.state().players[1].hand.contains(&effigy));
}

#[test]
fn fleeting_effigy_returns_to_its_owners_hand_when_another_player_controls_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let effigy = scenario
        .add_creature_from_oracle(P1, NAME, 2, 2, ORACLE)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .controlled_by(P0)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_end_step();
    assert_effigy_trigger(&runner, effigy, P0);
    runner.advance_until_stack_empty();
    assert_resolution_complete(&runner);

    // CR 400.3: The Effigy goes to its owner's hand.
    assert_eq!(runner.state().objects[&effigy].zone, Zone::Hand);
    assert!(runner.state().players[1].hand.contains(&effigy));
    assert!(!runner.state().players[0].hand.contains(&effigy));
}

#[test]
fn fleeting_effigy_that_left_the_battlefield_is_not_returned() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let effigy = scenario
        .add_creature_from_oracle(P0, NAME, 2, 2, ORACLE)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}{B}"))
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_end_step();
    assert_effigy_trigger(&runner, effigy, P0);

    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to Murder's caster");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    let pool = &mut runner.state_mut().players[1].mana_pool;
    for mana in [ManaType::Colorless, ManaType::Black, ManaType::Black] {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
    runner.cast(murder).target_object(effigy).commit();
    assert_eq!(runner.state().stack.len(), 2, "Murder above the trigger");
    for _ in 0..4 {
        if runner.state().stack.len() == 1 {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Murder");
    }

    let state = runner.state();
    assert_eq!(state.objects[&effigy].zone, Zone::Graveyard);
    assert!(state.players[0].graveyard.contains(&effigy));
    // CR 113.7a: Once triggered, an ability exists on the stack independently of its source.
    assert_eq!(
        state.stack.len(),
        1,
        "expected the surviving end-step trigger"
    );
    assert_eq!(state.stack[0].source_id, effigy);
    assert_eq!(state.stack[0].controller, P0);

    runner.advance_until_stack_empty();
    assert_resolution_complete(&runner);

    // CR 400.7: The Effigy in the graveyard is a new object.
    let state = runner.state();
    assert_eq!(state.objects[&effigy].zone, Zone::Graveyard);
    assert!(state.players[0].graveyard.contains(&effigy));
    assert!(state
        .players
        .iter()
        .all(|player| !player.hand.contains(&effigy)));
}
