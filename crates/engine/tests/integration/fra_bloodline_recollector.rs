//! Reality Fracture coverage for Bloodline Recollector's creature-death threshold.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Comparator, Effect, QuantityExpr, QuantityRef, TargetFilter,
    TriggerCondition, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BLOODLINE_ORACLE: &str = "At the beginning of each end step, if three or more creatures died this turn, this creature becomes prepared. (While it's prepared, you may cast a copy of its spell. Doing so unprepares it.)";
const TALLYMAN_ORACLE: &str = "Lifelink\nThe Seven-fold Chant — At the beginning of your end step, if a creature died this turn, you draw a card and you lose 1 life. If seven or more creatures died this turn, instead you draw seven cards and you lose 7 life.";

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn life_total(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn setup(death_count: usize) -> (GameRunner, engine::types::identifiers::ObjectId) {
    let parsed = parse_oracle_text(
        BLOODLINE_ORACLE,
        "Bloodline Recollector",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Bloodline must parse its end-step trigger");
    assert!(
        !serde_json::to_string(trigger)
            .expect("serialize trigger")
            .contains("\"Unimplemented\""),
        "verbatim Bloodline Oracle must contain no Unimplemented node"
    );
    let Some(TriggerCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ZoneChangeCountThisTurn {
                        from,
                        to,
                        filter: TargetFilter::Typed(filter),
                    },
            },
        comparator,
        rhs,
    }) = trigger.condition.as_ref()
    else {
        panic!("expected the typed creature-death threshold condition")
    };
    assert_eq!(*from, Some(Zone::Battlefield));
    assert_eq!(*to, Some(Zone::Graveyard));
    assert!(filter.type_filters.contains(&TypeFilter::Creature));
    assert_eq!(*comparator, Comparator::GE);
    assert_eq!(*rhs, QuantityExpr::Fixed { value: 3 });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Bloodline Recollector", 2, 2)
        .from_oracle_text(BLOODLINE_ORACLE)
        .id();

    let mut victims = Vec::new();
    let mut removal = Vec::new();
    for index in 0..death_count {
        victims.push(
            scenario
                .add_creature(P1, &format!("Threshold Victim {index}"), 2, 2)
                .id(),
        );
        removal.push(
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    &format!("Threshold Removal {index}"),
                    true,
                    "Destroy target creature.",
                )
                .id(),
        );
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .back_face = Some(BackFaceData {
        layout_kind: Some(LayoutKind::Prepare),
        ..BackFaceData::default()
    });

    for (spell, victim) in removal.into_iter().zip(victims) {
        runner.cast(spell).target_object(victim).resolve();
    }
    (runner, source)
}

fn source_trigger_count(
    runner: &GameRunner,
    source: engine::types::identifiers::ObjectId,
) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

fn advance_to_end_step_trigger(
    runner: &mut GameRunner,
    source: engine::types::identifiers::ObjectId,
) {
    for _ in 0..200 {
        if source_trigger_count(runner, source) > 0
            || (runner.state().phase == Phase::End
                && runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }))
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("pass priority while advancing to the end step"),
            WaitingFor::DeclareAttackers { .. } => runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers"),
            WaitingFor::DeclareBlockers { .. } => runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declare no blockers"),
            other => panic!("unexpected waiting state before end step: {other:?}"),
        };
    }
    panic!("phase machine did not reach the end-step trigger");
}

#[test]
fn bloodline_threshold_gates_at_two_and_fires_at_three() {
    let (mut below, source) = setup(2);
    advance_to_end_step_trigger(&mut below, source);
    assert_eq!(source_trigger_count(&below, source), 0);
    assert!(below.state().objects[&source].prepared.is_none());

    let (mut exact, source) = setup(3);
    advance_to_end_step_trigger(&mut exact, source);
    assert_eq!(source_trigger_count(&exact, source), 1);
    exact.advance_until_stack_empty();
    assert!(exact.state().objects[&source].prepared.is_some());
}

