use crate::types::format::FormatTopology;
use crate::types::format::GameFormat;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TeamId(pub u8);

pub(crate) fn team_id(state: &GameState, player: PlayerId) -> TeamId {
    match state.format_config.topology() {
        FormatTopology::IndividualSeats => TeamId(player.0),
        FormatTopology::FixedTeams { team_size, .. } => TeamId(player.0 / team_size),
        FormatTopology::OneVsMany { archenemy, .. } => {
            if player == archenemy {
                TeamId(0)
            } else {
                TeamId(1)
            }
        }
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
        FormatTopology::OneVsMany { archenemy, .. } => {
            if player == archenemy {
                state
                    .seat_order
                    .iter()
                    .copied()
                    .filter(|&id| id == archenemy && super::players::is_alive(state, id))
                    .collect()
            } else {
                state
                    .seat_order
                    .iter()
                    .copied()
                    .filter(|&id| id != archenemy && super::players::is_alive(state, id))
                    .collect()
            }
        }
    }
}

pub(crate) fn teammates(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    match state.format_config.topology() {
        FormatTopology::IndividualSeats => Vec::new(),
        FormatTopology::FixedTeams { .. } | FormatTopology::OneVsMany { .. } => {
            team_members(state, player)
                .into_iter()
                .filter(|&id| id != player)
                .collect()
        }
    }
}

pub(crate) fn is_opponent(state: &GameState, player: PlayerId, other: PlayerId) -> bool {
    player != other && team_id(state, player) != team_id(state, other)
}

pub(crate) fn team_dedup_key(state: &GameState, player: PlayerId) -> TeamId {
    team_id(state, player)
}

pub(crate) fn archenemy(state: &GameState) -> Option<PlayerId> {
    match state.format_config.topology() {
        FormatTopology::OneVsMany { archenemy, .. } => Some(archenemy),
        FormatTopology::IndividualSeats | FormatTopology::FixedTeams { .. } => None,
    }
}

/// CR 810.4 / CR 810.8 / CR 810.9 / CR 810.10: Two-Headed Giant shares life,
/// poison, and team loss. Default Archenemy uses shared turns (CR 805) but not
/// these shared-resource rules.
pub(crate) fn has_two_headed_giant_shared_resources(state: &GameState) -> bool {
    matches!(state.format_config.format, GameFormat::TwoHeadedGiant)
}

pub(crate) fn shared_resource_members(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    if has_two_headed_giant_shared_resources(state) {
        team_members(state, player)
    } else if super::players::is_alive(state, player) {
        vec![player]
    } else {
        Vec::new()
    }
}

pub(crate) fn shared_resource_dedup_key(state: &GameState, player: PlayerId) -> TeamId {
    if has_two_headed_giant_shared_resources(state) {
        team_id(state, player)
    } else {
        TeamId(player.0)
    }
}

pub(crate) fn apnap_choice_groups(state: &GameState) -> Vec<Vec<PlayerId>> {
    apnap_choice_groups_from(state, state.active_player)
}

pub(crate) fn apnap_choice_groups_from(
    state: &GameState,
    start_player: PlayerId,
) -> Vec<Vec<PlayerId>> {
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
        return Vec::new();
    }

    if !state.format_config.topology().has_shared_team_turns() {
        let start_idx = seat_order
            .iter()
            .position(|&id| id == start_player)
            .unwrap_or(0);
        return (0..len)
            .filter_map(|offset| {
                // CR 101.4 + CR 103.1: APNAP follows the current turn-order direction.
                let idx =
                    super::players::turn_order_index(start_idx, offset, len, state.turn_direction);
                let candidate = seat_order[idx];
                super::players::is_alive(state, candidate).then_some(vec![candidate])
            })
            .collect();
    }

    let start_idx = seat_order
        .iter()
        .position(|&id| id == start_player)
        .unwrap_or(0);
    let mut seen = std::collections::BTreeSet::new();
    let mut groups = Vec::new();
    for offset in 0..len {
        // CR 101.4 + CR 103.1: APNAP follows the current turn-order direction.
        let idx = super::players::turn_order_index(start_idx, offset, len, state.turn_direction);
        let candidate = seat_order[idx];
        if !super::players::is_alive(state, candidate) {
            continue;
        }
        let key = team_dedup_key(state, candidate);
        if seen.insert(key) {
            groups.push(team_members(state, candidate));
        }
    }
    groups
}

pub(crate) fn apnap_order_from(state: &GameState, start_player: PlayerId) -> Vec<PlayerId> {
    apnap_choice_groups_from(state, start_player)
        .into_iter()
        .flatten()
        .collect()
}

pub(crate) fn apnap_team_rank(state: &GameState, player: PlayerId) -> usize {
    let groups = apnap_choice_groups(state);
    groups
        .iter()
        .position(|group| group.contains(&player))
        .unwrap_or(groups.len())
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
        // CR 103.1: the next turn proceeds in the current turn-order direction.
        return super::players::next_player_in_turn_order(state, current);
    }

    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if seat_order.is_empty() {
        return normalize_shared_turn_recipient(state, current);
    }

    let current_team = team_id(state, current);
    let current_idx = seat_order.iter().position(|&id| id == current).unwrap_or(0);

    for offset in 1..=len {
        // CR 103.1: walk seats in the current turn-order direction.
        let idx = super::players::turn_order_index(current_idx, offset, len, state.turn_direction);
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
    fn next_turn_representative_reverses_with_turn_direction() {
        use crate::types::phase::TurnDirection;
        let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
        // CR 103.1: normal turn order walks forward (P0 → P1).
        assert_eq!(next_turn_representative(&state, PlayerId(0)), PlayerId(1));
        state.turn_direction = TurnDirection::Reversed;
        // Reversed: the next turn walks backward (P0 → P3).
        assert_eq!(next_turn_representative(&state, PlayerId(0)), PlayerId(3));
    }

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

    #[test]
    fn archenemy_team_members_by_side_for_supported_player_counts() {
        for player_count in [2, 4, 6] {
            let state = GameState::new(FormatConfig::archenemy(), player_count, 42);

            assert_eq!(archenemy(&state), Some(PlayerId(0)));
            assert_eq!(team_members(&state, PlayerId(0)), vec![PlayerId(0)]);

            let heroes: Vec<PlayerId> = (1..player_count).map(PlayerId).collect();
            assert_eq!(team_members(&state, PlayerId(1)), heroes);
        }
    }

    #[test]
    fn archenemy_team_members_exclude_eliminated_heroes() {
        let mut state = GameState::new(FormatConfig::archenemy(), 6, 42);
        state.players[2].is_eliminated = true;
        state.eliminated_players.push(PlayerId(2));

        assert_eq!(
            team_members(&state, PlayerId(1)),
            vec![PlayerId(1), PlayerId(3), PlayerId(4), PlayerId(5)]
        );
        assert_eq!(
            teammates(&state, PlayerId(1)),
            vec![PlayerId(3), PlayerId(4), PlayerId(5)]
        );
    }
}
