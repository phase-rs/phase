use crate::types::ability::{ControllerRef, PlayerRelation, SeatDirection};
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

/// CR 102.1 / CR 500.1: Previous living player in seat (turn) order.
///
/// Returns the previous living player in seat order before `current`, wrapping
/// around (the seat to `current`'s right, since turn order proceeds to the
/// left per CR 101.4 / CR 103.1). Skips eliminated players. If `current` is the
/// only living player, returns `current`.
pub fn previous_player(state: &GameState, current: PlayerId) -> PlayerId {
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
        return current;
    }

    let current_idx = seat_order.iter().position(|&id| id == current).unwrap_or(0);

    for offset in 1..=len {
        // Walk backward through the seat ring (wrapping) by adding `len - offset`.
        let idx = (current_idx + len - (offset % len)) % len;
        let candidate = seat_order[idx];
        if is_alive(state, candidate) {
            return candidate;
        }
    }

    // Only living player (or no living players — shouldn't happen)
    current
}

/// CR 102.1 + CR 103.1: Single authority for seating-neighbor resolution.
///
/// Resolves the living player seated immediately to `controller`'s left or
/// right. Turn order proceeds clockwise to the active player's left
/// (CR 101.4 / CR 103.1), so `Left` walks forward in `seat_order`
/// (`next_player`) and `Right` walks backward (`previous_player`).
pub fn neighbor(state: &GameState, controller: PlayerId, direction: SeatDirection) -> PlayerId {
    match direction {
        SeatDirection::Left => next_player(state, controller),
        SeatDirection::Right => previous_player(state, controller),
    }
}

/// CR 102.2 / CR 102.3: Opponents in two-player and multiplayer games.
///
/// Returns all living players not on the given player's team, in seat order.
pub fn opponents(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    state
        .seat_order
        .iter()
        .copied()
        .filter(|&id| is_opponent(state, player, id) && is_alive(state, id))
        .collect()
}

/// CR 102.2 / CR 102.3: Whether `other` is an opponent of `player`.
pub fn is_opponent(state: &GameState, player: PlayerId, other: PlayerId) -> bool {
    if player == other {
        return false;
    }
    if state.format_config.team_based {
        team_index(player) != team_index(other)
    } else {
        true
    }
}

