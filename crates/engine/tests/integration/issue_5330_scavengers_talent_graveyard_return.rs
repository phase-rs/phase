//! Regression for issue #5330: Scavenger's Talent level 3 must return a creature
//! card from your graveyard (not mis-scope the return to sacrificed permanents).
//!
//! Fixed on main in #5353; this locks the parse shape and end-step runtime.
//!
//! https://github.com/phase-rs/phase/issues/5330

use engine::game::scenario::{GameScenario, P0};
use engine::game::targeting::find_legal_targets;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Effect, EffectOutcomeSignal, FilterProp, TargetFilter, TargetRef,
    TriggerCondition, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const SCAVENGER_ORACLE: &str = "(Gain the next level as a sorcery to add its ability.)\n\
Whenever one or more creatures you control die, create a Food token. This ability triggers only once each turn.\n\
{1}{B}: Level 2\n\
Whenever you sacrifice a permanent, target player mills two cards.\n\
{2}{B}: Level 3\n\
At the beginning of your end step, you may sacrifice three other nonland permanents. If you do, return a creature card from your graveyard to the battlefield with a finality counter on it.";

fn level3_end_step_trigger(
    triggers: &[engine::types::ability::TriggerDefinition],
) -> engine::types::ability::TriggerDefinition {
    triggers
        .iter()
        .find(|t| {
            t.mode == TriggerMode::Phase
                && t.phase == Some(Phase::End)
                && matches!(
                    t.condition,
                    Some(TriggerCondition::ClassLevelGE { level: 3 })
                )
        })
        .cloned()
        .expect("level 3 end-step trigger")
}

fn graveyard_return_sub_ability(
    trigger: &engine::types::ability::TriggerDefinition,
) -> &engine::types::ability::AbilityDefinition {
    let execute = trigger.execute.as_ref().expect("trigger execute");
    let sacrifice_sub = execute
        .sub_ability
        .as_ref()
        .expect("sacrifice must chain to graveyard return");
    assert!(
        matches!(execute.effect.as_ref(), Effect::Sacrifice { .. }),
        "level 3 trigger must open with optional sacrifice, got {:?}",
        execute.effect
    );
    sacrifice_sub
}

#[test]
fn scavengers_talent_level3_return_targets_graveyard_creature_cards() {
    let mut scenario = GameScenario::new();
    let scavenger = scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(SCAVENGER_ORACLE)
        .id();
    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .id();
    let bf_creature = scenario.add_creature(P0, "Battlefield Bear", 3, 3).id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&scavenger)
        .unwrap()
        .class_level = Some(3);

    let trigger = level3_end_step_trigger(
        runner.state().objects[&scavenger]
            .trigger_definitions
            .as_slice(),
    );
    let return_step = graveyard_return_sub_ability(&trigger);
    let Effect::ChangeZone {
        origin,
        target,
        enter_with_counters,
        ..
    } = return_step.effect.as_ref()
    else {
        panic!(
            "expected graveyard ChangeZone sub-ability, got {:?}",
            return_step.effect
        );
    };

    assert_eq!(*origin, Some(Zone::Graveyard));
    assert_ne!(
        target,
        &TargetFilter::ParentTarget,
        "return must target graveyard creatures, not sacrificed permanents"
    );
    assert_eq!(
        target.extract_in_zone(),
        Some(Zone::Graveyard),
        "target filter must constrain to graveyard cards, got {target:?}"
    );
    match target {
        TargetFilter::Typed(tf) => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Graveyard)));
        }
        other => panic!("expected typed graveyard target filter, got {other:?}"),
    }
    assert!(
        matches!(
            return_step.condition,
            Some(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed
            })
        ),
        "return must be gated on performing the optional sacrifice, got {:?}",
        return_step.condition
    );
    assert!(
        enter_with_counters
            .iter()
            .any(|(counter, _)| { matches!(counter, CounterType::Finality) }),
        "return must enter with a finality counter, got {enter_with_counters:?}"
    );

    let legal = find_legal_targets(runner.state(), target, P0, scavenger);
    assert!(
        legal.contains(&TargetRef::Object(gy_creature)),
        "graveyard creature must be a legal return target"
    );
    assert!(
        !legal.contains(&TargetRef::Object(bf_creature)),
        "battlefield creature must not be a legal return target, got {legal:?}"
    );
}

