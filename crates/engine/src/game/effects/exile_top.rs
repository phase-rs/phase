use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, player_filter, face_down) = match &ability.effect {
        Effect::ExileTop {
            count,
            player,
            face_down,
        } => (
            // Use resolve_quantity_with_targets so that TargetZoneCardCount (and
            // DivideRounded wrapping it) can resolve against the targeted player.
            resolve_quantity_with_targets(state, count, ability) as usize,
            player.clone(),
            *face_down,
        ),
        _ => return Err(EffectError::MissingParam("ExileTop count".to_string())),
    };

    // CR 115.1: Mirror Draw/Mill/Discard — context-ref filters (Controller, etc.)
    // must consult state slots, not `ability.targets`. Otherwise a chained
    // sub-ability's "exile the top N cards of your library" would inherit the
    // parent's Player target and exile from the wrong library.
    let target_player = super::resolve_player_for_context_ref(state, ability, &player_filter);

    // CR 701.17b: A player can't mill/exile more cards than are in their library;
    // exile as many as possible.
    let player = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .ok_or(EffectError::PlayerNotFound)?;
    let count = count.min(player.library.len());
    let top_cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();
    let track_exiled_by_source =
        crate::game::exile_links::should_track_exiled_by_source(state, ability.source_id, ability);

    // CR 603.7: Tracked-set publishing for ExileTop is handled by the
    // generic chain processor in `effects::resolve_ability_chain` via
    // `affected_objects_from_events` (which already maps `ExileTop` to the
    // Exile destination zone). Publishing here as well would double-count
    // the moved objects in the unified set — see the
    // `compound_zone_change_chain_unifies_tracked_set` regression. Mirrors
    // `change_zone::resolve`, which likewise delegates publishing to the
    // chain processor.
    for object_id in top_cards {
        zones::move_to_zone(state, object_id, Zone::Exile, events);
        if track_exiled_by_source {
            crate::game::exile_links::push_tracked_by_source(state, object_id, ability.source_id);
        }
        // CR 406.3: A card exiled face down can't be examined by any player
        // except when instructions allow it. Set the moved object's
        // face-down state immediately after the zone change (mirrors the
        // foretell pattern in `casting.rs`) so `visibility.rs`'s
        // per-viewer redaction hides the card unless a separate effect grants
        // look permission (Necropotence / Bomat Courier / Asmodeus class).
        if face_down {
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.face_down = true;
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileTop,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CardTypeSetSource, ControllerRef, FilterProp,
        LinkedExileScope, ManaProduction, QuantityExpr, QuantityRef, TargetFilter, TargetRef,
        TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_exile_top_ability(count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed {
                    value: count as i32,
                },
                face_down: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn exile_top_moves_top_card_of_controller_library() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );
        let bottom = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bottom".to_string(),
            Zone::Library,
        );
        let ability = make_exile_top_ability(1);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&bottom).map(|obj| obj.zone),
            Some(Zone::Library)
        );
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn exile_top_tracks_when_source_has_linked_exile_mana_consumer() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Pit Style Land".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::ChoiceAmongExiledColors {
                    source: LinkedExileScope::ThisObject,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )]);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );
        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, top);
        assert_eq!(state.exile_links[0].source_id, source);
    }

    #[test]
    fn exile_top_triggering_player_uses_attacking_players_library() {
        let mut state = GameState::new_two_player(42);
        let controller_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Controller Top".to_string(),
            Zone::Library,
        );
        let opponent_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Top".to_string(),
            Zone::Library,
        );
        let attacker = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(0),
            attacks: vec![(
                attacker,
                crate::game::combat::AttackTarget::Player(PlayerId(0)),
            )],
        });
        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::TriggeringPlayer,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&opponent_top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&controller_top).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_moves_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Third".to_string(),
            Zone::Library,
        );
        let ability = make_exile_top_ability(2);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&second).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&third).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained ExileTop with `player: Controller`
        // must exile from the spell controller's library, not the parent's
        // inherited Player target.
        let mut state = GameState::new_two_player(42);
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 top".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&p0_top).map(|obj| obj.zone),
            Some(Zone::Exile),
            "P0's library top should be exiled (Controller filter resolves to caster)"
        );
        assert_eq!(
            state.objects.get(&p1_top).map(|obj| obj.zone),
            Some(Zone::Library),
            "P1's library must NOT be exiled — parent target inheritance must not override Controller filter"
        );
    }

    #[test]
    fn exile_top_dynamic_card_type_count_moves_that_many_cards() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Loot, the Key to Everything".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let artifact = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact];

        let enchantment = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&enchantment)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Enchantment];

        let creature = create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Third".to_string(),
            Zone::Library,
        );
        let fourth = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Fourth".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypes {
                        source: CardTypeSetSource::Objects {
                            filter: TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Permanent)
                                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
                                    .controller(ControllerRef::You)
                                    .properties(vec![FilterProp::Another]),
                            ),
                        },
                    },
                },
                face_down: false,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&second).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&third).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&fourth).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_with_empty_library_resolves_without_error() {
        let mut state = GameState::new_two_player(42);
        let ability = make_exile_top_ability(3);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::ExileTop,
                ..
            }
        )));
    }

    /// CR 603.7 + CR 406.1: `ExileTop` must publish a tracked set when a
    /// downstream `CreateDelayedTrigger { uses_tracked_set: true }` consumes
    /// it. Necropotence / Bomat Courier / Asmodeus class: the recall delayed
    /// trigger binds via `TargetFilter::TrackedSet { id: 0 }` (sentinel
    /// resolved to the most recently published set on this resolution chain).
    /// Without the publish, the recall would have an empty set and never
    /// return the exiled card.
    #[test]
    fn exile_top_publishes_tracked_set_when_followed_by_recall_delayed_trigger() {
        use crate::types::ability::DelayedTriggerCondition;
        use crate::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );
        let _bottom = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bottom".to_string(),
            Zone::Library,
        );

        // Build the Necropotence activated ability shape:
        // ExileTop -> sub_ability: CreateDelayedTrigger{uses_tracked_set: true,
        //   effect: ChangeZone{ origin: Exile, destination: Hand,
        //     target: TrackedSet{id: 0} }}
        let recall_inner = crate::types::ability::AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Exile),
                destination: Zone::Hand,
                target: TargetFilter::TrackedSet {
                    id: crate::types::identifiers::TrackedSetId(0),
                },
                enters_under: None,
                enter_transformed: false,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                owner_library: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: PlayerId(0),
                },
                effect: Box::new(recall_inner),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut top_ability = make_exile_top_ability(1);
        top_ability.sub_ability = Some(Box::new(delayed));

        // CR 603.7: Drive through `resolve_ability_chain` so the generic
        // chain processor's tracked-set publish runs (mirrors live game
        // resolution); the leaf `exile_top::resolve` deliberately delegates
        // publishing to the chain layer to avoid double-counting.
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &top_ability, &mut events, 0)
            .unwrap();

        // The top card moved to exile.
        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );

        // And a tracked set was published containing exactly that card so the
        // delayed-trigger recall can later resolve it.
        assert!(
            !state.tracked_object_sets.is_empty(),
            "expected ExileTop to publish a tracked set when followed by a uses_tracked_set delayed trigger",
        );
        let any_set_contains_top = state
            .tracked_object_sets
            .values()
            .any(|ids| ids.contains(&top));
        assert!(
            any_set_contains_top,
            "the published tracked set must contain the exiled object ({top:?}), got {:?}",
            state.tracked_object_sets,
        );
    }

    /// CR 406.3: `Effect::ExileTop { face_down: true }` must flip the
    /// exiled object's `face_down` flag so `visibility.rs` can redact the
    /// card unless a separate effect grants look permission (Necropotence /
    /// Bomat Courier / Asmodeus the Archfiend / Knowledge Vault class).
    #[test]
    fn exile_top_face_down_sets_object_face_down_flag() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&top).expect("object should exist");
        assert_eq!(obj.zone, Zone::Exile);
        assert!(
            obj.face_down,
            "expected face_down=true on the exiled object after `Effect::ExileTop {{ face_down: true }}`",
        );
    }

    /// CR 406.3: A face-up `Effect::ExileTop` must leave `face_down`
    /// untouched (default `false`) so cards exiled face up — the Cascade /
    /// Impulse / Adventure class — remain inspectable by every player.
    #[test]
    fn exile_top_face_up_does_not_set_face_down_flag() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                face_down: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&top).expect("object should exist");
        assert_eq!(obj.zone, Zone::Exile);
        assert!(
            !obj.face_down,
            "face-up ExileTop must not flip the object's `face_down` flag",
        );
    }
}
