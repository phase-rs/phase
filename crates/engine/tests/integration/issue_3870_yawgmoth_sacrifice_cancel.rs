//! Regression for issue #3870: cancelling Yawgmoth's activated ability after
//! paying a sacrifice cost must return the sacrificed permanent.
//!
//! https://github.com/phase-rs/phase/issues/3870

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, StaticDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::zones::{EtbTapState, Zone};

const YAWGMOTH_ORACLE: &str = "Protection from Humans\n\
Pay 1 life, Sacrifice another creature: Put a -1/-1 counter on up to one target creature and draw a card.\n\
{B}{B}, Discard a card: Proliferate.";

const ETB_DRAW_ORACLE: &str = "When this creature enters, draw a card.";
const DIES_DRAW_ORACLE: &str = "When this creature dies, draw a card.";

/// CR 614.6: Rest in Peace / Leyline of the Void graveyard-to-exile redirect.
fn graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .description(
            "If a card would be put into a graveyard from anywhere, exile it instead.".to_string(),
        )
}

fn yawgmoth_sacrifice_ability_index(runner: &engine::game::scenario::GameRunner) -> usize {
    let yawgmoth = runner
        .state()
        .objects
        .iter()
        .find(|(_, obj)| {
            obj.abilities.iter().any(|ability| {
                ability
                    .description
                    .as_deref()
                    .is_some_and(|d| d.contains("Sacrifice another creature"))
            })
        })
        .map(|(id, _)| *id)
        .expect("Yawgmoth must be on the battlefield");
    runner.state().objects[&yawgmoth]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|d| d.contains("Sacrifice another creature"))
        })
        .expect("Yawgmoth must expose the sacrifice ability")
}

fn mark_token(
    runner: &mut engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
) {
    runner.state_mut().objects.get_mut(&id).unwrap().is_token = true;
}

#[test]
fn yawgmoth_cancel_after_sacrifice_cost_returns_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH_ORACLE)
        .id();
    let spawn = scenario
        .add_creature(P0, "Eldrazi Spawn", 0, 1)
        .with_subtypes(vec!["Eldrazi", "Spawn"])
        .id();

    let mut runner = scenario.build();
    mark_token(&mut runner, spawn);
    let ability_index = yawgmoth_sacrifice_ability_index(&runner);

    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("begin Yawgmoth activation");

    let WaitingFor::PayCost {
        kind: PayCostKind::Sacrifice,
        choices,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected sacrifice PayCost after announcing Yawgmoth, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        choices.contains(&spawn),
        "Eldrazi Spawn must be legal sacrifice fodder"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![spawn] })
        .expect("sacrifice Eldrazi Spawn for cost");

    assert_eq!(
        runner.state().objects[&spawn].zone,
        Zone::Graveyard,
        "precondition: sacrifice cost must move the spawn to the graveyard"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancel after sacrifice cost");

    assert_eq!(
        runner.state().objects[&spawn].zone,
        Zone::Battlefield,
        "cancelled activation must return the sacrificed token to the battlefield"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "cancel must return to priority"
    );
}

#[test]
fn yawgmoth_cancel_restores_exiled_sacrifice_to_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_replacement());
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH_ORACLE)
        .id();
    let spawn = scenario
        .add_creature(P0, "Eldrazi Spawn", 0, 1)
        .with_subtypes(vec!["Eldrazi", "Spawn"])
        .id();

    let mut runner = scenario.build();
    mark_token(&mut runner, spawn);
    let ability_index = yawgmoth_sacrifice_ability_index(&runner);

    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("begin Yawgmoth activation");
    runner
        .act(GameAction::SelectCards { cards: vec![spawn] })
        .expect("sacrifice Eldrazi Spawn for cost");

    assert_eq!(
        runner.state().objects[&spawn].zone,
        Zone::Exile,
        "precondition: Rest in Peace must exile the sacrificed permanent"
    );

    runner
        .act(GameAction::CancelCast)
        .expect("cancel after redirected sacrifice cost");

    assert_eq!(
        runner.state().objects[&spawn].zone,
        Zone::Battlefield,
        "cancel must restore the sacrificed permanent to its pre-payment zone"
    );
}

#[test]
fn yawgmoth_cancel_restore_does_not_queue_etb_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH_ORACLE)
        .id();
    let visionary = scenario
        .add_creature_from_oracle(P0, "Elvish Visionary", 1, 1, ETB_DRAW_ORACLE)
        .id();

    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let ability_index = yawgmoth_sacrifice_ability_index(&runner);

    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("begin Yawgmoth activation");
    runner
        .act(GameAction::SelectCards {
            cards: vec![visionary],
        })
        .expect("sacrifice Elvish Visionary for cost");
    runner
        .act(GameAction::CancelCast)
        .expect("cancel after sacrifice cost");

    assert_eq!(
        runner.state().objects[&visionary].zone,
        Zone::Battlefield,
        "cancel must restore the sacrificed creature"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "CR 733.1: cancel rollback must not queue ETB triggers, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancel rollback must leave the stack empty"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "cancel rollback must not fire the restored creature's ETB draw"
    );
}

