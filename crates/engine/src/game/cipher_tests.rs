//! Tests for Cipher (CR 702.99). Declared from `game/mod.rs` so `cipher.rs`
//! stays implementation-only.

use super::cipher::{
    begin_encode_choice, collect_combat_damage_recast_triggers, encoded_cards_on_creature,
    finish_encode, handle_encode_choice, legal_encode_creatures, spell_can_encode,
};
use super::zones::create_object;
use crate::types::ability::{Effect, TargetFilter, TargetRef};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn creature(state: &mut GameState, card: u64, owner: PlayerId, name: &str, zone: Zone) -> ObjectId {
    let id = create_object(state, CardId(card), owner, name.to_string(), zone);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types.core_types.push(CoreType::Creature);
    id
}

/// A Cipher instant on the stack, controlled by `owner`.
fn cipher_spell(state: &mut GameState, card: u64, owner: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        owner,
        "Hidden Strings".to_string(),
        Zone::Stack,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    obj.base_card_types.core_types.push(CoreType::Instant);
    obj.keywords.push(Keyword::Cipher);
    id
}

/// CR 702.99a: only creatures the player controls are legal encode hosts.
#[test]
fn legal_encode_creatures_filters_by_controller() {
    let mut state = GameState::new_two_player(1);
    let mine = creature(&mut state, 1, PlayerId(0), "Mine", Zone::Battlefield);
    let _theirs = creature(&mut state, 2, PlayerId(1), "Theirs", Zone::Battlefield);
    let legal = legal_encode_creatures(&state, PlayerId(0));
    assert_eq!(legal, vec![mine]);
}

/// CR 702.99b: finishing the encode exiles the card and records the link.
#[test]
fn finish_encode_exiles_card_and_records_link() {
    let mut state = GameState::new_two_player(1);
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Spell".to_string(),
        Zone::Stack,
    );

    finish_encode(&mut state, card, host, &mut Vec::new());

    assert_eq!(state.objects[&card].zone, Zone::Exile);
    assert_eq!(encoded_cards_on_creature(&state, host), vec![card]);
}

/// CR 702.99c: the encode link drops when the card leaves exile.
#[test]
fn encode_link_drops_when_card_leaves_exile() {
    let mut state = GameState::new_two_player(1);
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Spell".to_string(),
        Zone::Stack,
    );
    finish_encode(&mut state, card, host, &mut Vec::new());

    super::zones::move_to_zone(&mut state, card, Zone::Graveyard, &mut Vec::new());
    assert!(encoded_cards_on_creature(&state, host).is_empty());
}

/// CR 702.99c: the encode link drops when the creature leaves the battlefield.
#[test]
fn encode_link_drops_when_creature_leaves_battlefield() {
    let mut state = GameState::new_two_player(1);
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Spell".to_string(),
        Zone::Stack,
    );
    finish_encode(&mut state, card, host, &mut Vec::new());

    super::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut Vec::new());
    assert!(encoded_cards_on_creature(&state, host).is_empty());
}

// ── Encode offer (on resolution) ──────────────────────────────────────────

/// CR 702.99a: only an encodable cipher card (non-permanent, not a token) can
/// be encoded.
#[test]
fn spell_can_encode_requires_cipher_nonpermanent_card() {
    let mut state = GameState::new_two_player(1);
    let spell = cipher_spell(&mut state, 1, PlayerId(0));
    assert!(spell_can_encode(&state, spell));

    // A token copy of a cipher spell may not be encoded (no card to exile).
    state.objects.get_mut(&spell).unwrap().is_token = true;
    assert!(!spell_can_encode(&state, spell));
}

/// CR 702.99a: with a legal host, the resolving spell pauses for the encode
/// choice; accepting exiles the card and encodes it on the chosen creature.
#[test]
fn begin_encode_choice_pauses_then_accept_encodes() {
    let mut state = GameState::new_two_player(1);
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let spell = cipher_spell(&mut state, 2, PlayerId(0));

    assert!(begin_encode_choice(&mut state, spell, PlayerId(0)));
    match &state.waiting_for {
        WaitingFor::CipherEncodeChoice {
            player,
            card_id,
            creatures,
        } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(*card_id, spell);
            assert_eq!(creatures, &vec![host]);
        }
        other => panic!("expected CipherEncodeChoice, got {other:?}"),
    }

    handle_encode_choice(&mut state, spell, Some(host), &mut Vec::new());
    assert_eq!(state.objects[&spell].zone, Zone::Exile);
    assert_eq!(encoded_cards_on_creature(&state, host), vec![spell]);
}

