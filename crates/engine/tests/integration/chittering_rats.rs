//! Tests for Chittering Rats:
//! "When this creature enters, target opponent puts a card from their hand on top of their library."
//!
//! Verifies:
//! - CR 115.1d: Trigger targets opponent upon entering battlefield.
//! - CR 608.2d: Target opponent chooses a card from their hand at resolution time.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Effect, EffectKind, FilterProp, LibraryPosition, QuantityExpr,
    TargetChoiceTiming, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::TargetRef;

const CHITTERING_RATS_ORACLE: &str =
    "When this creature enters, target opponent puts a card from their hand on top of their library.";

#[test]
fn shape_chittering_rats_ast() {
    let parsed = parse_oracle_text(CHITTERING_RATS_ORACLE, "Chittering Rats", &[], &[], &[]);
    assert_eq!(
        parsed.triggers.len(),
        1,
        "Chittering Rats must have exactly one triggered ability"
    );

    let trigger = &parsed.triggers[0];
    let execute = trigger
        .execute
        .as_ref()
        .expect("trigger must have execute ability");
    let Effect::TargetOnly { ref target } = *execute.effect else {
        panic!("expected TargetOnly wrapper, got {:?}", execute.effect);
    };

    // Outer ability targets opponent
    let TargetFilter::Typed(ref tf) = *target else {
        panic!("expected Typed target for opponent, got {:?}", target);
    };
    assert_eq!(tf.controller, Some(ControllerRef::Opponent));

    // Inner sub-ability is PutAtLibraryPosition
    let sub = execute.sub_ability.as_ref().expect("must have sub_ability");
    assert_eq!(
        sub.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "inner card choice must have Resolution timing (CR 608.2d)"
    );

    let Effect::PutAtLibraryPosition {
        ref target,
        ref count,
        ref position,
    } = *sub.effect
    else {
        panic!(
            "expected PutAtLibraryPosition sub-ability, got {:?}",
            sub.effect
        );
    };

    assert_eq!(*position, LibraryPosition::Top);
    assert_eq!(*count, QuantityExpr::Fixed { value: 1 });

    let TargetFilter::Typed(ref card_tf) = *target else {
        panic!("expected Typed card filter, got {:?}", target);
    };

    assert!(
        card_tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::Owned {
                controller: ControllerRef::ScopedPlayer
            }
        )),
        "card filter must be owned by ScopedPlayer, got {:?}",
        card_tf.properties
    );
    assert!(
        card_tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Hand })),
        "card filter must be InZone(Hand), got {:?}",
        card_tf.properties
    );
}

#[test]
fn chittering_rats_etb_prompts_target_opponent_and_moves_card_to_top_of_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Lib 1", "P0 Lib 2"]);
    scenario.with_library_top(P1, &["P1 Lib 1", "P1 Lib 2"]);
    scenario.with_cards_in_hand(P1, &["Opp Card A", "Opp Card B"]);
    scenario.with_cards_in_hand(P0, &["P0 Card"]);

    let rats = scenario
        .add_creature_to_hand_from_oracle(P0, "Chittering Rats", 2, 2, CHITTERING_RATS_ORACLE)
        .id();

    let mut runner = scenario.build();

    let p1_hand_cards = runner.state().players[P1.0 as usize].hand.clone();
    let p0_hand_cards = runner.state().players[P0.0 as usize].hand.clone();
    assert_eq!(p1_hand_cards.len(), 2);

    let mut commit = runner.cast(rats).commit();

    // Advance priority until ETB trigger resolves and prompts for card choice.
    // In a 2-player game, P1 is the only legal target opponent, so the engine
    // auto-assigns P1 as the target without needing manual TriggerTargetSelection.
    for _ in 0..20 {
        match commit.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { .. } => break,
            WaitingFor::OrderTriggers { triggers, .. } => {
                commit
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering trigger");
            }
            WaitingFor::Priority { .. } => {
                commit.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for before EffectZoneChoice: {other:?}"),
        }
    }

    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        library_position,
        ..
    } = commit.state().waiting_for.clone()
    else {
        panic!(
            "expected EffectZoneChoice for P1, got {:?}",
            commit.state().waiting_for
        );
    };

    assert_eq!(player, P1, "target opponent must be prompted for choice");
    assert_eq!(count, 1, "must choose 1 card");
    assert_eq!(library_position, Some(LibraryPosition::Top));

    for id in &p1_hand_cards {
        assert!(
            cards.contains(id),
            "P1 hand cards must be selectable; got {cards:?}"
        );
    }
    for id in &p0_hand_cards {
        assert!(
            !cards.contains(id),
            "P0 hand cards must NOT be offered to P1; got {cards:?}"
        );
    }

    let chosen_card = p1_hand_cards[0];
    let unchosen_card = p1_hand_cards[1];

    commit
        .act(GameAction::SelectCards {
            cards: vec![chosen_card],
        })
        .expect("P1 selects card to put on top of library");

    // The chosen card should now be in P1's library at position top.
    let p1_lib = &commit.state().players[P1.0 as usize].library;
    assert_eq!(
        commit.state().objects[&chosen_card].zone,
        Zone::Library,
        "chosen card must be moved to Library"
    );
    assert_eq!(
        p1_lib.front().copied(),
        Some(chosen_card),
        "chosen card must be on top of P1's library"
    );

    // Unchosen card is still in P1's hand
    assert_eq!(
        commit.state().objects[&unchosen_card].zone,
        Zone::Hand,
        "unchosen card must remain in hand"
    );
}

