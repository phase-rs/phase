use crate::types::ability::{AggregateFunction, ControllerRef, PlayerRelation, SeatDirection};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::GameState;
use crate::types::game_state::LinkedExileSnapshot;
use crate::types::identifiers::ObjectId;
use crate::types::phase::TurnDirection;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Returns true if the player exists in the game and is not eliminated.
pub fn is_alive(state: &GameState, player: PlayerId) -> bool {
    state
        .players
        .iter()
        .any(|p| p.id == player && !p.is_eliminated)
}

/// CR 607.2d / CR 607.2m (by analogy): true iff `player`'s durable per-player
/// `chosen_attributes` records a `ChosenAttribute::Label` equal to `label`
/// (case-insensitive). Single authority consulted by every "player who last
/// chose <anchor>" read site — `TargetFilter::PlayerWhoChoseLabel` (land-drop
/// static), `FilterProp::ControllerChoseLabel` (creature anthem), and the
/// `SwapChosenLabels` chaos effect — so the anchor-label predicate is defined
/// exactly once. Case-insensitive so parser canonicalization never desyncs.
pub fn player_last_chose_label(state: &GameState, player: PlayerId, label: &str) -> bool {
    state.players.iter().any(|p| {
        p.id == player
            && p.chosen_attributes.iter().any(|a| {
                matches!(a, crate::types::ability::ChosenAttribute::Label(l)
                    if l.eq_ignore_ascii_case(label))
            })
    })
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

/// CR 103.1: Seat index reached by walking `offset` seats from `start_idx` in
/// the current turn-order direction. `Normal` walks forward (clockwise, the
/// CR 103.1 default); `Reversed` walks backward (Temple of Atropos, Aeon Engine,
/// Time Distortion). This is the SINGLE authority for turn-order direction —
/// physical seating (`neighbor`/`next_player`/`previous_player`) is deliberately
/// NOT routed through it, since "the player to your left" is fixed regardless of
/// turn direction (Pramikon, Sky Rampart). The `Reversed` arithmetic matches the
/// backward walk in `previous_player`.
pub(crate) fn turn_order_index(
    start_idx: usize,
    offset: usize,
    len: usize,
    dir: TurnDirection,
) -> usize {
    match dir {
        TurnDirection::Normal => (start_idx + offset) % len,
        TurnDirection::Reversed => (start_idx + len - (offset % len)) % len,
    }
}

/// CR 101.4 / CR 103.1: Next living player to take a turn, in the current
/// turn-order direction. `Normal` == [`next_player`]; `Reversed` ==
/// [`previous_player`]. Use this for turn-order progression; use `next_player` /
/// `previous_player` directly only for fixed physical-seating queries.
pub fn next_player_in_turn_order(state: &GameState, current: PlayerId) -> PlayerId {
    match state.turn_direction {
        TurnDirection::Normal => next_player(state, current),
        TurnDirection::Reversed => previous_player(state, current),
    }
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

/// CR 102.2 + CR 508.1c: The nearest *opponent* in the given seating direction,
/// skipping living teammates. In a free-for-all every other seat is an opponent
/// so this equals [`neighbor`], but in team formats (Two-Headed Giant, CR 810)
/// an adjacent teammate is not the "nearest opponent" — the walk continues past
/// them to the first living opponent in that direction.
///
/// Walks the seat ring one living player at a time via [`neighbor`]; returns
/// `None` only if the walk returns to `controller` without finding an opponent
/// (e.g. `controller` is the sole living player). Termination is guaranteed:
/// each step advances deterministically around the finite living-seat ring.
pub fn nearest_opponent(
    state: &GameState,
    controller: PlayerId,
    direction: SeatDirection,
) -> Option<PlayerId> {
    let mut candidate = neighbor(state, controller, direction);
    while candidate != controller {
        if is_opponent(state, controller, candidate) {
            return Some(candidate);
        }
        candidate = neighbor(state, candidate, direction);
    }
    None
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
    super::topology::is_opponent(state, player, other)
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
            | ControllerRef::TargetOpponent
            | ControllerRef::ParentTargetController
            | ControllerRef::ParentTargetOwner
            | ControllerRef::DefendingPlayer
            | ControllerRef::SourceChosenPlayer
            | ControllerRef::ChosenPlayer { .. }
            | ControllerRef::TriggeringPlayer
            // CR 303.4b: Enchanted-player scope is not enumerable. Fail closed.
            | ControllerRef::EnchantedPlayer
            // CR 102.1: the active player is exactly this default anchor.
            | ControllerRef::ActivePlayer,
        ) => state.active_player,
    };

    if state.format_config.topology().has_shared_team_turns() {
        return super::topology::apnap_order_from(state, start_player);
    }

    let start_idx = seat_order
        .iter()
        .position(|&id| id == start_player)
        .unwrap_or(0);

    let mut result = Vec::new();
    for offset in 0..len {
        // CR 101.4 + CR 103.1: APNAP follows the current turn-order direction.
        let idx = turn_order_index(start_idx, offset, len, state.turn_direction);
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

    let live: Vec<LinkedExileSnapshot> = state
        .exile_links
        .iter()
        .filter(|link| link.source_id == source_id)
        .filter_map(|link| {
            state.objects.get(&link.exiled_id).and_then(|obj| {
                (obj.zone == Zone::Exile).then(|| LinkedExileSnapshot {
                    exiled_id: link.exiled_id,
                    owner: obj.owner,
                    // CR 202.3d + CR 709.4b: the exiled card is off the stack, so
                    // a split card records its combined mana value.
                    mana_value: obj.effective_mana_value(),
                })
            })
        })
        .collect();
    if !live.is_empty() {
        return live;
    }

    // CR 607.2b + CR 603.10e: The live links are gone (the source left the
    // battlefield — e.g. sacrificed as its own ability's cost). Fall back to the
    // persisted linked-exile LKI, filtered to cards STILL in exile so stale
    // entries (cards that later left exile) contribute nothing.
    state
        .linked_exile_lki
        .get(&source_id)
        .map(|snapshots| {
            snapshots
                .iter()
                .filter(|snap| {
                    state
                        .objects
                        .get(&snap.exiled_id)
                        .is_some_and(|obj| obj.zone == Zone::Exile)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
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
    super::topology::teammates(state, player)
}

/// CR 810.9a + CR 810.9d: Fold a player population into one i32 by aggregating
/// each DISTINCT team's shared `team_life_total` exactly once (dedup by team).
/// Min/Max = extremum over team totals; Sum = Σ team totals (no double-count).
/// Empty population → 0. Off-team every player is its own singleton team, so
/// this matches a per-individual fold: the dedup key falls back to `pid.0`,
/// which is distinct per player even when two players share a `team_index`
/// (e.g. a 1v1 where players 0 and 1 are both `team_index == 0`).
/// CR 810.9d is the confirming example: a per-team extremum (Repay in Kind)
/// reads each team's total once, not each member.
pub(crate) fn aggregate_over_teams<I>(
    state: &GameState,
    players: I,
    aggregate: AggregateFunction,
) -> i32
where
    I: IntoIterator<Item = PlayerId>,
{
    let mut seen = std::collections::BTreeSet::new();
    let team_totals = players.into_iter().filter_map(|pid| {
        let key = super::topology::shared_resource_dedup_key(state, pid);
        seen.insert(key).then(|| team_life_total(state, pid))
    });
    match aggregate {
        AggregateFunction::Max => team_totals.max().unwrap_or(0),
        AggregateFunction::Min => team_totals.min().unwrap_or(0),
        AggregateFunction::Sum => team_totals.sum(),
    }
}

/// CR 810.4 + CR 810.9a: A player's team's shared life total. In non-team
/// formats this is just the player's own life total — `teammates` returns
/// empty, so the sum degenerates to the single value. CR 810.9a: "If a cost
/// or effect needs to know the value of an individual player's life total,
/// that cost or effect uses the team's life total instead" — callers that
/// read an individual life total for a comparison, cost, or SBA check in a
/// team-based format must go through this accessor rather than `Player::life`
/// directly. The underlying per-player `life` fields remain the single
/// source of truth (CR 810.9: life loss/gain still happens to "each player
/// individually") — this is a pure derived sum, not a separate stored pool.
pub fn team_life_total(state: &GameState, player: PlayerId) -> i32 {
    super::topology::shared_resource_members(state, player)
        .into_iter()
        .filter_map(|member| state.players.iter().find(|p| p.id == member))
        .map(|p| p.life)
        .sum()
}

/// CR 810.10 + CR 810.10a: A player's team's shared poison-counter total.
/// Mirrors `team_life_total` — a pure derived sum over `Player::poison_counters`
/// for the player and their (living) teammates. Non-team formats degenerate
/// to the player's own count.
pub fn team_poison_total(state: &GameState, player: PlayerId) -> u32 {
    super::topology::shared_resource_members(state, player)
        .into_iter()
        .filter_map(|member| state.players.iter().find(|p| p.id == member))
        .map(|p| p.poison_counters)
        .sum()
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

    // --- turn-order direction (CR 103.1) ---

    #[test]
    fn turn_order_index_walks_backward_when_reversed() {
        // Seat ring of 4: from index 1, offset 1.
        assert_eq!(turn_order_index(1, 1, 4, TurnDirection::Normal), 2);
        assert_eq!(turn_order_index(1, 1, 4, TurnDirection::Reversed), 0);
        // Wrap: from index 0 backward one seat → 3.
        assert_eq!(turn_order_index(0, 1, 4, TurnDirection::Reversed), 3);
        // offset 0 is the start seat regardless of direction.
        assert_eq!(turn_order_index(2, 0, 4, TurnDirection::Normal), 2);
        assert_eq!(turn_order_index(2, 0, 4, TurnDirection::Reversed), 2);
    }

    #[test]
    fn next_player_in_turn_order_follows_direction() {
        let mut state = make_state(4, FormatConfig::free_for_all());
        // Normal: next of P1 is P2; Reversed: next of P1 is P0.
        assert_eq!(next_player_in_turn_order(&state, PlayerId(1)), PlayerId(2));
        state.turn_direction = TurnDirection::Reversed;
        assert_eq!(next_player_in_turn_order(&state, PlayerId(1)), PlayerId(0));
        // Physical seating (neighbor) is unaffected by turn direction.
        assert_eq!(
            neighbor(&state, PlayerId(1), SeatDirection::Left),
            PlayerId(2),
            "left neighbor is fixed regardless of turn direction"
        );
    }

    #[test]
    fn apnap_order_reverses_with_turn_direction() {
        let mut state = make_state(4, FormatConfig::free_for_all());
        state.active_player = PlayerId(0);
        assert_eq!(
            apnap_order(&state),
            vec![PlayerId(0), PlayerId(1), PlayerId(2), PlayerId(3)],
        );
        state.turn_direction = TurnDirection::Reversed;
        assert_eq!(
            apnap_order(&state),
            vec![PlayerId(0), PlayerId(3), PlayerId(2), PlayerId(1)],
            "CR 101.4: APNAP follows the reversed turn order",
        );
    }

    // --- nearest_opponent ---

    #[test]
    fn nearest_opponent_equals_neighbor_in_free_for_all() {
        // Individual seats: every other player is an opponent, so the nearest
        // opponent is just the adjacent seat.
        let state = make_state(4, FormatConfig::free_for_all());
        for dir in [SeatDirection::Left, SeatDirection::Right] {
            assert_eq!(
                nearest_opponent(&state, PlayerId(0), dir),
                Some(neighbor(&state, PlayerId(0), dir)),
                "free-for-all nearest opponent is the adjacent seat ({dir:?})"
            );
        }
    }

    #[test]
    fn nearest_opponent_skips_teammate_in_two_headed_giant() {
        // 2HG: teams {P0,P1} and {P2,P3}, seat order [P0,P1,P2,P3]. P0's left
        // neighbor P1 is a TEAMMATE; the nearest opponent to the left is P2.
        let state = make_state(4, FormatConfig::two_headed_giant());
        assert!(
            !is_opponent(&state, PlayerId(0), PlayerId(1)),
            "P1 is P0's teammate in 2HG"
        );
        assert_eq!(
            neighbor(&state, PlayerId(0), SeatDirection::Left),
            PlayerId(1),
            "the adjacent left seat is the teammate"
        );
        assert_eq!(
            nearest_opponent(&state, PlayerId(0), SeatDirection::Left),
            Some(PlayerId(2)),
            "nearest opponent skips the teammate to the first opponent P2"
        );
        assert_eq!(
            nearest_opponent(&state, PlayerId(0), SeatDirection::Right),
            Some(PlayerId(3)),
            "to the right, P3 is the first opponent"
        );
    }

    #[test]
    fn nearest_opponent_none_when_sole_survivor() {
        let mut state = make_state(4, FormatConfig::free_for_all());
        for p in [PlayerId(1), PlayerId(2), PlayerId(3)] {
            eliminate(&mut state, p);
        }
        assert_eq!(
            nearest_opponent(&state, PlayerId(0), SeatDirection::Left),
            None,
            "no living opponent in any direction → None"
        );
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
        assert!(is_opponent(&state, PlayerId(0), PlayerId(1)));
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
        assert!(!is_opponent(&state, PlayerId(0), PlayerId(1)));
        assert!(is_opponent(&state, PlayerId(0), PlayerId(2)));
        assert!(is_opponent(&state, PlayerId(0), PlayerId(3)));
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

    // --- team_life_total / team_poison_total ---

    /// CR 810.4: "Each team has a shared life total, which starts at 30
    /// life" — the TEAM's combined total at game start must be 30, not 30
    /// per player (60 per team). Regression for a bug where `GameState::new`
    /// gave every player the full `starting_life` regardless of team size.
    #[test]
    fn team_life_total_at_game_start_is_30_not_60() {
        let state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        assert_eq!(team_life_total(&state, PlayerId(0)), 30);
        assert_eq!(team_life_total(&state, PlayerId(1)), 30);
        assert_eq!(team_life_total(&state, PlayerId(2)), 30);
        assert_eq!(team_life_total(&state, PlayerId(3)), 30);
    }

    #[test]
    fn new_two_hg_initializes_15_per_seat_30_team_total() {
        let state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        assert!(state.players.iter().all(|player| player.life == 15));
        assert_eq!(team_life_total(&state, PlayerId(0)), 30);
        assert_eq!(team_life_total(&state, PlayerId(2)), 30);

        let standard = GameState::new(FormatConfig::standard(), 2, 0);
        assert_eq!(standard.players[0].life, 20);
        assert_eq!(standard.players[1].life, 20);

        let commander = GameState::new(FormatConfig::commander(), 4, 0);
        assert!(commander.players.iter().all(|player| player.life == 40));
    }

    /// Outside team-based formats, `team_life_total` degenerates to the
    /// player's own (full, unsplit) starting life — no regression from the
    /// 2HG even-split fix.
    #[test]
    fn team_life_total_non_team_format_is_full_starting_life() {
        let state = GameState::new(FormatConfig::commander(), 4, 0);
        assert_eq!(team_life_total(&state, PlayerId(0)), 40);
    }

    #[test]
    fn team_poison_total_sums_living_teammates() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[0].poison_counters = 6;
        state.players[1].poison_counters = 9;
        assert_eq!(team_poison_total(&state, PlayerId(0)), 15);
        assert_eq!(team_poison_total(&state, PlayerId(1)), 15);
        // Opposing team is unaffected.
        assert_eq!(team_poison_total(&state, PlayerId(2)), 0);
    }

    #[test]
    fn archenemy_life_and_poison_are_individual_not_shared_by_side() {
        let mut state = GameState::new(FormatConfig::archenemy(), 4, 0);
        state.players[1].poison_counters = 6;
        state.players[2].poison_counters = 9;

        assert_eq!(team_life_total(&state, PlayerId(0)), 40);
        assert_eq!(team_life_total(&state, PlayerId(1)), 20);
        assert_eq!(team_life_total(&state, PlayerId(2)), 20);
        assert_eq!(team_poison_total(&state, PlayerId(1)), 6);
        assert_eq!(team_poison_total(&state, PlayerId(2)), 9);
    }

    // --- aggregate_over_teams ---

    /// CR 810.9a + CR 810.9d: aggregating life over a population folds each
    /// DISTINCT team's shared total exactly once. Over the two opponents of
    /// team A (players 2 and 3 with 9 and 5 = team total 14), Sum/Max/Min all
    /// read 14 ONCE — not 28 (double-counted) and not 9 (individual). This is
    /// the byte-distinguishing regression for Malignus-style off-team reads.
    #[test]
    fn aggregate_over_teams_dedups_a_shared_team() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[2].life = 9;
        state.players[3].life = 5;
        let opp_team = vec![PlayerId(2), PlayerId(3)];
        assert_eq!(
            aggregate_over_teams(&state, opp_team.clone(), AggregateFunction::Sum),
            14,
            "Sum must count the shared team total once, not 28"
        );
        assert_eq!(
            aggregate_over_teams(&state, opp_team.clone(), AggregateFunction::Max),
            14
        );
        assert_eq!(
            aggregate_over_teams(&state, opp_team, AggregateFunction::Min),
            14
        );
    }

    /// The dedup key falls back to `pid.0` off-team so two players that share a
    /// `team_index` in a NON-team format are NOT collapsed. In Commander,
    /// players 0 and 1 both have `team_index == 0` (0/2 and 1/2); a bare
    /// `team_index` key would drop one and break Sum. With the `pid.0` guard,
    /// Sum over [11, 7] is 18 (both counted as singleton teams).
    #[test]
    fn aggregate_over_teams_non_team_format_keeps_players_distinct() {
        let mut state = GameState::new(FormatConfig::commander(), 4, 0);
        state.players[0].life = 11;
        state.players[1].life = 7;
        assert_eq!(
            aggregate_over_teams(
                &state,
                vec![PlayerId(0), PlayerId(1)],
                AggregateFunction::Sum
            ),
            18,
            "non-team players sharing a team_index must stay distinct via the pid.0 guard"
        );
    }

    /// Empty population → 0 for every aggregate.
    #[test]
    fn aggregate_over_teams_empty_population_is_zero() {
        let state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        let empty: Vec<PlayerId> = Vec::new();
        assert_eq!(
            aggregate_over_teams(&state, empty.clone(), AggregateFunction::Max),
            0
        );
        assert_eq!(
            aggregate_over_teams(&state, empty, AggregateFunction::Sum),
            0
        );
    }
}