/// CR 702.99a: with no creature to host it, there is no encode offer — the
/// caller routes the card to its graveyard.
#[test]
fn begin_encode_choice_skipped_without_host() {
    let mut state = GameState::new_two_player(1);
    let spell = cipher_spell(&mut state, 1, PlayerId(0));
    assert!(!begin_encode_choice(&mut state, spell, PlayerId(0)));
}

/// CR 608.2n: declining the encode puts the card into its owner's graveyard.
#[test]
fn handle_encode_choice_decline_routes_to_graveyard() {
    let mut state = GameState::new_two_player(1);
    let _host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let spell = cipher_spell(&mut state, 2, PlayerId(0));
    assert!(begin_encode_choice(&mut state, spell, PlayerId(0)));

    handle_encode_choice(&mut state, spell, None, &mut Vec::new());
    assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
    assert!(state.exile_links.is_empty());
}

// ── Combat-damage recast ──────────────────────────────────────────────────

/// CR 702.99c: an encoded creature dealing combat damage to a player produces
/// the optional "cast a copy of the encoded card" trigger, targeting the card.
#[test]
fn combat_damage_collects_optional_recast_trigger_for_encoded_card() {
    let mut state = GameState::new_two_player(1);
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Spell".to_string(),
        Zone::Stack,
    );
    finish_encode(&mut state, card, host, &mut Vec::new());

    let event = GameEvent::CombatDamageDealtToPlayer {
        player_id: PlayerId(1),
        source_amounts: vec![(host, 2)],
        total_damage: 2,
    };
    let mut pending = Vec::new();
    collect_combat_damage_recast_triggers(&state, std::slice::from_ref(&event), &mut pending);

    assert_eq!(pending.len(), 1, "one recast trigger for the encoded card");
    let trig = &pending[0].pending;
    assert_eq!(trig.source_id, host);
    assert_eq!(trig.controller, PlayerId(0));
    assert!(
        trig.ability.optional,
        "the recast is optional (\"you may cast\")"
    );
    // The encoded card is the copy source, carried in `ability.targets` (not as
    // a spell target — `target: None` keeps it off the target-slot path so the
    // trigger is not dropped for the exile card being an illegal target).
    assert_eq!(trig.ability.targets, vec![TargetRef::Object(card)]);
    match &trig.ability.effect {
        Effect::CastCopyOfCard { target, cost } => {
            assert_eq!(target, &TargetFilter::None);
            assert!(
                cost.is_without_paying_mana(),
                "cast without paying its mana cost"
            );
        }
        other => panic!("expected CastCopyOfCard, got {other:?}"),
    }
}

/// CR 702.99c: a creature with no encoded card produces no recast trigger.
#[test]
fn combat_damage_no_trigger_without_encode() {
    let mut state = GameState::new_two_player(1);
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let event = GameEvent::CombatDamageDealtToPlayer {
        player_id: PlayerId(1),
        source_amounts: vec![(host, 3)],
        total_damage: 3,
    };
    let mut pending = Vec::new();
    collect_combat_damage_recast_triggers(&state, std::slice::from_ref(&event), &mut pending);
    assert!(pending.is_empty());
}

// ── End-to-end recast (dispatch → resolution → cast copy) ──────────────────