/// CR 102.1 / CR 102.2 / CR 102.3 / CR 109.5: Match a player against a
/// relation to the resolving effect's controller.
pub fn matches_relation(
    state: &GameState,
    player: PlayerId,
    controller: PlayerId,
    relation: PlayerRelation,
) -> bool {
    match relation {
        PlayerRelation::Controller => player == controller,
        PlayerRelation::Opponent => is_opponent(state, controller, player),
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
    apnap_order_from(state, None, state.active_player)
}

/// CR 101.4 + CR 800.4 + CR 800.4f: APNAP ordering with an optional turn-order
/// override.
///
/// When `starting_with` is `None`, behaves identically to `apnap_order` —
/// living players are returned starting from the active player and walking
/// forward in seat order, per CR 101.4 (APNAP).
///
/// When `starting_with` is `Some(ControllerRef::You)` the sequence instead
/// begins at `controller`. This is required by Join Forces ("Starting with
/// you, each player may pay any amount of mana") and other effects that
/// override the default APNAP turn-order start (CR 800.4): players act in
/// turn order, but starting from a designated player rather than the active
/// player. Other `ControllerRef` variants are not currently produced as
/// turn-order overrides on `player_scope` iteration and fall back to the
/// APNAP anchor — the match below lists each explicitly so adding a new
/// variant intentionally forces the author to declare whether it shifts
/// the start or not.
///
/// CR 800.4f: For Join Forces in particular, eliminated players cannot pay
/// the cost; for the broader API, eliminated players never act, so they are
/// filtered out regardless of branch.
pub fn apnap_order_from(
    state: &GameState,
    starting_with: Option<ControllerRef>,
    controller: PlayerId,
) -> Vec<PlayerId> {
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
        return Vec::new();
    }

    // CR 101.4 + CR 800.4: Resolve the start anchor. Each `ControllerRef`
    // variant is listed explicitly so introducing a new variant produces a
    // compile error here rather than a silent fall-back to APNAP.
    let start_player = match starting_with {
        Some(ControllerRef::You) => controller,
        None
        | Some(
            ControllerRef::Opponent
            | ControllerRef::ScopedPlayer
            | ControllerRef::TargetPlayer
            | ControllerRef::ParentTargetController
            | ControllerRef::DefendingPlayer
            | ControllerRef::SourceChosenPlayer
            | ControllerRef::ChosenPlayer { .. }
            | ControllerRef::TriggeringPlayer,
        ) => state.active_player,
    };

    let start_idx = seat_order
        .iter()
        .position(|&id| id == start_player)
        .unwrap_or(0);

    let mut result = Vec::new();
    for offset in 0..len {
        let idx = (start_idx + offset) % len;
        let candidate = seat_order[idx];
        // CR 800.4f: A player who has left the game does not pay costs or
        // make choices on objects' behalf; skip eliminated players.
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
    let team_base = team_index(player) * 2;
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

fn team_index(player: PlayerId) -> u8 {
    player.0 / 2
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

    // --- previous_player ---

    #[test]
    fn previous_player_returns_previous_in_seat_order() {
        let state = make_state(3, FormatConfig::free_for_all());
        // seat_order [P0,P1,P2]: previous of P1 is P0, previous of P2 is P1.
        assert_eq!(previous_player(&state, PlayerId(1)), PlayerId(0));
        assert_eq!(previous_player(&state, PlayerId(2)), PlayerId(1));
    }

    #[test]
    fn previous_player_wraps_around() {
        let state = make_state(3, FormatConfig::free_for_all());
        // previous of P0 wraps to the last seat P2.
        assert_eq!(previous_player(&state, PlayerId(0)), PlayerId(2));
    }

    #[test]
    fn previous_player_skips_eliminated() {
        let mut state = make_state(4, FormatConfig::free_for_all());
        // seat_order [P0,P1,P2,P3]: immediate previous of P0 is P3; eliminate it.
        eliminate(&mut state, PlayerId(3));
        assert_eq!(previous_player(&state, PlayerId(0)), PlayerId(2));
    }

    #[test]
    fn previous_player_returns_self_if_only_living() {
        let mut state = make_state(3, FormatConfig::free_for_all());
        eliminate(&mut state, PlayerId(1));
        eliminate(&mut state, PlayerId(2));
        assert_eq!(previous_player(&state, PlayerId(0)), PlayerId(0));
    }

    #[test]
    fn previous_player_two_player_standard() {
        let state = make_state(2, FormatConfig::standard());
        assert_eq!(previous_player(&state, PlayerId(0)), PlayerId(1));
        assert_eq!(previous_player(&state, PlayerId(1)), PlayerId(0));
    }

    // --- neighbor ---

    #[test]
    fn neighbor_left_is_next_right_is_previous() {
        let state = make_state(3, FormatConfig::free_for_all());
        // seat_order [P0,P1,P2], controller P0: left = next = P1, right = prev = P2.
        assert_eq!(
            neighbor(&state, PlayerId(0), SeatDirection::Left),
            PlayerId(1)
        );
        assert_eq!(
            neighbor(&state, PlayerId(0), SeatDirection::Right),
            PlayerId(2)
        );
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

    #[test]
    fn opponents_two_headed_giant_excludes_teammate() {
        let state = make_state(4, FormatConfig::two_headed_giant());
        assert_eq!(
            opponents(&state, PlayerId(0)),
            vec![PlayerId(2), PlayerId(3)]
        );
        assert!(!is_opponent(&state, PlayerId(0), PlayerId(1)));
        assert!(is_opponent(&state, PlayerId(0), PlayerId(2)));
    }

    #[test]
    fn matches_relation_opponent_excludes_two_headed_giant_teammate() {
        let state = make_state(4, FormatConfig::two_headed_giant());
        assert!(!matches_relation(
            &state,
            PlayerId(1),
            PlayerId(0),
            PlayerRelation::Opponent
        ));
        assert!(matches_relation(
            &state,
            PlayerId(2),
            PlayerId(0),
            PlayerRelation::Opponent
        ));
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

    // --- apnap_order_from ---

    #[test]
    fn apnap_order_from_none_defaults_to_active_player() {
        // CR 101.4: With no override, the order begins at the active player.
        let mut state = make_state(4, FormatConfig::commander());
        state.active_player = PlayerId(2);
        let order = apnap_order_from(&state, None, PlayerId(0));
        assert_eq!(
            order,
            vec![PlayerId(2), PlayerId(3), PlayerId(0), PlayerId(1)],
        );
    }

    #[test]
    fn apnap_order_from_starting_with_you_uses_controller() {
        // CR 101.4 + CR 800.4: Join Forces "Starting with you" overrides APNAP
        // so the controller is prompted first regardless of whose turn it is.
        let mut state = make_state(4, FormatConfig::commander());
        state.active_player = PlayerId(2);
        let order = apnap_order_from(&state, Some(ControllerRef::You), PlayerId(0));
        assert_eq!(
            order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2), PlayerId(3)],
        );
    }

    #[test]
    fn apnap_order_from_starting_with_you_three_player_active_p2() {
        // 3-player game, AP=P2, controller=P0 → P0 first, then P1, then P2.
        let mut state = make_state(3, FormatConfig::commander());
        state.active_player = PlayerId(2);
        let order = apnap_order_from(&state, Some(ControllerRef::You), PlayerId(0));
        assert_eq!(order, vec![PlayerId(0), PlayerId(1), PlayerId(2)]);
    }

    #[test]
    fn apnap_order_from_skips_eliminated_with_override() {
        // CR 800.4f: Eliminated players are filtered out of the starting-with
        // iteration just like the default APNAP path.
        let mut state = make_state(4, FormatConfig::commander());
        state.active_player = PlayerId(3);
        eliminate(&mut state, PlayerId(1));
        let order = apnap_order_from(&state, Some(ControllerRef::You), PlayerId(0));
        assert_eq!(order, vec![PlayerId(0), PlayerId(2), PlayerId(3)]);
    }

    #[test]
    fn apnap_order_from_other_controller_refs_fall_back_to_apnap() {
        // Only `Some(You)` shifts the start; other refs (Opponent, etc.) are
        // not currently produced as turn-order overrides — fall back to APNAP.
        let mut state = make_state(3, FormatConfig::free_for_all());
        state.active_player = PlayerId(1);
        let order = apnap_order_from(&state, Some(ControllerRef::Opponent), PlayerId(0));
        assert_eq!(order, vec![PlayerId(1), PlayerId(2), PlayerId(0)]);
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
