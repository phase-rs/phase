//! Integration tests for Wernog, Rider's Chaplain's "you investigate X times,
//! where X is one plus the number of opponents who investigated this way"
//! clause (CR 608.2c + CR 109.5 + CR 701.16a).
//!
//! The third sub-ability already parses with
//! `repeat_for: Variable("one plus the number of opponents who investigated
//! this way")` because the inner player-action count did not resolve. Once the
//! "[population] who investigated this way" combinator and the
//! `PlayerActionKind::Investigate` ledger event are in place, the inner count
//! resolves to `PlayerCount { PerformedActionThisWay { Opponent, Investigate } }`
//! and the wrapping "one plus" produces `Offset(+1, …)`.
//!
//! This mirrors `tempt_with_discovery.rs` — the identical "each opponent may X,
//! then you do X once per opponent who did" machinery, but for Investigate
//! instead of SearchLibrary.
//!
//! OUT OF SCOPE: clause 2 ("each opponent who doesn't loses 1 life") currently
//! parses unconditionally (no decline-gate linking it to clause 1's optional
//! investigate), so every opponent loses 1 life regardless of whether they
//! investigated. That is a separate pre-existing parser gap (the decline-gate),
//! not part of this count fix, and these tests deliberately assert nothing
//! about clause-2 life loss.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::events::PlayerActionKind;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

const WERNOG_ORACLE: &str = "When Wernog, Rider's Chaplain enters or leaves the battlefield, \
     each opponent may investigate. Each opponent who doesn't loses 1 life. \
     You investigate X times, where X is one plus the number of opponents who \
     investigated this way.";

fn count_clues(state: &GameState) -> usize {
    state.objects.values().filter(|o| o.name == "Clue").count()
}

/// Build a fresh `n`-player game and the resolved ETB trigger ability for
/// Wernog (controller = P0). Returns the game state and the ability.
fn make_game_and_ability(num_players: u8) -> (GameState, ResolvedAbility) {
    let parsed = engine::parser::oracle::parse_oracle_text(
        WERNOG_ORACLE,
        "Wernog, Rider's Chaplain",
        &[],
        &["Creature".to_string()],
        &[],
    );
    // The enters-the-battlefield trigger is the first of the two zone-change
    // triggers; both share the identical body.
    let def = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("Wernog ETB trigger must have an execute body");
    let ability = build_resolved_from_def(def, ObjectId(9000), PlayerId(0));
    let state = GameState::new(FormatConfig::standard(), num_players, 42);
    (state, ability)
}

/// CR 608.2c + CR 109.5 + CR 701.16a: With 2 opponents who both investigate,
/// the clause-3 `repeat_for` count resolves to `1 + 2 = 3` — P0 investigates 3
/// times. Total Clue tokens = P1(1) + P2(1) + P0(3) = 5.
///
/// This drives the real `player_scope: Opponent` fan-out through `apply()`:
/// each opponent's `Effect::Investigate` emits
/// `GameEvent::PlayerPerformedAction { Investigate }`, which the generic scan
/// records into `player_actions_this_way` keyed by the *scoped opponent* (the
/// driver rebinds `ability.controller` to each opponent). The clause-3 count
/// then reads that ledger.
#[test]
fn wernog_clause3_count_is_one_plus_two_investigating_opponents() {
    let (mut state, ability) = make_game_and_ability(3);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // Clause 1 fans out over opponents (APNAP from P0 → P1 then P2). P1 is
    // prompted first.
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(1),
                ..
            }
        ),
        "expected P1 to be prompted to investigate, got {:?}",
        state.waiting_for
    );

    // P1 accepts — investigates. Ledger now records P1 (the scoped opponent).
    apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .unwrap();
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(1), PlayerActionKind::Investigate)),
        "P1's investigate must be recorded against P1 (the scoped opponent), \
         got {:?}",
        state.player_actions_this_way
    );

    // P2 accepts — investigates. This is the final clause-1 iteration; clause-2
    // (LoseLife, out of scope) and clause-3 (the repeat_for self-investigate)
    // then run inside the same top-level resolution, so the ledger is fully
    // populated with both opponents BEFORE the clause-3 count resolves.
    apply(
        &mut state,
        PlayerId(2),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .unwrap();

    // Resolution completed — no further opponent prompts pending.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "all clauses should have resolved, got {:?}",
        state.waiting_for
    );

    // Both opponents recorded as having investigated this way.
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(1), PlayerActionKind::Investigate)),
        "P1 must remain recorded after full resolution"
    );
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(2), PlayerActionKind::Investigate)),
        "P2 must be recorded after full resolution"
    );

    // X = 1 + 2 = 3 → P0 investigated 3 times (clause 3), so the controller
    // also appears in the ledger.
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(0), PlayerActionKind::Investigate)),
        "controller P0 must have investigated via the clause-3 repeat_for"
    );

    // Total Clues = P1(1) + P2(1) + P0(X=3) = 5. If the inner count had stayed
    // an opaque Variable (resolving to 0), the "one plus" Offset would give
    // X=1 and the total would be 3, not 5.
    assert_eq!(
        count_clues(&state),
        5,
        "expected 5 Clue tokens: 1 per investigating opponent + (1 + 2) for P0; \
         a wrong count means the clause-3 X did not resolve to 1 + investigating \
         opponents"
    );
}

/// CR 608.2c + CR 109.5: Boundary — when no opponent investigates, the clause-3
/// count resolves to `1 + 0 = 1`, so P0 investigates exactly once. Total Clues
/// = P0(1) = 1.
#[test]
fn wernog_clause3_count_is_one_when_no_opponent_investigates() {
    let (mut state, ability) = make_game_and_ability(3);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // Both opponents decline.
    apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: false },
    )
    .unwrap();
    apply(
        &mut state,
        PlayerId(2),
        GameAction::DecideOptionalEffect { accept: false },
    )
    .unwrap();

    // No opponent recorded.
    assert!(
        !state
            .player_actions_this_way
            .contains(&(PlayerId(1), PlayerActionKind::Investigate)),
        "P1 declined and must not be recorded"
    );
    assert!(
        !state
            .player_actions_this_way
            .contains(&(PlayerId(2), PlayerActionKind::Investigate)),
        "P2 declined and must not be recorded"
    );

    // X = 1 + 0 = 1 → exactly one Clue, created by P0's single clause-3
    // investigate.
    assert_eq!(
        count_clues(&state),
        1,
        "with zero investigating opponents, X = 1 + 0 = 1, so exactly one Clue \
         (P0's) should exist; got a different count"
    );
}

/// CR 701.16a: A bare `Effect::Investigate` (no `player_scope`, no `repeat_for`)
/// still creates exactly one Clue token. The new `PlayerPerformedAction`
/// emission is unconditional in the resolver — the ledger entry it leaves is
/// harmless for a standalone investigate.
#[test]
fn bare_investigate_creates_exactly_one_clue() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let ability = ResolvedAbility::new(Effect::Investigate, vec![], ObjectId(9000), PlayerId(0));

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        count_clues(&state),
        1,
        "a bare investigate must create exactly one Clue token"
    );
    // The harmless ledger entry is present but does not affect anything here.
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(0), PlayerActionKind::Investigate)),
        "the unconditional ledger emit records the investigating player"
    );
}
