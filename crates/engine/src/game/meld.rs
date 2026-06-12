//! Meld (CR 701.42 / CR 712.4) — runtime resolver for the meld keyword action.
//!
//! A meld instigator's ability (triggered or activated) resolves to
//! [`Effect::Meld`], dispatched here via [`perform_meld`]. When the controller
//! both OWNS and CONTROLS the instigator (`source_id`) AND a battlefield object
//! named `partner` (CR 701.42b), both cards are exiled and a single melded
//! permanent is put onto the battlefield presenting the `result` card's
//! characteristics (the combined back faces, exposed in card-data as the named
//! result card). If either half is missing/illegal the objects stay in place
//! (CR 701.42c, a silent no-op).
//!
//! ## LAYER-ONLY characteristics (never mutate the survivor's base)
//!
//! Unlike a naive "apply the result face onto the survivor", this resolver is
//! LAYER-ONLY: it NEVER calls `apply_card_face_to_object` on the survivor (that
//! would overwrite the survivor's `base_*`, permanently replacing Gisela's
//! printed identity with Brisela). Instead it builds a [`CopiableValues`] from
//! the result `CardFace` via [`meld_copiable_values`] and installs it as a
//! layer-1 `CopyValues` continuous effect through
//! [`merge::install_merge_layer_effect`] — exactly mirroring `merge_object_onto`'s
//! never-touch-base discipline. On leave, `split_merged_permanent_on_leave`
//! returns each card as its own FRONT face (Gisela / Bruna), satisfying
//! CR 712.4b / CR 712.21.
//!
//! ## Meld DOES enter the battlefield (unlike Mutate)
//!
//! `merge_object_onto` (Mutate) SUPPRESSES ETB per CR 730.2b; meld does NOT
//! (CR 701.42a / CR 712.4a — "put them onto the battlefield"). The survivor's
//! exile→battlefield entry is therefore driven through the NORMAL
//! `zones::move_to_zone` path so the `ZoneChanged { to: Battlefield }` event
//! fires and ETB triggers match (CR 603.6a). This is why `merge_object_onto`
//! is NOT reused wholesale — only its `merged_components` bookkeeping, its
//! layer-install pattern, and `split_merged_permanent_on_leave` are shared.