/// CR 702.99c + CR 707.12: end-to-end — an encoded creature dealing combat
/// damage puts the recast trigger on the stack (it must NOT be dropped by the
/// target-slot path: the encoded card sits in exile and is a copy *source*, not
/// a legal spell target), and accepting the optional ability casts a copy of the
/// encoded card while the original stays encoded in exile.
#[test]
fn recast_trigger_resolves_into_a_cast_copy_from_exile() {
    use super::triggers::process_triggers;
    use crate::game::engine;
    use crate::game::stack::resolve_top;
    use crate::types::actions::GameAction;
    use crate::types::game_state::{StackEntryKind, WaitingFor};

    let mut state = GameState::new_two_player(1);
    state.active_player = PlayerId(0);
    state.phase = crate::types::phase::Phase::PostCombatMain;
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let card = create_object(
        &mut state,
        CardId(42),
        PlayerId(0),
        "Spell".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&card)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);
    finish_encode(&mut state, card, host, &mut Vec::new());

    // CR 702.99c: combat damage to a player fires the recast trigger.
    process_triggers(
        &mut state,
        &[GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_amounts: vec![(host, 2)],
            total_damage: 2,
        }],
    );
    assert!(
        state
            .stack
            .iter()
            .any(|e| matches!(&e.kind, StackEntryKind::TriggeredAbility { .. })),
        "recast trigger must reach the stack (regression: dropped by target-slot fizzle)"
    );

    // Resolve the trigger — it's optional, so it pauses for accept/decline.
    resolve_top(&mut state, &mut Vec::new());
    assert!(
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "the recast is a \"you may\" — expected an optional choice, got {:?}",
        state.waiting_for
    );

    // Accept: cast a copy of the encoded card.
    engine::apply(
        &mut state,
        PlayerId(0),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accepting the recast must succeed");

    // A copy of the encoded card is now a spell on the stack...
    let copy = state.stack.iter().find(|e| {
        matches!(&e.kind, StackEntryKind::Spell { card_id, .. } if *card_id == CardId(42))
            && e.id != card
    });
    assert!(
        copy.is_some(),
        "a copy of the encoded card must be cast onto the stack"
    );
    assert_eq!(copy.unwrap().controller, PlayerId(0));

    // ...and the original card stays encoded in exile (CR 702.99c).
    assert_eq!(state.objects[&card].zone, Zone::Exile);
    assert_eq!(encoded_cards_on_creature(&state, host), vec![card]);
}

/// CR 702.99a + CR 707.12a: the COPY cast by Cipher's own recast is NOT
/// represented by a card, so when it resolves it must NOT itself offer to encode.
/// Regression: `spell_can_encode` previously gated on `!is_token`, but
/// `CastCopyOfCard` sets `is_token = false`, so the recast copy (which inherits
/// the source's Cipher keyword) slipped through and was wrongly offered the
/// encode — exiling a copy that then ceases to exist / dangling the link.
#[test]
fn recast_copy_is_not_offered_to_encode() {
    use super::triggers::process_triggers;
    use crate::game::engine;
    use crate::game::stack::resolve_top;
    use crate::types::actions::GameAction;
    use crate::types::game_state::StackEntryKind;

    let mut state = GameState::new_two_player(1);
    state.active_player = PlayerId(0);
    state.phase = crate::types::phase::Phase::PostCombatMain;
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    // The encoded card carries Cipher, so its copy inherits the keyword — the
    // exact condition that made the copy wrongly eligible to re-encode.
    let card = cipher_spell(&mut state, 42, PlayerId(0));
    finish_encode(&mut state, card, host, &mut Vec::new());

    // Encoded creature deals combat damage → recast trigger → accept → copy cast.
    process_triggers(
        &mut state,
        &[GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_amounts: vec![(host, 2)],
            total_damage: 2,
        }],
    );
    resolve_top(&mut state, &mut Vec::new()); // optional recast trigger pauses
    engine::apply(
        &mut state,
        PlayerId(0),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accepting the recast must succeed");

    // Resolve the copy that is now on the stack.
    let copy_id = state
        .stack
        .iter()
        .find(|e| matches!(&e.kind, StackEntryKind::Spell { .. }) && e.id != card)
        .map(|e| e.id)
        .expect("a copy of the encoded card is on the stack");
    resolve_top(&mut state, &mut Vec::new());

    // CR 702.99a: the resolving copy must NOT pause to encode itself.
    assert!(
        !matches!(state.waiting_for, WaitingFor::CipherEncodeChoice { .. }),
        "a copy is not represented by a card and must not offer to encode; got {:?}",
        state.waiting_for
    );
    // Only the original card remains encoded — the copy did not create a link.
    assert_eq!(
        encoded_cards_on_creature(&state, host),
        vec![card],
        "the recast copy must not become encoded on the creature"
    );
    assert!(
        state
            .objects
            .get(&copy_id)
            .is_none_or(|o| o.zone != Zone::Exile),
        "the recast copy must not be exiled (encoded)"
    );
}

