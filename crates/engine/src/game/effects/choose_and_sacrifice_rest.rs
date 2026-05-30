use crate::game::players;
use crate::types::ability::{
    CategoryChooserScope, Effect, EffectError, EffectKind, PlayerFilter, ResolvedAbility,
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
    // CR 102.2 (two-player) / CR 102.3 (team multiplayer): An ability with
    // `player_scope` (e.g. Liliana, Dreadhorde General's "Each opponent …")
    // restricts the choose-and-sacrifice to the scoped players only.
    // `player_scope == None` (Cataclysm, Tragic Arrogance "each player") keeps
    // the full table; `player_scope == All` is equivalent. For `Opponent`, the
    // ability's own controller is excluded. In a two-player game the opponent
    // set is the other player (CR 102.2); the CR 102.3 team definition only
    // governs >2-player team games.
    let scope = ability.player_scope.clone().unwrap_or(PlayerFilter::All);
    let player_order: Vec<PlayerId> = players::apnap_order(state)
        .into_iter()
        .filter(|pid| {
            super::matches_player_scope(state, *pid, &scope, ability.controller, ability.source_id)
        })
        .collect();

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
        // CR 603.10a: the permanents this sweep sacrifices left the battlefield
        // together — stamp the sub-slice so a co-departing leaves-the-battlefield
        // observer among them observes the rest.
        let before = events.len();
        sacrifice_unchosen(state, &[], &player_order, ability.source_id, events);
        crate::game::zones::stamp_simultaneous_from_slice(state, &mut events[before..]);
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
            &player_order,
            events,
        );
    }

    // Auto-resolve if every category has at most one choice and no overlaps.
    if let Some(auto_choices) = try_auto_resolve(&eligible) {
        let kept: Vec<ObjectId> = auto_choices.iter().filter_map(|&opt| opt).collect();
        if remaining_players.is_empty() {
            // CR 603.10a: co-departing observer among the sacrificed group
            // observes the rest — stamp the sweep's sub-slice.
            let before = events.len();
            sacrifice_unchosen(state, &kept, &player_order, ability.source_id, events);
            crate::game::zones::stamp_simultaneous_from_slice(state, &mut events[before..]);
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
            &player_order,
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
        scoped_players: player_order,
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
    scoped_players: &[PlayerId],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if remaining.is_empty() {
        // CR 603.10a: terminal APNAP sweep — the sacrificed group left the
        // battlefield together, so stamp this sub-slice for co-departing observers.
        let before = events.len();
        sacrifice_unchosen(state, &all_kept, scoped_players, source_id, events);
        crate::game::zones::stamp_simultaneous_from_slice(state, &mut events[before..]);
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
            scoped_players,
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
            scoped_players,
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
        scoped_players: scoped_players.to_vec(),
    };

    Ok(())
}

/// CR 701.21a: Sacrifice all permanents on the battlefield that were not chosen.
/// Public alias for engine_resolution_choices handler.
pub(crate) fn sacrifice_unchosen_from_handler(
    state: &mut GameState,
    kept: &[ObjectId],
    scoped_players: &[PlayerId],
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    sacrifice_unchosen(state, kept, scoped_players, source_id, events);
}

/// CR 701.21a: Sacrifice all permanents on the battlefield that were not chosen.
fn sacrifice_unchosen(
    state: &mut GameState,
    kept: &[ObjectId],
    scoped_players: &[PlayerId],
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    // CR 701.21a: Sacrifice each permanent NOT chosen, restricted to the
    // permanents controlled by the players within `player_scope`. A player
    // outside the effect's scope (e.g. Liliana's controller, scope = Opponent)
    // keeps their whole board.
    // CR 102.2 (two-player) / CR 102.3 (team multiplayer): `scoped_players` is
    // the APNAP-ordered scoped set computed in `resolve`. An empty
    // `scoped_players` can only arise from a mid-`CategoryChoice` save/reload
    // deserializing the `#[serde(default)]` field to `Vec::new()`. An empty set
    // would make the `contains` filter sacrifice NOTHING — a silent wrong
    // result. Fall back to the full APNAP set (pre-#519 all-players sweep):
    // over-sweep at worst, never a silent no-op.
    let effective_scope: Vec<PlayerId> = if scoped_players.is_empty() {
        players::apnap_order(state)
    } else {
        scoped_players.to_vec()
    };
    // Collect all battlefield permanents not in the kept set, controlled by a
    // player within scope.
    let to_sacrifice: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            !kept.contains(id)
                && state
                    .objects
                    .get(id)
                    .is_some_and(|obj| !obj.is_emblem && effective_scope.contains(&obj.controller))
        })
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

    fn make_scoped_ability(
        categories: Vec<CoreType>,
        chooser_scope: CategoryChooserScope,
        player_scope: Option<PlayerFilter>,
        controller: PlayerId,
    ) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::ChooseAndSacrificeRest {
                categories,
                chooser_scope,
            },
            vec![],
            ObjectId(100),
            controller,
        );
        ability.player_scope = player_scope;
        ability
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

    /// Regression for #447: a non-active player whose battlefield contains an
    /// artifact creature shared across the Artifact and Creature categories,
    /// plus extra options in each, must produce a real `CategoryChoice` (no
    /// auto-resolve) — and every `SelectCategoryPermanents` candidate the AI
    /// enumerator yields must apply cleanly through the engine (the duplicate
    /// guard would otherwise softlock the seat).
    #[test]
    fn non_active_player_shared_type_permanent_enumerates_applicable_choices() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        // 3-player game so a non-active player makes the choice.
        let mut state = crate::types::game_state::GameState::new(
            crate::types::format::FormatConfig::commander(),
            3,
            42,
        );
        // Player 0 (active) has nothing — resolve advances to player 1.
        // Player 1: an artifact creature (in both categories) + an extra
        // artifact + an extra creature, so neither category auto-resolves.
        let _ac = add_battlefield_permanent(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Steel Hellkite",
            vec![CoreType::Artifact, CoreType::Creature],
        );
        let _extra_artifact = add_battlefield_permanent(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Sol Ring",
            vec![CoreType::Artifact],
        );
        let _extra_creature = add_battlefield_permanent(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Grizzly Bears",
            vec![CoreType::Creature],
        );

        let ability = ResolvedAbility::new(
            Effect::ChooseAndSacrificeRest {
                categories: vec![CoreType::Artifact, CoreType::Creature],
                chooser_scope: CategoryChooserScope::EachPlayerSelf,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let chooser = match &state.waiting_for {
            WaitingFor::CategoryChoice {
                player,
                target_player,
                eligible_per_category,
                ..
            } => {
                assert_eq!(*target_player, PlayerId(1));
                assert_eq!(eligible_per_category[0].len(), 2); // 2 artifacts
                assert_eq!(eligible_per_category[1].len(), 2); // 2 creatures
                *player
            }
            other => panic!("Expected CategoryChoice (not auto-resolved), got {other:?}"),
        };

        // Every enumerated SelectCategoryPermanents candidate must apply
        // cleanly — none may repeat an object across categories.
        let candidates = crate::ai_support::legal_actions(&state);
        let category_actions: Vec<GameAction> = candidates
            .into_iter()
            .filter(|a| matches!(a, GameAction::SelectCategoryPermanents { .. }))
            .collect();
        assert!(
            !category_actions.is_empty(),
            "legal_actions must enumerate at least one SelectCategoryPermanents"
        );
        for action in category_actions {
            let mut clone = state.clone();
            apply(&mut clone, chooser, action.clone())
                .unwrap_or_else(|e| panic!("candidate {action:?} failed to apply: {e:?}"));
        }
    }

    #[test]
    fn opponent_scope_sweeps_only_opponent_board() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        // Liliana, Dreadhorde General −9: "Each opponent chooses a permanent they
        // control of each permanent type and sacrifices the rest."
        // player_scope = Opponent → only P1's board is swept; P0 (the Liliana
        // controller) keeps its entire board.
        //
        // REVERTED-FIX MUTATION: without the §6 resolver/driver fix, `player_order`
        // includes P0, so P0's non-kept permanents are sacrificed and the P0
        // survive-assertions fail. Without the §6c empty-`scoped_players` fall-back,
        // a save/reload would make the sweep sacrifice nothing and the P1
        // positive-sweep assertion fails. Both halves are required to be
        // discriminating.
        let mut state = setup_two_player();
        // Pin the active player explicitly so APNAP order and the post-filter
        // `target_player` are deterministic, not a coincidence of the
        // single-element `player_order`.
        state.active_player = PlayerId(0);

        // P0 (controller) — three permanents that MUST survive.
        let p0_creature = add_battlefield_permanent(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Bear",
            vec![CoreType::Creature],
        );
        let p0_creature2 = add_battlefield_permanent(
            &mut state,
            CardId(2),
            PlayerId(0),
            "P0 Lion",
            vec![CoreType::Creature],
        );
        let p0_artifact = add_battlefield_permanent(
            &mut state,
            CardId(3),
            PlayerId(0),
            "P0 Sol Ring",
            vec![CoreType::Artifact],
        );

        // P1 (opponent) — two creatures so the Creature category needs a real
        // choice (no auto-resolve), proving the effect fires AND leaving exactly
        // one non-kept creature to be swept.
        let p1_keep = add_battlefield_permanent(
            &mut state,
            CardId(4),
            PlayerId(1),
            "P1 Bear",
            vec![CoreType::Creature],
        );
        let p1_sacrificed = add_battlefield_permanent(
            &mut state,
            CardId(5),
            PlayerId(1),
            "P1 Lion",
            vec![CoreType::Creature],
        );

        // Ability controlled by P0, scope = Opponent.
        let ability = make_scoped_ability(
            vec![CoreType::Creature],
            CategoryChooserScope::EachPlayerSelf,
            Some(PlayerFilter::Opponent),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The effect must address only P1 — P0 must NEVER be a target_player.
        let eligible = match &state.waiting_for {
            WaitingFor::CategoryChoice {
                target_player,
                eligible_per_category,
                ..
            } => {
                assert_eq!(
                    *target_player,
                    PlayerId(1),
                    "scoped effect must address the opponent, not the controller"
                );
                // P1 has two creatures eligible for the single Creature category.
                assert_eq!(eligible_per_category.len(), 1);
                assert_eq!(eligible_per_category[0].len(), 2);
                eligible_per_category[0].clone()
            }
            other => panic!("Expected CategoryChoice for P1, got {other:?}"),
        };
        // Sanity: P1 keeps `p1_keep`, sacrifices `p1_sacrificed`.
        assert!(eligible.contains(&p1_keep) && eligible.contains(&p1_sacrificed));

        // DRIVE: P1 chooses to keep `p1_keep` for the Creature category. Apply it
        // through the engine pipeline so the real continuation
        // (`engine_resolution_choices.rs`) runs `sacrifice_unchosen`.
        let action = GameAction::SelectCategoryPermanents {
            choices: vec![Some(p1_keep)],
        };
        let result = apply(&mut state, PlayerId(1), action);
        assert!(
            result.is_ok(),
            "SelectCategoryPermanents must apply cleanly"
        );
        // Two-player game: after P1's only choice the resolution completes — no
        // further CategoryChoice is pending.
        assert!(
            !matches!(state.waiting_for, WaitingFor::CategoryChoice { .. }),
            "two-player scoped sweep completes after the single opponent chooses"
        );

        // DISCRIMINATING ASSERTION 1 — every P0 permanent still on the battlefield.
        assert!(
            state.battlefield.contains(&p0_creature),
            "controller's creature must NOT be sacrificed by an Opponent-scoped effect"
        );
        assert!(
            state.battlefield.contains(&p0_creature2),
            "controller's second creature must NOT be sacrificed"
        );
        assert!(
            state.battlefield.contains(&p0_artifact),
            "controller's artifact must NOT be sacrificed"
        );

        // DISCRIMINATING ASSERTION 2 — the sweep ACTUALLY FIRED for P1.
        // p1_keep survives (chosen); p1_sacrificed was swept. Without this,
        // an empty-`scoped_players` no-op bug would pass assertion 1 vacuously.
        assert!(
            state.battlefield.contains(&p1_keep),
            "opponent's chosen creature must survive"
        );
        assert!(
            !state.battlefield.contains(&p1_sacrificed),
            "opponent's non-kept creature MUST be sacrificed — proves the sweep fired"
        );
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
