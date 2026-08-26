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

use super::triggers::{trigger_source_context_for_latch, PendingTrigger, PendingTriggerContext};
use super::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
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
pub(crate) fn finish_encode(
    state: &mut GameState,
    card_id: ObjectId,
    creature_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    // CR 702.99a + CR 614.6: the cipher card exiles itself on resolution. Route
    // through the zone-change pipeline so a board-wide `Moved` "would be exiled →
    // ... instead" redirect is consulted (none target Exile in the current pool
    // today, but a future redirect could send the card elsewhere or surface a
    // CR 616.1 ordering choice). The card moves itself, attributed to its own
    // object id.
    let result = zone_pipeline::move_object(
        state,
        ZoneMoveRequest::effect(card_id, Zone::Exile, card_id),
        events,
    );
    // CR 702.99b: only record the encoded relationship once the card has
    // actually landed in exile. A `NeedsChoice` pause leaves the move
    // unresolved (the caller surfaces the parked prompt), and a `Moved` redirect
    // could send the card to a different zone — in either case the card is not
    // encoded, so the link must NOT be recorded. The redirect-safe zone check is
    // the single guard that covers both cases.
    if matches!(result, ZoneMoveResult::Done)
        && state.objects.get(&card_id).map(|o| o.zone) == Some(Zone::Exile)
    {
        add_encode_link(state, card_id, creature_id);
    }
    result
}