/// CR 702.99a: end-to-end — a resolving Cipher spell pauses for the encode
/// choice via `resolve_top`, and dispatching `CipherEncode` exiles+encodes it.
#[test]
fn cipher_spell_resolution_pauses_and_encodes_via_dispatch() {
    use crate::game::engine;
    use crate::game::stack::resolve_top;
    use crate::types::actions::GameAction;
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};

    let mut state = GameState::new_two_player(1);
    state.active_player = PlayerId(0);
    state.phase = crate::types::phase::Phase::PreCombatMain;
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    let spell = cipher_spell(&mut state, 2, PlayerId(0));

    state.stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: state.objects[&spell].card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    resolve_top(&mut state, &mut Vec::new());
    match &state.waiting_for {
        WaitingFor::CipherEncodeChoice {
            card_id, creatures, ..
        } => {
            assert_eq!(*card_id, spell);
            assert_eq!(creatures, &vec![host]);
        }
        other => panic!("expected CipherEncodeChoice on resolution, got {other:?}"),
    }

    engine::apply(
        &mut state,
        PlayerId(0),
        GameAction::CipherEncode {
            creature: Some(host),
        },
    )
    .expect("encode dispatch must succeed");
    assert_eq!(state.objects[&spell].zone, Zone::Exile);
    assert_eq!(encoded_cards_on_creature(&state, host), vec![spell]);
}

/// CR 603.2 + CR 702.99c: the recast trigger fires even when the encoded
/// creature deals lethal combat damage and dies simultaneously. Triggers are
/// collected at the moment of the event (creature still alive, link + controller
/// intact) BEFORE state-based actions destroy it — so the trigger reaches the
/// stack, and the encoded card stays in exile to be copied even after the link
/// is pruned by the creature leaving the battlefield.
#[test]
fn recast_fires_when_encoded_creature_dies_in_combat() {
    use super::sba::check_state_based_actions;
    use super::triggers::process_triggers;
    use crate::types::game_state::StackEntryKind;

    let mut state = GameState::new_two_player(1);
    state.active_player = PlayerId(0);
    state.phase = crate::types::phase::Phase::PostCombatMain;
    let host = creature(&mut state, 1, PlayerId(0), "Host", Zone::Battlefield);
    {
        // 1/1 with lethal damage already marked — SBA will destroy it.
        let obj = state.objects.get_mut(&host).unwrap();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.damage_marked = 1;
    }
    let card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Spell".to_string(),
        Zone::Stack,
    );
    finish_encode(&mut state, card, host, &mut Vec::new());

    // CR 603.2: triggers are collected at the moment of the event — the creature
    // is still alive here, so the recast is collected onto the stack.
    process_triggers(
        &mut state,
        &[GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_amounts: vec![(host, 1)],
            total_damage: 1,
        }],
    );
    assert!(
        state
            .stack
            .iter()
            .any(|e| matches!(&e.kind, StackEntryKind::TriggeredAbility { .. })),
        "recast trigger must be collected while the creature is still alive"
    );

    // Now SBAs destroy the creature and prune the encode link...
    check_state_based_actions(&mut state, &mut Vec::new());
    assert_ne!(
        state.objects[&host].zone,
        Zone::Battlefield,
        "creature died"
    );
    assert!(
        encoded_cards_on_creature(&state, host).is_empty(),
        "link pruned"
    );

    // ...but the already-collected trigger survives, and the encoded card is
    // still in exile so the copy can be cast on resolution.
    assert!(
        state
            .stack
            .iter()
            .any(|e| matches!(&e.kind, StackEntryKind::TriggeredAbility { .. })),
        "the collected recast trigger is not removed by the creature's death"
    );
    assert_eq!(state.objects[&card].zone, Zone::Exile);
}
