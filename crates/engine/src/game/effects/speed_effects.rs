use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::speed::{decrease_speed, increase_speed, set_speed};
use crate::types::ability::{Effect, EffectError, PlayerFilter, ResolvedAbility, SpeedDelta};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

fn players_for_filter(
    state: &GameState,
    filter: &PlayerFilter,
    ability: &ResolvedAbility,
) -> Vec<PlayerId> {
    let controller = ability.controller;
    let source_id = ability.source_id;
    match filter {
        PlayerFilter::Controller => vec![controller],
        PlayerFilter::Opponent => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated && player.id != controller)
            .map(|player| player.id)
            .collect(),
        PlayerFilter::DefendingPlayer => {
            crate::game::targeting::resolve_event_context_target_for_event_or_state(
                state,
                &crate::types::ability::TargetFilter::DefendingPlayer,
                source_id,
                state.current_trigger_event.as_ref(),
            )
            .and_then(|target| match target {
                crate::types::ability::TargetRef::Player(player) => Some(player),
                _ => None,
            })
            .into_iter()
            .collect()
        }
        PlayerFilter::OpponentLostLife => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| player.id != controller && player.life_lost_this_turn > 0)
            .map(|player| player.id)
            .collect(),
        PlayerFilter::OpponentGainedLife => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| player.id != controller && player.life_gained_this_turn > 0)
            .map(|player| player.id)
            .collect(),
        // CR 104.5 / CR 800.4: Players who lost have left the game; this
        // filter is quantity-only and has no live speed-effect recipient.
        PlayerFilter::HasLostTheGame => Vec::new(),
        // CR 506.2 + CR 508.6: Count-only filter (Suppressor Skyguard); it has
        // no live speed-effect recipient meaning.
        PlayerFilter::OpponentOfTriggeringPlayerNotAttacked => Vec::new(),
        // CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i: Each opponent who was
        // dealt combat damage this turn, optionally restricted to a matching
        // source.
        PlayerFilter::OpponentDealtCombatDamage { source } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::quantity::opponent_dealt_combat_damage_matches(
                    state, player.id, controller, source, source_id,
                )
            })
            .map(|player| player.id)
            .collect(),
        // CR 508.6: each opponent the subject attacked within scope.
        PlayerFilter::OpponentAttacked { subject, scope } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                player.id != controller
                    && state.opponent_attacked(*subject, *scope, controller, source_id, player.id)
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::All => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .map(|player| player.id)
            .collect(),
        PlayerFilter::HighestSpeed => {
            let highest_speed = state
                .players
                .iter()
                .filter(|player| !player.is_eliminated)
                .map(|player| crate::game::speed::effective_speed(state, player.id))
                .max()
                .unwrap_or(0);
            state
                .players
                .iter()
                .filter(|player| !player.is_eliminated)
                .filter(|player| {
                    crate::game::speed::effective_speed(state, player.id) == highest_speed
                })
                .map(|player| player.id)
                .collect()
        }
        PlayerFilter::ZoneChangedThisWay => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                state.last_zone_changed_ids.iter().any(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|obj| obj.owner == player.id)
                })
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::PerformedActionThisWay { relation, action } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::players::matches_relation(state, player.id, controller, *relation)
                    && crate::game::players::performed_action_this_way(state, player.id, *action)
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::OwnersOfCardsExiledBySource => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::players::owns_card_exiled_by_source(state, player.id, source_id)
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::TriggeringPlayer => state
            .current_trigger_event
            .as_ref()
            .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
            .into_iter()
            .collect(),
        // CR 120.3 + CR 603.2c: Each opponent other than the triggering opponent.
        // Falls back to plain Opponent semantics when no trigger event is in scope.
        PlayerFilter::OpponentOtherThanTriggering => {
            let triggering = state
                .current_trigger_event
                .as_ref()
                .and_then(|e| crate::game::targeting::extract_player_from_event(e, state));
            state
                .players
                .iter()
                .filter(|player| {
                    !player.is_eliminated
                        && crate::game::players::is_opponent(state, controller, player.id)
                })
                .filter(|player| triggering.is_none_or(|pid| pid != player.id))
                .map(|player| player.id)
                .collect()
        }
        // CR 608.2c + CR 701.38: Players who cast a vote for the recorded
        // choice index in the most recent vote within the current top-level
        // resolution. Read directly off the transient ballot ledger.
        PlayerFilter::VotedFor { choice_index } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                state
                    .last_vote_ballots
                    .iter()
                    .any(|(voter, idx)| *voter == player.id && *idx == *choice_index)
            })
            .map(|player| player.id)
            .collect(),
        // CR 109.4 + CR 608.2c: the controller of the first object target of
        // the resolving ability ("reduce that opponent's speed", anaphoring
        // the controller of a bounced creature).
        PlayerFilter::ParentObjectTargetController => {
            crate::game::ability_utils::parent_target_controller(ability, state)
                .filter(|pid| {
                    state
                        .players
                        .iter()
                        .any(|player| player.id == *pid && !player.is_eliminated)
                })
                .into_iter()
                .collect()
        }
        // CR 108.3 + CR 109.4: the owner of the first object target — owner-axis
        // sibling of `ParentObjectTargetController`.
        PlayerFilter::ParentObjectTargetOwner => {
            crate::game::ability_utils::parent_target_owner(ability, state)
                .filter(|pid| {
                    state
                        .players
                        .iter()
                        .any(|player| player.id == *pid && !player.is_eliminated)
                })
                .into_iter()
                .collect()
        }
        // CR 608.2c + CR 109.4: the resolution-scoped chosen player at `index`.
        PlayerFilter::ChosenPlayer { index } => ability
            .chosen_players
            .get(*index as usize)
            .copied()
            .filter(|pid| {
                state
                    .players
                    .iter()
                    .any(|player| player.id == *pid && !player.is_eliminated)
            })
            .into_iter()
            .collect(),
        // CR 109.4 + CR 109.5: "each [player class] who controls [comparator]
        // [count] [filter]" — candidates satisfying both `relation` and the
        // controlled-permanent count comparison.
        PlayerFilter::ControlsCount {
            relation,
            filter,
            comparator,
            count,
        } => {
            let threshold =
                crate::game::quantity::resolve_quantity(state, count, controller, source_id);
            state
                .players
                .iter()
                .filter(|player| !player.is_eliminated)
                .filter(|player| {
                    crate::game::players::matches_relation(state, player.id, controller, *relation)
                        && crate::game::effects::player_control_count_compares(
                            state,
                            player.id,
                            filter,
                            *comparator,
                            threshold,
                            source_id,
                        )
                })
                .map(|player| player.id)
                .collect()
        }
        // CR 402.1 / 119.1 / 122.1f / 404.1: "each [player class] whose [scalar
        // attr] [comparator] [value]" — candidates satisfying both `relation`
        // and the per-candidate scalar comparison. `attr` is read directly off
        // each candidate; `value` is the controller-relative threshold,
        // resolved once.
        PlayerFilter::PlayerAttribute {
            relation,
            attr,
            comparator,
            value,
        } => {
            let threshold =
                crate::game::quantity::resolve_quantity(state, value, controller, source_id);
            state
                .players
                .iter()
                .filter(|player| !player.is_eliminated)
                .filter(|player| {
                    crate::game::players::matches_relation(state, player.id, controller, *relation)
                        && crate::game::effects::candidate_player_scalar_with_state(
                            state, player, controller, attr,
                        )
                        .is_some_and(|lhs| comparator.evaluate(lhs, threshold))
                })
                .map(|player| player.id)
                .collect()
        }
    }
}