#[test]
fn chittering_rats_opponent_empty_hand_resolves_cleanly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Lib 1", "P0 Lib 2"]);
    scenario.with_library_top(P1, &["P1 Lib 1", "P1 Lib 2"]);
    // P1 has an empty hand
    scenario.with_cards_in_hand(P1, &[]);

    let rats = scenario
        .add_creature_to_hand_from_oracle(P0, "Chittering Rats", 2, 2, CHITTERING_RATS_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Resolve cast and trigger: with empty hand, resolution completes cleanly at Priority.
    let outcome = runner.cast(rats).resolve();
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                source_id,
                ..
            } if *source_id == rats
        )),
        "empty-hand ETB must resolve PutAtLibraryPosition before returning priority"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "trigger must resolve cleanly without hanging on empty hand, got {:?}",
        outcome.final_waiting_for()
    );
}

#[test]
fn chittering_rats_multiplayer_prompts_for_target_opponent() {
    use engine::types::player::PlayerId;
    const P2: PlayerId = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Lib 1", "P0 Lib 2"]);
    scenario.with_library_top(P1, &["P1 Lib 1", "P1 Lib 2"]);
    scenario.with_library_top(P2, &["P2 Lib 1", "P2 Lib 2"]);

    scenario.with_cards_in_hand(P1, &["P1 Card A"]);
    scenario.with_cards_in_hand(P2, &["P2 Card A", "P2 Card B"]);

    let rats = scenario
        .add_creature_to_hand_from_oracle(P0, "Chittering Rats", 2, 2, CHITTERING_RATS_ORACLE)
        .id();

    let mut runner = scenario.build();
    let p1_hand_cards = runner.state().players[P1.0 as usize].hand.clone();
    let p2_hand_cards = runner.state().players[P2.0 as usize].hand.clone();
    assert_eq!(p1_hand_cards.len(), 1);
    assert_eq!(p2_hand_cards.len(), 2);

    let mut commit = runner.cast(rats).commit();

    // Advance priority until spell resolves and ETB trigger prompts for target.
    // In a 3-player game, both P1 and P2 are opponents, so target selection is ambiguous.
    for _ in 0..20 {
        match commit.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => break,
            WaitingFor::OrderTriggers { triggers, .. } => {
                commit
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering trigger");
            }
            WaitingFor::Priority { .. } => {
                commit.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for before target prompt: {other:?}"),
        }
    }

    let WaitingFor::TriggerTargetSelection {
        target_slots,
        source_id,
        ..
    } = commit.state().waiting_for.clone()
    else {
        panic!(
            "expected TriggerTargetSelection, got {:?}",
            commit.state().waiting_for
        );
    };

    assert_eq!(source_id, Some(rats));
    assert_eq!(target_slots.len(), 1);
    // Opponents P1 and P2 are legal targets
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(P1)),
        "P1 must be a legal target: {:?}",
        target_slots[0].legal_targets
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(P2)),
        "P2 must be a legal target: {:?}",
        target_slots[0].legal_targets
    );
    // Controller P0 is NOT a legal target
    assert!(
        !target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(P0)),
        "P0 (controller) must NOT be a legal target: {:?}",
        target_slots[0].legal_targets
    );

    // Choose P2 as target
    commit
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P2)),
        })
        .expect("targeting P2");

    // Advance priority until trigger resolves and prompts P2 for card choice
    for _ in 0..20 {
        match commit.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                commit.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for before EffectZoneChoice: {other:?}"),
        }
    }

    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        library_position,
        ..
    } = commit.state().waiting_for.clone()
    else {
        panic!(
            "expected EffectZoneChoice for P2, got {:?}",
            commit.state().waiting_for
        );
    };

    assert_eq!(
        player, P2,
        "targeted opponent P2 must be prompted for choice"
    );
    assert_eq!(count, 1, "must choose 1 card");
    assert_eq!(library_position, Some(LibraryPosition::Top));

    for id in &p2_hand_cards {
        assert!(
            cards.contains(id),
            "P2 hand cards must be selectable; got {cards:?}"
        );
    }
    for id in &p1_hand_cards {
        assert!(
            !cards.contains(id),
            "P1 hand cards must NOT be offered to target opponent P2; got {cards:?}"
        );
    }

    let chosen_card = p2_hand_cards[0];
    commit
        .act(GameAction::SelectCards {
            cards: vec![chosen_card],
        })
        .expect("P2 selects card to put on top of library");

    // Chosen card is on top of P2's library
    let p2_lib = &commit.state().players[P2.0 as usize].library;
    assert_eq!(
        commit.state().objects[&chosen_card].zone,
        Zone::Library,
        "chosen card must be moved to Library"
    );
    assert_eq!(
        p2_lib.front().copied(),
        Some(chosen_card),
        "chosen card must be on top of P2's library"
    );
}
