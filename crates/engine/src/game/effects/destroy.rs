use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetRef, TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 701.8a + CR 614: Apply an accepted Destroy proposed event.
///
/// Routes the inner `ZoneChange(Battlefield → Graveyard)` through the
/// replacement pipeline (CR 614.6) so regeneration (CR 701.8c), redirects
/// (e.g., Rest in Peace → exile), and leaves-the-battlefield replacements
/// all compose on the destruction event.
///
/// Shared by the fresh-destroy path (`resolve`/`resolve_all`) and the
/// post-replacement-choice delivery path (`handle_replacement_choice`).
///
/// Returns `true` on success, `false` if the inner ZoneChange itself
/// needed a replacement choice (caller must not advance).
pub fn apply_destroy_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    match event {
        ProposedEvent::Destroy {
            object_id, source, ..
        } => {
            // CR 701.8a: Destruction resolved — now propose the inner ZoneChange
            // so Moved replacements can intercept the actual zone transfer.
            let zone_proposed =
                ProposedEvent::zone_change(object_id, Zone::Battlefield, Zone::Graveyard, source);
            match replacement::replace_event(state, zone_proposed, events) {
                ReplacementResult::Execute(zone_event) => {
                    if let ProposedEvent::ZoneChange {
                        object_id: oid, to, ..
                    } = zone_event
                    {
                        zones::move_to_zone(state, oid, to, events);
                        state.layers_dirty = true;
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                    return false;
                }
            }
            events.push(GameEvent::CreatureDestroyed { object_id });
            true
        }
        ProposedEvent::ZoneChange { object_id, to, .. } => {
            // Destroy replacement redirected directly to a zone change.
            zones::move_to_zone(state, object_id, to, events);
            state.layers_dirty = true;
            true
        }
        _ => true,
    }
}

/// Outcome of destroying a single object through the guarded path.
///
/// Lets callers (the top-level `Effect::Destroy` loop and the counter-source
/// rider) map a single-object destruction onto their own control flow:
/// `Completed`/`Skipped` continue, `NeedsChoice` requires returning without
/// advancing because the replacement pipeline set `state.waiting_for`.
pub(crate) enum DestroyOutcome {
    /// The object was destroyed (or its destruction was replaced/prevented
    /// inline, e.g. regeneration) — caller may continue.
    Completed,
    /// A guard fired (emblem CR 114.5, not on battlefield, or indestructible
    /// CR 702.12b) so nothing was destroyed — caller may continue.
    Skipped,
    /// A replacement requires a player choice mid-resolution; `state.waiting_for`
    /// is already set. Caller must return without advancing.
    NeedsChoice,
}

/// CR 114.5 / CR 701.8a / CR 702.12b: Destroy a single object through the
/// emblem, zone, and indestructible guards followed by the replacement-aware
/// destruction pipeline.
///
/// Factored out of `resolve`'s per-target loop body so that any caller wanting
/// to destroy one determined object (the top-level Destroy effect, the
/// counter-source rider in `counter.rs`) shares exactly one guarded path — the
/// guards (CR 114.5 emblem, battlefield-zone, CR 702.12b indestructible) live
/// here, *before* `ProposedEvent::Destroy`, so they cannot be bypassed.
pub(crate) fn destroy_single_object(
    state: &mut GameState,
    object_id: ObjectId,
    source: ObjectId,
    cant_regenerate: bool,
    events: &mut Vec<GameEvent>,
) -> DestroyOutcome {
    let Some(obj) = state.objects.get(&object_id) else {
        return DestroyOutcome::Skipped;
    };

    // CR 114.5: Emblems are neither cards nor permanents — cannot be destroyed.
    if obj.is_emblem {
        return DestroyOutcome::Skipped;
    }

    // CR 701.8a: Destroy moves a permanent from the battlefield to its owner's
    // graveyard — only battlefield objects are destroyable.
    if obj.zone != Zone::Battlefield {
        return DestroyOutcome::Skipped;
    }

    // CR 702.12b: A permanent with indestructible can't be destroyed.
    if obj.has_keyword(&crate::types::keywords::Keyword::Indestructible) {
        return DestroyOutcome::Skipped;
    }

    let proposed = ProposedEvent::Destroy {
        object_id,
        source: Some(source),
        cant_regenerate,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if apply_destroy_after_replacement(state, event, events) {
                DestroyOutcome::Completed
            } else {
                DestroyOutcome::NeedsChoice
            }
        }
        ReplacementResult::Prevented => DestroyOutcome::Completed,
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            DestroyOutcome::NeedsChoice
        }
    }
}

