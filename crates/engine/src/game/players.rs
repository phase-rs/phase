use crate::types::ability::PlayerRelation;
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::GameState;
use crate::types::game_state::LinkedExileSnapshot;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Returns true if the player exists in the game and is not eliminated.
pub fn is_alive(state: &GameState, player: PlayerId) -> bool {
    state
        .players
        .iter()
        .any(|p| p.id == player && !p.is_eliminated)
}

/// CR 102.1 / CR 500.1: Next living player in seat (turn) order.
///
/// Returns the next living player in seat order after `current`, wrapping around.
/// If `current` is the only living player, returns `current`.
pub fn next_player(state: &GameState, current: PlayerId) -> PlayerId {
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
        return current;
    }

    let current_idx = seat_order.iter().position(|&id| id == current).unwrap_or(0);

    for offset in 1..=len {
        let idx = (current_idx + offset) % len;
        let candidate = seat_order[idx];
        if is_alive(state, candidate) {
            return candidate;
        }
    }

    // Only living player (or no living players — shouldn't happen)
    current
}

/// CR 102.2 / CR 102.3: Opponents in two-player and multiplayer games.
///
/// Returns all living players except the given player, in seat order.
pub fn opponents(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    state
        .seat_order
        .iter()
        .copied()
        .filter(|&id| id != player && is_alive(state, id))
        .collect()
}

/// CR 102.1 / CR 102.2 / CR 109.5: Match a player against a relation to the
/// resolving effect's controller.
pub fn matches_relation(player: PlayerId, controller: PlayerId, relation: PlayerRelation) -> bool {
    match relation {
        PlayerRelation::Controller => player == controller,
        PlayerRelation::Opponent => player != controller,
        PlayerRelation::All => true,
    }
}

/// CR 608.2c + CR 109.5: Whether `player` performed `action` during the
/// current top-level resolution.
pub fn performed_action_this_way(
    state: &GameState,
    player: PlayerId,
    action: PlayerActionKind,
) -> bool {
    state.player_actions_this_way.contains(&(player, action))
}

/// CR 101.4: APNAP (Active Player, Non-Active Player) ordering.
///
/// Returns living players in APNAP order, starting from the active player
/// and proceeding in seat order.
pub fn apnap_order(state: &GameState) -> Vec<PlayerId> {
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
        return Vec::new();
    }

    let active_idx = seat_order
        .iter()
        .position(|&id| id == state.active_player)
        .unwrap_or(0);

    let mut result = Vec::new();
    for offset in 0..len {
        let idx = (active_idx + offset) % len;
        let candidate = seat_order[idx];
        if is_alive(state, candidate) {
            result.push(candidate);
        }
    }
    result
}

/// CR 603.10a + CR 607.2a: Return the cards linked as "exiled with" `source_id`.
/// Leaves-the-battlefield triggers prefer the trigger event's zone-change snapshot
/// because `TrackedBySource` links are pruned immediately on battlefield exit per
/// CR 400.7. Outside that look-back path, fall back to the live exile-link store.
pub fn linked_exile_cards_for_source(
    state: &GameState,
    source_id: ObjectId,
) -> Vec<LinkedExileSnapshot> {
    if let Some(GameEvent::ZoneChanged {
        object_id,
        from: Some(Zone::Battlefield),
        record,
        ..
    }) = state.current_trigger_event.as_ref()
    {
        if *object_id == source_id && !record.linked_exile_snapshot.is_empty() {
            return record.linked_exile_snapshot.clone();
        }
    }

    state
        .exile_links
        .iter()
        .filter(|link| link.source_id == source_id)
        .filter_map(|link| {
            state.objects.get(&link.exiled_id).and_then(|obj| {
                (obj.zone == Zone::Exile).then(|| LinkedExileSnapshot {
                    exiled_id: link.exiled_id,
                    owner: obj.owner,
                    mana_value: obj.mana_cost.mana_value(),
                })
            })
        })
        .collect()
}

/// CR 406.6 + CR 607.1: Returns true if `player` owns at least one card currently
/// in exile that is linked to `source_id`.
pub fn owns_card_exiled_by_source(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
) -> bool {
    linked_exile_cards_for_source(state, source_id)
        .iter()
        .any(|entry| entry.owner == player)
}

