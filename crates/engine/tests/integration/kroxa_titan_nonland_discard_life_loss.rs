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

/// Regression guard for the guard-scoping fix at `strip_each_player_subject`'s
/// "who didn't"/"who did not" reservation: that reservation must only claim
/// the "discard" verb the Kroxa dispatcher understands, not every verb.
///
/// Before the fix, reserving bare "who didn't"/"who did not" (any verb) left
/// non-discard "who didn't <verb> ... this way, <body>" clauses — e.g. Kynaios
/// and Tiro of Meletis: "each other player who didn't put a card onto the
/// battlefield this way draws a card" — with no dispatcher able to claim
/// them. They fell through to the ordinary imperative parser, which matched
/// the leftover "put a card onto the battlefield" fragment as a `ChangeZone`
/// effect and silently dropped the actual "draws a card" action entirely.
fn add_library_card(state: &mut GameState, card_id: u64, player: PlayerId) -> ObjectId {
    create_object(
        state,
        CardId(card_id),
        player,
        "Card".to_string(),
        Zone::Library,
    )
}

fn hand_size(state: &GameState, player: PlayerId) -> usize {
    state
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Hand && obj.owner == player)
        .count()
}

/// Strongarm Tactics — the sibling all-players (`PlayerFilter::All`) scope
/// of the same mandatory-FILTERED decline-tail construction, with a
/// "creature" filter instead of Kroxa's "nonland" filter:
///   "Each player discards a card. Then each player who didn't discard a
///    creature card this way loses 4 life."
///
/// Unlike Kroxa's opponent-only fan-out, "each player" includes the
/// ability's own controller — this pins that the controller-inclusive scope
/// and its per-player `ZoneChangedThisWay` gate both resolve correctly.
const STRONGARM_TACTICS_BODY: &str = "each player discards a card, then each \
     player who didn't discard a creature card this way loses 4 life.";

fn strongarm_tactics_effect(controller: PlayerId, source_id: ObjectId) -> ResolvedAbility {
    let def = parse_effect_chain(STRONGARM_TACTICS_BODY, AbilityKind::Spell);
    build_resolved_from_def(&def, source_id, controller)
}

fn add_creature_hand_card(state: &mut GameState, card_id: u64, player: PlayerId) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        player,
        "Card".to_string(),
        Zone::Hand,
    );
    let obj = state
        .objects
        .get_mut(&oid)
        .expect("just-created hand object");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    oid
}

/// Three players, all in the "each player" scope (including the caster):
/// one discards a creature card (avoids life loss), one discards a
/// noncreature card (loses life), one has an empty hand (still loses life
/// per the same "didn't discard" ruling Kroxa relies on).
#[test]
fn strongarm_tactics_all_players_gated_by_own_discard() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Strongarm Tactics".to_string(),
        Zone::Battlefield,
    );
    // PlayerId(0) is the caster and is still in the "each player" scope.
    add_creature_hand_card(&mut state, 100, PlayerId(0));
    add_hand_card(&mut state, 200, PlayerId(1), false);
    // PlayerId(2) has no cards in hand.

    let p0_life_before = life(&state, PlayerId(0));
    let p1_life_before = life(&state, PlayerId(1));
    let p2_life_before = life(&state, PlayerId(2));
    let ability = strongarm_tactics_effect(PlayerId(0), source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        life(&state, PlayerId(0)),
        p0_life_before,
        "caster discarded a creature card, so the life loss must not fire for the caster"
    );
    assert_eq!(
        life(&state, PlayerId(1)),
        p1_life_before - 4,
        "P1 discarded a noncreature card, so the life loss must fire for P1"
    );
    assert_eq!(
        life(&state, PlayerId(2)),
        p2_life_before - 4,
        "P2 had no cards to discard, so they didn't discard a creature card either — life loss must still fire"
    );
}

#[test]
fn non_discard_who_didnt_clause_still_draws_a_card() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Kynaios and Tiro of Meletis".to_string(),
        Zone::Battlefield,
    );
    add_library_card(&mut state, 100, PlayerId(1));

    let text = "each other player who didn't put a card onto the battlefield \
        this way draws a card.";
    let def = parse_effect_chain(text, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, PlayerId(0));

    let hand_before = hand_size(&state, PlayerId(1));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(
        hand_size(&state, PlayerId(1)),
        hand_before + 1,
        "the non-discard decline-tail clause must still resolve as a Draw, \
         not be misparsed into a dropped ChangeZone effect"
    );
}
