//! Integration test for Gran-Gran (GH-87).
//!
//! Oracle text:
//!   Whenever Gran-Gran becomes tapped, draw a card, then discard a card.
//!
//! Parser lowers this to:
//!   execute = Draw(1, Controller)
//!     sub_ability = Discard(1, Controller)
//!
//! Bug (GH-87): When the trigger resolves, the Draw fires but the Discard
//! chain pauses on DiscardChoice. After the player submits SelectCards, the
//! post-choice continuation must drain correctly. The reported symptom is
//! that only one of {draw, discard} visibly fires.
//!
//! This test drives the full chain: Draw (instant) → Discard (DiscardChoice
//! WaitingFor) → SelectCards → final hand state. It verifies:
//!   - After chain entry: WaitingFor::DiscardChoice is active (draw already happened).
//!   - Library shrank by 1 (draw fired).
//!   - After SelectCards: chosen card is in graveyard; hand size = pre-chain + 0.

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply_as_current;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCondition, AbilityKind, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn gran_gran_chain(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let discard = ResolvedAbility::new(
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            random: false,
            unless_filter: None,
            filter: None,
        },
        vec![],
        source_id,
        controller,
    )
    .kind(AbilityKind::Spell);

    ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source_id,
        controller,
    )
    .kind(AbilityKind::Spell)
    .sub_ability(discard)
}

#[test]
fn gran_gran_draw_then_discard_fires_both() {
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let source_id = ObjectId(100);

    // Seed hand with 2 cards so we can discard one.
    let _hand_a = create_object(&mut state, CardId(1), controller, "A".into(), Zone::Hand);
    let hand_b = create_object(&mut state, CardId(2), controller, "B".into(), Zone::Hand);

    // Seed library so Draw has a card to draw.
    let library_card = create_object(
        &mut state,
        CardId(3),
        controller,
        "Top".into(),
        Zone::Library,
    );
    let lib_size_before = state.players[0].library.len();
    let hand_size_before = state.players[0].hand.len();

    let chain = gran_gran_chain(source_id, controller);
    let mut events = Vec::new();

    resolve_ability_chain(&mut state, &chain, &mut events, 0).unwrap();

    // Draw already happened (instant), library shrank by 1, hand grew by 1.
    assert_eq!(state.players[0].library.len(), lib_size_before - 1);
    assert_eq!(state.players[0].hand.len(), hand_size_before + 1);
    assert!(state.players[0].hand.contains(&library_card));

    // Discard paused on WaitingFor::DiscardChoice.
    match &state.waiting_for {
        WaitingFor::DiscardChoice { player, count, .. } => {
            assert_eq!(*player, controller);
            assert_eq!(*count, 1);
        }
        other => panic!("expected DiscardChoice, got {:?}", other),
    }

    // Submit the player's discard selection (hand_b).
    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![hand_b],
        },
    )
    .unwrap();

    // After SelectCards: hand_b is in graveyard, drawn card still in hand.
    assert!(state.players[0].graveyard.contains(&hand_b));
    assert!(state.players[0].hand.contains(&library_card));
    // Net hand size = before + 1 (drew) - 1 (discarded) = before.
    assert_eq!(state.players[0].hand.len(), hand_size_before);
}

#[test]
fn gran_gran_empty_hand_still_draws() {
    // CR 101.4: "Draw, then discard" resolves sequentially — draw always fires.
    // If hand is empty after drawing, discard resolves as a no-op (or no choice).
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let source_id = ObjectId(100);

    // Hand starts empty; seed one card in library so Draw picks it up.
    let library_card = create_object(
        &mut state,
        CardId(3),
        controller,
        "Top".into(),
        Zone::Library,
    );
    assert_eq!(state.players[0].hand.len(), 0);

    let chain = gran_gran_chain(source_id, controller);
    let mut events = Vec::new();

    resolve_ability_chain(&mut state, &chain, &mut events, 0).unwrap();

    // Draw fired (card entered hand from library), then the forced-discard
    // branch auto-discarded the only card in hand: library_card is now in
    // graveyard with hand empty.
    if matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }) {
        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![library_card],
            },
        )
        .unwrap();
    }
    assert!(state.players[0].graveyard.contains(&library_card));
    assert_eq!(state.players[0].hand.len(), 0);
}

