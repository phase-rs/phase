//! Liliana, Waker of the Dead [+1] — cross-scope mandatory-impossible
//! decline-tail.
//!
//! Oracle (loyalty ability body):
//!   "Each player discards a card. Each opponent who can't loses 3 life."
//!
//! CR anchors:
//!   - CR 101.3: A player with an empty hand "can't" discard.
//!   - CR 109.5: The decline clause's PRINTED scope is `Opponent` ("each
//!     opponent who can't"), which differs from the parent's `All` scope
//!     ("each player discards"). The body must fire only on iterations
//!     whose scoped player matches the decline clause's PlayerFilter —
//!     i.e., opponents only. The controller, even if they can't discard,
//!     must NOT lose 3 life.
//!   - CR 118.12 (mandatory-cost branch): the "can't" relative clause is
//!     the subject-only sibling of the prepositional "For each opponent
//!     who can't, …".
//!   - CR 608.2c: per-iteration `cost_payment_failed_flag` reset (engine
//!     driver loop) keeps the previous iteration's failure from leaking
//!     forward.
//!   - CR 119.3: directed life loss — the rewritten target is
//!     `Some(ScopedPlayer)`.
//!
//! AST shape (verified by the parser unit test in
//! `parser/oracle_effect/mod.rs::liliana_waker_of_the_dead_plus_one_cross_scope_decline_tail`):
//!   `Discard { player_scope: All }` → `sub_ability: LoseLife {
//!   condition: And { [Not{IfCurrentScopeSucceeded},
//!   ScopedPlayerMatches(Opponent)] }, target: Some(ScopedPlayer),
//!   amount: 3 }`.
//!
//! This file is the discriminating end-to-end regression that the
//! maintainer's review explicitly asked for. On main Liliana parses as
//! `LoseLife { player_scope: Opponent }` with the correct scope but no
//! can't-gate (so all opponents lose life unconditionally). This PR keeps
//! the can't-gate AND restores the cross-scope filter via the
//! `ScopedPlayerMatches` conjunct.
//!
//! Setup: 2 players. Controller (P0) — empty hand (can't discard) + 20
//! life. Opponent (P1) — empty hand (can't discard) + 20 life. After the
//! [+1] resolves: P0 (controller) MUST NOT lose 3 life (CR 109.5);
//! P1 (opponent) MUST lose 3 life. Without the `ScopedPlayerMatches(Opponent)`
//! conjunct, P0 would also lose 3 life — the maintainer's blocking finding.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const LILIANA_PLUS_ONE: &str = "each player discards a card. Each opponent who can't loses 3 life.";

fn liliana_plus_one(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(LILIANA_PLUS_ONE, AbilityKind::Activated);
    build_resolved_from_def(&def, source_id, controller)
}

fn life_of(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// CR 101.3 + CR 109.5: Cross-scope decline-tail. Parent iterates `All`
/// ("each player discards"); decline clause's own scope is `Opponent`
/// ("each opponent who can't loses 3 life"). With BOTH players unable to
/// discard (empty hands), only the opponent loses 3 life — the controller
/// must NOT, because the decline-clause `PlayerFilter::Opponent` excludes
/// them. This is the cross-scope regression guard the maintainer review
/// asked for.
#[test]
fn liliana_waker_plus_one_only_opponents_lose_life_when_no_one_can_discard() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Liliana, Waker of the Dead".to_string(),
        Zone::Battlefield,
    );
    // Both players have EMPTY hands — neither can discard. CR 101.3 makes
    // both iterations' mandatory discards fail. The cross-scope gate
    // narrows which iterations fire the LoseLife body: only those whose
    // scoped player matches `Opponent` relative to the controller (P0).
    let p0_life_before = life_of(&state, PlayerId(0));
    let p1_life_before = life_of(&state, PlayerId(1));

    let ability = liliana_plus_one(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        life_of(&state, PlayerId(0)),
        p0_life_before,
        "P0 is the controller. Even though P0 couldn't discard (empty hand), \
         the decline clause's PlayerFilter is `Opponent` — CR 109.5 \
         restricts the life-loss body to opponents only. If this fails, \
         the cross-scope `ScopedPlayerMatches(Opponent)` conjunct was \
         dropped and P0 lost life through their own decline branch."
    );
    assert_eq!(
        life_of(&state, PlayerId(1)),
        p1_life_before - 3,
        "P1 is an opponent. Their empty hand makes their mandatory discard \
         fail (CR 101.3), and they match the decline clause's `Opponent` \
         scope, so they lose exactly 3 life."
    );
}

/// CR 101.3 + CR 109.5: Negative control — when an opponent CAN discard,
/// they do NOT lose life. Pins the can't-gate (the `Not{IfCurrentScopeSucceeded}`
/// conjunct) end-to-end against the cross-scope class. The controller (P0)
/// has an empty hand (can't discard) — but the `Opponent` scope filter
/// excludes them, so they don't lose life either.
#[test]
fn liliana_waker_plus_one_opponent_who_can_discard_does_not_lose_life() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Liliana, Waker of the Dead".to_string(),
        Zone::Battlefield,
    );
    // P1 (opponent) has a card to discard — their mandatory discard
    // succeeds (CR 101.3 does NOT trigger), so the LoseLife body must
    // NOT fire for P1. P0 (controller) has an empty hand but is excluded
    // by the `Opponent` decline-clause scope.
    create_object(
        &mut state,
        CardId(100),
        PlayerId(1),
        "Forest".to_string(),
        Zone::Hand,
    );
    let p0_life_before = life_of(&state, PlayerId(0));
    let p1_life_before = life_of(&state, PlayerId(1));

    let ability = liliana_plus_one(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        life_of(&state, PlayerId(0)),
        p0_life_before,
        "P0 is the controller — the `Opponent` decline-clause scope \
         excludes them regardless of whether they could discard."
    );
    assert_eq!(
        life_of(&state, PlayerId(1)),
        p1_life_before,
        "P1 discarded successfully — the can't-gate (Not{{IfCurrentScopeSucceeded}}) \
         keeps the LoseLife body silent for this iteration."
    );
}
