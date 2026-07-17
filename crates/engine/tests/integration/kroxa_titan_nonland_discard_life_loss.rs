//! Kroxa, Titan of Death's Hunger — subject-only mandatory-FILTERED
//! decline-tail (issue #6007).
//!
//! Oracle (ETB/attack trigger body):
//!   "Whenever Kroxa enters or attacks, each opponent discards a card, then
//!    each opponent who didn't discard a nonland card this way loses 3
//!    life."
//!
//! Before this fix, the "then each opponent who didn't discard a nonland
//! card this way" clause was parsed as an ordinary, unconditional per-player
//! imperative (no gate at all), so every opponent lost 3 life regardless of
//! what — or whether anything — they discarded.
//!
//! CR anchors:
//!   - CR 608.2c: "each opponent who didn't discard a nonland card this way"
//!     gates the life-loss sub-ability on `Not { ZoneChangedThisWay {
//!     filter: nonland } }` — a property of WHAT moved via `last_zone_changed_ids`
//!     during THIS iteration's discard, not whether the discard happened at
//!     all (distinguishing this from the plain "who can't" mandatory-
//!     impossible class).
//!   - CR 701.9a: To discard, move a card from hand to graveyard — the
//!     Discard sub-resolution stamps `last_zone_changed_ids` with the
//!     discarded object so the filter check reads the correct card.
//!   - Ruling: a player with no cards in hand discards no card this way, so
//!     they haven't discarded a nonland card — the life loss still applies.
//!   - CR 109.5: the body's implicit recipient ("loses 3 life" with no
//!     stated subject) binds to the per-iteration scoped player via the
//!     shared `rebind_clause_recipients_with(_,
//!     rebind_subject_only_body_recipient)` walker (`LoseLife.target`:
//!     `None` → `Some(ScopedPlayer)`).

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

const TRIGGER_BODY: &str = "each opponent discards a card, then each opponent \
     who didn't discard a nonland card this way loses 3 life.";

fn kroxa_trigger(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(TRIGGER_BODY, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

fn add_hand_card(state: &mut GameState, card_id: u64, player: PlayerId, is_land: bool) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        player,
        "Card".to_string(),
        Zone::Hand,
    );
    if is_land {
        let obj = state
            .objects
            .get_mut(&oid)
            .expect("just-created hand object");
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }
    oid
}

fn life(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// An opponent who discards a LAND card "didn't discard a nonland card this
/// way" — the life loss must fire.
#[test]
fn kroxa_opponent_discards_land_loses_three_life() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Kroxa".to_string(),
        Zone::Battlefield,
    );
    add_hand_card(&mut state, 100, PlayerId(1), true);

    let opponent_life_before = life(&state, PlayerId(1));
    let ability = kroxa_trigger(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        life(&state, PlayerId(1)),
        opponent_life_before - 3,
        "opponent discarded a land card (not a nonland card), so the life loss must fire"
    );
}

/// An opponent who discards a NONLAND card DID "discard a nonland card this
/// way" — the life loss must NOT fire.
#[test]
fn kroxa_opponent_discards_nonland_avoids_life_loss() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Kroxa".to_string(),
        Zone::Battlefield,
    );
    add_hand_card(&mut state, 100, PlayerId(1), false);

    let opponent_life_before = life(&state, PlayerId(1));
    let ability = kroxa_trigger(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        life(&state, PlayerId(1)),
        opponent_life_before,
        "opponent discarded a nonland card this way, so the life loss must not fire"
    );
}

/// An opponent with an empty hand discards no card at all — they still
/// haven't discarded a nonland card, so the life loss must fire per the
/// printed ruling.
#[test]
fn kroxa_opponent_with_empty_hand_still_loses_three_life() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Kroxa".to_string(),
        Zone::Battlefield,
    );
    // PlayerId(1) has no cards in hand.

    let opponent_life_before = life(&state, PlayerId(1));
    let ability = kroxa_trigger(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        life(&state, PlayerId(1)),
        opponent_life_before - 3,
        "opponent had no cards to discard, so they didn't discard a nonland card either — life loss must still fire"
    );
}

/// Three players: two opponents in the same per-opponent fan-out, one
/// discarding a land and the other a nonland card. Each opponent's own
/// discard must gate their own life loss independently — a structural
/// regression guard for `detach_after_player_scope_local_chain` keeping the
/// `ZoneChangedThisWay`-gated sub-ability attached to its own iteration
/// instead of a once-after-all-iterations tail (which would read only the
/// last-processed opponent's discard for every opponent).
#[test]
fn kroxa_three_player_each_opponent_gated_by_their_own_discard() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Kroxa".to_string(),
        Zone::Battlefield,
    );
    add_hand_card(&mut state, 100, PlayerId(1), true);
    add_hand_card(&mut state, 200, PlayerId(2), false);

    let p1_life_before = life(&state, PlayerId(1));
    let p2_life_before = life(&state, PlayerId(2));
    let ability = kroxa_trigger(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        life(&state, PlayerId(1)),
        p1_life_before - 3,
        "P1 discarded a land — life loss must fire for P1"
    );
    assert_eq!(
        life(&state, PlayerId(2)),
        p2_life_before,
        "P2 discarded a nonland card — life loss must NOT fire for P2"
    );
}
