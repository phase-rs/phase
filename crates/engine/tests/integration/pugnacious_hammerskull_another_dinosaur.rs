//! Pugnacious Hammerskull — "Whenever this creature attacks while you don't
//! control another Dinosaur, put a stun counter on it."
//!
//! CR 508.1m: the attack trigger fires when attackers are declared; the
//! "while" gate is read at that moment (official ruling 2023-11-10).
//! "another Dinosaur" excludes the source from the controlled Dinosaur set.
//!
//! Field report (2026-09-28): the negated control gate failed to parse on
//! "another", the whole while-gate was dropped, and the stun counter landed on
//! every attack — even with a second Dinosaur on the battlefield.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

// Verbatim Oracle text (card-data.json).
const HAMMERSKULL: &str = "Whenever this creature attacks while you don't control another Dinosaur, put a stun counter on it. (If a permanent with a stun counter would become untapped, remove one from it instead.)";

fn stun(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Stun)
        .copied()
        .unwrap_or(0)
}

fn attack_with_hammerskull(companion: Option<(&str, Vec<&str>)>) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hammerskull = {
        let mut b = scenario.add_creature(P0, "Pugnacious Hammerskull", 6, 6);
        b.with_subtypes(vec!["Dinosaur"]);
        b.from_oracle_text(HAMMERSKULL);
        b.id()
    };
    if let Some((name, subtypes)) = companion {
        let mut b = scenario.add_creature(P0, name, 2, 2);
        b.with_subtypes(subtypes);
    }
    let mut runner = scenario.build();

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward declare attackers");
            }
            other => panic!("unexpected waiting_for before attackers: {other:?}"),
        }
    }
    runner
        .declare_attackers(&[(hammerskull, AttackTarget::Player(P1))])
        .expect("Hammerskull should be a legal attacker");
    runner.advance_until_stack_empty();
    stun(&runner, hammerskull)
}

#[test]
fn hammerskull_alone_gets_stunned() {
    // "another" must exclude the source itself — otherwise Hammerskull would
    // count as its own companion and never be stunned.
    assert_eq!(attack_with_hammerskull(None), 1);
}

#[test]
fn hammerskull_with_another_dinosaur_is_not_stunned() {
    assert_eq!(
        attack_with_hammerskull(Some(("Raptor Friend", vec!["Dinosaur"]))),
        0,
        "controlling another Dinosaur must switch the while-gate off"
    );
}

#[test]
fn hammerskull_with_a_non_dinosaur_is_still_stunned() {
    assert_eq!(
        attack_with_hammerskull(Some(("Bear Friend", vec!["Bear"]))),
        1
    );
}

/// CR 508.1m + CR 603.4: the "while" gate is read when Hammerskull is declared
/// an attacker and never again. The official 2023-11-10 ruling: "If you didn't
/// control another Dinosaur when you declared Pugnacious Hammerskull as an
/// attacker, it doesn't matter whether or not you control one as its ability
/// resolves." A Dinosaur that enters while the trigger is on the stack must not
/// stop the stun counter — a resolution recheck would remove the ability.
#[test]
fn dinosaur_entering_in_response_does_not_stop_the_stun() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hammerskull = {
        let mut b = scenario.add_creature(P0, "Pugnacious Hammerskull", 6, 6);
        b.with_subtypes(vec!["Dinosaur"]);
        b.from_oracle_text(HAMMERSKULL);
        b.id()
    };
    let late_dinosaur = {
        let mut b = scenario.add_creature_to_hand(P0, "Late Raptor", 2, 2);
        b.with_subtypes(vec!["Dinosaur"]);
        b.id()
    };
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hammerskull, AttackTarget::Player(P1))])
        .expect("Hammerskull should be a legal attacker");

    // The trigger fired (no other Dinosaur at declaration) and sits on the
    // stack with NO resolution-time condition: the while-gate was event-time.
    let stacked = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { condition, .. }
                if entry.source_id == hammerskull =>
            {
                Some(condition.clone())
            }
            _ => None,
        })
        .expect("the attack trigger must be on the stack");
    assert!(
        stacked.is_none(),
        "the while-gate must not be carried to the stack as a recheck, got {stacked:?}"
    );

    // Another Dinosaur enters in response through the replacement-aware
    // production zone-change pipeline (CR 614.1).
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(late_dinosaur, Zone::Battlefield, late_dinosaur),
            &mut events,
        ),
        "the response move must complete without a replacement choice"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. }
            if *object_id == late_dinosaur
    )));
    assert_eq!(
        runner.state().objects[&late_dinosaur].zone,
        Zone::Battlefield
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        stun(&runner, hammerskull),
        1,
        "the stun counter still lands: the gate was read at declaration"
    );
}
