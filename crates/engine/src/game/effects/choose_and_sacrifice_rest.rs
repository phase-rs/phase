use crate::game::players;
use crate::types::ability::{
    CategoryChooserScope, Effect, EffectError, EffectKind, ResolvedAbility,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// CR 101.4 + CR 701.21a: Each player chooses one permanent per type category
/// from among the permanents they control, then sacrifices the rest.
/// The `chooser_scope` determines whether each player chooses independently
/// (APNAP order) or the controller chooses for all players.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (categories, chooser_scope) = match &ability.effect {
        Effect::ChooseAndSacrificeRest {
            categories,
            chooser_scope,
        } => (categories.clone(), *chooser_scope),
        _ => {
            return Err(EffectError::MissingParam(
                "ChooseAndSacrificeRest".to_string(),
            ))
        }
    };

    // CR 101.4: Determine player order using APNAP.
    let player_order = players::apnap_order(state);

    if player_order.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseAndSacrificeRest,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Start with the first player in APNAP order.
    let current_player = player_order[0];
    let remaining_players: Vec<PlayerId> = player_order[1..].to_vec();

    // CR 101.4: Determine who makes the choice for this player's permanents.
    let chooser = match chooser_scope {
        CategoryChooserScope::EachPlayerSelf => current_player,
        CategoryChooserScope::ControllerForAll => ability.controller,
    };

    let eligible = compute_eligible_per_category(state, current_player, &categories);

    // If all categories are empty for all players, skip directly to sacrifice.
    if eligible.iter().all(|e| e.is_empty()) && remaining_players.is_empty() {
        sacrifice_unchosen(state, &[], ability.source_id, events);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseAndSacrificeRest,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // If all categories are empty for this player but there are more players, advance.
    if eligible.iter().all(|e| e.is_empty()) {
        return advance_to_next_player(
            state,
            &categories,
            chooser_scope,
            ability.controller,
            ability.source_id,
            &remaining_players,
            Vec::new(),
            events,
        );
    }

    // Auto-resolve if every category has at most one choice and no overlaps.
    if let Some(auto_choices) = try_auto_resolve(&eligible) {
        let kept: Vec<ObjectId> = auto_choices.iter().filter_map(|&opt| opt).collect();
        if remaining_players.is_empty() {
            sacrifice_unchosen(state, &kept, ability.source_id, events);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ChooseAndSacrificeRest,
                source_id: ability.source_id,
            });
            return Ok(());
        }
        return advance_to_next_player(
            state,
            &categories,
            chooser_scope,
            ability.controller,
            ability.source_id,
            &remaining_players,
            kept,
            events,
        );
    }

    state.waiting_for = WaitingFor::CategoryChoice {
        player: chooser,
        target_player: current_player,
        categories,
        eligible_per_category: eligible,
        source_id: ability.source_id,
        remaining_players,
        all_kept: Vec::new(),
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseAndSacrificeRest,
        source_id: ability.source_id,
    });

    Ok(())
}

/// Compute eligible permanents for each category from a player's battlefield.
pub(crate) fn compute_eligible_per_category(
    state: &GameState,
    player: PlayerId,
    categories: &[CoreType],
) -> Vec<Vec<ObjectId>> {
    categories
        .iter()
        .map(|core_type| {
            state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == player
                            && !obj.is_emblem
                            && obj.card_types.core_types.contains(core_type)
                    })
                })
                .collect()
        })
        .collect()
}

/// Try to auto-resolve when every category has at most one eligible permanent
/// and no permanent appears in multiple categories.
fn try_auto_resolve(eligible: &[Vec<ObjectId>]) -> Option<Vec<Option<ObjectId>>> {
    let mut choices: Vec<Option<ObjectId>> = Vec::with_capacity(eligible.len());
    let mut used = Vec::new();

    for category_eligible in eligible {
        // Filter out already-used objects.
        let available: Vec<ObjectId> = category_eligible
            .iter()
            .copied()
            .filter(|id| !used.contains(id))
            .collect();

        match available.len() {
            0 => choices.push(None),
            1 => {
                let id = available[0];
                used.push(id);
                choices.push(Some(id));
            }
            _ => return None, // Multiple choices — needs player input.
        }
    }

    Some(choices)
}

