use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::speed::{increase_speed, set_speed};
use crate::types::ability::{Effect, EffectError, PlayerFilter, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

fn players_for_filter(
    state: &GameState,
    filter: &PlayerFilter,
    controller: PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> Vec<PlayerId> {
    match filter {
        PlayerFilter::Controller => vec![controller],
        PlayerFilter::Opponent => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated && player.id != controller)
            .map(|player| player.id)
            .collect(),
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
                .filter(|player| !player.is_eliminated && player.id != controller)
                .filter(|player| triggering.is_none_or(|pid| pid != player.id))
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

    for player_id in players_for_filter(state, player_scope, ability.controller, ability.source_id)
    {
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

/// CR 702.179c-d: Increase speed by the resolved amount for each selected player.
pub fn resolve_increase(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::IncreaseSpeed {
        player_scope,
        amount,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "expected IncreaseSpeed".to_string(),
        ));
    };

    let amount = resolve_quantity_with_targets(state, amount, ability);
    let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
    if amount == 0 {
        return Ok(());
    }

    for player_id in players_for_filter(state, player_scope, ability.controller, ability.source_id)
    {
        increase_speed(state, player_id, amount, events);
    }

    Ok(())
}
