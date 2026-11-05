#![cfg(test)]
//! Integration tests for Petals of Insight (Champions of Kamigawa).
//!
//! Oracle: "Look at the top three cards of your library. You may put those cards
//! on the bottom of your library in any order. If you do, return Petals of
//! Insight to its owner's hand. Otherwise, draw three cards."
//!
//! Rules verification:
//! - CR 701.20e: Looking at cards in a library does not move them out of the library.
//! - CR 608.2c: An optional sub-ability ("You may...") pauses for the controller's decision.
//!   If accepted, "If you do" branches execute. If declined, "Otherwise" branches execute.
//! - CR 608.2m: As the final step of resolving an instant or sorcery, if it was not moved
//!   by an effect (e.g. self-bounce), it is placed into its owner's graveyard.

use crate::rules::{GameAction, GameRunner, GameScenario, ObjectId, WaitingFor, Zone, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{DigRestOrder, DigSource, Effect, QuantityExpr, TargetFilter};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const PETALS_OF_INSIGHT_ORACLE: &str = "Look at the top three cards of your library. \
You may put those cards on the bottom of your library in any order. \
If you do, return Petals of Insight to its owner's hand. Otherwise, draw three cards.";

fn petals_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 4,
    }
}

fn setup_scenario() -> (GameRunner, ObjectId, [ObjectId; 3], [ObjectId; 3]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Petals of Insight", false, PETALS_OF_INSIGHT_ORACLE)
        .with_mana_cost(petals_cost())
        .id();

    // 5 Islands to pay {4}{U}.
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }

    // Add cards bottom-to-top so library[0] is Top A:
    let deep_c = scenario.add_card_to_library_top(P0, "Deep C");
    let deep_b = scenario.add_card_to_library_top(P0, "Deep B");
    let deep_a = scenario.add_card_to_library_top(P0, "Deep A");
    let top_c = scenario.add_card_to_library_top(P0, "Top C");
    let top_b = scenario.add_card_to_library_top(P0, "Top B");
    let top_a = scenario.add_card_to_library_top(P0, "Top A");

    let runner = scenario.build();
    (
        runner,
        spell,
        [top_a, top_b, top_c],
        [deep_a, deep_b, deep_c],
    )
}

#[test]
pub fn petals_of_insight_ast_shape() {
    let parsed = parse_oracle_text(
        PETALS_OF_INSIGHT_ORACLE,
        "Petals of Insight",
        &[],
        &["Sorcery".to_string(), "Arcane".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1, "must have 1 root spell ability");
    let root = &parsed.abilities[0];

    // Root is look at top 3 cards (keep_count=0).
    match root.effect.as_ref() {
        Effect::Dig {
            count,
            keep_count,
            source,
            ..
        } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
            assert_eq!(*keep_count, Some(0));
            assert_eq!(*source, DigSource::Library);
        }
        other => panic!("expected Root Dig, got {:?}", other),
    }

    // Sub-ability is optional: put those cards on bottom of library in any order.
    let sub = root
        .sub_ability
        .as_ref()
        .expect("root must have sub_ability");
    assert!(sub.optional, "sub_ability must be optional ('You may')");
    match sub.effect.as_ref() {
        Effect::Dig {
            source,
            keep_count,
            rest_destination,
            rest_order,
            ..
        } => {
            assert_eq!(*source, DigSource::PriorLook);
            assert_eq!(*keep_count, Some(0));
            assert_eq!(*rest_destination, Some(Zone::Library));
            assert_eq!(*rest_order, DigRestOrder::PlayerChoice);
        }
        other => panic!("expected PriorLook Dig, got {:?}", other),
    }

    // "If you do" branch: return Petals of Insight to owner's hand.
    let if_you_do = sub
        .sub_ability
        .as_ref()
        .expect("sub_ability must have 'If you do' sub_ability");
    assert!(
        if_you_do
            .condition
            .as_ref()
            .is_some_and(|c| c.is_optional_effect_performed()),
        "condition must be OptionalEffectPerformed"
    );
    match if_you_do.effect.as_ref() {
        Effect::Bounce { target, .. } => {
            assert_eq!(*target, TargetFilter::SelfRef);
        }
        other => panic!("expected Bounce SelfRef, got {:?}", other),
    }

    // "Otherwise" branch: draw three cards. Attached as else_ability of the if-you-do gate.
    let otherwise = if_you_do
        .else_ability
        .as_ref()
        .expect("if_you_do must have 'Otherwise' else_ability");
    match otherwise.effect.as_ref() {
        Effect::Draw { count, target } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
            assert_eq!(*target, TargetFilter::Controller);
        }
        other => panic!("expected Draw 3, got {:?}", other),
    }
}

