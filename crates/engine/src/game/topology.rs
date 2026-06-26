use crate::types::format::FormatTopology;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TeamId(pub u8);

pub(crate) fn team_id(state: &GameState, player: PlayerId) -> TeamId {
    match state.format_config.topology() {
        FormatTopology::IndividualSeats => TeamId(player.0),
        FormatTopology::FixedTeams { team_size, .. } => TeamId(player.0 / team_size),
    }
}

pub(crate) fn team_members(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    match state.format_config.topology() {
        FormatTopology::IndividualSeats => state
            .seat_order
            .iter()
            .copied()
            .filter(|&id| id == player && super::players::is_alive(state, id))
            .collect(),
        FormatTopology::FixedTeams { team_count, .. } => {
            let team = team_id(state, player);
            if team.0 >= team_count {
                return Vec::new();
            }

            state
                .players
                .iter()
                .map(|player| player.id)
                .filter(|&id| team_id(state, id) == team && super::players::is_alive(state, id))
                .collect()
        }
    }
}

pub(crate) fn teammates(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    match state.format_config.topology() {
        FormatTopology::IndividualSeats => Vec::new(),
        FormatTopology::FixedTeams { .. } => team_members(state, player)
            .into_iter()
            .filter(|&id| id != player)
            .collect(),
    }
}

pub(crate) fn is_opponent(state: &GameState, player: PlayerId, other: PlayerId) -> bool {
    player != other && team_id(state, player) != team_id(state, other)
}

pub(crate) fn team_dedup_key(state: &GameState, player: PlayerId) -> TeamId {
    team_id(state, player)
}

pub(crate) fn normalize_shared_turn_recipient(state: &GameState, player: PlayerId) -> PlayerId {
    if !state.format_config.topology().has_shared_team_turns() {
        return player;
    }

    team_members(state, player)
        .into_iter()
        .next()
        .unwrap_or(player)
}

/// CR 117.6 + CR 805.5b: In shared-team-turn multiplayer games, teams rather
/// than individual players have priority; when no player on a team acts, that
/// team passes.
pub(crate) fn priority_pass_representative(state: &GameState, player: PlayerId) -> PlayerId {
    if !state.format_config.topology().has_shared_team_turns() {
        return player;
    }

    normalize_shared_turn_recipient(state, player)
}

/// CR 805.4: In shared-team-turn formats, each team takes turns rather than
/// each player.
pub(crate) fn next_turn_representative(state: &GameState, current: PlayerId) -> PlayerId {
    if !state.format_config.topology().has_shared_team_turns() {
        return super::players::next_player(state, current);
    }

    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if seat_order.is_empty() {
        return normalize_shared_turn_recipient(state, current);
    }

    let current_team = team_id(state, current);
    let current_idx = seat_order.iter().position(|&id| id == current).unwrap_or(0);

    for offset in 1..=len {
        let idx = (current_idx + offset) % len;
        let candidate = seat_order[idx];
        if super::players::is_alive(state, candidate) && team_id(state, candidate) != current_team {
            return normalize_shared_turn_recipient(state, candidate);
        }
    }

    normalize_shared_turn_recipient(state, current)
}

pub(crate) fn priority_pass_participants(state: &GameState) -> Vec<PlayerId> {
    let participants = super::players::apnap_order(state);
    if !state.format_config.topology().has_shared_team_turns() {
        return participants;
    }

    participants
        .into_iter()
        .map(|player| priority_pass_representative(state, player))
        .fold(Vec::new(), |mut reps, rep| {
            if !reps.contains(&rep) {
                reps.push(rep);
            }
            reps
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::format::FormatConfig;

    #[test]
    fn two_hg_priority_pass_participants_are_team_representatives() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.active_player = PlayerId(0);

        assert_eq!(
            priority_pass_participants(&state),
            vec![PlayerId(0), PlayerId(2)]
        );
    }

    #[test]
    fn two_hg_priority_pass_representative_uses_living_teammate() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.active_player = PlayerId(0);
        state.players[0].is_eliminated = true;
        state.eliminated_players.push(PlayerId(0));

        assert_eq!(
            priority_pass_representative(&state, PlayerId(0)),
            PlayerId(1)
        );
        assert_eq!(
            priority_pass_participants(&state),
            vec![PlayerId(1), PlayerId(2)]
        );
    }
}