// ─────────────────────────────────────────────────────────────────────────
// Abandon Attachments / IfYouDo-gated draw — the companion class (V2).
// Oracle: "You may discard a card. If you do, draw two cards."
// Structure:
//   outer = Discard(1, optional=true)
//     sub_ability = Draw(2, condition=IfYouDo)
//
// Flow:
//   1. OptionalEffectChoice(Yes) → context.optional_effect_performed = true
//   2. Discard resolves → WaitingFor::DiscardChoice
//   3. Sub Draw is stashed as pending_continuation with context propagated
//   4. SelectCards → drain_pending_continuation → Draw condition evaluates
//      `optional_effect_performed && !cost_payment_failed_flag`
//
// The bug class (GH-87 V2): if the continuation fires its sub-ability with a
// stale `cost_payment_failed_flag` from the discard step, the draw is gated off
// even though the player successfully discarded.
// ─────────────────────────────────────────────────────────────────────────

fn abandon_attachments_chain(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let mut draw_sub = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
        vec![],
        source_id,
        controller,
    )
    .kind(AbilityKind::Spell);
    draw_sub.condition = Some(AbilityCondition::IfYouDo);

    let mut outer = ResolvedAbility::new(
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            random: false,
            unless_filter: None,
            filter: None,
        },
        vec![],
        source_id,
        controller,
    )
    .kind(AbilityKind::Spell)
    .sub_ability(draw_sub);
    outer.optional = true;
    outer
}

#[test]
fn abandon_attachments_discard_then_draw_fires() {
    use engine::types::ability::SpellContext;
    use engine::types::game_state::PendingContinuation;

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let source_id = ObjectId(100);

    // Give controller one card in hand (the discard target).
    let hand_card = create_object(&mut state, CardId(1), controller, "H".into(), Zone::Hand);
    // Two cards in library so Draw(2) succeeds.
    let lib_top = create_object(
        &mut state,
        CardId(2),
        controller,
        "L1".into(),
        Zone::Library,
    );
    let lib_second = create_object(
        &mut state,
        CardId(3),
        controller,
        "L2".into(),
        Zone::Library,
    );

    // Simulate OptionalEffectChoice::Yes: strip `optional`, set
    // optional_effect_performed on context, and resolve the chain.
    let mut chain = abandon_attachments_chain(source_id, controller);
    chain.optional = false;
    let ctx = SpellContext {
        optional_effect_performed: true,
        ..SpellContext::default()
    };
    chain.context = ctx.clone();
    if let Some(sub) = chain.sub_ability.as_mut() {
        sub.context = ctx;
    }

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &chain, &mut events, 0).unwrap();

    // Discard should have paused on DiscardChoice (or auto-discarded since hand==count).
    // With hand==count==1, the forced-discard branch auto-resolves without WaitingFor.
    // In that path the sub_ability is NOT stashed as pending_continuation —
    // it is resolved inline via the sub_ability chain after Discard completes.
    // So the draw may already have happened by now.

    let drew_inline = state
        .objects
        .get(&lib_top)
        .is_some_and(|o| o.zone == Zone::Hand);
    if !drew_inline {
        // Forced-path didn't drain the sub; discard is in WaitingFor.
        match &state.waiting_for {
            WaitingFor::DiscardChoice { .. } => {
                apply_as_current(
                    &mut state,
                    GameAction::SelectCards {
                        cards: vec![hand_card],
                    },
                )
                .unwrap();
            }
            WaitingFor::Priority { .. } => {
                // Forced path completed + sub drained already — that's fine.
            }
            other => panic!("unexpected waiting_for: {:?}", other),
        }
    }

    // Expected: hand_card in graveyard, both top cards drawn.
    assert!(
        state.players[0].graveyard.contains(&hand_card),
        "discarded card must be in graveyard"
    );
    assert!(
        state.players[0].hand.contains(&lib_top),
        "first drawn card must be in hand (IfYouDo draw fired)"
    );
    assert!(
        state.players[0].hand.contains(&lib_second),
        "second drawn card must be in hand (Draw(2) completed)"
    );
    // Guard: no stale cost_payment_failed_flag left behind.
    assert!(
        !state.cost_payment_failed_flag,
        "cost_payment_failed_flag must not linger after successful IfYouDo path"
    );
    // Suppress unused warning in the auto-drain path.
    let _ = PendingContinuation::new(Box::new(chain.clone()));
}

