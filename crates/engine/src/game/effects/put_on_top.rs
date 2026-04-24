use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, LibraryPosition, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.24g / CR 401.3: Place target card at a specific position in its owner's
/// library. Unlike ChangeZone { destination: Library } which auto-shuffles per
/// CR 401.3, this places at a specific position without shuffling.
///
/// Also handles LTB self-return triggers (CR 603.10a) such as Avenging Angel:
/// "When this creature dies, you may put it on top of its owner's library."
/// When the trigger resolves, the source is already in the graveyard. The parser
/// emits `target: ParentTarget` (or `SelfRef`) with empty `ability.targets`; the
/// resolver treats that as a self-reference to `ability.source_id`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (position, target_filter) = match &ability.effect {
        Effect::PutAtLibraryPosition { position, target } => (position.clone(), target.clone()),
        _ => (LibraryPosition::Top, TargetFilter::None),
    };

    // CR 608.2c + 603.10a: An anaphoric "it" / "~" in a top-level trigger effect
    // has no parent target to inherit from — it refers to the source object.
    // `TriggeringSource` is deliberately excluded: it resolves via
    // `state.current_trigger_event`, not the ability source.
    let use_self = matches!(
        target_filter,
        TargetFilter::None | TargetFilter::SelfRef | TargetFilter::ParentTarget
    ) && ability.targets.is_empty();

    let object_id = if use_self {
        ability.source_id
    } else {
        ability
            .targets
            .iter()
            .find_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .ok_or(EffectError::InvalidParam(
                "PutAtLibraryPosition requires a target".to_string(),
            ))?
    };

    let index = match position {
        // CR 701.24g: Top = index 0, Bottom = None (push to end),
        // NthFromTop = index n-1 ("second from the top" = index 1).
        LibraryPosition::Top => Some(0),
        LibraryPosition::Bottom => None,
        LibraryPosition::NthFromTop { n } => Some(n.saturating_sub(1) as usize),
    };
    zones::move_to_library_at_index(state, object_id, index, events);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PutAtLibraryPosition,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn test_resolve_puts_card_on_top_of_library() {
        let mut state = GameState::new_two_player(42);
        // Create two cards in the library
        let _id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );

        // id2 is at the end of the library; put it on top
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                position: LibraryPosition::Top,
            },
            vec![TargetRef::Object(id2)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // id2 should now be at library[0] (top)
        assert_eq!(state.players[0].library[0], id2);
        assert_eq!(state.objects[&id2].zone, Zone::Library);
    }

    #[test]
    fn test_resolve_does_not_shuffle_library() {
        let mut state = GameState::new_two_player(42);
        // Create three cards to verify order is preserved
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        let id3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Card C".to_string(),
            Zone::Library,
        );

        // Record order before: [id1, id2, id3]
        let lib = &state.players[0].library;
        let before_order: Vec<_> = lib.iter().copied().collect();
        assert_eq!(before_order, vec![id1, id2, id3]);

        // Put id2 on top
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                position: LibraryPosition::Top,
            },
            vec![TargetRef::Object(id2)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Expected: [id2, id1, id3] — id2 on top, rest preserved in order
        let lib = &state.players[0].library;
        let after_order: Vec<_> = lib.iter().copied().collect();
        assert_eq!(after_order, vec![id2, id1, id3]);
    }

    #[test]
    fn test_resolve_puts_card_on_bottom() {
        let mut state = GameState::new_two_player(42);
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        // Put id1 on bottom
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                position: LibraryPosition::Bottom,
            },
            vec![TargetRef::Object(id1)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // id1 should be at the bottom, id2 on top
        let lib = &state.players[0].library;
        assert_eq!(*lib.last().unwrap(), id1);
        assert_eq!(lib[0], id2);
    }

    /// CR 603.10a / Avenging Angel class: LTB self-return triggers fire after
    /// the source has moved to the graveyard. The parsed effect is
    /// `PutAtLibraryPosition { target: ParentTarget }` with empty
    /// `ability.targets`; the resolver must treat that as "put the source object
    /// from the graveyard on top of its owner's library."
    #[test]
    fn test_put_on_top_ltb_self_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Avenging Angel".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ParentTarget,
                position: LibraryPosition::Top,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].graveyard.contains(&obj_id));
        assert_eq!(state.players[0].library[0], obj_id);
        assert_eq!(state.objects[&obj_id].zone, Zone::Library);
    }

    #[test]
    fn test_put_on_top_ltb_self_ref_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Selfsame".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::SelfRef,
                position: LibraryPosition::Top,
            },
            vec![],
            obj_id,
            PlayerId(1),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[1].graveyard.contains(&obj_id));
        assert_eq!(state.players[1].library[0], obj_id);
    }

    /// End-to-end Avenging Angel-class pipeline test.
    #[test]
    fn test_put_on_top_ltb_pipeline_returns_to_top_of_library() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let angel_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Avenging Angel".to_string(),
            Zone::Battlefield,
        );

        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.trigger_zones = vec![Zone::Graveyard];
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ParentTarget,
                position: LibraryPosition::Top,
            },
        )));
        state
            .objects
            .get_mut(&angel_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, angel_id, Zone::Graveyard, &mut events);
        assert!(state.players[0].graveyard.contains(&angel_id));

        process_triggers(&mut state, &events);
        assert_eq!(state.stack.len(), 1, "LTB trigger did not reach the stack");

        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        assert_eq!(
            state.players[0].library[0], angel_id,
            "Avenging Angel should be on top of its owner's library"
        );
        assert!(!state.players[0].graveyard.contains(&angel_id));
    }

    #[test]
    fn test_resolve_puts_card_nth_from_top() {
        let mut state = GameState::new_two_player(42);
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        let id3 = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Card C".to_string(),
            Zone::Library,
        );
        let id4 = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Card D".to_string(),
            Zone::Hand,
        );

        // Library is [id1, id2, id3]. Put id4 (from hand) third from top.
        let ability = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                position: LibraryPosition::NthFromTop { n: 3 },
            },
            vec![TargetRef::Object(id4)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // "third from the top" = index 2: [id1, id2, id4, id3]
        let lib: Vec<_> = state.players[0].library.iter().copied().collect::<Vec<_>>();
        assert_eq!(lib, vec![id1, id2, id4, id3]);
        assert_eq!(state.objects[&id4].zone, Zone::Library);
    }
}
