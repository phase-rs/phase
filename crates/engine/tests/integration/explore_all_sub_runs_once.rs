//! Regression: `ExploreAll`'s sub-ability must resolve exactly once.
//!
//! The bug: `explore::resolve_single_explorer` is the authority for an
//! `ExploreAll`'s sub-ability chain (it carries the printed tail onto the
//! terminal explorer and synthesizes the per-explorer `TrackedSet`
//! continuation). The generic chain walker in `resolve_chain_body` ALSO
//! processed `ExploreAll.sub_ability`, so on a paused explore (the nonland
//! `DigChoice`) the sub was stashed onto `pending_continuation` a SECOND time.
//!
//! When the sub is a benign tail (gain life) it double-executes. When the sub
//! is the synthesized `ExploreAll { TrackedSet }` continuation, the second
//! prepend chains it to itself, producing a self-renewing loop that re-explores
//! the same permanent forever (Hakbal of the Surging Soul: unbounded +1/+1
//! counters — the reported Discord bug).
use engine::game::scenario::GameScenario;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, EffectKind, QuantityExpr, TargetFilter,
    TargetRef, TriggerConstraint, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const HAKBAL_ORACLE_TEXT: &str = "At the beginning of combat on your turn, each Merfolk creature you control explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on the exploring creature, then put the card back or put it into your graveyard.)\nWhenever Hakbal attacks, you may put a land card from your hand onto the battlefield. If you don't, draw a card.";

fn library_top_as_lands(
    runner: &mut engine::game::scenario::GameRunner,
    player: PlayerId,
    count: usize,
) -> Vec<ObjectId> {
    let cards: Vec<ObjectId> = runner
        .state()
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .expect("player exists")
        .library
        .iter()
        .take(count)
        .copied()
        .collect();
    let state = runner.state_mut();
    for card in &cards {
        let object = state.objects.get_mut(card).expect("library card exists");
        object.card_types.core_types.push(CoreType::Land);
        object.base_card_types = object.card_types.clone();
    }
    cards
}

fn explore_completion_ids(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } => Some(*source_id),
            _ => None,
        })
        .collect()
}

#[test]
fn explore_all_tail_effect_runs_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    // A Merfolk with a begin-combat trigger: "each Merfolk you control
    // explores, then you gain 3 life." The gain-life tail is the observable
    // proxy for "the ExploreAll sub-ability resolved".
    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::BeginCombat)
        .trigger_zones(vec![Zone::Battlefield])
        .constraint(TriggerConstraint::OnlyDuringYourTurn)
        .execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ExploreAll {
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype("Merfolk".to_string())
                            .controller(ControllerRef::You),
                    ),
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
            )),
        );

    let explorer = {
        let mut b = scenario.add_creature(P0, "Test Merfolk", 2, 2);
        b.with_subtypes(vec!["Merfolk"]);
        b.with_trigger_definition(trigger);
        b.id()
    };
    let _ = explorer;

    // Nonland on top so the explore takes the +1/+1 / DigChoice branch (the
    // pausing path that triggered the double-stash).
    scenario.with_library_top(P0, &["Lightning Bolt", "Lightning Bolt"]);

    let mut runner = scenario.build();
    assert_eq!(runner.state().players[0].life, 20);

    runner.advance_to_combat();

    // Resolve any explore prompts; bounded so an infinite loop is a test failure.
    for step in 0..40 {
        let waiting = runner.state().waiting_for.clone();
        let action = match waiting {
            WaitingFor::ExploreChoice { choosable, .. } => GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(choosable[0])),
            },
            WaitingFor::DigChoice { cards, .. } => GameAction::SelectCards {
                cards: vec![cards[0]],
            },
            WaitingFor::Priority { .. } | WaitingFor::DeclareAttackers { .. } => break,
            other => panic!("unexpected prompt at step {step}: {other:?}"),
        };
        if runner.act(action).is_err() {
            break;
        }
        assert!(step < 39, "explore did not terminate — infinite loop");
    }

    // CR 119.3: the gain-life tail must fire exactly once → 20 + 3 = 23.
    // The double-stash made it fire twice (26) or, for a self-referencing
    // explore continuation, loop forever.
    assert_eq!(
        runner.state().players[0].life,
        23,
        "ExploreAll sub-ability (gain 3 life) must resolve exactly once"
    );
}

