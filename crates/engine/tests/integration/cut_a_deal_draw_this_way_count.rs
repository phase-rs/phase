//! Integration tests for Cut a Deal's second-draw count (CR 121.1 + CR 608.2c +
//! CR 109.5).
//!
//! Oracle text (verbatim): "Each opponent draws a card, then you draw a card for
//! each opponent who drew a card this way."
//!
//! The misparse this fixes: the second draw's "for each opponent who drew a card
//! this way" count parsed to `QuantityRef::TrackedSetSize`, but the preceding
//! opponent-scoped Draw publishes no tracked object set, so it resolved to 0 (or
//! a stale set) instead of counting the opponents who drew. The corrected parse
//! is `PlayerCount { PerformedActionThisWay { Opponent, Draw } }`, resolved from
//! the `player_actions_this_way` ledger that each settled draw now populates.
//!
//! This mirrors `tempt_with_discovery.rs` / `wernog_riders_chaplain_investigate_count.rs`
//! — the identical "each opponent does X, then you do X once per opponent who did
//! it this way" machinery — but for Draw instead of Search/Investigate, and with
//! a MANDATORY first clause (no `may`), so no `OptionalEffectChoice` prompts: the
//! opponents draw automatically and the whole chain resolves in one call.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Effect, PlayerFilter, PlayerRelation, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
};
use engine::types::events::PlayerActionKind;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CUT_A_DEAL_ORACLE: &str =
    "Each opponent draws a card, then you draw a card for each opponent who drew a card this way.";

/// Parse Cut a Deal and build its resolved Spell ability (controller = P0).
fn make_game_and_ability(num_players: u8) -> (GameState, ResolvedAbility) {
    let parsed = parse_oracle_text(
        CUT_A_DEAL_ORACLE,
        "Cut a Deal",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = build_resolved_from_def(&parsed.abilities[0], ObjectId(9000), PlayerId(0));
    let state = GameState::new(FormatConfig::standard(), num_players, 42);
    (state, ability)
}

fn seed_library(state: &mut GameState, owner: PlayerId, count: u64, base_id: u64) {
    for i in 0..count {
        create_object(
            state,
            CardId(base_id + i),
            owner,
            format!("Card {owner:?}-{i}"),
            Zone::Library,
        );
    }
}

fn hand_size(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// CR 121.1 + CR 608.2c + CR 109.5: The parsed AST must fix ONLY the second
/// draw's count. The first clause ("each opponent draws a card") stays a
/// `Draw { Fixed(1), Controller }` fanned out over `player_scope: Opponent`; the
/// second draw's count becomes `PlayerCount { PerformedActionThisWay { Opponent,
/// Draw } }` instead of the object-count `TrackedSetSize` misparse.
///
/// Revert-failing: reverting the `parse_drew_arm` addition makes the second
/// count fall back to `TrackedSetSize`, flipping the final `assert_eq!`.
#[test]
fn cut_a_deal_parses_second_draw_as_player_count_over_droppers() {
    let parsed = parse_oracle_text(
        CUT_A_DEAL_ORACLE,
        "Cut a Deal",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        !parsed.abilities.is_empty(),
        "Cut a Deal must produce a Spell ability, got {:?}",
        parsed.abilities
    );
    let def = &parsed.abilities[0];

    // First clause is unchanged: each opponent draws one card.
    assert!(
        matches!(
            &*def.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }
        ),
        "outer clause must stay 'each opponent draws a card', got {:?}",
        def.effect
    );
    assert_eq!(
        def.player_scope,
        Some(PlayerFilter::Opponent),
        "the first clause fans out over opponents"
    );

    // Second clause: you draw one card per opponent who drew this way.
    let sub = def
        .sub_ability
        .as_ref()
        .expect("Cut a Deal has a second-draw sub_ability");
    let Effect::Draw { count, target } = &*sub.effect else {
        panic!("second clause must be a Draw, got {:?}", sub.effect);
    };
    assert_eq!(*target, TargetFilter::Controller, "you draw");
    assert_eq!(
        *count,
        QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::PerformedActionThisWay {
                    relation: PlayerRelation::Opponent,
                    action: PlayerActionKind::Draw,
                },
            },
        },
        "the second draw must count opponents who drew a card this way \
         (the misparse produced TrackedSetSize)"
    );
}

/// CR 121.1 + CR 608.2c + CR 109.5: Happy path — 3 players (P0 controller, P1 +
/// P2 opponents). Both opponents draw one card each (mandatory first clause),
/// each recording itself in `player_actions_this_way`; then the controller's
/// detached draw resolves `PlayerCount { PerformedActionThisWay { Opponent,
/// Draw } }` = 2 and draws exactly two cards.
///
/// Revert-failing on BOTH halves of the fix: reverting the parser change leaves
/// the count as `TrackedSetSize` (0/stale → P0 draws 0); reverting the draw
/// emission leaves the ledger empty (`PlayerCount` = 0 → P0 draws 0). Either way
/// the `hand_size(P0) == 2` assertion fails.
#[test]
fn cut_a_deal_controller_draws_one_per_opponent_who_drew() {
    let (mut state, ability) = make_game_and_ability(3);
    seed_library(&mut state, PlayerId(0), 5, 100);
    seed_library(&mut state, PlayerId(1), 2, 200);
    seed_library(&mut state, PlayerId(2), 2, 300);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    // Both opponents drew this way and are recorded against themselves (the
    // scoped opponent, not the controller).
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(1), PlayerActionKind::Draw)),
        "P1 drew a card this way and must be recorded, got {:?}",
        state.player_actions_this_way
    );
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(2), PlayerActionKind::Draw)),
        "P2 drew a card this way and must be recorded, got {:?}",
        state.player_actions_this_way
    );

    assert_eq!(
        hand_size(&state, PlayerId(1)),
        1,
        "each opponent draws exactly one card"
    );
    assert_eq!(hand_size(&state, PlayerId(2)), 1);
    assert_eq!(
        hand_size(&state, PlayerId(0)),
        2,
        "controller draws one card per opponent who drew this way (2); a wrong \
         count means the second draw resolved TrackedSetSize (0/stale) or the \
         draw emission never populated the ledger"
    );
}