/// CR 702.99a: Begin the on-resolution encode offer for a Cipher spell. Returns
/// `true` when this hook has taken the card off the caller's hands — normally
/// because resolution paused for the choice (the caller stops finalizing the
/// spell and returns, leaving the card held off the stack like a mutating
/// spell), and in the one degenerate case below because the offer already
/// completed as a decline and routed the card itself. Returns `false` when there
/// is no encode to offer at all — the spell isn't an encodable cipher card, or
/// the controller has no creature to host it — so the caller routes the card
/// normally (to its owner's graveyard).
///
/// Both `true` arms leave the caller with nothing to route, which is what makes
/// them one answer: the distinction that matters to a caller is whether the card
/// is still its responsibility.
pub fn begin_encode_choice(
    state: &mut GameState,
    card_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> bool {
    if !spell_can_encode(state, card_id) {
        return false;
    }
    let creatures = legal_encode_creatures(state, controller);
    if creatures.is_empty() {
        return false;
    }
    let pending = crate::types::resolution::PendingCipherEncode {
        stage: crate::types::resolution::CipherEncodeStage::Parked,
        card_id,
        controller,
        creatures,
    };

    // CR 702.99a: the encode is the spell's LAST instruction. When the spell's
    // own effects are still paused on a player answer, this offer must not
    // overwrite that live prompt (issue #7470) — it is parked BELOW the frame
    // that owns the prompt and armed by `resume_resolution_frames` once that
    // owner is consumed. Either way the caller's contract is the same: the
    // resolution owes an answer, so the card is held off the stack.
    park_encode_offer(state, pending, events);
    true
}

/// Park the encode offer, arming it immediately only when nothing else owns the
/// current prompt.
///
/// The offer always leaves this function accounted for: armed as the live
/// prompt, parked as a frame that will arm later, or — if the stack refuses a
/// prompt-less frame at all — completed as a decline. It is never dropped.
fn park_encode_offer(
    state: &mut GameState,
    pending: crate::types::resolution::PendingCipherEncode,
    events: &mut Vec<GameEvent>,
) {
    // The question is not what SHAPE the top frame has — it is whether the
    // resolution is currently asking the player anything at all. Keying this to
    // `FrameGate::DirectChoice` missed the discard pause (Mental Vapors), whose
    // frame owns a prompt without being a direct-choice owner. `waiting_for`
    // is the engine's single answer to "is a question open", so ask it.
    let resolution_paused = !matches!(state.waiting_for, WaitingFor::Priority { .. });
    if !resolution_paused {
        let (player, card_id, creatures) = (
            pending.controller,
            pending.card_id,
            pending.creatures.clone(),
        );
        // The frame and the prompt it may consume are installed as one step:
        // a direct-choice owner that is visible with an unrelated `WaitingFor`
        // is the very state #7470 left behind, and this authority makes the two
        // unable to disagree.
        let armed = crate::types::resolution::ResolutionFrame::CipherEncode(
            crate::types::resolution::PendingCipherEncode {
                stage: crate::types::resolution::CipherEncodeStage::Armed,
                ..pending
            },
        );
        if state
            .install_direct_choice_frame(
                armed,
                WaitingFor::CipherEncodeChoice {
                    player,
                    card_id,
                    creatures,
                },
            )
            .is_err()
        {
            // Same reasoning as the parked branch below: a refusal means the
            // stack was already invalid, and the card still has to leave
            // resolution by a legal route, so the offer completes as a decline
            // (CR 608.2n) rather than being dropped.
            handle_encode_choice(state, card_id, None, events);
        }
        return;
    }
    // Where a prompt-less frame may sit is a property of the stack's shape, not
    // a guess this caller gets to make: an empty stack (a discard prompt owns no
    // frame), the ordinary position below the active child, or outside a paused
    // post-replacement/draw pair whose adjacency `validate` protects. The stack
    // answers that itself, so no legal shape can refuse the offer.
    let card_id = pending.card_id;
    if state
        .park_cipher_encode_beneath_live_prompt(pending)
        .is_err()
    {
        // The stack rejected a frame that owns no prompt, which means it was
        // already invalid before this offer existed. The card must still leave
        // resolution by one of its two legal routes, so complete the offer the
        // way a declined one completes (CR 608.2n: the card goes to its owner's
        // graveyard) instead of dropping it and stranding the card off the
        // stack. The live prompt is untouched either way — a decline moves a
        // card, it does not ask a question.
        handle_encode_choice(state, card_id, None, events);
    }
}

/// CR 702.99a: Arm a parked encode offer once it reaches the stack top, i.e.
/// after the spell's own effects have finished. Called from the exhaustive
/// frame-resume dispatch, which is what guarantees a parked offer is never
/// forgotten.
pub(crate) fn arm_parked_encode_offer(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let Some(pending) = state.resolution_stack.active_cipher_encode() else {
        return;
    };
    // CR 702.99a: re-read legal hosts — the spell's own effects ran since the
    // offer was parked and may have changed the board.
    let creatures = legal_encode_creatures(state, pending.controller);
    let (player, card_id) = (pending.controller, pending.card_id);
    if creatures.is_empty() {
        // No legal host left: consume the frame and route the card the way a
        // declined offer does (CR 608.2n).
        let _ = state.take_active_cipher_encode_frame();
        handle_encode_choice(state, card_id, None, events);
        return;
    }
    if let Some(frame) = state.resolution_stack.active_cipher_encode_mut() {
        frame.stage = crate::types::resolution::CipherEncodeStage::Armed;
        frame.creatures = creatures.clone();
    }
    state.waiting_for = WaitingFor::CipherEncodeChoice {
        player,
        card_id,
        creatures,
    };
}

/// CR 702.99a–b: Resolve the encode choice. `creature = Some(id)` encodes the
/// card on that creature (exile + link); `None` — or a creature that is no
/// longer a legal host — declines, routing the card to its owner's graveyard
/// (CR 608.2n). The chosen creature is re-validated against the current board.
///
/// Returns the [`ZoneMoveResult`] of the move so the caller knows whether a
/// CR 616.1 replacement-ordering choice parked a prompt — either the declined
/// card hit a graveyard→exile redirect, or (future-proof) the accepted card's
/// self-exile hit an Exile-targeting redirect. No such Exile redirect exists in
/// the current pool, so the accept path reports `Done` today.
pub(crate) fn handle_encode_choice(
    state: &mut GameState,
    card_id: ObjectId,
    creature: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    let controller = state.objects.get(&card_id).map(|o| o.controller);
    let chosen = creature
        .filter(|id| controller.is_some_and(|c| legal_encode_creatures(state, c).contains(id)));
    match chosen {
        // CR 702.99a–b: the encode self-exile is normally non-pausing (no
        // Exile-targeting `Moved` redirect exists today), but a future redirect
        // could surface a CR 616.1 ordering choice — propagate `finish_encode`'s
        // result so the caller surfaces the parked prompt instead of returning to
        // priority mid-pause.
        Some(creature_id) => finish_encode(state, card_id, creature_id, events),
        // CR 608.2n + CR 614.6: a declined cipher card is the resolving spell's
        // card being put into its owner's graveyard — route it through the
        // zone-change pipeline so a `Moved` graveyard→exile redirect (Rest in
        // Peace / Leyline of the Void) fires on it. The raw `move_to_zone` never
        // proposed the inner ZoneChange, silently dropping those redirects. The
        // spell's card moves itself on resolution, so the cause is
        // `SpellResolutionDefault` (no external source). A CR 616.1 ordering
        // choice (two simultaneous redirects) is parked centrally by
        // `move_object`; the caller surfaces the parked prompt instead of
        // returning to priority.
        None => zone_pipeline::move_object(
            state,
            ZoneMoveRequest::spell_resolution_default(card_id, Zone::Graveyard),
            events,
        ),
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
            let Some(source) = state.objects.get(creature_id) else {
                continue;
            };
            let controller = source.controller;
            let source_context = trigger_source_context_for_latch(state, source);
            for card_id in encoded_cards_on_creature(state, *creature_id) {
                pending.push(recast_trigger(
                    *creature_id,
                    controller,
                    card_id,
                    event,
                    source_context.clone(),
                ));
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
    source_context: crate::types::game_state::TriggerSourceContext,
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
            count: None,
        },
        vec![TargetRef::Object(card_id)],
        creature_id,
        controller,
    );
    // CR 702.99c: "you may cast" — the controller chooses whether to recast.
    ability.optional = true;
    ability.set_trigger_source_recursive(source_context);

    PendingTriggerContext::single(PendingTrigger {
        source_id: creature_id,
        controller,
        condition: None,
        ability: Box::new(ability),
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
        provenance: None,
    })
}
