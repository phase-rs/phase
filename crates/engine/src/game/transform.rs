use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

use super::engine::EngineError;
use super::game_object::BackFaceData;

/// Transform a permanent between its front and back face.
///
/// Toggles `obj.transformed`, swaps current characteristics with back_face data,
/// emits `GameEvent::Transformed`, and marks layers dirty.
///
/// Returns an error if the object has no back face (not a DFC).
pub fn transform_permanent(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    let back_face = obj
        .back_face
        .clone()
        .ok_or_else(|| EngineError::InvalidAction("Card has no back face".to_string()))?;

    let obj = state.objects.get_mut(&object_id).unwrap();

    if obj.transformed {
        // Transforming back to front: swap back_face with current, restoring front face
        let current_back = BackFaceData {
            name: obj.name.clone(),
            power: obj.power,
            toughness: obj.toughness,
            card_types: obj.card_types.clone(),
            keywords: obj.keywords.clone(),
            abilities: obj.abilities.clone(),
            color: obj.color.clone(),
        };

        // Restore front face from back_face
        obj.name = back_face.name;
        obj.power = back_face.power;
        obj.toughness = back_face.toughness;
        obj.base_power = back_face.power;
        obj.base_toughness = back_face.toughness;
        obj.card_types = back_face.card_types;
        obj.keywords = back_face.keywords.clone();
        obj.base_keywords = back_face.keywords;
        obj.abilities = back_face.abilities;
        obj.color = back_face.color.clone();
        obj.base_color = back_face.color;

        // Store current (back) face data so we can transform again
        obj.back_face = Some(current_back);
        obj.transformed = false;
    } else {
        // Transforming front to back: swap current with back_face
        let current_front = BackFaceData {
            name: obj.name.clone(),
            power: obj.power,
            toughness: obj.toughness,
            card_types: obj.card_types.clone(),
            keywords: obj.keywords.clone(),
            abilities: obj.abilities.clone(),
            color: obj.color.clone(),
        };

        // Apply back face characteristics
        obj.name = back_face.name;
        obj.power = back_face.power;
        obj.toughness = back_face.toughness;
        obj.base_power = back_face.power;
        obj.base_toughness = back_face.toughness;
        obj.card_types = back_face.card_types;
        obj.keywords = back_face.keywords.clone();
        obj.base_keywords = back_face.keywords;
        obj.abilities = back_face.abilities;
        obj.color = back_face.color.clone();
        obj.base_color = back_face.color;

        // Store front face data for transforming back
        obj.back_face = Some(current_front);
        obj.transformed = true;
    }

    state.layers_dirty = true;

    events.push(GameEvent::Transformed { object_id });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_dfc(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Werewolf Front".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(3);
        obj.base_power = Some(2);
        obj.base_toughness = Some(3);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Werewolf".to_string()],
        };
        obj.keywords = vec![Keyword::Vigilance];
        obj.base_keywords = vec![Keyword::Vigilance];
        obj.abilities = vec![crate::types::ability::AbilityDefinition {
            kind: crate::types::ability::AbilityKind::Spell,
            effect: crate::types::ability::Effect::Unimplemented {
                name: "FrontAbility".to_string(),
                description: None,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        }];
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];

        obj.back_face = Some(BackFaceData {
            name: "Werewolf Back".to_string(),
            power: Some(4),
            toughness: Some(4),
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Werewolf".to_string()],
            },
            keywords: vec![Keyword::Trample],
            abilities: vec![crate::types::ability::AbilityDefinition {
                kind: crate::types::ability::AbilityKind::Spell,
                effect: crate::types::ability::Effect::Unimplemented {
                    name: "BackAbility".to_string(),
                    description: None,
                },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            }],
            color: vec![ManaColor::Green, ManaColor::Red],
        });

        id
    }

    #[test]
    fn transform_flips_to_back_face() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        let mut events = Vec::new();

        transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(obj.transformed);
        assert_eq!(obj.name, "Werewolf Back");
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(4));
        assert_eq!(obj.keywords, vec![Keyword::Trample]);
        assert_eq!(
            crate::types::ability::effect_variant_name(&obj.abilities[0].effect),
            "BackAbility"
        );
        assert_eq!(obj.color, vec![ManaColor::Green, ManaColor::Red]);
        assert!(state.layers_dirty);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], GameEvent::Transformed { object_id: id });
    }

    #[test]
    fn transform_back_restores_front_face() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        let mut events = Vec::new();

        // Transform to back
        transform_permanent(&mut state, id, &mut events).unwrap();
        // Transform back to front
        transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Werewolf Front");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(3));
        assert_eq!(obj.keywords, vec![Keyword::Vigilance]);
        assert_eq!(
            crate::types::ability::effect_variant_name(&obj.abilities[0].effect),
            "FrontAbility"
        );
        assert_eq!(obj.color, vec![ManaColor::Green]);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn zone_change_resets_transformed() {
        let mut state = GameState::new_two_player(42);
        let id = setup_dfc(&mut state);
        let mut events = Vec::new();

        // Transform to back face
        transform_permanent(&mut state, id, &mut events).unwrap();
        assert!(state.objects[&id].transformed);
        assert_eq!(state.objects[&id].name, "Werewolf Back");

        // Move to graveyard (zone change should reset to front face)
        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Werewolf Front");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(3));
    }

    #[test]
    fn non_dfc_cannot_transform() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Regular Card".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        let result = transform_permanent(&mut state, id, &mut events);
        assert!(result.is_err());
        assert!(events.is_empty());
    }
}