#[test]
fn bloodline_intervening_if_is_rechecked_on_resolution() {
    let (mut runner, source) = setup(3);
    advance_to_end_step_trigger(&mut runner, source);
    assert_eq!(source_trigger_count(&runner, source), 1);

    // CR 603.4 requires a live resolution-time recheck. Death history is
    // monotonic during ordinary play, so clear the observed ledger after the
    // trigger reaches the stack to make a skipped recheck observably wrong.
    runner.state_mut().zone_changes_this_turn.clear();
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&source].prepared.is_none());
}

fn setup_tallyman(death_count: usize) -> (GameRunner, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Tallyman of Nurgle", 2, 3)
        .from_oracle_text(TALLYMAN_ORACLE)
        .id();

    let mut victims = Vec::new();
    let mut removal = Vec::new();
    for index in 0..death_count {
        victims.push(
            scenario
                .add_creature(P1, &format!("Sevenfold Victim {index}"), 1, 1)
                .id(),
        );
        removal.push(
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    &format!("Sevenfold Removal {index}"),
                    true,
                    "Destroy target creature.",
                )
                .id(),
        );
    }
    for index in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("Sevenfold Draw {index}"));
    }

    let mut runner = scenario.build();
    for (spell, victim) in removal.into_iter().zip(victims) {
        runner.cast(spell).target_object(victim).resolve();
    }
    (runner, source)
}

#[test]
fn tallyman_instead_branch_preserves_draw_and_life_loss_chains() {
    let parsed = parse_oracle_text(
        TALLYMAN_ORACLE,
        "Tallyman of Nurgle",
        &[],
        &["Creature".to_string()],
        &["Astartes".to_string(), "Warrior".to_string()],
    );
    let base = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("Tallyman's end-step trigger must have an effect");
    assert!(matches!(
        base.effect.as_ref(),
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));

    let replacement = base
        .sub_ability
        .as_ref()
        .expect("the seven-death replacement must be attached to the base draw");
    assert!(matches!(
        replacement.condition,
        Some(AbilityCondition::ConditionInstead { .. })
    ));
    assert!(matches!(
        replacement.effect.as_ref(),
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 7 },
            ..
        }
    ));
    assert!(matches!(
        replacement
            .sub_ability
            .as_ref()
            .expect("the replacement must retain its lose-7 tail")
            .effect
            .as_ref(),
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 7 },
            ..
        }
    ));
    assert!(matches!(
        replacement
            .else_ability
            .as_ref()
            .expect("the false branch must retain the printed lose-1 tail")
            .effect
            .as_ref(),
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            ..
        }
    ));
}

/// CR 608.2c: the seven-death condition selects the printed `instead` branch
/// while the ability resolves. This drives the real trigger and resolution
/// pipeline and asserts both instructions in the selected branch.
#[test]
fn tallyman_seven_deaths_draws_seven_and_loses_seven_life() {
    let (mut runner, source) = setup_tallyman(7);

    let hand_before = hand_len(&runner, P0);
    let life_before = life_total(&runner, P0);
    advance_to_end_step_trigger(&mut runner, source);
    assert_eq!(source_trigger_count(&runner, source), 1);
    runner.advance_until_stack_empty();

    assert_eq!(hand_len(&runner, P0), hand_before + 7);
    assert_eq!(life_total(&runner, P0), life_before - 7);
}

/// CR 608.2c: below seven deaths, resolution follows the base instructions and
/// does not select the printed `instead` branch. This is the negative witness:
/// an `and` tail peeled into an unconditional sibling loses 7 life here.
#[test]
fn tallyman_one_death_keeps_the_one_card_one_life_base_branch() {
    let (mut runner, source) = setup_tallyman(1);

    let hand_before = hand_len(&runner, P0);
    let life_before = life_total(&runner, P0);
    advance_to_end_step_trigger(&mut runner, source);
    assert_eq!(source_trigger_count(&runner, source), 1);
    runner.advance_until_stack_empty();

    assert_eq!(hand_len(&runner, P0), hand_before + 1);
    assert_eq!(life_total(&runner, P0), life_before - 1);
}
