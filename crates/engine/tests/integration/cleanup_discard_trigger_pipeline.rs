//! Regression for Discord thread 1535814760485093526: cleanup discards must
//! settle their discard triggers before the cleanup step can advance.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::zones::create_object;
use engine::types::ability::{
    DelayedTriggerCondition, Effect, QuantityExpr, ReplacementDefinition, ReplacementMode,
    ResolvedAbility, TargetFilter, TriggerDefinition, TurnGate, WheneverEventExpiry,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{ActivePlayerControl, DelayedTrigger, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const MAGMAKIN_ARTILLERIST: &str = "Whenever you discard one or more cards, this creature deals that much damage to each opponent.\nCycling {1}{R} ({1}{R}, Discard this card: Draw a card.)\nWhen you cycle this card, it deals 1 damage to each opponent.";
const CURIOSITY: &str = "Enchant creature\nWhenever enchanted creature deals damage to an opponent, you may draw a card.";

fn setup_cleanup_discard(
    hand_size: usize,
    artillerist_count: usize,
    with_curiosity: bool,
) -> (
    GameRunner,
    Vec<engine::types::identifiers::ObjectId>,
    Vec<engine::types::identifiers::ObjectId>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::Cleanup);

    let artillerists = (0..artillerist_count)
        .map(|index| {
            scenario
                .add_creature_from_oracle(
                    P0,
                    &format!("Magmakin Artillerist {index}"),
                    4,
                    4,
                    MAGMAKIN_ARTILLERIST,
                )
                .id()
        })
        .collect::<Vec<_>>();
    let curiosity = with_curiosity.then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Curiosity", CURIOSITY)
            .with_subtypes(vec!["Aura"])
            .id()
    });
    let cards = (0..hand_size)
        .map(|index| scenario.add_card_to_hand(P0, &format!("Hand Card {index}")))
        .collect::<Vec<_>>();
    for index in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {index}"));
    }

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = engine::types::phase::Phase::Cleanup;
        state.waiting_for = WaitingFor::DiscardToHandSize {
            player: P0,
            count: hand_size - 7,
            cards: cards.clone(),
        };
        if let (Some(&artillerist), Some(curiosity)) = (artillerists.first(), curiosity) {
            state.objects.get_mut(&curiosity).unwrap().attached_to =
                Some(AttachTarget::Object(artillerist));
            state
                .objects
                .get_mut(&artillerist)
                .unwrap()
                .attachments
                .push(curiosity);
            reindex_object_triggers(state, curiosity);
        }
    }

    (runner, cards, artillerists)
}

fn resolve_until_optional_choice(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving cleanup trigger");
            }
            ref waiting => panic!("expected Curiosity optional choice, got {waiting:?}"),
        }
    }
    panic!("cleanup trigger chain did not reach Curiosity");
}

#[test]
fn cleanup_discard_stacks_magmakin_then_curiosity_exactly_once() {
    let (mut runner, cards, artillerists) = setup_cleanup_discard(8, 1, true);
    let artillerist = artillerists[0];

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("submit cleanup discard");

    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, engine::types::events::GameEvent::Discarded { .. }))
            .count(),
        1,
        "the cleanup selection must emit one discard event"
    );
    assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().phase, engine::types::phase::Phase::Cleanup);
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == artillerist)
            .count(),
        1,
        "Magmakin's batched discard trigger must be placed exactly once"
    );

    resolve_until_optional_choice(&mut runner);
    assert_eq!(
        runner.state().players[1].life,
        19,
        "Magmakin deals one damage once"
    );
    let hand_before_draw = runner.state().players[0].hand.len();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Curiosity draw");
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before_draw + 1,
        "Curiosity draws exactly one card after the single damage event"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the single Curiosity trigger must not be duplicated"
    );
    assert!(
        runner.state().stack.is_empty(),
        "accepting Curiosity must leave no duplicate Curiosity trigger on the stack"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    for _ in 0..2 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardToHandSize { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to begin the repeated cleanup step");
    }
    let second_cleanup_card = match &runner.state().waiting_for {
        WaitingFor::DiscardToHandSize {
            player,
            count,
            cards,
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*count, 1);
            cards[0]
        }
        waiting => panic!("expected repeated cleanup discard choice, got {waiting:?}"),
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![second_cleanup_card],
        })
        .expect("submit the repeated cleanup discard");
    resolve_until_optional_choice(&mut runner);
    assert_eq!(
        runner.state().players[1].life,
        18,
        "both cleanup discards must produce exactly one Magmakin trigger"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the second Curiosity opportunity");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the second cleanup chain must surface exactly one Curiosity opportunity"
    );
    assert!(runner.state().stack.is_empty());
    for _ in 0..4 {
        if runner.state().phase != engine::types::phase::Phase::Cleanup {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority through the repeated cleanup step");
    }
    assert_ne!(
        runner.state().phase,
        engine::types::phase::Phase::Cleanup,
        "after the trigger stack empties, cleanup must run again before the turn advances"
    );
}

