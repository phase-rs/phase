use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, ResolutionSourceRelatch, StackEntry, StackEntryKind, ZoneChangeCombatStatus,
};
use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    ResolvedControllerOverrideCommand, ResolvedControllerOverrideReplayInvariantError,
    ResolvedEntryProvenanceCommand, ResolvedEntryProvenanceReplayInvariantError,
    ResolvedObjectCeaseCommand, ResolvedObjectCeaseReplayInvariantError, ResolvedZoneChangeCommand,
    ResolvedZoneChangeReplayInvariantError,
};
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::game_object::GameObject;
use super::printed_cards::{apply_back_face_to_object, snapshot_object_face};

/// CR 109.1 + CR 601.2a + CR 405.1: A spell is an object on the stack from
/// announcement, even while this engine retains its origin-zone field until
/// finalization. The retained-origin representation is stack-resident only while
/// the exact spell's `PendingCast` lifecycle and announcement placeholder both
/// remain live; a bare same-id stack entry is insufficient.
fn object_has_stack_residency(state: &GameState, obj: &GameObject) -> bool {
    if obj.zone == Zone::Stack {
        return true;
    }

    let is_pending_spell = |pending: &crate::types::game_state::PendingCast| {
        pending.object_id == obj.id && pending.activation_ability_index.is_none()
    };
    let has_pending_spell = state.pending_cast.as_deref().is_some_and(is_pending_spell)
        || state
            .waiting_for
            .pending_cast_ref()
            .is_some_and(is_pending_spell);

    has_pending_spell
        && state
            .stack
            .iter()
            .any(|entry| entry.id == obj.id && matches!(entry.kind, StackEntryKind::Spell { .. }))
}

/// CR 704.5d / CR 111.7 / CR 111.8: A token outside the battlefield ceases to
/// exist at the next SBA and can't change zones before then. Effectively
/// stack-resident tokens are excluded so announced spell copies can finish
/// casting and resolving before the next applicable SBA check.
pub(super) fn token_is_outside_battlefield_and_stack(state: &GameState, obj: &GameObject) -> bool {
    obj.is_token && obj.zone != Zone::Battlefield && !object_has_stack_residency(state, obj)
}

/// CR 704.5e + CR 707.10a: A copy of a card in any zone other than the stack or
/// the battlefield ceases to exist as a state-based action. Distinct from the
/// token rule (CR 704.5d): a copy of a card is legal on the battlefield
/// (CR 707.10f makes a permanent copy a token there) and may change zones freely
/// while alive, so this predicate is used ONLY by the cease-to-exist SBA — never
/// by the CR 111.8 "can't change zones" movement guards, which apply to tokens only.
pub(super) fn copy_of_card_outside_battlefield_and_stack(
    state: &GameState,
    obj: &GameObject,
) -> bool {
    obj.is_copy && obj.zone != Zone::Battlefield && !object_has_stack_residency(state, obj)
}

/// CR 122.2 + CR 113.6b: Determine whether `object_id`'s counters survive a move
/// into the `to` zone. The default (CR 122.2) is that counters cease to exist on
/// any zone change. A `StaticMode::CountersPersistAcrossZones` ability overrides
/// this for destination zones NOT in its `excluded_zones` list (Me, the
/// Immortal; Skullbriar, the Walking Grave).
///
/// CR 113.6b: the ability is read from the object's state in the zone it is
/// moving FROM. This function must be called while the object's `zone` field
/// still holds the from-zone (before `move_to_zone` updates it), so the
/// ability's `condition` gate is evaluated from the correct zone — matching
/// Me's official ruling.
///
/// Two documented limitations, both arising because this helper reads
/// `obj.static_definitions` directly (via `active_static_definitions`) rather
/// than the layer-resolved view of the object:
///
/// 1. `active_zones`: `active_static_definitions` only enforces the
///    `active_zones` membership gate for the Command zone
///    (functioning_abilities.rs); for other zones the full `active_zones` gate
///    lives in the layers pipeline (layers.rs), which this helper bypasses.
///    Persistence here is therefore gated by `excluded_zones`, not by
///    `active_zones`. This is sound for the shipping cards because their
///    `excluded_zones` (Hand, Library) coincide with the inactive zones where
///    objects never carry counters. A future `CountersPersistAcrossZones` card
///    with a different active/excluded split would need an explicit
///    `active_zones` check added here.
///
/// 2. Layer-6 ability removal (Humility / Yixlid Jailer's "Cards in graveyards
///    lose all abilities"): `evaluate_layers` (layers.rs) only applies layers to
///    battlefield + hand objects, so a graveyard/exile object's
///    `static_definitions` never has its abilities stripped. With Yixlid Jailer
///    in play and Me/Skullbriar in a graveyard bearing counters, this helper
///    still observes the persistence static and would INCORRECTLY persist the
///    counters on a graveyard→exile move — CR 113.6b reads the ability from the
///    from-zone state, where it is rules-meant to be removed. This is not a
///    regression (graveyard ability-removal is unmodeled engine-wide), but it is
///    a known-wrong interaction on this new path, called out here explicitly
///    rather than left implicit.
///    TODO: once `evaluate_layers` applies Layer-6 ability removal to non-
///    battlefield zones, re-check persistence against the layer-resolved view
///    here so Humility/Yixlid correctly suppress it.
fn counters_persist_on_move(state: &GameState, object_id: ObjectId, to: Zone) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    super::functioning_abilities::active_static_definitions(state, obj).any(|def| {
        matches!(
            &def.mode,
            StaticMode::CountersPersistAcrossZones { excluded_zones }
                if !excluded_zones.contains(&to)
        )
    })
}

/// CR 603.10a + CR 603.6e: Capture a snapshot of every attachment on `obj` at the
/// moment of the zone change. The snapshot records each attachment's current
/// controller and kind (Aura/Equipment) so that look-back triggers of the form
/// "for each Aura you controlled that was attached to it" (Hateful Eidolon)
/// can resolve their quantity after SBA has already unattached the Auras.
pub(crate) fn capture_attachment_snapshot(
    state: &GameState,
    obj: &GameObject,
) -> Vec<crate::types::game_state::AttachmentSnapshot> {
    use crate::types::ability::AttachmentKind;
    obj.attachments
        .iter()
        .filter_map(|id| {
            let att = state.objects.get(id)?;
            let kind = if att.card_types.subtypes.iter().any(|s| s == "Aura") {
                AttachmentKind::Aura
            } else if att.card_types.subtypes.iter().any(|s| s == "Equipment") {
                AttachmentKind::Equipment
            } else {
                // Fortifications and other attachment types — skip; only
                // Aura/Equipment predicates are modeled.
                return None;
            };
            Some(crate::types::game_state::AttachmentSnapshot {
                object_id: *id,
                identity: Some(ObjectIncarnationRef::from_object(att)),
                controller: att.controller,
                kind,
            })
        })
        .collect()
}

/// CR 400.7: Snapshot LKI and apply all cleanup side effects when an object
/// leaves its current zone. Shared by `move_to_zone` and `move_to_library_at_index`.
///
/// Handles: LKI snapshot (CR 400.7), activation-use clearing, transform
/// revert (CR 712.14), exile permission clearing (CR 113.6e), monstrous reset
/// (CR 701.37b), counter clearing (CR 122.2), layer pruning, and mana-tap
/// cleanup.
/// `attachments` MUST be captured by the caller BEFORE
/// `sever_battlefield_attachment_graph_on_exit` runs. This function cannot capture it
/// itself: in `move_to_zone` the sever happens first, so by the time we get here the
/// live object's attachment list is already empty. CR 608.2h needs the pre-sever set
/// (see the `LKISnapshot::attachments` doc comment), so the caller — which is the only
/// code that still has it — supplies it.
pub(crate) fn apply_zone_exit_cleanup(
    state: &mut GameState,
    object_id: ObjectId,
    from: Zone,
    to: Zone,
    attachments: Vec<crate::types::game_state::AttachmentSnapshot>,
) {
    // CR 400.7: An object that changes zones becomes a new object with no
    // memory of its previous existence. The information authority receives the
    // pre-move occurrence so the future Zone command can apply this same clear
    // without ever exposing the new incarnation through the old reveal lease.
    let occurrence = state
        .objects
        .get(&object_id)
        .map(ObjectIncarnationRef::from_object)
        .expect("zone-exit cleanup must reference a live object");
    state.clear_revealed_information_on_zone_exit(occurrence);
    // CR 400.7 + CR 702.187b: The "discarded this turn" mark (Mayhem's gate)
    // belongs to the old object. Clear it on any zone change so a card that
    // leaves the graveyard and returns is not treated as still discarded; the
    // discard pipeline re-stamps it after the move-to-graveyard completes.
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.discarded_turn = None;
    }
    // CR 400.7 + CR 403.4: Activation-use history belongs to the old
    // object. `ObjectId` is storage identity here, so clear per-object counts
    // at the zone-change boundary before the same id can represent a new object.
    state
        .activated_abilities_this_turn
        .retain(|(id, _), _| *id != object_id);
    state
        .activated_abilities_this_game
        .retain(|(id, _), _| *id != object_id);

    // CR 400.7: Snapshot LKI before zone change from battlefield or exile.
    // Power/toughness reflect layer modifications on battlefield (Layer 7);
    // from exile they will be None (no layer computation), which is correct.
    if from == Zone::Battlefield || from == Zone::Exile {
        let lki_copiable_values =
            crate::game::layers::compute_current_copiable_values(state, object_id);
        if let Some(obj) = state.objects.get(&object_id) {
            let incarnation = obj.incarnation;
            let lki = crate::types::game_state::LKISnapshot {
                name: obj.name.clone(),
                token_image_ref: obj.token_image_ref.clone(),
                power: obj.power,
                toughness: obj.toughness,
                // CR 208.4b + CR 613.4b: Capture the layer-7b base values so
                // base-scope P/T look-back filters read the base, not current.
                base_power: obj.layer_base_power.or(obj.base_power),
                base_toughness: obj.layer_base_toughness.or(obj.base_toughness),
                // CR 202.3d + CR 709.4b: this LKI is captured on leaving the
                // battlefield or exile (off the stack), so a split card records
                // its combined mana value and colors (no-op for single-face and
                // battlefield Rooms, which gate out).
                mana_value: obj.effective_mana_value(),
                controller: obj.controller,
                owner: obj.owner,
                // CR 400.7: Capture core types for "if it was a creature" patterns.
                card_types: obj.card_types.core_types.clone(),
                subtypes: obj.card_types.subtypes.clone(),
                supertypes: obj.card_types.supertypes.clone(),
                keywords: obj.keywords.clone(),
                colors: obj.effective_colors(),
                chosen_attributes: obj.chosen_attributes.clone(),
                // CR 400.7: Capture counters for "if it had counters on it" patterns.
                counters: obj.counters.clone(),
                // CR 110.5 + CR 110.5d: Capture tap status AT zone exit. Once the
                // object leaves the battlefield it is neither tapped nor untapped,
                // so a use_lki rider ("if it was tapped", Brackish Blunder) reads
                // this captured value instead of the live (now-absent) object.
                tapped: obj.tapped,
                // CR 701.60b: Capture suspected status at zone exit for
                // "was suspected" look-back riders.
                is_suspected: obj.is_suspected,
                // CR 608.2h: The attachment set as it stood BEFORE SBA unattached it
                // (CR 704.5m/n), so a source-referential intervening-if re-checked at
                // resolution ("if this creature is enchanted" — Dreampod Druid) reads
                // last known information once its source has left the battlefield.
                // Supplied by the caller: the sever already ran by the time we get here.
                attachments,
            };
            state.lki_cache.insert(object_id, lki.clone());
            state
                .lki_by_incarnation
                .entry(object_id)
                .or_default()
                .insert(incarnation, lki);
        }
        if let Some(values) = lki_copiable_values {
            state.lki_copiable_values.insert(object_id, values);
        }
    }

    // CR 122.2 + CR 113.6b: Decide counter persistence using the still-current
    // from-zone object state, BEFORE taking the mutable borrow below (the helper
    // needs `&state` to read the object's functioning statics). Me, the
    // Immortal / Skullbriar keep their counters on a move to any zone outside
    // their `excluded_zones`; every other object follows the CR 122.2 default.
    let preserve_counters = counters_persist_on_move(state, object_id, to);

    if let Some(obj_mut) = state.objects.get_mut(&object_id) {
        // CR 400.7 + CR 614.1a: Rod of Absorption's stack-exile rider is a
        // transient marker on the spell object. The stack resolver snapshots it
        // before moving the spell, so all zone exits can clear the field here.
        obj_mut.exile_from_stack_linked_source = None;
        // CR 400.7 + CR 603.7a + CR 702.170c: the exile-instead consequence
        // rider clears in lockstep — a spell that leaves the stack any other
        // way (countered, fizzled) never takes the consequence (Feather's
        // return, Lilah's plot).
        obj_mut.exile_from_stack_rider = None;

        // CR 400.7 + CR 730.3c: a component split out of a merged permanent is a
        // new object on every zone change, so its survivor back-link is
        // meaningful only while it stays in the zone it split into. Clear it on
        // ANY exit (it is re-set by `merge::split_merged_permanent_on_leave` if it
        // re-leaves a merged permanent) so it cannot wrongly re-collect on a later
        // continuity return after moving between non-battlefield zones (e.g.
        // exile → graveyard). The split sets the link AFTER this cleanup runs, so
        // this never clobbers the initial set.
        obj_mut.split_from_merge_survivor = None;

        // CR 712.8a + CR 400.7: Transformed permanents revert to front face on any
        // zone exit (transform DFCs are only valid in transformed state on the battlefield).
        if obj_mut.transformed {
            if let Some(back_face) = obj_mut.back_face.clone() {
                let current_back = snapshot_object_face(obj_mut);
                apply_back_face_to_object(obj_mut, back_face);
                obj_mut.back_face = Some(current_back);
                obj_mut.transformed = false;
            }
        }

        // CR 712.8a + CR 400.7: MDFC objects showing their back face revert to
        // front face in any zone other than the stack or battlefield (back face is
        // valid on the stack while the spell is being cast, and on the battlefield).
        if obj_mut.modal_back_face && to != Zone::Stack && to != Zone::Battlefield {
            if let Some(back_face) = obj_mut.back_face.clone() {
                let current_back = snapshot_object_face(obj_mut);
                apply_back_face_to_object(obj_mut, back_face);
                obj_mut.back_face = Some(current_back);
                obj_mut.modal_back_face = false;
            }
        }

        // CR 708.9: A face-down permanent leaving the battlefield, or a
        // face-down spell leaving the stack for a zone other than the battlefield,
        // is revealed to all players. Restore its stored identity so public zones
        // show the real card instead of a face-down 2/2 shell.
        if obj_mut.face_down
            && (from == Zone::Battlefield || (from == Zone::Stack && to != Zone::Battlefield))
        {
            obj_mut.face_down = false;
            if let Some(back_face) = obj_mut.back_face.take() {
                apply_back_face_to_object(obj_mut, back_face);
            }
        }

        // CR 710.4 + CR 110.5: A flipped permanent that leaves the battlefield
        // retains no memory of its status, and in every zone other than the
        // battlefield a flip card has only its normal characteristics
        // (CR 710.2). Restore the normal half and clear the flipped status.
        //
        // Ordered AFTER the CR 708.9 face-down restore on purpose: a flipped
        // permanent that was then turned face down (Ixidron, Cyber Conversion)
        // shares this one `back_face` slot between both statuses.
        // `effects::turn_face_down` keeps the flip stash (the normal half) in
        // it, so the face-down restore above already puts the normal half back
        // on the object; this call then only has to clear the flipped status
        // (its `back_face == None` branch). Running it first would instead
        // consume the flip stash and leave the face-down 2/2 shell to be
        // restored into the graveyard.
        crate::game::flip::revert_flip_on_zone_exit(obj_mut);

        clear_cast_origin_off_provenance_zones(obj_mut, to);

        // CR 400.7 + CR 113.6e: Clear exile-based casting permissions when leaving exile
        // (prevents re-casting if the card returns to exile via a different effect).
        if from == Zone::Exile {
            // CR 702.143c-d + CR 400.7: Foretold is a designation of the card
            // while it remains in exile. Once it changes zones, the new object
            // is no longer a foretold card.
            obj_mut.foretold = false;
            // CR 708.4: a spell CAST face down (morph/disguise via an exile
            // permission) is turned face down as part of the cast and keeps
            // that status on the stack. Only the exile-zone face-down
            // designation ends here (foretold/hideaway cards, which stash no
            // identity in `back_face`); the cast is the one exile exit whose
            // destination is the stack and whose object carries the cast
            // stash (`spell_is_cast_face_down`, #5171's discriminator).
            if !(to == Zone::Stack && obj_mut.spell_is_cast_face_down()) {
                obj_mut.face_down = false;
            }
            obj_mut.casting_permissions.retain(|p| {
                !matches!(
                    p,
                    crate::types::ability::CastingPermission::AdventureCreature
                        | crate::types::ability::CastingPermission::ExileWithAltCost { .. }
                        | crate::types::ability::CastingPermission::ExileWithAltAbilityCost { .. }
                        | crate::types::ability::CastingPermission::PlayFromExile { .. }
                        | crate::types::ability::CastingPermission::ExileWithEnergyCost
                        | crate::types::ability::CastingPermission::WarpExile { .. }
                        // CR 702.170d + CR 400.7: Plotted permission is scoped
                        // to the exile zone. Once the card leaves exile (cast
                        // resolves, or another effect moves it), drop the
                        // permission so a later return-to-exile doesn't
                        // inherit a stale turn_plotted value.
                        | crate::types::ability::CastingPermission::Plotted { .. }
                        | crate::types::ability::CastingPermission::Foretold { .. }
                )
            });
            state.exile_links.retain(|link| link.exiled_id != object_id);
        }

        // CR 400.7 + CR 601.2a + CR 118.9 + CR 701.17d: An object-tagged cast/play
        // grant ("you may cast it without paying its mana cost", a milled card's
        // "you may play that card") attaches an `ExileWithAltCost` /
        // `ExileWithAltAbilityCost` / `PlayFromExile` permission *in place* on a
        // card picked from the hand or graveyard (Sunforger searches a card to
        // hand and casts it from there; Electrodominance / Emry cast in place;
        // Ark of Hunger / Tablet of Discovery mill-grant in the graveyard) — see
        // `effects::cast_from_zone` and the #751 graveyard `PlayFromExile` path.
        // Such a card never passes through exile, so the exile-exit clear above
        // never fires for it. Each grant authorizes exactly one cast of *that*
        // card; once it has been cast and is leaving the stack (resolved or
        // countered), the spent grant must be dropped so CR 400.7's new object
        // does not inherit it. Without this, a stale grant lands back in the
        // graveyard where `has_graveyard_timed_alt_cost_permission` /
        // `graveyard_spell_objects_available_to_cast` re-offer the free cast on
        // every priority — an unbounded recast loop. Exile-origin grants are
        // already cleared at the Exile→Stack move, so this is a no-op for them
        // (impulse draw, Suspend, Discover, Cascade). Other exile-scoped
        // permissions (AdventureCreature, Plotted, Foretold, WarpExile) are left
        // untouched: an Adventure spell, for instance, gains `AdventureCreature`
        // precisely as it resolves to exile.
        if from == Zone::Stack {
            obj_mut.casting_permissions.retain(|p| {
                !matches!(
                    p,
                    crate::types::ability::CastingPermission::ExileWithAltCost { .. }
                        | crate::types::ability::CastingPermission::ExileWithAltAbilityCost { .. }
                        | crate::types::ability::CastingPermission::PlayFromExile { .. }
                )
            });
        }

        if from == Zone::Battlefield {
            obj_mut.reset_for_battlefield_exit();
        }

        // CR 702.103b: A bestowed Aura's type-changing effect lasts until the
        // spell or permanent ceases to be bestowed (CR 702.103e–g). The form
        // is applied at cast-prepare time on the hand object, so it must
        // persist through every zone change while the spell/permanent is in a
        // "live bestow" state — that is, on its way to the stack from hand,
        // on the stack as a bestowed Aura spell, and on the battlefield as
        // the bestowed Aura permanent. Revert only when the object leaves
        // those live zones to a "dead" zone:
        //   * Stack → Graveyard / Hand / Library / Exile / Command (countered,
        //     bounced, exiled — the spell ceases to exist as a bestow Aura).
        //   * Battlefield → anywhere (death, exile, bounce — the printed
        //     creature face is restored for graveyard / exile-cast / future
        //     interactions).
        // CR 702.103f's unattach exception keeps the form on the battlefield
        // through SBA-driven unattach (handled in sba.rs::check_unattached_auras
        // by calling `revert_bestow_form` before the SBA runs).
        // Idempotent — a no-op if the flag is already false (e.g., the
        // CR 702.103e illegal-target path reverts before move_to_zone fires).
        let preserve_bestow_form = match from {
            // Hand / Library / Graveyard / Exile / Command → Stack: cast
            // bestowed; the form was just applied during cast preparation
            // and must persist as the spell enters the stack.
            _ if to == Zone::Stack => true,
            // Stack → Battlefield: bestowed Aura resolves as the bestowed
            // permanent (CR 702.103b "the permanent it becomes as it resolves
            // will be a bestowed Aura").
            Zone::Stack if to == Zone::Battlefield => true,
            _ => false,
        };
        if !preserve_bestow_form && obj_mut.bestow_form.is_some() {
            super::casting::revert_bestow_aura_form(obj_mut);
            state.layers_dirty.mark_full();
        }

        // CR 702.148a + CR 612: A cleave spell's text-changing effect functions
        // only "while a spell with cleave is on the stack." The bracket-removed
        // ability set is installed on the hand object at cast time and must be
        // reverted to the printed form when the spell leaves the stack —
        // whether it resolved (Stack → Graveyard/Exile), was countered, or
        // fizzled. Without this revert the same object id carries the cleave
        // (bracket-removed) abilities into the graveyard, and a graveyard→hand
        // recursion (Regrowth, Eternal Witness) — which reuses the object id
        // without re-projecting the printed face — would let a later
        // normal-cost recast resolve with the wrong (cleave) text.
        //
        // Gated the same way as bestow (preserve only on → Stack and on
        // Stack → Battlefield) so the logic is uniform and future-proof, even
        // though cleave instants/sorceries never resolve onto the battlefield.
        let preserve_cleave_form = match from {
            _ if to == Zone::Stack => true,
            Zone::Stack if to == Zone::Battlefield => true,
            _ => false,
        };
        if !preserve_cleave_form && obj_mut.cleave_form.is_some() {
            super::casting::revert_cleave_text_change(obj_mut);
        }

        // CR 702.160a + CR 400.7: Prototype's alternative characteristics
        // apply only to the spell/permanent produced by casting it prototyped.
        // Preserve the marker while the cast becomes a stack spell and while
        // that spell resolves to the battlefield; clear it for every other
        // zone change so the new object reverts to printed characteristics.
        let preserve_prototype_form = match from {
            _ if to == Zone::Stack => true,
            Zone::Stack if to == Zone::Battlefield => true,
            _ => false,
        };
        if !preserve_prototype_form && obj_mut.prototype_form.is_some() {
            super::casting::clear_prototype_form(obj_mut);
            state.layers_dirty.mark_full();
        }

        // CR 400.7d + CR 702.150a: Compleated's Phyrexian life-payment count
        // is cast metadata. Preserve it while the cast object moves to the
        // stack, and while the resolving permanent spell becomes the
        // battlefield object whose ETB counter replacement will consume it.
        // Every other zone change creates an object with no memory of that
        // payment.
        let preserve_phyrexian_life_paid =
            to == Zone::Stack || (from == Zone::Stack && to == Zone::Battlefield);
        if !preserve_phyrexian_life_paid {
            obj_mut.phyrexian_life_paid = 0;
        }

        // CR 122.2: Counters cease to exist when an object changes zones —
        // UNLESS a `CountersPersistAcrossZones` ability (read from the from-zone
        // state above) keeps them for this destination (CR 113.6b). Me, the
        // Immortal / Skullbriar retain their counters on a move to any zone
        // other than a player's hand or library.
        if !preserve_counters {
            obj_mut.counters.clear();
        }
        if !crate::game::stickers::zone_retains_stickers(to) && !obj_mut.stickers.is_empty() {
            obj_mut.stickers.clear();
            obj_mut.revert_layered_characteristics_to_base();
        }
    }

    if from == Zone::Battlefield {
        // CR 701.54e: A player's Ring-bearer designation applies only while
        // that permanent remains on the battlefield under that player's control.
        super::effects::ring::clear_ring_bearer_if_object(state, object_id);
    }

    // Prune host-bound transient effects and clean up mana-tap tracking
    // when a permanent leaves the battlefield.
    if from == Zone::Battlefield {
        // CR 506.4: A permanent is removed from combat when it leaves the
        // battlefield. Combat role is snapshotted into the zone-change record
        // (capture_combat_status) before this cleanup runs so look-back
        // triggers still read attacking/blocking status (CR 603.10a).
        super::effects::remove_from_combat::remove_object_from_combat(state, object_id);
        super::pairing::break_pair(state, object_id);
        crate::game::layers::mark_layers_full(state);
        // CR 400.7 + CR 702.11b: The "has dealt damage since entering" sticky flag
        // belongs to the old object. ObjectId persists across this zone change, so
        // clear it on a battlefield exit (death/bounce/exile/flicker) — otherwise a
        // flickered "has hexproof if it hasn't dealt damage yet" creature would
        // re-enter still treated as having dealt damage and never regain hexproof.
        state.objects_that_dealt_damage.remove(&object_id);
        super::layers::prune_host_left_effects(state, object_id);
        super::layers::prune_affected_object_left_effects(state, object_id);
        // CR 611.2b + CR 400.7: the captured source leaving play, OR the host
        // leaving and re-entering as a new object (same storage ObjectId), ends
        // the "can't become untapped for as long as you control [source]"
        // continuous effect permanently — drop the gated def from base+live so
        // it cannot revive on a same-ObjectId re-entry.
        super::layers::prune_controller_controls_source_on_leave(state, object_id);
        // CR 613.1 + CR 400.7: Copy effects are pruned above, but layer-derived
        // characteristics (name, types, abilities) persist on the object until
        // explicitly reset. Revert to printed baseline so graveyard/exile objects
        // do not retain copied identity (Vesuva legend-rule sacrifice).
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.revert_layered_characteristics_to_base();
            if crate::game::stickers::zone_retains_stickers(to) && !obj.stickers.is_empty() {
                crate::game::stickers::rebuild_public_zone_stickers(obj);
            }
        }
        for tapped in state.lands_tapped_for_mana.values_mut() {
            tapped.retain(|&id| id != object_id);
        }
        // CR 400.7 + CR 610.3: Drop `TrackedBySource` exile links keyed to a
        // source that has now left the battlefield. Object identity resets, so
        // a re-entering (e.g. blinked) permanent must not inherit the previous
        // object's "exiled with" linkage (Pit of Offerings, Bojuka Bog, etc.).
        // `UntilSourceLeaves` links are intentionally preserved here because
        // `check_exile_returns` runs later in the priority loop and consumes
        // them to return the exiled cards (CR 610.3a).
        // CR 702.55b + CR 702.55c: `Haunt` links are likewise preserved — the haunted
        // creature leaving the battlefield (its death) is exactly when the
        // card's haunt-payoff trigger reads the link to fire from exile. The
        // link is pruned later, when the haunting card itself leaves exile.
        // CR 702.167a/c: `CraftMaterial` links are preserved too — the craft
        // source self-exiles mid-activation and returns with the same ObjectId,
        // so the material links must survive its battlefield exit for the
        // returned permanent to still read what it was crafted with.
        // CR 607.2a + CR 400.7: `TrackedBySource` links are preserved when the
        // source leaves the battlefield TO EXILE. A source that self-exiles
        // (typically as its own activation cost — Mechtitan Core: "Exile this
        // Vehicle and four other …: … return all cards exiled with this Vehicle
        // …") keeps a stable ObjectId in exile and remains the linked-ability
        // referent (CR 607.2a: the second ability refers to the cards put in exile
        // by the first). Preserve its links so a deferred `ExiledBySource` return
        // still finds the pile turns later — the blink-identity reset instead
        // happens if that source RE-ENTERS the battlefield (paired entry prune
        // below). Narrowed to the exile-exit so ordinary death/bounce (to
        // graveyard/hand) still prunes.
        let source_exits_to_exile = to == Zone::Exile;
        state.exile_links.retain(|link| {
            link.source_id != object_id
                || matches!(
                    link.kind,
                    crate::types::game_state::ExileLinkKind::UntilSourceLeaves { .. }
                        | crate::types::game_state::ExileLinkKind::UntilOpponentBecomesMonarch { .. }
                        | crate::types::game_state::ExileLinkKind::Haunt
                        | crate::types::game_state::ExileLinkKind::CraftMaterial
                )
                || (source_exits_to_exile
                    && matches!(
                        link.kind,
                        crate::types::game_state::ExileLinkKind::TrackedBySource
                    ))
        });

        // PR-7 Phase 4c (B5 defuse): CR 104.4b / CR 110.1 / CR 700.4 — an enabling
        // permanent leaving the battlefield revokes the revocable-∞ capability it
        // enabled (every-enabler: `interactive_loop_bridge` Path C). Gated on a
        // non-empty enabler map so Off/On games (which never populate it — only the
        // Interactive B5 arm does) pay nothing and stay byte-identical. Whole-
        // capability clear per controller whose enabler set contains this object:
        // `clear_unbounded_loop` drops SIX maps, incl. the accepted-collapse stash.
        if !state.unbounded_loop_enablers.is_empty() {
            let revoked: Vec<PlayerId> = state
                .unbounded_loop_enablers
                .iter()
                .filter(|(_, ids)| ids.contains(&object_id))
                .map(|(c, _)| *c)
                .collect();
            for controller in revoked {
                state.clear_unbounded_loop(controller);
            }
        }
    }
}

