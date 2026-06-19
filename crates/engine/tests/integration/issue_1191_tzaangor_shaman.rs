//! Regression for GitHub issue #1191 — Tzaangor Shaman delayed copy trigger.
//!
//! Oracle: "Whenever this creature deals combat damage to a player, copy the next
//! instant or sorcery spell you cast this turn when you cast it."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    CopyRetargetPermission, DelayedTriggerCondition, Effect, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::AttackTarget;

const TZAANGOR_ORACLE: &str = "Flying\n\
Whenever this creature deals combat damage to a player, copy the next instant or sorcery spell you cast this turn when you cast it. \
You may choose new targets for the copy.";

#[test]
fn tzaangor_combat_damage_installs_when_next_copy_delayed_trigger() {
    let parsed = parse_oracle_text(
        TZAANGOR_ORACLE,
        "Tzaangor Shaman",
        &["Flying".to_string()],
        &["Creature".to_string()],
        &["Mutant".to_string(), "Shaman".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "expected one triggered ability");
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::DamageDone);

    let execute = trigger
        .execute
        .as_deref()
        .expect("combat damage trigger must have execute body");
    let Effect::CreateDelayedTrigger {
        condition,
        effect: inner,
        ..
    } = execute.effect.as_ref()
    else {
        panic!(
            "expected CreateDelayedTrigger on combat damage, got {:?}",
            execute.effect
        );
    };
    let DelayedTriggerCondition::WhenNextEvent {
        trigger: spell_trigger,
        ..
    } = condition
    else {
        panic!("expected WhenNextEvent delayed trigger, got {condition:?}");
    };
    assert_eq!(spell_trigger.mode, TriggerMode::SpellCast);
    assert!(
        matches!(
            *inner.effect,
            Effect::CopySpell {
                target: TargetFilter::TriggeringSource,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                ..
            }
        ),
        "inner effect must copy the triggering spell, got {:?}",
        inner.effect
    );
}

#[test]
fn tzaangor_delayed_trigger_copies_next_instant_on_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shaman = scenario
        .add_creature_from_oracle(P0, "Tzaangor Shaman", 3, 3, TZAANGOR_ORACLE)
        .with_subtypes(vec!["Mutant", "Shaman"])
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(shaman, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attack");

    for _ in 0..40 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blocks");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            _ if runner.state().phase == Phase::PostCombatMain => break,
            _ => runner.pass_both_players(),
        }
    }

    assert!(
        runner.state().delayed_triggers.iter().any(|dt| {
            matches!(dt.condition, DelayedTriggerCondition::WhenNextEvent { .. })
                && matches!(dt.ability.effect, Effect::CopySpell { .. })
                && dt.source_id == shaman
        }),
        "combat damage must install a WhenNextEvent CopySpell delayed trigger; got {:?}",
        runner.state().delayed_triggers
    );

    let stack_before = runner.state().stack.len();
    runner.cast(bolt).target_player(P1).commit();
    let stack_after = runner.state().stack.len();
    assert!(
        stack_after > stack_before,
        "Lightning Bolt must be on the stack (before={stack_before}, after={stack_after})"
    );
    assert!(
        stack_after >= stack_before + 2,
        "Tzaangor delayed trigger should put a copy on the stack alongside Bolt (stack {stack_before}→{stack_after})"
    );
}