#[test]
fn cleanup_discard_batches_two_cards_into_one_magmakin_trigger() {
    let (mut runner, cards, artillerists) = setup_cleanup_discard(9, 1, false);
    let artillerist = artillerists[0];

    runner
        .act(GameAction::SelectCards {
            cards: cards[..2].to_vec(),
        })
        .expect("submit two-card cleanup discard");

    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == artillerist)
            .count(),
        1,
        "one-or-more discard trigger must be batched and collected exactly once"
    );
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Magmakin");
    }
    assert!(
        runner.state().stack.is_empty(),
        "Magmakin trigger must resolve"
    );
    assert_eq!(
        runner.state().players[1].life,
        18,
        "the one batched trigger must deal damage equal to both discarded cards"
    );
}

#[test]
fn cleanup_discard_fires_persistent_delayed_trigger_exactly_once() {
    let (mut runner, cards, _) = setup_cleanup_discard(8, 0, false);
    let creation_turn = runner.state().turn_number;
    let source = create_object(
        runner.state_mut(),
        CardId(9_002),
        P0,
        "Persistent Discard Trigger".to_string(),
        Zone::Battlefield,
    );
    let mut ability = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.set_trigger_source_recursive(engine::game::triggers::trigger_source_context_for_latch(
        runner.state(),
        &runner.state().objects[&source],
    ));
    runner
        .state_mut()
        .delayed_triggers
        .push(DelayedTrigger::new(
            DelayedTriggerCondition::WheneverEvent {
                trigger: Box::new(TriggerDefinition::new(TriggerMode::DiscardedAll)),
                expiry: WheneverEventExpiry::UntilControllersNextTurn {
                    after: TurnGate::After(creation_turn),
                },
            },
            Box::new(ability),
            P0,
            source,
            false,
        ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("submit cleanup discard with persistent delayed trigger");

    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == source)
            .count(),
        1,
        "the local suffix scan must be the only scan of the persistent delayed discard trigger"
    );
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve persistent delayed trigger");
    }
    assert_eq!(runner.state().players[0].life, 21);
}

#[test]
fn cleanup_discard_orders_same_controller_triggers_before_advancing() {
    let (mut runner, cards, artillerists) = setup_cleanup_discard(8, 2, false);

    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("submit cleanup discard with two Magmakin observers");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OrderTriggers { player: P0, .. }
    ));
    assert_eq!(runner.state().phase, engine::types::phase::Phase::Cleanup);
    assert!(runner.state().stack.is_empty());

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("order simultaneous cleanup discard triggers");
    for artillerist in artillerists {
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| entry.source_id == artillerist)
                .count(),
            1,
            "each Magmakin trigger must reach the stack after ordering"
        );
    }
    assert_eq!(runner.state().phase, engine::types::phase::Phase::Cleanup);
}

#[test]
fn cleanup_discard_priority_uses_controlled_turn_authority() {
    let (mut runner, cards, artillerists) = setup_cleanup_discard(8, 1, false);
    let artillerist = artillerists[0];
    {
        let state = runner.state_mut();
        state.turn_decision_controller = Some(P1);
        state.turn_decision_control_timestamp = Some(1);
        state.active_full_turn_control = Some(ActivePlayerControl {
            controller: P1,
            timestamp: 1,
        });
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("the controlled-turn player submits the cleanup discard");

    assert_eq!(
        runner.state().priority_player,
        P1,
        "cleanup trigger priority must be authorized to the turn controller, not the nominal active player"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == artillerist)
            .count(),
        1,
        "the controlled-turn regression must still use the local suffix trigger scan"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("turn controller must be able to submit the active player's priority pass");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    assert_eq!(runner.state().priority_player, P1);
}

#[test]
fn cleanup_discard_without_observers_advances_normally() {
    let (mut runner, cards, _) = setup_cleanup_discard(8, 0, false);
    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("submit cleanup discard without observers");

    assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Graveyard);
    assert_ne!(
        runner.state().phase,
        engine::types::phase::Phase::Cleanup,
        "a settled cleanup discard with no observers must retain normal advancement"
    );
}

#[test]
fn cleanup_discard_replacement_returns_immediate_choice_without_advance() {
    // A discard observer makes this an actual early-return guard: it must not
    // be scanned before the replacement choice completes the discard event.
    let (mut runner, cards, artillerists) = setup_cleanup_discard(8, 1, false);
    let artillerist = artillerists[0];
    let source = create_object(
        runner.state_mut(),
        CardId(9_001),
        P0,
        "Discard Replacement".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("replacement source exists")
        .replacement_definitions
        .push(
            ReplacementDefinition::new(ReplacementEvent::Discard)
                .mode(ReplacementMode::Optional { decline: None })
                .description("cleanup discard replacement".to_string()),
        );

    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("submit cleanup discard with replacement");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(runner.state().phase, engine::types::phase::Phase::Cleanup);
    assert_eq!(
        runner.state().objects[&artillerist].zone,
        Zone::Battlefield,
        "reach guard: Magmakin must be present to observe the pending discard"
    );
    assert!(
        runner.state().stack.is_empty(),
        "no observer scan runs before the replacement choice"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == artillerist)
            .count(),
        0,
        "Magmakin must not be scanned until the replacement resolves"
    );
}
