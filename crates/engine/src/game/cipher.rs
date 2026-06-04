//! Cipher (CR 702.99) — self-contained runtime for the keyword.
//!
//! Cipher is two abilities on an instant/sorcery:
//!
//! 1. **Spell ability (on resolution).** "If this spell is represented by a
//!    card, you may exile this card encoded on a creature you control"
//!    (CR 702.99a). Handled by [`offer_encode`] / [`finish_encode`]: the
//!    resolving spell pauses (mirroring Mutate's resolution pause), the
//!    controller picks one of their creatures (or declines), and on accept the
//!    card is exiled and an [`ExileLinkKind::Cipher`] link records the
//!    *encoded* relationship (CR 702.99b).
//!
//! 2. **Static ability (while the card is encoded).** "For as long as this card
//!    is encoded on that creature, that creature has 'Whenever this creature
//!    deals combat damage to a player, you may copy the encoded card and you
//!    may cast the copy without paying its mana cost'" (CR 702.99c). Handled by
//!    [`combat_damage_recast_triggers`]: when an encoded creature deals combat
//!    damage to a player, an optional [`Effect::CastCopyOfCard`] triggered
//!    ability is put on the stack, targeting the encoded card in exile.
//!
//! The encode relationship lives in `state.exile_links` and is pruned for free
//! by the existing `zones.rs` cleanup: the card leaving exile, or the creature
//! leaving the battlefield, drops the link — exactly CR 702.99c's lifetime. A
//! later cipher spell can re-encode onto the same creature (CR 702.99); each
//! encode is an independent link.

use super::triggers::{PendingTrigger, PendingTriggerContext};
use crate::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 702.99b: Record the *encoded* relationship between an exiled card and the
/// creature it is encoded on. `card_id` must already be in the exile zone.
fn add_encode_link(state: &mut GameState, card_id: ObjectId, creature_id: ObjectId) {
    state.exile_links.push(ExileLink {
        exiled_id: card_id,
        source_id: creature_id,
        kind: ExileLinkKind::Cipher,
    });
}

/// CR 702.99c: The cards currently encoded on `creature_id` (one per cipher
/// spell encoded there). Reads the canonical `exile_links` state.
pub fn encoded_cards_on_creature(state: &GameState, creature_id: ObjectId) -> Vec<ObjectId> {
    state
        .exile_links
        .iter()
        .filter(|link| link.source_id == creature_id && link.kind == ExileLinkKind::Cipher)
        .map(|link| link.exiled_id)
        .collect()
}

/// CR 702.99a: Whether `card_id` is a resolving spell that may be encoded — it
/// must carry Cipher, be represented by a card (not a token and not a copy,
/// CR 707.12a), and be a non-permanent spell (cipher only appears on instants
/// and sorceries).
pub fn spell_can_encode(state: &GameState, card_id: ObjectId) -> bool {
    state.objects.get(&card_id).is_some_and(|obj| {
        // CR 702.99a: "If this spell is represented by a card …". A token or a
        // copy (e.g. the copy cast by Cipher's own recast via `CastCopyOfCard`,
        // CR 707.12a) is NOT represented by a card and can never be encoded.
        obj.is_represented_by_a_card()
            && super::keywords::has_keyword(obj, &Keyword::Cipher)
            && obj
                .card_types
                .core_types
                .iter()
                .all(|t| !t.is_permanent_type())
    })
}

/// CR 702.99a: The creatures `player` controls that the card could be encoded
/// on ("a creature you control"). Empty means the encode offer is skipped.
pub fn legal_encode_creatures(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == player && obj.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .collect()
}

/// CR 702.99a–b: Complete the encode — exile the resolving card and link it to
/// the chosen creature. Caller has already validated `creature_id` is a legal
/// "creature you control". The card moves graveyard-free from the stack to
/// exile (the cipher static functions while the card is in exile, CR 702.99a).
pub fn finish_encode(
    state: &mut GameState,
    card_id: ObjectId,
    creature_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    super::zones::move_to_zone(state, card_id, Zone::Exile, events);
    add_encode_link(state, card_id, creature_id);
}