#[test]
fn yawgmoth_cancel_restore_does_not_queue_dies_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH_ORACLE)
        .id();
    let fodder = scenario
        .add_creature_from_oracle(P0, "Fodder", 1, 1, DIES_DRAW_ORACLE)
        .id();

    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let ability_index = yawgmoth_sacrifice_ability_index(&runner);

    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("begin Yawgmoth activation");
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("sacrifice fodder for cost");
    runner
        .act(GameAction::CancelCast)
        .expect("cancel after sacrifice cost");

    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Battlefield,
        "cancel must restore the sacrificed creature"
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "CR 733.1: cancel must purge sacrifice-side deferred triggers"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancel rollback must leave the stack empty"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "cancel rollback must not fire the restored creature's dies draw"
    );
}

#[test]
fn yawgmoth_cancel_restores_tapped_and_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH_ORACLE)
        .id();
    let spawn = scenario.add_creature(P0, "Stateful Spawn", 2, 2).id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&spawn).unwrap();
        obj.tapped = true;
        obj.counters.insert(CounterType::Plus1Plus1, 1);
    }

    let ability_index = yawgmoth_sacrifice_ability_index(&runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("begin Yawgmoth activation");
    runner
        .act(GameAction::SelectCards { cards: vec![spawn] })
        .expect("sacrifice stateful spawn for cost");
    runner
        .act(GameAction::CancelCast)
        .expect("cancel after sacrifice cost");

    let obj = &runner.state().objects[&spawn];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.tapped, "cancel must restore tapped state");
    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "cancel must restore counters"
    );
}

fn activate_yawgmoth_and_sacrifice(
    runner: &mut engine::game::scenario::GameRunner,
    yawgmoth: engine::types::identifiers::ObjectId,
    sacrifice: engine::types::identifiers::ObjectId,
) {
    let ability_index = yawgmoth_sacrifice_ability_index(runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("begin Yawgmoth activation");
    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrifice],
        })
        .expect("sacrifice for Yawgmoth cost");
}

fn commit_yawgmoth_activation_to_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget { target: None })
                    .expect("decline optional -1/-1 target");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.pass_both_players();
            }
            other => {
                panic!("unexpected waiting state while committing Yawgmoth activation: {other:?}")
            }
        }
    }
    panic!("Yawgmoth activation never reached the stack");
}

#[test]
fn completed_activation_does_not_restore_prior_sacrifice_on_later_cancel() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH_ORACLE)
        .id();
    let first_fodder = scenario.add_creature(P0, "First Fodder", 1, 1).id();
    let second_fodder = scenario.add_creature(P0, "Second Fodder", 1, 1).id();
    scenario.add_spell_to_library_top(P0, "Library Filler A", true);
    scenario.add_spell_to_library_top(P0, "Library Filler B", true);

    let mut runner = scenario.build();

    activate_yawgmoth_and_sacrifice(&mut runner, yawgmoth, first_fodder);
    commit_yawgmoth_activation_to_stack(&mut runner);
    runner.advance_until_stack_empty();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("skip attack step before second activation");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("skip block step before second activation");
            }
            WaitingFor::Priority { .. }
                if matches!(
                    runner.state().phase,
                    Phase::PreCombatMain | Phase::PostCombatMain
                ) =>
            {
                break;
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected state before second activation: {other:?}"),
        }
    }

    assert_ne!(
        runner.state().objects[&first_fodder].zone,
        Zone::Battlefield,
        "first activation sacrifice must stay spent after the ability resolves"
    );

    activate_yawgmoth_and_sacrifice(&mut runner, yawgmoth, second_fodder);
    runner
        .act(GameAction::CancelCast)
        .expect("cancel second activation after sacrifice");

    assert_ne!(
        runner.state().objects[&first_fodder].zone,
        Zone::Battlefield,
        "cancel on a later activation must not resurrect an earlier sacrifice"
    );
    assert_eq!(
        runner.state().objects[&second_fodder].zone,
        Zone::Battlefield,
        "cancel must restore only the current activation's sacrifice"
    );
}

#[test]
fn yawgmoth_cancel_after_prevented_sacrifice_does_not_duplicate_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yawgmoth = scenario
        .add_creature_from_oracle(P0, "Yawgmoth, Thran Physician", 2, 4, YAWGMOTH_ORACLE)
        .id();
    let protected = scenario
        .add_creature(P0, "Protected Fodder", 1, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
                .affected(TargetFilter::SelfRef),
        )
        .id();

    let mut runner = scenario.build();
    let ability_index = yawgmoth_sacrifice_ability_index(&runner);

    runner
        .act(GameAction::ActivateAbility {
            source_id: yawgmoth,
            ability_index,
        })
        .expect("begin Yawgmoth activation");
    runner
        .act(GameAction::SelectCards {
            cards: vec![protected],
        })
        .expect("attempt sacrifice on protected creature");
    runner
        .act(GameAction::CancelCast)
        .expect("cancel after prevented sacrifice cost");

    assert_eq!(
        runner.state().objects[&protected].zone,
        Zone::Battlefield,
        "prevented sacrifice must leave the creature on the battlefield"
    );
    let battlefield_dupes = runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| id == protected)
        .count();
    assert_eq!(
        battlefield_dupes, 1,
        "cancel rollback must not duplicate battlefield zone-list entries for same-zone snapshots"
    );
}
