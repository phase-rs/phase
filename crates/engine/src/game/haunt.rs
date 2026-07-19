//! Haunt (CR 702.55) — self-contained runtime for the keyword.
//!
//! Haunt is two abilities, mirroring the established Cipher pattern
//! (`game/cipher.rs`) for an exiled card dynamically linked to a battlefield
//! object:
//!
//! 1. **The haunt ability (CR 702.55a).** A triggered ability that exiles the
//!    card *haunting target creature*, synthesized in `database/haunt.rs` as a
//!    `TriggerMode::ChangesZone` "put into a graveyard" trigger whose effect is
//!    [`Effect::ExileHaunting`] (resolved by [`resolve`] — the card, currently
//!    in a graveyard, moves to exile and an [`ExileLinkKind::Haunt`] link
//!    records the haunted creature). The two forms are:
//!    - on a permanent: "When this permanent is put into a graveyard from the
//!      battlefield, exile it haunting target creature";
//!    - on an instant/sorcery: "When this spell is put into a graveyard during
//!      its resolution, exile it haunting target creature".
//!
//! 2. **The haunt-payoff (CR 702.55c).** "Triggered abilities of cards with
//!    haunt that refer to the haunted creature can trigger in the exile zone."
//!    Modeled as `TriggerMode::HauntedCreatureDies` triggers carrying
//!    `trigger_zones = [Exile]`; [`match_haunted_creature_dies`] fires one when
//!    the creature the card haunts dies, looked up through the link.
//!
//! The link's lifetime (CR 702.55b) is handled by `zones.rs`: it is preserved
//! when the haunted creature leaves the battlefield (so the payoff can read it
//! at that moment) and pruned when the haunting card leaves exile.

use super::zone_pipeline::{self, ZoneMoveRequest};
use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState, TriggerSourceContext};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 702.55b: Record that the exiled `card` haunts `creature`.
fn add_haunt_link(state: &mut GameState, card: ObjectId, creature: ObjectId) {
    state.exile_links.push(ExileLink {
        exiled_id: card,
        source_id: creature,
        kind: ExileLinkKind::Haunt,
    });
}

/// CR 702.55b: The creature `card` haunts, if any (the `source_id` of its
/// `Haunt` link). `None` once the card is no longer haunting (link pruned on
/// exile-exit).
pub fn haunted_creature(state: &GameState, card: ObjectId) -> Option<ObjectId> {
    state
        .exile_links
        .iter()
        .find(|link| link.exiled_id == card && link.kind == ExileLinkKind::Haunt)
        .map(|link| link.source_id)
}

/// CR 702.55a: Resolve the haunt ability — exile the source card from the
/// graveyard haunting the target creature. The card was put into a graveyard by
/// dying (permanent) or by resolving (spell); `ability.source_id` still names it
/// (its `ObjectId` is stable across the zone change). The haunted creature is
/// the ability's chosen target.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ExileHaunting { .. } = &ability.effect else {
        return Err(EffectError::MissingParam("ExileHaunting".to_string()));
    };

    // CR 608.2b: if the haunted creature is no longer a legal target the haunt
    // ability is removed from the stack and doesn't resolve; by resolution the
    // engine has already pruned illegal targets, so an empty target set means
    // there is nothing to haunt and the card stays in its graveyard.
    let Some(creature) = ability.targets.iter().find_map(|t| match t {
        crate::types::ability::TargetRef::Object(id) => Some(*id),
        crate::types::ability::TargetRef::Player(_) => None,
    }) else {
        return Ok(());
    };

    let card = ability.source_id;
    // CR 702.55a: only a card actually in a graveyard can be exiled haunting —
    // guard against the card having left the graveyard before this resolves.
    if state
        .objects
        .get(&card)
        .is_none_or(|obj| obj.zone != Zone::Graveyard)
    {
        return Ok(());
    }

    // CR 702.55a + CR 614.6: the haunting card is exiled from its graveyard.
    // Route through the zone-change pipeline so a board-wide `Moved` exile
    // redirect is consulted (none target Exile today — behavior-preserving,
    // future-proof). Attributed to the card itself.
    let result = zone_pipeline::move_object(
        state,
        ZoneMoveRequest::effect(card, Zone::Exile, card),
        events,
    );
    // CR 616.1: a future Exile-targeting `Moved` redirect could surface an
    // ordering choice. Park the prompt (mirrors `exile_from_top_until`'s
    // NeedsChoice arm) and return without recording the link — the card is not
    // yet in exile, so it is not haunting. The resume re-runs SBAs once the
    // choice is made; the haunt link is recorded only when the card lands in
    // exile (the zone check below is the single redirect-safe guard).
    if let zone_pipeline::ZoneMoveResult::NeedsChoice(player) = result {
        state.waiting_for = crate::game::replacement::replacement_choice_waiting_for(player, state);
        return Ok(());
    }
    // CR 702.55b: record the haunt relationship only once the card has actually
    // landed in exile. A `Moved` redirect could have sent it elsewhere, in which
    // case it is not haunting and the link must NOT be recorded.
    if state.objects.get(&card).map(|o| o.zone) == Some(Zone::Exile) {
        add_haunt_link(state, card, creature);
    }
    Ok(())
}

/// CR 702.55c: A `HauntedCreatureDies` payoff trigger on a card in exile fires
/// when the creature that card haunts dies — i.e. the event is that creature
/// being put into a graveyard from the battlefield. The exact source context
/// identifies the haunting card (in exile); the haunted creature is read from
/// its `Haunt` link.
pub fn match_haunted_creature_dies(
    event: &GameEvent,
    _trigger: &crate::types::ability::TriggerDefinition,
    source_context: &TriggerSourceContext,
    state: &GameState,
) -> bool {
    // CR 603.6c + CR 700.4: "dies" means a creature put into a graveyard from
    // the battlefield. Read the dying object's core types from the event's
    // pre-move snapshot (`record`) — its last-known information on the
    // battlefield. `state.objects` now holds the *graveyard* object, which has
    // shed any granted creature type (an animated land/artifact or a
    // creature-land that died as a creature would otherwise fail this check).
    let GameEvent::ZoneChanged {
        object_id,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record,
    } = event
    else {
        return false;
    };
    if !record.core_types.contains(&CoreType::Creature) {
        return false;
    }
    // This is event-link attribution, not a source-relative characteristic
    // read. The source id only selects the Haunt relation.
    haunted_creature(state, source_context.identity.reference.object_id) == Some(*object_id)
}
