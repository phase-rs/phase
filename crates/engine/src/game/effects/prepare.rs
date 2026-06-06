use crate::types::ability::{
    CastingPermission, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card::LayoutKind;
use crate::types::events::GameEvent;
use crate::types::game_state::{CopyTargetSlot, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use crate::game::ability_utils::build_target_slots;
use crate::game::casting;
use crate::game::game_object::PreparedState;
use crate::game::printed_cards::apply_back_face_to_object;

// The prepare cast path now materializes a short-lived exile GameObject copy of
// the prepare-spell face so the copy can reuse the normal casting pipeline
// (targets/modes/mana payment/cost modifiers).

/// Extract object targets from `ability.targets`, or fall back to `last_created_token_ids`
/// for `TargetFilter::LastCreated`. Mirrors the pattern used by `suspect::resolve`.
fn resolve_object_targets(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let filter = match &ability.effect {
        Effect::BecomePrepared { target } | Effect::BecomeUnprepared { target } => target,
        _ => return Vec::new(),
    };
    if matches!(filter, TargetFilter::LastCreated) {
        return state.last_created_token_ids.clone();
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
    obj.back_face
        .as_ref()
        .is_some_and(|b| matches!(b.layout_kind, Some(LayoutKind::Prepare)))
}

/// CR 722.3a-c: Prepare — resolver for `Effect::BecomePrepared`.
///
/// Idempotent: no-op (and no event emitted) if the target is already prepared
/// or if the target lacks a prepare face (Biblioplex gate). Otherwise sets
/// `prepared = Some(PreparedState)` and emits `BecamePrepared`.
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
    });
    Ok(())
}

/// CR 722.3b: Prepare — resolver for `Effect::BecomeUnprepared`.
///
/// Idempotent: no-op (and no event emitted) if the target is not prepared.
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
        let Some(obj) = state.objects.get_mut(&object_id) else {
            continue;
        };
        if obj.prepared.is_none() {
            continue;
        }
        obj.prepared = None;
        events.push(GameEvent::BecameUnprepared { object_id });
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::BecomeUnprepared,
        source_id: ability.source_id,
    });
    Ok(())
}

/// Direct-call variant used by `GameAction::CastPreparedCopy` handling — flips
/// `prepared` to None on a specific object, emitting the event only when the
/// toggle actually fires. Centralizes the "cast-time unprepare" rule so the
/// action handler doesn't inspect the field directly (single-authority).
pub fn unprepare_object(state: &mut GameState, object_id: ObjectId, events: &mut Vec<GameEvent>) {
    let Some(obj) = state.objects.get_mut(&object_id) else {
        return;
    };
    if obj.prepared.is_none() {
        return;
    }
    obj.prepared = None;
    events.push(GameEvent::BecameUnprepared { object_id });
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
        current_slot: 0,
    };
    Ok(true)
}

fn cleanup_failed_prepared_copy_cast(state: &mut GameState, copy_id: ObjectId) {
    // Defensive cleanup for any failed cast attempt after synthesizing the
    // ephemeral copy object.
    state.stack.retain(|entry| entry.id != copy_id);
    state.objects.remove(&copy_id);
}

fn synthesize_prepared_copy_object(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
) -> Result<(ObjectId, crate::types::identifiers::CardId), String> {
    // CR 722.3c: As the player casts the prepared copy, synthesize a distinct
    // copy object of the prepare face in exile to feed through normal casting.
    //
    // DEFERRED (CR 722.3c): strictly, the copy is created in exile when the card
    // becomes prepared, not at cast time. Materializing it here means exile-zone
    // replacements/triggers (Containment Priest, Rest in Peace, Leyline of the
    // Void) cannot observe the prepared copy before it is cast. No current card
    // depends on that interaction; wiring prepare-time materialization is the
    // remaining work to make this fully rules-correct.
    let (src_clone, card_id) = {
        let Some(src_obj) = state.objects.get(&source_id) else {
            return Err(format!("source {source_id:?} not found"));
        };
        if src_obj.prepared.is_none() {
            return Err("source is not prepared".to_string());
        }
        (src_obj.clone(), src_obj.card_id)
    };
    let Some(back) = src_clone.back_face.clone() else {
        return Err("source has no prepare face".to_string());
    };
    if !matches!(back.layout_kind, Some(LayoutKind::Prepare)) {
        return Err("source back_face is not a Prepare face".to_string());
    }

    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    let mut copy_obj = src_clone;
    copy_obj.id = copy_id;
    copy_obj.zone = Zone::Exile;
    copy_obj.controller = controller;
    copy_obj.owner = controller;
    copy_obj.is_token = true;
    copy_obj.tapped = false;
    copy_obj.prepared = None;
    // Do not re-enter alternative-face casting logic for this synthetic copy.
    copy_obj.back_face = None;
    apply_back_face_to_object(&mut copy_obj, back.clone());
    copy_obj
        .casting_permissions
        .push(CastingPermission::ExileWithAltCost {
            cost: back.mana_cost.clone(),
            cast_transformed: false,
            constraint: None,
            granted_to: Some(controller),
            resolution_cleanup: None,
            duration: None,
        });
    state.objects.insert(copy_id, copy_obj);

    Ok((copy_id, card_id))
}