/// Allocate a new ObjectId, create a GameObject with defaults, insert into state.objects, and add to the specified zone.
pub fn create_object(
    state: &mut GameState,
    card_id: CardId,
    owner: PlayerId,
    name: String,
    zone: Zone,
) -> ObjectId {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    let obj = GameObject::new(id, card_id, owner, name, zone);
    state.objects.insert(id, obj);
    add_to_zone(state, id, zone, owner);

    // CR 302.6 + CR 403.4: Record ETB turn as a global counter (used by
    // "this turn" triggers and filters). NOTE: this helper is used both for
    // initial test/scenario setup and for a few synthesis paths. The
    // summoning-sickness flag (`summoning_sick`) is NOT set here — it's set
    // on the real ETB pipeline via `GameObject::reset_for_battlefield_entry`
    // (invoked by `move_to_zone`). This keeps test scaffolding that places
    // "pre-existing" creatures directly on the battlefield (before any turn
    // has run) from spuriously starting sick.
    if zone == Zone::Battlefield {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.entered_battlefield_turn = Some(state.turn_number);
        }
    }

    id
}

/// CR 700.11: A player has "descended this turn" when a permanent card has
/// been put into their graveyard from anywhere this turn. Single authority for
/// the descend bookkeeping, shared by `move_to_zone` and the merge-split
/// component delivery (`merge::put_component_into_zone`). Tokens are not cards
/// and do not count.
pub(crate) fn record_descend_on_graveyard_arrival(
    state: &mut GameState,
    object_id: ObjectId,
    owner: PlayerId,
) {
    let is_permanent_card = state.objects.get(&object_id).is_some_and(|obj| {
        !obj.is_token
            && obj
                .card_types
                .core_types
                .iter()
                .any(|ct| ct.is_permanent_type())
    });
    if is_permanent_card {
        if let Some(player) = state.players.iter_mut().find(|p| p.id == owner) {
            player.descended_this_turn = true;
        }
    }
}

/// CR 400.7j (+ CR 400.7g/h cast hop): if the currently-resolving ability just
/// moved its OWN source (`object_id == resolving ability's source_id`), record the
/// from→to incarnation so `source_is_current` can re-find the moved object after
/// the all-zone bump advanced its epoch. Chains across multiple self-moves in one
/// resolution: the first self-move sets `original_stamp` (bound to the ability's
/// captured incarnation); a chained self-move keeps `original_stamp` fixed and only
/// advances `current_incarnation`. A foreign object, or a move whose pre-move
/// incarnation matches neither the captured stamp nor the record's current value,
/// never writes. Call AFTER the bump, passing the pre-bump and post-bump values.
pub(crate) fn record_resolution_source_relatch(
    state: &mut GameState,
    object_id: ObjectId,
    pre_move_incarnation: u64,
    new_incarnation: u64,
) {
    // A faithful READ of the resolving ability's captured source identity. The
    // clone is disconnected from the local resolving borrow, so it cannot be the
    // carrier — the record on `state` is (consumed inside `source_is_current`).
    let Some((source_id, Some(captured))) = state
        .resolving_stack_entry
        .as_ref()
        .and_then(StackEntry::ability)
        .map(|a| (a.source_id, a.trigger_source_incarnation()))
    else {
        return;
    };
    if object_id != source_id {
        return;
    }
    // First self-move: pre-move value must equal the ability's captured stamp.
    let matches_first = pre_move_incarnation == captured;
    // Chained self-move: pre-move value must equal the record's current value.
    let chained = state
        .resolution_source_relatch
        .as_ref()
        .filter(|r| r.object_id == object_id && r.current_incarnation == pre_move_incarnation);
    if matches_first || chained.is_some() {
        // Keep `original_stamp` fixed across chained hops; the CR 400.7j identity
        // is the FIRST captured stamp, not each intermediate incarnation.
        let original_stamp = chained.map_or(captured, |r| r.original_stamp);
        state.resolution_source_relatch = Some(ResolutionSourceRelatch {
            object_id,
            original_stamp,
            current_incarnation: new_incarnation,
        });
    }
}

fn zone_container_len(
    state: &GameState,
    zone: Zone,
    owner: PlayerId,
    object_id: ObjectId,
) -> usize {
    match zone {
        Zone::Library => state
            .players
            .iter()
            .find(|player| player.id == owner)
            .expect("zone command owner exists")
            .library
            .len(),
        Zone::Hand => state
            .players
            .iter()
            .find(|player| player.id == owner)
            .expect("zone command owner exists")
            .hand
            .len(),
        Zone::Graveyard => state
            .players
            .iter()
            .find(|player| player.id == owner)
            .expect("zone command owner exists")
            .graveyard
            .len(),
        Zone::Battlefield => state.battlefield.len(),
        Zone::Stack => state.stack.len(),
        Zone::Exile => state.exile.len(),
        Zone::Command => {
            let object = state
                .objects
                .get(&object_id)
                .expect("zone command object exists");
            let player = state
                .players
                .iter()
                .find(|player| player.id == owner)
                .expect("zone command owner exists");
            if object.in_attraction_deck {
                player.attraction_deck.len()
            } else if object.in_contraption_deck {
                player.contraption_deck.len()
            } else {
                state.command_zone.len()
            }
        }
    }
}

fn destination_position_after_removal(
    state: &GameState,
    object_id: ObjectId,
    from: Zone,
    to: Zone,
    owner: PlayerId,
) -> usize {
    let destination_len = zone_container_len(state, to, owner, object_id);
    if from == to {
        destination_len
            .checked_sub(1)
            .expect("moving object must occupy its source container")
    } else {
        destination_len
    }
}

/// Resolves, applies, and journals the transition core of one ordinary zone move.
///
/// CR 400.7 + CR 613.7d: the ordinary path consumes its timestamp, captures the
/// resulting incarnation and destination position, then delegates the exact
/// installation to [`apply_resolved_zone_change`]. Replay never allocates a
/// timestamp or a new object identity.
pub fn resolve_and_apply_zone_change(
    state: &mut GameState,
    object_id: ObjectId,
    from: Zone,
    to: Zone,
    owner: PlayerId,
    mut zone_change_record: crate::types::game_state::ZoneChangeRecord,
) -> Result<ResolvedZoneChangeCommand, ResolvedZoneChangeReplayInvariantError> {
    let object = state.objects.get(&object_id).ok_or(
        ResolvedZoneChangeReplayInvariantError::UnknownObject(object_id),
    )?;
    let occurrence = ObjectIncarnationRef::from_object(object);
    if object.zone != from {
        return Err(ResolvedZoneChangeReplayInvariantError::SourceZoneMismatch {
            expected: from,
            found: object.zone,
        });
    }
    if object.owner != owner {
        return Err(ResolvedZoneChangeReplayInvariantError::OwnerMismatch {
            expected: owner,
            found: object.owner,
        });
    }

    let entry_timestamp = (to == Zone::Battlefield).then(|| state.next_timestamp());
    let resulting_incarnation = if to == Zone::Battlefield || from != to {
        occurrence.incarnation + 1
    } else {
        occurrence.incarnation
    };
    let destination_position =
        destination_position_after_removal(state, object_id, from, to, owner);
    let turn_zone_change_index = state.zone_changes_this_turn.len();
    zone_change_record.entered_incarnation =
        (to == Zone::Battlefield).then_some(resulting_incarnation);
    zone_change_record.turn_zone_change_index = turn_zone_change_index;
    zone_change_record.recorded_turn_number = state.turn_number;

    let command = ResolvedZoneChangeCommand {
        object: occurrence,
        resulting_incarnation,
        from,
        to,
        destination_position,
        owner,
        entry_timestamp,
        turn_zone_change_index,
        zone_change_record,
        cause: state.current_or_begin_rules_execution_node(),
    };
    apply_resolved_zone_change(state, &command)?;
    state
        .resolved_rules_journal
        .record_zone_change(command.clone())
        .expect("resolved zone change must have a live journal cause");
    Ok(command)
}

