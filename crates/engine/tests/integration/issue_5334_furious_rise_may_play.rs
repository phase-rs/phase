//! Regression for issue #5334: Furious Rise's "you may play" clause must grant
//! optional play permission without making the exile optional or forcing a cast.
//!
//! https://github.com/phase-rs/phase/issues/5334

use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{CastingPermission, Effect, PlayPermissionInvalidation};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const FURIOUS_RISE_ORACLE: &str = "At the beginning of your end step, if you control a creature with power 4 or greater, exile the top card of your library. You may play that card until you exile another card with this enchantment.";

#[test]
fn furious_rise_parses_mandatory_exile_with_optional_play_grant() {
    let parsed = parse_oracle_text(
        FURIOUS_RISE_ORACLE,
        "Furious Rise",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1, "expected one end-step trigger");
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::Phase);
    assert!(
        !trigger.optional,
        "trigger must not be optional — only the play permission is optional"
    );
    let execute = trigger.execute.as_ref().expect("trigger must have execute");
    assert!(!execute.optional, "exile step must not be optional");
    assert_eq!(
        execute.effect.as_ref(),
        &Effect::ExileTop {
            player: engine::types::ability::TargetFilter::Controller,
            count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
            face_down: false,
        }
    );
    let grant = execute
        .sub_ability
        .as_ref()
        .expect("PlayFromExile grant must chain after ExileTop");
    let Effect::GrantCastingPermission { permission, .. } = grant.effect.as_ref() else {
        panic!(
            "expected GrantCastingPermission sub-ability, got {:?}",
            grant.effect
        );
    };
    let CastingPermission::PlayFromExile { invalidation, .. } = permission else {
        panic!("expected PlayFromExile permission, got {permission:?}");
    };
    assert_eq!(
        *invalidation,
        Some(PlayPermissionInvalidation::UntilNextGrantFromSameSource)
    );
}

fn reach_end_step_and_resolve_stack(runner: &mut engine::game::scenario::GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack declaration should succeed");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("optional decline should succeed");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => {
                if runner.state().stack.is_empty() {
                    runner.pass_both_players();
                } else {
                    runner.act(GameAction::PassPriority).ok();
                }
            }
            _ if runner.state().phase == Phase::End && runner.state().stack.is_empty() => return,
            _ => runner.pass_both_players(),
        }
    }
}

#[test]
fn furious_rise_end_step_exiles_and_grants_optional_play_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let furious_rise = scenario
        .add_creature(P0, "Furious Rise", 0, 0)
        .as_enchantment()
        .from_oracle_text(FURIOUS_RISE_ORACLE)
        .id();
    let _big_creature = scenario.add_creature(P0, "Big Creature", 5, 5).id();
    let library_top = scenario.add_card_to_library_top(P0, "Exiled Spell");

    let mut runner = scenario.build();
    let mut saw_optional_prompt = false;
    runner.advance_to_end_step();
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional_prompt = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap();
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => {
                if runner.state().stack.is_empty() {
                    runner.pass_both_players();
                } else {
                    runner.act(GameAction::PassPriority).ok();
                }
            }
            _ if runner.state().phase == Phase::End && runner.state().stack.is_empty() => break,
            _ => runner.pass_both_players(),
        }
    }

    let exiled = runner.state().objects.get(&library_top).unwrap();
    assert_eq!(
        exiled.zone,
        Zone::Exile,
        "Furious Rise must mandatorily exile the top card when the intervening-if is satisfied"
    );
    assert!(
        !saw_optional_prompt,
        "exile + play grant must not surface OptionalEffectChoice — 'you may play' is permission, not optional resolution"
    );
    let permission = exiled
        .casting_permissions
        .iter()
        .find_map(|p| match p {
            CastingPermission::PlayFromExile {
                invalidation,
                source_id,
                ..
            } => Some((invalidation, source_id)),
            _ => None,
        })
        .expect("exiled card must receive PlayFromExile permission");
    assert_eq!(
        permission.0,
        &Some(PlayPermissionInvalidation::UntilNextGrantFromSameSource)
    );
    assert_eq!(permission.1, &Some(furious_rise));
}

#[test]
fn furious_rise_does_not_fire_without_power_four_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Furious Rise", 0, 0)
        .as_enchantment()
        .from_oracle_text(FURIOUS_RISE_ORACLE);
    let _small_creature = scenario.add_creature(P0, "Small Creature", 2, 2).id();
    let library_top = scenario.add_card_to_library_top(P0, "Should Stay");

    let mut runner = scenario.build();
    reach_end_step_and_resolve_stack(&mut runner);

    assert_eq!(
        runner.state().objects.get(&library_top).unwrap().zone,
        Zone::Library,
        "intervening-if must suppress the trigger when no creature has power 4+"
    );
}
