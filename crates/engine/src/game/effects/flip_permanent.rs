use crate::game::flip::flip_permanent;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 710.4: Flip a Kamigawa flip permanent to its alternative face.
///
/// Every printed flip instruction names the permanent itself ("flip this
/// creature" / "flip it" / "flip <name>"), so the no-target form resolving
/// against `source_id` is the production shape; the explicit-object-target form
/// mirrors `transform_effect` so an anaphoric trigger subject can bind a
/// different permanent.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::FlipPermanent { .. } => {}
        _ => {
            return Err(EffectError::InvalidParam(
                "expected FlipPermanent effect".to_string(),
            ))
        }
    }

    // CR 710.1: if the named permanent isn't represented by a flip card, or
    // isn't on the battlefield, `flip_permanent` no-ops (CR 710.2).
    let object_id = match ability.targets.as_slice() {
        [TargetRef::Object(object_id)] => *object_id,
        [] => ability.source_id,
        _ => {
            return Err(EffectError::InvalidParam(
                "flip expects exactly one object target".to_string(),
            ))
        }
    };

    // CR 400.7: a self-flip instruction must not follow a source that left the
    // battlefield and returned — that is a new object (a new, unflipped
    // permanent per CR 110.5b), not the one the ability was put on the stack
    // for. CR 710.4 already makes a repeat flip of the SAME object a no-op, so
    // no transformation-count-style generation guard is needed here.
    let stale_self_flip = object_id == ability.source_id && !ability.source_is_current(state);
    if !stale_self_flip {
        flip_permanent(state, object_id, events)
            .map_err(|err| EffectError::InvalidParam(err.to_string()))?;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::FlipPermanent,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::{CardType, CoreType, Supertype};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Nezumi Shortfang // Stabwhisker the Odious — a {1}{B} 1/1 Rat Rogue
    /// whose alternative half is a 3/3 Legendary Rat Shaman.
    fn setup_flip_card(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Nezumi Shortfang".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Rat".to_string(), "Rogue".to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        };
        obj.base_mana_cost = obj.mana_cost.clone();
        obj.color = vec![ManaColor::Black];
        obj.base_color = obj.color.clone();
        obj.back_face = Some(BackFaceData {
            name: "Stabwhisker the Odious".to_string(),
            power: Some(3),
            toughness: Some(3),
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Rat".to_string(), "Shaman".to_string()],
            },
            mana_cost: ManaCost::default(),
            keywords: vec![Keyword::Menace],
            abilities: Vec::new(),
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: Vec::new(),
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: Some(crate::types::card::LayoutKind::Flip),
        });
        id
    }

    /// CR 710.4: the no-target form flips the ability's source — the shape
    /// every printed flip card produces.
    #[test]
    fn flip_effect_uses_source_when_no_explicit_target() {
        let mut state = GameState::new_two_player(42);
        let source_id = setup_flip_card(&mut state);
        let ability = ResolvedAbility::new(
            Effect::FlipPermanent {
                target: TargetFilter::SelfRef,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let object = &state.objects[&source_id];
        assert!(object.flipped);
        assert_eq!(object.name, "Stabwhisker the Odious");
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::Flipped { object_id } if *object_id == source_id)
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::FlipPermanent,
                source_id: emitted_source,
                ..
            } if *emitted_source == source_id
        )));
    }

    /// CR 710.1c: the resolver path preserves color and mana cost, because it
    /// routes through `flip::flip_permanent` and not the double-faced
    /// applicator.
    #[test]
    fn flip_effect_preserves_color_and_mana_cost() {
        let mut state = GameState::new_two_player(42);
        let source_id = setup_flip_card(&mut state);
        let cost_before = state.objects[&source_id].mana_cost.clone();
        let ability = ResolvedAbility::new(
            Effect::FlipPermanent {
                target: TargetFilter::SelfRef,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let object = &state.objects[&source_id];
        assert_eq!(object.mana_cost, cost_before);
        assert_eq!(object.color, vec![ManaColor::Black]);
    }

    /// An explicit object target flips that permanent, not the source.
    #[test]
    fn flip_effect_uses_explicit_object_target() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target_id = setup_flip_card(&mut state);
        let ability = ResolvedAbility::new(
            Effect::FlipPermanent {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&target_id].flipped);
        assert!(!state.objects[&source_id].flipped);
    }

    /// CR 400.7: a self-flip instruction on the stack must not follow a source
    /// that left and re-entered the battlefield — that is a new object.
    #[test]
    fn self_flip_does_not_follow_a_blinked_source() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::stack::push_to_stack;
        use crate::game::zones::move_to_zone;
        use crate::types::ability::{AbilityDefinition, AbilityKind};
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let source_id = setup_flip_card(&mut state);
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipPermanent {
                target: TargetFilter::SelfRef,
            },
        );
        let ability = build_resolved_from_def(&definition, source_id, PlayerId(0));
        let mut events = Vec::new();
        push_to_stack(
            &mut state,
            StackEntry {
                id: ObjectId(100),
                source_id,
                controller: PlayerId(0),
                kind: StackEntryKind::ActivatedAbility { source_id, ability },
            },
            &mut events,
        );

        move_to_zone(&mut state, source_id, Zone::Exile, &mut events);
        move_to_zone(&mut state, source_id, Zone::Battlefield, &mut events);

        let entry = state.stack.pop_back().expect("flip ability on stack");
        resolve(
            &mut state,
            entry.ability().expect("activated ability"),
            &mut events,
        )
        .expect("flip ability resolves");

        assert!(
            !state.objects[&source_id].flipped,
            "CR 400.7: a stale self-flip must not affect the re-entered source"
        );
    }
}
