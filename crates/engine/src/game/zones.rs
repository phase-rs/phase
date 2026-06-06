use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
#[cfg(test)]
use crate::types::game_state::ZoneChangeRecord;
use crate::types::game_state::{GameState, ZoneChangeCombatStatus};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::game_object::GameObject;
use super::printed_cards::{apply_back_face_to_object, snapshot_object_face};

/// CR 603.10a + CR 603.6e: Capture a snapshot of every attachment on `obj` at the
/// moment of the zone change. The snapshot records each attachment's current
/// controller and kind (Aura/Equipment) so that look-back triggers of the form
/// "for each Aura you controlled that was attached to it" (Hateful Eidolon)
/// can resolve their quantity after SBA has already unattached the Auras.
fn capture_attachment_snapshot(
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
fn apply_zone_exit_cleanup(state: &mut GameState, object_id: ObjectId, from: Zone, to: Zone) {
    // CR 400.7: An object that changes zones becomes a new object with no
    // memory of its previous existence. Both the short-lived `revealed_cards`
    // (cleared at action boundaries) and the persistent `public_revealed_cards`
    // (reveal memory that survives action boundaries so e.g. a Duress-revealed
    // card stays visible in the opponent's hand) are keyed by ObjectId. Since
    // ObjectId here is storage identity and persists across the zone change,
    // we must drop both flags so a card shuffled back into the library and
    // re-drawn does not surface as "still revealed."
    state.revealed_cards.remove(&object_id);
    state.public_revealed_cards.remove(&object_id);
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
        if let Some(obj) = state.objects.get(&object_id) {
            let lki = crate::types::game_state::LKISnapshot {
                name: obj.name.clone(),
                power: obj.power,
                toughness: obj.toughness,
                // CR 208.4b + CR 613.4b: Capture the layer-7b base values so
                // base-scope P/T look-back filters read the base, not current.
                base_power: obj.base_power,
                base_toughness: obj.base_toughness,
                mana_value: obj.mana_cost.mana_value(),
                controller: obj.controller,
                owner: obj.owner,
                // CR 400.7: Capture core types for "if it was a creature" patterns.
                card_types: obj.card_types.core_types.clone(),
                subtypes: obj.card_types.subtypes.clone(),
                supertypes: obj.card_types.supertypes.clone(),
                keywords: obj.keywords.clone(),
                colors: obj.color.clone(),
                chosen_attributes: obj.chosen_attributes.clone(),
                // CR 400.7: Capture counters for "if it had counters on it" patterns.
                counters: obj.counters.clone(),
            };
            state.lki_cache.insert(object_id, lki);
        }
    }

    if let Some(obj_mut) = state.objects.get_mut(&object_id) {
        // CR 712.14 + CR 400.7: Transformed permanents revert to front face on zone change.
        if obj_mut.transformed {
            if let Some(back_face) = obj_mut.back_face.clone() {
                let current_back = snapshot_object_face(obj_mut);
                apply_back_face_to_object(obj_mut, back_face);
                obj_mut.back_face = Some(current_back);
                obj_mut.transformed = false;
            }
        }

        // CR 400.7 + CR 113.6e: Clear exile-based casting permissions when leaving exile
        // (prevents re-casting if the card returns to exile via a different effect).
        if from == Zone::Exile {
            // CR 702.143c-d + CR 400.7: Foretold is a designation of the card
            // while it remains in exile. Once it changes zones, the new object
            // is no longer a foretold card.
            obj_mut.foretold = false;
            obj_mut.face_down = false;
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

        // CR 122.2: Counters cease to exist when an object changes zones.
        obj_mut.counters.clear();
    }

    if from == Zone::Battlefield {
        // CR 701.54e: A player's Ring-bearer designation applies only while
        // that permanent remains on the battlefield under that player's control.
        super::effects::ring::clear_ring_bearer_if_object(state, object_id);
    }

    // Prune host-bound transient effects and clean up mana-tap tracking
    // when a permanent leaves the battlefield.
    if from == Zone::Battlefield {
        super::pairing::break_pair(state, object_id);
        crate::game::layers::mark_layers_full(state);
        super::layers::prune_host_left_effects(state, object_id);
        super::layers::prune_affected_object_left_effects(state, object_id);
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
        state.exile_links.retain(|link| {
            link.source_id != object_id
                || matches!(
                    link.kind,
                    crate::types::game_state::ExileLinkKind::UntilSourceLeaves { .. }
                )
        });
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

/// CR 400.7: Move an object to a new zone. An object that moves to a new zone becomes a new object.
pub fn move_to_zone(
    state: &mut GameState,
    object_id: ObjectId,
    mut to: Zone,
    events: &mut Vec<GameEvent>,
) {
    // CR 903.9a: A fresh zone change resets the "declined zone return" flag
    // so the owner gets a new choice opportunity if the commander moves again.
    state.commander_declined_zone_return.remove(&object_id);

    // CR 614.1d: Check CantEnterBattlefieldFrom statics before allowing the move.
    // e.g., Grafdigger's Cage: "Creature cards in graveyards and libraries can't enter the battlefield."
    if to == Zone::Battlefield {
        if let Some(obj) = state.objects.get(&object_id) {
            if is_blocked_from_entering_battlefield(state, obj) {
                return;
            }
            // CR 304.4 / CR 307.4 / CR 400.4a: Instants and sorceries can't enter
            // the battlefield. Skip for face-down (morph/manifest) and objects with
            // a permanent type (MDFC back faces).
            if !obj.face_down
                && (obj.card_types.core_types.contains(&CoreType::Instant)
                    || obj.card_types.core_types.contains(&CoreType::Sorcery))
                && !obj.card_types.core_types.iter().any(|ct| {
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
    let unattached_from = if from == Zone::Battlefield {
        obj.attached_to
            .map(super::effects::attach::target_ref_from_attach_target)
    } else {
        None
    };
    let mut zone_change_record = obj.snapshot_for_zone_change(object_id, Some(from), to);
    // CR 603.10a + CR 603.6e: Capture attachment snapshot before SBA can detach.
    zone_change_record.attachments = capture_attachment_snapshot(state, obj);
    // CR 603.10a + CR 607.2a: Leaves-the-battlefield triggers look back to the
    // object as it existed immediately before the move. Snapshot linked "exiled
    // with" cards here, before CR 400.7 cleanup prunes `TrackedBySource`.
    zone_change_record.linked_exile_snapshot =
        capture_linked_exile_snapshot(state, object_id, from);
    zone_change_record.combat_status = capture_combat_status(state, object_id);

    apply_zone_exit_cleanup(state, object_id, from, to);

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

    // CR 603.6c: Drop the leaving permanent from the TriggerIndex. The
    // leaves-battlefield last-known-information scan in
    // `collect_pending_triggers` reads `state.objects` directly (the object's
    // zone is no longer Battlefield), unaffected by this removal. The
    // authoritative correctness path is the `evaluate_layers` rebuild
    // (CR 611.2e); this hook is incremental optimization between layer flushes.
    if from == Zone::Battlefield {
        state.trigger_index.remove(object_id);
    }

    let obj_mut = state.objects.get_mut(&object_id).unwrap();
    obj_mut.zone = to;

    if to == Zone::Battlefield {
        obj_mut.reset_for_battlefield_entry(state.turn_number);
    }

    // Track descended: a permanent card was put into its owner's graveyard.
    if to == Zone::Graveyard {
        let is_permanent_card = obj_mut.card_types.core_types.iter().any(|ct| {
            matches!(
                ct,
                CoreType::Creature
                    | CoreType::Artifact
                    | CoreType::Enchantment
                    | CoreType::Planeswalker
                    | CoreType::Land
                    | CoreType::Battle
            )
        });
        if is_permanent_card && !obj_mut.is_token {
            if let Some(player) = state.players.iter_mut().find(|p| p.id == owner) {
                player.descended_this_turn = true;
            }
        }
    }

    // Mark layers dirty when objects enter the battlefield, or the hand (so
    // Lorehold-style hand-zone grants re-apply to newly-drawn cards).
    // Exit-side dirty marking is handled by apply_zone_exit_cleanup.
    // CR 702.94a + CR 400.3: hand-zone continuous effects require re-evaluation
    // when a hand object appears or departs.
    if to == Zone::Battlefield || to == Zone::Hand || from == Zone::Hand {
        crate::game::layers::mark_layers_full(state);
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
        let registration = state.objects.get(&object_id).map(|obj| {
            let defs: smallvec::SmallVec<[crate::types::ability::TriggerDefinition; 4]> =
                obj.trigger_definitions.as_slice().iter().cloned().collect();
            let synthetic = super::trigger_index::has_synthetic_keyword_trigger_for(obj);
            (defs, synthetic)
        });
        if let Some((defs, synthetic)) = registration {
            state.trigger_index.add(object_id, &defs, synthetic);
        }
    }

    super::restrictions::record_zone_change(state, zone_change_record.clone());

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

fn capture_linked_exile_snapshot(
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
        .filter(|link| {
            link.source_id == source_id
                && matches!(
                    link.kind,
                    crate::types::game_state::ExileLinkKind::TrackedBySource
                )
        })
        .filter_map(|link| {
            state.objects.get(&link.exiled_id).and_then(|obj| {
                (obj.zone == Zone::Exile).then(|| crate::types::game_state::LinkedExileSnapshot {
                    exiled_id: link.exiled_id,
                    owner: obj.owner,
                    mana_value: obj.mana_cost.mana_value(),
                })
            })
        })
        .collect()
}

fn capture_combat_status(state: &GameState, object_id: ObjectId) -> ZoneChangeCombatStatus {
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
        defending_player: attacker.map(|attacker| attacker.defending_player),
    }
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
    // CR 903.9a: A fresh zone change resets the "declined zone return" flag.
    state.commander_declined_zone_return.remove(&object_id);

    let obj = state.objects.get(&object_id).expect("object exists");
    let from = obj.zone;
    let owner = obj.owner;
    let unattached_from = if from == Zone::Battlefield {
        obj.attached_to
            .map(super::effects::attach::target_ref_from_attach_target)
    } else {
        None
    };
    let mut zone_change_record = obj.snapshot_for_zone_change(object_id, Some(from), Zone::Library);
    // CR 603.10a + CR 603.6e: Capture attachment snapshot before SBA can detach.
    zone_change_record.attachments = capture_attachment_snapshot(state, obj);
    zone_change_record.combat_status = capture_combat_status(state, object_id);

    apply_zone_exit_cleanup(state, object_id, from, Zone::Library);

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

    if let Some(obj_mut) = state.objects.get_mut(&object_id) {
        obj_mut.zone = Zone::Library;
    }

    super::restrictions::record_zone_change(state, zone_change_record.clone());

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
            state.stack.retain(|e| e.id != object_id);
            state.stack_paid_facts.remove(&object_id);
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
            } else {
                state.command_zone.retain(|id| *id != object_id);
            }
        }
    }
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
            } else {
                state.command_zone.push_back(object_id);
            }
        }
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
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.base_controller = Some(controller);
        obj.controller = controller;
    }

    if let Some(record) = state
        .zone_changes_this_turn
        .iter_mut()
        .rev()
        .find(|record| record.object_id == object_id && record.to_zone == Zone::Battlefield)
    {
        record.controller = controller;
    }

    if let Some(record) = state
        .battlefield_entries_this_turn
        .iter_mut()
        .rev()
        .find(|record| record.object_id == object_id)
    {
        record.controller = controller;
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
    }
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
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::game_state::GameState;

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
        assert_eq!(
            events[0],
            GameEvent::ZoneChanged {
                object_id: id,
                from: Some(Zone::Hand),
                to: Zone::Graveyard,
                record: Box::new(ZoneChangeRecord {
                    name: "Card".to_string(),
                    ..ZoneChangeRecord::test_minimal(id, Some(Zone::Hand), Zone::Graveyard)
                }),
            }
        );
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
}
