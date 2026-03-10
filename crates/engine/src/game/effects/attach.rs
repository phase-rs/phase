use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// Resolve an Attach effect: sets source.attached_to = target and adds source to target.attachments.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let source_id = ability.source_id;

    // Determine target from ability targets
    let target_id = ability
        .targets
        .first()
        .and_then(|t| match t {
            crate::types::ability::TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .ok_or_else(|| EffectError::MissingParam("No target for Attach".to_string()))?;

    attach_to(state, source_id, target_id);

    events.push(GameEvent::EffectResolved {
        api_type: "Attach".to_string(),
        source_id,
    });

    Ok(())
}

/// Core attachment logic: attach `attachment_id` to `target_id`.
/// Handles detaching from a previous target if already attached.
pub fn attach_to(state: &mut GameState, attachment_id: ObjectId, target_id: ObjectId) {
    // If already attached to something, detach first
    if let Some(old_target_id) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
    {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }

    // Set attached_to on the attachment
    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = Some(target_id);
    }

    // Add to target's attachments list
    if let Some(target) = state.objects.get_mut(&target_id) {
        if !target.attachments.contains(&attachment_id) {
            target.attachments.push(attachment_id);
        }
    }

    state.layers_dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
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
            Some(creature_id)
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
            Some(creature_a)
        );

        // Re-equip to creature B
        attach_to(&mut state, equipment_id, creature_b);

        // Should be attached to B now
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(creature_b)
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
}
