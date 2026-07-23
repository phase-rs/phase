//! Tekuthal, Inquiry Dominus — "If you would proliferate, proliferate twice
//! instead." (GitHub issue #1337)
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1a: "instead" marks a replacement effect.
//!   - CR 614.6: a replacement applies only once to a given event.
//!   - CR 701.34a: proliferate — choose targets, then add a counter of each kind.

use engine::game::effects::proliferate::resolve;
use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, ReplacementPlayerScope, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::ResolutionStateWire;

const TEKUTHAL: &str = "If you would proliferate, proliferate twice instead.";
const TEKUTHAL_FULL: &str = "Flying\nIf you would proliferate, proliferate twice instead.\n{1}{U/P}{U/P}, Remove three counters from among other artifacts, creatures, and planeswalkers you control: Put an indestructible counter on Tekuthal. ({U/P} can be paid with either {U} or 2 life.)";

fn proliferate_action_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: engine::types::events::PlayerActionKind::Proliferate,
                    ..
                }
            )
        })
        .count()
}

fn add_tekuthal(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Tekuthal, Inquiry Dominus", 2, 4, TEKUTHAL_FULL)
        .id()
}

#[test]
fn parses_tekuthal_proliferate_replacement() {
    let parsed = parse_oracle_text(TEKUTHAL, "Tekuthal, Inquiry Dominus", &[], &[], &[]);
    let repl = parsed
        .replacements
        .iter()
        .find(|r| matches!(r.event, ReplacementEvent::Proliferate))
        .expect("Tekuthal must parse a Proliferate replacement");

    assert_eq!(
        repl.valid_player,
        Some(ReplacementPlayerScope::You),
        "Tekuthal is controller-scoped"
    );

    let execute = repl.execute.as_ref().expect("execute ability");
    assert!(matches!(*execute.effect, Effect::Proliferate));
    assert_eq!(
        execute.repeat_for,
        Some(engine::types::ability::QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(engine::types::ability::QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::EventContextAmount,
            }),
        }),
        "Tekuthal doubles proliferate via repeat_for (Multiply so stacked doublers compound)"
    );
}

#[test]
fn applier_doubles_controllers_proliferate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_tekuthal(&mut scenario);
    let mut runner = scenario.build();

    let mut events = Vec::new();
    let proposed = ProposedEvent::proliferate(P0, 1);
    match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::Execute(ProposedEvent::Proliferate { count, .. }) => {
            assert_eq!(count, 2, "Tekuthal doubles proliferate");
        }
        other => panic!("expected Execute(Proliferate {{ count: 2 }}), got {other:?}"),
    }
}

#[test]
fn tekuthal_proliferate_opens_two_choices_and_adds_two_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_tekuthal(&mut scenario);
    let creature = scenario.add_creature(P1, "Pumped", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    let ability = ResolvedAbility::new(Effect::Proliferate, vec![], creature, P0);
    let mut events = Vec::new();
    resolve(runner.state_mut(), &ability, &mut events).expect("proliferate resolves");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ProliferateChoice { .. }
        ),
        "first proliferate choice should open"
    );
    assert!(
        runner.state().active_proliferate_frame().is_some(),
        "the typed proliferate frame owns the first real target-choice prompt"
    );
    let v2 = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("real proliferate prompt serializes as v2");
    assert_eq!(v2["resolution_state_version"], 2);
    assert!(v2.get("pending_proliferate_actions").is_none());
    let restored: ResolutionStateWire =
        serde_json::from_value(v2).expect("v2 proliferate prompt round-trips");
    *runner.state_mut() = restored.into_game_state();
    assert!(
        runner.state().active_proliferate_frame().is_some(),
        "the v2 round-trip preserves the prompt-owning proliferate frame"
    );

    let first = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(creature)],
        })
        .expect("first proliferate choice");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ProliferateChoice { .. }
        ),
        "Tekuthal must open a second proliferate choice"
    );
    assert!(
        runner.state().active_proliferate_frame().is_some(),
        "the remaining proliferate action re-parks a new top frame"
    );
    assert_eq!(
        proliferate_action_count(&first.events),
        1,
        "first proliferate action should have fired"
    );
    assert_eq!(
        runner.state().objects[&creature].counters[&CounterType::Plus1Plus1],
        2,
        "first proliferate should add one +1/+1 counter"
    );

    let second = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(creature)],
        })
        .expect("second proliferate choice");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "both proliferate actions should complete"
    );
    assert!(
        runner.state().active_proliferate_frame().is_none(),
        "the final target choice releases the proliferate frame"
    );
    assert_eq!(
        proliferate_action_count(&second.events),
        1,
        "second proliferate action should fire in its own step"
    );
    assert_eq!(
        runner.state().objects[&creature].counters[&CounterType::Plus1Plus1],
        3,
        "two proliferates should add two +1/+1 counters total"
    );
}

#[test]
fn proliferate_without_tekuthal_remains_single_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Pumped", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    let ability = ResolvedAbility::new(Effect::Proliferate, vec![], creature, P0);
    resolve(runner.state_mut(), &ability, &mut Vec::new()).expect("proliferate resolves");

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(creature)],
        })
        .expect("proliferate choice");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "baseline proliferate should not open a second choice"
    );
    assert_eq!(
        runner.state().objects[&creature].counters[&CounterType::Plus1Plus1],
        2
    );
}

#[test]
fn two_tekuthals_compound_proliferate_to_four() {
    // CR 616.1 + the MOM ruling: two Tekuthal each apply once to the proliferate
    // event, re-evaluating between applications, so the controller proliferates
    // 1 -> 2 -> 4 times — four +1/+1 counters added (1 base -> 5). With the prior
    // `Fixed { value: 2 }` model the second doubler discarded the in-flight count
    // and yielded a flat 2 (-> 3); `Multiply { factor: 2, EventContextAmount }`
    // compounds through the pipeline's re-evaluation.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_tekuthal(&mut scenario);
    add_tekuthal(&mut scenario);
    let creature = scenario.add_creature(P1, "Pumped", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    let ability = ResolvedAbility::new(Effect::Proliferate, vec![], creature, P0);
    resolve(runner.state_mut(), &ability, &mut Vec::new()).expect("proliferate resolves");

    // Drive the CR 616.1 ordering of the two doublers, then each proliferate
    // action, to completion. Bounded to guard against a non-terminating loop.
    // Two ×2 doublers commute, so CR 616.1 ordering is immaterial: the pipeline
    // auto-applies both (no ReplacementChoice prompt) and resolve opens one
    // ProliferateChoice per action. Drive each to completion (bounded guard).
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 32, "proliferate choice loop did not terminate");
        match runner.state().waiting_for.clone() {
            WaitingFor::ProliferateChoice { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(creature)],
                    })
                    .expect("proliferate target choice");
            }
            _ => break,
        }
    }

    assert_eq!(
        runner.state().objects[&creature].counters[&CounterType::Plus1Plus1],
        5,
        "two Tekuthal compound to four proliferations (1 base + 4), not two"
    );
}
