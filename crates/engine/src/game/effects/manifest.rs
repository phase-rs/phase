use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.40a: Manifest — turn the top card of a player's library face down,
/// making it a 2/2 creature with no text, no name, no subtypes, and no mana cost,
/// and put it onto the battlefield.
///
/// CR 701.40e: If manifesting multiple cards, manifest them one at a time.
///
/// The acting player is resolved from `Effect::Manifest { target }`:
/// - `Controller` — the ability's controller ("you manifest...").
/// - `ParentTargetController` — the controller of the parent target object.
/// - `TriggeringPlayer` — the player involved in the triggering event
///   ("that player's library").
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target, count) = match &ability.effect {
        Effect::Manifest { target, count } => (
            target.clone(),
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
        ),
        _ => return Err(EffectError::MissingParam("count".to_string())),
    };

    let player = super::resolve_player_for_context_ref(state, ability, &target);

    // CR 701.40e: Manifest cards one at a time
    for _ in 0..count {
        let has_cards = state
            .players
            .iter()
            .find(|p| p.id == player)
            .map(|p| !p.library.is_empty())
            .unwrap_or(false);

        if !has_cards {
            break;
        }

        // CR 701.40a: Manifest the top card using the shared morph infrastructure
        crate::game::morph::manifest(state, player, events)
            .map_err(|e| EffectError::MissingParam(format!("{e}")))?;
    }

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
    use crate::types::ability::{QuantityExpr, TargetFilter, TargetRef};
    use crate::types::events::GameEvent;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_manifest_ability(count: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Manifest {
                target: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: count },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_manifest_ability_for_target(
        count: i32,
        target_filter: TargetFilter,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Manifest {
                target: target_filter,
                count: QuantityExpr::Fixed { value: count },
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn manifest_single_card() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = create_object(
            &mut state,
            CardId(1),
            player,
            "Test Card".to_string(),
            Zone::Library,
        );

        let ability = make_manifest_ability(1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(obj.face_down);
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
    }

    #[test]
    fn manifest_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id1 = create_object(
            &mut state,
            CardId(1),
            player,
            "Card A".to_string(),
            Zone::Library,
        );
        let id2 = create_object(
            &mut state,
            CardId(2),
            player,
            "Card B".to_string(),
            Zone::Library,
        );

        let ability = make_manifest_ability(2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Both should be manifested face-down on battlefield
        for id in [id1, id2] {
            let obj = &state.objects[&id];
            assert!(obj.face_down, "Card {id:?} should be face down");
            assert_eq!(obj.zone, Zone::Battlefield);
            assert_eq!(obj.power, Some(2));
            assert_eq!(obj.toughness, Some(2));
        }
    }

    #[test]
    fn manifest_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_manifest_ability(1);
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
    }

    #[test]
    fn manifest_more_than_library_manifests_available() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        create_object(
            &mut state,
            CardId(1),
            player,
            "Only Card".to_string(),
            Zone::Library,
        );

        // Try to manifest 3, but only 1 card in library
        let ability = make_manifest_ability(3);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have manifested the one available card
        let battlefield_count = state
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.face_down)
            .count();
        assert_eq!(battlefield_count, 1);
    }

    #[test]
    fn manifest_parent_target_controller_uses_target_owners_library() {
        // CR 701.40a + CR 608.2c: "its controller manifests the top card of
        // their library" — the acting player is the controller of the parent
        // target object (Reality Shift).
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let target_controller = PlayerId(1);

        // Put a card in the target controller's library.
        let lib_card = create_object(
            &mut state,
            CardId(1),
            target_controller,
            "Opponent Card".to_string(),
            Zone::Library,
        );
        // Also put a card in caster's library to verify it's not used.
        create_object(
            &mut state,
            CardId(2),
            caster,
            "My Card".to_string(),
            Zone::Library,
        );

        // Create the parent target object (the exiled creature) owned/controlled
        // by the opposing player.
        let parent_target_id = create_object(
            &mut state,
            CardId(3),
            target_controller,
            "Exiled Creature".to_string(),
            Zone::Exile,
        );

        let ability = make_manifest_ability_for_target(
            1,
            TargetFilter::ParentTargetController,
            vec![TargetRef::Object(parent_target_id)],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The opponent's top library card should be manifested, not the caster's.
        let obj = &state.objects[&lib_card];
        assert!(obj.face_down);
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.controller, target_controller);
    }

    #[test]
    fn manifest_triggering_player_uses_damaged_players_library() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let damaged_player = PlayerId(1);

        let opponent_card = create_object(
            &mut state,
            CardId(1),
            damaged_player,
            "Damaged Player Card".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            caster,
            "Caster Card".to_string(),
            Zone::Library,
        );
        let source = create_object(
            &mut state,
            CardId(3),
            caster,
            "Orochi Soul-Reaver".to_string(),
            Zone::Battlefield,
        );

        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(damaged_player),
            amount: 5,
            is_combat: true,
            excess: 0,
        });

        let ability = make_manifest_ability_for_target(1, TargetFilter::TriggeringPlayer, vec![]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&opponent_card];
        assert!(obj.face_down);
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.controller, damaged_player);
    }
}