/// CR 702.179a: Effects that instruct players to start their engines set speed to 1
/// only if the player currently has no speed.
pub fn resolve_start(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::StartYourEngines { player_scope } = &ability.effect else {
        return Err(EffectError::InvalidParam(
            "expected StartYourEngines".to_string(),
        ));
    };

    for player_id in players_for_filter(state, player_scope, ability) {
        let has_no_speed = state
            .players
            .iter()
            .find(|player| player.id == player_id)
            .is_some_and(|player| player.speed.is_none());
        if has_no_speed {
            set_speed(state, player_id, Some(1), events);
        }
    }

    Ok(())
}

/// CR 702.179c-d: Change speed by the resolved amount in `direction` for each
/// selected player. `Decrease` honors an optional card-text-derived `floor`.
pub fn resolve_change_speed(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ChangeSpeed {
        player_scope,
        amount,
        direction,
        floor,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "expected ChangeSpeed".to_string(),
        ));
    };

    let amount = resolve_quantity_with_targets(state, amount, ability);
    let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
    if amount == 0 {
        return Ok(());
    }

    for player_id in players_for_filter(state, player_scope, ability) {
        match direction {
            SpeedDelta::Increase => increase_speed(state, player_id, amount, events),
            SpeedDelta::Decrease => decrease_speed(state, player_id, amount, *floor, events),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        Comparator, PlayerRelation, PlayerScope, QuantityExpr, QuantityRef, TargetRef,
    };
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::ObjectId;

    /// CR 119.1 + CR 810.9a: `players_for_filter` with a `PlayerAttribute`
    /// life-total predicate reads each candidate's TEAM total through the
    /// migrated `candidate_player_scalar_with_state` call (Site 6). Controller
    /// P0 (team A); opposing team B {2,3} has members at 5 and 6 (team 11).
    /// A `LifeTotal >= 10` opponent predicate counts BOTH team-B members
    /// because each reads the team total (11 >= 10), even though neither
    /// individual reaches 10. Reverting Site 6 (stateless `Some(p.life)`)
    /// reads 5 and 6 individually and the filter selects NOBODY.
    #[test]
    fn player_attribute_life_total_reads_team_total_in_2hg() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[2].life = 5;
        state.players[3].life = 6; // team B total = 11

        let filter = PlayerFilter::PlayerAttribute {
            relation: PlayerRelation::Opponent,
            attr: Box::new(QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            }),
            comparator: Comparator::GE,
            value: Box::new(QuantityExpr::Fixed { value: 10 }),
        };
        let ability = ResolvedAbility::new(
            Effect::StartYourEngines {
                player_scope: PlayerFilter::Controller,
            },
            Vec::<TargetRef>::new(),
            ObjectId(0),
            PlayerId(0),
        );

        let mut selected = players_for_filter(&state, &filter, &ability);
        selected.sort_by_key(|p| p.0);
        assert_eq!(
            selected,
            vec![PlayerId(2), PlayerId(3)],
            "both team-B members count: each reads the team total (11 >= 10)"
        );

        // Sibling: raise the threshold above the team total → neither counts.
        let filter_high = PlayerFilter::PlayerAttribute {
            relation: PlayerRelation::Opponent,
            attr: Box::new(QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            }),
            comparator: Comparator::GE,
            value: Box::new(QuantityExpr::Fixed { value: 12 }),
        };
        assert!(players_for_filter(&state, &filter_high, &ability).is_empty());
    }
}
