use crate::types::ability::{
    CastingPermission, Effect, EffectError, EffectKind, EffectScope, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{CopyTargetSlot, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use crate::game::ability_utils::build_target_slots;
use crate::game::casting;
use crate::game::engine::{PriorityAnnouncementFacadeAccess, PriorityPrincipal};
use crate::game::game_object::{GameObject, PhaseStatus, PreparedState};
use crate::game::printed_cards::{apply_back_face_to_object, effective_prepare_face};

/// An engine-authored prepared-copy announcement for the Priority preflight.
/// The source identity remains private to the Prepare authority until the
/// Priority facade reconstructs the ordinary reducer primer.
pub(in crate::game) struct PriorityPreparedCopyAnnouncement {
    source_id: ObjectId,
}

impl PriorityPreparedCopyAnnouncement {
    fn new(source_id: ObjectId) -> Self {
        Self { source_id }
    }

    pub(in crate::game) fn source_id(
        &self,
        _access: &PriorityAnnouncementFacadeAccess,
    ) -> ObjectId {
        self.source_id
    }
}

/// Enumerates prepared copies that the current Priority holder can cast now.
/// `can_cast_prepared_copy_now` remains the canonical legality authority; the
/// ordinary reducer revalidates the fixed source when replaying on a clone.
pub(in crate::game) fn priority_prepared_copy_announcements(
    state: &GameState,
    principal: &PriorityPrincipal,
) -> Vec<PriorityPreparedCopyAnnouncement> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&source_id| {
            state.objects.get(&source_id).is_some_and(|object| {
                object.controller == principal.semantic_holder()
                    && object.prepared.is_some()
                    && can_cast_prepared_copy_now(state, principal.semantic_holder(), source_id)
            })
        })
        .map(PriorityPreparedCopyAnnouncement::new)
        .collect()
}

// The prepare cast path now materializes a short-lived exile GameObject copy of
// the prepare-spell face so the copy can reuse the normal casting pipeline
// (targets/modes/mana payment/cost modifiers).

/// Resolve the object subjects of a BecomePrepared / BecomeUnprepared effect.
///
/// Dispatches on the effect's `scope` (mirrors `suspect::resolve_object_targets`):
///
/// - [`EffectScope::All`] — the untargeted population ("Each creature you
///   control becomes prepared", Codie, Ravenous Codex): CR 115.10a, nothing is
///   a target, so every battlefield permanent matching the filter at resolution
///   is a subject. The designation gates (prepare spell, already prepared) stay
///   in `prepare_object` / `unprepare_object`.
/// - [`EffectScope::Single`] — a declared target, a self-reference or an
///   anaphor; see [`resolve_single_object_targets`].
///
/// Shared by both resolvers so the designation pair selects subjects
/// identically.
fn resolve_object_targets(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let (filter, scope) = match &ability.effect {
        Effect::BecomePrepared { target, scope } | Effect::BecomeUnprepared { target, scope } => {
            (target, *scope)
        }
        _ => return Vec::new(),
    };
    match scope {
        EffectScope::All => {
            crate::game::effects::resolved_battlefield_population_ids(state, ability, filter)
        }
        EffectScope::Single => resolve_single_object_targets(state, ability, filter),
    }
}

/// Extract object targets from `ability.targets`, or fall back to `last_created_token_ids`
/// for `TargetFilter::LastCreated`. Mirrors the pattern used by `suspect::resolve`.
///
/// A pure event-context reference (`TriggeringSource` for "that creature" /
/// "it" in a triggered ability, ...) resolves from the trigger event(s), live,
/// and only to a phased-in battlefield permanent.
fn resolve_single_object_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    if matches!(filter, TargetFilter::LastCreated) {
        return state.last_created_token_ids.clone();
    }
    // CR 722.3a: a self-referential "this creature becomes prepared" (e.g.
    // Stensian Sanguinist's combat-damage delayed trigger) carries no explicit
    // object target — the subject is the ability's own source.
    // CR 608.2c: a triggered BecomePrepared bound to the source via ParentTarget
    // with no explicit target (e.g. Tam landfall) likewise resolves to source_id.
    if matches!(filter, TargetFilter::SelfRef)
        || (ability.targets.is_empty() && matches!(filter, TargetFilter::ParentTarget))
    {
        return vec![ability.source_id];
    }
    // CR 608.2k + CR 603.2: "that creature" / "it" in a triggered ability's effect
    // names the object the trigger condition referred to, not a declared target.
    // Pure event-context refs resolve from the published trigger event(s): never
    // from `ability.targets` and never from `source_id`. With no published event
    // the result is empty. The only other referent is the shared Aura/Equipment
    // attached-host rule of the `TriggeringSource` arm (CR 301.5a + CR 303.4b),
    // which applies only when a published event names no object.
    if crate::game::targeting::is_pure_event_context_filter(filter) {
        // CR 400.7: only the incarnation the event recorded — a referent that
        // blinked since the trigger was put on the stack is a new object.
        return crate::game::targeting::resolve_event_referent_objects(
            state,
            filter,
            ability.source_id,
        )
        .into_iter()
        .filter(|&id| is_phased_in_battlefield_permanent(state, id))
        .collect();
    }
    ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// CR 722.3a + CR 110.1 + CR 702.26b: only a phased-in permanent on the
/// battlefield can gain or lose the prepared designation. An event referent
/// (CR 608.2k) can have left the battlefield (CR 400.7: it is then a new
/// object) or phased out before the trigger resolves; neither is a permanent
/// that can be affected.
fn is_phased_in_battlefield_permanent(state: &GameState, id: ObjectId) -> bool {
    state
        .objects
        .get(&id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield && obj.is_phased_in())
}

/// Returns true if the given permanent has a printed `CardLayout::Prepare(_, _)`
/// — i.e., is eligible to become prepared. Biblioplex-style "target creature
/// becomes prepared" effects no-op on creatures without a prepare face per the
/// reminder text: "Only creatures with prepare spells can become prepared."
fn has_prepare_face(state: &GameState, object_id: ObjectId) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    // The printed-cards loader populates `back_face.layout_kind` with
    // `LayoutKind::Prepare` for cards whose printed `CardLayout::Prepare(_, _)`
    // supplies the prepare-spell face. Biblioplex-style "target creature
    // becomes prepared" no-ops on creatures lacking this face.
    // CR 722.2b + CR 707.2: read through the effective-face authority so a
    // Layer-1 copy of a preparation creature has the prepare spell too.
    effective_prepare_face(obj).is_some()
}