/// CR 701.44d + CR 608.2c: Hakbal's real beginning-of-combat trigger asks its
/// controller to order all three Merfolk explorations. Resolving the first
/// choice must retire that `ExploreChoice`, so the next choice contains only
/// the two siblings rather than resurrecting the already-consumed three-card
/// prompt.
#[test]
fn hakbal_begin_combat_explores_each_merfolk_once_after_first_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hakbal = {
        let mut builder = scenario.add_creature_from_oracle(
            P1,
            "Hakbal of the Surging Soul",
            3,
            3,
            HAKBAL_ORACLE_TEXT,
        );
        builder.with_subtypes(vec!["Merfolk", "Scout"]);
        builder.id()
    };
    let sibling_one = scenario
        .add_creature(P1, "Merfolk One", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let sibling_two = scenario
        .add_creature(P1, "Merfolk Two", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    scenario.with_library_top(P1, &["Forest", "Island", "Plains"]);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let lands = library_top_as_lands(&mut runner, P1, 3);

    runner.advance_to_combat();

    match runner.state().waiting_for.clone() {
        WaitingFor::ExploreChoice {
            player, choosable, ..
        } => {
            assert_eq!(player, P1, "Hakbal's controller chooses the first explorer");
            assert_eq!(choosable.len(), 3, "all three Merfolk must be choosable");
            assert!(choosable.contains(&hakbal));
            assert!(choosable.contains(&sibling_one));
            assert!(choosable.contains(&sibling_two));
        }
        other => panic!("expected Hakbal's ExploreChoice, got {other:?}"),
    }

    let mut events = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(hakbal)),
        })
        .expect("Hakbal is a legal first explorer")
        .events;

    let second_choice = match runner.state().waiting_for.clone() {
        WaitingFor::ExploreChoice { choosable, .. } => choosable,
        other => panic!("expected sibling ExploreChoice after Hakbal, got {other:?}"),
    };
    let mut expected_siblings = vec![sibling_one, sibling_two];
    expected_siblings.sort_unstable_by_key(|id| id.0);
    let mut actual_siblings = second_choice.clone();
    actual_siblings.sort_unstable_by_key(|id| id.0);
    assert_eq!(
        actual_siblings, expected_siblings,
        "only Hakbal's siblings remain"
    );

    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second_choice[0])),
            })
            .expect("a sibling is a legal second explorer")
            .events,
    );

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "the resolved Hakbal trigger returns priority to its active controller"
    );
    assert!(
        runner.state().active_ability_continuation().is_none(),
        "no ExploreAll continuation remains after the final explorer"
    );
    for land in lands {
        assert!(
            runner.state().players[1].hand.contains(&land),
            "each revealed land is put into P1's hand"
        );
    }

    let mut expected_completions = vec![hakbal, sibling_one, sibling_two];
    expected_completions.sort_unstable_by_key(|id| id.0);
    let mut completions = explore_completion_ids(&events);
    completions.sort_unstable_by_key(|id| id.0);
    assert_eq!(
        completions, expected_completions,
        "each explorer completes exactly once across all submitted choices"
    );
}

/// CR 701.44d + CR 117.3b: A beginning-of-combat ExploreAll controlled by P1
/// can resolve during P0's turn when it has no `OnlyDuringYourTurn` condition.
/// The P1 chooser's completed response must still leave P0 with priority and
/// reset the priority-pass bookkeeping.
#[test]
fn cross_controller_explore_all_returns_priority_to_active_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::BeginCombat)
        .trigger_zones(vec![Zone::Battlefield])
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
        .add_creature(P1, "P1 Explore Caller", 2, 2)
        .with_trigger_definition(trigger);
    let first = scenario
        .add_creature(P1, "P1 Merfolk One", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let second = scenario
        .add_creature(P1, "P1 Merfolk Two", 2, 2)
        .with_subtypes(vec!["Merfolk"])
        .id();
    scenario.with_library_top(P1, &["Forest", "Island"]);

    let mut runner = scenario.build();
    let lands = library_top_as_lands(&mut runner, P1, 2);
    runner.advance_to_combat();

    match runner.state().waiting_for.clone() {
        WaitingFor::ExploreChoice {
            player, choosable, ..
        } => {
            assert_eq!(player, P1, "P1 controls the simultaneous explores");
            let mut actual = choosable;
            actual.sort_unstable_by_key(|id| id.0);
            let mut expected = vec![first, second];
            expected.sort_unstable_by_key(|id| id.0);
            assert_eq!(actual, expected);
        }
        other => panic!("expected P1 ExploreChoice during P0's turn, got {other:?}"),
    }

    {
        let state = runner.state_mut();
        state.priority_passes.insert(P0);
        state.priority_pass_count = 1;
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(first)),
        })
        .expect("P1 can choose its first Merfolk explorer");

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the active player, not the P1 chooser, receives final priority"
    );
    assert_eq!(runner.state().priority_player, P0);
    assert!(runner.state().priority_passes.is_empty());
    assert_eq!(runner.state().priority_pass_count, 0);
    assert!(runner.state().active_ability_continuation().is_none());
    for land in lands {
        assert!(runner.state().players[1].hand.contains(&land));
    }
}