#[test]
pub fn petals_of_insight_accept_puts_cards_on_bottom_and_bounces_to_hand() {
    let (mut runner, spell, [top_a, top_b, top_c], [deep_a, _, _]) = setup_scenario();

    // Cast Petals of Insight and stop at the optional decision prompt.
    let stopped = runner.cast(spell).commit().stop_at_optional().resolve();

    assert!(
        matches!(
            stopped.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "CR 608.2c: must pause for optional bottom placement choice, got {:?}",
        stopped.final_waiting_for()
    );

    // Private look IDs must hold the top 3 cards looked at.
    assert_eq!(
        runner.state().private_look_ids,
        vec![top_a, top_b, top_c],
        "CR 701.20e: private look IDs must contain the 3 cards"
    );

    // Player accepts the optional choice: put those cards on the bottom of library.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional effect");

    // CR 701.20e + CR 401.4: with 3 cards "in any order", the player must
    // announce a permutation. The test submits the reverse of the looked-at
    // order to verify the choice is respected.
    let cards = match &runner.state().waiting_for {
        WaitingFor::DigBottomOrder { cards, .. } => cards.clone(),
        other => panic!("expected DigBottomOrder prompt, got {:?}", other),
    };
    // Submit reverse order (top_c, top_b, top_a) to verify choice is honored.
    let reverse_order: Vec<ObjectId> = cards.iter().rev().copied().collect();
    runner
        .act(GameAction::SelectCards {
            cards: reverse_order.clone(),
        })
        .expect("submit bottom order");

    // Stack must be empty and game waiting for priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "game must return to priority after resolution, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");

    // Petals of Insight must be back in P0's hand, not in graveyard.
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "Petals of Insight must return to its owner's hand"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&spell),
        "P0's hand must contain Petals of Insight"
    );
    assert!(
        !runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&spell),
        "P0's graveyard must NOT contain Petals of Insight"
    );

    // Library top must now be Deep A.
    let p0_library = &runner.state().players[P0.0 as usize].library;
    assert_eq!(
        p0_library[0], deep_a,
        "new library top must be Deep A after top 3 were put on bottom"
    );

    // Bottom of library must contain the 3 looked-at cards.
    let len = p0_library.len();
    assert_eq!(len, 6, "all 6 cards remain in library");
    let bottom_ids: Vec<ObjectId> = p0_library.iter().skip(len - 3).copied().collect();
    assert_eq!(
        bottom_ids, reverse_order,
        "bottom of library must match the submitted order"
    );

    // CR 701.20e: the look window ends with the placement it gated — nothing
    // downstream may still read the finished peek once the cards have moved.
    assert!(
        runner.state().private_look_ids.is_empty(),
        "look window must close after the bottom placement, got {:?}",
        runner.state().private_look_ids
    );
    assert_eq!(
        runner.state().private_look_player,
        None,
        "look window must close after the bottom placement"
    );
}

