//! Plaguecrafter (DOM, M19 reprint) — subject-only mandatory-impossible
//! decline-tail.
//!
//! Oracle (ETB trigger body):
//!   "When this creature enters, each player sacrifices a creature or
//!    planeswalker of their choice. Each player who can't discards a card."
//!
//! CR anchors:
//!   - CR 101.3: Any part of an instruction that's impossible to perform is
//!     ignored. A player with no creature or planeswalker on the battlefield
//!     "can't" sacrifice.
//!   - CR 109.5: The body's implicit recipient ("discards a card" with no
//!     stated subject) binds to the per-iteration scoped player, not the
//!     printed ability controller — the shared
//!     `rebind_clause_recipients_with(_, rebind_subject_only_body_recipient)`
//!     walker rewrites `Discard.target` from `Controller` → `ScopedPlayer`.
//!   - CR 118.12 (mandatory-cost branch): The "can't" relative-clause is the
//!     subject-only sibling of the prepositional "For each opponent who
//!     can't, …" — both gate on `Not{IfCurrentScopeSucceeded}`. Plaguecrafter
//!     is the same-scope class: the parent's `player_scope: All` and the
//!     decline clause's PlayerFilter `All` agree, so the
//!     `ScopedPlayerMatches(All)` conjunct that fires for every iteration is
//!     trivially true. Cross-scope cards (Liliana, Waker of the Dead — parent
//!     `All`, decline `Opponent`) rely on that conjunct for correctness.
//!   - CR 608.2c: Each scoped iteration is a fresh sub-resolution; the
//!     per-iteration `cost_payment_failed_flag` reset (effects/mod.rs driver
//!     loop) is the load-bearing invariant that keeps a prior player's
//!     failure from leaking forward.
//!   - CR 701.21 (Sacrifice): the parent effect's keyword action — "each
//!     player sacrifices a creature or planeswalker of their choice".
//!   - CR 701.9 (Discard): the body effect's keyword action — "Each player
//!     who can't discards a card", rewritten to target the per-iteration
//!     ScopedPlayer.
//!
//! AST shape (verified by the parser unit test in
//! `parser/oracle_effect/mod.rs::plaguecrafter_etb_lowers_subject_only_decline_tail`):
//!   `Sacrifice { player_scope: All }` → `sub_ability: Discard {
//!   condition: And { [Not{IfCurrentScopeSucceeded},
//!   ScopedPlayerMatches(All)] }, target: ScopedPlayer,
//!   sub_link: ContinuationStep, player_scope: None (inherits parent) }`.
//!
//! Test: 2 players, discriminating setup.
//!   - Controller (P0) — has 1 creature on the battlefield and 2 cards in
//!     hand. Can sacrifice → flag stays false → discard sub-ability does
//!     NOT fire for this iteration.
//!   - Opponent (P1) — empty battlefield and 1 card in hand. Cannot
//!     sacrifice → mandatory-impossible (CR 101.3) sets the flag →
//!     `Not{IfCurrentScopeSucceeded}` evaluates true → discard fires for
//!     this player, rewritten Controller → ScopedPlayer so P1 discards from
//!     their own hand.
//!
//! The 4-player APNAP fan-out variant is out of scope here — the parser
//! unit test pins the per-iteration scope semantics; the runtime invariant
//! (per-iteration flag reset) is already covered by `refurbished_familiar.rs`.
//! This file's job is to lock in the recipient rebind on the actual
//! Plaguecrafter Oracle text end-to-end.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ETB_BODY: &str = "each player sacrifices a creature or planeswalker of their choice. Each \
     player who can't discards a card.";

fn plaguecrafter_etb(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(ETB_BODY, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

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

fn add_battlefield_creature(
    state: &mut GameState,
    card_id: u64,
    player: PlayerId,
    name: &str,
) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    // Mark the object as a creature so the sacrifice target legality check
    // ("a creature or planeswalker") passes.
    let obj = state
        .objects
        .get_mut(&oid)
        .expect("just-created battlefield object");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    oid
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

/// CR 101.3 + CR 109.5: Plaguecrafter's decline-tail rebinds the implicit
/// discard recipient to the per-iteration scoped player. P1 has no
/// creature/planeswalker, so its mandatory sacrifice fails (flag set);
/// `Not{IfCurrentScopeSucceeded}` then fires the discard for P1 only,
/// targeting P1's own hand (NOT the controller's). P0 sacrifices its
/// creature, so the flag stays false for P0's iteration and P0 does not
/// discard.
///
/// This regression guards the parser's recipient rewrite via the shared
/// `rebind_clause_recipients_with(_, rebind_subject_only_body_recipient)`
/// walker: without it, the discard would target the printed controller, so
/// P0 would discard regardless of who could/couldn't sacrifice.
#[test]
fn plaguecrafter_only_player_who_cant_sacrifice_discards() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Plaguecrafter".to_string(),
        Zone::Battlefield,
    );
    // P0 has a creature to sacrifice and 2 cards in hand (untouched if
    // recipient rebind is correct — P0 sacrifices, no discard for P0).
    let _p0_creature = add_battlefield_creature(&mut state, 100, PlayerId(0), "Bear");
    add_hand_cards(&mut state, 200, PlayerId(0), 2);
    // P1 has no creature/planeswalker (mandatory sacrifice fails) and 1 card
    // in hand (the discard target).
    add_hand_cards(&mut state, 300, PlayerId(1), 1);

    let p0_hand_before = hand_len(&state, PlayerId(0));
    let p1_hand_before = hand_len(&state, PlayerId(1));
    assert_eq!(p0_hand_before, 2);
    assert_eq!(p1_hand_before, 1);

    let ability = plaguecrafter_etb(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // P1 (no creature/planeswalker) was the only player who couldn't
    // sacrifice — they must discard exactly 1 from their own hand. The
    // recipient rebind is the load-bearing assertion: without it, the
    // discard's target would still be Controller (P0) and P0's hand would
    // shrink instead.
    assert_eq!(
        hand_len(&state, PlayerId(0)),
        p0_hand_before,
        "P0 sacrificed its Bear, so the can't-discard sub-effect must NOT \
         fire for P0 — its hand stays at {p0_hand_before}. If this fails, \
         the subject-only decline-tail's recipient rebind was skipped and \
         Discard.target is still Controller."
    );
    assert!(
        hand_len(&state, PlayerId(1)) < p1_hand_before,
        "P1 had no creature/planeswalker (CR 101.3 mandatory-impossible). \
         Its iteration must rewrite Discard.target → ScopedPlayer and reduce \
         P1's hand by exactly 1, got {} → {}.",
        p1_hand_before,
        hand_len(&state, PlayerId(1))
    );
    assert_eq!(
        hand_len(&state, PlayerId(1)),
        p1_hand_before - 1,
        "P1 discards exactly 1 card from their own hand"
    );
}