/// Returns teammates of the given player.
/// For Two-Headed Giant: players 0+1 are team A, players 2+3 are team B.
/// For non-team formats, returns an empty vec.
pub fn teammates(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    if !state.format_config.team_based {
        return Vec::new();
    }

    // 2HG team pairing: even-indexed players are paired with the next odd-indexed player
    let player_idx = player.0;
    let team_base = (player_idx / 2) * 2;
    let partner_idx = if player_idx == team_base {
        team_base + 1
    } else {
        team_base
    };
    let partner = PlayerId(partner_idx);

    if is_alive(state, partner) {
        vec![partner]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::format::FormatConfig;

    fn make_state(player_count: u8, config: FormatConfig) -> GameState {
        GameState::new(config, player_count, 0)
    }

    fn eliminate(state: &mut GameState, player: PlayerId) {
        if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
            p.is_eliminated = true;
        }
        state.eliminated_players.push(player);
    }

    // --- is_alive ---

    #[test]
    fn is_alive_returns_true_for_living_player() {
        let state = make_state(3, FormatConfig::free_for_all());
        assert!(is_alive(&state, PlayerId(0)));
        assert!(is_alive(&state, PlayerId(1)));
        assert!(is_alive(&state, PlayerId(2)));
    }

    #[test]
    fn is_alive_returns_false_for_eliminated_player() {
        let mut state = make_state(3, FormatConfig::free_for_all());
        eliminate(&mut state, PlayerId(1));
        assert!(!is_alive(&state, PlayerId(1)));
    }

    #[test]
    fn is_alive_returns_false_for_nonexistent_player() {
        let state = make_state(2, FormatConfig::standard());
        assert!(!is_alive(&state, PlayerId(5)));
    }

    // --- next_player ---

    #[test]
    fn next_player_returns_next_in_seat_order() {
        let state = make_state(3, FormatConfig::free_for_all());
        assert_eq!(next_player(&state, PlayerId(0)), PlayerId(1));
        assert_eq!(next_player(&state, PlayerId(1)), PlayerId(2));
    }

    #[test]
    fn next_player_wraps_around() {
        let state = make_state(3, FormatConfig::free_for_all());
        assert_eq!(next_player(&state, PlayerId(2)), PlayerId(0));
    }

    #[test]
    fn next_player_skips_eliminated() {
        let mut state = make_state(3, FormatConfig::free_for_all());
        eliminate(&mut state, PlayerId(1));
        assert_eq!(next_player(&state, PlayerId(0)), PlayerId(2));
    }

    #[test]
    fn next_player_returns_self_if_only_living() {
        let mut state = make_state(3, FormatConfig::free_for_all());
        eliminate(&mut state, PlayerId(1));
        eliminate(&mut state, PlayerId(2));
        assert_eq!(next_player(&state, PlayerId(0)), PlayerId(0));
    }

    #[test]
    fn next_player_two_player_standard() {
        let state = make_state(2, FormatConfig::standard());
        assert_eq!(next_player(&state, PlayerId(0)), PlayerId(1));
        assert_eq!(next_player(&state, PlayerId(1)), PlayerId(0));
    }

    // --- opponents ---

    #[test]
    fn opponents_returns_all_living_except_self() {
        let state = make_state(3, FormatConfig::free_for_all());
        assert_eq!(
            opponents(&state, PlayerId(0)),
            vec![PlayerId(1), PlayerId(2)]
        );
    }

    #[test]
    fn opponents_skips_eliminated() {
        let mut state = make_state(3, FormatConfig::free_for_all());
        eliminate(&mut state, PlayerId(1));
        assert_eq!(opponents(&state, PlayerId(0)), vec![PlayerId(2)]);
    }

    #[test]
    fn opponents_two_player() {
        let state = make_state(2, FormatConfig::standard());
        assert_eq!(opponents(&state, PlayerId(0)), vec![PlayerId(1)]);
        assert_eq!(opponents(&state, PlayerId(1)), vec![PlayerId(0)]);
    }

    // --- apnap_order ---

    #[test]
    fn apnap_order_starts_from_active_player() {
        let mut state = make_state(3, FormatConfig::free_for_all());
        state.active_player = PlayerId(1);
        assert_eq!(
            apnap_order(&state),
            vec![PlayerId(1), PlayerId(2), PlayerId(0)]
        );
    }

    #[test]
    fn apnap_order_skips_eliminated() {
        let mut state = make_state(3, FormatConfig::free_for_all());
        state.active_player = PlayerId(0);
        eliminate(&mut state, PlayerId(1));
        assert_eq!(apnap_order(&state), vec![PlayerId(0), PlayerId(2)]);
    }

    #[test]
    fn apnap_order_two_player_active_first() {
        let mut state = make_state(2, FormatConfig::standard());
        state.active_player = PlayerId(1);
        assert_eq!(apnap_order(&state), vec![PlayerId(1), PlayerId(0)]);
    }

    #[test]
    fn apnap_order_six_player_commander() {
        let mut state = make_state(6, FormatConfig::commander());
        state.active_player = PlayerId(3);
        assert_eq!(
            apnap_order(&state),
            vec![
                PlayerId(3),
                PlayerId(4),
                PlayerId(5),
                PlayerId(0),
                PlayerId(1),
                PlayerId(2)
            ]
        );
    }

    // --- teammates ---

    #[test]
    fn teammates_empty_for_non_team_format() {
        let state = make_state(4, FormatConfig::commander());
        assert!(teammates(&state, PlayerId(0)).is_empty());
    }

    #[test]
    fn teammates_2hg_player_0_has_teammate_1() {
        let state = make_state(4, FormatConfig::two_headed_giant());
        assert_eq!(teammates(&state, PlayerId(0)), vec![PlayerId(1)]);
    }

    #[test]
    fn teammates_2hg_player_1_has_teammate_0() {
        let state = make_state(4, FormatConfig::two_headed_giant());
        assert_eq!(teammates(&state, PlayerId(1)), vec![PlayerId(0)]);
    }

    #[test]
    fn teammates_2hg_player_2_has_teammate_3() {
        let state = make_state(4, FormatConfig::two_headed_giant());
        assert_eq!(teammates(&state, PlayerId(2)), vec![PlayerId(3)]);
    }

    #[test]
    fn teammates_2hg_player_3_has_teammate_2() {
        let state = make_state(4, FormatConfig::two_headed_giant());
        assert_eq!(teammates(&state, PlayerId(3)), vec![PlayerId(2)]);
    }

    #[test]
    fn teammates_2hg_eliminated_teammate_not_returned() {
        let mut state = make_state(4, FormatConfig::two_headed_giant());
        eliminate(&mut state, PlayerId(1));
        assert!(teammates(&state, PlayerId(0)).is_empty());
    }
}
