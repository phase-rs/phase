use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.24a: Shuffle — randomize the cards in a library.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 608.2c + CR 115.1: Resolve the shuffle's acting player. The filter is
    // the source of truth for *which* player. Mirror Draw/Mill/Discard:
    // context-ref filters (Controller, etc.) MUST NOT consult `ability.targets`
    // because chained sub-abilities inherit the parent's Player targets — a
    // `Shuffle { target: Controller }` chained off a Player-targeting parent
    // would otherwise shuffle the wrong library. `Owner` ("its owner's library")
    // resolves against `source_id`, not targets, so it is handled separately.
    let shuffle_target = match &ability.effect {
        Effect::Shuffle { target } => target.clone(),
        _ => TargetFilter::Controller,
    };
    let target_player = if matches!(shuffle_target, TargetFilter::Owner) {
        // CR 400.3: "its owner's library" resolves to the owner of source_id.
        state
            .objects
            .get(&ability.source_id)
            .map(|obj| obj.owner)
            .unwrap_or(ability.controller)
    } else {
        super::resolve_player_for_context_ref(state, ability, &shuffle_target)
    };

    // CR 701.24: "Can't shuffle" suppresses library shuffling. Per CR 701.24d,
    // if a player would shuffle their library and can't, they don't shuffle.
    // The effect itself still resolves (EffectResolved fires below).
    let suppressed =
        crate::game::static_abilities::player_has_static_other(state, target_player, "CantShuffle");

    if !suppressed {
        let GameState { players, rng, .. } = state;
        let player = players
            .iter_mut()
            .find(|p| p.id == target_player)
            .ok_or(EffectError::PlayerNotFound)?;

        // CR 701.24a: Randomize cards so that no player knows their order.
        crate::util::im_ext::shuffle_vector(&mut player.library, rng);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Shuffle,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_shuffle_ability(targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn shuffle_emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_shuffle_ability(vec![]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )));
    }

    #[test]
    fn shuffle_preserves_library_size() {
        let mut state = GameState::new_two_player(42);
        for i in 0..10 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let original_ids: Vec<_> = state.players[0].library.iter().copied().collect();

        let ability = make_shuffle_ability(vec![]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let shuffled_ids: Vec<_> = state.players[0].library.iter().copied().collect();
        assert_eq!(shuffled_ids.len(), original_ids.len());
        let mut sorted_original = original_ids.clone();
        let mut sorted_shuffled = shuffled_ids.clone();
        sorted_original.sort_by_key(|id| id.0);
        sorted_shuffled.sort_by_key(|id| id.0);
        assert_eq!(sorted_original, sorted_shuffled);
    }

    #[test]
    fn cant_shuffle_preserves_library_order() {
        // CR 701.24: A player under "Can't shuffle" doesn't shuffle their library.
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        for i in 0..20 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let before = state.players[0].library.clone();

        // Install CantShuffle static controlled by the affected player.
        let source = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Aven Mindcensor".to_string(),
            Zone::Battlefield,
        );
        use crate::types::ability::{ControllerRef, TypedFilter};
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantShuffle".to_string())).affected(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
                ),
            );

        let ability = make_shuffle_ability(vec![]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].library, before,
            "library order must be preserved under CantShuffle"
        );
        // EffectResolved still fires.
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )));
    }

    #[test]
    fn shuffle_targets_specified_player() {
        // CR 608.2c: For non-context-ref filters (e.g. `Any` from "target
        // player shuffles their library"), the chosen TargetRef::Player wins.
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let p1_lib_before = state.players[1].library.clone();
        let p0_lib_before = state.players[0].library.clone();

        // Mirrors the parser's "target/that player shuffles" emission
        // (`TargetFilter::Player`).
        let ability = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].library, p0_lib_before);
        assert_eq!(state.players[1].library.len(), p1_lib_before.len());
    }

    #[test]
    fn shuffle_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained Shuffle whose `target` is the
        // context-ref `Controller` must shuffle the spell controller's library,
        // even when an inherited `TargetRef::Player` from the parent is in
        // `ability.targets`. Mirrors the Discard / Draw / Mill guard.
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let p1_lib_before = state.players[1].library.clone();
        let p0_lib_before = state.players[0].library.clone();

        // Inherited parent target = P1; filter = Controller (so the spell
        // controller P0 must be the one whose library shuffles).
        let ability = make_shuffle_ability(vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // P1's library is unchanged (parent target inherited, but ignored).
        // P0's library size preserved (it was shuffled in place).
        assert_eq!(
            state.players[1].library, p1_lib_before,
            "P1's library must NOT be shuffled — Controller filter resolves to caster (P0)"
        );
        assert_eq!(state.players[0].library.len(), p0_lib_before.len());
    }

    /// CR 608.2c + CR 701.24a: Assassin's Trophy-shape — `Effect::Shuffle
    /// { target: ParentTargetController }` resolves the acting player from the
    /// first Object in `ability.targets` (the destroyed permanent), and shuffles
    /// that object's controller's library. Works on the fail-to-find path where
    /// the `SearchChoice` continuation never injected a Player target.
    #[test]
    fn shuffle_parent_target_controller_shuffles_target_objects_controller() {
        use crate::types::ability::Effect;
        let mut state = GameState::new_two_player(42);
        for i in 0..4 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let destroyed = create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Opponent Land".to_string(),
            Zone::Graveyard,
        );
        let p0_lib_before = state.players[0].library.clone();
        let p1_lib_before = state.players[1].library.clone();

        // Ability.controller = caster (P0). Target = destroyed permanent (P1-owned).
        // No TargetRef::Player in targets — must resolve via ParentTargetController.
        let ability = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::ParentTargetController,
            },
            vec![TargetRef::Object(destroyed)],
            ObjectId(9000),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Caster's library untouched; opponent's library shuffled.
        assert_eq!(
            state.players[0].library, p0_lib_before,
            "caster's library must not be shuffled"
        );
        assert_eq!(
            state.players[1].library.len(),
            p1_lib_before.len(),
            "opponent's library size preserved under shuffle"
        );
    }

    /// CR 400.3 + CR 701.24: `Effect::Shuffle { target: Owner }` must route to the
    /// owner of `ability.source_id`, NOT the ability's controller. This is the
    /// shuffle-back path for Nexus of Fate / Blightsteel under Mind Control:
    /// opponent controls the card, but it shuffles into its owner's library.
    #[test]
    fn shuffle_owner_routes_to_source_objects_owner_not_controller() {
        use crate::types::ability::Effect;
        let mut state = GameState::new_two_player(42);
        // P0 owns the Blightsteel; P1 currently controls it (Mind Control).
        // Populate both libraries so we can distinguish.
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("P0 Card {}", i),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(10 + i + 1),
                PlayerId(1),
                format!("P1 Card {}", i),
                Zone::Library,
            );
        }
        let blightsteel = create_object(
            &mut state,
            CardId(100),
            PlayerId(0), // owner
            "Blightsteel Colossus".to_string(),
            Zone::Library,
        );
        // Simulate Mind Control: P1 controls the P0-owned card.
        if let Some(obj) = state.objects.get_mut(&blightsteel) {
            obj.controller = PlayerId(1);
        }
        let p0_lib_before = state.players[0].library.clone();
        let p1_lib_before = state.players[1].library.clone();

        // ability.controller = P1 (thief), source_id = the P0-owned Blightsteel.
        let ability = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Owner,
            },
            vec![],
            blightsteel,
            PlayerId(1),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Owner's (P0's) library shuffled; thief's (P1's) library untouched.
        assert_eq!(
            state.players[1].library, p1_lib_before,
            "thief's library must not be shuffled — ownership, not control, is authoritative"
        );
        assert_eq!(
            state.players[0].library.len(),
            p0_lib_before.len(),
            "owner's library size preserved under shuffle"
        );
    }
}