/// CR 722.3a-c: Prepare — resolver for `Effect::BecomePrepared`.
///
/// Applies to each subject `resolve_object_targets` selects (one for the
/// `Single` scope, the whole population for `All`). Per subject: no-op (and no
/// event emitted) if it is already prepared or lacks a prepare face (Biblioplex
/// gate). Otherwise sets `prepared = Some(PreparedState)`, creates its linked
/// exile copy (CR 722.3c) and emits `BecamePrepared`.
pub fn resolve_become_prepared(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_ids = resolve_object_targets(state, ability);
    for object_id in target_ids {
        prepare_object(state, object_id, events);
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::BecomePrepared,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 722.3b: Prepare — resolver for `Effect::BecomeUnprepared`.
///
/// Applies to each subject `resolve_object_targets` selects (one for the
/// `Single` scope, the whole population for `All`). Per subject: no-op (and no
/// event emitted) if it is not prepared.
/// Otherwise clears `prepared` and emits `BecameUnprepared`. Single authority
/// for the "Doing so unprepares it." consumption — callers must not inspect
/// the field directly.
pub fn resolve_become_unprepared(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_ids = resolve_object_targets(state, ability);
    for object_id in target_ids {
        unprepare_object(state, object_id, events);
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::BecomeUnprepared,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// Direct-call variant used by `GameAction::CastPreparedCopy` handling — flips
/// `prepared` to None on a specific object, emitting the event only when the
/// toggle actually fires. Centralizes the "cast-time unprepare" rule so the
/// action handler doesn't inspect the field directly (single-authority).
pub fn unprepare_object(state: &mut GameState, object_id: ObjectId, events: &mut Vec<GameEvent>) {
    unprepare_object_inner(state, object_id, events, false);
}

fn unprepare_object_inner(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
    retain_linked_copy: bool,
) {
    let Some(obj) = state.objects.get_mut(&object_id) else {
        return;
    };
    if obj.prepared.is_none() {
        return;
    }
    obj.prepared = None;
    events.push(GameEvent::BecameUnprepared { object_id });
    if !retain_linked_copy {
        remove_linked_prepared_copy_if_idle(state, object_id);
    }
}

/// CR 722.3a: Direct-call variant that gives a specific object the prepared
/// designation, emitting `BecamePrepared` only when the toggle actually fires.
/// Mirrors [`unprepare_object`] for the opposite direction. Enforces the same
/// two gates as `resolve_become_prepared`: the object must have a prepare-spell
/// face ("A permanent can't gain this designation unless it has a prepare
/// spell") and must not already be prepared (idempotent). Single authority for
/// the "become prepared" toggle — used by the become-prepared resolver path and
/// the debug `SetPrepared` action so neither sets the field directly.
pub fn prepare_object(state: &mut GameState, object_id: ObjectId, events: &mut Vec<GameEvent>) {
    // Biblioplex gate — only creatures with prepare spells can become prepared.
    if !has_prepare_face(state, object_id) {
        return;
    }
    let Some(obj) = state.objects.get_mut(&object_id) else {
        return;
    };
    if obj.prepared.is_some() {
        return;
    }
    obj.prepared = Some(PreparedState);
    let controller = obj.controller;
    if synthesize_prepared_copy_object(state, object_id, controller).is_err() {
        state.objects.get_mut(&object_id).unwrap().prepared = None;
        return;
    }
    events.push(GameEvent::BecamePrepared { object_id });
}

/// CR 601.2c / CR 722.3c: After pushing a freshly cast prepare/paradigm copy
/// to the stack, open target selection via `WaitingFor::CopyRetarget` if the
/// copy's ability requires targets. The copy is not a copy of an
/// already-targeted spell, so each slot starts with no chosen target and
/// exposes its full legal alternatives list to the frontend/AI.
///
/// Returns `Ok(true)` if a `CopyRetarget` wait was armed, `Ok(false)` if the
/// ability has no target slots and the caller should return to Priority
/// directly. Single authority for copy-cast initial target selection —
/// shared by Prepare and Paradigm copy paths.
pub(crate) fn open_copy_target_selection(
    state: &mut GameState,
    copy_id: ObjectId,
    controller: PlayerId,
    paradigm_remaining_offers: Option<Vec<ObjectId>>,
) -> Result<bool, String> {
    // Snapshot the ability from the stack entry we just pushed so we can
    // compute slots without holding a mutable borrow across `build_target_slots`.
    let resolved = {
        let Some(entry) = state.stack.iter().find(|e| e.id == copy_id) else {
            return Err(format!("copy stack entry {copy_id:?} not found"));
        };
        let Some(ability) = entry.ability() else {
            return Ok(false);
        };
        ability.clone()
    };

    let slots = build_target_slots(state, &resolved).map_err(|e| format!("{e:?}"))?;
    if slots.is_empty() {
        return Ok(false);
    }

    // CR 601.2c / CR 722.3c: This is a cast of a fresh copy, not a copied
    // already-targeted spell. Do not seed "current" from the first legal
    // target; that would make battlefield order look like an intentional
    // target choice. The player must choose the target that completes the cast.
    let target_slots: Vec<CopyTargetSlot> = slots
        .iter()
        .map(|slot| CopyTargetSlot {
            current: None,
            legal_alternatives: slot.legal_targets.clone(),
        })
        .collect();

    state.waiting_for = WaitingFor::CopyRetarget {
        player: controller,
        copy_id,
        target_slots,
        effect_kind: crate::types::ability::EffectKind::CopySpell,
        effect_source_id: Some(copy_id),
        current_slot: 0,
        paradigm_remaining_offers,
    };
    Ok(true)
}

fn cleanup_failed_prepared_copy_cast(state: &mut GameState, copy_id: ObjectId) {
    // Defensive cleanup for any failed cast attempt after synthesizing the
    // ephemeral copy object. The predicate filters on a unique id, so this
    // removes at most ONE entry — routed through the shared stack-removal
    // authority rather than expressed as a `retain`, which would leave both
    // per-entry side tables stranded and the removal unjournaled.
    if let Some(idx) = state.stack.iter().position(|entry| entry.id == copy_id) {
        crate::game::stack::remove_nonresolving_stack_entry_at(
            state,
            idx,
            crate::game::lifecycle::DelayedTerminalDisposition::Removed,
        )
        .expect("position yielded a live stack index");
    }
    if let Some(object) = state.objects.get(&copy_id) {
        let (zone, owner) = (object.zone, object.owner);
        crate::game::zones::cease_object(state, copy_id, zone, owner);
    }
}

/// CR 722.3c: The prepared permanent this object is the linked prepare-spell
/// copy of, if any. The link counts only while the copy is in exile, the zone
/// CR 722.3c keeps it in; a copy displaced elsewhere is no longer the linked
/// copy. Single definition of the "linked prepare copy" relation.
pub(crate) fn linked_prepared_copy_source(object: &GameObject) -> Option<ObjectId> {
    object
        .prepared_copy_source
        .filter(|_| object.zone == Zone::Exile)
}

/// CR 722.3c: Whether `object` is the linked prepare-spell copy waiting in
/// exile. That copy is cast only through its prepared permanent's
/// `CastPreparedCopy` action, so the generic exile cast paths exclude it.
pub(crate) fn is_linked_prepared_copy(object: &GameObject) -> bool {
    linked_prepared_copy_source(object).is_some()
}

/// CR 722.3c + CR 702.26b + CR 702.26d: Whether `object` is a prepared
/// permanent on the battlefield — it "remains on the battlefield and has the
/// prepared designation". A phased-out permanent is treated as though it is not
/// on the battlefield, so it does not qualify while phased out. Shared by
/// linked-copy retention and the prepared-copy cast gates so the two can never
/// disagree.
pub(crate) fn is_prepared_permanent_in_play(object: &GameObject) -> bool {
    object.zone == Zone::Battlefield && !object.is_phased_out() && object.prepared.is_some()
}

/// CR 722.3c: The linked copy "remains in exile for as long as the prepared
/// permanent remains on the battlefield and has the prepared designation. This
/// is an exception to rule 704.5e." Live read of the source on every call, so a
/// departed, unprepared, phased-out or missing source never protects its copy.
pub(crate) fn is_retained_linked_prepared_copy(state: &GameState, object: &GameObject) -> bool {
    linked_prepared_copy_source(object)
        .and_then(|source_id| state.objects.get(&source_id))
        .is_some_and(is_prepared_permanent_in_play)
}

fn linked_prepared_copy_id(state: &GameState, source_id: ObjectId) -> Option<ObjectId> {
    state.objects.values().find_map(|object| {
        (linked_prepared_copy_source(object) == Some(source_id)).then_some(object.id)
    })
}

fn authorize_linked_copy_for_controller(
    state: &mut GameState,
    copy_id: ObjectId,
    controller: PlayerId,
) -> Result<(), String> {
    let copy = state
        .objects
        .get_mut(&copy_id)
        .ok_or_else(|| format!("prepared copy {copy_id:?} not found"))?;
    copy.controller = controller;
    let Some(CastingPermission::ExileWithAltCost { granted_to, .. }) =
        copy.casting_permissions.first_mut()
    else {
        return Err("prepared copy has no canonical exile casting permission".to_string());
    };
    *granted_to = Some(controller);
    Ok(())
}

fn linked_prepared_copy_if_idle(
    state: &GameState,
    source_id: ObjectId,
) -> Option<(ObjectId, Zone, PlayerId)> {
    let copy_id = linked_prepared_copy_id(state, source_id)?;
    let pending = state
        .pending_cast
        .as_deref()
        .is_some_and(|cast| cast.object_id == copy_id)
        || state
            .waiting_for
            .pending_cast_ref()
            .is_some_and(|cast| cast.object_id == copy_id);
    let object = state.objects.get(&copy_id)?;
    if pending || object.zone == Zone::Stack {
        return None;
    }
    Some((copy_id, object.zone, object.owner))
}

pub(crate) fn linked_prepared_copy_if_idle_id(
    state: &GameState,
    source_id: ObjectId,
) -> Option<ObjectId> {
    linked_prepared_copy_if_idle(state, source_id).map(|(copy_id, _, _)| copy_id)
}

pub(crate) fn remove_linked_prepared_copy_if_idle(state: &mut GameState, source_id: ObjectId) {
    let Some((copy_id, zone, owner)) = linked_prepared_copy_if_idle(state, source_id) else {
        return;
    };
    crate::game::zones::cease_object(state, copy_id, zone, owner);
}

pub(crate) fn replay_remove_linked_prepared_copy_if_idle(
    state: &mut GameState,
    source_id: ObjectId,
    cause: crate::types::resolved_commands::RulesExecutionNodeRef,
) {
    let Some((copy_id, zone, owner)) = linked_prepared_copy_if_idle(state, source_id) else {
        return;
    };
    let command = crate::types::resolved_commands::ResolvedObjectCeaseCommand {
        object: crate::types::ObjectIncarnationRef::from_object(&state.objects[&copy_id]),
        expected_zone: zone,
        owner,
        cause,
    };
    crate::game::zones::apply_resolved_object_cease(state, &command)
        .expect("validated linked idle copy must cease during zone replay");
}

/// CR 722.3c + CR 707.2: Clear, on a clone of the prepared permanent, every
/// piece of state the permanent holds that its prepare-spell copy does not
/// acquire. The copy "has only the characteristics of that permanent's prepare
/// spell", and a copy never takes the original's status, counters or stickers.
/// Single construction-site authority for the CR 722.3c copy.
///
/// Fields not cleared here are either rewritten for the copy afterwards (the
/// prepare-face characteristics via `apply_back_face_to_object`, plus the
/// identity, zone, link and permission assignments in
/// `synthesize_prepared_copy_object`) or are inert on an exile object, which no
/// layer pass re-seeds: printed front-face metadata, `intensity`,
/// `perpetual_mods`, `transformation_count`, `trigger_occurrence_state` and
/// `timestamp`.
fn strip_non_copiable_state(copy: &mut GameObject) {
    // CR 400.7: the battlefield-exit authority clears the designations, cast
    // provenance, cast-payment stamps and merge identity that belong to the
    // permanent, not to a new object built from its card.
    copy.reset_for_battlefield_exit();

    // CR 707.2 + CR 110.5: status (tapped, flipped, face down, phased out),
    // counters and stickers are not copied.
    copy.tapped = false;
    copy.flipped = false;
    copy.face_down = false;
    copy.face_down_cause = None;
    copy.phase_status = PhaseStatus::PhasedIn;
    copy.counters.clear();
    copy.stickers.clear();

    // CR 722.3c: only the prepare spell's characteristics — none of the
    // permanent's battlefield state (marked damage, attachment and pairing
    // relationships, entry bookkeeping, combat-assignment flags, face
    // orientation) and none of the alternative forms it was cast or exists in.
    copy.damage_marked = 0;
    copy.dealt_deathtouch_damage = false;
    copy.attached_to = None;
    copy.attachments.clear();
    copy.paired_with = None;
    copy.pair_controller = None;
    copy.chosen_attributes.clear();
    copy.entered_battlefield_turn = None;
    copy.summoning_sick = false;
    copy.echo_due = false;
    copy.loyalty_activations_this_turn = 0;
    copy.assigns_damage_from_toughness = false;
    copy.assigns_damage_as_though_unblocked = false;
    copy.assigns_no_combat_damage = false;
    copy.transformed = false;
    copy.modal_back_face = false;
    copy.cast_face_committed = false;
    copy.bestow_form = None;
    copy.prototype_form = None;
    copy.mutate_form = None;
    copy.cleave_form = None;
    copy.cleave_variant = None;
    copy.split_from_merge_survivor = None;
    // Layer-derived carriers of the permanent's characteristics. Layers do not
    // re-seed an exile object, so they would otherwise describe the permanent
    // instead of the prepare-face characteristics installed afterwards.
    copy.granted_abilities_from = None;
    copy.layer1_copy_effect = None;
    copy.layer1_name_origin = None;
    copy.copied_room_halves = None;
    copy.copied_prepare_face = None;
    copy.base_name_origin = None;

    // CR 707.2: the choices made while casting are copied only from an object
    // on the stack; the permanent's own cast (kicker, additional costs, modes,
    // alternative cost, timing permission, cost-paid object) is not the
    // copy's, and the copy's later cast records its own.
    copy.kickers_paid.clear();
    copy.additional_cost_payment_count = 0;
    copy.additional_cost_payments.clear();
    copy.chosen_modes.clear();
    copy.cast_variant_paid = None;
    copy.cast_timing_permission = None;
    copy.cast_cost_paid_object = None;
    copy.fused_split_spell = false;
    copy.cast_occurrence = None;

    // CR 903.3 + CR 903.9a: the commander designation is an attribute of the
    // card itself, not of a copy, so the copy in exile is never a commander
    // eligible for the command-zone return. The Oathbreaker signature-spell
    // role is likewise a role of the card.
    copy.is_commander = false;
    copy.commander_tax = None;
    copy.signature_spell = None;
}

fn synthesize_prepared_copy_object(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
) -> Result<(ObjectId, crate::types::identifiers::CardId), String> {
    // CR 722.3c: Materialize the distinct prepare-face copy in exile at the
    // prepared-state transition. Legacy saves that carry only the designation
    // may also call this lazily once to restore the required linked copy.
    let (src_clone, card_id) = {
        let Some(src_obj) = state.objects.get(&source_id) else {
            return Err(format!("source {source_id:?} not found"));
        };
        if src_obj.prepared.is_none() {
            return Err("source is not prepared".to_string());
        }
        (src_obj.clone(), src_obj.card_id)
    };
    let Some(back) = effective_prepare_face(&src_clone).cloned() else {
        return Err("source has no prepare face".to_string());
    };

    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    let mut copy_obj = src_clone;
    copy_obj.id = copy_id;
    // allow-raw-zone: prepared-copy birth in exile has no from-zone event (CR 722.3c).
    copy_obj.zone = Zone::Exile;
    // Assigned before the strip below: the battlefield-exit reset seeds
    // `base_controller` from `owner`, which must already be the copy's owner
    // (the creating controller), not the source card's owner.
    copy_obj.controller = controller;
    copy_obj.owner = controller;
    strip_non_copiable_state(&mut copy_obj);
    // CR 722.3c + CR 707.12: this is a castable copy of a card in exile, not a
    // token. A token outside the battlefield would cease to exist under CR
    // 111.7; a permanent-spell copy instead becomes a token only as it resolves
    // under CR 111.13 + CR 707.10f.
    copy_obj.is_token = false;
    copy_obj.is_copy = true;
    copy_obj.prepared = None;
    // Linked after the strip, which clears the field.
    copy_obj.prepared_copy_source = Some(source_id);
    // Do not re-enter alternative-face casting logic for this synthetic copy.
    copy_obj.back_face = None;
    apply_back_face_to_object(&mut copy_obj, back.clone());
    copy_obj.casting_permissions.clear();
    copy_obj
        .casting_permissions
        .push(CastingPermission::ExileWithAltCost {
            cost: back.mana_cost.clone(),
            // CR 118.9a: the back face's own printed cost — normal payment.
            cost_provenance: crate::types::ability::ExileGrantCostProvenance::NormalCost,
            cast_transformed: false,
            constraint: None,
            granted_to: Some(controller),
            resolution_cleanup: None,
            duration: None,
            // CR 611.2a: no duration, so no host to bind to.
            source_id: None,
            graveyard_replacement: None,
            enters_with_counter: None,
            enters_with_modifications: Vec::new(),
            mana_spend_permission: None,
            cast_cost_modifier: None,
        });
    state.objects.insert(copy_id, copy_obj);
    // allow-raw-zone: registers CR 722.3c copy birth in exile; there is no source-zone move.
    crate::game::zones::add_to_zone(state, copy_id, Zone::Exile, controller);

    Ok((copy_id, card_id))
}

fn can_cast_prepared_copy_now_in_simulated_state(
    simulated: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    // CR 722.3c + CR 702.26b + CR 702.26d: only the controller of a prepared
    // permanent on the battlefield (phased in) may cast its copy. Checked before
    // any lazy synthesis so a rejected source never gains a copy.
    if !simulated.objects.get(&source_id).is_some_and(|source| {
        is_prepared_permanent_in_play(source) && source.controller == controller
    }) {
        return false;
    }
    let original_next_object_id = simulated.next_object_id;
    let synthesized = linked_prepared_copy_id(simulated, source_id).is_none();
    let (copy_id, _) = if let Some(copy_id) = linked_prepared_copy_id(simulated, source_id) {
        (copy_id, simulated.objects[&copy_id].card_id)
    } else {
        let Ok(copy) = synthesize_prepared_copy_object(simulated, source_id, controller) else {
            return false;
        };
        copy
    };
    if authorize_linked_copy_for_controller(simulated, copy_id, controller).is_err() {
        return false;
    }
    let can_cast = casting::can_cast_object_now(simulated, controller, copy_id);
    if synthesized {
        cleanup_failed_prepared_copy_cast(simulated, copy_id);
    }
    simulated.next_object_id = original_next_object_id;
    can_cast
}

/// Fast castability probe for `GameAction::CastPreparedCopy` candidate
/// generation. Probes the linked copy, synthesizing a throwaway one in a
/// temporary game-state clone when it is missing, then asks the canonical
/// casting predicate whether that copy is castable right now.
pub fn can_cast_prepared_copy_now(
    state: &GameState,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    let mut simulated = state.clone();
    can_cast_prepared_copy_now_in_simulated_state(&mut simulated, controller, source_id)
}

/// Shared low-allocation probe for candidate enumeration loops that can reuse
/// one mutable simulation clone across many prepared permanents.
pub(crate) fn can_cast_prepared_copy_now_with_simulation(
    simulated: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    can_cast_prepared_copy_now_in_simulated_state(simulated, controller, source_id)
}

fn mark_prepare_copy_cancel_rollback(
    state: &mut GameState,
    waiting: &mut WaitingFor,
    source_id: ObjectId,
    copy_id: ObjectId,
) {
    if let Some(pending) = waiting.pending_cast_mut() {
        debug_assert_eq!(
            pending.object_id, copy_id,
            "prepare pending_cast must point at synthesized copy"
        );
        pending.cancel_restore_prepared_source = Some(source_id);
        return;
    }

    if matches!(
        waiting,
        WaitingFor::ManaPayment { .. }
            | WaitingFor::ManaSourceSelection { .. }
            | WaitingFor::PhyrexianPayment { .. }
    ) {
        if let Some(pending) = state.pending_cast.as_mut() {
            debug_assert_eq!(
                pending.object_id, copy_id,
                "prepare pending_cast must point at synthesized copy"
            );
            pending.cancel_restore_prepared_source = Some(source_id);
        }
    }
}

/// CR 722.3c + CR 707.12 + CR 601.2: Cast the retained prepare-spell copy
/// (face `b`) from exile through the normal spell-casting
/// pipeline so costs/targets/modes are handled by the same single authority as
/// every other cast.
pub fn cast_prepared_copy(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    // CR 722.3c + CR 702.26b + CR 702.26d: only the controller of a prepared
    // permanent on the battlefield (phased in) may cast its copy. Checked before
    // the lazy re-synthesis below so a rejected cast never resurrects a copy.
    if !state.objects.get(&source_id).is_some_and(|source| {
        is_prepared_permanent_in_play(source) && source.controller == controller
    }) {
        return Err("source is not a prepared permanent controlled by caster".to_string());
    }
    let (copy_id, card_id) = if let Some(copy_id) = linked_prepared_copy_id(state, source_id) {
        (copy_id, state.objects[&copy_id].card_id)
    } else {
        // Legacy saves encoded only the prepared designation. Repair that old
        // representation at the first authoritative Prepare operation.
        synthesize_prepared_copy_object(state, source_id, controller)?
    };
    authorize_linked_copy_for_controller(state, copy_id, controller)?;

    // CR 117.1d + CR 601.2g: The copy must be castable with feasible mana
    // payment options right now; otherwise this special action is illegal.
    if !casting::can_cast_object_now(state, controller, copy_id) {
        return Err("prepared copy is not castable now".to_string());
    }

    let mut waiting = match casting::handle_cast_spell(state, controller, copy_id, card_id, events)
    {
        Ok(waiting) => waiting,
        Err(err) => {
            cleanup_failed_prepared_copy_cast(state, copy_id);
            synthesize_prepared_copy_object(state, source_id, controller)?;
            return Err(format!("{err}"));
        }
    };

    // CR 601.2i + CR 722.3c: If the cast is cancelled before completion,
    // restore the source's prepared marker and leave the linked copy waiting in
    // exile (the cast announcement never moved it).
    mark_prepare_copy_cancel_rollback(state, &mut waiting, source_id, copy_id);

    // CR 722.3c: "Doing so unprepares it." Unprepare-at-cast, not at resolve —
    // so countered / fizzled copies still leave the source unprepared. Single
    // authority via `unprepare_object`.
    unprepare_object_inner(state, source_id, events, true);

    Ok(waiting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_support::legal_actions;
    use crate::game::zones::create_object;
    use crate::parser::oracle_effect::parse_effect;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CastingPermission, QuantityExpr, ReplacementDefinition,
        TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card::LayoutKind;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    // CR 722.3a-b: Parser tests for "becomes prepared" / "becomes unprepared"
    // imperative patterns.
    #[test]
    fn parse_target_becomes_prepared() {
        let effect = parse_effect("Target creature becomes prepared.");
        assert!(
            matches!(effect, Effect::BecomePrepared { .. }),
            "expected BecomePrepared, got {effect:?}"
        );
    }

    #[test]
    fn parse_target_becomes_unprepared() {
        let effect = parse_effect("Target creature becomes unprepared.");
        assert!(
            matches!(effect, Effect::BecomeUnprepared { .. }),
            "expected BecomeUnprepared, got {effect:?}"
        );
    }

    #[test]
    fn become_prepared_parent_target_with_empty_targets_prepares_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tam, Observant Sequencer".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.back_face = Some(BackFaceForTest::prepare());
        }

        let ability = ResolvedAbility::new(
            Effect::BecomePrepared {
                target: TargetFilter::ParentTarget,
                scope: EffectScope::Single,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.objects[&source].prepared.is_some(),
            "ParentTarget BecomePrepared with empty targets must prepare the source"
        );
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::BecamePrepared { object_id } if *object_id == source)
        ));
    }

    fn setup_creature(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        id
    }

    #[test]
    fn enters_prepared_replacement_marks_permanent_before_priority_actions() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = crate::types::Phase::PreCombatMain;
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quill-Blade Laureate".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&object_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.back_face = Some(BackFaceForTest::prepare());
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::BecomePrepared {
                            target: TargetFilter::SelfRef,
                            scope: EffectScope::Single,
                        },
                    ))
                    .valid_card(TargetFilter::SelfRef),
            );
        }
        state.stack.push_back(StackEntry {
            id: object_id,
            source_id: object_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(state.objects[&object_id].zone, Zone::Battlefield);
        assert!(state.objects[&object_id].prepared.is_some());
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::BecamePrepared { object_id: id } if *id == object_id)
        ));
        let copy = {
            let copies = linked_copies(&state, object_id);
            assert_eq!(
                copies.len(),
                1,
                "CR 722.3c: entering prepared creates one copy"
            );
            copies[0]
        };

        // CR 722.3c + CR 704.5e: the copy made as the permanent entered prepared
        // survives the state-based actions checked before priority.
        crate::game::sba::check_state_based_actions(&mut state, &mut Vec::new());
        assert_eq!(linked_copies(&state, object_id), vec![copy]);
        assert!(state.exile.contains(&copy));

        let actions = legal_actions(&state);
        assert!(actions.iter().any(
            |action| matches!(action, GameAction::CastPreparedCopy { source } if *source == object_id)
        ));
    }

    #[test]
    fn effect_zone_move_enters_prepared_replacement_marks_permanent() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.phase = crate::types::Phase::PreCombatMain;
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Quill-Blade Laureate".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&object_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.back_face = Some(BackFaceForTest::prepare());
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::BecomePrepared {
                            target: TargetFilter::SelfRef,
                            scope: EffectScope::Single,
                        },
                    ))
                    .valid_card(TargetFilter::SelfRef),
            );
        }

        let mut events = Vec::new();
        let _ = crate::game::effects::change_zone::execute_zone_move(
            &mut state,
            object_id,
            Zone::Hand,
            Zone::Battlefield,
            ObjectId(999),
            None,
            false,
            crate::types::zones::EtbTapState::Unspecified,
            false,
            None,
            &[],
            None,
            false,
            None,
            None,
            &mut events,
        );

        assert_eq!(state.objects[&object_id].zone, Zone::Battlefield);
        assert!(state.objects[&object_id].prepared.is_some());
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::BecamePrepared { object_id: id } if *id == object_id)
        ));

        let actions = legal_actions(&state);
        assert!(actions.iter().any(
            |action| matches!(action, GameAction::CastPreparedCopy { source } if *source == object_id)
        ));
    }

    #[test]
    fn become_prepared_noop_without_prepare_face() {
        // Biblioplex gate — a creature that isn't a prepare-family card must
        // not become prepared even if targeted.
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::BecomePrepared {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(
            obj.prepared.is_none(),
            "creature without prepare face must not become prepared"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::BecamePrepared { .. })),
            "no BecamePrepared event on no-op"
        );
    }

    #[test]
    fn become_unprepared_cleans_linked_copy_and_is_idempotent() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.objects.get_mut(&id).unwrap().back_face = Some(BackFaceForTest::prepare());
        prepare_object(&mut state, id, &mut Vec::new());
        let copy_id = linked_prepared_copy_id(&state, id).expect("prepare creates its exile copy");

        let ability = ResolvedAbility::new(
            Effect::BecomeUnprepared {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_unprepared(&mut state, &ability, &mut events).unwrap();

        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::BecameUnprepared { object_id } if *object_id == id)
            ),
            "the resolver emits the designation change"
        );
        assert!(state.objects[&id].prepared.is_none());
        assert!(!state.objects.contains_key(&copy_id));
        assert!(!state.exile.contains(&copy_id));

        let mut second_events = Vec::new();
        resolve_become_unprepared(&mut state, &ability, &mut second_events).unwrap();
        assert!(
            !second_events
                .iter()
                .any(|event| matches!(event, GameEvent::BecameUnprepared { .. })),
            "an already-unprepared object emits no second designation event"
        );
    }

    #[test]
    fn unprepare_object_flips_and_emits_event() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.objects.get_mut(&id).unwrap().prepared = Some(PreparedState);

        let mut events = Vec::new();
        unprepare_object(&mut state, id, &mut events);

        assert!(state.objects[&id].prepared.is_none());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BecameUnprepared { object_id } if *object_id == id)));

        // Idempotency — second call must not re-emit.
        let mut events2 = Vec::new();
        unprepare_object(&mut state, id, &mut events2);
        assert!(events2.is_empty());
    }

    #[test]
    fn prepare_object_flips_and_emits_when_prepare_face_present() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.objects.get_mut(&id).unwrap().back_face = Some(BackFaceForTest::prepare());

        let mut events = Vec::new();
        prepare_object(&mut state, id, &mut events);

        assert!(state.objects[&id].prepared.is_some());
        let copy_id = linked_prepared_copy_id(&state, id)
            .expect("CR 722.3c creates the linked prepare-face copy immediately");
        assert_eq!(state.objects[&copy_id].zone, Zone::Exile);
        assert!(state.exile.contains(&copy_id));
        assert!(state.objects[&copy_id].is_copy);
        assert!(!state.objects[&copy_id].is_token);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BecamePrepared { object_id } if *object_id == id)));

        // Idempotency — second call must not re-emit.
        let mut events2 = Vec::new();
        prepare_object(&mut state, id, &mut events2);
        assert!(events2.is_empty());
        assert_eq!(linked_prepared_copy_id(&state, id), Some(copy_id));
    }

    /// CR 722.2b + CR 707.2: a permanent that is a Layer-1 copy of a preparation
    /// creature has the prepare spell (gate and linked copy come from the
    /// copied face); a preparation creature copying something else has none.
    #[test]
    fn layer1_copy_of_a_preparation_creature_can_become_prepared() {
        let copy_effect = crate::types::ability::CopyEffectInstanceRef {
            continuous_effect_id: 9,
            modification_index: 0,
        };
        let mut state = GameState::new_two_player(42);
        let clone = setup_creature(&mut state);
        {
            let obj = state.objects.get_mut(&clone).unwrap();
            obj.layer1_copy_effect = Some(copy_effect);
            obj.copied_prepare_face = Some(std::sync::Arc::new(BackFaceForTest::prepare()));
        }
        let mut events = Vec::new();
        prepare_object(&mut state, clone, &mut events);
        assert!(state.objects[&clone].prepared.is_some());
        let copy_id = linked_prepared_copy_id(&state, clone)
            .expect("CR 722.3c: the copy's linked prepare-face copy exists");
        assert_eq!(state.objects[&copy_id].zone, Zone::Exile);

        let loses = setup_creature(&mut state);
        {
            let obj = state.objects.get_mut(&loses).unwrap();
            obj.back_face = Some(BackFaceForTest::prepare());
            obj.layer1_copy_effect = Some(copy_effect);
        }
        let mut events = Vec::new();
        prepare_object(&mut state, loses, &mut events);
        assert!(
            state.objects[&loses].prepared.is_none(),
            "a copy of a creature with no prepare spell has none, whatever it printed"
        );
    }

    #[test]
    fn unprepare_and_battlefield_exit_cease_only_the_linked_idle_copy() {
        let mut state = GameState::new_two_player(42);
        let source = setup_creature(&mut state);
        state.objects.get_mut(&source).unwrap().back_face = Some(BackFaceForTest::prepare());
        let unrelated = create_object(
            &mut state,
            CardId(9_999),
            PlayerId(0),
            "Unrelated exile card".to_string(),
            Zone::Exile,
        );

        prepare_object(&mut state, source, &mut Vec::new());
        let first_copy = linked_prepared_copy_id(&state, source).unwrap();
        unprepare_object(&mut state, source, &mut Vec::new());
        assert!(!state.objects.contains_key(&first_copy));
        assert!(!state.exile.contains(&first_copy));
        assert!(state.objects.contains_key(&unrelated));

        prepare_object(&mut state, source, &mut Vec::new());
        let second_copy = linked_prepared_copy_id(&state, source).unwrap();
        crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut Vec::new());
        assert!(!state.objects.contains_key(&second_copy));
        assert!(!state.exile.contains(&second_copy));
        assert!(state.objects.contains_key(&unrelated));
    }

    /// CR 722.3c + CR 704.5e + CR 702.26b/d: the linked-copy retention building
    /// block. Every false row is built from the same freshly prepared pair as
    /// the true row and changes exactly one input.
    #[test]
    fn linked_copy_retention_requires_prepared_source_in_play_and_copy_in_exile() {
        use crate::game::game_object::{PhaseOutCause, PhaseStatus};

        fn prepared_pair() -> (GameState, ObjectId, ObjectId) {
            let mut state = GameState::new_two_player(42);
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.phase = Phase::PreCombatMain;
            state.waiting_for = WaitingFor::Priority {
                player: PlayerId(0),
            };
            let source = setup_creature(&mut state);
            state.objects.get_mut(&source).unwrap().back_face = Some(BackFaceForTest::prepare());
            prepare_object(&mut state, source, &mut Vec::new());
            let copy = linked_prepared_copy_id(&state, source).expect("prepare creates the copy");
            (state, source, copy)
        }
        fn retained(state: &GameState, copy: ObjectId) -> bool {
            is_retained_linked_prepared_copy(state, &state.objects[&copy])
        }

        // True row: prepared source on the battlefield, phased in, copy in exile.
        let (state, source, copy) = prepared_pair();
        assert_eq!(
            linked_prepared_copy_source(&state.objects[&copy]),
            Some(source)
        );
        assert!(retained(&state, copy));
        assert!(can_cast_prepared_copy_now(&state, PlayerId(0), source));

        // Source loses the designation (field cleared, no eager cleanup).
        let (mut state, source, copy) = prepared_pair();
        state.objects.get_mut(&source).unwrap().prepared = None;
        assert!(!retained(&state, copy));

        // Source is no longer on the battlefield.
        let (mut state, source, copy) = prepared_pair();
        state.objects.get_mut(&source).unwrap().zone = Zone::Graveyard;
        assert!(!retained(&state, copy));

        // Source is phased out: treated as not on the battlefield, so the copy
        // is not retained and the copy can't be cast; phased back in, both hold.
        let (mut state, source, copy) = prepared_pair();
        state.objects.get_mut(&source).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        assert!(!retained(&state, copy));
        assert!(!can_cast_prepared_copy_now(&state, PlayerId(0), source));
        state.objects.get_mut(&source).unwrap().phase_status = PhaseStatus::PhasedIn;
        assert!(retained(&state, copy));
        assert!(can_cast_prepared_copy_now(&state, PlayerId(0), source));

        // The link names a missing object.
        let (mut state, _source, copy) = prepared_pair();
        state.objects.get_mut(&copy).unwrap().prepared_copy_source = Some(ObjectId(9_999));
        assert!(!retained(&state, copy));

        // The copy is not in exile: the link no longer counts.
        let (mut state, _source, copy) = prepared_pair();
        state.objects.get_mut(&copy).unwrap().zone = Zone::Graveyard;
        assert_eq!(linked_prepared_copy_source(&state.objects[&copy]), None);
        assert!(!retained(&state, copy));

        // An unlinked copy in exile.
        let (mut state, _source, copy) = prepared_pair();
        state.objects.get_mut(&copy).unwrap().prepared_copy_source = None;
        assert_eq!(linked_prepared_copy_source(&state.objects[&copy]), None);
        assert!(!retained(&state, copy));
    }

    #[test]
    fn prepared_source_control_change_authorizes_current_controller_only() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        let source = setup_creature(&mut state);
        state.objects.get_mut(&source).unwrap().back_face = Some(BackFaceForTest::prepare());
        prepare_object(&mut state, source, &mut Vec::new());
        let copy = linked_prepared_copy_id(&state, source).unwrap();
        assert_eq!(state.objects[&copy].owner, PlayerId(0));

        state.objects.get_mut(&source).unwrap().controller = PlayerId(1);
        assert!(!can_cast_prepared_copy_now(&state, PlayerId(0), source));
        assert!(can_cast_prepared_copy_now(&state, PlayerId(1), source));
        cast_prepared_copy(&mut state, source, PlayerId(1), &mut Vec::new()).unwrap();
        assert_eq!(state.objects[&copy].controller, PlayerId(1));
        assert_eq!(state.objects[&copy].owner, PlayerId(0));
    }

    /// CR 722.3c + CR 111.2 / CR 707.10 by analogy: the copy's controller (the
    /// prepared permanent's controller) creates it and owns it; its owner-seeded
    /// `base_controller` names that player, never the source card's owner.
    #[test]
    fn prepared_copy_of_a_stolen_permanent_is_owned_and_controlled_by_its_controller() {
        let mut state = GameState::new_two_player(42);
        let source = setup_creature(&mut state);
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.back_face = Some(BackFaceForTest::prepare());
            obj.controller = PlayerId(1);
            obj.base_controller = Some(PlayerId(1));
        }
        assert_eq!(state.objects[&source].owner, PlayerId(0));

        prepare_object(&mut state, source, &mut Vec::new());

        let copy = linked_prepared_copy_id(&state, source).expect("prepare creates the copy");
        let copy = &state.objects[&copy];
        assert_eq!(copy.owner, PlayerId(1));
        assert_eq!(copy.controller, PlayerId(1));
        assert_eq!(copy.base_controller, Some(PlayerId(1)));
        assert_eq!(
            state.objects[&source].owner,
            PlayerId(0),
            "reach guard: the source card is still owned by P0"
        );
    }

    /// CR 707.2 + CR 110.5 + CR 722.3c + CR 903.3: the copy-hygiene building
    /// block resets every permanent-only field it owns, whatever the clone
    /// carried in.
    #[test]
    fn strip_non_copiable_state_resets_every_permanent_only_field() {
        use crate::game::game_object::{AttachTarget, PhaseOutCause, SignatureSpellState};
        use crate::types::ability::{
            CastTimingPermission, CastVariantPaid, ChosenAttribute, FaceDownCause, KickerVariant,
        };
        use crate::types::counter::CounterType;
        use crate::types::mana::ManaColor;
        use crate::types::stickers::{AppliedSticker, StickerLocator};

        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let mut obj = state.objects[&id].clone();
        obj.counters.insert(CounterType::Plus1Plus1, 2);
        obj.stickers.push(AppliedSticker::Name {
            locator: StickerLocator {
                sheet: "Sheet".into(),
                index: 1,
            },
            text: "Name".into(),
            position: 0,
            timestamp: 1,
        });
        obj.damage_marked = 3;
        obj.dealt_deathtouch_damage = true;
        obj.attachments.push(ObjectId(77));
        obj.attached_to = Some(AttachTarget::Object(ObjectId(78)));
        obj.paired_with = Some(ObjectId(79));
        obj.pair_controller = Some(PlayerId(0));
        obj.tapped = true;
        obj.flipped = true;
        obj.face_down = true;
        obj.face_down_cause = Some(FaceDownCause::Morph);
        obj.phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));
        obj.entered_battlefield_turn = Some(3);
        obj.summoning_sick = true;
        obj.echo_due = true;
        obj.transformed = true;
        obj.modal_back_face = true;
        obj.monstrous = true;
        obj.is_suspected = true;
        obj.goaded_by.insert(PlayerId(1));
        obj.kickers_paid.push(KickerVariant::First);
        obj.additional_cost_payment_count = 2;
        obj.chosen_modes.push(1);
        obj.cast_variant_paid = Some((CastVariantPaid::Evoke, 3));
        obj.cast_timing_permission = Some((CastTimingPermission::AsThoughHadFlash, 3));
        obj.fused_split_spell = true;
        obj.mana_spent_to_cast_amount = 4;
        obj.is_commander = true;
        obj.commander_tax = Some(2);
        obj.signature_spell = Some(SignatureSpellState {});
        obj.prepared = Some(PreparedState);
        obj.prepared_copy_source = Some(ObjectId(80));

        strip_non_copiable_state(&mut obj);

        assert!(obj.counters.is_empty());
        assert!(obj.stickers.is_empty());
        assert_eq!(obj.damage_marked, 0);
        assert!(!obj.dealt_deathtouch_damage);
        assert!(obj.attachments.is_empty());
        assert_eq!(obj.attached_to, None);
        assert_eq!(obj.paired_with, None);
        assert_eq!(obj.pair_controller, None);
        assert!(!obj.tapped);
        assert!(!obj.flipped);
        assert!(!obj.face_down);
        assert_eq!(obj.face_down_cause, None);
        assert_eq!(obj.phase_status, PhaseStatus::PhasedIn);
        assert!(obj.chosen_attributes.is_empty());
        assert_eq!(obj.entered_battlefield_turn, None);
        assert!(!obj.summoning_sick);
        assert!(!obj.echo_due);
        assert!(!obj.transformed);
        assert!(!obj.modal_back_face);
        assert!(!obj.monstrous);
        assert!(!obj.is_suspected);
        assert!(obj.goaded_by.is_empty());
        assert!(obj.kickers_paid.is_empty());
        assert_eq!(obj.additional_cost_payment_count, 0);
        assert!(obj.chosen_modes.is_empty());
        assert_eq!(obj.cast_variant_paid, None);
        assert_eq!(obj.cast_timing_permission, None);
        assert!(!obj.fused_split_spell);
        assert_eq!(obj.mana_spent_to_cast_amount, 0);
        assert!(!obj.is_commander);
        assert_eq!(obj.commander_tax, None);
        assert_eq!(obj.signature_spell, None);
        assert!(!obj.uses_command_zone_rules());
        assert_eq!(obj.prepared, None);
        assert_eq!(obj.prepared_copy_source, None);
    }

    #[test]
    fn prepare_object_noop_without_prepare_face() {
        // CR 722.3a Biblioplex gate — an object with no prepare-spell face
        // can't gain the prepared designation (matches the debug SetPrepared
        // path's single-authority guarantee).
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let mut events = Vec::new();
        prepare_object(&mut state, id, &mut events);

        assert!(state.objects[&id].prepared.is_none());
        assert!(events.is_empty());
    }

    // CR 707.10c: `open_copy_target_selection` detects whether the copy's
    // spell ability requires targets and, if so, arms `CopyRetarget` with
    // seeded targets + legal alternatives. Returns false (no-op) for copies
    // without target slots. Shared by Prepare and Paradigm copy paths.
    #[test]
    fn open_copy_target_selection_no_slots_returns_false() {
        use crate::types::ability::{QuantityExpr, ResolvedAbility};
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let copy_id = ObjectId(200);
        // Build a minimal stack entry with a no-target effect ("Draw a card").
        let resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            copy_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: copy_id,
            source_id: copy_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let armed = open_copy_target_selection(&mut state, copy_id, PlayerId(0), None).unwrap();
        assert!(!armed, "no target slots → no CopyRetarget");
        // WaitingFor should remain unchanged (default Priority here).
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::CopyRetarget { .. }
        ));
    }

    #[test]
    fn open_copy_target_selection_arms_copy_retarget_with_legal_alternatives() {
        use crate::types::ability::{QuantityExpr, ResolvedAbility, TypedFilter};
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        // Legal target: a creature on battlefield.
        let creature_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&creature_id).unwrap().base_power = Some(1);
        state.objects.get_mut(&creature_id).unwrap().base_toughness = Some(1);
        state.objects.get_mut(&creature_id).unwrap().power = Some(1);
        state.objects.get_mut(&creature_id).unwrap().toughness = Some(1);

        let copy_id = ObjectId(999);
        // Copy's ability requires targeting a creature.
        let resolved = ResolvedAbility::new(
            Effect::DealDamage {
                target: TargetFilter::Typed(TypedFilter::creature()),
                amount: QuantityExpr::Fixed { value: 2 },
                damage_source: None,
                excess: None,
            },
            Vec::new(),
            copy_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: copy_id,
            source_id: copy_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(42),
                ability: Some(Box::new(resolved)),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        // GameObject backing the stack entry.
        let _ = create_object(
            &mut state,
            CardId(42),
            PlayerId(0),
            "Copy".to_string(),
            Zone::Stack,
        );

        let armed = open_copy_target_selection(&mut state, copy_id, PlayerId(0), None).unwrap();
        assert!(armed, "target slot → arms CopyRetarget");
        match &state.waiting_for {
            WaitingFor::CopyRetarget {
                player,
                copy_id: cid,
                target_slots,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*cid, copy_id);
                assert_eq!(target_slots.len(), 1);
                assert!(
                    target_slots[0]
                        .legal_alternatives
                        .contains(&TargetRef::Object(creature_id)),
                    "legal alternatives must include battlefield creature"
                );
                assert_eq!(
                    target_slots[0].current, None,
                    "freshly cast copy should not preselect a target"
                );
            }
            other => panic!("expected CopyRetarget, got {other:?}"),
        }

        // Verify the stack entry's ability targets remain empty until the
        // player actually chooses a target.
        let entry_targets = state
            .stack
            .iter()
            .find(|e| e.id == copy_id)
            .and_then(|e| e.ability())
            .map(|a| a.targets.clone())
            .unwrap_or_default();
        assert!(
            entry_targets.is_empty(),
            "stack entry must not seed a target"
        );

        let legal_actions = legal_actions(&state);
        assert!(
            !legal_actions
                .iter()
                .any(|action| matches!(action, GameAction::KeepAllCopyTargets)),
            "freshly cast copy has no current target to keep"
        );
        assert!(
            !legal_actions
                .iter()
                .any(|action| matches!(action, GameAction::ChooseTarget { target: None })),
            "freshly cast copy has no current target to keep for this slot"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature_id)),
            },
        )
        .expect("choosing a legal target should complete copy target selection");

        let chosen_targets = state
            .stack
            .iter()
            .find(|e| e.id == copy_id)
            .and_then(|e| e.ability())
            .map(|a| a.targets.clone())
            .unwrap_or_default();
        assert_eq!(chosen_targets, vec![TargetRef::Object(creature_id)]);
    }

    #[test]
    fn become_prepared_idempotent_when_already_prepared() {
        // Direct assert of the idempotency branch: resolver must not re-emit
        // the event when target is already prepared.
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.objects.get_mut(&id).unwrap().prepared = Some(PreparedState);

        let ability = ResolvedAbility::new(
            Effect::BecomePrepared {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::BecamePrepared { .. })),
            "no BecamePrepared event when already prepared"
        );
    }

    // Test gap #3: Single-copy invariant under multiple triggers. A second call
    // to `resolve_become_prepared` on an already-prepared source must be a
    // no-op — the flag is unit-typed so "already prepared" is semantically
    // idempotent. Complements the existing `become_prepared_idempotent_when_
    // already_prepared` test by exercising the resolve-twice loop path: two
    // sequential resolver invocations must produce exactly one event total.
    #[test]
    fn resolve_become_prepared_twice_emits_event_only_once() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        // Give the creature a Prepare back face so the gate passes.
        state.objects.get_mut(&id).unwrap().back_face = Some(BackFaceForTest::prepare());

        let ability = ResolvedAbility::new(
            Effect::BecomePrepared {
                target: TargetFilter::Any,
                scope: EffectScope::Single,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        let flip_events = events
            .iter()
            .filter(|e| matches!(e, GameEvent::BecamePrepared { .. }))
            .count();
        assert_eq!(flip_events, 1, "second resolve must no-op");
        assert!(state.objects[&id].prepared.is_some());
    }

    // Test gap #7: Battlefield-exit must clear the `prepared` flag via
    // `reset_for_battlefield_exit`. The prepared state is a property of the
    // permanent and must not carry across zone changes (CR 400.7 new-object
    // identity on zone transition).
    #[test]
    fn battlefield_exit_clears_prepared_flag() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.objects.get_mut(&id).unwrap().prepared = Some(PreparedState);
        assert!(state.objects[&id].prepared.is_some());

        state
            .objects
            .get_mut(&id)
            .unwrap()
            .reset_for_battlefield_exit();

        assert!(
            state.objects[&id].prepared.is_none(),
            "battlefield exit must clear prepared state"
        );
    }

    // Test gap #2 (partial — pre-stack level): cast-time unprepare is
    // authoritative. `unprepare_object` is the single call site invoked by
    // `cast_prepared_copy`; calling it leaves `prepared = None` even when no
    // resolution event has happened yet. This is what makes counter-the-copy
    // still leave the source unprepared: the unprepare fired at cast time,
    // before the counter could interact with the stack copy.
    #[test]
    fn cast_time_unprepare_happens_before_resolution() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.objects.get_mut(&id).unwrap().prepared = Some(PreparedState);
        let mut events = Vec::new();
        unprepare_object(&mut state, id, &mut events);
        // After cast-time unprepare, source is no longer prepared regardless
        // of what happens to the copy on the stack.
        assert!(state.objects[&id].prepared.is_none());
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn cast_prepared_copy_requires_payable_mana_cost() {
        let mut state = GameState::new_two_player(42);
        let source_id = setup_creature(&mut state);
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.prepared = Some(PreparedState);
            // CR 722.3c + CR 118.9a: prepared-copy casting must use the
            // prepare face's mana cost, not any stale free-cast permission that
            // happened to be stored on the battlefield source before cloning.
            source
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    source_id: None,
                    cost_provenance: crate::types::ability::ExileGrantCostProvenance::Alternative,
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: None,
                    duration: None,
                    graveyard_replacement: None,
                    enters_with_counter: None,
                    enters_with_modifications: Vec::new(),
                    mana_spend_permission: None,
                    cast_cost_modifier: None,
                });
            source.back_face = Some(BackFaceForTest::prepare_with_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            }));
        }

        let mut events = Vec::new();
        let result = cast_prepared_copy(&mut state, source_id, PlayerId(0), &mut events);

        assert!(
            result.is_err(),
            "prepared copy cast must fail when mana cost is not payable"
        );
        assert!(
            state.objects[&source_id].prepared.is_some(),
            "source remains prepared when cast cannot start"
        );
        let retained = linked_prepared_copy_id(&state, source_id)
            .expect("legacy designation is repaired with a retained exile copy");
        assert_eq!(state.objects[&retained].zone, Zone::Exile);
        assert!(state.exile.contains(&retained));
        assert!(
            state.stack.is_empty(),
            "failed cast must not leave stack entries"
        );
    }

    #[test]
    fn synthesized_prepared_permanent_copy_becomes_token_only_on_resolution() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let source_id = setup_creature(&mut state);
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.prepared = Some(PreparedState);
            source.back_face = Some(BackFaceForTest::prepare_permanent());
        }
        assert!(linked_prepared_copy_id(&state, source_id).is_none());

        let waiting = cast_prepared_copy(&mut state, source_id, PlayerId(0), &mut Vec::new())
            .expect("the zero-cost prepared permanent copy is cast");
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
        let copy_id = state.stack.back().expect("prepared copy is on stack").id;
        let stack_copy = &state.objects[&copy_id];
        assert_eq!(stack_copy.zone, Zone::Stack);
        assert!(stack_copy.is_copy, "the synthesized object is a card copy");
        assert!(
            !stack_copy.is_token,
            "CR 722.3c + CR 707.12: the synthesized exile/stack copy is not yet a token"
        );

        crate::game::stack::resolve_top(&mut state, &mut Vec::new());

        let permanent = &state.objects[&copy_id];
        assert_eq!(permanent.zone, Zone::Battlefield);
        assert!(
            permanent.is_token,
            "CR 111.13 + CR 707.10f: the resolving permanent copy becomes a token"
        );
        assert!(
            !permanent.is_copy,
            "CR 111.13 + CR 707.10f: the battlefield token is no longer a spell copy"
        );
        assert_eq!(permanent.prepared_copy_source, None);
        assert!(linked_prepared_copy_id(&state, source_id).is_none());
    }

    #[test]
    fn cancel_pending_prepare_copy_restores_prepared_source() {
        let mut state = GameState::new_two_player(42);
        let source_id = setup_creature(&mut state);
        state.objects.get_mut(&source_id).unwrap().prepared = Some(PreparedState);

        // Synthetic prepare-copy object announced on stack.
        let copy_id = create_object(
            &mut state,
            CardId(77),
            PlayerId(0),
            "Prepared Copy".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&copy_id).unwrap().is_token = false;
        state.objects.get_mut(&copy_id).unwrap().is_copy = true;
        state
            .objects
            .get_mut(&copy_id)
            .unwrap()
            .prepared_copy_source = Some(source_id);
        state.stack.push_back(StackEntry {
            id: copy_id,
            source_id: copy_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(77),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Cast-time unprepare happened before cancellation.
        state.objects.get_mut(&source_id).unwrap().prepared = None;

        let mut pending = crate::types::game_state::PendingCast::new(
            copy_id,
            CardId(77),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                copy_id,
                PlayerId(0),
            ),
            ManaCost::NoCost,
        );
        pending.cancel_restore_prepared_source = Some(source_id);

        crate::game::casting::handle_cancel_cast(&mut state, &pending, &mut Vec::new());

        assert!(
            state.objects[&source_id].prepared.is_some(),
            "cancelled cast must restore source prepared state"
        );
        assert!(
            state.objects.contains_key(&copy_id),
            "cancelled cast must retain the linked prepare copy in exile"
        );
        assert_eq!(state.objects[&copy_id].zone, Zone::Exile);
        assert!(state.exile.contains(&copy_id));
        assert_eq!(linked_prepared_copy_id(&state, source_id), Some(copy_id));
        assert!(
            state.stack.iter().all(|entry| entry.id != copy_id),
            "cancelled cast must remove stack placeholder for synthesized copy"
        );
    }

    #[test]
    fn prepared_sorcery_not_castable_during_opponents_main_phase() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let source_id = setup_creature(&mut state);
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.prepared = Some(PreparedState);
            source.back_face = Some(BackFaceForTest::prepare_with_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            }));
        }
        state.players[0].mana_pool.mana.push(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));

        assert!(
            !can_cast_prepared_copy_now(&state, PlayerId(0), source_id),
            "prepared sorcery must not be castable during the opponent's main phase"
        );
    }

    #[test]
    fn prepared_sorcery_requires_payable_mana_even_at_sorcery_speed() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let source_id = setup_creature(&mut state);
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.prepared = Some(PreparedState);
            source.back_face = Some(BackFaceForTest::prepare_with_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            }));
        }

        assert!(
            !can_cast_prepared_copy_now(&state, PlayerId(0), source_id),
            "prepared copy must not be castable without payable mana"
        );
    }

    // ---- EffectScope::All: "Each creature you control becomes (un)prepared" ----

    /// A battlefield creature controlled (and owned) by `controller`, with or
    /// without a prepare spell (CR 722.2a).
    fn add_prepare_creature(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        with_face: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        if with_face {
            obj.back_face = Some(BackFaceForTest::prepare());
        }
        id
    }

    /// The population filter of Codie, Ravenous Codex: "each creature you control".
    fn creatures_you_control() -> TargetFilter {
        TargetFilter::Typed(
            crate::types::ability::TypedFilter::creature()
                .controller(crate::types::ability::ControllerRef::You),
        )
    }

    fn mass_prepare(scope: EffectScope) -> Effect {
        Effect::BecomePrepared {
            target: creatures_you_control(),
            scope,
        }
    }

    fn mass_unprepare(scope: EffectScope) -> Effect {
        Effect::BecomeUnprepared {
            target: creatures_you_control(),
            scope,
        }
    }

    fn became_prepared(events: &[GameEvent]) -> Vec<ObjectId> {
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::BecamePrepared { object_id } => Some(*object_id),
                _ => None,
            })
            .collect()
    }

    fn became_unprepared(events: &[GameEvent]) -> Vec<ObjectId> {
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::BecameUnprepared { object_id } => Some(*object_id),
                _ => None,
            })
            .collect()
    }

    /// CR 722.3c: every linked exile copy of `source`'s prepare spell.
    fn linked_copies(state: &GameState, source: ObjectId) -> Vec<ObjectId> {
        state
            .objects
            .values()
            .filter(|object| {
                object.zone == Zone::Exile && object.prepared_copy_source == Some(source)
            })
            .map(|object| object.id)
            .collect()
    }

    /// CR 115.10a: the mass scope names no target, so `target_filter()` is `None`
    /// and no slot is built; the single scope with the same filter builds one slot
    /// (reach guard: the slot builder does build slots for this effect).
    #[test]
    fn become_prepared_scope_gates_target_slot() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
        let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
        for (all, single) in [
            (
                mass_prepare(EffectScope::All),
                mass_prepare(EffectScope::Single),
            ),
            (
                mass_unprepare(EffectScope::All),
                mass_unprepare(EffectScope::Single),
            ),
        ] {
            assert!(all.target_filter().is_none(), "{all:?}");
            let slots = build_target_slots(
                &state,
                &ResolvedAbility::new(all.clone(), vec![], source, PlayerId(0)),
            )
            .unwrap();
            assert!(slots.is_empty(), "{all:?} must build no target slot");

            assert_eq!(single.target_filter(), Some(&creatures_you_control()));
            let slots = build_target_slots(
                &state,
                &ResolvedAbility::new(single.clone(), vec![], source, PlayerId(0)),
            )
            .unwrap();
            assert_eq!(slots.len(), 1, "{single:?} builds exactly one slot");
            assert!(slots[0].legal_targets.contains(&TargetRef::Object(a)));
        }
    }

    /// CR 722.3a + CR 722.3c + CR 608.2h: every creature the ability's controller
    /// controls that has a prepare spell becomes prepared, with one linked exile
    /// copy each; the population is read at resolution.
    #[test]
    fn become_prepared_all_prepares_each_controlled_eligible_creature() {
        let mut state = GameState::new_two_player(42);
        // The source stands in for Codie: a creature with no prepare spell.
        let source = add_prepare_creature(&mut state, PlayerId(0), "Codie stand-in", false);
        let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
        let b = add_prepare_creature(&mut state, PlayerId(0), "B", true);
        let c = add_prepare_creature(&mut state, PlayerId(0), "Faceless C", false);
        let d = add_prepare_creature(&mut state, PlayerId(1), "Opponent D", true);
        let ability =
            ResolvedAbility::new(mass_prepare(EffectScope::All), vec![], source, PlayerId(0));
        // Created after the ability was built, before it resolves.
        let f = add_prepare_creature(&mut state, PlayerId(0), "Late F", true);

        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        let mut prepared = became_prepared(&events);
        prepared.sort_by_key(|id| id.0);
        assert_eq!(prepared, vec![a, b, f]);
        for id in [a, b, f] {
            assert!(state.objects[&id].prepared.is_some());
            let copies = linked_copies(&state, id);
            assert_eq!(copies.len(), 1, "CR 722.3c: one linked copy");
            assert_eq!(state.objects[&copies[0]].controller, PlayerId(0));
        }
        for id in [c, source, d] {
            assert!(state.objects[&id].prepared.is_none());
            assert!(linked_copies(&state, id).is_empty());
        }
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::BecomePrepared,
                ..
            }
        )));
    }

    /// CR 722.3a: an already-prepared creature can't gain the designation again —
    /// no second event and no second linked copy.
    #[test]
    fn become_prepared_all_skips_already_prepared() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
        let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
        let b = add_prepare_creature(&mut state, PlayerId(0), "B", true);
        prepare_object(&mut state, b, &mut Vec::new());
        let b_copy = linked_copies(&state, b);
        assert_eq!(b_copy.len(), 1);

        let ability =
            ResolvedAbility::new(mass_prepare(EffectScope::All), vec![], source, PlayerId(0));
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        assert_eq!(became_prepared(&events), vec![a]);
        assert!(state.objects[&a].prepared.is_some());
        assert_eq!(linked_copies(&state, b), b_copy);
    }

    /// CR 115.10a: object targets propagated through a chain are not the mass
    /// subject — the population is read from the filter alone. Paired: the same
    /// targets under `Single` do act on the foreign creature.
    #[test]
    fn become_prepared_all_ignores_foreign_targets() {
        let build = || {
            let mut state = GameState::new_two_player(42);
            let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
            let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
            let b = add_prepare_creature(&mut state, PlayerId(0), "B", true);
            let c = add_prepare_creature(&mut state, PlayerId(0), "Faceless C", false);
            let d = add_prepare_creature(&mut state, PlayerId(1), "Opponent D", true);
            (state, source, a, b, c, d)
        };

        let (mut state, source, a, b, c, d) = build();
        let foreign = vec![TargetRef::Object(d), TargetRef::Object(c)];
        let ability = ResolvedAbility::new(
            mass_prepare(EffectScope::All),
            foreign.clone(),
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert!(state.objects[&a].prepared.is_some());
        assert!(state.objects[&b].prepared.is_some());
        assert!(state.objects[&d].prepared.is_none());
        assert!(state.objects[&c].prepared.is_none());

        let (mut state, source, a, _, _, d) = build();
        let ability = ResolvedAbility::new(
            mass_prepare(EffectScope::Single),
            foreign,
            source,
            PlayerId(0),
        );
        resolve_become_prepared(&mut state, &ability, &mut Vec::new()).unwrap();
        assert!(
            state.objects[&d].prepared.is_some(),
            "the Single scope acts on the announced targets"
        );
        assert!(state.objects[&a].prepared.is_none());
    }

    /// CR 702.26b: a phased-out permanent is treated as though it does not exist,
    /// so it is not in the population. Paired: once phased in, it is.
    #[test]
    fn become_prepared_all_excludes_phased_out() {
        use crate::game::game_object::{PhaseOutCause, PhaseStatus};
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
        let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
        let e = add_prepare_creature(&mut state, PlayerId(0), "Phased E", true);
        state.objects.get_mut(&e).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        let ability =
            ResolvedAbility::new(mass_prepare(EffectScope::All), vec![], source, PlayerId(0));

        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![a]);
        assert!(state.objects[&e].prepared.is_none());
        assert!(linked_copies(&state, e).is_empty());

        state.objects.get_mut(&e).unwrap().phase_status = PhaseStatus::PhasedIn;
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![e], "phased in, E is reached");
    }

    /// CR 109.5 + CR 113.8: "you" is the ability's controller, not the source's
    /// controller.
    #[test]
    fn become_prepared_all_binds_you_to_the_ability_controller() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
        let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
        let d = add_prepare_creature(&mut state, PlayerId(1), "D", true);
        let ability =
            ResolvedAbility::new(mass_prepare(EffectScope::All), vec![], source, PlayerId(1));

        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![d]);
        assert!(state.objects[&d].prepared.is_some());
        assert!(state.objects[&a].prepared.is_none());
    }

    /// CR 722.3b + CR 722.3c: the unprepare mirror removes the designation from
    /// each creature in the population, and the linked copy does not outlive it.
    /// Paired: the single scope on A alone leaves B prepared.
    #[test]
    fn become_unprepared_all_unprepares_each_controlled_creature() {
        let build = || {
            let mut state = GameState::new_two_player(42);
            let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
            let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
            let b = add_prepare_creature(&mut state, PlayerId(0), "B", true);
            let c = add_prepare_creature(&mut state, PlayerId(0), "Faceless C", false);
            let d = add_prepare_creature(&mut state, PlayerId(1), "Opponent D", true);
            for id in [a, b, d] {
                prepare_object(&mut state, id, &mut Vec::new());
                assert_eq!(linked_copies(&state, id).len(), 1);
            }
            (state, source, a, b, c, d)
        };

        let (mut state, source, a, b, c, d) = build();
        let ability = ResolvedAbility::new(
            mass_unprepare(EffectScope::All),
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_unprepared(&mut state, &ability, &mut events).unwrap();
        let mut unprepared = became_unprepared(&events);
        unprepared.sort_by_key(|id| id.0);
        assert_eq!(unprepared, vec![a, b]);
        for id in [a, b] {
            assert!(state.objects[&id].prepared.is_none());
            assert!(linked_copies(&state, id).is_empty());
        }
        assert!(state.objects[&d].prepared.is_some());
        assert_eq!(linked_copies(&state, d).len(), 1);
        assert!(state.objects[&c].prepared.is_none());

        let (mut state, source, a, b, _, _) = build();
        let ability = ResolvedAbility::new(
            mass_unprepare(EffectScope::Single),
            vec![TargetRef::Object(a)],
            source,
            PlayerId(0),
        );
        resolve_become_unprepared(&mut state, &ability, &mut Vec::new()).unwrap();
        assert!(state.objects[&a].prepared.is_none());
        assert!(state.objects[&b].prepared.is_some());
    }

    /// The single scope still acts only on its declared targets.
    #[test]
    fn become_prepared_single_still_acts_only_on_declared_targets() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
        let a = add_prepare_creature(&mut state, PlayerId(0), "A", true);
        let b = add_prepare_creature(&mut state, PlayerId(0), "B", true);
        let ability = ResolvedAbility::new(
            mass_prepare(EffectScope::Single),
            vec![TargetRef::Object(a)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![a]);
        assert!(state.objects[&b].prepared.is_none());
    }

    /// CR 109.5 + CR 722.3c: "you control" follows the controller, not the owner,
    /// and the linked copy is created by the prepared permanent's controller.
    #[test]
    fn become_prepared_all_follows_controller_not_owner() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Source", false);
        let borrowed = add_prepare_creature(&mut state, PlayerId(1), "Owned by P1", true);
        state.objects.get_mut(&borrowed).unwrap().controller = PlayerId(0);
        let lent = add_prepare_creature(&mut state, PlayerId(0), "Owned by P0", true);
        state.objects.get_mut(&lent).unwrap().controller = PlayerId(1);
        let ability =
            ResolvedAbility::new(mass_prepare(EffectScope::All), vec![], source, PlayerId(0));

        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![borrowed]);
        let copies = linked_copies(&state, borrowed);
        assert_eq!(copies.len(), 1);
        assert_eq!(state.objects[&copies[0]].controller, PlayerId(0));
        assert!(state.objects[&lent].prepared.is_none());
    }

    /// Serde: a legacy payload without `scope` loads as `Single`; `All`
    /// round-trips.
    #[test]
    fn become_prepared_scope_serde_default_and_roundtrip() {
        let legacy: Effect =
            serde_json::from_str(r#"{"type":"BecomePrepared","target":{"type":"ParentTarget"}}"#)
                .unwrap();
        assert_eq!(
            legacy,
            Effect::BecomePrepared {
                target: TargetFilter::ParentTarget,
                scope: EffectScope::Single,
            }
        );
        let legacy: Effect =
            serde_json::from_str(r#"{"type":"BecomeUnprepared","target":{"type":"ParentTarget"}}"#)
                .unwrap();
        assert_eq!(
            legacy,
            Effect::BecomeUnprepared {
                target: TargetFilter::ParentTarget,
                scope: EffectScope::Single,
            }
        );

        for effect in [
            mass_prepare(EffectScope::All),
            mass_unprepare(EffectScope::All),
        ] {
            let json = serde_json::to_value(&effect).unwrap();
            assert_eq!(json["scope"]["type"], "All");
            let back: Effect = serde_json::from_value(json).unwrap();
            assert_eq!(back, effect);
        }
    }

    // ---- Event referent: "that creature" / "it" in a triggered ability ----
    //
    // CR 608.2k + CR 603.2: the subject is the object the trigger condition
    // referred to, read from the published trigger event at resolution. The
    // event is `PermanentUntapped`, whose only field is the object the event
    // names (`extract_source_from_event`).

    /// `BecomePrepared` / `BecomeUnprepared` naming the trigger event's object.
    fn event_referent_prepare() -> Effect {
        Effect::BecomePrepared {
            target: TargetFilter::TriggeringSource,
            scope: EffectScope::Single,
        }
    }

    fn event_referent_unprepare() -> Effect {
        Effect::BecomeUnprepared {
            target: TargetFilter::TriggeringSource,
            scope: EffectScope::Single,
        }
    }

    /// Publish a single-object trigger event naming `object_id`, as
    /// `stack.rs` does while the trigger resolves.
    fn publish_event_naming(state: &mut GameState, object_id: ObjectId) {
        state.current_trigger_event = Some(GameEvent::PermanentUntapped { object_id });
        state.current_trigger_events.clear();
    }

    fn clear_published_events(state: &mut GameState) {
        state.current_trigger_event = None;
        state.current_trigger_events.clear();
    }

    /// CR 608.2k + CR 722.3a: the event's object becomes prepared; the ability's
    /// eligible source does not.
    #[test]
    fn become_prepared_event_referent_prepares_the_event_object() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", true);
        let entering = add_prepare_creature(&mut state, PlayerId(0), "Entering", true);
        publish_event_naming(&mut state, entering);
        let ability = ResolvedAbility::new(event_referent_prepare(), vec![], source, PlayerId(0));

        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        assert_eq!(became_prepared(&events), vec![entering]);
        assert!(state.objects[&entering].prepared.is_some());
        assert!(
            state.objects[&source].prepared.is_none(),
            "the trigger's source is not the event referent"
        );
    }

    /// CR 608.2c + CR 608.2k: an unrelated object target inherited through a
    /// chain does not displace the event referent.
    #[test]
    fn become_prepared_event_referent_beats_inherited_chain_targets() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", false);
        let entering = add_prepare_creature(&mut state, PlayerId(0), "Entering", true);
        let decoy = add_prepare_creature(&mut state, PlayerId(0), "Decoy", true);
        publish_event_naming(&mut state, entering);
        let ability = ResolvedAbility::new(
            event_referent_prepare(),
            vec![TargetRef::Object(decoy)],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();

        assert_eq!(became_prepared(&events), vec![entering]);
        assert!(state.objects[&entering].prepared.is_some());
        assert!(
            state.objects[&decoy].prepared.is_none(),
            "a chain target is not the event referent"
        );
    }

    /// With no published trigger event there is no referent: neither the
    /// chain's object target nor the source is a substitute. Paired: with the
    /// event published, the same shape prepares the event's object.
    #[test]
    fn become_prepared_event_referent_without_a_published_event_fails_closed() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", true);
        let entering = add_prepare_creature(&mut state, PlayerId(0), "Entering", true);
        let decoy = add_prepare_creature(&mut state, PlayerId(0), "Decoy", true);

        // Reach guard: the branch resolves the event's object when one is published.
        publish_event_naming(&mut state, entering);
        let ability = ResolvedAbility::new(event_referent_prepare(), vec![], source, PlayerId(0));
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![entering]);

        clear_published_events(&mut state);
        let ability = ResolvedAbility::new(
            event_referent_prepare(),
            vec![TargetRef::Object(decoy)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert!(became_prepared(&events).is_empty(), "{events:?}");
        assert!(state.objects[&decoy].prepared.is_none());
        assert!(state.objects[&source].prepared.is_none());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::BecomePrepared,
                ..
            }
        )));
    }

    /// CR 722.3a + CR 110.1 + CR 400.7: an event referent that has left the
    /// battlefield is not a permanent and cannot become prepared, even with a
    /// prepare face. Paired: an event referent on the battlefield does.
    #[test]
    fn become_prepared_event_referent_skips_an_object_that_left_the_battlefield() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", false);
        let entering = add_prepare_creature(&mut state, PlayerId(0), "Entering", true);
        let departed = add_prepare_creature(&mut state, PlayerId(0), "Departed", true);

        publish_event_naming(&mut state, entering);
        let ability = ResolvedAbility::new(event_referent_prepare(), vec![], source, PlayerId(0));
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![entering], "reach guard");

        crate::game::zones::move_to_zone(&mut state, departed, Zone::Graveyard, &mut Vec::new());
        let obj = state.objects.get_mut(&departed).unwrap();
        assert_eq!(obj.zone, Zone::Graveyard, "fixture reach guard");
        // Keep it otherwise eligible so only the battlefield gate can skip it.
        obj.back_face = Some(BackFaceForTest::prepare());
        publish_event_naming(&mut state, departed);
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert!(became_prepared(&events).is_empty(), "{events:?}");
        assert!(state.objects[&departed].prepared.is_none());
    }

    /// The tap event a "becomes tapped" trigger carries, stamped with the
    /// incarnation of the permanent that is on the battlefield now.
    fn tapped_event_for(state: &GameState, object_id: ObjectId) -> GameEvent {
        GameEvent::permanent_tapped(state, object_id, None)
    }

    /// CR 400.7: blink `id` (battlefield -> exile -> battlefield) so it returns as
    /// a new object at the same `ObjectId`, re-equipped with its prepare face.
    fn blink(state: &mut GameState, id: ObjectId) {
        let before = state.objects[&id].incarnation;
        crate::game::zones::move_to_zone(state, id, Zone::Exile, &mut Vec::new());
        crate::game::zones::move_to_zone(state, id, Zone::Battlefield, &mut Vec::new());
        let obj = state.objects.get_mut(&id).unwrap();
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "fixture reach guard: it returned"
        );
        assert!(
            obj.incarnation > before,
            "fixture reach guard: a blink is a new incarnation at the same id"
        );
        obj.back_face = Some(BackFaceForTest::prepare());
    }

    /// CR 400.7 + CR 608.2k + CR 722.3a: the trigger event names the incarnation
    /// that became tapped. Paired with the same-incarnation positive, a referent
    /// that blinked before the trigger resolved is a new object and is not
    /// prepared; the new object is not related to the old one.
    #[test]
    fn become_prepared_event_referent_ignores_a_blinked_new_incarnation() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", false);
        let tapped = add_prepare_creature(&mut state, PlayerId(0), "Tapped", true);
        let ability = ResolvedAbility::new(event_referent_prepare(), vec![], source, PlayerId(0));

        let event = tapped_event_for(&state, tapped);
        state.current_trigger_event = Some(event.clone());
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            became_prepared(&events),
            vec![tapped],
            "same incarnation: positive control"
        );
        state.objects.get_mut(&tapped).unwrap().prepared = None;

        blink(&mut state, tapped);
        state.current_trigger_event = Some(event);
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert!(became_prepared(&events).is_empty(), "{events:?}");
        assert!(state.objects[&tapped].prepared.is_none());

        // The same blink does not stop a fresh event for the new incarnation.
        let fresh = tapped_event_for(&state, tapped);
        state.current_trigger_event = Some(fresh);
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![tapped]);
    }

    /// CR 400.7 + CR 608.2k: the unprepare twin — a blinked, since-prepared new
    /// incarnation keeps its designation against the stale event.
    #[test]
    fn become_unprepared_event_referent_ignores_a_blinked_new_incarnation() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", false);
        let tapped = add_prepare_creature(&mut state, PlayerId(0), "Tapped", true);
        prepare_object(&mut state, tapped, &mut Vec::new());
        let ability = ResolvedAbility::new(event_referent_unprepare(), vec![], source, PlayerId(0));

        let stale = tapped_event_for(&state, tapped);
        blink(&mut state, tapped);
        prepare_object(&mut state, tapped, &mut Vec::new());
        assert!(
            state.objects[&tapped].prepared.is_some(),
            "fixture reach guard"
        );

        state.current_trigger_event = Some(stale);
        let mut events = Vec::new();
        resolve_become_unprepared(&mut state, &ability, &mut events).unwrap();
        assert!(became_unprepared(&events).is_empty(), "{events:?}");
        assert!(state.objects[&tapped].prepared.is_some());

        let fresh = tapped_event_for(&state, tapped);
        state.current_trigger_event = Some(fresh);
        let mut events = Vec::new();
        resolve_become_unprepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            became_unprepared(&events),
            vec![tapped],
            "same incarnation: positive control"
        );
    }

    /// CR 702.26b: a phased-out event referent is treated as though it does not
    /// exist. Paired: once phased in, it becomes prepared.
    #[test]
    fn become_prepared_event_referent_skips_a_phased_out_object() {
        use crate::game::game_object::PhaseOutCause;
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", false);
        let entering = add_prepare_creature(&mut state, PlayerId(0), "Entering", true);
        state.objects.get_mut(&entering).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        publish_event_naming(&mut state, entering);
        let ability = ResolvedAbility::new(event_referent_prepare(), vec![], source, PlayerId(0));

        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert!(became_prepared(&events).is_empty(), "{events:?}");
        assert!(state.objects[&entering].prepared.is_none());

        state.objects.get_mut(&entering).unwrap().phase_status = PhaseStatus::PhasedIn;
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            became_prepared(&events),
            vec![entering],
            "phased in, it is reached"
        );
    }

    /// CR 722.3a: an event referent without a prepare spell is skipped. Paired:
    /// an eligible event referent in the same state becomes prepared.
    #[test]
    fn become_prepared_event_referent_without_a_prepare_face_is_skipped() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", true);
        let eligible = add_prepare_creature(&mut state, PlayerId(0), "Eligible", true);
        let faceless = add_prepare_creature(&mut state, PlayerId(0), "Faceless", false);
        let ability = ResolvedAbility::new(event_referent_prepare(), vec![], source, PlayerId(0));

        publish_event_naming(&mut state, eligible);
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert_eq!(became_prepared(&events), vec![eligible], "reach guard");

        publish_event_naming(&mut state, faceless);
        let mut events = Vec::new();
        resolve_become_prepared(&mut state, &ability, &mut events).unwrap();
        assert!(became_prepared(&events).is_empty(), "{events:?}");
        assert!(state.objects[&faceless].prepared.is_none());
        assert!(state.objects[&source].prepared.is_none());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::BecomePrepared,
                ..
            }
        )));
    }

    /// CR 115.10a: the event referent is affected, not targeted, so no target
    /// slot is built. Paired: a typed filter under the same scope builds one.
    #[test]
    fn become_prepared_event_referent_claims_no_target_slot() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", false);
        add_prepare_creature(&mut state, PlayerId(0), "A", true);

        for effect in [event_referent_prepare(), event_referent_unprepare()] {
            assert_eq!(
                effect.target_filter(),
                Some(&TargetFilter::TriggeringSource)
            );
            assert!(crate::game::triggers::extract_target_filter_from_effect(&effect).is_none());
            let slots = build_target_slots(
                &state,
                &ResolvedAbility::new(effect.clone(), vec![], source, PlayerId(0)),
            )
            .unwrap();
            assert!(slots.is_empty(), "{effect:?} must build no target slot");
        }

        let typed = mass_prepare(EffectScope::Single);
        assert_eq!(
            crate::game::triggers::extract_target_filter_from_effect(&typed),
            Some(&creatures_you_control())
        );
    }

    /// CR 722.3b + CR 608.2k: only the event's object loses the designation.
    #[test]
    fn become_unprepared_event_referent_unprepares_only_the_event_object() {
        let mut state = GameState::new_two_player(42);
        let source = add_prepare_creature(&mut state, PlayerId(0), "Watcher", true);
        let event_obj = add_prepare_creature(&mut state, PlayerId(0), "Attacker", true);
        let bystander = add_prepare_creature(&mut state, PlayerId(0), "Bystander", true);
        for id in [source, event_obj, bystander] {
            prepare_object(&mut state, id, &mut Vec::new());
            assert!(state.objects[&id].prepared.is_some(), "precondition");
        }
        publish_event_naming(&mut state, event_obj);
        let ability = ResolvedAbility::new(event_referent_unprepare(), vec![], source, PlayerId(0));

        let mut events = Vec::new();
        resolve_become_unprepared(&mut state, &ability, &mut events).unwrap();

        assert_eq!(became_unprepared(&events), vec![event_obj]);
        assert!(state.objects[&event_obj].prepared.is_none());
        assert!(state.objects[&bystander].prepared.is_some());
        assert!(state.objects[&source].prepared.is_some());
    }

    /// Helper to build a minimal back-face with `layout_kind == Prepare` so
    /// the resolver's `has_prepare_face` gate passes in tests.
    struct BackFaceForTest;
    impl BackFaceForTest {
        fn prepare() -> crate::game::game_object::BackFaceData {
            Self::prepare_with_cost(Default::default())
        }

        fn prepare_with_cost(mana_cost: ManaCost) -> crate::game::game_object::BackFaceData {
            let mut card_types = crate::types::card_type::CardType::default();
            card_types.core_types.push(CoreType::Sorcery);
            crate::game::game_object::BackFaceData {
                is_swap_snapshot: false,
                trigger_printed_origins: Vec::new(),
                name: "Test Prepare Face".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
                printed_loyalty: None,
                defense: None,
                card_types,
                mana_cost,
                keywords: Vec::new(),
                abilities: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )],
                trigger_definitions: crate::types::definitions::Definitions::default(),
                replacement_definitions: crate::types::definitions::Definitions::default(),
                static_definitions: crate::types::definitions::Definitions::default(),
                color: Vec::new(),
                printed_ref: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: Vec::new(),
                casting_options: Vec::new(),
                layout_kind: Some(LayoutKind::Prepare),
                parse_warnings: vec![],
            }
        }

        fn prepare_permanent() -> crate::game::game_object::BackFaceData {
            let mut back = Self::prepare_with_cost(ManaCost::zero());
            back.card_types.core_types = vec![CoreType::Creature];
            back.abilities.clear();
            back
        }
    }
}