use crate::game::game_object::MergeKind;
use crate::game::merge;
use crate::game::printed_cards::{meld_copiable_values, printed_ref_from_face};
use crate::game::zone_pipeline::{self, ZoneMoveRequest};
use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 701.42 / CR 712.4: Resolve a meld instigator ability. Exiles both halves
/// (CR 701.42a) and puts a single melded permanent onto the battlefield
/// presenting the `result` card's characteristics, or no-ops if the meld is
/// illegal (CR 701.42c).
pub fn perform_meld(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::Meld {
        source: expected_source,
        partner,
        result,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("Meld".to_string()));
    };

    let source = ability.source_id;
    let controller = ability.controller;

    // CR 701.42b: the controller must both OWN and CONTROL the instigator, and
    // the instigator must be the real meld card — only a card (CR 111.1: not a
    // token; CR 707.10: not a copy) printed with the meld instruction can be
    // melded. A token copy or a renamed non-meld impostor carrying the same name
    // is NOT a valid meld half. The instigator must be on the battlefield (it was
    // put there to activate/trigger this ability).
    let source_ok = state.objects.get(&source).is_some_and(|o| {
        o.zone == Zone::Battlefield
            && o.controller == controller
            && o.owner == controller
            && o.is_represented_by_a_card()
            && o.base_name.eq_ignore_ascii_case(expected_source.as_str())
    });
    if !source_ok {
        // CR 701.42c: objects that can't be melded stay in their current zone.
        return Ok(());
    }

    // CR 701.42b: find a battlefield object that is the real `partner` meld card,
    // co-owned and co-controlled. We match `base_name` (the object's PRINTED
    // identity), NOT the layer-modified current `name`: `FilterProp::Named`
    // matches the post-layer `name` (filter.rs:2812), which (i) would let a
    // renamed non-meld impostor whose current name equals the partner pass, and
    // (ii) would wrongly REJECT a real partner whose name was changed by an
    // effect. `base_name` is rename-proof — `ContinuousModification::SetName`
    // overwrites `name` but never `base_name`, and layers reset `name =
    // base_name` each pass — so a `base_name` match proves the object IS the real
    // pair card (closing the token/copy/rename impostor classes and the
    // real-partner-renamed inverse). It must also be card-backed (CR 111.1 /
    // CR 707.10) for the same reason as the source.
    let Some(partner_id) = state.battlefield.iter().copied().find(|&id| {
        id != source
            && state.objects.get(&id).is_some_and(|o| {
                o.controller == controller
                    && o.owner == controller
                    && o.is_represented_by_a_card()
                    && o.base_name.eq_ignore_ascii_case(partner.as_str())
            })
    }) else {
        // CR 701.42c: no real co-owned/controlled partner → no-op.
        return Ok(());
    };

    // CR 712.4b: resolve the result `CardFace` from the registry (seeded at init
    // via `walk_effect` → `build_conjure_registry`; conjure lookup pattern). If
    // the result card is unknown, the meld cannot produce its permanent — no-op.
    let Some(result_face) = state
        .card_face_registry
        .get(&result.to_lowercase())
        .cloned()
    else {
        return Ok(());
    };

    // CR 701.42a / CR 614.6: exile BOTH halves. Route through the zone-change
    // pipeline so any exile `Moved` redirect is consulted (none target Exile
    // today — behavior-preserving, future-proof), mirroring `haunt::resolve`.
    for &id in &[source, partner_id] {
        let res = zone_pipeline::move_object(
            state,
            ZoneMoveRequest::effect(id, Zone::Exile, source),
            events,
        );
        if let zone_pipeline::ZoneMoveResult::NeedsChoice(player) = res {
            // CR 616.1: a future Exile-targeting redirect could surface an
            // ordering choice. Park the prompt and return (no redirect targets
            // Exile today, so this is behavior-preserving).
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    // CR 730.2c-analogue: the survivor is the instigator (`source`), keeping its
    // `ObjectId`. Record the merge identity (CR 712.21 leave-split reads it) and
    // the TYPED meld discriminator (CR 712.4c transform guard keys on it).
    // DO NOT call `apply_card_face_to_object` — meld is LAYER-ONLY; the
    // survivor's `base_*` (its front face, Gisela) must stay intact so it returns
    // as its own front face on leave (CR 712.4b / CR 712.21).
    if let Some(survivor) = state.objects.get_mut(&source) {
        survivor.merged_components = vec![source, partner_id];
        survivor.merge_kind = Some(MergeKind::Meld);
    }

    // CR 712.4b: install the result characteristics (combined back faces) as a
    // layer-1 copy effect WITHOUT mutating the survivor's base identity, so each
    // card returns as its own front face on leave (CR 712.21). Build the values
    // directly from the result face (LAYER-ONLY) — never from the survivor's base
    // (which is intentionally never written here). `install_merge_layer_effect`
    // calls `flush_layers`, so the survivor already presents the result
    // characteristics BEFORE the ETB-emitting entry below — mirroring
    // `merge_object_onto`'s install-then-observe ordering and the conjure entry
    // path, so the ETB scan sees the melded permanent.
    let values = meld_copiable_values(&result_face);
    let printed_ref = printed_ref_from_face(&result_face);
    merge::install_merge_layer_effect(
        state,
        source,
        controller,
        values,
        crate::game::game_object::DisplaySource::Card,
        printed_ref,
        None,
    );

    // CR 701.42a / CR 730.2: absorb the partner into the single melded permanent
    // — it is no longer an independent object; remove it from the exile list and
    // mark it absorbed (zone == Battlefield, in no zone list), mirroring
    // merge_object_onto, so the CR 712.21 leave-split routes it to the graveyard
    // exactly once. This runs BEFORE the survivor's pipeline entry below: an
    // entry-replacement consult (CR 614.1c) can park a `NeedsChoice` pause, and
    // absorbing first guarantees the partner is never stranded in exile across
    // that pause.
    let partner_owner = state.objects.get(&partner_id).map(|o| o.owner);
    if let Some(owner) = partner_owner {
        crate::game::zones::remove_from_zone(state, partner_id, Zone::Exile, owner);
    }
    if let Some(partner) = state.objects.get_mut(&partner_id) {
        partner.zone = Zone::Battlefield;
    }

    // CR 603.6a / CR 701.42a: drive the survivor's exile→battlefield entry through
    // the zone-change pipeline so the `ZoneChanged { to: Battlefield }` event
    // fires and ETB triggers match (unlike Mutate's CR 730.2b suppression). The
    // `Effect` cause is non-exempt, so `execute_zone_move` runs the entry
    // replacement consult (CR 614.1c entries-with-counters / CR 614.12a
    // enters-tapped) that a raw `move_to_zone` would skip. The leave-split
    // (CR 712.21) is wired automatically into the exit seam via
    // `split_merged_permanent_on_leave` — no leave code needed here.
    // `reset_for_battlefield_entry` does NOT clear merged_components / merge_kind
    // / merge_layer_effect_id, so the merge identity survives the entry.
    match zone_pipeline::move_object(
        state,
        ZoneMoveRequest::effect(source, Zone::Battlefield, source),
        events,
    ) {
        zone_pipeline::ZoneMoveResult::Done => {
            // CR 613.1 + CR 400.7: the battlefield entry re-derived the survivor's
            // characteristics from its (intact) base, so re-flush the layers to
            // re-apply the installed meld `CopyValues` on top — the melded
            // permanent presents the result identity (Brisela) once it is on the
            // battlefield (the same re-flush the prior raw path performed). The
            // transient continuous effect persists across the move (its id is
            // keyed to the survivor and not cleared on entry).
            crate::game::layers::flush_layers(state);
            Ok(())
        }
        zone_pipeline::ZoneMoveResult::NeedsChoice(player) => {
            // CR 616.1: an entry replacement surfaced an ordering choice. Park the
            // prompt and return; the partner is already absorbed (above), so it is
            // not stranded across the pause.
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            Ok(())
        }
        // CR 303.4f: only an Aura entering via a non-spell effect needs an
        // enchant-host choice. A melded permanent is never an Aura, so this arm is
        // documented-unreachable — handled exhaustively rather than wildcarded.
        zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => Ok(()),
    }
}
