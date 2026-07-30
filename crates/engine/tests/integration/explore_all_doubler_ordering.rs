//! Issue #5272: `ExploreAll` ("each creature you control explores", Hakbal of
//! the Surging Soul) under an explore doubler (Topography Tracker — "if a
//! creature you control would explore, instead it explores, then it explores
//! again") must (a) terminate and (b) keep each creature's two doubled explores
//! CONSECUTIVE — the next creature must not explore between the two halves.
//!
//! This drives the REAL production continuation shape:
//! `Explore(C1).sub(ExploreAll { TrackedSet = [C2] })` built by
//! `explore::resolve_single_explorer`, which the unit tests only approximate
//! with a plain `Explore(C2)` sub.
use engine::game::scenario::GameScenario;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, ReplacementDefinition, TargetFilter,
    TriggerConstraint, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Topography Tracker's replacement: "instead it explores, then it explores
/// again" — execute = Explore, sub = Explore, over any creature you control.
fn double_explore_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Explore)
        .execute(
            AbilityDefinition::new(AbilityKind::Spell, Effect::Explore)
                .sub_ability(AbilityDefinition::new(AbilityKind::Spell, Effect::Explore)),
        )
        .valid_card(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
}

fn plus_one_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

#[test]
fn explore_all_under_doubler_keeps_each_creatures_explores_consecutive() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two Merfolk that will explore; the trigger source carries the ExploreAll
    // and the double-explore replacement.
    let m1 = scenario
        .add_creature(P0, "Merfolk One", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let m2 = scenario
        .add_creature(P0, "Merfolk Two", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();

    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::BeginCombat)
        .trigger_zones(vec![Zone::Battlefield])
        .constraint(TriggerConstraint::OnlyDuringYourTurn)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ExploreAll {
                filter: TargetFilter::Typed(
                    TypedFilter::creature()
                        .subtype("Merfolk".to_string())
                        .controller(ControllerRef::You),
                ),
            },
        ));

    scenario
        .add_creature(P0, "Topography Tracker", 2, 2)
        .with_trigger_definition(trigger)
        .with_replacement_definition(double_explore_replacement());

    // Four nonlands on top so every one of the four explores (2 creatures × 2)
    // takes the pausing DigChoice branch; a card beneath anchors the top.
    scenario.with_library_top(
        P0,
        &[
            "Lightning Bolt",
            "Lightning Bolt",
            "Lightning Bolt",
            "Lightning Bolt",
            "Lightning Bolt",
        ],
    );

    let mut runner = scenario.build();
    runner.advance_to_combat();

    // The top-level ExploreChoice fixes which Merfolk explores first.
    let (first, second) = match runner.state().waiting_for.clone() {
        WaitingFor::ExploreChoice { choosable, .. } => {
            let first = choosable[0];
            let second = if first == m1 { m2 } else { m1 };
            (first, second)
        }
        other => panic!("expected the ExploreAll to open an ExploreChoice, got {other:?}"),
    };

    // Drive every prompt to completion; the loop bound turns the historic
    // self-renewing explore loop (unbounded counters) into a test failure.
    for step in 0..64 {
        // Invariant: the SECOND creature may not gain a +1/+1 counter until the
        // FIRST has both of its doubled explores done (2 counters). Its two
        // explores stay consecutive — no interleaving.
        if plus_one_counters(&runner, second) > 0 {
            assert_eq!(
                plus_one_counters(&runner, first),
                2,
                "second creature explored before the first's doubled explore finished"
            );
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ExploreChoice { choosable, .. } => GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(choosable[0])),
            },
            // Send each revealed nonland to the graveyard so the next explore
            // reveals a fresh card.
            WaitingFor::DigChoice { .. } => GameAction::SelectCards { cards: vec![] },
            WaitingFor::Priority { .. } | WaitingFor::DeclareAttackers { .. } => break,
            other => panic!("unexpected prompt at step {step}: {other:?}"),
        };
        assert!(step < 63, "explore did not terminate — infinite loop");
        if runner.act(action).is_err() {
            break;
        }
    }

    // Each Merfolk explored exactly twice → exactly two +1/+1 counters each,
    // and the loop terminated.
    assert_eq!(
        plus_one_counters(&runner, first),
        2,
        "first creature must explore exactly twice"
    );
    assert_eq!(
        plus_one_counters(&runner, second),
        2,
        "second creature must explore exactly twice"
    );
}