/// Installs one recorded transition core without a replacement consult, query,
/// timestamp allocation, or incarnation allocation.
/// CR 400.7: the narrow `cast_from_zone` lifetime — the stamp survives only
/// onto the STACK (the cast itself) and onto the BATTLEFIELD (whose entry
/// reset + `CastLinkSnapshot` restore own it there,
/// `reset_for_battlefield_entry`/`_exit`). Every other destination clears it,
/// so a spell leaving the stack countered/fizzled/resolved-to-graveyard
/// cannot hand a stale origin to a later recast. ONE primitive shared by the
/// live transition cleanup and the resolved-zone-change replay applier, so
/// replay equivalence holds by construction rather than by two hand-kept
/// conditions.
pub(crate) fn clear_cast_origin_off_provenance_zones(
    obj: &mut crate::game::game_object::GameObject,
    to: Zone,
) {
    if to != Zone::Stack && to != Zone::Battlefield {
        obj.cast_from_zone = None;
    }
}

pub fn apply_resolved_zone_change(
    state: &mut GameState,
    command: &ResolvedZoneChangeCommand,
) -> Result<(), ResolvedZoneChangeReplayInvariantError> {
    let turn_number = state.turn_number;
    let object = state.objects.get(&command.object.object_id).ok_or(
        ResolvedZoneChangeReplayInvariantError::UnknownObject(command.object.object_id),
    )?;
    let found = ObjectIncarnationRef::from_object(object);
    if found != command.object {
        return Err(ResolvedZoneChangeReplayInvariantError::OccurrenceMismatch {
            expected: command.object,
            found,
        });
    }
    if object.owner != command.owner {
        return Err(ResolvedZoneChangeReplayInvariantError::OwnerMismatch {
            expected: command.owner,
            found: object.owner,
        });
    }
    if object.zone != command.from {
        return Err(ResolvedZoneChangeReplayInvariantError::SourceZoneMismatch {
            expected: command.from,
            found: object.zone,
        });
    }
    if command.to == Zone::Battlefield && command.entry_timestamp.is_none() {
        return Err(ResolvedZoneChangeReplayInvariantError::MissingBattlefieldEntryTimestamp);
    }
    if command.to != Zone::Battlefield && command.entry_timestamp.is_some() {
        return Err(ResolvedZoneChangeReplayInvariantError::UnexpectedNonbattlefieldTimestamp);
    }
    if command.turn_zone_change_index != state.zone_changes_this_turn.len() {
        return Err(
            ResolvedZoneChangeReplayInvariantError::TurnRecordIndexMismatch {
                expected: command.turn_zone_change_index,
                found: state.zone_changes_this_turn.len(),
            },
        );
    }
    if command.zone_change_record.recorded_turn_number != state.turn_number {
        return Err(
            ResolvedZoneChangeReplayInvariantError::RecordedTurnMismatch {
                expected: command.zone_change_record.recorded_turn_number,
                found: state.turn_number,
            },
        );
    }

    let destination_position = destination_position_after_removal(
        state,
        command.object.object_id,
        command.from,
        command.to,
        command.owner,
    );
    if destination_position != command.destination_position {
        return Err(
            ResolvedZoneChangeReplayInvariantError::DestinationPositionMismatch {
                expected: command.destination_position,
                found: destination_position,
            },
        );
    }

    remove_from_zone(state, command.object.object_id, command.from, command.owner);
    add_to_zone(state, command.object.object_id, command.to, command.owner);

    let object = state
        .objects
        .get_mut(&command.object.object_id)
        .expect("validated zone command object remains live");
    object.zone = command.to;
    if command.to == Zone::Battlefield {
        object.reset_for_battlefield_entry(
            turn_number,
            command
                .entry_timestamp
                .expect("validated battlefield command has a timestamp"),
        );
    } else {
        object.incarnation = command.resulting_incarnation;
        // CR 400.7: same cast-origin lifetime as the live transition cleanup.
        clear_cast_origin_off_provenance_zones(object, command.to);
    }
    if object.incarnation != command.resulting_incarnation {
        return Err(
            ResolvedZoneChangeReplayInvariantError::ResultingIncarnationMismatch {
                expected: command.resulting_incarnation,
                found: object.incarnation,
            },
        );
    }

    // CR 613.7d: the battlefield entry drew this timestamp during the original
    // execution, so replay installs it rather than drawing a fresh one — and
    // must carry the allocator past it or a later draw reissues it. A move to
    // any other zone drew none, which is why this is bound to the recorded
    // `Option` rather than applied to every zone change.
    if let Some(entry_timestamp) = command.entry_timestamp {
        state.adopt_replayed_timestamp(entry_timestamp);
    }

    let mut zone_change_record = command.zone_change_record.clone();
    let turn_zone_change_index =
        super::restrictions::record_zone_change(state, &mut zone_change_record);
    if turn_zone_change_index != command.turn_zone_change_index {
        return Err(
            ResolvedZoneChangeReplayInvariantError::TurnRecordIndexMismatch {
                expected: command.turn_zone_change_index,
                found: turn_zone_change_index,
            },
        );
    }
    Ok(())
}

/// CR 400.7: Move an object to a new zone. An object that moves to a new zone becomes a new object.
///
/// Plain-entry convenience wrapper: delegates to
/// [`move_to_zone_with_entry_flags`] with `enter_transformed = false`, so
/// every existing call site that does not instruct an effect-driven
/// transformed entry is unchanged. Only the plain-fallback branch of
/// `deliver_replaced_zone_change` threads the flag through the
/// `with_entry_flags` form.
pub fn move_to_zone(
    state: &mut GameState,
    object_id: ObjectId,
    to: Zone,
    events: &mut Vec<GameEvent>,
) {
    move_to_zone_with_entry_flags(state, object_id, to, events, false);
}

/// CR 400.7: Move an object to a new zone. An object that moves to a new zone becomes a new object.
///
/// `enter_transformed` (CR 712.14a) is the transient, single-authority "enters
/// with its back face up" intent carried LIVE from the post-replacement
/// `ProposedEvent::ZoneChange.enter_transformed` into the battlefield-entry
/// guard below. It is a synchronous parameter for this one delivery — never a
/// stored/written `GameState` field.
///
/// WHY a parameter rather than a transient `obj.transformed` marker: CR 712.8a
/// (zones.rs:291-298) reverts a transformed permanent to its front face on any
/// non-battlefield zone exit, and the post-move transform itself
/// (zone_pipeline.rs:3670, `transform_permanent`, CR 712.14a) executes the same
/// face swap when the object reaches the battlefield. A pre-move transient
/// `transformed` flag would survive into that authoritative swap and
/// double-corrupt the face (CR 712.8a exit revert + post-move transform both
/// mutating `back_face`/the live face). The parameter carries the intent without
/// touching object state. (The `modal_back_face` revert at zones.rs:301-310 is
/// a SEPARATE MDFC mechanism and is not implicated.)
///
/// SF1 asymmetry: a single-faced object (`back_face.is_none()`) instructed to
/// enter transformed can never enter that way — CR 712.14a (2nd sentence)
/// requires a back face, and the object's FRONT-face core types must NOT be
/// consulted as a fallback for the CR 307.4 / CR 400.4a eligibility check. The
/// asymmetric guard below therefore returns before any core-type consult.
///
/// A3 (no post-move re-assert): unlike the face-down entry profile's
/// re-assertion authority (`apply_face_down_entry_profile` in zone_pipeline.rs),
/// a transformed entry needs no analogous re-assert after the move.
/// `transform_permanent` (zone_pipeline.rs:3687) is the SINGLE authoritative
/// post-move face swap and already runs on `to == Zone::Battlefield`, so the
/// guard here only gates eligibility — it never mutates the face.
pub(crate) fn move_to_zone_with_entry_flags(
    state: &mut GameState,
    object_id: ObjectId,
    mut to: Zone,
    events: &mut Vec<GameEvent>,
    enter_transformed: bool,
) {
    // CR 111.8: A token that has left the battlefield can't move to another zone
    // or come back onto the battlefield — "if such a token would change zones, it
    // remains in its current zone instead." It ceases to exist at the next SBA
    // (CR 111.7, enforced in sba.rs). Without this guard a single-resolution
    // flicker ("exile target permanent, then return it") on a token would return
    // it before the cease-to-exist SBA runs. The Stack carve-out matches the
    // CR 111.7 SBA so a copy of a spell still resolves off the stack normally.
    if state
        .objects
        .get(&object_id)
        .is_some_and(|obj| token_is_outside_battlefield_and_stack(state, obj))
    {
        return;
    }

    // CR 614.12 + CR 701.42: a meld result is projected while its two physical
    // cards remain in exile. Once its approved battlefield delivery commits,
    // that projection is the authority for the entry snapshot; the source
    // card's front-face object remains the storage authority so the meld can
    // later split back into its physical fronts.
    let liminal_entry_projection = (to == Zone::Battlefield)
        .then(|| {
            state
                .liminal_entries
                .get(&object_id)
                .map(|entry| entry.object.projected().clone())
        })
        .flatten();
    let liminal_attack_target = (to == Zone::Battlefield)
        .then(|| {
            state
                .liminal_entries
                .get(&object_id)
                .and_then(|entry| match &entry.kind {
                    crate::types::game_state::LiminalEntryKind::Meld { attack_target, .. } => {
                        *attack_target
                    }
                    crate::types::game_state::LiminalEntryKind::Token => None,
                })
        })
        .flatten();

    // CR 903.9a: A fresh zone change resets the "declined zone return" flag
    // so the owner gets a new choice opportunity if the commander moves again.
    state.commander_declined_zone_return.remove(&object_id);

    // CR 614.1d: Check CantEnterBattlefieldFrom statics before allowing the move.
    // e.g., Grafdigger's Cage: "Creature cards in graveyards and libraries can't enter the battlefield."
    if to == Zone::Battlefield {
        if let Some(obj) = liminal_entry_projection
            .as_ref()
            .or_else(|| state.objects.get(&object_id))
        {
            if is_blocked_from_entering_battlefield(state, obj) {
                return;
            }
            // CR 712.14a (2nd sentence) + CR 712.8e: a transformed entry reads
            // the BACK face's card types for the CR 307.4 / CR 400.4a
            // eligibility check (CR 712.8e: "read from its back face"). A
            // single-faced object instructed to enter transformed has no back
            // face, so it can never enter that way — and its FRONT face's
            // permanent types must NOT be consulted as a fallback (CR 712.14a
            // 2nd sentence). This asymmetric guard precedes any core-type
            // consult so the front face is never used for a transformed entry.
            if enter_transformed && obj.back_face.is_none() {
                return; // CR 712.14a: no back face -> remain in previous zone
            }
            let entry_core_types = if enter_transformed {
                // CR 712.14a + CR 712.8e: eligibility reads the back face's core
                // types. `back_face` is guaranteed `Some` after the guard above;
                // the `unwrap_or_default()` empty-slice is an unreachable
                // safeguard (present only so the borrow stays total).
                obj.back_face
                    .as_ref()
                    .map(|b| b.card_types.core_types.as_slice())
                    .unwrap_or_default()
            } else {
                obj.card_types.core_types.as_slice()
            };
            // CR 304.4 / CR 307.4 / CR 400.4a: Instants and sorceries can't enter
            // the battlefield. Skip for face-down (morph/manifest) and objects with
            // a permanent type (DFC/MDFC back faces).
            if !obj.face_down
                && (entry_core_types.contains(&CoreType::Instant)
                    || entry_core_types.contains(&CoreType::Sorcery))
                && !entry_core_types.iter().any(|ct| {
                    matches!(
                        ct,
                        // CR 110.4: Permanent types
                        CoreType::Creature
                            | CoreType::Artifact
                            | CoreType::Enchantment
                            | CoreType::Planeswalker
                            | CoreType::Land
                            | CoreType::Battle
                    )
                })
            {
                return; // CR 400.4a: Remain in previous zone
            }
        }
    }

    // CR 730.3: When a merged permanent leaves the battlefield, each absorbed
    // component is routed to its own owner's destination zone before the surviving
    // object completes its move. No-op for non-merged objects. Done here (while
    // the object is still on the battlefield with its `merged_components` intact,
    // before `apply_zone_exit_cleanup` clears them).
    {
        let leaving_battlefield = state
            .objects
            .get(&object_id)
            .is_some_and(|o| o.zone == Zone::Battlefield && !o.merged_components.is_empty());
        if leaving_battlefield {
            super::merge::split_merged_permanent_on_leave(state, object_id, to, events);
        }
    }

    let obj = state.objects.get(&object_id).expect("object exists");
    let from = obj.zone;

    // CR 603.2g + CR 603.6a: A Battlefield → Battlefield no-op does not put a
    // permanent onto the battlefield, so no trigger event occurs and no ETB
    // ability can trigger. No new object is created and no ZoneChanged event is
    // emitted.
    // Without this guard, move_to_zone(coiling_id, Battlefield) while Coiling
    // Oracle is already on the battlefield removes then re-adds it, emits a
    // spurious ZoneChanged{from:Battlefield, to:Battlefield} event, and fires
    // its own ETB trigger again — causing an infinite loop.
    if from == Zone::Battlefield && to == Zone::Battlefield {
        return;
    }

    let owner = obj.owner;
    let redirect_attraction_to_command = super::attractions::is_attraction_card(obj)
        && !matches!(to, Zone::Battlefield | Zone::Exile | Zone::Command);
    if redirect_attraction_to_command {
        // CR 717.6: Astrotorium-backed cards that would move to any zone other
        // than battlefield, exile, or command move to command instead.
        to = Zone::Command;
    }
    // CR 400.7 + CR 611.3a: a static may depend on an object's membership in
    // either the leaving or destination zone through its recipient filter,
    // condition, or dynamic quantity. Query before the move and repeat below
    // after the object reaches its destination, before any trigger collection.
    let static_dependency_before =
        crate::game::layers::static_layer_dependency_for_zone_transition(state, from, to);
    let unattached_from = state.objects.get(&object_id).and_then(|obj| {
        obj.attached_to
            .map(super::effects::attach::target_ref_from_attach_target)
    });
    let snapshot_object = liminal_entry_projection.as_ref().unwrap_or(obj);
    let mut zone_change_record =
        snapshot_object.snapshot_for_zone_change(object_id, Some(from), to);
    // CR 603.10a + CR 603.6e: Capture attachment snapshot before SBA can detach.
    zone_change_record.attachments = capture_attachment_snapshot(state, obj);
    // CR 603.10a + CR 607.2a: Leaves-the-battlefield triggers look back to the
    // object as it existed immediately before the move. Snapshot linked "exiled
    // with" cards here, before CR 400.7 cleanup prunes `TrackedBySource`.
    zone_change_record.linked_exile_snapshot =
        capture_linked_exile_snapshot(state, object_id, from);
    zone_change_record.sync_trigger_source_exiled_cards(
        state
            .cards_exiled_with_source_this_turn
            .get(&object_id)
            .cloned()
            .unwrap_or_default(),
    );
    // CR 607.2b + CR 603.10e: Persist the linked-exile snapshot as last-known
    // information so a self-sacrifice ability that refers to "cards exiled with
    // this permanent" (Rod of Absorption) still resolves correctly after its own
    // source is gone and the live `TrackedBySource` links have been pruned.
    if !zone_change_record.linked_exile_snapshot.is_empty() {
        state
            .linked_exile_lki
            .insert(object_id, zone_change_record.linked_exile_snapshot.clone());
    }
    zone_change_record.combat_status = if let Some(target) = liminal_attack_target {
        ZoneChangeCombatStatus {
            attacking: true,
            defending_player: super::combat::entry_attack_target_defender(
                state,
                snapshot_object.controller,
                target,
            ),
            ..ZoneChangeCombatStatus::default()
        }
    } else {
        capture_combat_status(state, object_id)
    };
    zone_change_record.sync_trigger_source_context();

    sever_battlefield_attachment_graph_on_exit(state, object_id, &unattached_from);

    // CR 730.2d + CR 111.7: for a merged permanent whose topmost component
    // temporarily changed the survivor's token-ness, the ZoneChanged record above
    // must retain the merged permanent's event-time token-ness. Restore the
    // survivor only after that snapshot so the moved token component can cease to
    // exist without corrupting leave-trigger filters.
    super::merge::restore_pre_merge_tokenness_for_leave(state, object_id);

    // CR 608.2h: hand the LKI the PRE-SEVER attachment set captured above — the sever
    // has already emptied the live object's attachment list by this point.
    apply_zone_exit_cleanup(
        state,
        object_id,
        from,
        to,
        zone_change_record.attachments.clone(),
    );

    // Command-zone routes select between the ordinary command container and
    // the owner-specific Attraction/Contraption containers. Stack routes have
    // their `StackEntry` insertion/removal owned by the casting and resolution
    // paths, not by `add_to_zone`. Those special containers are outside this
    // first cut, so preserve their existing raw transition rather than encoding
    // a partial container identity in this generic command.
    let (pre_bump_incarnation, new_incarnation, transition_recorded) =
        if matches!(from, Zone::Command | Zone::Stack) || matches!(to, Zone::Command | Zone::Stack)
        {
            remove_from_zone(state, object_id, from, owner);
            if redirect_attraction_to_command {
                // CR 717.6a: Cards redirected this way are kept in the command-zone
                // junkyard pile, separate from the Attraction deck.
                state
                    .objects
                    .get_mut(&object_id)
                    .expect("object exists")
                    .in_attraction_deck = false;
            }
            add_to_zone(state, object_id, to, owner);

            // CR 613.7d: An object receives a timestamp when it enters a zone.
            let entry_timestamp = (to == Zone::Battlefield).then(|| state.next_timestamp());
            let obj_mut = state.objects.get_mut(&object_id).expect("object exists");
            let pre_bump_incarnation = obj_mut.incarnation;
            obj_mut.zone = to;
            if to == Zone::Battlefield {
                obj_mut.reset_for_battlefield_entry(
                    state.turn_number,
                    entry_timestamp.expect("battlefield entry draws a timestamp"),
                );
                zone_change_record.entered_incarnation = Some(obj_mut.incarnation);
            } else if from != to {
                // CR 400.7: a move between zones creates a new object.
                obj_mut.bump_incarnation();
            }
            (pre_bump_incarnation, obj_mut.incarnation, false)
        } else {
            let resolved_zone_change = resolve_and_apply_zone_change(
                state,
                object_id,
                from,
                to,
                owner,
                zone_change_record,
            )
            .expect("ordinary zone transition must install its resolved core");
            zone_change_record = resolved_zone_change.zone_change_record;
            (
                resolved_zone_change.object.incarnation,
                resolved_zone_change.resulting_incarnation,
                true,
            )
        };

    // CR 603.6c: Drop the leaving permanent from the TriggerIndex. The
    // leaves-battlefield last-known-information scan in
    // `collect_pending_triggers` reads `state.objects` directly (the object's
    // zone is no longer Battlefield), unaffected by this removal. The
    // authoritative correctness path is the `evaluate_layers` rebuild
    // (CR 611.2e); this hook is incremental optimization between layer flushes.
    if from == Zone::Battlefield {
        state.trigger_index.remove(object_id);
    }

    if new_incarnation != pre_bump_incarnation {
        record_resolution_source_relatch(state, object_id, pre_bump_incarnation, new_incarnation);
    }

    // CR 700.11: a permanent card was put into its owner's graveyard.
    if to == Zone::Graveyard {
        record_descend_on_graveyard_arrival(state, object_id, owner);
    }

    // CR 400.7: A permanent that re-enters the battlefield is a new object with no
    // relation to its previous existence, so it sheds the "exiled with it"
    // associations of its prior incarnation. The battlefield-EXIT cleanup above
    // now preserves a source's `TrackedBySource` links when it leaves TO EXILE
    // (so a self-exiled source's pending linked return still finds its pile —
    // Mechtitan Core); this paired entry prune is the blink-back reset that drops
    // those stale links if that same source is later returned to the battlefield,
    // preventing `ExiledBySource` from reading a prior incarnation's pile. Scoped
    // to `TrackedBySource`: `CraftMaterial` links must survive re-entry (the craft
    // source returns transformed and must still read its materials).
    if to == Zone::Battlefield {
        state.exile_links.retain(|link| {
            link.source_id != object_id
                || !matches!(
                    link.kind,
                    crate::types::game_state::ExileLinkKind::TrackedBySource
                )
        });
    }

    let static_dependency_after =
        crate::game::layers::static_layer_dependency_for_zone_transition(state, from, to);

    // pod-lab loop-3 Q5: a plain Battlefield entry that doesn't originate
    // from Hand or Exile, and isn't itself the source of a live
    // zone-membership-dependent static (static_dependency_before/after),
    // can take the cheaper `mark_layers_entered` path instead of forcing a
    // full re-evaluation of every object's characteristics. This does NOT
    // skip re-verification: `prepare_incremental_flush` (layers.rs) re-runs
    // its own full Axis-1/Axis-2 safety analysis fresh from live state at
    // flush time regardless of which mark got set here, and escalates to a
    // full pass itself whenever that analysis can't prove the entering
    // object is safe (a sourced continuous effect, a CDA, counters,
    // attachments, or a population-perturbing static). This call only
    // proposes the cheap mark when the mutation site itself has nothing
    // else forcing a full re-evaluation; it is not the safety net.
    //
    // Hand and Exile are excluded UNCONDITIONALLY here, not merely folded
    // into static_dependency_before/after, because both have a proven blind
    // spot in that check:
    //   - CR 611.3a + CR 400.3: hand size affects continuous effects gated
    //     on the controller's hand (Carnage Interpreter, issue #3991), and
    //     `layers.rs`'s `quantity_ref_reads_zone` classifier maps
    //     `QuantityRef::HandSize` to a hardcoded `false` — a live
    //     HandSize-gated static is not detected as a zone dependency at all.
    //   - CR 613.1: characteristics set by "for each card exiled with/by
    //     [this]"-style statics (`QuantityRef::CardsExiledBySource`,
    //     `ExiledCardPower`, `TrackedSetSize`, `FilteredTrackedSetSize`,
    //     `TrackedSetAggregate` — e.g. Unlicensed Hearse, Veteran Survivor,
    //     Sutured Ghoul) have the identical blind spot: the same classifier
    //     maps all of them to `false`, and the count is live-filtered on
    //     `obj.zone == Zone::Exile` (see `linked_exile_for_context` /
    //     `players.rs`), so it changes the instant a linked card leaves
    //     Exile for the Battlefield. Neither axis has a Axis-2 analog in
    //     `prepare_incremental_flush` (which is exclusively board-population
    //     framed), so there is no flush-time safety net for either — the
    //     unconditional mark at this mutation site is these statics' ONLY
    //     protection, exactly as it is today.
    if to == Zone::Battlefield
        && from != Zone::Hand
        && from != Zone::Exile
        && !(static_dependency_before || static_dependency_after)
    {
        crate::game::layers::mark_layers_entered(state, object_id);
    } else if to == Zone::Battlefield
        || from == Zone::Battlefield
        || to == Zone::Hand
        || from == Zone::Hand
        || static_dependency_before
        || static_dependency_after
    {
        crate::game::layers::mark_layers_full(state);
    }

    // CR 401.5 + CR 611.3a: A move into or out of a library can change that
    // library's top card, flipping a continuous static gated on it (Vampire
    // Nocturnus: "as long as the top card of your library is black, ..."). The
    // hand/battlefield and graveyard marks above don't cover library moves (a
    // draw off the top, a mill into the graveyard, a put-on-top), so re-evaluate
    // — self-gated so routine library churn stays cheap when no such static is
    // live.
    if to == Zone::Library || from == Zone::Library {
        crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
    }

    // CR 702.145c + CR 702.145f: Daybound/Nightbound permanents entering under
    // the opposite day/night designation transform immediately. Runs after
    // battlefield-entry bookkeeping but before the ZoneChanged event is emitted
    // so the record reflects the face the object entered with. Skipped when
    // day/night is uninitialized.
    if to == Zone::Battlefield {
        if let Some(designation) = state.day_night {
            let needs_transform =
                state
                    .objects
                    .get(&object_id)
                    .is_some_and(|obj| match designation {
                        crate::types::game_state::DayNight::Night => {
                            obj.has_keyword(&crate::types::keywords::Keyword::Daybound)
                                && !obj.transformed
                        }
                        crate::types::game_state::DayNight::Day => {
                            obj.has_keyword(&crate::types::keywords::Keyword::Nightbound)
                                && obj.transformed
                        }
                    });
            if needs_transform {
                let _ = super::transform::transform_permanent(state, object_id, events);
            }
        }
    }

    // CR 603.6a: Register the post-reset trigger definitions in the index so
    // `state.clone()` consumers see a coherent battlefield → trigger candidate
    // map. AUTHORITATIVE PATH: the end-of-`evaluate_layers` rebuild
    // (CR 611.2e, `layers.rs`) is the safety net; this hook is incremental
    // optimization between layer flushes. `state.layers_dirty = true` was set
    // above, guaranteeing a post-layer rebuild on the next
    // `collect_pending_triggers` consult.
    if to == Zone::Battlefield {
        super::trigger_index::reindex_object_triggers(state, object_id);
    }

    if !transition_recorded {
        super::restrictions::record_zone_change(state, &mut zone_change_record);
    }

    if let Some(old_target) = unattached_from {
        events.push(GameEvent::Unattached {
            attachment_id: object_id,
            old_target,
        });
    }

    events.push(GameEvent::ZoneChanged {
        object_id,
        from: Some(from),
        to,
        record: Box::new(zone_change_record),
    });
}