#[test]
fn scavengers_talent_level3_end_step_returns_creature_with_finality() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let scavenger = scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(SCAVENGER_ORACLE)
        .id();
    let sacrifice_a = scenario.add_creature(P0, "Sacrifice A", 1, 1).id();
    let sacrifice_b = scenario.add_creature(P0, "Sacrifice B", 1, 1).id();
    let sacrifice_c = scenario
        .add_creature(P0, "Sacrifice C", 1, 1)
        .as_artifact()
        .id();
    let gy_creature = scenario
        .add_creature_to_graveyard(P0, "Returned Bear", 4, 4)
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&scavenger)
        .unwrap()
        .class_level = Some(3);

    runner.advance_to_end_step();

    for _ in 0..80 {
        if runner.state().objects[&gy_creature].zone == Zone::Battlefield {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack declaration");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional sacrifice");
            }
            WaitingFor::EffectZoneChoice { zone, .. } => {
                let cards = if zone == Zone::Graveyard {
                    vec![gy_creature]
                } else {
                    vec![sacrifice_a, sacrifice_b, sacrifice_c]
                };
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("complete zone choice prompt");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(gy_creature)),
                    })
                    .expect("choose graveyard creature to return");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).ok();
            }
            _ => runner.pass_both_players(),
        }
    }

    assert_eq!(
        runner.state().objects[&gy_creature].zone,
        Zone::Battlefield,
        "graveyard creature must return to the battlefield"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&gy_creature)
            .and_then(|o| o.counters.get(&CounterType::Finality))
            .copied(),
        Some(1),
        "returned creature must enter with a finality counter"
    );
    for sacrificed in [sacrifice_a, sacrifice_b, sacrifice_c] {
        assert_eq!(
            runner.state().objects[&sacrificed].zone,
            Zone::Graveyard,
            "sacrificed permanent {sacrificed:?} must be in the graveyard"
        );
    }
}

#[test]
fn scavengers_talent_level3_end_step_skipped_below_level_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let scavenger = scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(SCAVENGER_ORACLE)
        .id();
    let _fodder = scenario.add_creature(P0, "Fodder", 2, 2).id();
    scenario.add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2);

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&scavenger)
        .unwrap()
        .class_level = Some(2);

    runner.advance_to_end_step();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack declaration");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => break,
            _ if runner.state().phase == Phase::End => break,
            _ => runner.pass_both_players(),
        }
    }

    let level3_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility {
                    source_id,
                    condition,
                    ..
                } if *source_id == scavenger
                    && matches!(
                        condition,
                        Some(TriggerCondition::ClassLevelGE { level: 3 })
                    )
            )
        })
        .count();

    assert_eq!(
        level3_triggers, 0,
        "class level 2 must not put the level 3 sacrifice/return trigger on the stack"
    );
}

#[test]
fn scavengers_talent_card_data_level3_shape_matches_oracle_parse() {
    let parsed = parse_oracle_text(
        SCAVENGER_ORACLE,
        "Scavenger's Talent",
        &[],
        &["Enchantment".to_string()],
        &["Class".to_string()],
    );
    let trigger = level3_end_step_trigger(&parsed.triggers);
    let return_step = graveyard_return_sub_ability(&trigger);
    assert!(
        matches!(return_step.effect.as_ref(), Effect::ChangeZone { .. }),
        "parsed level 3 chain must end in graveyard ChangeZone, got {:?}",
        return_step.effect
    );
}