/// CR 701.20e: while the bottom-order prompt is open, only the looking player
/// may learn which library cards were looked at. The opponent's serialized
/// projection carries neither the private-look ledger nor the prompt's ids.
#[test]
pub fn petals_of_insight_bottom_order_prompt_hides_looked_at_ids_from_opponent() {
    let (mut runner, spell, looked, _) = setup_scenario();
    let _ = runner.cast(spell).commit().stop_at_optional().resolve();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional effect");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DigBottomOrder { player: P0, .. }
        ),
        "reach guard: must be at the bottom-order prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().private_look_ids,
        looked.to_vec(),
        "reach guard: the private-look ledger is live during the prompt"
    );

    let own = filter_state_for_viewer(runner.state(), P0);
    assert_eq!(own.private_look_ids, looked.to_vec());
    assert!(matches!(
        &own.waiting_for,
        WaitingFor::DigBottomOrder { cards, .. } if *cards == looked.to_vec()
    ));
    // CR 701.20e: redaction is scoped to viewers without private access — the
    // wire projection the looker's own client receives must keep the ledger.
    let own_wire = serde_json::to_value(&own).expect("own projection serializes");
    assert_eq!(
        own_wire.get("private_look_ids"),
        Some(&serde_json::json!(looked.to_vec())),
        "serialized own projection must keep the looked-at ids"
    );

    let opponent = filter_state_for_viewer(runner.state(), P1);
    assert!(
        opponent.private_look_ids.is_empty(),
        "opponent must not receive the looked-at ids, got {:?}",
        opponent.private_look_ids
    );
    assert!(matches!(
        &opponent.waiting_for,
        WaitingFor::DigBottomOrder { cards, .. } if *cards == vec![ObjectId(0); 3]
    ));
    let wire = serde_json::to_value(&opponent).expect("projection serializes");
    assert!(
        wire.get("private_look_ids").is_none(),
        "serialized opponent projection must omit private_look_ids, got {:?}",
        wire.get("private_look_ids")
    );

    // CR 701.20e: an unseated wire has no private access to any player either,
    // so it gets the same redaction as the opponent.
    let unseated = filter_state_for_unseated_viewer(runner.state());
    assert!(
        unseated.private_look_ids.is_empty(),
        "unseated viewer must not receive the looked-at ids, got {:?}",
        unseated.private_look_ids
    );
    assert!(matches!(
        &unseated.waiting_for,
        WaitingFor::DigBottomOrder { cards, .. } if *cards == vec![ObjectId(0); 3]
    ));
    let unseated_wire = serde_json::to_value(&unseated).expect("projection serializes");
    assert!(
        unseated_wire.get("private_look_ids").is_none(),
        "serialized unseated projection must omit private_look_ids, got {:?}",
        unseated_wire.get("private_look_ids")
    );
}

#[test]
pub fn petals_of_insight_decline_draws_three_cards_and_goes_to_graveyard() {
    let (mut runner, spell, [top_a, top_b, top_c], [deep_a, _, _]) = setup_scenario();

    // Cast Petals of Insight and stop at the optional decision prompt.
    let stopped = runner.cast(spell).commit().stop_at_optional().resolve();

    assert!(
        matches!(
            stopped.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "CR 608.2c: must pause for optional bottom placement choice, got {:?}",
        stopped.final_waiting_for()
    );

    // Player declines the optional choice: "Otherwise, draw three cards."
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline optional effect");

    // Stack must be empty and game waiting for priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "game must return to priority after resolution, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");

    // P0 drew the top 3 cards into hand.
    let p0_hand = &runner.state().players[P0.0 as usize].hand;
    assert!(
        p0_hand.contains(&top_a) && p0_hand.contains(&top_b) && p0_hand.contains(&top_c),
        "P0 must have drawn Top A, Top B, and Top C into hand"
    );

    // Petals of Insight was NOT bounced, so it goes to graveyard (CR 608.2m).
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2m: Petals of Insight must go to graveyard upon resolution"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&spell),
        "P0's graveyard must contain Petals of Insight"
    );

    // Library top must now be Deep A.
    let p0_library = &runner.state().players[P0.0 as usize].library;
    assert_eq!(
        p0_library[0], deep_a,
        "new library top must be Deep A after 3 cards drawn"
    );
    assert_eq!(p0_library.len(), 3, "3 cards remain in library");
}

/// CR 401.4 + CR 701.20e: "in any order" is only a choice when there is more
/// than one card to order. With a single card in the library, accepting the
/// bottom placement must NOT pause for a `DigBottomOrder` prompt — there is no
/// permutation to announce — while the accepted "If you do" branch still
/// returns Petals of Insight to its owner's hand.
#[test]
pub fn petals_of_insight_single_looked_at_card_needs_no_bottom_order_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Petals of Insight", false, PETALS_OF_INSIGHT_ORACLE)
        .with_mana_cost(petals_cost())
        .id();
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let only_card = scenario.add_card_to_library_top(P0, "Only Card");
    let mut runner = scenario.build();

    let stopped = runner.cast(spell).commit().stop_at_optional().resolve();
    assert!(
        matches!(
            stopped.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "CR 608.2c: must pause for the optional bottom placement, got {:?}",
        stopped.final_waiting_for()
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional effect");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a single card has no order to announce, so resolution must not pause \
         for DigBottomOrder; got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");

    // Positive guard: the "If you do" branch ran, so the placement resolved.
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "the accepted placement must still resolve the 'If you do' bounce"
    );

    let library = &runner.state().players[P0.0 as usize].library;
    assert_eq!(library.len(), 1, "the single card remains in the library");
    assert_eq!(library[0], only_card, "the single card is the library");
    assert_eq!(
        runner.state().objects[&only_card].zone,
        Zone::Library,
        "the placed card must still be in the library"
    );
}
