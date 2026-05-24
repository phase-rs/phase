use std::sync::Arc;

use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card::LayoutKind;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, CopyTargetSlot, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use crate::game::ability_utils::{build_resolved_from_def, build_target_slots};
use crate::game::game_object::PreparedState;

// KNOWN_LIMITATION: The reminder text for Prepare says a copy of the
// prepare-spell "appears in exile" while the creature is prepared. This design
// does NOT materialize that copy as a GameObject. The cast-offer is produced
// at priority time by scanning battlefield creatures whose `prepared.is_some()`
// and whose printed `CardLayout::Prepare(_, b)` supplies face `b`. As a result,
// exile-event triggers and "going-to-exile" replacement effects (Rest in
// Peace, Leyline of the Void, Containment Priest) will NOT observe the copy.
// Acceptable for the SOS-era cards — no card in the set interacts with
// prepare-copies through those hooks — and aligned with CR 722.3c's special
// exception for prepare-spell copies existing in exile. If a future card
// requires the copy to be a first-class exile GameObject, materialization can
// be retrofitted around the existing offer scan without touching the resolver
// layer.

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
        // Biblioplex gate — only creatures with prepare spells can become prepared.
        if !has_prepare_face(state, object_id) {
            continue;
        }
        let Some(obj) = state.objects.get_mut(&object_id) else {
            continue;
        };
        // Idempotency: no-op if already prepared.
        if obj.prepared.is_some() {
            continue;
        }
        obj.prepared = Some(PreparedState);
        events.push(GameEvent::BecamePrepared { object_id });
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

/// CR 702.xxx + CR 707.10f: Build a token spell-copy on the stack from the
/// prepare-spell face (face `b`) of `source_id`'s printed card. The resulting
/// stack entry mirrors the `copy_spell` effect's construction — a fresh
/// ObjectId, `is_token = true`, `CastingVariant::Normal`, controller = acting
/// player. The source creature is unprepared at cast time (reminder: "Doing
/// so unprepares it."), not on resolution — so counter-the-copy leaves the
/// source permanently unprepared.
///
/// If the prepare-face spell requires targets (e.g., Biblioplex's companion
/// prepare cards), the caller enters `WaitingFor::CopyRetarget` so the
/// controller can pick legal targets via `open_copy_target_selection`.
///
/// Returns Ok(copy_id) on success. Returns Err if the source is not prepared,
/// lacks a prepare face, or doesn't exist.
pub fn cast_prepared_copy(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<ObjectId, String> {
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
    // Select the first ability on face_b as the spell ability. SOS prepare
    // spells each have a single spell ability (Sorcery-type); more complex
    // multi-ability prepare faces are out of scope.
    let ability_def = back
        .abilities
        .first()
        .cloned()
        .ok_or_else(|| "prepare face has no spell ability".to_string())?;

    // Allocate a new object id for the copy.
    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    // Build a GameObject for the token copy — clone core characteristics from
    // back_face so zone transitions and filter predicates see the correct
    // face. Name from face_b, zone = Stack, is_token = true.
    let mut copy_obj = src_clone;
    copy_obj.id = copy_id;
    copy_obj.name = back.name.clone();
    copy_obj.power = back.power;
    copy_obj.toughness = back.toughness;
    copy_obj.loyalty = back.loyalty;
    copy_obj.defense = back.defense;
    copy_obj.card_types = back.card_types.clone();
    copy_obj.mana_cost = back.mana_cost.clone();
    copy_obj.keywords = back.keywords.clone();
    copy_obj.abilities = Arc::new(back.abilities.clone());
    copy_obj.color = back.color.clone();
    copy_obj.printed_ref = back.printed_ref.clone();
    copy_obj.controller = controller;
    copy_obj.owner = controller;
    copy_obj.zone = Zone::Stack;
    copy_obj.is_token = true;
    // CR 722.3c: the copy is a distinct object — clear any per-permanent
    // state carried over from the source's creature face.
    copy_obj.tapped = false;
    copy_obj.prepared = None;
    copy_obj.back_face = None;
    state.objects.insert(copy_id, copy_obj);

    // CR 707.10: Build a ResolvedAbility from face_b's ability definition
    // preserving sub-ability chains, optional flags, and duration metadata.
    // `build_resolved_from_def` is the authoritative constructor used by
    // normal casting (see `ability_utils`).
    let resolved = build_resolved_from_def(&ability_def, copy_id, controller);

    state.stack.push_back(StackEntry {
        id: copy_id,
        source_id: copy_id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: Some(resolved),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    events.push(crate::types::events::GameEvent::StackPushed { object_id: copy_id });

    // CR 722.3c: "Doing so unprepares it." Unprepare-at-cast, not at resolve —
    // so countered / fizzled copies still leave the source unprepared. Single
    // authority via `unprepare_object`.
    unprepare_object(state, source_id, events);

    Ok(copy_id)
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

    /// Helper to build a minimal back-face with `layout_kind == Prepare` so
    /// the resolver's `has_prepare_face` gate passes in tests.
    struct BackFaceForTest;
    impl BackFaceForTest {
        fn prepare() -> crate::game::game_object::BackFaceData {
            let mut card_types = crate::types::card_type::CardType::default();
            card_types.core_types.push(CoreType::Sorcery);
            crate::game::game_object::BackFaceData {
                name: "Test Prepare Face".to_string(),
                power: None,
                toughness: None,
                loyalty: None,
                defense: None,
                card_types,
                mana_cost: Default::default(),
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