/// CR 701.8a: Destroy moves permanent from battlefield to owner's graveyard.
/// CR 701.8b: Indestructible permanents can't be destroyed.
/// Skips objects with the "indestructible" keyword.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let cant_regenerate = matches!(
        &ability.effect,
        Effect::Destroy {
            cant_regenerate: true,
            ..
        }
    );
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            match destroy_single_object(state, *obj_id, ability.source_id, cant_regenerate, events)
            {
                DestroyOutcome::Completed | DestroyOutcome::Skipped => {}
                DestroyOutcome::NeedsChoice => return Ok(()),
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Destroy all permanents matching the filter.
/// CR 701.8: Routes each destruction through the replacement pipeline
/// so regeneration shields and other replacements can intercept.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, cant_regenerate) = match &ability.effect {
        Effect::DestroyAll {
            target,
            cant_regenerate,
        } => (target.clone(), *cant_regenerate),
        _ => (crate::types::ability::TargetFilter::Any, false),
    };

    // Use a creature filter as default if the effect's target is None
    let effective_filter = if matches!(target_filter, crate::types::ability::TargetFilter::None) {
        crate::types::ability::TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            controller: None,
            properties: vec![],
        })
    } else {
        crate::game::effects::resolved_object_filter(ability, &target_filter)
    };

    // Collect matching object IDs that are on the battlefield and not indestructible.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            let is_indestructible = state
                .objects
                .get(id)
                .map(|obj| obj.has_keyword(&crate::types::keywords::Keyword::Indestructible))
                .unwrap_or(false);
            !is_indestructible
                && crate::game::filter::matches_target_filter(state, **id, &effective_filter, &ctx)
        })
        .copied()
        .collect();

    for &obj_id in &matching {
        let proposed = ProposedEvent::Destroy {
            object_id: obj_id,
            source: Some(ability.source_id),
            cant_regenerate,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if !apply_destroy_after_replacement(state, event, events) {
                    return Ok(());
                }
            }
            ReplacementResult::Prevented => {} // Regenerated or other replacement
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
    }

    // CR 603.10a + CR 704.3: every creature destroyed by this effect left the
    // battlefield simultaneously, so co-departing leaves-the-battlefield/dies
    // observers (Blood Artist, Zulaport Cutthroat) must observe each other.
    // CR 701.19a/b: a regenerated member (and any other Prevented destruction)
    // stays on the battlefield, so `departed_subset` excludes it from every
    // survivor's co-departed group.
    crate::game::zones::mark_simultaneous_departures(
        events,
        &crate::game::zones::departed_subset(state, &matching),
    );

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
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn destroy_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    #[test]
    fn destroy_skips_indestructible() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "God".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Indestructible);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
    }

    #[test]
    fn destroy_emits_creature_destroyed_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CreatureDestroyed { object_id } if *object_id == obj_id)
        ));
    }

    #[test]
    fn destroy_all_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Non-creature should survive
        let _land = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&bear1));
        assert!(!state.battlefield.contains(&bear2));
        // Land survives
        assert_eq!(state.battlefield.len(), 1);
    }

    #[test]
    fn destroy_prevented_by_regen_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Add regeneration shield
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(bear_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature survived
        assert!(
            state.battlefield.contains(&bear_id),
            "Creature with regen shield should survive Destroy"
        );
        // No CreatureDestroyed event
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CreatureDestroyed { .. })));
        // Regenerated event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Regenerated { .. })));
    }

    #[test]
    fn destroy_all_prevented_by_regen_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);

        // Protected creature
        let protected_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shielded".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&protected_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&protected_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        // Unprotected creature
        let unprotected_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Unshielded".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&unprotected_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // Protected creature survives
        assert!(
            state.battlefield.contains(&protected_id),
            "Creature with regen shield should survive DestroyAll"
        );
        // Unprotected creature destroyed
        assert!(
            !state.battlefield.contains(&unprotected_id),
            "Unshielded creature should be destroyed by DestroyAll"
        );
    }

    #[test]
    fn destroy_all_cant_regenerate_bypasses_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.battlefield.contains(&bear_id),
            "cant_regenerate should bypass regen shield in DestroyAll"
        );
    }

    // ---------------------------------------------------------------------
    // "Destroyed this way" tracked-set regression tests (Fumigate class).
    // CR 603.7 + CR 701.8a: DestroyAll must record the actually-destroyed
    // objects into a tracked set so downstream sub-abilities referencing
    // `QuantityRef::TrackedSetSize` resolve against the correct count.
    // ---------------------------------------------------------------------

    use crate::game::effects::resolve_ability_chain;
    use crate::types::ability::{
        GainLifePlayer, QuantityExpr, QuantityRef, TypeFilter, TypedFilter,
    };

    /// Builds the Fumigate-shape chain: `DestroyAll(creatures)` followed by
    /// `GainLife(amount = TrackedSetSize, player = Controller)`.
    fn fumigate_chain(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            controller,
        );
        ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                cant_regenerate: false,
            },
            vec![],
            source_id,
            controller,
        )
        .sub_ability(gain_life)
    }

    fn spawn_creature(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    #[test]
    fn fumigate_gains_zero_life_when_no_creatures_on_battlefield() {
        let mut state = GameState::new_two_player(42);
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life);
        // A tracked set must still be recorded (even if empty) so stale prior
        // sets are not reused by TrackedSetSize.
        assert_eq!(state.tracked_object_sets.len(), 1);
        assert!(state
            .tracked_object_sets
            .values()
            .next()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn fumigate_gains_one_life_for_one_destroyed_creature() {
        let mut state = GameState::new_two_player(42);
        let _bear = spawn_creature(&mut state, CardId(1), PlayerId(0), "Bear");
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life + 1);
    }

    #[test]
    fn fumigate_gains_n_life_for_n_destroyed_creatures() {
        let mut state = GameState::new_two_player(42);
        for i in 0u64..5 {
            spawn_creature(
                &mut state,
                CardId(i + 1),
                PlayerId((i % 2) as u8),
                "Creature",
            );
        }
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life + 5);
    }

    #[test]
    fn fumigate_excludes_regenerated_creature_from_life_gained() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let shielded = spawn_creature(&mut state, CardId(1), PlayerId(0), "Shielded");
        // CR 701.8c: regeneration shield replaces the destruction.
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&shielded)
            .unwrap()
            .replacement_definitions
            .push(shield);

        // Two unshielded creatures + one shielded = 2 should actually die.
        spawn_creature(&mut state, CardId(2), PlayerId(0), "UnshieldedA");
        spawn_creature(&mut state, CardId(3), PlayerId(1), "UnshieldedB");
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Life gained must equal *actually destroyed* count (2), not filter-matched (3).
        assert_eq!(state.players[0].life, starting_life + 2);
        // Regenerated creature survives.
        assert!(state.battlefield.contains(&shielded));
    }

    #[test]
    fn fumigate_excludes_indestructible_creature_from_life_gained() {
        let mut state = GameState::new_two_player(42);
        let god = spawn_creature(&mut state, CardId(1), PlayerId(0), "God");
        state
            .objects
            .get_mut(&god)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Indestructible);
        spawn_creature(&mut state, CardId(2), PlayerId(1), "Bear");
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Only the non-indestructible creature was destroyed.
        assert_eq!(state.players[0].life, starting_life + 1);
        assert!(state.battlefield.contains(&god));
    }

    #[test]
    fn destroy_single_target_records_tracked_set_for_downstream_gain_life() {
        // Single-target `Destroy` variant (not DestroyAll) — the class fix must
        // cover both resolve() and resolve_all() paths.
        let mut state = GameState::new_two_player(42);
        let bear = spawn_creature(&mut state, CardId(1), PlayerId(1), "Bear");
        let starting_life = state.players[0].life;

        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(200),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(bear)],
            ObjectId(200),
            PlayerId(0),
        )
        .sub_ability(gain_life);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life + 1);
    }
}
