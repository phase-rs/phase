//! Refurbished Familiar (MH3) — mandatory-impossible decline-tail.
//!
//! Oracle (ETB trigger body):
//!   "Each opponent discards a card. For each opponent who can't, you draw
//!   a card."
//!
//! CR anchors:
//!   - CR 101.3: Any part of an instruction that's impossible to perform is
//!     ignored. An opponent with an empty hand "can't" discard.
//!   - CR 608.2c: Each scoped iteration is a fresh sub-resolution; the
//!     per-iteration `cost_payment_failed_flag` reset (effects/mod.rs driver
//!     loop) is the load-bearing invariant that keeps a prior opponent's
//!     failure from leaking forward.
//!   - CR 118.12 (mandatory-cost branch): The "can't" clause checks whether
//!     the mandatory instruction succeeded — distinct from the "doesn't"
//!     optional-decline branch (Braids-class).
//!
//! AST shape (verified by the parser unit test in
//! `parser/oracle_effect/mod.rs::for_each_opponent_who_cant_lowers_to_if_current_scope_succeeded`):
//!   `Discard { player_scope: Opponent }` → `sub_ability: Draw {
//!   condition: Not{IfCurrentScopeSucceeded}, target: OriginalController,
//!   sub_link: ContinuationStep }`.
//!
//! Tests:
//!   - `refurbished_familiar_synchronous_draws_only_for_cant_opponent`: 2
//!     opponents, A=0 cards (can't discard → flag set true → draw fires),
//!     B=1 card (auto-discards → flag stays false → draw does NOT fire).
//!     Purely synchronous (no `WaitingFor` raised). Regresses the per-iteration
//!     reset: without it, B would inherit A's `true` flag and erroneously
//!     trigger a second draw.
//!   - `refurbished_familiar_resumed_iteration_isolated_from_cant_opponent`:
//!     2 opponents, A=0 cards (synchronous failure, draw fires for A), B=2
//!     cards (interactive `DiscardChoice` pause). After B picks a card, B's
//!     drained sub_ability evaluates `!cost_payment_failed_flag = true`,
//!     `Not = false` — B's draw does NOT fire. Only A's did.
//!
//! Aclazotz, Deepest Betrayal (LCI) is not separately tested — same AST
//! building block, same engine path. The parser unit test covers the AST
//! contract; the runtime tests here cover the per-iteration flag lifecycle.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ETB_BODY: &str =
    "Each opponent discards a card. For each opponent who can't, you draw a card.";