/// Advance to the next player in the APNAP sequence, or sacrifice if done.
#[allow(clippy::too_many_arguments)]
pub(crate) fn advance_to_next_player(
    state: &mut GameState,
    categories: &[CoreType],
    chooser_scope: CategoryChooserScope,
    controller: PlayerId,
    source_id: ObjectId,
    remaining: &[PlayerId],
    mut all_kept: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if remaining.is_empty() {
        sacrifice_unchosen(state, &all_kept, source_id, events);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseAndSacrificeRest,
            source_id,
        });
        return Ok(());
    }

    let next_player = remaining[0];
    let next_remaining: Vec<PlayerId> = remaining[1..].to_vec();

    let chooser = match chooser_scope {
        CategoryChooserScope::EachPlayerSelf => next_player,
        CategoryChooserScope::ControllerForAll => controller,
    };

    let eligible = compute_eligible_per_category(state, next_player, categories);

    // If all categories empty for this player, skip ahead.
    if eligible.iter().all(|e| e.is_empty()) {
        return advance_to_next_player(
            state,
            categories,
            chooser_scope,
            controller,
            source_id,
            &next_remaining,
            all_kept,
            events,
        );
    }

    // Auto-resolve if trivial.
    if let Some(auto_choices) = try_auto_resolve(&eligible) {
        let kept: Vec<ObjectId> = auto_choices.iter().filter_map(|&opt| opt).collect();
        all_kept.extend(kept);
        return advance_to_next_player(
            state,
            categories,
            chooser_scope,
            controller,
            source_id,
            &next_remaining,
            all_kept,
            events,
        );
    }

    state.waiting_for = WaitingFor::CategoryChoice {
        player: chooser,
        target_player: next_player,
        categories: categories.to_vec(),
        eligible_per_category: eligible,
        source_id,
        remaining_players: next_remaining,
        all_kept,
    };

    Ok(())
}

/// CR 701.21a: Sacrifice all permanents on the battlefield that were not chosen.
/// Public alias for engine_resolution_choices handler.
pub(crate) fn sacrifice_unchosen_from_handler(
    state: &mut GameState,
    kept: &[ObjectId],
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    sacrifice_unchosen(state, kept, source_id, events);
}