#[test]
fn abandon_attachments_empty_hand_vetoes_draw() {
    // Edge: player has `optional: true`, accepts, but has no card to discard.
    // CR 608.2c: empty-hand discard is a no-op → cost_payment_failed_flag set →
    // IfYouDo draw must NOT fire.
    use engine::types::ability::SpellContext;

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let source_id = ObjectId(100);

    // Hand empty; library has cards (they should NOT be drawn).
    let lib_top = create_object(
        &mut state,
        CardId(2),
        controller,
        "L1".into(),
        Zone::Library,
    );
    let _lib_second = create_object(
        &mut state,
        CardId(3),
        controller,
        "L2".into(),
        Zone::Library,
    );

    let mut chain = abandon_attachments_chain(source_id, controller);
    chain.optional = false;
    let ctx = SpellContext {
        optional_effect_performed: true,
        ..SpellContext::default()
    };
    chain.context = ctx.clone();
    if let Some(sub) = chain.sub_ability.as_mut() {
        sub.context = ctx;
    }

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &chain, &mut events, 0).unwrap();

    // No card drawn — IfYouDo gated off by cost_payment_failed_flag.
    assert!(
        !state.players[0].hand.contains(&lib_top),
        "Draw must NOT fire when discard was a no-op"
    );
    assert_eq!(state.players[0].hand.len(), 0);
}

// Stronger coverage: force the *interactive* DiscardChoice path (hand > count),
// so the sub Draw is actually stashed as `pending_continuation` and drained
// after `GameAction::SelectCards`. This pins the `optional_effect_performed`
// context propagation across the `WaitingFor::DiscardChoice` boundary — the
// architectural hotspot flagged in GH-87 V2 ("was-cost-paid evaluated against
// wrong state snapshot").
#[test]
fn abandon_attachments_interactive_discard_drains_continuation_draw() {
    use engine::types::ability::SpellContext;

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let source_id = ObjectId(100);

    // Two cards in hand so DiscardChoice is interactive (hand > count).
    let hand_a = create_object(&mut state, CardId(1), controller, "A".into(), Zone::Hand);
    let _hand_b = create_object(&mut state, CardId(2), controller, "B".into(), Zone::Hand);
    // Two cards in library so Draw(2) has fuel.
    let lib_top = create_object(
        &mut state,
        CardId(3),
        controller,
        "L1".into(),
        Zone::Library,
    );
    let lib_second = create_object(
        &mut state,
        CardId(4),
        controller,
        "L2".into(),
        Zone::Library,
    );

    let mut chain = abandon_attachments_chain(source_id, controller);
    chain.optional = false;
    let ctx = SpellContext {
        optional_effect_performed: true,
        ..SpellContext::default()
    };
    chain.context = ctx.clone();
    if let Some(sub) = chain.sub_ability.as_mut() {
        sub.context = ctx;
    }

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &chain, &mut events, 0).unwrap();

    // Outer Discard MUST have paused on DiscardChoice (the stashed-continuation path).
    assert!(
        matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }),
        "expected interactive DiscardChoice, got {:?}",
        state.waiting_for
    );
    // No draw yet — it's sitting in pending_continuation waiting for the player.
    assert!(!state.players[0].hand.contains(&lib_top));

    // Submit the discard.
    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![hand_a],
        },
    )
    .unwrap();

    // After drain: discarded card in graveyard, IfYouDo draw fired.
    assert!(state.players[0].graveyard.contains(&hand_a));
    assert!(
        state.players[0].hand.contains(&lib_top),
        "IfYouDo Draw must fire after DiscardChoice drains"
    );
    assert!(
        state.players[0].hand.contains(&lib_second),
        "Both cards of Draw(2) must be drawn"
    );
    assert!(
        !state.cost_payment_failed_flag,
        "Success path must clear cost_payment_failed_flag"
    );
}