/// Build the ETB trigger body as a `ResolvedAbility` controlled by
/// `controller`, with `source_id` as the source permanent.
fn refurbished_familiar_etb(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(ETB_BODY, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

/// Put `n` cards into `player`'s hand.
fn add_hand_cards(state: &mut GameState, base_card_id: u64, player: PlayerId, n: usize) {
    for i in 0..n {
        create_object(
            state,
            CardId(base_card_id + i as u64),
            player,
            "Forest".to_string(),
            Zone::Hand,
        );
    }
}

/// Seed `n` cards into `player`'s library so `Draw` has something to draw.
fn seed_library(state: &mut GameState, base_card_id: u64, player: PlayerId, n: usize) {
    for i in 0..n {
        let card = create_object(
            state,
            CardId(base_card_id + i as u64),
            player,
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .players
            .iter_mut()
            .find(|p| p.id == player)
            .expect("player exists")
            .library
            .push_back(card);
    }
}

fn hand_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Test 1 — synchronous mixed. 2 opponents:
///   - A (P1) = 0 cards → mandatory discard fails (CR 101.3 / discard.rs:162
///     sets `cost_payment_failed_flag = true`). Sub_ability draws for the
///     controller.
///   - B (P2) = 1 card → auto-discards at `discard.rs:185-202`; flag stays
///     false. Sub_ability does NOT fire.
///
/// Regresses the per-iteration `cost_payment_failed_flag = false` reset at
/// the top of the player-scope driver loop. Without it, A's `true` would
/// persist into B's iteration: B's `!cost_payment_failed_flag = !true =
/// false`, `Not = true` → controller would erroneously draw 2 cards.
#[test]
fn refurbished_familiar_synchronous_draws_only_for_cant_opponent() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Refurbished Familiar".to_string(),
        Zone::Battlefield,
    );
    // A has empty hand — mandatory discard fails.
    // B has exactly 1 card — count == hand_cards.len(), auto-resolves.
    add_hand_cards(&mut state, 200, PlayerId(2), 1);
    // Controller may draw once (for A only).
    seed_library(&mut state, 300, PlayerId(0), 4);

    let p0_hand_before = hand_len(&state, PlayerId(0));

    let ability = refurbished_familiar_etb(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "purely synchronous — no choice prompt should be raised, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before + 1,
        "controller draws exactly 1 card (for A only — A couldn't discard); B \
         could discard so its sub_ability must not fire. Without the \
         per-iteration cost_payment_failed_flag reset, A's failure would \
         leak into B and the controller would draw 2."
    );
    assert_eq!(
        hand_len(&state, PlayerId(1)),
        0,
        "A's hand was empty and stays empty"
    );
    assert_eq!(
        hand_len(&state, PlayerId(2)),
        0,
        "B auto-discarded its one card"
    );
}

/// Test 2 — pause-resume isolation. 2 opponents:
///   - A (P1) = 0 cards → synchronous mandatory failure; sub_ability draws
///     for controller in-line.
///   - B (P2) = 2 cards → mandatory `Discard 1` falls through to the
///     interactive path (`discard.rs:203-216` / `WaitingFor::DiscardChoice`).
///     The sub_ability is stashed in an `AbilityContinuationFrame` at
///     `effects/mod.rs:3226-3234`.
///
/// After B submits `SelectCards` (`engine_resolution_choices.rs:1276-1402`),
/// the drained sub_ability re-evaluates `IfCurrentScopeSucceeded`. The
/// interactive completion path does not touch `cost_payment_failed_flag`,
/// so flag is `false`, `!false = true`, `Not = false` — B's draw does NOT
/// fire. Only A's did.
///
/// This is the case v3 reviewers flagged as the lifecycle discriminator —
/// it would fail under the v3 ledger design (ledger writes fire after the
/// per-iteration chain returns, never before the drained sub_ability
/// dispatches), and passes under v4 (direct flag read).
#[test]
fn refurbished_familiar_resumed_iteration_isolated_from_cant_opponent() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Refurbished Familiar".to_string(),
        Zone::Battlefield,
    );
    // A: empty hand — synchronous mandatory-impossible.
    // B: 2 cards — must choose 1 to discard (interactive).
    let b_card_a = create_object(
        &mut state,
        CardId(200),
        PlayerId(2),
        "Forest".to_string(),
        Zone::Hand,
    );
    create_object(
        &mut state,
        CardId(201),
        PlayerId(2),
        "Forest".to_string(),
        Zone::Hand,
    );
    // Controller may draw at most once (for A only).
    seed_library(&mut state, 300, PlayerId(0), 4);

    let p0_hand_before = hand_len(&state, PlayerId(0));

    let ability = refurbished_familiar_etb(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // After A's iteration (synchronous failure → controller drew 1) and
    // B's iteration entered interactive Discard, we expect a DiscardChoice
    // for B and the controller's hand to already reflect the +1 from A.
    let (b_prompt_player, _b_count) = match &state.waiting_for {
        WaitingFor::DiscardChoice { player, count, .. } => (*player, *count),
        other => panic!("expected DiscardChoice for B's interactive discard, got {other:?}"),
    };
    assert_eq!(b_prompt_player, PlayerId(2));
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before + 1,
        "controller draws once for A (synchronous mandatory failure) before \
         B's interactive choice opens"
    );

    // B selects a card to discard. Without per-iteration reset OR the
    // direct-flag-read condition, the resumed sub_ability would evaluate
    // incorrectly and either fire (drawing a second card) or fail to be
    // dispatched at all.
    apply(
        &mut state,
        PlayerId(2),
        GameAction::SelectCards {
            cards: vec![b_card_a],
        },
    )
    .expect("B's discard selection should succeed");

    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before + 1,
        "after B resumes and successfully discards, B's sub_ability must NOT \
         fire — controller still has only the 1 card drawn for A"
    );
    assert_eq!(
        hand_len(&state, PlayerId(2)),
        1,
        "B discarded 1 of its 2 cards"
    );
    assert_eq!(hand_len(&state, PlayerId(1)), 0, "A's hand stays empty");
}
