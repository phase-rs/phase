use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const GHYRSON_STARN: &str = "Ward {2}\nWhenever another source you control deals exactly 1 damage to a permanent or player, Ghyrson Starn, Kelermorph deals 2 damage to that permanent or player.";
const DEAL_ONE: &str = "~ deals 1 damage to any target.";
const DEAL_TWO: &str = "~ deals 2 damage to any target.";

fn scenario_with_ghyrson() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ghyrson = scenario
        .add_creature_from_oracle(P0, "Ghyrson Starn, Kelermorph", 3, 2, GHYRSON_STARN)
        .id();
    (scenario, ghyrson)
}

fn damage_marked(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object].damage_marked
}

fn run_unblocked_combat(runner: &mut GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attacker");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers");
    }
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner.combat_damage();
}

#[test]
fn ghyrson_adds_two_damage_to_original_object_recipient() {
    let (mut scenario, _) = scenario_with_ghyrson();
    let victim = scenario.add_creature(P1, "Target Dummy", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Needle Spark", true, DEAL_ONE)
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).target_object(victim).resolve();

    assert_eq!(
        damage_marked(&runner, victim),
        3,
        "Ghyrson must deal 2 more damage to the original damaged permanent"
    );
}

#[test]
fn ghyrson_adds_two_damage_to_original_player_recipient() {
    let (mut scenario, _) = scenario_with_ghyrson();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Needle Spark", true, DEAL_ONE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    outcome.assert_life_delta(P1, -3);
    outcome.assert_life_delta(P0, 0);
}

#[test]
fn ghyrson_multiple_one_damage_events_keep_distinct_recipients() {
    let (mut scenario, _) = scenario_with_ghyrson();
    let first = scenario.add_creature(P1, "First Dummy", 4, 4).id();
    let second = scenario.add_creature(P1, "Second Dummy", 4, 4).id();
    let spell_a = scenario
        .add_spell_to_hand_from_oracle(P0, "First Spark", true, DEAL_ONE)
        .id();
    let spell_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Second Spark", true, DEAL_ONE)
        .id();
    let mut runner = scenario.build();

    runner.cast(spell_a).target_object(first).resolve();
    runner.cast(spell_b).target_object(second).resolve();

    assert_eq!(damage_marked(&runner, first), 3);
    assert_eq!(damage_marked(&runner, second), 3);
}

#[test]
fn ghyrson_does_not_trigger_on_two_damage_event() {
    let (mut scenario, _) = scenario_with_ghyrson();
    let victim = scenario.add_creature(P1, "Target Dummy", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bigger Spark", true, DEAL_TWO)
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).target_object(victim).resolve();

    assert_eq!(damage_marked(&runner, victim), 2);
}

#[test]
fn ghyrson_does_not_trigger_on_opponents_source() {
    let (mut scenario, _) = scenario_with_ghyrson();
    let victim = scenario.add_creature(P0, "Friendly Dummy", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Opponent Spark", true, DEAL_ONE)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner.cast(spell).target_object(victim).resolve();

    assert_eq!(damage_marked(&runner, victim), 1);
}

#[test]
fn ghyrson_does_not_trigger_when_ghyrson_is_the_damage_source() {
    let (mut scenario, ghyrson) = scenario_with_ghyrson();
    let victim = scenario.add_creature(P1, "Target Dummy", 4, 4).id();
    let mut runner = scenario.build();
    let event = GameEvent::DamageDealt {
        source_id: ghyrson,
        target: TargetRef::Object(victim),
        amount: 1,
        is_combat: false,
        excess: 0,
    };

    process_triggers(runner.state_mut(), &[event]);

    assert!(
        runner.state().stack.is_empty(),
        "Ghyrson's Another source filter must reject Ghyrson as the damage source"
    );
}

#[test]
fn ghyrson_combat_damage_to_player_uses_synthetic_damage_event_target() {
    let (mut scenario, _) = scenario_with_ghyrson();
    let attacker = scenario.add_creature(P0, "One Power Attacker", 1, 1).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    run_unblocked_combat(&mut runner, attacker);

    assert_eq!(runner.life(P1), life_before - 3);
}

#[test]
fn ghyrson_combat_damage_two_power_negative() {
    let (mut scenario, _) = scenario_with_ghyrson();
    let attacker = scenario.add_creature(P0, "Two Power Attacker", 2, 2).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    run_unblocked_combat(&mut runner, attacker);

    assert_eq!(runner.life(P1), life_before - 2);
}
