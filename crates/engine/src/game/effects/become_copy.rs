use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 707.2 / CR 613.1a: Become a copy of target permanent.
/// Copies the copiable characteristics (name, mana cost, color, types, subtypes,
/// power/toughness, abilities, keywords) from the target to the source.
/// This modifies the base characteristics so the layer system sees them at Layer 1.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 707.2: The target is the object whose characteristics are copied.
    let target_id = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .ok_or_else(|| EffectError::MissingParam("BecomeCopy requires a target".to_string()))?;

    let source_id = ability.source_id;

    // Read copiable characteristics from the target.
    // CR 707.2: Copiable values are name, mana cost, color, card type/subtype/supertype,
    // rules text (abilities), power, toughness, and loyalty.
    let target = state
        .objects
        .get(&target_id)
        .ok_or(EffectError::ObjectNotFound(target_id))?;

    // Snapshot the copiable characteristics from the target's base values.
    let name = target.name.clone();
    let mana_cost = target.mana_cost.clone();
    let color = target.base_color.clone();
    let card_types = target.base_card_types.clone();
    let power = target.base_power;
    let toughness = target.base_toughness;
    let loyalty = target.loyalty;
    let keywords = target.base_keywords.clone();
    let abilities = target.base_abilities.clone();
    let trigger_definitions = target.base_trigger_definitions.clone();
    let replacement_definitions = target.base_replacement_definitions.clone();
    let static_definitions = target.base_static_definitions.clone();

    // Apply characteristics to the source object.
    let source = state
        .objects
        .get_mut(&source_id)
        .ok_or(EffectError::ObjectNotFound(source_id))?;

    source.name = name;
    source.mana_cost = mana_cost;
    source.base_color = color.clone();
    source.color = color;
    source.base_card_types = card_types.clone();
    source.card_types = card_types;
    source.base_power = power;
    source.power = power;
    source.base_toughness = toughness;
    source.toughness = toughness;
    source.loyalty = loyalty;
    source.base_keywords = keywords.clone();
    source.keywords = keywords;
    source.base_abilities = abilities.clone();
    source.abilities = abilities;
    source.base_trigger_definitions = trigger_definitions.clone();
    source.trigger_definitions = trigger_definitions;
    source.base_replacement_definitions = replacement_definitions.clone();
    source.replacement_definitions = replacement_definitions;
    source.base_static_definitions = static_definitions.clone();
    source.static_definitions = static_definitions;

    state.layers_dirty = true;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter, TargetRef};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn become_copy_copies_characteristics() {
        let mut state = GameState::new_two_player(42);

        // Create target creature with specific characteristics
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.power = Some(2);
            target.toughness = Some(2);
            target.base_color = vec![ManaColor::Green];
            target.color = vec![ManaColor::Green];
            target.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            };
            target.card_types = target.base_card_types.clone();
            target.base_keywords = vec![Keyword::Trample];
            target.keywords = target.base_keywords.clone();
        }

        // Create source creature that will become a copy
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Copy Source".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_power = Some(1);
            source.base_toughness = Some(1);
            source.power = Some(1);
            source.toughness = Some(1);
        }

        let mut events = Vec::new();
        let ability = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                duration: None,
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        let source = state.objects.get(&source_id).unwrap();
        assert_eq!(source.name, "Target Bear");
        assert_eq!(source.power, Some(2));
        assert_eq!(source.toughness, Some(2));
        assert_eq!(source.color, vec![ManaColor::Green]);
        assert!(source.card_types.core_types.contains(&CoreType::Creature));
        assert!(source.card_types.subtypes.contains(&"Bear".to_string()));
        assert!(source.keywords.contains(&Keyword::Trample));
        assert!(state.layers_dirty);
    }
}
