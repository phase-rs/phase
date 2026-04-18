use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// CR 702.179f: Effects that refer to speed treat "no speed" as 0.
pub fn effective_speed(state: &GameState, player: PlayerId) -> u8 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.speed)
        .unwrap_or(0)
}

/// CR 702.179e: A player has max speed if their speed is 4.
/// Some effects allow speed to exceed 4 and still count as max speed at 4 or greater.
pub fn has_max_speed(state: &GameState, player: PlayerId) -> bool {
    let speed = effective_speed(state, player);
    if can_increase_speed_beyond_4(state, player) {
        speed >= 4
    } else {
        speed == 4
    }
}

pub fn set_speed(
    state: &mut GameState,
    player: PlayerId,
    new_speed: Option<u8>,
    events: &mut Vec<GameEvent>,
) {
    let Some(player_state) = state.players.iter_mut().find(|p| p.id == player) else {
        return;
    };
    let old_speed = player_state.speed;
    if old_speed == new_speed {
        return;
    }
    player_state.speed = new_speed;
    events.push(GameEvent::SpeedChanged {
        player,
        old_speed,
        new_speed,
    });
}

/// CR 702.179c-d: Increasing speed sets absent speed to the increase amount and otherwise
/// increments it, subject to the default cap of 4 unless a static ability says otherwise.
pub fn increase_speed(
    state: &mut GameState,
    player: PlayerId,
    amount: u8,
    events: &mut Vec<GameEvent>,
) {
    let current = state
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.speed);
    let Some(base) = current else {
        set_speed(state, player, Some(amount), events);
        return;
    };

    let increased = base.saturating_add(amount);
    let capped = if can_increase_speed_beyond_4(state, player) {
        increased
    } else {
        increased.min(4)
    };
    set_speed(state, player, Some(capped), events);
}

/// CR 702.179a: Start your engines checks whether a player controls a permanent with the keyword.
pub fn controls_start_your_engines(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state.objects.get(id).is_some_and(|obj| {
            obj.controller == player && obj.has_keyword(&Keyword::StartYourEngines)
        })
    })
}

/// Card-specific rule modification for effects like Gomif.
pub fn can_increase_speed_beyond_4(state: &GameState, player: PlayerId) -> bool {
    // CR 702.26b + CR 604.1: `active_static_definitions` owns the gating.
    state.battlefield.iter().any(|&id| {
        state.objects.get(&id).is_some_and(|obj| {
            if obj.controller != player {
                return false;
            }
            crate::game::functioning_abilities::active_static_definitions(state, obj)
                .any(|def| def.mode == StaticMode::SpeedCanIncreaseBeyondFour)
        })
    })
}

pub fn mark_speed_trigger_used(state: &mut GameState, player: PlayerId) {
    if let Some(player_state) = state.players.iter_mut().find(|p| p.id == player) {
        player_state.speed_trigger_used_this_turn = true;
    }
}

pub fn speed_trigger_available(state: &GameState, player: PlayerId) -> bool {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|p| !p.speed_trigger_used_this_turn)
}

pub fn speed_key_source() -> ObjectId {
    ObjectId(0)
}