/// CR 400.7 + CR 608.2i + CR 603.6a: record AND emit the battlefield entry of an object that came
/// into existence on the battlefield — a zone change with NO origin zone (`from: None`): a created
/// token (CR 111.1), a copy token (CR 707.2), an Incubator, or a conjured card. The `Some(from)`
/// counterpart is the emit at the end of `move_to_zone`.
///
/// Routes through [`crate::game::restrictions::record_zone_change`] — the single authority that
/// assigns this turn's zone-change index and performs the CR 608.2i battlefield-entry bookkeeping —
/// then writes the assigned index back onto the record it emits.
///
/// Callers must NOT also call `restrictions::record_battlefield_entry` (`record_zone_change` does
/// it; a second call double-counts `battlefield_entries_this_turn`) and must NOT also push onto
/// `state.zone_changes_this_turn` (that would write a duplicate CR 400.7 row).
///
/// WHY record and emit are ONE call: `GameObject::snapshot_for_zone_change` leaves
/// `turn_zone_change_index` at its `0` placeholder for the recorder to overwrite. The CR 603.2c
/// batched zone-change replay guard (`triggers.rs::batched_zone_change_already_collected`) dedups
/// on `(definition_ref, turn_zone_change_index)` read off the EVENT, and
/// `Ability::self_ref_own_departure_successor` (`types/ability.rs`) uses that same index as a
/// SUBSCRIPT into `state.zone_changes_this_turn`, then requires the row it lands on to carry the
/// same `trigger_source_context().identity.reference` as the event's own record. An entry that
/// emits without recording therefore ships index `0`, aliases onto occurrence `0`, and both
/// consumers read a row belonging to a different object. Splitting the two halves is what made
/// that defect writable at SIX call sites (measured on `4b34e5465`: `conjure.rs`, `counters.rs` x2,
/// `gift_delivery.rs`, `token_copy.rs` x2); fusing them removes the seam a seventh would be written
/// through.
///
/// Tripwired — not proved impossible — by
/// `crates/engine/tests/integration/battlefield_entry_authority_census.rs`, a source-text census
/// whose ceilings are documented in its own module header.
///
/// Returns the recorded row with its assigned index. `None` when the object is gone, in which case
/// NOTHING is recorded and NOTHING is emitted.
///
/// THE `None` ARM IS NOT A SILENT NO-OP AT EVERY CALLER, and an earlier revision of this paragraph
/// said it was — it named `gift_delivery.rs` and `token_copy.rs`, which are callers of
/// [`crate::game::effects::token::push_committed_token_entry_events`] ONE LEVEL UP, not of this
/// function. (That sentence is correct about ITS subject: of that emitter's eight callers, exactly
/// those two `.expect(…)` its return.) Measured over this function's four direct callers with
/// `rg -n 'record_and_emit_entry_from_no_zone\(' crates/engine/src`:
///
/// * `effects/conjure.rs:218` — `.expect("conjured object was just created")`: PANICS on `None`.
/// * `effects/incubate.rs:123` — `.expect("incubator token was just created")`: PANICS on `None`.
/// * `effects/token.rs:1881` — `if record.is_some()`, which is how
///   `push_committed_token_entry_events` gates its `GameEvent::TokenCreated` emit. This is the
///   object-existence predicate the token-creation ledger triple agrees on.
/// * `effects/counters.rs:530` — statement position, discards.
///
/// So `None` is inert on exactly ONE of the four routes. The two `.expect` callers keep their
/// pre-existing "just created" panic deliberately: each creates its object inside the same call, so
/// `None` there is an engine invariant violation rather than a reachable game state.
pub(crate) fn record_and_emit_entry_from_no_zone(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Option<crate::types::game_state::ZoneChangeRecord> {
    let mut record = state
        .objects
        .get(&object_id)
        .map(|obj| obj.snapshot_for_zone_change(object_id, None, Zone::Battlefield))?;
    super::restrictions::record_zone_change(state, &mut record);
    events.push(GameEvent::ZoneChanged {
        object_id,
        from: None,
        to: Zone::Battlefield,
        record: Box::new(record.clone()),
    });
    Some(record)
}

/// CR 601.2 + CR 733.1: Restore an object while reversing an incomplete action.
/// This intentionally uses the raw mover rather than the replacement-consulting
/// pipeline: an undone action does not apply replacement effects, but preserves
/// the prior raw move's event and ordering behavior.
pub(crate) fn restore_after_rollback(
    state: &mut GameState,
    object_id: ObjectId,
    to: Zone,
    events: &mut Vec<GameEvent>,
) {
    move_to_zone(state, object_id, to, events);
    // CR 601.2 + CR 733.1: reversing an incomplete action needs full
    // reconciliation regardless of which mark move_to_zone's own
    // axis-gated internal logic picked — an undone action is rare
    // (not gameplay-hot) and can leave board state in a shape the
    // entry-only incremental-flush safety classifier was never designed to
    // reason about, so there is no perf case for trusting it here. This is
    // conservatively at-or-above today's marking, not byte-for-byte
    // identical to it: some rollback transitions `move_to_zone` marks
    // nothing for today (e.g. Stack->Library) become `Full` here, which is
    // strictly safe, never a behavior change a test could observe as wrong.
    crate::game::layers::mark_layers_full(state);
}

/// CR 603.10a: Record that every member of `group` left the battlefield in the
/// SAME simultaneous event, so leaves-the-battlefield / dies observers that are
/// themselves in the group observe each other via last-known information (the
/// CR 603.10a worked example: a Blood Artist destroyed by the same Wrath of God
/// as the creatures it counts triggers once per co-dying creature).
///
/// Producers of a simultaneous departure batch — one board wipe (`DestroyAll`),
/// one state-based-action destruction pass (CR 704.7), one mass bounce/exile —
/// call this on the events they just produced, AFTER moving every member. This
/// is the authority for simultaneity: it is established here at the
/// event-production layer rather than inferred downstream from the shape of the
/// accumulated event vector, so sequential departures within a single
/// resolution are never grouped (a member only appears in another member's
/// `co_departed` when they truly left together).
pub fn mark_simultaneous_departures(events: &mut [GameEvent], group: &[ObjectId]) {
    if group.len() < 2 {
        return;
    }
    for event in events.iter_mut() {
        if let GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            record,
            ..
        } = event
        {
            if group.contains(object_id) {
                record.co_departed = group
                    .iter()
                    .copied()
                    .filter(|&member| member != *object_id)
                    .collect();
            }
        }
    }
}

/// CR 603.10a: Mirror a simultaneous-departure stamp into the authoritative
/// per-turn LKI records. A replacement-choice pause can split one logical
/// simultaneous action across two action-result event buffers; the prior
/// buffer is intentionally deferred until terminal completion, while these
/// records were already committed to `GameState` when each zone move occurred.
/// Updating the exact record indices retained by the batch preserves the same
/// co-departure fact for later look-back queries and trigger processing.
pub fn mark_simultaneous_departure_records(
    state: &mut GameState,
    record_indices: &[usize],
    group: &[ObjectId],
) {
    if group.len() < 2 {
        return;
    }
    for &index in record_indices {
        let Some(record) = state.zone_changes_this_turn.get_mut(index) else {
            continue;
        };
        if record.from_zone == Some(Zone::Battlefield) && group.contains(&record.object_id) {
            record.co_departed = group
                .iter()
                .copied()
                .filter(|&member| member != record.object_id)
                .collect();
        }
    }
}

/// CR 603.10a: Filter `ids` to those whose object has actually left the
/// battlefield (now resides in some other zone). Producers that accumulate a
/// candidate ID list — bounce, change-zone, sacrifice, destroy — pass that list
/// through this filter before `mark_simultaneous_departures` so that a member
/// which never actually departed (regenerated, sacrifice-prevented, bounce
/// guarded out) is excluded from every survivor's `co_departed` group.
pub fn departed_subset(state: &GameState, ids: &[ObjectId]) -> Vec<ObjectId> {
    ids.iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.zone != Zone::Battlefield)
        })
        .collect()
}

/// CR 603.10a: Stamp simultaneous departure on a slice of events produced by a
/// sweep that does not expose an explicit ID list (e.g. `sacrifice_unchosen`
/// internal loops). Collects every battlefield-origin `ZoneChanged` in `slice`
/// whose object is now off-battlefield, then groups them as co-departed.
pub fn stamp_simultaneous_from_slice(state: &GameState, slice: &mut [GameEvent]) {
    let departed: Vec<ObjectId> = slice
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                ..
            } if state
                .objects
                .get(object_id)
                .is_some_and(|o| o.zone != Zone::Battlefield) =>
            {
                Some(*object_id)
            }
            _ => None,
        })
        .collect();
    mark_simultaneous_departures(slice, &departed);
}

/// CR 406.6 + CR 607.2a (issue #6437): Snapshot `source_id`'s linked exiles at
/// the moment it leaves the battlefield, for a leaves-the-battlefield
/// trigger's later `ExiledBySource` lookup (`filter.rs`'s `trigger_source.
/// is_some()` branch). Every `ExileLinkKind` is kind-agnostically readable via
/// `ExiledBySource` (`HideawayLookable`'s and `CraftMaterial`'s own doc
/// comments say so explicitly) and the LIVE lookup
/// (`players::linked_exile_cards_for_source`) does not filter by kind either —
/// this snapshot must match that surface exactly, or a card whose "play the
/// exiled card" clause resolves via a TRIGGERED ability (Fight Rigging's
/// begin-of-combat trigger, as opposed to Windbrisk Heights' activated
/// ability) silently finds nothing: Hideaway's link is `HideawayLookable`, and
/// a `TrackedBySource`-only filter here dropped it before the previous fix.
pub(crate) fn capture_linked_exile_snapshot(
    state: &GameState,
    source_id: ObjectId,
    from: Zone,
) -> Vec<crate::types::game_state::LinkedExileSnapshot> {
    if from != Zone::Battlefield {
        return Vec::new();
    }

    state
        .exile_links
        .iter()
        .filter(|link| link.source_id == source_id)
        .filter_map(|link| {
            state.objects.get(&link.exiled_id).and_then(|obj| {
                (obj.zone == Zone::Exile).then(|| crate::types::game_state::LinkedExileSnapshot {
                    exiled_id: link.exiled_id,
                    owner: obj.owner,
                    // CR 202.3d + CR 709.4b: the exiled card is off the stack, so
                    // a split card records its combined mana value.
                    mana_value: obj.effective_mana_value(),
                })
            })
        })
        .collect()
}

/// After leave-time snapshots are captured on the zone-change record, sever
/// live attachment graph edges for a permanent departing the battlefield.
///
/// Attached Auras/Equipment that remain on the battlefield are cleaned up by
/// SBAs (CR 704.5m/704.5n). Hosts must not carry a stale `attachments` list
/// into other zones (commander zone return, blink, etc.), and attachments that
/// leave the battlefield must not keep a dangling `attached_to` pointer.
fn sever_battlefield_attachment_graph_on_exit(
    state: &mut GameState,
    object_id: ObjectId,
    unattached_from: &Option<crate::types::ability::TargetRef>,
) {
    if unattached_from.is_some() {
        if let Some(old_target_id) = state
            .objects
            .get(&object_id)
            .and_then(|o| o.attached_to)
            .and_then(|t| t.as_object())
        {
            if let Some(host) = state.objects.get_mut(&old_target_id) {
                host.attachments.retain(|&id| id != object_id);
            }
        }
        if let Some(attacher) = state.objects.get_mut(&object_id) {
            attacher.attached_to = None;
        }
        crate::game::layers::mark_layers_full(state);
    }

    if let Some(host) = state.objects.get_mut(&object_id) {
        if !host.attachments.is_empty() {
            host.attachments.clear();
            crate::game::layers::mark_layers_full(state);
        }
    }
}