/// CR 701.21a: Sacrifice all permanents on the battlefield that were not chosen.
fn sacrifice_unchosen(
    state: &mut GameState,
    kept: &[ObjectId],
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    // Collect all battlefield permanents not in the kept set.
    let to_sacrifice: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| !kept.contains(id) && state.objects.get(id).is_some_and(|obj| !obj.is_emblem))
        .collect();

    for obj_id in to_sacrifice {
        let controller = state
            .objects
            .get(&obj_id)
            .map(|obj| obj.controller)
            .unwrap_or(state.active_player);
        // Use the sacrifice primitive directly — single authority for sacrifice.
        match crate::game::sacrifice::sacrifice_permanent(state, obj_id, controller, events) {
            Ok(crate::game::sacrifice::SacrificeOutcome::Complete) => {}
            Ok(crate::game::sacrifice::SacrificeOutcome::NeedsReplacementChoice(player)) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                // Replacement choice will resume; remaining sacrifices happen after.
                return;
            }
            Err(_) => {
                // Object may have left the battlefield; skip silently.
            }
        }
    }

    let _ = source_id; // used by caller for EffectResolved event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::Effect;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_ability(
        categories: Vec<CoreType>,
        chooser_scope: CategoryChooserScope,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChooseAndSacrificeRest {
                categories,
                chooser_scope,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn setup_two_player() -> GameState {
        GameState::new_two_player(42)
    }

    fn add_battlefield_permanent(
        state: &mut GameState,
        card_id: CardId,
        player: PlayerId,
        name: &str,
        core_types: Vec<CoreType>,
    ) -> ObjectId {
        let obj_id = create_object(state, card_id, player, name.to_string(), Zone::Battlefield);
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.card_types.core_types = core_types;
        }
        obj_id
    }

    #[test]
    fn resolve_sets_category_choice_with_eligible() {
        let mut state = setup_two_player();
        let _creature = add_battlefield_permanent(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear",
            vec![CoreType::Creature],
        );
        let _artifact = add_battlefield_permanent(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sol Ring",
            vec![CoreType::Artifact],
        );
        // Player 0 has creature + artifact, so must choose one of each for 2 categories.
        // But also add a second creature so auto-resolve won't trigger.
        let _creature2 = add_battlefield_permanent(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Lion",
            vec![CoreType::Creature],
        );

        let ability = make_ability(
            vec![CoreType::Artifact, CoreType::Creature],
            CategoryChooserScope::EachPlayerSelf,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CategoryChoice {
                player,
                target_player,
                categories,
                eligible_per_category,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*target_player, PlayerId(0));
                assert_eq!(categories.len(), 2);
                assert_eq!(eligible_per_category[0].len(), 1); // 1 artifact
                assert_eq!(eligible_per_category[1].len(), 2); // 2 creatures
            }
            other => panic!("Expected CategoryChoice, got {:?}", other),
        }
    }

    #[test]
    fn auto_resolve_when_trivial() {
        let mut state = setup_two_player();
        let creature = add_battlefield_permanent(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear",
            vec![CoreType::Creature],
        );
        let artifact = add_battlefield_permanent(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sol Ring",
            vec![CoreType::Artifact],
        );
        // Player 1 has nothing — trivial for both players.
        let ability = make_ability(
            vec![CoreType::Artifact, CoreType::Creature],
            CategoryChooserScope::EachPlayerSelf,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should auto-resolve: creature and artifact kept, no waiting state needed.
        assert!(
            !matches!(state.waiting_for, WaitingFor::CategoryChoice { .. }),
            "Should auto-resolve when each category has exactly one option"
        );

        // Both permanents should still be on battlefield (they were the only ones).
        assert!(state.battlefield.contains(&creature));
        assert!(state.battlefield.contains(&artifact));
    }

    #[test]
    fn controller_for_all_sets_correct_chooser() {
        let mut state = setup_two_player();
        // Player 1 has two creatures — needs a choice.
        let _c1 = add_battlefield_permanent(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear",
            vec![CoreType::Creature],
        );
        let _c2 = add_battlefield_permanent(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lion",
            vec![CoreType::Creature],
        );

        // Tragic Arrogance pattern: controller (P0) chooses for all.
        let ability = make_ability(
            vec![CoreType::Creature],
            CategoryChooserScope::ControllerForAll,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::CategoryChoice {
                player,
                target_player,
                ..
            } => {
                // Controller (P0) chooses for P0's permanents.
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*target_player, PlayerId(0));
            }
            other => panic!("Expected CategoryChoice, got {:?}", other),
        }
    }

    #[test]
    fn empty_battlefield_skips_choice() {
        let mut state = setup_two_player();
        let ability = make_ability(
            vec![CoreType::Artifact, CoreType::Creature],
            CategoryChooserScope::EachPlayerSelf,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::CategoryChoice { .. }),
            "Should skip choice when no player has permanents"
        );
    }

    #[test]
    fn compute_eligible_filters_by_type_and_controller() {
        let mut state = setup_two_player();
        let _c = add_battlefield_permanent(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear",
            vec![CoreType::Creature],
        );
        let _a = add_battlefield_permanent(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Sol Ring",
            vec![CoreType::Artifact],
        );

        let eligible = compute_eligible_per_category(
            &state,
            PlayerId(0),
            &[CoreType::Creature, CoreType::Artifact],
        );

        assert_eq!(eligible[0].len(), 1); // P0's creature
        assert_eq!(eligible[1].len(), 0); // P0 has no artifacts (P1's artifact excluded)
    }

    #[test]
    fn multi_type_permanent_appears_in_multiple_categories() {
        let mut state = setup_two_player();
        let _ac = add_battlefield_permanent(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact Creature",
            vec![CoreType::Artifact, CoreType::Creature],
        );

        let eligible = compute_eligible_per_category(
            &state,
            PlayerId(0),
            &[CoreType::Artifact, CoreType::Creature],
        );

        // The artifact creature should appear in both categories.
        assert_eq!(eligible[0].len(), 1);
        assert_eq!(eligible[1].len(), 1);
        assert_eq!(eligible[0][0], eligible[1][0]);
    }
}