/// CR 121.1 + CR 109.5: Boundary — 2 players (one opponent). The single opponent
/// draws one card; the controller's detached draw counts exactly one opponent
/// who drew this way and draws one card. The controller's OWN detached draw also
/// enters the ledger, but the `Opponent` relation excludes it from its own count,
/// so P0 draws 1 and not 2.
#[test]
fn cut_a_deal_two_players_controller_draws_one() {
    let (mut state, ability) = make_game_and_ability(2);
    seed_library(&mut state, PlayerId(0), 5, 100);
    seed_library(&mut state, PlayerId(1), 2, 200);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(1), PlayerActionKind::Draw)),
        "the single opponent drew this way and must be recorded"
    );
    assert_eq!(hand_size(&state, PlayerId(1)), 1);
    assert_eq!(
        hand_size(&state, PlayerId(0)),
        1,
        "controller counts only the one opponent who drew — its own detached \
         draw is excluded by the Opponent relation, so P0 draws 1, not 2"
    );
}

/// CR 121.1 + CR 608.2c: Ruling #1 — an opponent who doesn't draw is not counted.
/// 3 players; P2's library is empty, so P2's mandatory draw delivers no card and
/// records nothing this way (the `drawn_count > 0` emission gate). P1 draws one
/// card and is recorded; the controller draws exactly one (only P1 counted).
#[test]
fn cut_a_deal_opponent_who_cannot_draw_is_not_counted() {
    let (mut state, ability) = make_game_and_ability(3);
    seed_library(&mut state, PlayerId(0), 5, 100);
    seed_library(&mut state, PlayerId(1), 2, 200);
    // P2 has an EMPTY library — its draw delivers no card.

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(1), PlayerActionKind::Draw)),
        "P1 drew a card this way and must be recorded"
    );
    assert!(
        !state
            .player_actions_this_way
            .contains(&(PlayerId(2), PlayerActionKind::Draw)),
        "P2 drew no card (empty library) and must NOT be recorded, got {:?}",
        state.player_actions_this_way
    );
    assert_eq!(
        hand_size(&state, PlayerId(2)),
        0,
        "P2's empty-library draw delivers no card"
    );
    assert_eq!(
        hand_size(&state, PlayerId(0)),
        1,
        "only the one opponent who actually drew (P1) counts toward the \
         controller's draw (CR 608.2c ruling #1)"
    );
}

/// CR 121.1: A bare `Effect::Draw` (no `player_scope`, no count reference) still
/// draws the right number and leaves a harmless `(controller, Draw)` ledger
/// entry — the unconditional emission does not perturb ordinary draws.
#[test]
fn bare_draw_leaves_harmless_this_way_ledger_entry() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    seed_library(&mut state, PlayerId(0), 3, 100);

    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        ObjectId(9000),
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        hand_size(&state, PlayerId(0)),
        1,
        "a bare draw must draw exactly one card"
    );
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(0), PlayerActionKind::Draw)),
        "the unconditional ledger emit records the drawing player"
    );
}

/// CR 121.1: A single multi-card draw instruction records ONE draw event in both
/// ledgers — the `player_actions_this_turn` Vec (which preserves repeated actions
/// for count-style consumers) gets exactly one `(P0, Draw)` entry for a two-card
/// draw, not one per card. This is the production-path guard for the latent
/// over-count: a future `QuantityRef::PlayerActionsThisTurn { action: Draw }`
/// consumer must measure draw EVENTS, not cards drawn.
///
/// Revert-failing: emitting `PlayerPerformedAction { Draw }` per card (the old
/// per-unit shape) makes the Vec hold two `(P0, Draw)` entries and fails the
/// count assertion.
#[test]
fn multi_card_draw_records_one_turn_ledger_entry() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    seed_library(&mut state, PlayerId(0), 5, 100);

    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
        vec![],
        ObjectId(9000),
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        hand_size(&state, PlayerId(0)),
        2,
        "the two-card draw instruction delivers two cards"
    );
    let turn_draws = state
        .player_actions_this_turn
        .iter()
        .filter(|(player, action)| *player == PlayerId(0) && *action == PlayerActionKind::Draw)
        .count();
    assert_eq!(
        turn_draws, 1,
        "a two-card draw is ONE draw event: the turn-scoped Vec must hold exactly one \
         (P0, Draw), not one per card (count-style turn ledger measures draw events)"
    );
    // The set counterpart also records the drawer exactly once.
    assert!(
        state
            .player_actions_this_way
            .contains(&(PlayerId(0), PlayerActionKind::Draw)),
        "the set ledger records the drawing player once"
    );
}