pub(crate) fn capture_combat_status(
    state: &GameState,
    object_id: ObjectId,
) -> ZoneChangeCombatStatus {
    let Some(combat) = &state.combat else {
        return ZoneChangeCombatStatus::default();
    };
    let attacker = combat
        .attackers
        .iter()
        .find(|attacker| attacker.object_id == object_id);

    ZoneChangeCombatStatus {
        attacking: attacker.is_some(),
        blocking: combat.blocker_to_attacker.contains_key(&object_id),
        blocked: attacker.is_some_and(|attacker| attacker.blocked),
        // CR 506.5 + CR 603.10a: snapshot the sole-attacker / sole-blocker
        // status via the shared combat authority so it cannot diverge from the
        // live `FilterProp::AttackingAlone` / `BlockingAlone` evaluation.
        attacking_alone: crate::game::combat::attacking_alone(state, object_id),
        blocking_alone: crate::game::combat::blocking_alone(state, object_id),
        defending_player: attacker.map(|attacker| attacker.defending_player),
    }
}

/// Reorder objects that remain in one player's library without performing a
/// zone change. `ordered` is placed at `index` in the supplied order, or
/// appended when `index` is `None`.
pub(crate) fn reorder_within_library(
    state: &mut GameState,
    player: PlayerId,
    ordered: &[ObjectId],
    index: Option<usize>,
) {
    let player_state = state
        .players
        .iter_mut()
        .find(|candidate| candidate.id == player)
        .expect("player exists");
    player_state.library.retain(|id| !ordered.contains(id));
    let insert_index = index
        .unwrap_or(player_state.library.len())
        .min(player_state.library.len());
    for (offset, &object_id) in ordered.iter().enumerate() {
        player_state
            .library
            .insert(insert_index + offset, object_id);
    }
    state.advance_library_knowledge_epoch(player);

    // CR 401.5 + CR 611.3a: A library reorder can change its top card without
    // creating a ZoneChanged event, so invalidate the dependent static directly
    // (self-gated).
    crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
}

/// Move an object to a specific position in its owner's library (top or bottom), emitting a ZoneChanged event.
/// Convention: library[0] = top of library.
pub fn move_to_library_position(
    state: &mut GameState,
    object_id: ObjectId,
    top: bool,
    events: &mut Vec<GameEvent>,
) {
    let index = if top { Some(0) } else { None }; // None = push to end
    move_to_library_at_index(state, object_id, index, events);
}

/// Digital-only Alchemy placement (no CR entry): resolve a uniformly-random
/// 0-based insertion index for `LibraryPosition::RandomWithinTop { n }`. A card
/// slotted "into the top `top_n` cards of a library at random" lands among the
/// top `top_n` positions; with `slots_after_insert` total positions available
/// (the destination library's length *including* the card being placed), the
/// reachable range is `0..min(top_n, slots_after_insert)`. Consumes exactly one
/// RNG draw. Single authority for the random-top-N index so the conjure resolver
/// and the zone pipeline compute it identically.
pub(crate) fn random_top_slot_index(
    rng: &mut impl rand::Rng,
    top_n: usize,
    slots_after_insert: usize,
) -> usize {
    let upper = top_n.min(slots_after_insert).max(1);
    rng.random_range(0..upper)
}

/// Move an object to a specific index in its owner's library.
/// `index = Some(0)` = top, `index = None` = bottom, `index = Some(n)` = nth position.
/// Handles full cross-zone cleanup (LKI, transform revert, layer pruning, restrictions)
/// unlike ChangeZone { destination: Library } which shuffles the destination library.
pub fn move_to_library_at_index(
    state: &mut GameState,
    object_id: ObjectId,
    index: Option<usize>,
    events: &mut Vec<GameEvent>,
) {
    // CR 111.8: A token that has left the battlefield can't move to another zone.
    if state
        .objects
        .get(&object_id)
        .is_some_and(|obj| token_is_outside_battlefield_and_stack(state, obj))
    {
        return;
    }

    let obj = state.objects.get(&object_id).expect("object exists");
    let from = obj.zone;
    let owner = obj.owner;
    if from == Zone::Library {
        reorder_within_library(state, owner, &[object_id], index);
        return;
    }

    // CR 903.9a: A fresh zone change resets the "declined zone return" flag.
    state.commander_declined_zone_return.remove(&object_id);
    let unattached_from = state.objects.get(&object_id).and_then(|obj| {
        obj.attached_to
            .map(super::effects::attach::target_ref_from_attach_target)
    });
    let mut zone_change_record = obj.snapshot_for_zone_change(object_id, Some(from), Zone::Library);
    // CR 603.10a + CR 603.6e: Capture attachment snapshot before SBA can detach.
    zone_change_record.attachments = capture_attachment_snapshot(state, obj);
    zone_change_record.combat_status = capture_combat_status(state, object_id);
    zone_change_record.sync_trigger_source_exiled_cards(
        state
            .cards_exiled_with_source_this_turn
            .get(&object_id)
            .cloned()
            .unwrap_or_default(),
    );
    zone_change_record.sync_trigger_source_context();

    sever_battlefield_attachment_graph_on_exit(state, object_id, &unattached_from);

    // CR 608.2h: hand the LKI the PRE-SEVER attachment set captured above.
    apply_zone_exit_cleanup(
        state,
        object_id,
        from,
        Zone::Library,
        zone_change_record.attachments.clone(),
    );

    remove_from_zone(state, object_id, from, owner);

    // CR 603.6c: Drop the leaving permanent from the TriggerIndex when this
    // path is used to move a battlefield permanent into the library
    // (Conduit-of-Worlds-style "shuffle a permanent into your library").
    if from == Zone::Battlefield {
        state.trigger_index.remove(object_id);
    }

    // Place at specified index or push to end (bottom)
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == owner)
        .expect("owner exists");
    match index {
        Some(i) => {
            let clamped = i.min(player.library.len());
            player.library.insert(clamped, object_id);
        }
        None => player.library.push_back(object_id),
    }
    state.advance_library_knowledge_epoch(owner);

    let mut bump: Option<(u64, u64)> = None;
    if let Some(obj_mut) = state.objects.get_mut(&object_id) {
        let pre_bump_incarnation = obj_mut.incarnation;
        obj_mut.zone = Zone::Library;
        // CR 400.7: a move INTO the library from any other zone makes a new object.
        // A within-Library reposition (reveal / scry bottom placement / look-at-top-N,
        // CR 701.20b) is zero moves — `from == Library` here — and must NOT bump.
        if from != Zone::Library {
            obj_mut.bump_incarnation();
            bump = Some((pre_bump_incarnation, obj_mut.incarnation));
        }
    }
    if let Some((pre, new)) = bump {
        record_resolution_source_relatch(state, object_id, pre, new);
    }

    super::restrictions::record_zone_change(state, &mut zone_change_record);

    if let Some(old_target) = unattached_from {
        events.push(GameEvent::Unattached {
            attachment_id: object_id,
            old_target,
        });
    }

    events.push(GameEvent::ZoneChanged {
        object_id,
        from: Some(from),
        to: Zone::Library,
        record: Box::new(zone_change_record),
    });

    // CR 401.5 + CR 611.3a: placing a card at library index 0 changes the top
    // card, so a `TopOfLibraryMatches` static must be re-evaluated. This path
    // bypasses `move_to_zone`, so it invalidates directly (self-gated).
    crate::game::layers::mark_layers_full_if_top_of_library_static_live(state);
}

/// Remove an ObjectId from the appropriate zone collection (CR 400.1).
pub fn remove_from_zone(state: &mut GameState, object_id: ObjectId, zone: Zone, owner: PlayerId) {
    match zone {
        Zone::Library | Zone::Hand | Zone::Graveyard => {
            let player = state
                .players
                .iter_mut()
                .find(|p| p.id == owner)
                .expect("owner exists");
            match zone {
                Zone::Library => player.library.retain(|id| *id != object_id),
                Zone::Hand => player.hand.retain(|id| *id != object_id),
                Zone::Graveyard => player.graveyard.retain(|id| *id != object_id),
                _ => unreachable!(),
            }
        }
        Zone::Battlefield => state.battlefield.retain(|id| *id != object_id),
        Zone::Stack => {
            // A unique id, so at most ONE entry matches. Routed through the
            // shared stack-removal authority, which journals it and drops BOTH
            // per-entry side tables (this arm previously dropped only
            // `stack_paid_facts`). A miss is normal: the resolution pop already
            // removed the entry before the card is routed to its next zone.
            if let Some(idx) = state.stack.iter().position(|e| e.id == object_id) {
                crate::game::stack::remove_nonresolving_stack_entry_at(
                    state,
                    idx,
                    crate::game::lifecycle::DelayedTerminalDisposition::Removed,
                )
                .expect("position yielded a live stack index");
            }
        }
        Zone::Exile => state.exile.retain(|id| *id != object_id),
        Zone::Command => {
            if state
                .objects
                .get(&object_id)
                .is_some_and(|obj| obj.in_attraction_deck)
            {
                state
                    .players
                    .iter_mut()
                    .find(|p| p.id == owner)
                    .expect("owner exists")
                    .attraction_deck
                    .retain(|id| *id != object_id);
            } else if state
                .objects
                .get(&object_id)
                .is_some_and(|obj| obj.in_contraption_deck)
            {
                state
                    .players
                    .iter_mut()
                    .find(|p| p.id == owner)
                    .expect("owner exists")
                    .contraption_deck
                    .retain(|id| *id != object_id);
            } else {
                state.command_zone.retain(|id| *id != object_id);
            }
        }
    }
}

/// CR 704.5d + CR 704.5e: Remove a token or copy that ceases to exist.
/// This is not a zone change and deliberately emits no event.
pub(crate) fn cease_object(
    state: &mut GameState,
    object_id: ObjectId,
    zone: Zone,
    owner: PlayerId,
) {
    // CR 733: capture the occurrence BEFORE the removal — after it there is no
    // object left to reference. A caller that passes an already-absent object
    // keeps the prior silent behavior and journals nothing.
    let Some(object) = state.objects.get(&object_id) else {
        remove_from_zone(state, object_id, zone, owner);
        return;
    };
    let command = ResolvedObjectCeaseCommand {
        object: ObjectIncarnationRef::from_object(object),
        expected_zone: zone,
        owner,
        cause: state.current_or_begin_rules_execution_node(),
    };
    apply_resolved_object_cease(state, &command)
        .expect("the freshly read object must satisfy its own cease precondition");
    state
        .resolved_rules_journal
        .record_object_cease(command)
        .expect("resolved cease-to-exist must have a live journal cause");
}

/// Installs one already-resolved CR 704.5d cease-to-exist removal verbatim.
///
/// Deliberately re-runs none of the CR 704.5d/e eligibility scan: whether this
/// object was a token outside the battlefield was settled by the SBA sweep that
/// recorded the command.
pub fn apply_resolved_object_cease(
    state: &mut GameState,
    command: &ResolvedObjectCeaseCommand,
) -> Result<(), ResolvedObjectCeaseReplayInvariantError> {
    let object_id = command.object.object_id;
    let object = state.objects.get(&object_id).ok_or(
        ResolvedObjectCeaseReplayInvariantError::UnknownObject(object_id),
    )?;
    let found = ObjectIncarnationRef::from_object(object);
    if found != command.object {
        return Err(ResolvedObjectCeaseReplayInvariantError::StaleObject {
            expected: command.object,
            found,
        });
    }
    if object.zone != command.expected_zone {
        return Err(ResolvedObjectCeaseReplayInvariantError::ZoneMismatch {
            expected: command.expected_zone,
            found: object.zone,
        });
    }
    if object.owner != command.owner {
        return Err(ResolvedObjectCeaseReplayInvariantError::OwnerMismatch {
            expected: command.owner,
            found: object.owner,
        });
    }

    remove_from_zone(state, object_id, command.expected_zone, command.owner);
    state.objects.remove(&object_id);
    Ok(())
}

/// Add an ObjectId to the appropriate zone collection.
pub fn add_to_zone(state: &mut GameState, object_id: ObjectId, zone: Zone, owner: PlayerId) {
    match zone {
        Zone::Library | Zone::Hand | Zone::Graveyard => {
            let player = state
                .players
                .iter_mut()
                .find(|p| p.id == owner)
                .expect("owner exists");
            match zone {
                Zone::Library => player.library.push_back(object_id),
                Zone::Hand => player.hand.push_back(object_id),
                Zone::Graveyard => player.graveyard.push_back(object_id),
                _ => unreachable!(),
            }
        }
        // CR 400.4a: Instants/sorceries blocked by early check in move_to_zone.
        Zone::Battlefield => state.battlefield.push_back(object_id),
        Zone::Stack => {} // Stack entries are managed separately via StackEntry
        Zone::Exile => state.exile.push_back(object_id),
        Zone::Command => {
            if state
                .objects
                .get(&object_id)
                .is_some_and(|obj| obj.in_attraction_deck)
            {
                state
                    .players
                    .iter_mut()
                    .find(|p| p.id == owner)
                    .expect("owner exists")
                    .attraction_deck
                    .push_back(object_id);
            } else if state
                .objects
                .get(&object_id)
                .is_some_and(|obj| obj.in_contraption_deck)
            {
                state
                    .players
                    .iter_mut()
                    .find(|p| p.id == owner)
                    .expect("owner exists")
                    .contraption_deck
                    .push_back(object_id);
            } else {
                state.command_zone.push_back(object_id);
            }
        }
    }
}

/// Absorb a component into a battlefield survivor without creating an
/// independent zone-change event. `from` is `None` when the component's prior
/// zone membership was already consumed (for example, by stack resolution).
/// Callers that require zone-exit cleanup perform it before absorption.
pub(crate) fn absorb_component(state: &mut GameState, component_id: ObjectId, from: Option<Zone>) {
    let owner = state.objects.get(&component_id).map(|obj| obj.owner);
    if let (Some(from), Some(owner)) = (from, owner) {
        remove_from_zone(state, component_id, from, owner);
    }
    if let Some(component) = state.objects.get_mut(&component_id) {
        component.zone = Zone::Battlefield;
    }
}

/// CR 730.3: Route an absorbed merge component to its owner's destination as
/// a new object, without representing it as an independent battlefield exit.
/// The caller snapshots the component and emits its `ZoneChanged { from: None
/// }` event around this delivery.
pub(crate) fn route_component(state: &mut GameState, component_id: ObjectId, to: Zone) {
    let Some(owner) = state.objects.get(&component_id).map(|obj| obj.owner) else {
        return;
    };

    // CR 608.2h: no sever has run on this path, so the live attachment list is
    // still intact when this component becomes a new object.
    let attachments = state
        .objects
        .get(&component_id)
        .map(|obj| capture_attachment_snapshot(state, obj))
        .unwrap_or_default();
    apply_zone_exit_cleanup(state, component_id, Zone::Battlefield, to, attachments);
    // CR 730.2: the component is absorbed into the survivor and is not an
    // independent member of the battlefield list; defensively ensure it is not
    // left there (a no-op under the runtime invariant) before adding it to its
    // OWN owner's destination zone.
    remove_from_zone(state, component_id, Zone::Battlefield, owner);
    add_to_zone(state, component_id, to, owner);
    if let Some(component) = state.objects.get_mut(&component_id) {
        component.zone = to;
        // CR 730.3 + CR 400.7: the component becomes a new object in its
        // owner's destination zone. Keep this beside the raw delivery so
        // `apply_zone_exit_cleanup` cannot double-bump normal moves.
        component.bump_incarnation();
    }
    // CR 700.11: a nontoken permanent card put into its owner's graveyard from
    // anywhere counts as having descended this turn — shared single authority
    // with `move_to_zone`.
    if to == Zone::Graveyard {
        record_descend_on_graveyard_arrival(state, component_id, owner);
    }
}

/// CR 110.2a + CR 603.6a: Apply an "under your control" battlefield-entry
/// controller override to both the live object and the zone-change snapshots
/// created for this entry.
pub(crate) fn apply_battlefield_entry_controller_override(
    state: &mut GameState,
    events: &mut [GameEvent],
    object_id: ObjectId,
    controller: PlayerId,
) {
    // Read the pre-override identity and controllers once: they are the CR 733
    // command's occurrence reference and preconditions. An absent object still
    // retags the snapshots below, exactly as before, and simply journals nothing.
    let object_snapshot = state.objects.get(&object_id);
    let reference = object_snapshot.map(ObjectIncarnationRef::from_object);
    let expected_old_base_controller = object_snapshot.and_then(|obj| obj.base_controller);
    let expected_old_controller = object_snapshot.map(|obj| obj.controller);

    // Resolve the snapshot POSITIONS rather than mutating through a scan: the
    // position is what the CR 733 command records, so replay retags the same
    // record instead of re-running a last-match scan (CR 400.7 permits the same
    // object to hold several entries in one turn).
    let zone_change_index = state
        .zone_changes_this_turn
        .iter()
        .rposition(|record| record.object_id == object_id && record.to_zone == Zone::Battlefield);
    let battlefield_entry_index = state
        .battlefield_entries_this_turn
        .iter()
        .rposition(|record| record.object_id == object_id);

    // CR 733: the retag itself is performed by the command applier, so resolve and
    // replay install through one body instead of two copies that can drift. An
    // absent object has nothing to retag on the object side but still retags its
    // snapshots, exactly as before.
    let command = reference.zip(expected_old_controller).map(|(object, old)| {
        ResolvedControllerOverrideCommand {
            object,
            expected_old_base_controller,
            expected_old_controller: old,
            resulting_controller: controller,
            zone_change_index,
            battlefield_entry_index,
            cause: state.current_or_begin_rules_execution_node(),
        }
    });
    match &command {
        Some(command) => apply_resolved_controller_override(state, command)
            .expect("the freshly read object must satisfy its own override precondition"),
        None => retag_battlefield_entry_snapshots(
            state,
            zone_change_index,
            battlefield_entry_index,
            controller,
        ),
    }

    if let Some(GameEvent::ZoneChanged { record, .. }) = events.iter_mut().rev().find(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id: id,
                to: Zone::Battlefield,
                ..
            } if *id == object_id
        )
    }) {
        record.controller = controller;
        record.sync_trigger_source_context();
    }

    // CR 733: journal the settled override. The event fix-up above is deliberately
    // NOT part of the command — events are transient carriers consumed by the same
    // resolution, not persistent state a replay reconstructs.
    // CR 110.2a: an override onto the controller the object already had, with the
    // base controller already pinned there, retagged nothing and is not recorded.
    let Some(command) = command else {
        return;
    };
    if command.expected_old_base_controller == Some(controller)
        && command.expected_old_controller == controller
    {
        return;
    }
    state
        .resolved_rules_journal
        .record_controller_override(command)
        .expect("resolved controller override must have a live journal cause");
}