fn can_cast_prepared_copy_now_in_simulated_state(
    simulated: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    let original_next_object_id = simulated.next_object_id;
    let Ok((copy_id, _)) = synthesize_prepared_copy_object(simulated, source_id, controller) else {
        return false;
    };
    let can_cast = casting::can_cast_object_now(simulated, controller, copy_id);
    cleanup_failed_prepared_copy_cast(simulated, copy_id);
    simulated.next_object_id = original_next_object_id;
    can_cast
}

/// Fast castability probe for `GameAction::CastPreparedCopy` candidate
/// generation. Synthesizes the same ephemeral copy object used by the actual
/// cast path in a temporary game-state clone, then asks the canonical casting
/// predicate whether that copy is castable right now.
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
        WaitingFor::ManaPayment { .. } | WaitingFor::PhyrexianPayment { .. }
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

/// CR 722.3c + CR 601.2: Build an ephemeral token copy of the prepare-spell
/// face (face `b`) in exile, then cast it through the normal spell-casting
/// pipeline so costs/targets/modes are handled by the same single authority as
/// every other cast.
pub fn cast_prepared_copy(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    let (copy_id, card_id) = synthesize_prepared_copy_object(state, source_id, controller)?;

    // CR 117.1d + CR 601.2g: The copy must be castable with feasible mana
    // payment options right now; otherwise this special action is illegal.
    if !casting::can_cast_object_now(state, controller, copy_id) {
        cleanup_failed_prepared_copy_cast(state, copy_id);
        return Err("prepared copy is not castable now".to_string());
    }

    let mut waiting = match casting::handle_cast_spell(state, controller, copy_id, card_id, events)
    {
        Ok(waiting) => waiting,
        Err(err) => {
            cleanup_failed_prepared_copy_cast(state, copy_id);
            return Err(format!("{err}"));
        }
    };

    // CR 601.2i + CR 722.3c: If the cast is cancelled before completion,
    // restore the source's prepared marker and remove the synthetic copy.
    mark_prepare_copy_cancel_rollback(state, &mut waiting, source_id, copy_id);

    // CR 722.3c: "Doing so unprepares it." Unprepare-at-cast, not at resolve —
    // so countered / fizzled copies still leave the source unprepared. Single
    // authority via `unprepare_object`.
    unprepare_object(state, source_id, events);

    Ok(waiting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_support::legal_actions;
    use crate::game::zones::create_object;
    use crate::parser::oracle_effect::parse_effect;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, ReplacementDefinition, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaCost, ManaCostShard};
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
            false,
            None,
            &[],
            None,
            false,
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
    fn become_unprepared_is_idempotent() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        let ability = ResolvedAbility::new(
            Effect::BecomeUnprepared {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_unprepared(&mut state, &ability, &mut events).unwrap();

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::BecameUnprepared { .. })),
            "no BecameUnprepared event when already unprepared"
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
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BecamePrepared { object_id } if *object_id == id)));

        // Idempotency — second call must not re-emit.
        let mut events2 = Vec::new();
        prepare_object(&mut state, id, &mut events2);
        assert!(events2.is_empty());
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
                ability: Some(resolved),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let armed = open_copy_target_selection(&mut state, copy_id, PlayerId(0)).unwrap();
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
                ability: Some(resolved),
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

        let armed = open_copy_target_selection(&mut state, copy_id, PlayerId(0)).unwrap();
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
        assert!(
            state.stack.is_empty(),
            "failed cast must not leave stack entries"
        );
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
        state.objects.get_mut(&copy_id).unwrap().is_token = true;
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
            !state.objects.contains_key(&copy_id),
            "cancelled cast must clear synthesized copy object"
        );
        assert!(
            state.stack.iter().all(|entry| entry.id != copy_id),
            "cancelled cast must remove stack placeholder for synthesized copy"
        );
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
                name: "Test Prepare Face".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
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
            }
        }
    }
}
