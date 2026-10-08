//! Regression for issue #5327: Doctor Doom's discard trigger must fire only when
//! you discard one or more land cards, not on every discard.
//!
//! https://github.com/phase-rs/phase/issues/5327

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityKind, CardSelectionMode, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::triggers::TriggerMode;

const DOCTOR_DOOM_ORACLE: &str = "Whenever you discard one or more land cards, each opponent loses 2 life.\n\
At the beginning of combat on your turn, target Villain you control gains menace until end of turn. It connives.";

fn doctor_doom_discard_trigger<'a>(
    triggers: impl IntoIterator<Item = &'a engine::types::ability::TriggerDefinition>,
) -> &'a engine::types::ability::TriggerDefinition {
    triggers
        .into_iter()
        .find(|t| t.mode == TriggerMode::DiscardedAll)
        .expect("Doctor Doom land-discard trigger")
}

fn discard_one_card_chain(
    source_id: ObjectId,
    controller: engine::types::player::PlayerId,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            selection: CardSelectionMode::Chosen,
            unless_filter: None,
            filter: None,
        },
        vec![],
        source_id,
        controller,
    )
    .kind(AbilityKind::Spell)
}

fn drain_stack(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 64, "stack did not drain");
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve trigger");
            }
            other => panic!("unexpected wait while draining stack: {other:?}"),
        }
    }
}

fn opponent_life_delta_after_discard(discard_land: bool) -> i32 {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature(P0, "Doctor Doom, King of Latveria", 4, 4)
        .from_oracle_text(DOCTOR_DOOM_ORACLE);
    scenario.add_card_to_hand(P0, "Decoy");
    let discard_target = if discard_land {
        scenario.add_land_to_hand(P0, "Forest").id()
    } else {
        scenario.add_creature_to_hand(P0, "Bear", 2, 2).id()
    };
    let mut runner = scenario.build();
    let p1_before = runner.state().players[P1.0 as usize].life;

    let trigger = doctor_doom_discard_trigger(
        runner
            .state()
            .objects
            .values()
            .find(|o| o.name.contains("Doctor Doom"))
            .expect("doom")
            .trigger_definitions
            .iter_unchecked()
            .map(engine::types::ability::TriggerEntry::definition),
    );
    assert!(
        trigger.valid_card.is_some(),
        "runtime trigger must carry land valid_card filter (issue #5327 regression)"
    );

    let mut events = Vec::new();
    resolve_ability_chain(
        runner.state_mut(),
        &discard_one_card_chain(ObjectId(999), P0),
        &mut events,
        0,
    )
    .expect("begin discard");

    match &runner.state().waiting_for {
        WaitingFor::DiscardChoice { .. } => {}
        other => panic!("expected DiscardChoice, got {other:?}"),
    }

    apply_as_current(
        runner.state_mut(),
        GameAction::SelectCards {
            cards: vec![discard_target],
        },
    )
    .expect("discard selected card");

    assert!(
        runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&discard_target),
        "selected card must be in graveyard after discard"
    );

    drain_stack(&mut runner);
    runner.state().players[P1.0 as usize].life - p1_before
}

#[test]
fn doctor_doom_discard_trigger_parses_land_card_filter() {
    let parsed = parse_oracle_text(
        DOCTOR_DOOM_ORACLE,
        "Doctor Doom, King of Latveria",
        &[],
        &["Creature".to_string()],
        &[
            "Human".to_string(),
            "Noble".to_string(),
            "Villain".to_string(),
        ],
    );
    let trigger = doctor_doom_discard_trigger(parsed.triggers.iter());
    assert_eq!(trigger.valid_target, Some(TargetFilter::Controller));
    assert!(trigger.batched);
    let engine::types::ability::TargetFilter::Typed(tf) =
        trigger.valid_card.as_ref().expect("land card filter")
    else {
        panic!("expected typed valid_card, got {:?}", trigger.valid_card);
    };
    assert!(
        tf.type_filters.contains(&TypeFilter::Land),
        "expected Land filter, got {:?}",
        tf.type_filters
    );
    assert_eq!(tf.controller, None);
    assert!(
        matches!(
            trigger.execute.as_ref().map(|e| e.effect.as_ref()),
            Some(Effect::LoseLife { .. })
        ),
        "trigger must lose life for each opponent"
    );
}

#[test]
fn doctor_doom_loses_opponent_life_when_you_discard_land() {
    assert_eq!(
        opponent_life_delta_after_discard(true),
        -2,
        "discarding a land must make each opponent lose 2 life"
    );
}

#[test]
fn doctor_doom_does_not_trigger_when_you_discard_nonland() {
    assert_eq!(
        opponent_life_delta_after_discard(false),
        0,
        "discarding a nonland must not trigger Doctor Doom's life loss"
    );
}