/// Retags the CR 400.7 zone-change and CR 608.2i battlefield-entry snapshots at
/// the exact recorded positions. Shared by the resolve-time authority and the
/// replay applier so both install the same retag.
///
/// CR 608.2i, not CR 403.3: `battlefield_entries_this_turn` is an entry-time
/// characteristics snapshot kept so later effects can look back at a previous
/// game state. CR 403.3 ("Permanents exist only on the battlefield") is
/// definitional and describes no such record.
fn retag_battlefield_entry_snapshots(
    state: &mut GameState,
    zone_change_index: Option<usize>,
    battlefield_entry_index: Option<usize>,
    controller: PlayerId,
) {
    if let Some(record) =
        zone_change_index.and_then(|index| state.zone_changes_this_turn.get_mut(index))
    {
        record.controller = controller;
        record.sync_trigger_source_context();
    }
    if let Some(record) =
        battlefield_entry_index.and_then(|index| state.battlefield_entries_this_turn.get_mut(index))
    {
        record.controller = controller;
    }
}

/// Installs one already-resolved CR 110.2a controller override verbatim.
///
/// Deliberately re-runs none of the entry-time decision that produced the
/// override: whether the permanent enters under another player's control was
/// settled when the command was recorded. The applier verifies the state it is
/// installing into, then retags the object and the exact snapshots the authority
/// retagged.
pub fn apply_resolved_controller_override(
    state: &mut GameState,
    command: &ResolvedControllerOverrideCommand,
) -> Result<(), ResolvedControllerOverrideReplayInvariantError> {
    let object_id = command.object.object_id;
    let object = state
        .objects
        .get(&object_id)
        .ok_or(ResolvedControllerOverrideReplayInvariantError::UnknownObject(object_id))?;
    let found = ObjectIncarnationRef::from_object(object);
    if found != command.object {
        return Err(
            ResolvedControllerOverrideReplayInvariantError::StaleObject {
                expected: command.object,
                found,
            },
        );
    }
    if object.base_controller != command.expected_old_base_controller {
        return Err(
            ResolvedControllerOverrideReplayInvariantError::BaseControllerPreconditionMismatch {
                expected: command.expected_old_base_controller,
                found: object.base_controller,
            },
        );
    }
    if object.controller != command.expected_old_controller {
        return Err(
            ResolvedControllerOverrideReplayInvariantError::ControllerPreconditionMismatch {
                expected: command.expected_old_controller,
                found: object.controller,
            },
        );
    }
    // Both recorded snapshot positions are checked before any mutation so a
    // rejected command leaves no partial retag.
    if let Some(index) = command.zone_change_index {
        if index >= state.zone_changes_this_turn.len() {
            return Err(
                ResolvedControllerOverrideReplayInvariantError::MissingZoneChangeRecord(index),
            );
        }
    }
    if let Some(index) = command.battlefield_entry_index {
        if index >= state.battlefield_entries_this_turn.len() {
            return Err(
                ResolvedControllerOverrideReplayInvariantError::MissingBattlefieldEntryRecord(
                    index,
                ),
            );
        }
    }

    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.base_controller = Some(command.resulting_controller);
        obj.controller = command.resulting_controller;
    }
    retag_battlefield_entry_snapshots(
        state,
        command.zone_change_index,
        command.battlefield_entry_index,
        command.resulting_controller,
    );
    Ok(())
}

/// CR 603.6a: Stamps the entering permanent with the ability that put it onto
/// the battlefield, so anti-recursion intervening-ifs ("if it wasn't put onto
/// the battlefield with this ability") can exclude the permanents that very
/// ability placed.
///
/// This is the single authority for the stamp: the delivery tail wrote the field
/// raw, leaving a CR 733 replay with no record that the permanent's entry was
/// ability-driven.
pub(crate) fn stamp_battlefield_entry_provenance(
    state: &mut GameState,
    object_id: ObjectId,
    source_id: ObjectId,
) {
    let Some(object) = state.objects.get(&object_id) else {
        return;
    };
    let reference = ObjectIncarnationRef::from_object(object);
    let expected_old_source = object.entered_via_ability_source;
    // CR 603.6a: re-stamping the source already recorded changes nothing.
    if expected_old_source == Some(source_id) {
        return;
    }

    let command = ResolvedEntryProvenanceCommand {
        object: reference,
        expected_old_source,
        resulting_source: source_id,
        cause: state.current_or_begin_rules_execution_node(),
    };
    apply_resolved_entry_provenance(state, &command)
        .expect("the freshly read object must satisfy its own provenance precondition");
    state
        .resolved_rules_journal
        .record_entry_provenance(command)
        .expect("resolved entry provenance must have a live journal cause");
}

/// Installs one already-resolved CR 603.6a provenance stamp verbatim.
pub fn apply_resolved_entry_provenance(
    state: &mut GameState,
    command: &ResolvedEntryProvenanceCommand,
) -> Result<(), ResolvedEntryProvenanceReplayInvariantError> {
    let object_id = command.object.object_id;
    let object = state.objects.get(&object_id).ok_or(
        ResolvedEntryProvenanceReplayInvariantError::UnknownObject(object_id),
    )?;
    let found = ObjectIncarnationRef::from_object(object);
    if found != command.object {
        return Err(ResolvedEntryProvenanceReplayInvariantError::StaleObject {
            expected: command.object,
            found,
        });
    }
    if object.entered_via_ability_source != command.expected_old_source {
        return Err(
            ResolvedEntryProvenanceReplayInvariantError::SourcePreconditionMismatch {
                expected: command.expected_old_source,
                found: object.entered_via_ability_source,
            },
        );
    }
    state
        .objects
        .get_mut(&object_id)
        .expect("the validated object must remain present")
        .entered_via_ability_source = Some(command.resulting_source);
    Ok(())
}

