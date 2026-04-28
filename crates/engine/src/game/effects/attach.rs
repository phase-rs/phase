use crate::game::game_object::AttachTarget;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 701.3a + CR 701.3b: Attach — to place an Aura, Equipment, or Fortification on another object or player.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let source_id = ability.source_id;
    let (attachment_filter, target_filter) = match &ability.effect {
        Effect::Attach { attachment, target } => (attachment, target),
        _ => (&TargetFilter::SelfRef, &TargetFilter::Any),
    };

    let mut target_slots = ability.targets.iter();
    let attachment_id = resolve_object_filter(state, ability, attachment_filter, &mut target_slots)
        .ok_or_else(|| EffectError::MissingParam("No attachment for Attach".to_string()))?;
    let target_id = resolve_object_filter(state, ability, target_filter, &mut target_slots)
        .ok_or_else(|| EffectError::MissingParam("No target for Attach".to_string()))?;

    attach_to(state, attachment_id, target_id);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id,
    });

    Ok(())
}

fn resolve_object_filter<'a>(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    target_slots: &mut impl Iterator<Item = &'a TargetRef>,
) -> Option<ObjectId> {
    match filter {
        TargetFilter::SelfRef => Some(ability.source_id),
        TargetFilter::LastCreated => target_slots
            .find_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .or_else(|| state.last_created_token_ids.first().copied()),
        TargetFilter::TriggeringSource | TargetFilter::AttachedTo => {
            crate::game::targeting::resolve_event_context_target(state, filter, ability.source_id)
                .and_then(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                })
        }
        TargetFilter::ParentTarget => ability.targets.iter().find_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        }),
        _ => target_slots.find_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        }),
    }
}

/// CR 701.3c: Attaching to a different object gives the attachment a new timestamp.
/// Core attachment logic: attach `attachment_id` to `target_id`.
/// Handles detaching from a previous target if already attached.
pub fn attach_to(state: &mut GameState, attachment_id: ObjectId, target_id: ObjectId) {
    // CR 701.3, CR 702.5, CR 702.6: Attachment prohibitions on the target.
    // `CantBeAttached` blocks any attachment (Aura / Equipment / Fortification);
    // `CantBeEnchanted` blocks Auras specifically; `CantBeEquipped` blocks Equipment.
    // A blocked attachment is a silent no-op — no state mutation, no events.
    if crate::game::static_abilities::object_has_static_other(state, target_id, "CantBeAttached") {
        return;
    }
    let attacher_is_aura = state
        .objects
        .get(&attachment_id)
        .is_some_and(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"));
    let attacher_is_equipment = state
        .objects
        .get(&attachment_id)
        .is_some_and(|obj| obj.card_types.subtypes.iter().any(|s| s == "Equipment"));
    if attacher_is_aura
        && crate::game::static_abilities::object_has_static_other(
            state,
            target_id,
            "CantBeEnchanted",
        )
    {
        return;
    }
    if attacher_is_equipment
        && crate::game::static_abilities::object_has_static_other(
            state,
            target_id,
            "CantBeEquipped",
        )
    {
        return;
    }

    // CR 701.3a: Attaching moves attachment onto target.
    // If already attached to something, detach first. We only need to clear an
    // Object host's `attachments` list — a Player host has no such list.
    if let Some(old_target_id) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
        .and_then(|t| t.as_object())
    {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }

    // Set attached_to on the attachment. `From<ObjectId> for AttachTarget`
    // selects the `Object` variant; player attachment has its own entry point
    // (`attach_to_player`).
    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = Some(target_id.into());
    }

    // Add to target's attachments list
    if let Some(target) = state.objects.get_mut(&target_id) {
        if !target.attachments.contains(&attachment_id) {
            target.attachments.push(attachment_id);
        }
    }

    state.layers_dirty = true;
}

