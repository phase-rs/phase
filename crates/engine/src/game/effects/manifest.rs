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
    let (target, count, profile, enters_under) = match &ability.effect {
        Effect::Manifest {
            target,
            count,
            profile,
            enters_under,
        } => (
            target.clone(),
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
            profile.clone(),
            enters_under.clone(),
        ),
        _ => return Err(EffectError::MissingParam("count".to_string())),
    };

    // `player` is the LIBRARY OWNER (whose top cards are manifested), resolved
    // from `target`. `controller` is the optional CR 110.2a override for which
    // player the cards enter the battlefield under ("under your control").
    let player = super::resolve_player_for_context_ref(state, ability, &target);
    // CR 110.2a: Resolve the optional controller override through the single
    // canonical authority shared with `ChangeZone`/`ChangeZoneAll` — never a
    // hand-rolled second resolver (per the single-authority rule). `None` keeps
    // each manifested card under its library owner's control (CR 701.40a).
    let controller = super::change_zone::resolve_enters_under_player(
        state,
        ability,
        "Manifest",
        enters_under.as_ref(),
    )?;

    // CR 708.2a: Use the effect-specified face-down profile when present
    // ("They're 2/2 Cyberman artifact creatures."), otherwise the vanilla 2/2
    // manifest default (CR 701.40a).
    let profile = profile.unwrap_or_else(crate::types::ability::FaceDownProfile::vanilla_2_2);

    // CR 701.40e: Manifest cards one at a time
    for _ in 0..count {
        // CR 701.40a: Resolve the top card of the library owner's library, then
        // manifest it through the shared morph infrastructure, routing the
        // effect-specified profile and the optional controller override.
        let object_id = match crate::game::morph::top_library_object(state, player) {
            Ok(id) => id,
            // The library owner has no cards left — stop manifesting.
            Err(_) => break,
        };
        crate::game::morph::manifest_card(
            state,
            player,
            object_id,
            ability.source_id,
            profile.clone(),
            controller,
            events,
        )
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
                profile: None,
                enters_under: None,
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
                profile: None,
                enters_under: None,
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
    fn manifest_parent_target_controller_falls_back_to_effect_context_object() {
        // CR 608.2c + CR 400.7j (issue #2890): Reality Shift's chained manifest
        // must resolve the exiled creature's controller from the propagated
        // referent snapshot when inherited targets are absent.
        let mut state = GameState::new_two_player(42);
        let target_controller = PlayerId(1);

        let lib_card = create_object(
            &mut state,
            CardId(1),
            target_controller,
            "Opponent Card".to_string(),
            Zone::Library,
        );

        let mut ability =
            make_manifest_ability_for_target(1, TargetFilter::ParentTargetController, vec![]);
        ability.effect_context_object = Some(crate::types::ability::CostPaidObjectSnapshot {
            object_id: ObjectId(404),
            lki: crate::types::game_state::LKISnapshot {
                name: "Exiled Creature".to_string(),
                power: Some(2),
                toughness: Some(2),
                base_power: Some(2),
                base_toughness: Some(2),
                mana_value: 2,
                controller: target_controller,
                owner: target_controller,
                card_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: std::collections::HashMap::new(),
                tapped: false,
            },
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

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

    /// CR 708.2a + CR 110.2a: Manifest with an effect-specified `profile`
    /// (the Cybership "2/2 Cyberman artifact creature" shape) and an
    /// `enters_under` controller override must move the LIBRARY OWNER's top
    /// cards onto the battlefield face down, applying the profile's
    /// characteristics, while routing control to the override player. Exercises
    /// the new `profile`/`enters_under` parameters, not Cybership specifically.
    #[test]
    fn manifest_with_profile_and_controller_override() {
        use crate::types::ability::{FaceDownProfile, ResolvedAbility};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0); // "under your control"
        let library_owner = PlayerId(1); // "that player's library"

        let card1 = create_object(
            &mut state,
            CardId(1),
            library_owner,
            "Owner Top".to_string(),
            Zone::Library,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            library_owner,
            "Owner Next".to_string(),
            Zone::Library,
        );

        let profile = FaceDownProfile {
            power: None,
            toughness: None,
            body: crate::types::ability::FaceDownBody::Creature,
            extra_core_types: vec![CoreType::Artifact],
            subtypes: vec!["Cyberman".to_string()],
            ward: None,
        };
        let ability = ResolvedAbility::new(
            Effect::Manifest {
                target: TargetFilter::TriggeringPlayer,
                count: QuantityExpr::Fixed { value: 2 },
                profile: Some(profile),
                enters_under: Some(crate::types::ability::ControllerRef::You),
            },
            vec![],
            ObjectId(100),
            controller,
        );

        // Bind TriggeringPlayer to the library owner via a combat-damage event.
        let source = create_object(
            &mut state,
            CardId(3),
            controller,
            "Cybership".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(library_owner),
            amount: 4,
            is_combat: true,
            excess: 0,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for id in [card1, card2] {
            let obj = &state.objects[&id];
            assert!(obj.face_down, "card {id:?} should be face down");
            assert_eq!(obj.zone, Zone::Battlefield);
            assert_eq!(obj.power, Some(2));
            assert_eq!(obj.toughness, Some(2));
            // CR 708.2a: Creature is always present + Artifact layered on.
            assert!(obj.card_types.core_types.contains(&CoreType::Creature));
            assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
            assert_eq!(obj.card_types.subtypes, vec!["Cyberman".to_string()]);
            // CR 110.2a: enters under the override controller, not the owner.
            assert_eq!(obj.controller, controller);
            assert_eq!(obj.owner, library_owner);
        }
    }

    /// CR 701.40a: Without a `profile`/`enters_under` override, Manifest still
    /// yields the vanilla 2/2 default under the library owner's control —
    /// confirming the new fields are additive and the existing behavior is
    /// preserved.
    #[test]
    fn manifest_without_overrides_is_vanilla_under_owner() {
        let mut state = GameState::new_two_player(42);
        let owner = PlayerId(0);
        let id = create_object(
            &mut state,
            CardId(1),
            owner,
            "Vanilla".to_string(),
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
        assert_eq!(
            obj.card_types.core_types,
            vec![crate::types::card_type::CoreType::Creature]
        );
        assert!(obj.card_types.subtypes.is_empty());
        assert_eq!(obj.controller, owner, "no override → owner controls");
    }
}