/// CR 702.99a: Begin the on-resolution encode offer for a Cipher spell. Returns
/// `true` when resolution paused for the choice (the caller must stop finalizing
/// the spell and return, leaving the card held off the stack like a mutating
/// spell), or `false` when there is no encode to offer — the spell isn't an
/// encodable cipher card, or the controller has no creature to host it — so the
/// caller routes the card normally (to its owner's graveyard).
pub fn begin_encode_choice(state: &mut GameState, card_id: ObjectId, controller: PlayerId) -> bool {
    if !spell_can_encode(state, card_id) {
        return false;
    }
    let creatures = legal_encode_creatures(state, controller);
    if creatures.is_empty() {
        return false;
    }
    state.waiting_for = WaitingFor::CipherEncodeChoice {
        player: controller,
        card_id,
        creatures,
    };
    true
}

/// CR 702.99a–b: Resolve the encode choice. `creature = Some(id)` encodes the
/// card on that creature (exile + link); `None` — or a creature that is no
/// longer a legal host — declines, routing the card to its owner's graveyard
/// (CR 608.2n). The chosen creature is re-validated against the current board.
pub fn handle_encode_choice(
    state: &mut GameState,
    card_id: ObjectId,
    creature: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) {
    let controller = state.objects.get(&card_id).map(|o| o.controller);
    let chosen = creature
        .filter(|id| controller.is_some_and(|c| legal_encode_creatures(state, c).contains(id)));
    match chosen {
        Some(creature_id) => finish_encode(state, card_id, creature_id, events),
        // CR 608.2n: a declined cipher card is put into its owner's graveyard.
        None => super::zones::move_to_zone(state, card_id, Zone::Graveyard, events),
    }
}

/// CR 702.99c: "Whenever this creature deals combat damage to a player, its
/// controller may cast a copy of the encoded card without paying its mana
/// cost." This is a state-derived trigger (the granting ability lives on the
/// encoded card in exile, not in the creature's printed trigger set), so it is
/// collected here and appended to the pending set — mirroring how `The Ring`'s
/// "Ring-bearer deals combat damage" emblem trigger is injected during
/// `collect_pending_triggers`.
///
/// One trigger per encoded card per combat-damage-to-a-player event. Double
/// strike yields one event per damage step (`source_amounts` is step-local), so
/// a double-striking encoded creature correctly triggers in each step.
pub fn collect_combat_damage_recast_triggers(
    state: &GameState,
    events: &[GameEvent],
    pending: &mut Vec<PendingTriggerContext>,
) {
    for event in events {
        let GameEvent::CombatDamageDealtToPlayer { source_amounts, .. } = event else {
            continue;
        };
        for (creature_id, amount) in source_amounts {
            if *amount == 0 {
                continue;
            }
            // CR 702.99c: "its controller" — the creature's current controller,
            // which may differ from the player who cast the cipher spell.
            let Some(controller) = state.objects.get(creature_id).map(|o| o.controller) else {
                continue;
            };
            for card_id in encoded_cards_on_creature(state, *creature_id) {
                pending.push(recast_trigger(*creature_id, controller, card_id, event));
            }
        }
    }
}

/// CR 702.99c + CR 707.12: Build the optional "cast a copy of the encoded card
/// without paying its mana cost" triggered ability. The encoded card is the
/// copy source carried in `ability.targets`; `CastCopyOfCard` copies it in its
/// exile zone and casts the copy for `ManaCost::zero()`, re-prompting for the
/// copy's own targets.
fn recast_trigger(
    creature_id: ObjectId,
    controller: PlayerId,
    card_id: ObjectId,
    event: &GameEvent,
) -> PendingTriggerContext {
    let mut ability = ResolvedAbility::new(
        // CR 702.99c: the encoded card is a *copy source*, not a spell target —
        // cipher's recast is not "target". `TargetFilter::None` keeps the
        // copy-and-cast effect off the target-slot path (the card sits in exile
        // and is not a legal target there, which would otherwise drop the whole
        // trigger), while the card rides in `ability.targets` for the
        // `CastCopyOfCard` resolver to pick up as its copy source.
        Effect::CastCopyOfCard {
            target: TargetFilter::None,
            cost: ManaCost::zero(),
        },
        vec![TargetRef::Object(card_id)],
        creature_id,
        controller,
    );
    // CR 702.99c: "you may cast" — the controller chooses whether to recast.
    ability.optional = true;

    PendingTriggerContext {
        pending: PendingTrigger {
            source_id: creature_id,
            controller,
            condition: None,
            ability,
            timestamp: 0,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: Some(event.clone()),
            modal: None,
            mode_abilities: Vec::new(),
            description: Some("Cipher — cast a copy of the encoded card".to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        },
        trigger_events: vec![event.clone()],
    }
}
