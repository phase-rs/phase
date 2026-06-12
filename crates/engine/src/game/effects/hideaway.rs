//! Hideaway conceal step (CR 702.75a).
//!
//! The chosen card has just been exiled (face up) by the preceding `Effect::Dig`
//! step of a Hideaway ETB ability (`database/hideaway.rs`); the `DigChoice`
//! resolution bound that card onto this sub-ability's targets. This resolver
//! finishes CR 702.75a's exile clause by:
//!
//! - turning the exiled card **face down** (CR 406.3 — a card exiled face down
//!   can't be examined by any player except where an instruction allows it), and
//! - **linking it to the source** in the persistent `exile_links` pool
//!   (CR 607.2a / CR 406.6) so the card's companion "you may play the exiled
//!   card …" ability (`TargetFilter::ExiledBySource`) can later play it, and so
//!   `visibility.rs` grants the source's controller the CR 702.75a
//!   "may look at this card in the exile zone" permission.
//!
//! This is a continuation step, never announced as a targeted effect: its target
//! is `TargetFilter::ParentTarget`, inherited from the `Dig` continuation.

use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 702.75a: Conceal the just-exiled card — turn it face down and link it to
/// the Hideaway source.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::HideawayConceal { target } = &ability.effect else {
        return Err(EffectError::MissingParam("HideawayConceal".to_string()));
    };

    for obj_id in resolved_object_ids_for_filter(state, ability, target) {
        // CR 406.3: only a card actually in exile (placed there by the preceding
        // Dig) is concealed; guard against a card that left exile via a
        // replacement before this step resolves. A single mutable lookup serves
        // both the zone guard and the face-down flip; the borrow ends before the
        // exile-link push below re-borrows `state`.
        match state.objects.get_mut(&obj_id) {
            Some(obj) if obj.zone == Zone::Exile => obj.face_down = true,
            _ => continue,
        }
        // CR 607.2a + CR 406.6 + CR 702.75a: link the exiled card to the source
        // with the look-permission kind. Like a plain tracked link it stays
        // discoverable by the kind-agnostic `ExiledBySource` companion ability,
        // but additionally grants the source's controller the "may look at this
        // card in the exile zone" permission keyed in `visibility.rs` — without
        // exposing other face-down `TrackedBySource` exiles (Bomat Courier, etc.).
        crate::game::exile_links::push_with_kind(
            state,
            obj_id,
            ability.source_id,
            crate::types::game_state::ExileLinkKind::HideawayLookable,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::HideawayConceal,
        source_id: ability.source_id,
    });
    Ok(())
}