/// CR 303.4 + CR 702.5: Attach an Aura to a player (Curse cycle, Faith's
/// Fetters-class). Mirrors `attach_to`'s "detach from previous host" cleanup
/// for Object hosts, but no host-side `attachments` list is touched (a player
/// is not a `GameObject` and has no such field).
pub fn attach_to_player(state: &mut GameState, attachment_id: ObjectId, target_player: PlayerId) {
    // CR 701.3a: If already attached to an object, detach from that object's
    // `attachments` list. Re-attaching to a player has no symmetric cleanup —
    // the previous Player host has no list to clear.
    if let Some(AttachTarget::Object(old_target_id)) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
    {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }

    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = Some(AttachTarget::Player(target_player));
    }

    state.layers_dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{StaticDefinition, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    /// Build a permanent with the given subtype on the battlefield.
    fn spawn_with_subtype(state: &mut GameState, name: &str, subtype: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.subtypes.push(subtype.to_string());
        id
    }

    fn spawn_creature(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(2),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    fn apply_static(state: &mut GameState, id: ObjectId, mode_name: &str) {
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::Other(mode_name.to_string()))
                .affected(TargetFilter::SelfRef),
        );
    }

    #[test]
    fn test_attach_sets_attached_to_and_attachments() {
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .subtypes
            .push("Equipment".to_string());

        let creature_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        attach_to(&mut state, equipment_id, creature_id);

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_id))
        );
        assert!(state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn test_attach_re_equip_moves_equipment() {
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );

        let creature_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let creature_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Attach to creature A
        attach_to(&mut state, equipment_id, creature_a);
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_a))
        );

        // Re-equip to creature B
        attach_to(&mut state, equipment_id, creature_b);

        // Should be attached to B now
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_b))
        );
        assert!(state
            .objects
            .get(&creature_b)
            .unwrap()
            .attachments
            .contains(&equipment_id));

        // Should no longer be on A's attachments
        assert!(!state
            .objects
            .get(&creature_a)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn cant_be_attached_blocks_any_attachment() {
        // CR 701.3: "Can't be attached" blocks any attachment (Aura/Equipment).
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let victim = spawn_creature(&mut state, "Victim");
        apply_static(&mut state, victim, "CantBeAttached");

        attach_to(&mut state, aura, victim);

        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);
        assert!(state.objects.get(&victim).unwrap().attachments.is_empty());
    }

    #[test]
    fn cant_be_enchanted_blocks_aura() {
        // CR 702.5: "Can't be enchanted" blocks Auras specifically.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Pacifism", "Aura");
        let victim = spawn_creature(&mut state, "Kira");
        apply_static(&mut state, victim, "CantBeEnchanted");

        attach_to(&mut state, aura, victim);

        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);
    }

    #[test]
    fn cant_be_equipped_blocks_equipment() {
        // CR 702.6: "Can't be equipped" blocks Equipment specifically.
        let mut state = setup();
        let equipment = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let victim = spawn_creature(&mut state, "Skittering Surveyor");
        apply_static(&mut state, victim, "CantBeEquipped");

        attach_to(&mut state, equipment, victim);

        assert_eq!(state.objects.get(&equipment).unwrap().attached_to, None);
    }

    #[test]
    fn attach_resolves_last_created_target() {
        // CR 702.182a: Attach sub-ability with TargetFilter::LastCreated resolves
        // target from state.last_created_token_ids (Job select pattern).
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rod".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .subtypes
            .push("Equipment".to_string());

        let token_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Hero".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&token_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.last_created_token_ids = vec![token_id];

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::LastCreated,
            },
            vec![], // No explicit targets — should fall back to LastCreated
            equipment_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(token_id))
        );
        assert!(state
            .objects
            .get(&token_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn attach_with_explicit_targets_ignores_last_created() {
        // Regression: when explicit targets exist, LastCreated on the effect
        // should NOT be used — explicit targets take precedence.
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        let creature_a = spawn_creature(&mut state, "Bear A");
        let creature_b = spawn_creature(&mut state, "Bear B");
        state.last_created_token_ids = vec![creature_b];

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::LastCreated,
            },
            vec![crate::types::ability::TargetRef::Object(creature_a)],
            equipment_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should attach to creature_a (explicit target), not creature_b (LastCreated)
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_a))
        );
    }

    #[test]
    fn attach_resolves_non_source_attachment_from_target_slot() {
        let mut state = setup();
        let source_id = spawn_creature(&mut state, "Windwalker");
        let equipment_id = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let creature_id = spawn_creature(&mut state, "Bear");

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Any,
                target: TargetFilter::Any,
            },
            vec![
                crate::types::ability::TargetRef::Object(equipment_id),
                crate::types::ability::TargetRef::Object(creature_id),
            ],
            source_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_id))
        );
        assert!(state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
        assert_eq!(state.objects.get(&source_id).unwrap().attached_to, None);
    }

    #[test]
    fn attach_prohibitions_distinguish_aura_vs_equipment() {
        // CantBeEnchanted allows Equipment; CantBeEquipped allows Auras;
        // CantBeAttached blocks both.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let equipment = spawn_with_subtype(&mut state, "Sword", "Equipment");

        // Case A: CantBeEnchanted creature accepts Equipment.
        let cant_be_enchanted = spawn_creature(&mut state, "Kira");
        apply_static(&mut state, cant_be_enchanted, "CantBeEnchanted");
        attach_to(&mut state, equipment, cant_be_enchanted);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(cant_be_enchanted)),
            "Equipment should attach to a creature with CantBeEnchanted"
        );
        // Aura is rejected
        attach_to(&mut state, aura, cant_be_enchanted);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            None,
            "Aura should be blocked by CantBeEnchanted"
        );

        // Case B: CantBeEquipped creature accepts Auras.
        let cant_be_equipped = spawn_creature(&mut state, "Citanul Druid");
        apply_static(&mut state, cant_be_equipped, "CantBeEquipped");
        attach_to(&mut state, aura, cant_be_equipped);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(AttachTarget::Object(cant_be_equipped)),
            "Aura should attach to a creature with CantBeEquipped"
        );

        // Case C: CantBeAttached creature rejects both.
        // Detach the aura and equipment first by removing the static from earlier cases.
        let cant_be_attached = spawn_creature(&mut state, "Warded Keep");
        apply_static(&mut state, cant_be_attached, "CantBeAttached");
        let aura2 = spawn_with_subtype(&mut state, "Aura2", "Aura");
        let equipment2 = spawn_with_subtype(&mut state, "Sword2", "Equipment");
        attach_to(&mut state, aura2, cant_be_attached);
        attach_to(&mut state, equipment2, cant_be_attached);
        assert_eq!(state.objects.get(&aura2).unwrap().attached_to, None);
        assert_eq!(state.objects.get(&equipment2).unwrap().attached_to, None);
    }
}