/// CR 614.1d: Check if any active CantEnterBattlefieldFrom static prevents this
/// object from entering the battlefield from its current zone.
/// e.g., Grafdigger's Cage: "Creature cards in graveyards and libraries can't enter the battlefield."
fn is_blocked_from_entering_battlefield(state: &GameState, obj: &GameObject) -> bool {
    let object_id = obj.id;
    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the phased-out /
    // command-zone / condition gate so Grafdigger's Cage phased out no longer
    // blocks ETB from graveyard/library.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        if def.mode != StaticMode::CantEnterBattlefieldFrom {
            continue;
        }
        // The affected filter encodes both card type and zone restrictions
        // (e.g., Creature + InAnyZone[Graveyard, Library]).
        if let Some(ref filter) = def.affected {
            if super::filter::matches_target_filter(
                state,
                object_id,
                filter,
                &super::filter::FilterContext::from_source(state, bf_obj.id),
            ) {
                return true;
            }
        }
    }

    // CR 611.2a + CR 614.1d: floating turn-scoped "cards can't enter the
    // battlefield from <zone>" restrictions (Bad Wolf Bay's chaos ability) block
    // entry the same way as the permanent CantEnterBattlefieldFrom static. The
    // object is still in its origin zone here, so the filter's `InAnyZone` prop
    // matches `obj.zone`.
    for restriction in &state.restrictions {
        if let crate::types::ability::GameRestriction::CantEnterBattlefieldFrom {
            filter,
            source,
            ..
        } = restriction
        {
            if super::filter::matches_target_filter(
                state,
                object_id,
                filter,
                &super::filter::FilterContext::from_source(state, *source),
            ) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, FilterProp, StaticDefinition, TargetFilter,
        TypeFilter, TypedFilter,
    };
    use crate::types::game_state::GameState;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn create_object_assigns_id_and_inserts() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        assert_eq!(id, ObjectId(1));
        assert!(state.objects.contains_key(&id));
        assert_eq!(state.objects[&id].name, "Forest");
        assert_eq!(state.objects[&id].zone, Zone::Hand);
        assert_eq!(state.next_object_id, 2);
    }

    #[test]
    fn create_object_adds_to_player_hand() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        assert!(state.players[0].hand.contains(&id));
    }

    #[test]
    fn hand_to_stack_marks_layers_dirty_for_hand_size_statics() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Hand,
        );
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Stack, &mut events);

        assert_eq!(state.objects[&id].zone, Zone::Stack);
        assert!(
            matches!(
                state.layers_dirty,
                crate::types::game_state::LayersDirty::Full
            ),
            "hand-to-stack movement must mark layers dirty so hand-size-gated statics re-evaluate"
        );
    }

    #[test]
    fn hand_to_stack_with_hand_zone_static_dirties_layers() {
        let mut state = setup();
        let grant_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Instant)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Miracle(ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                }),
            }]);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "HandGrantSource".to_string(),
            Zone::Battlefield,
        );
        {
            let src = state.objects.get_mut(&source).unwrap();
            src.static_definitions.push(grant_static.clone());
            src.base_static_definitions = Arc::new(vec![grant_static]);
        }
        let id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Hand,
        );
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Stack, &mut events);

        assert_eq!(state.objects[&id].zone, Zone::Stack);
        assert!(
            state.layers_dirty.is_dirty(),
            "hand-zone continuous effects must re-evaluate when a hand card departs"
        );
    }

    /// CR 404 + CR 611.3a: PRODUCTION-PATH proof for the graveyard-gated static
    /// invalidation seam (issue #4774). A flying creature card milled from the
    /// library into a graveyard via the normal `move_to_zone` path — with NO
    /// manual `mark_layers_full` — must dirty layers so Cairn Wanderer's "~ has
    /// flying as long as a creature card with flying is in a graveyard"
    /// re-evaluates and the grant applies. Library→Graveyard deliberately avoids
    /// the pre-existing hand/battlefield invalidation, so this exercises the new
    /// graveyard seam specifically; it fails on revert of that seam.
    #[test]
    fn graveyard_arrival_reevaluates_graveyard_gated_static_via_zone_move() {
        let cairn_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }])
            .condition(crate::types::ability::StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature).properties(vec![
                        FilterProp::WithKeyword {
                            value: Keyword::Flying,
                        },
                        FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                    ]),
                )),
            });

        let mut state = setup();
        let cairn = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cairn Wanderer".to_string(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&cairn).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
            o.static_definitions.push(cairn_static.clone());
            o.base_static_definitions = Arc::new(vec![cairn_static]);
        }

        // A flying creature card in the library (to be milled into the graveyard).
        let flyer = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Storm Crow".to_string(),
            Zone::Library,
        );
        {
            let o = state.objects.get_mut(&flyer).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
            o.base_keywords.push(Keyword::Flying);
            o.keywords.push(Keyword::Flying);
        }

        // Baseline: empty graveyard → Cairn has no Flying; layers clean after eval.
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::evaluate_layers(&mut state);
        assert!(!state.objects[&cairn].has_keyword(&Keyword::Flying));
        assert!(!state.layers_dirty.is_dirty());

        // PRODUCTION PATH: mill the flyer Library → Graveyard (no manual mark_full).
        let mut events = Vec::new();
        move_to_zone(&mut state, flyer, Zone::Graveyard, &mut events);

        assert!(
            state.layers_dirty.is_dirty(),
            "a flying creature card entering a graveyard must dirty layers for Cairn's graveyard-gated static"
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert!(
            state.objects[&cairn].has_keyword(&Keyword::Flying),
            "after the flyer reaches the graveyard via move_to_zone, Cairn gains Flying without a manual mark_full"
        );
    }

    /// CR 404 + CR 611.3a: the graveyard invalidation is SCOPED — with no active
    /// graveyard-membership-gated static, a card entering a graveyard must NOT
    /// dirty layers, so routine graveyard churn (deaths, mill, discard) stays
    /// cheap and this is not a blanket per-graveyard-move full re-eval.
    #[test]
    fn graveyard_arrival_does_not_dirty_layers_without_graveyard_gated_static() {
        let mut state = setup();
        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&bear).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
        }
        let card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Storm Crow".to_string(),
            Zone::Library,
        );
        {
            let o = state.objects.get_mut(&card).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
        }

        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::evaluate_layers(&mut state);
        assert!(!state.layers_dirty.is_dirty());

        let mut events = Vec::new();
        move_to_zone(&mut state, card, Zone::Graveyard, &mut events);
        assert!(
            !state.layers_dirty.is_dirty(),
            "graveyard arrival must NOT dirty layers when no graveyard-membership-gated static is active"
        );
    }

    /// CR 404 + CR 611.3a: PRODUCTION-PATH proof that the graveyard invalidation
    /// also covers COUNT/threshold gates, not just `IsPresent`. A static gated on
    /// `QuantityComparison(GraveyardSize >= 1)` must re-evaluate when a card is
    /// milled into the graveyard via the normal `move_to_zone` path (no manual
    /// `mark_full`). Fails on revert of the `QuantityComparison`/`QuantityRef`
    /// branch of the zone-read detector.
    #[test]
    fn graveyard_count_gated_static_reevaluates_via_zone_move() {
        let count_static = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }])
            .condition(crate::types::ability::StaticCondition::QuantityComparison {
                lhs: crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::GraveyardSize {
                        player: crate::types::ability::PlayerScope::Controller,
                    },
                },
                comparator: crate::types::ability::Comparator::GE,
                rhs: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            });

        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Graveyard-count source".to_string(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&source).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
            o.static_definitions.push(count_static.clone());
            o.base_static_definitions = Arc::new(vec![count_static]);
        }
        let card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Milled card".to_string(),
            Zone::Library,
        );
        {
            let o = state.objects.get_mut(&card).unwrap();
            o.card_types.core_types.push(CoreType::Creature);
            o.base_card_types = o.card_types.clone();
        }

        // Baseline: empty graveyard → count gate unsatisfied → no Trample.
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::evaluate_layers(&mut state);
        assert!(!state.objects[&source].has_keyword(&Keyword::Trample));
        assert!(!state.layers_dirty.is_dirty());

        // Production path: mill a card Library → Graveyard (no manual mark_full).
        let mut events = Vec::new();
        move_to_zone(&mut state, card, Zone::Graveyard, &mut events);
        assert!(
            state.layers_dirty.is_dirty(),
            "a card entering a graveyard must dirty layers for a GraveyardSize-count-gated static"
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert!(
            state.objects[&source].has_keyword(&Keyword::Trample),
            "with one card in the graveyard the count gate is satisfied and the grant applies"
        );
    }

    #[test]
    fn create_object_adds_to_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );
        assert!(state.battlefield.contains(&id));
    }

    /// CR 111.8: A token that has left the battlefield can't move to another zone
    /// or come back onto the battlefield; it remains in its current zone and
    /// ceases to exist at the next SBA (CR 111.7). A single-resolution flicker
    /// ("exile target permanent, then return it") on a token therefore must NOT
    /// bring it back — modeled here as the two zone changes such an effect makes,
    /// battlefield -> exile then exile -> battlefield, with no SBA in between.
    #[test]
    fn token_that_left_battlefield_cannot_return() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cat".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().is_token = true;

        let mut events = Vec::new();
        // Flicker step 1: the token leaves the battlefield (exiled).
        move_to_zone(&mut state, id, Zone::Exile, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Exile);

        // Flicker step 2 (same resolution, no SBA between): attempt to return it.
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        // CR 111.8: it stays in exile; it must not re-enter the battlefield.
        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "CR 111.8: a token that left the battlefield can't return"
        );
        assert!(
            !state.battlefield.contains(&id),
            "returned token must not be on the battlefield"
        );
    }

    /// CR 111.8: A token that has left the battlefield can't move into a
    /// library before the next SBA removes it.
    #[test]
    fn token_that_left_battlefield_cannot_move_to_library_position() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Cat".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().is_token = true;

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Exile, &mut events);
        move_to_library_position(&mut state, id, true, &mut events);

        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "CR 111.8: a token that left the battlefield can't move into a library"
        );
        assert!(
            !state.players[0].library.contains(&id),
            "token must not be inserted into its owner's library"
        );
    }

    #[test]
    fn create_object_increments_id() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Hand,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Hand,
        );
        assert_eq!(id1, ObjectId(1));
        assert_eq!(id2, ObjectId(2));
    }

    #[test]
    fn move_hand_to_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        assert!(!state.players[0].hand.contains(&id));
        assert!(state.battlefield.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert_eq!(events.len(), 1);
        match &events[0] {
            GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                record,
            } => {
                assert_eq!(*object_id, id);
                assert_eq!(*from, Some(Zone::Hand));
                assert_eq!(*to, Zone::Battlefield);
                assert_eq!(record.object_id, id);
                assert_eq!(record.from_zone, Some(Zone::Hand));
                assert_eq!(record.to_zone, Zone::Battlefield);
            }
            _ => panic!("expected ZoneChanged event"),
        }
    }

    /// CR 603.2g + CR 603.6a: a no-op Battlefield → Battlefield move does not
    /// create a zone-change event, so ETB triggers have no event to observe.
    #[test]
    fn move_battlefield_to_battlefield_is_no_op() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coiling Oracle".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        assert!(state.battlefield.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert!(
            events.is_empty(),
            "same-zone battlefield move must not emit ZoneChanged events"
        );
    }

    #[test]
    fn move_library_to_hand() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Hand, &mut events);

        assert!(!state.players[0].library.contains(&id));
        assert!(state.players[0].hand.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Hand);
    }

    /// CR 122.2 + CR 400.7: Counters cease to exist when an object changes
    /// zones. The Personify class ("Exile target creature you control, then
    /// return that card to the battlefield under its owner's control") moves
    /// the creature Battlefield → Exile → Battlefield. ObjectId is storage
    /// identity in this engine (the same slot is reused), so unless the
    /// exit-cleanup hook actually clears `obj.counters` at the boundary, the
    /// returning permanent will retain its pre-exile counters — which the
    /// rules say cease to exist. This test drives `move_to_zone` directly
    /// (not a shape assertion on the HashMap) and would have caught a
    /// regression in `apply_zone_exit_cleanup`'s counter-clear branch.
    #[test]
    fn issue_4223_combat_role_cleared_on_battlefield_exit() {
        use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};

        let mut state = setup();
        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Strangleroot Geist".to_string(),
            Zone::Battlefield,
        );
        let blocker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Blocker".to_string(),
            Zone::Battlefield,
        );

        let mut combat = CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: true,
                band_id: None,
            }],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);

        let mut events = Vec::new();
        // Blocker dies (e.g. combat damage) — must leave combat before Undying
        // returns the same ObjectId to the battlefield.
        move_to_zone(&mut state, blocker, Zone::Graveyard, &mut events);
        let combat = state.combat.as_ref().unwrap();
        assert!(
            !combat.blocker_to_attacker.contains_key(&blocker),
            "CR 506.4: blocker must be removed from combat when it leaves the battlefield"
        );
        assert!(
            combat
                .blocker_assignments
                .get(&attacker)
                .is_none_or(|blockers| !blockers.contains(&blocker)),
            "dead blocker must not remain assigned to the attacker"
        );

        // Undying-style return: same ObjectId re-enters without combat role.
        move_to_zone(&mut state, blocker, Zone::Battlefield, &mut events);
        let combat = state.combat.as_ref().unwrap();
        assert!(
            !combat.blocker_to_attacker.contains_key(&blocker),
            "returned creature must not inherit stale blocking status (issue #4223)"
        );

        // Attacker dies and returns — must not remain an attacker either.
        move_to_zone(&mut state, attacker, Zone::Graveyard, &mut events);
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat
                .attackers
                .iter()
                .all(|info| info.object_id != attacker),
            "CR 506.4: attacker must be removed from combat when it leaves the battlefield"
        );
        assert!(
            !combat.blocker_assignments.contains_key(&attacker),
            "CR 506.4: attacker-keyed block assignment must be removed on battlefield exit"
        );
        assert!(
            combat
                .blocker_to_attacker
                .values()
                .all(|attackers| !attackers.contains(&attacker)),
            "CR 506.4: departed attacker must be pruned from every blocker's reverse lookup"
        );
        move_to_zone(&mut state, attacker, Zone::Battlefield, &mut events);
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat
                .attackers
                .iter()
                .all(|info| info.object_id != attacker),
            "returned attacker must not inherit stale attacking status"
        );
        assert!(
            !combat.blocker_assignments.contains_key(&attacker),
            "returned attacker must not inherit stale attacker-keyed block assignment"
        );
        assert!(
            !combat.blocker_to_attacker.contains_key(&attacker),
            "returned attacker must not inherit stale blocking status via reverse lookup"
        );
    }

    #[test]
    fn counters_cease_to_exist_across_exile_and_return() {
        use crate::types::counter::CounterType;
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Stapled Cat".to_string(),
            Zone::Battlefield,
        );
        // Put -1/-1 counters on the creature while it's on the battlefield —
        // mirrors the user-reported Personify scenario (the reported leak was
        // -1/-1 counters specifically, e.g. from a Wither/Infect source).
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Minus1Minus1, 2);

        let mut events = Vec::new();
        // Personify step 1: Battlefield → Exile. Counters must cease to
        // exist on the exit boundary (CR 122.2).
        move_to_zone(&mut state, id, Zone::Exile, &mut events);
        assert!(
            state.objects[&id].counters.is_empty(),
            "counters must cease to exist when leaving the battlefield (CR 122.2); had {:?}",
            state.objects[&id].counters
        );

        // Personify step 2: Exile → Battlefield. The new object on the
        // battlefield must have no counters — there's nothing to restore.
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
        assert!(
            state.objects[&id].counters.is_empty(),
            "returning object is a new object per CR 400.7 — no counters carry; had {:?}",
            state.objects[&id].counters
        );
    }

    #[test]
    fn move_battlefield_to_graveyard() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Graveyard);
    }

    #[test]
    fn move_to_zone_clears_old_object_activation_counts() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quirion Ranger".to_string(),
            Zone::Battlefield,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Relic".to_string(),
            Zone::Battlefield,
        );

        state.activated_abilities_this_turn.insert((id, 0), 1);
        state.activated_abilities_this_game.insert((id, 0), 1);
        state.activated_abilities_this_turn.insert((other, 0), 1);
        state.activated_abilities_this_game.insert((other, 0), 1);

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Hand, &mut events);

        assert!(!state.activated_abilities_this_turn.contains_key(&(id, 0)));
        assert!(!state.activated_abilities_this_game.contains_key(&(id, 0)));
        assert_eq!(
            state.activated_abilities_this_turn.get(&(other, 0)),
            Some(&1)
        );
        assert_eq!(
            state.activated_abilities_this_game.get(&(other, 0)),
            Some(&1)
        );
    }

    #[test]
    fn token_dying_does_not_count_as_descending() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Token".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_token = true;
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        assert!(!state.players[0].descended_this_turn);
    }

    #[test]
    fn permanent_card_to_graveyard_counts_as_descending() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        assert!(state.players[0].descended_this_turn);
    }

    #[test]
    fn move_to_exile() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Exile, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.exile.contains(&id));
        assert_eq!(state.objects[&id].zone, Zone::Exile);
    }

    #[test]
    fn move_generates_zone_changed_event() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        assert_eq!(events.len(), 1);
        let GameEvent::ZoneChanged {
            object_id,
            from,
            to,
            record,
        } = &events[0]
        else {
            panic!("move_to_zone must emit ZoneChanged");
        };
        assert_eq!(*object_id, id);
        assert_eq!(*from, Some(Zone::Hand));
        assert_eq!(*to, Zone::Graveyard);
        assert_eq!(record.name, "Card");
        let context = record
            .trigger_source_context()
            .expect("real zone-change events carry their source context");
        assert_eq!(context.identity.reference.object_id, id);
        assert_eq!(context.identity.expected_zone, Zone::Hand);
        assert_eq!(context.card_id, CardId(1));
    }

    #[test]
    fn move_to_library_top() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bottom".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Top".to_string(),
            Zone::Hand,
        );

        let mut events = Vec::new();
        move_to_library_position(&mut state, id2, true, &mut events);

        assert_eq!(state.players[0].library[0], id2); // top
        assert_eq!(state.players[0].library[1], id1); // bottom
        assert_eq!(state.objects[&id2].zone, Zone::Library);
    }

    #[test]
    fn move_to_library_bottom() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );

        let mut events = Vec::new();
        move_to_library_position(&mut state, id2, false, &mut events);

        assert_eq!(state.players[0].library[0], id1); // stays at top
        assert_eq!(state.players[0].library[1], id2); // goes to bottom
    }

    #[test]
    fn within_library_reposition_does_not_create_a_zone_change() {
        let mut state = setup();
        let filler = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Filler".to_string(),
            Zone::Library,
        );
        let card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let incarnation_before = state.objects[&card].incarnation;
        state.commander_declined_zone_return.insert(card);
        let mut events = Vec::new();
        move_to_library_at_index(&mut state, card, Some(0), &mut events); // to top
        move_to_library_at_index(&mut state, card, None, &mut events); // to bottom

        assert_eq!(
            state.objects[&card].incarnation, incarnation_before,
            "a within-library reposition must preserve object identity"
        );
        assert!(
            state.players[0].library.contains(&filler) && state.players[0].library.contains(&card)
        );
        assert!(
            events.is_empty(),
            "repositioning within a library emits no events"
        );
        assert!(
            state.zone_changes_this_turn.is_empty(),
            "repositioning within a library does not enter the zone-change ledger"
        );
        assert!(
            state.commander_declined_zone_return.contains(&card),
            "without a zone change, the commander marker must be preserved"
        );
    }

    #[test]
    fn reorder_within_library_clamps_after_removal_and_appends_when_unspecified() {
        let mut state = setup();
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Third".to_string(),
            Zone::Library,
        );

        reorder_within_library(&mut state, PlayerId(0), &[first, third], Some(99));
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            [second, first, third]
        );

        reorder_within_library(&mut state, PlayerId(0), &[first], None);
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            [second, third, first]
        );
    }

    #[test]
    fn player_zones_are_per_player() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Card".to_string(),
            Zone::Hand,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Card".to_string(),
            Zone::Hand,
        );

        assert!(state.players[0].hand.contains(&id1));
        assert!(!state.players[0].hand.contains(&id2));
        assert!(state.players[1].hand.contains(&id2));
        assert!(!state.players[1].hand.contains(&id1));
    }

    #[test]
    fn shared_zones_work_for_any_player() {
        let mut state = setup();
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Creature".to_string(),
            Zone::Battlefield,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 Creature".to_string(),
            Zone::Battlefield,
        );

        assert!(state.battlefield.contains(&id1));
        assert!(state.battlefield.contains(&id2));
    }

    #[test]
    fn multiple_zone_transfers() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        // Library -> Hand (draw)
        move_to_zone(&mut state, id, Zone::Hand, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Hand);

        // Hand -> Battlefield (play)
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);

        // Battlefield -> Graveyard (destroy)
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Graveyard);

        assert_eq!(events.len(), 3);
    }

    #[test]
    fn instant_cannot_enter_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        // CR 400.4a: Instant should remain in hand
        assert_eq!(state.objects[&id].zone, Zone::Hand);
        assert!(state.players[0].hand.contains(&id));
    }

    #[test]
    fn counters_cleared_on_move_to_zone() {
        // CR 122.2: Counters cease to exist when an object changes zones.
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 3);

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        assert!(state.objects[&id].counters.is_empty());
    }

    #[test]
    fn counters_cleared_on_move_to_library() {
        // CR 122.2: Counters cease to exist when an object changes zones.
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 2);

        let mut events = Vec::new();
        move_to_library_at_index(&mut state, id, Some(0), &mut events);

        assert!(state.objects[&id].counters.is_empty());
    }

    #[test]
    fn counters_cleared_on_exile_to_hand() {
        // CR 122.2: Counters cease to exist on ANY zone transition, not just from battlefield.
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 1);

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Hand, &mut events);

        assert!(state.objects[&id].counters.is_empty());
    }

    /// CR 122.2 + CR 113.6b building-block test for
    /// `StaticMode::CountersPersistAcrossZones`: Me, the Immortal / Skullbriar
    /// retain counters on a move to any zone OTHER than a player's hand or
    /// library, and follow the normal CR 122.2 clear for hand/library moves.
    /// Exercises the full destination matrix so the parameter (the
    /// `excluded_zones` set), not a single card, is verified.
    fn make_persistent_counter_object(state: &mut GameState, card: u64, zone: Zone) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            PlayerId(0),
            "Counter Keeper".to_string(),
            zone,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 4);
        // "Counters remain on ~ as it moves to any zone other than a player's
        // hand or library." Functions in every zone the object can leave with
        // counters on it (CR 113.6b).
        obj.static_definitions.push(
            crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::CountersPersistAcrossZones {
                    excluded_zones: vec![Zone::Hand, Zone::Library],
                },
            )
            .affected(crate::types::ability::TargetFilter::SelfRef)
            .active_zones(vec![
                Zone::Battlefield,
                Zone::Graveyard,
                Zone::Exile,
                Zone::Command,
                Zone::Stack,
            ]),
        );
        id
    }

    #[test]
    fn persistent_counters_survive_move_to_non_excluded_zones() {
        for to in [Zone::Graveyard, Zone::Exile, Zone::Command] {
            let mut state = setup();
            let id = make_persistent_counter_object(&mut state, 1, Zone::Battlefield);
            let mut events = Vec::new();
            move_to_zone(&mut state, id, to, &mut events);
            assert_eq!(
                state.objects[&id]
                    .counters
                    .get(&crate::types::counter::CounterType::Plus1Plus1)
                    .copied(),
                Some(4),
                "counters should persist on move to {to:?}"
            );
        }
    }

    #[test]
    fn persistent_counters_cleared_on_move_to_excluded_hand_or_library() {
        // CR 122.2: hand and library are in `excluded_zones`, so the default
        // clear still applies (matches Me, the Immortal's ruling).
        for to in [Zone::Hand, Zone::Library] {
            let mut state = setup();
            let id = make_persistent_counter_object(&mut state, 1, Zone::Battlefield);
            let mut events = Vec::new();
            move_to_zone(&mut state, id, to, &mut events);
            assert!(
                state.objects[&id].counters.is_empty(),
                "counters should clear on move to excluded zone {to:?}"
            );
        }
    }

    #[test]
    fn persistent_counters_survive_graveyard_to_battlefield_reanimation() {
        // CR 113.6b: the ability is read from the graveyard (from-zone) state;
        // a reanimated Me/Skullbriar keeps its graveyard counters.
        let mut state = setup();
        let id = make_persistent_counter_object(&mut state, 1, Zone::Graveyard);
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
        assert_eq!(
            state.objects[&id]
                .counters
                .get(&crate::types::counter::CounterType::Plus1Plus1)
                .copied(),
            Some(4),
            "graveyard→battlefield should preserve counters per the from-zone ability"
        );
    }

    #[test]
    fn face_down_instant_can_enter_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Morph Instant".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.face_down = true;
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        // Face-down instants (morph) can enter the battlefield
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
    }

    #[test]
    fn sorcery_cannot_enter_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Time Walk".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        // CR 307.4 / CR 400.4a: Sorcery should remain in hand
        assert_eq!(state.objects[&id].zone, Zone::Hand);
        assert!(state.players[0].hand.contains(&id));
    }

    #[test]
    fn instant_creature_mdfc_can_enter_battlefield() {
        // CR 110.4: An object with both Instant and Creature types (MDFC back face)
        // should be allowed to enter the battlefield because it has a permanent type.
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "MDFC Back".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        // Should enter because it has a permanent type (Creature)
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
    }

    /// CR 712.14a + CR 712.8e: a DFC whose FRONT face is a Sorcery (non-permanent)
    /// can still enter the battlefield when it is instructed to enter TRANSFORMED
    /// (back face up) — eligibility reads the BACK face's core types (a Creature,
    /// a permanent type, CR 110.4), so the CR 307.4 / CR 400.4a reject is bypassed.
    ///
    /// REVERT-CATCHER: flips red if the entry-face rewrite (reading the back
    /// face for a transformed entry) is removed — the front Sorcery type would
    /// then trip the instant/sorcery guard and the DFC would stay in hand.
    #[test]
    fn transform_entry_sorcery_front_creature_back_allowed_via_flag() {
        use crate::game::game_object::BackFaceData;
        use crate::types::card_type::CardType;

        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Esper Origins".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Sorcery],
                subtypes: vec![],
            };
            obj.base_card_types = obj.card_types.clone();
            obj.back_face = Some(BackFaceData {
                name: "Summon: Esper Maduin".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
                printed_loyalty: None,
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![],
                },
                mana_cost: crate::types::mana::ManaCost::default(),
                keywords: vec![],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: None,
                parse_warnings: vec![],
            });
        }

        let mut events = Vec::new();
        move_to_zone_with_entry_flags(&mut state, id, Zone::Battlefield, &mut events, true);

        assert_eq!(
            state.objects[&id].zone,
            Zone::Battlefield,
            "CR 712.14a + CR 712.8e: a transformed entry reads the back face's \
             Creature (permanent, CR 110.4) type and is permitted by CR 400.4a"
        );
    }

    /// CR 307.4 / CR 400.4a negative reach-guard: the SAME Sorcery//Creature DFC
    /// entering through the PUBLIC `move_to_zone` (enter_transformed = false) is
    /// rejected — its FRONT Sorcery face falls to the instant/sorcery guard. This
    /// proves the transformed-entry carve-out is conditioned on `enter_transformed`
    /// and is never unconditional.
    #[test]
    fn transform_entry_sorcery_front_rejected_without_flag() {
        use crate::game::game_object::BackFaceData;
        use crate::types::card_type::CardType;

        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Esper Origins".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Sorcery],
                subtypes: vec![],
            };
            obj.base_card_types = obj.card_types.clone();
            obj.back_face = Some(BackFaceData {
                name: "Summon: Esper Maduin".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
                printed_loyalty: None,
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Creature],
                    subtypes: vec![],
                },
                mana_cost: crate::types::mana::ManaCost::default(),
                keywords: vec![],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: None,
                parse_warnings: vec![],
            });
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        assert_eq!(
            state.objects[&id].zone,
            Zone::Hand,
            "CR 307.4 / CR 400.4a: without enter_transformed the front Sorcery \
             face cannot enter the battlefield"
        );
        assert!(state.players[0].hand.contains(&id));
    }

    /// CR 712.14a (2nd sentence) — SF1 asymmetric branch, DIRECT reach-guard: a
    /// SINGLE-FACED permanent-front object (`back_face = None`) instructed to
    /// enter transformed can NEVER enter, even though its front face is a
    /// creature. `move_to_zone_with_entry_flags(..., true)` drives the wrapper
    /// directly, bypassing the zone_pipeline single-faced early-return (so only
    /// this guard's SF1 branch is exercised).
    ///
    /// REVERT-CATCHER for SF1: if the asymmetric guard were removed or regressed
    /// to a front-face fallback, this single-faced Creature-with-flag=true call
    /// would land in Battlefield and this test flips red.
    #[test]
    fn transform_entry_single_faced_permanent_front_rejected_with_flag() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Single-Faced".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        // back_face intentionally left None (single-faced; the GameState default).

        let mut events = Vec::new();
        move_to_zone_with_entry_flags(&mut state, id, Zone::Battlefield, &mut events, true);

        assert_eq!(
            state.objects[&id].zone,
            Zone::Hand,
            "CR 712.14a (2nd sentence): a single-faced object cannot enter transformed"
        );
        assert!(state.players[0].hand.contains(&id));
    }

    /// CR 712.14a + CR 400.4a positive reach-guard pairing the SF1 rejection: the
    /// SAME single-faced permanent-front fixture entering through the PUBLIC
    /// `move_to_zone` (enter_transformed = false) lands in Battlefield. Proves the
    /// rejection above is conditioned on `enter_transformed`, NOT on
    /// single-facedness — a bare single-faced Creature on a plain entry has no
    /// instant/sorcery type on the entry face, so CR 400.4a passes.
    #[test]
    fn transform_entry_single_faced_permanent_front_allowed_without_flag() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Single-Faced".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        // back_face intentionally left None (single-faced).

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        assert_eq!(
            state.objects[&id].zone,
            Zone::Battlefield,
            "CR 400.4a: a single-faced Creature on a plain entry is a permanent and \
             enters normally"
        );
    }

    #[test]
    fn phased_out_grafdiggers_cage_allows_reanimation_from_graveyard() {
        // CR 702.26b + CR 614.1d regression: Grafdigger's Cage on the
        // battlefield prevents a creature from entering from graveyard /
        // library. Phased out, it must NOT — so reanimation succeeds.
        // Drives the real `move_to_zone` -> `is_blocked_from_entering_battlefield`
        // pipeline.
        use crate::types::ability::{FilterProp, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::statics::StaticMode;

        let mut state = setup();

        // Grafdigger's Cage: "Creature cards in graveyards and libraries can't
        // enter the battlefield." Affected filter = creature cards whose zone
        // is graveyard OR library.
        let cage = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grafdigger's Cage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&cage).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.static_definitions.push(
                crate::types::ability::StaticDefinition::new(StaticMode::CantEnterBattlefieldFrom)
                    .affected(TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Graveyard, Zone::Library],
                            }]),
                    )),
            );
        }

        // A creature card sitting in P0's graveyard, the target of reanimation.
        let dead = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Dead Bear".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&dead).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }

        // Baseline: with Cage functioning, reanimation is blocked.
        let mut events = Vec::new();
        move_to_zone(&mut state, dead, Zone::Battlefield, &mut events);
        assert_eq!(
            state.objects[&dead].zone,
            Zone::Graveyard,
            "Functioning Cage must block ETB from graveyard"
        );

        // Phase out the Cage via the real pipeline — CR 702.26b puts it into
        // PhasedOut status, which the functioning-abilities gate must drop.
        let mut phase_events = Vec::new();
        crate::game::phasing::phase_out_object(
            &mut state,
            cage,
            crate::game::game_object::PhaseOutCause::Directly,
            &mut phase_events,
        );

        // Reanimate again — now the move must succeed because the phased-out
        // Cage contributes no CantEnterBattlefieldFrom static.
        let mut events2 = Vec::new();
        move_to_zone(&mut state, dead, Zone::Battlefield, &mut events2);
        assert_eq!(
            state.objects[&dead].zone,
            Zone::Battlefield,
            "Phased-out Cage must not block ETB from graveyard"
        );
    }

    #[test]
    fn floating_cant_enter_from_exile_blocks_then_expires_at_cleanup() {
        // CR 611.2a + CR 614.1d + CR 514.2 runtime proof for Bad Wolf Bay's
        // chaos ability: a floating `GameRestriction::CantEnterBattlefieldFrom`
        // (origin = exile) blocks an object from entering the battlefield from
        // exile via the SAME `move_to_zone` -> `is_blocked_from_entering_
        // battlefield` gate as the Grafdigger's Cage static, and the "this turn"
        // restriction is pruned at cleanup so a later move succeeds.
        use crate::types::ability::{
            FilterProp, GameRestriction, RestrictionExpiry, TargetFilter, TypedFilter,
        };

        let mut state = setup();

        // A creature card sitting in exile (Bad Wolf Bay exiled it at combat and
        // wants it back at the next end step — but chaos ensued this turn).
        let exiled = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Exiled Bear".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&exiled).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }

        // "cards can't enter from exile this turn" — empty type_filters = any
        // card; origin zone = exile.
        state
            .restrictions
            .push(GameRestriction::CantEnterBattlefieldFrom {
                source: crate::types::identifiers::ObjectId(0),
                expiry: RestrictionExpiry::EndOfTurn,
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::InAnyZone {
                        zones: vec![Zone::Exile],
                    },
                ])),
            });

        // With the restriction active the return is blocked — the object stays
        // in exile. CR 614.1d: the "[objects] can't enter the battlefield"
        // continuous effect is a replacement effect; CR 101.2: the "can't"
        // effect takes precedence over the attempt to enter, so the creature
        // remains in exile.
        let mut events = Vec::new();
        move_to_zone(&mut state, exiled, Zone::Battlefield, &mut events);
        assert_eq!(
            state.objects[&exiled].zone,
            Zone::Exile,
            "floating CantEnterBattlefieldFrom must block ETB from exile"
        );

        // CR 514.2: the "this turn" restriction ends at cleanup.
        let mut cleanup_events = Vec::new();
        crate::game::turns::execute_cleanup(&mut state, &mut cleanup_events);
        assert!(
            state.restrictions.is_empty(),
            "EndOfTurn restriction must be pruned at cleanup, got {:?}",
            state.restrictions
        );

        // Now the same move succeeds — the gate no longer fires.
        let mut events2 = Vec::new();
        move_to_zone(&mut state, exiled, Zone::Battlefield, &mut events2);
        assert_eq!(
            state.objects[&exiled].zone,
            Zone::Battlefield,
            "after the restriction expires, ETB from exile must succeed"
        );
    }

    #[test]
    fn move_to_zone_snapshots_linked_exile_before_pruning_tracked_links() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Skyclave Apparition".to_string(),
            Zone::Battlefield,
        );
        let exiled = create_object(
            &mut state,
            CardId(51),
            PlayerId(1),
            "Exiled Card".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled).unwrap().mana_cost =
            crate::types::mana::ManaCost::generic(4);
        state.exile_links.push(crate::types::game_state::ExileLink {
            source_id: source,
            exiled_id: exiled,
            kind: crate::types::game_state::ExileLinkKind::TrackedBySource,
        });

        let mut events = Vec::new();
        move_to_zone(&mut state, source, Zone::Graveyard, &mut events);

        let record = match &events[0] {
            GameEvent::ZoneChanged { record, .. } => record,
            other => panic!("expected ZoneChanged event, got {other:?}"),
        };

        assert_eq!(
            record.linked_exile_snapshot,
            vec![crate::types::game_state::LinkedExileSnapshot {
                exiled_id: exiled,
                owner: PlayerId(1),
                mana_value: 4,
            }]
        );
        assert!(
            state
                .exile_links
                .iter()
                .all(|link| link.source_id != source),
            "TrackedBySource links should still be pruned immediately after LTB"
        );
    }

    /// CR 607.2a + CR 400.7: A source that leaves the battlefield TO EXILE
    /// (self-exile as an activation cost — Mechtitan Core) keeps a stable
    /// ObjectId in exile and stays the linked-ability referent for its
    /// "exiled with ~" pile, so its `TrackedBySource` links must survive the
    /// exit. The sibling above (exit to graveyard) proves the survival is
    /// exile-scoped, not a blanket "never prune".
    #[test]
    fn tracked_by_source_links_survive_source_exit_to_exile() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Mechtitan Core".to_string(),
            Zone::Battlefield,
        );
        let exiled = create_object(
            &mut state,
            CardId(61),
            PlayerId(1),
            "Exiled With Source".to_string(),
            Zone::Exile,
        );
        state.exile_links.push(crate::types::game_state::ExileLink {
            source_id: source,
            exiled_id: exiled,
            kind: crate::types::game_state::ExileLinkKind::TrackedBySource,
        });

        let mut events = Vec::new();
        move_to_zone(&mut state, source, Zone::Exile, &mut events);

        assert!(
            state
                .exile_links
                .iter()
                .any(|link| link.source_id == source && link.exiled_id == exiled),
            "TrackedBySource link must survive the source's self-exile"
        );
        // Reach-guard: the deferred `ExiledBySource` lookup keyed to the now-exiled
        // source still resolves the pile (the property the Mechtitan return relies on).
        assert!(
            crate::game::players::linked_exile_cards_for_source(&state, source)
                .iter()
                .any(|snap| snap.exiled_id == exiled),
            "linked_exile_cards_for_source must return the pile after the source self-exiles"
        );
    }

    /// CR 400.7: A source that self-exiled (keeping its `TrackedBySource` links)
    /// and is then returned to the battlefield is a new object and sheds those
    /// stale links, so a later "cards exiled with ~" reference cannot read a
    /// prior incarnation's pile. This is the blink-back reset paired with the
    /// exit-to-exile preservation above.
    #[test]
    fn tracked_by_source_links_reset_on_battlefield_reentry() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(62),
            PlayerId(0),
            "Blinked Source".to_string(),
            Zone::Exile,
        );
        let exiled = create_object(
            &mut state,
            CardId(63),
            PlayerId(1),
            "Old Pile Card".to_string(),
            Zone::Exile,
        );
        state.exile_links.push(crate::types::game_state::ExileLink {
            source_id: source,
            exiled_id: exiled,
            kind: crate::types::game_state::ExileLinkKind::TrackedBySource,
        });

        let mut events = Vec::new();
        move_to_zone(&mut state, source, Zone::Battlefield, &mut events);

        assert!(
            state
                .exile_links
                .iter()
                .all(|link| link.source_id != source),
            "TrackedBySource links keyed to a re-entering source must be dropped (CR 400.7)"
        );
    }

    /// CR 712.8a + CR 400.7: An MDFC permanent that entered the battlefield as
    /// its back face (modal_back_face = true) must revert to its front face when
    /// it leaves the battlefield (battlefield is the only non-stack zone where
    /// back face is permitted).
    #[test]
    fn mdfc_back_face_reverts_to_front_face_on_leaving_battlefield() {
        use crate::game::game_object::BackFaceData;
        use crate::game::printed_cards::apply_back_face_to_object;
        use crate::types::card_type::{CardType, CoreType};
        use crate::types::keywords::Keyword;

        let mut state = setup();

        // Create an MDFC in command zone, showing its front face (Valki-like).
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Front Face".to_string(),
            Zone::Command,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["God".to_string()],
            };
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            // Store back face data (original MDFC back face).
            obj.back_face = Some(BackFaceData {
                name: "Back Face".to_string(),
                power: Some(6),
                toughness: Some(6),
                loyalty: None,
                printed_loyalty: None,
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Planeswalker],
                    subtypes: vec!["Devil".to_string()],
                },
                mana_cost: crate::types::mana::ManaCost::default(),
                keywords: vec![Keyword::Trample],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: Some(crate::types::card::LayoutKind::Modal),
                parse_warnings: vec![],
            });
        }

        // Simulate ChooseModalFace { back_face: true }: apply back face and set flag.
        let front_snapshot =
            crate::game::printed_cards::snapshot_object_face(state.objects.get(&id).unwrap());
        let back_data = state
            .objects
            .get_mut(&id)
            .unwrap()
            .back_face
            .take()
            .unwrap();
        {
            let obj = state.objects.get_mut(&id).unwrap();
            apply_back_face_to_object(obj, back_data);
            obj.back_face = Some(front_snapshot);
            obj.modal_back_face = true;
        }

        // Move to battlefield.
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        {
            let obj = &state.objects[&id];
            assert!(obj.modal_back_face, "flag must still be set on battlefield");
            assert_eq!(obj.name, "Back Face");
        }

        // Leave the battlefield (dies / commander SBA).
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        let obj = &state.objects[&id];
        // CR 712.8a: must revert to front face.
        assert!(
            !obj.modal_back_face,
            "modal_back_face must be cleared after leaving battlefield"
        );
        assert_eq!(obj.name, "Front Face", "must show front face in graveyard");
        assert_eq!(obj.power, Some(1), "power must revert to front face");
        assert_eq!(obj.card_types.core_types, vec![CoreType::Creature]);
    }

    /// #7782 round 4: the REPLAY applier must install the same cast-origin
    /// lifetime as the live path — a replayed stamped Stack → Graveyard
    /// command clears the stamp exactly like the live transition did.
    #[test]
    fn a_replayed_stack_exit_clears_the_stamp_like_the_live_one() {
        let mut live = setup();
        let id = create_object(
            &mut live,
            CardId(7783),
            PlayerId(0),
            "Replayed Spell".to_string(),
            Zone::Stack,
        );
        live.objects.get_mut(&id).unwrap().cast_from_zone = Some(Zone::Hand);
        let mut replayed = live.clone();

        let record = crate::types::game_state::ZoneChangeRecord::test_minimal(
            id,
            Some(Zone::Stack),
            Zone::Graveyard,
        );
        let command = resolve_and_apply_zone_change(
            &mut live,
            id,
            Zone::Stack,
            Zone::Graveyard,
            PlayerId(0),
            record,
        )
        .expect("live transition must resolve");
        assert_eq!(
            live.objects[&id].cast_from_zone, None,
            "reach-guard: the live transition clears the stamp"
        );

        apply_resolved_zone_change(&mut replayed, &command)
            .expect("replaying the recorded command must succeed");
        assert_eq!(
            replayed.objects[&id].cast_from_zone, None,
            "the replayed transition must clear the stamp exactly like the live one"
        );
    }

    /// #7782 round 3: a spell leaving the STACK for a non-battlefield zone
    /// (countered / fizzled / instant to the graveyard) must lose its
    /// `cast_from_zone` stamp (CR 400.7 — a new object has no memory of its
    /// cast), so a later recast from another zone cannot inherit the stale
    /// origin. The battlefield legs are owned by `reset_for_battlefield_entry`
    /// / `_exit` and their `CastLinkSnapshot` restore.
    #[test]
    fn the_cast_from_zone_stamp_dies_off_stack_and_battlefield() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(7782),
            PlayerId(0),
            "Stamped Spell".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&id).unwrap().cast_from_zone = Some(Zone::Hand);

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        assert_eq!(
            state.objects[&id].cast_from_zone, None,
            "a spell leaving the stack for the graveyard must lose the stamp (CR 400.7)"
        );
    }

    /// CR 708.9: A face-down permanent is revealed when it leaves the battlefield.
    #[test]
    fn face_down_permanent_turns_face_up_when_leaving_battlefield() {
        use crate::game::morph::manifest_card;
        use crate::types::ability::FaceDownProfile;

        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(3285),
            PlayerId(0),
            "Hidden Bear".to_string(),
            Zone::Library,
        );
        state.players[0].library.push_front(id);

        let mut events = Vec::new();
        manifest_card(
            &mut state,
            PlayerId(0),
            id,
            id,
            FaceDownProfile::vanilla_2_2(),
            None,
            &mut events,
        )
        .unwrap();
        assert!(state.objects[&id].face_down);

        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        let obj = &state.objects[&id];
        assert!(
            !obj.face_down,
            "CR 708.9 must clear face_down on battlefield exit"
        );
        assert_eq!(obj.name, "Hidden Bear");
        assert!(obj.back_face.is_none());
    }

    /// CR 708.4: A face-down spell that resolves to the battlefield becomes a
    /// face-down permanent. CR 708.9 reveal only applies when it leaves the stack
    /// for a zone other than the battlefield.
    #[test]
    fn face_down_spell_stays_face_down_when_resolving_to_battlefield() {
        use crate::game::morph::apply_face_down_creature_characteristics;
        use crate::game::printed_cards::snapshot_object_face;
        use crate::types::ability::FaceDownProfile;

        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(3286),
            PlayerId(0),
            "Hidden Stack Bear".to_string(),
            Zone::Stack,
        );
        {
            let original = snapshot_object_face(&state.objects[&id]);
            let obj = state.objects.get_mut(&id).unwrap();
            apply_face_down_creature_characteristics(obj, &FaceDownProfile::vanilla_2_2());
            obj.back_face = Some(original);
        }

        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Battlefield, &mut events);

        let obj = &state.objects[&id];
        assert!(
            obj.face_down,
            "CR 708.4 keeps the resolved permanent face down"
        );
        assert_eq!(obj.name, "");
        assert!(obj.back_face.is_some());
    }

    /// CR 712.8a: A countered MDFC spell (stack → graveyard) must also revert to
    /// front face — the graveyard is "a zone other than the battlefield or stack."
    #[test]
    fn mdfc_back_face_reverts_on_countered_spell_to_graveyard() {
        use crate::game::game_object::BackFaceData;
        use crate::game::printed_cards::apply_back_face_to_object;
        use crate::types::card_type::{CardType, CoreType};

        let mut state = setup();

        let id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Front Face".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.back_face = Some(BackFaceData {
                name: "Back Face".to_string(),
                power: Some(6),
                toughness: Some(6),
                loyalty: None,
                printed_loyalty: None,
                defense: None,
                card_types: CardType {
                    supertypes: vec![],
                    core_types: vec![CoreType::Planeswalker],
                    subtypes: vec![],
                },
                mana_cost: crate::types::mana::ManaCost::default(),
                keywords: vec![],
                abilities: vec![],
                trigger_definitions: Default::default(),
                replacement_definitions: Default::default(),
                static_definitions: Default::default(),
                color: vec![],
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                layout_kind: Some(crate::types::card::LayoutKind::Modal),
                parse_warnings: vec![],
            });
        }
        // Apply back face (simulating ChooseModalFace on stack).
        let front_snapshot =
            crate::game::printed_cards::snapshot_object_face(state.objects.get(&id).unwrap());
        let back_data = state
            .objects
            .get_mut(&id)
            .unwrap()
            .back_face
            .take()
            .unwrap();
        {
            let obj = state.objects.get_mut(&id).unwrap();
            apply_back_face_to_object(obj, back_data);
            obj.back_face = Some(front_snapshot);
            obj.modal_back_face = true;
        }

        // Spell is countered: stack → graveyard.
        let mut events = Vec::new();
        move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        // CR 712.8a: graveyard is not battlefield/stack — must show front face.
        let obj = &state.objects[&id];
        assert!(
            !obj.modal_back_face,
            "flag must be cleared when spell goes to graveyard"
        );
        assert_eq!(
            obj.name, "Front Face",
            "must revert to front face in graveyard"
        );
    }

    #[test]
    fn aura_leaving_battlefield_clears_attached_to() {
        use crate::game::effects::attach::attach_to;
        use crate::types::card_type::CoreType;

        let mut state = setup();
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Host".to_string(),
            Zone::Battlefield,
        );
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Aura".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .subtypes
            .push("Aura".to_string());
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        attach_to(&mut state, aura, host);

        let mut events = Vec::new();
        move_to_zone(&mut state, aura, Zone::Graveyard, &mut events);

        assert_eq!(state.objects[&aura].zone, Zone::Graveyard);
        assert!(
            state.objects[&aura].attached_to.is_none(),
            "attached_to must be cleared when the aura leaves the battlefield"
        );
        assert!(
            !state.objects[&host].attachments.contains(&aura),
            "host must not retain a stale attachments entry"
        );
    }

    #[test]
    fn sba_pipeline_graveyard_clears_attached_to() {
        use crate::game::effects::attach::attach_to;
        use crate::game::zone_pipeline::{ZoneMoveRequest, ZoneMoveResult};
        use crate::types::card_type::CoreType;

        let mut state = setup();
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Host".to_string(),
            Zone::Battlefield,
        );
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Aura".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .subtypes
            .push("Aura".to_string());
        state
            .objects
            .get_mut(&aura)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);
        attach_to(&mut state, aura, host);

        let mut events = Vec::new();
        let result = crate::game::zone_pipeline::move_object(
            &mut state,
            ZoneMoveRequest::state_based_action(aura, Zone::Graveyard),
            &mut events,
        );
        assert!(matches!(result, ZoneMoveResult::Done));
        assert_eq!(state.objects[&aura].zone, Zone::Graveyard);
        assert!(state.objects[&aura].attached_to.is_none());
        assert!(
            events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::Unattached {
                        attachment_id,
                        old_target
                    } if *attachment_id == aura
                        && *old_target == crate::types::ability::TargetRef::Object(host)
                )
            }),
            "SBA zone movement must still publish the unattach event for triggers"
        );
    }

    /// pod-lab loop-3 Q5, row 5: `restore_after_rollback` targeting the
    /// battlefield must still force a full layers re-evaluation
    /// unconditionally — CR 601.2 + CR 733.1, reversing an incomplete action
    /// is rare (not gameplay-hot) and can leave board state in a shape the
    /// entry-only incremental-flush safety classifier was never designed to
    /// reason about, so there is no perf case for trusting `move_to_zone`'s
    /// own (now axis-gated) internal decision here. Today's only production
    /// caller targets Graveyard, not Battlefield, so this exercises the
    /// function's general contract directly rather than replaying an
    /// existing call site.
    #[test]
    fn restore_after_rollback_to_battlefield_marks_full() {
        let mut state = setup();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rolled Back Spell".to_string(),
            Zone::Stack,
        );
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        let mut events = Vec::new();
        restore_after_rollback(&mut state, id, Zone::Battlefield, &mut events);

        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert!(
            matches!(
                state.layers_dirty,
                crate::types::game_state::LayersDirty::Full
            ),
            "restore_after_rollback targeting the battlefield must \
             unconditionally force a full re-evaluation, got {:?}",
            state.layers_dirty
        );
    }
}
