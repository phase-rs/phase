use crate::types::format::FormatTopology;
use crate::types::format::GameFormat;
use crate::types::format::TurnStructure;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TeamId(pub u8);

/// One SIDE of the game: a group of seats that share a win/loss fate, together
/// with how that side takes turns. A side's `turn_structure` is its own axis, so
/// different sides can differ — the whole point of the general model: Horde's
/// survivor side takes a shared team turn (CR 805) while each Horde takes its own
/// individual turn (alternating, LOTR Two Towers), and Emperor's teams take
/// individual turns (CR 809.4). The other per-side axis — whether a side pools
/// life/poison (2HG's shared team vs Emperor's independent teammates, CR 809.7) —
/// is [`has_shared_life`], kept as a direct O(1) predicate (like [`team_id`])
/// rather than a field so it stays allocation-free in the hot SBA path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Side {
    pub seats: Vec<PlayerId>,
    pub turn_structure: TurnStructure,
}

/// The game's SIDES — the single grouping+turn-structure authority. [`team_members`]
/// derives its grouping from this; the turn-rotation / priority / APNAP functions
/// derive their per-side turn structure from it; [`team_id`] is kept as an O(1)
/// equality key and pinned consistent with this by `team_id_matches_side_index`.
///
/// Sides include EVERY seat (alive or not) so ordering is stable — a side's index
/// reproduces the historical [`TeamId`]: IndividualSeats is one side per seat
/// ordered by id; FixedTeams is one side per team; OneVsMany is the archenemy side
/// first, then the many (in seat order, matching the pre-refactor `team_members`).
/// Alive-filtering is the caller's job.
///
/// It currently REPRODUCES the three [`FormatTopology`] shapes exactly
/// (behavior-preserving): each side's turn structure is IndividualTurns for
/// IndividualSeats and the topology's own `turn_structure` for FixedTeams /
/// OneVsMany — so every side of a given format shares one structure today. Mixed
/// structures (Horde, Emperor) arrive when those formats emit their own sides.
pub(crate) fn sides(state: &GameState) -> Vec<Side> {
    // A two-Horde-force deck (LOTR "Two Towers") emits a two-side shape the
    // `FormatTopology` enum can't: the survivors are one side, both Horde seats an
    // ALLIED side (they never attack each other and share a collective loss). BOTH
    // sides take SHARED team turns (CR 805) — the game is 2HG-shaped: survivors and
    // the Horde alternate turns. The one twist ("the hordes alternate turns") is
    // that the Horde side's ACTIVE commander rotates each Horde turn (Sauron, then
    // Saruman, …), so the side takes one turn per round but a different seat drives
    // it — see `next_turn_representative`. A single-Horde game has no second Horde
    // seat and falls through to the OneVsMany shape below.
    if let Some(hordes) = two_horde_seats(state) {
        // Seat order gives a stable side ordering (matching OneVsMany, which the
        // Horde replaces): survivors first (side 0), the Horde side second.
        let survivors: Vec<PlayerId> = state
            .seat_order
            .iter()
            .copied()
            .filter(|id| !hordes.contains(id))
            .collect();
        let horde_seats: Vec<PlayerId> = state
            .seat_order
            .iter()
            .copied()
            .filter(|id| hordes.contains(id))
            .collect();
        // Life-sharing is a per-side axis carried by [`has_shared_life`]: the
        // survivor side pools life (like 2HG), while the two allied Horde seats do
        // NOT pool with each other — each has no life total anyway.
        return vec![
            Side {
                seats: survivors,
                turn_structure: TurnStructure::SharedTeamTurns,
            },
            Side {
                seats: horde_seats,
                turn_structure: TurnStructure::SharedTeamTurns,
            },
        ];
    }

    match state.format_config.topology() {
        FormatTopology::IndividualSeats => {
            let mut ids: Vec<PlayerId> = state.players.iter().map(|p| p.id).collect();
            ids.sort_by_key(|id| id.0);
            ids.into_iter()
                .map(|id| Side {
                    seats: vec![id],
                    turn_structure: TurnStructure::IndividualTurns,
                })
                .collect()
        }
        FormatTopology::FixedTeams {
            team_size,
            team_count,
            turn_structure,
        } => {
            let mut teams: Vec<Vec<PlayerId>> = vec![Vec::new(); team_count as usize];
            let mut ids: Vec<PlayerId> = state.players.iter().map(|p| p.id).collect();
            ids.sort_by_key(|id| id.0);
            for id in ids {
                let team = (id.0 / team_size) as usize;
                if let Some(members) = teams.get_mut(team) {
                    members.push(id);
                }
            }
            teams
                .into_iter()
                .map(|seats| Side {
                    seats,
                    turn_structure,
                })
                .collect()
        }
        FormatTopology::OneVsMany {
            archenemy,
            turn_structure,
        } => {
            let many: Vec<PlayerId> = state
                .seat_order
                .iter()
                .copied()
                .filter(|&id| id != archenemy)
                .collect();
            vec![
                Side {
                    seats: vec![archenemy],
                    turn_structure,
                },
                Side {
                    seats: many,
                    turn_structure,
                },
            ]
        }
    }
}

/// The side that contains `player`, if any.
pub(crate) fn side_of(state: &GameState, player: PlayerId) -> Option<Side> {
    sides(state).into_iter().find(|s| s.seats.contains(&player))
}

/// The designated Horde seats when this game uses the two-Horde MIXED side shape
/// (a Horde game with 2+ designated seats), else `None`. A single-Horde game
/// (0 or 1 designated seats) is exactly the plain OneVsMany archenemy shape, so
/// this returns `None` and callers fall through to the [`FormatTopology`] paths.
///
/// Reads the `horde_seats` field directly (not [`crate::game::horde::horde_seats`],
/// which clones + falls back to the sole archenemy) so [`team_id`] stays an O(1),
/// allocation-free equality key. The `len() >= 2` gate is equivalent: the
/// fallback only ever yields 0 or 1 seat.
fn two_horde_seats(state: &GameState) -> Option<&[PlayerId]> {
    (state.format_config.format == GameFormat::Horde && state.horde_seats.len() >= 2)
        .then_some(state.horde_seats.as_slice())
}

pub(crate) fn team_id(state: &GameState, player: PlayerId) -> TeamId {
    // Two-Horde MIXED shape: survivors are side 0, the allied Horde seats side 1
    // (kept consistent with [`sides`] and pinned by `team_id_matches_side_index`).
    if let Some(hordes) = two_horde_seats(state) {
        return if hordes.contains(&player) {
            TeamId(1)
        } else {
            TeamId(0)
        };
    }
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

/// Which seats an APNAP or team walk admits. Live rules walks admit only
/// players still in the game (CR 800.4a); `IncludingDeparted` exists for
/// restore-time reconstruction of a population fixed before players left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeatAdmission {
    Living,
    IncludingDeparted,
}

impl SeatAdmission {
    fn admits(self, state: &GameState, player: PlayerId) -> bool {
        match self {
            SeatAdmission::Living => super::players::is_alive(state, player),
            SeatAdmission::IncludingDeparted => true,
        }
    }
}

pub(crate) fn team_members(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    team_members_admitting(state, player, SeatAdmission::Living)
}

/// Whether `player`'s side takes a single shared team turn (CR 805) rather than
/// each member taking an individual turn. Per-side: the question is asked about
/// a specific seat's side (the ACTIVE player's side at the peripheral consumers —
/// land-drop, draw step, turn-control authorization), so a future mixed game
/// answers per side without touching the call sites. This is THE turn-structure
/// predicate — it replaced a whole-format one. Every side of a game shares one
/// structure today (2HG-shaped Horde included), so the answer currently matches
/// the old whole-format predicate for all sided players.
pub(crate) fn side_takes_shared_turn(state: &GameState, player: PlayerId) -> bool {
    side_of(state, player).is_some_and(|s| s.turn_structure == TurnStructure::SharedTeamTurns)
}

fn team_members_admitting(
    state: &GameState,
    player: PlayerId,
    admission: SeatAdmission,
) -> Vec<PlayerId> {
    // The admitted members of `player`'s side (see [`sides`], the grouping
    // authority). A player with no side (shouldn't happen) has no teammates.
    // Per-side rather than whole-format, so a mixed game (Horde: one shared
    // survivor side plus one individual side per Horde seat) groups each seat
    // with its own side.
    side_of(state, player)
        .map(|side| side.seats)
        .unwrap_or_default()
        .into_iter()
        .filter(|&id| admission.admits(state, id))
        .collect()
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
/// these shared-resource rules. Only the 2HG game-over branch still reads the
/// whole bundle at once — life, poison, and loss each have their own
/// per-player predicate below, because Archenemy Commander shares life
/// (CR 904.13b) while keeping poison individual (CR 904.13c).
pub(crate) fn has_two_headed_giant_shared_resources(state: &GameState) -> bool {
    state.format_config.format.is_two_headed_giant_family()
}

/// CR 810.4 / CR 810.8 / CR 810.9: Whether `player`'s SIDE pools life as a
/// team — the per-side life axis. This is the single authority every
/// per-player life aggregation goes through ([`shared_life_members`]/
/// [`shared_life_dedup_key`], and the life-loss / can't-gain-or-lose-life
/// checks in `elimination`/`sba`/`static_abilities`). Team loss rides with
/// shared life — CR 810.8a in 2HG, CR 810.8a-via-904.13b for the heroes —
/// so the elimination cascade and the can't-lose teammate folds gate on
/// this predicate too.
///
/// Computed directly (like [`team_id`]) rather than off [`sides`], so it stays an
/// O(1), allocation-free check in the hot SBA path:
/// - The 2HG family (`FixedTeams`, 2HG and 2HG Commander): the team pools. A
///   future individual-turn `FixedTeams` (Emperor) does NOT (CR 809.7) —
///   distinguished by format, not topology.
/// - Archenemy Commander (`OneVsMany`): the heroes pool (CR 904.13b applies
///   CR 810.8/CR 810.9 to the opposing team); the archenemy reads their own
///   60 (CR 904.13b).
/// - Single-Horde (`OneVsMany`): the "many" survivor side pools; the Horde
///   seat (no life total) never does. Default Archenemy heroes stay
///   independent (CR 904.5).
/// - Two-Horde MIXED shape: the survivor side pools; each Horde seat does NOT
///   (it has no life total, and the two Horde seats are not a shared-life
///   team). This is the case a whole-game predicate gets wrong.
/// - Free-for-all: never.
pub(crate) fn has_shared_life(state: &GameState, player: PlayerId) -> bool {
    if state.format_config.format.is_two_headed_giant_family() {
        return true;
    }
    if state.format_config.format == GameFormat::ArchenemyCommander {
        return Some(player) != archenemy(state);
    }
    if let Some(hordes) = two_horde_seats(state) {
        // Survivors pool; the allied (life-total-less) Horde seats do not.
        return !hordes.contains(&player);
    }
    match state.format_config.topology() {
        FormatTopology::OneVsMany { archenemy, .. } => {
            player != archenemy && state.format_config.format == GameFormat::Horde
        }
        FormatTopology::IndividualSeats | FormatTopology::FixedTeams { .. } => false,
    }
}

/// CR 810.10: Whether `player`'s poison total is shared with their team: the
/// 2HG family plus the Horde survivor sides (whose shared poison rides with
/// their shared life). Archenemy Commander explicitly keeps poison
/// individual — the archenemy loses at 10+ and each hero loses at 10+
/// (CR 904.13c) — so it answers false here while answering true for
/// life above.
pub(crate) fn has_shared_poison(state: &GameState, player: PlayerId) -> bool {
    if state.format_config.format.is_two_headed_giant_family() {
        return true;
    }
    if let Some(hordes) = two_horde_seats(state) {
        // Survivors pool; the allied Horde seats do not.
        return !hordes.contains(&player);
    }
    match state.format_config.topology() {
        FormatTopology::OneVsMany { archenemy, .. } => {
            player != archenemy && state.format_config.format == GameFormat::Horde
        }
        FormatTopology::IndividualSeats | FormatTopology::FixedTeams { .. } => false,
    }
}

pub(crate) fn shared_life_members(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    if has_shared_life(state, player) {
        team_members(state, player)
    } else if super::players::is_alive(state, player) {
        vec![player]
    } else {
        Vec::new()
    }
}

pub(crate) fn shared_poison_members(state: &GameState, player: PlayerId) -> Vec<PlayerId> {
    if has_shared_poison(state, player) {
        team_members(state, player)
    } else if super::players::is_alive(state, player) {
        vec![player]
    } else {
        Vec::new()
    }
}

/// Dedup key for shared-life aggregation ([`aggregate_over_teams`]). A side
/// that shares life pools under its team; an individual seat pools under
/// itself. The two namespaces are deliberately distinct variants rather than
/// one shared `TeamId` space: under one-vs-many the survivor side IS
/// `TeamId(1)` while a Horde seat anchored at seat 1 would also key as
/// `TeamId(1)`, collapsing the Horde's pool into the survivors' (Repay in
/// Kind would read the survivors' 40 as the minimum and raise the bosses
/// instead of setting everyone to 5). The same structural separation keeps
/// the Archenemy Commander archenemy (CR 904.13b, an individual seat) out of
/// the heroes' shared-side pool with no topology-specific carve-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SharedLifeKey {
    SharedSide(TeamId),
    IndividualSeat(PlayerId),
}

pub(crate) fn shared_life_dedup_key(state: &GameState, player: PlayerId) -> SharedLifeKey {
    if has_shared_life(state, player) {
        SharedLifeKey::SharedSide(team_id(state, player))
    } else {
        SharedLifeKey::IndividualSeat(player)
    }
}

pub(crate) fn apnap_choice_groups(state: &GameState) -> Vec<Vec<PlayerId>> {
    apnap_choice_groups_from(state, state.active_player)
}

pub(crate) fn apnap_choice_groups_from(
    state: &GameState,
    start_player: PlayerId,
) -> Vec<Vec<PlayerId>> {
    apnap_choice_groups_admitting(state, start_player, SeatAdmission::Living)
}

fn apnap_choice_groups_admitting(
    state: &GameState,
    start_player: PlayerId,
    admission: SeatAdmission,
) -> Vec<Vec<PlayerId>> {
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
        return Vec::new();
    }

    // CR 101.4 + CR 103.1: APNAP follows the current turn-order direction. The
    // all-individual case returns early (one group per admitted seat); below, a
    // shared-team side chooses as ONE group (its admitted members), deduped so
    // it appears once, while a seat on an individual-turn side is its own group.
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
                admission
                    .admits(state, candidate)
                    .then_some(vec![candidate])
            })
            .collect();
    }

    let start_idx = seat_order
        .iter()
        .position(|&id| id == start_player)
        .unwrap_or(0);
    let mut seen_shared = std::collections::BTreeSet::new();
    let mut groups = Vec::new();
    for offset in 0..len {
        let idx = super::players::turn_order_index(start_idx, offset, len, state.turn_direction);
        let candidate = seat_order[idx];
        if !admission.admits(state, candidate) {
            continue;
        }
        if side_takes_shared_turn(state, candidate) {
            // Dedup the shared side by its stable side key so it contributes one
            // group at its first-appearing member's position.
            if seen_shared.insert(team_dedup_key(state, candidate)) {
                groups.push(team_members_admitting(state, candidate, admission));
            }
        } else {
            groups.push(vec![candidate]);
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

/// APNAP order from `start_player` over the given seat admission, following
/// the current turn-order direction (CR 101.4 + CR 103.1) and, under the
/// shared team turns option, team order (CR 805.6).
pub(crate) fn apnap_order_admitting(
    state: &GameState,
    start_player: PlayerId,
    admission: SeatAdmission,
) -> Vec<PlayerId> {
    apnap_choice_groups_admitting(state, start_player, admission)
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
    if !side_takes_shared_turn(state, player) {
        return player;
    }

    // LOTR "Two Towers" (Reading B): the Horde side's representative is its ACTIVE
    // commander — the current `active_player` during a Horde turn, else the
    // scheduled next commander (which rotates each Horde turn). Every Horde-side
    // member normalizes to it, so the turn recipient, priority, and APNAP all agree
    // on which commander is driving the Horde. The survivor side (and any other
    // shared side) keeps the first-living-member rule.
    if two_horde_seats(state).is_some_and(|h| h.contains(&player)) {
        if let Some(active) = active_horde_seat_now_or_next(state) {
            return active;
        }
    }

    team_members(state, player)
        .into_iter()
        .next()
        .unwrap_or(player)
}

/// The Horde side's active commander for turn/priority purposes in a two-Horde
/// (LOTR "Two Towers") game: the current `active_player` when it is a Horde seat
/// (the Horde's own turn), otherwise the commander scheduled to drive the next
/// Horde turn. `None` outside a two-Horde game.
fn active_horde_seat_now_or_next(state: &GameState) -> Option<PlayerId> {
    let hordes = two_horde_seats(state)?;
    if hordes.contains(&state.active_player) {
        return Some(state.active_player);
    }
    scheduled_active_horde(state)
}

/// The Horde commander scheduled to drive the NEXT Horde turn — Reading B has the
/// two commanders alternate, so this is `horde_seats[active_horde_index % living]`
/// in seat order (advanced once per Horde turn in `horde::begin_wave`). `None`
/// outside a two-Horde game. Horde seats have no life total so all stay alive; the
/// alive filter keeps it robust regardless.
fn scheduled_active_horde(state: &GameState) -> Option<PlayerId> {
    let living: Vec<PlayerId> = two_horde_seats(state)?
        .iter()
        .copied()
        .filter(|&id| super::players::is_alive(state, id))
        .collect();
    (!living.is_empty()).then(|| living[state.active_horde_index % living.len()])
}

/// CR 117.6 + CR 805.5b: In shared-team-turn multiplayer games, teams rather
/// than individual players have priority; when no player on a team acts, that
/// team passes.
pub(crate) fn priority_pass_representative(state: &GameState, player: PlayerId) -> PlayerId {
    // `normalize_shared_turn_recipient` already returns `player` unchanged when
    // their side takes individual turns, so this is correct per-side.
    normalize_shared_turn_recipient(state, player)
}

/// Canonicalize a set of semantic priority seats to their currently living
/// representatives (CR 117.6 + CR 805.5b).
///
/// Resolution sessions store representatives, never incidental teammate seats.
/// Reusing this at construction and at each live priority beat makes a changed
/// team topology observable before an automated pass can consume a stack entry.
pub(crate) fn canonical_priority_representatives<I>(
    state: &GameState,
    players: I,
) -> BTreeSet<PlayerId>
where
    I: IntoIterator<Item = PlayerId>,
{
    players
        .into_iter()
        .map(|player| priority_pass_representative(state, player))
        .filter(|&player| super::players::is_alive(state, player))
        .collect()
}

/// CR 805.4: In shared-team-turn formats, each team takes turns rather than
/// each player.
pub(crate) fn next_turn_representative(state: &GameState, current: PlayerId) -> PlayerId {
    // Individual-turn side: the next turn is simply the next living seat in turn
    // order (CR 103.1), normalized in case that seat belongs to a shared side — a
    // mixed game, e.g. the last individual Horde turn handing back to the shared
    // survivor side. For today's uniform individual formats the normalize is a
    // no-op, preserving the plain `next_player_in_turn_order` behavior.
    if !side_takes_shared_turn(state, current) {
        return normalize_shared_turn_recipient(
            state,
            super::players::next_player_in_turn_order(state, current),
        );
    }

    // Shared side (CR 805.4): the whole side took ONE turn, so skip the rest of it
    // and hand the next turn to the next living seat on a DIFFERENT side (its
    // representative).
    let seat_order = &state.seat_order;
    let len = seat_order.len();
    if len == 0 {
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
    // Map every APNAP player to its per-side priority representative (self for an
    // individual seat, the side's representative for a shared side) and dedup. For
    // uniform-individual formats every player is its own rep, so this returns the
    // plain APNAP order; for shared/mixed games it collapses each shared side to
    // one representative.
    super::players::apnap_order(state)
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
        assert_eq!(
            canonical_priority_representatives(&state, [PlayerId(0), PlayerId(1)]),
            [PlayerId(1)].into_iter().collect(),
            "a frozen session re-canonicalizes an eliminated teammate to its living team seat"
        );
    }

    #[test]
    fn two_hg_commander_shares_resources_and_seats_two_teams_of_two() {
        let state = GameState::new(FormatConfig::two_headed_giant_commander(), 4, 42);

        // CR 810.4 / CR 810.8 / CR 810.9 / CR 810.10 via the 2HG family.
        assert!(has_two_headed_giant_shared_resources(&state));
        assert_eq!(
            team_members(&state, PlayerId(0)),
            vec![PlayerId(0), PlayerId(1)]
        );
        assert_eq!(
            team_members(&state, PlayerId(2)),
            vec![PlayerId(2), PlayerId(3)]
        );
        assert_eq!(teammates(&state, PlayerId(0)), vec![PlayerId(1)]);
        assert_eq!(teammates(&state, PlayerId(3)), vec![PlayerId(2)]);
        // 60 shared per team, 30 per seat.
        assert_eq!(state.players[0].life, 30);
        assert_eq!(
            crate::game::players::team_life_total(&state, PlayerId(0)),
            60
        );
        assert_eq!(
            crate::game::players::team_life_total(&state, PlayerId(3)),
            60
        );
    }

    #[test]
    fn archenemy_commander_shares_life_but_not_poison() {
        let state = GameState::new(FormatConfig::archenemy_commander(), 4, 42);

        // CR 904.13b: heroes share life; the archenemy does not.
        assert!(!has_shared_life(&state, PlayerId(0)));
        assert!(has_shared_life(&state, PlayerId(1)));
        assert!(has_shared_life(&state, PlayerId(3)));
        // CR 904.13c: nobody shares poison.
        for seat in 0..4 {
            assert!(!has_shared_poison(&state, PlayerId(seat)));
        }
        assert_eq!(
            shared_life_members(&state, PlayerId(1)),
            vec![PlayerId(1), PlayerId(2), PlayerId(3)]
        );
        assert_eq!(
            shared_poison_members(&state, PlayerId(1)),
            vec![PlayerId(1)]
        );
        // 60 for the archenemy, 20 per hero seat at 4 seats, team 60.
        assert_eq!(state.players[0].life, 60);
        assert_eq!(state.players[1].life, 20);
        assert_eq!(
            crate::game::players::team_life_total(&state, PlayerId(1)),
            60
        );
        assert_eq!(
            crate::game::players::team_life_total(&state, PlayerId(0)),
            60
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

    /// `sides()` reproduces the historical grouping AND per-side turn structure
    /// for each `FormatTopology` shape — what makes the refactor behavior-preserving.
    #[test]
    fn sides_reproduce_the_topology_groupings() {
        let seats = |state: &GameState| -> Vec<Vec<PlayerId>> {
            sides(state).into_iter().map(|s| s.seats).collect()
        };
        let turn_kinds = |state: &GameState| -> Vec<TurnStructure> {
            sides(state).into_iter().map(|s| s.turn_structure).collect()
        };

        // Free-for-all: one side per seat, ordered by id, each individual-turn.
        let ffa = GameState::new(FormatConfig::free_for_all(), 4, 42);
        assert_eq!(
            seats(&ffa),
            vec![
                vec![PlayerId(0)],
                vec![PlayerId(1)],
                vec![PlayerId(2)],
                vec![PlayerId(3)],
            ]
        );
        assert!(turn_kinds(&ffa)
            .iter()
            .all(|t| *t == TurnStructure::IndividualTurns));

        // Two-Headed Giant: two teams of two (by id), each shared-turn.
        let thg = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        assert_eq!(
            seats(&thg),
            vec![
                vec![PlayerId(0), PlayerId(1)],
                vec![PlayerId(2), PlayerId(3)],
            ]
        );
        assert!(turn_kinds(&thg)
            .iter()
            .all(|t| *t == TurnStructure::SharedTeamTurns));

        // Archenemy: the archenemy side first, then the heroes; both shared-turn
        // (CR 904.2 uses the shared-team-turns option).
        let arch = GameState::new(FormatConfig::archenemy(), 4, 42);
        let s = sides(&arch);
        assert_eq!(s[0].seats, vec![PlayerId(0)], "archenemy side is first");
        let mut heroes = s[1].seats.clone();
        heroes.sort_by_key(|id| id.0);
        assert_eq!(heroes, vec![PlayerId(1), PlayerId(2), PlayerId(3)]);
        assert!(turn_kinds(&arch)
            .iter()
            .all(|t| *t == TurnStructure::SharedTeamTurns));
    }

    /// The O(1) `team_id` equality key must never diverge from the full `sides()`
    /// grouping: `team_id(player)` equals the index of the player's side.
    #[test]
    fn team_id_matches_side_index() {
        for state in [
            GameState::new(FormatConfig::free_for_all(), 4, 42),
            GameState::new(FormatConfig::two_headed_giant(), 4, 42),
            GameState::new(FormatConfig::archenemy(), 6, 42),
        ] {
            let groups = sides(&state);
            for p in &state.players {
                let idx = groups
                    .iter()
                    .position(|g| g.seats.contains(&p.id))
                    .expect("every seat belongs to a side");
                assert_eq!(
                    team_id(&state, p.id),
                    TeamId(idx as u8),
                    "team_id must equal the sides() index for {:?} in {:?}",
                    p.id,
                    state.format_config.format
                );
            }
        }
    }

    /// A two-Horde-force game (LOTR Two Towers): seats 0 and 1 are the allied
    /// Horde seats, seats 2 and 3 the survivors. `seat_order` is pinned explicitly
    /// so the rotation assertions don't depend on construction details.
    fn two_horde_state() -> GameState {
        use crate::types::format::ChallengeDeck;
        let mut state = GameState::new(
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            4,
            42,
        );
        state.horde_seats = vec![PlayerId(0), PlayerId(1)];
        state.seat_order = vec![PlayerId(0), PlayerId(1), PlayerId(2), PlayerId(3)];
        // A survivor is the active player (their setup turn) — the common case for
        // "survivor turn hands to the scheduled Horde commander". Tests that need a
        // specific active player override this.
        state.active_player = PlayerId(2);
        state
    }

    /// The two-side shape no `FormatTopology` variant can express: survivors are
    /// one shared-turn side, both Horde seats a single allied shared-turn side
    /// (2HG-shaped — survivors and the Horde alternate turns; the acting commander
    /// alternates each Horde turn, a runtime layer atop this grouping).
    #[test]
    fn two_horde_game_splits_survivors_and_allied_hordes_into_sides() {
        let state = two_horde_state();
        let s = sides(&state);
        assert_eq!(s.len(), 2, "survivors + Horde side");

        assert_eq!(
            s[0].seats,
            vec![PlayerId(2), PlayerId(3)],
            "survivors first"
        );
        assert_eq!(s[0].turn_structure, TurnStructure::SharedTeamTurns);

        assert_eq!(
            s[1].seats,
            vec![PlayerId(0), PlayerId(1)],
            "both Horde seats form one side"
        );
        assert_eq!(
            s[1].turn_structure,
            TurnStructure::SharedTeamTurns,
            "the Horde side takes one turn per round (2HG-style)"
        );

        // The O(1) `team_id` key cannot diverge from the mixed `sides()` grouping.
        for p in &state.players {
            let idx = s.iter().position(|g| g.seats.contains(&p.id)).unwrap();
            assert_eq!(team_id(&state, p.id), TeamId(idx as u8));
        }
    }

    /// The two Horde seats are allies (they never attack each other and share a
    /// collective loss); each opposes every survivor.
    #[test]
    fn two_horde_seats_are_allies_survivors_are_their_opponents() {
        let state = two_horde_state();
        assert!(
            !is_opponent(&state, PlayerId(0), PlayerId(1)),
            "the two Hordes are allied"
        );
        assert!(
            !is_opponent(&state, PlayerId(2), PlayerId(3)),
            "the survivors are allied"
        );
        assert!(is_opponent(&state, PlayerId(0), PlayerId(2)));
        assert!(is_opponent(&state, PlayerId(1), PlayerId(3)));
        assert_eq!(
            teammates(&state, PlayerId(0)),
            vec![PlayerId(1)],
            "a Horde seat's only ally is the other Horde seat"
        );
    }

    /// Turn rotation is 2HG-shaped: the survivors and the Horde side alternate,
    /// each taking ONE shared turn. (The acting Horde commander alternates each
    /// Horde turn — a runtime layer atop this; here the Horde rep is the first
    /// living Horde seat.)
    #[test]
    fn two_horde_rotation_alternates_survivor_and_horde_sides() {
        let state = two_horde_state();
        // The Horde side's turn hands off to the survivors' one shared turn.
        assert_eq!(next_turn_representative(&state, PlayerId(0)), PlayerId(2));
        // The survivors' shared turn hands back to the Horde side (its rep); the
        // other survivor does NOT get a separate turn.
        assert_eq!(next_turn_representative(&state, PlayerId(2)), PlayerId(0));
    }

    /// Priority: both allied Horde seats collapse to ONE representative and the
    /// survivor side to one — the two 2HG-style sides.
    #[test]
    fn two_horde_priority_participants_collapse_each_side() {
        let mut state = two_horde_state();
        state.active_player = PlayerId(0);
        assert_eq!(
            priority_pass_participants(&state),
            vec![PlayerId(0), PlayerId(2)]
        );
    }

    /// A real two-Horde config (a Horde ruleset with a co-horde deck) — the way a
    /// LOTR "Two Towers" game is built — makes `GameState::new` designate the first
    /// N=`horde_decks().len()` seats as the Horde, populate `state.horde_seats`,
    /// give every Horde seat no life total, and engage the two-Horde topology.
    #[test]
    fn two_horde_config_designates_both_seats_as_hordes() {
        use crate::types::format::ChallengeDeck;
        let mut ruleset = ChallengeDeck::SauronHorde.default_ruleset();
        ruleset.co_horde_decks = vec![ChallengeDeck::SarumanHorde];
        let state = GameState::new(FormatConfig::horde(ruleset), 4, 42);

        assert_eq!(
            state.horde_seats,
            vec![PlayerId(0), PlayerId(1)],
            "the first two seats are Sauron + Saruman"
        );
        assert!(state.players[0].has_no_life_total);
        assert!(state.players[1].has_no_life_total);
        assert!(!state.players[2].has_no_life_total, "survivor keeps life");
        assert!(!state.players[3].has_no_life_total, "survivor keeps life");

        // The two-Horde two-side topology engages (both sides shared-turn).
        let s = sides(&state);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].seats, vec![PlayerId(0), PlayerId(1)]);
        assert_eq!(s[1].turn_structure, TurnStructure::SharedTeamTurns);
    }

    /// Reading B: the survivors' shared turn hands to the SCHEDULED Horde commander,
    /// which alternates with `active_horde_index` (advanced each Horde turn by
    /// `horde::begin_wave`) — Sauron (seat 0), then Saruman (seat 1), wrapping.
    #[test]
    fn two_horde_active_commander_alternates_with_the_cursor() {
        let mut state = two_horde_state();

        state.active_horde_index = 0;
        assert_eq!(
            next_turn_representative(&state, PlayerId(2)),
            PlayerId(0),
            "cursor 0 → first commander (Sauron)"
        );
        state.active_horde_index = 1;
        assert_eq!(
            next_turn_representative(&state, PlayerId(2)),
            PlayerId(1),
            "cursor 1 → the other commander (Saruman)"
        );
        state.active_horde_index = 2;
        assert_eq!(
            next_turn_representative(&state, PlayerId(2)),
            PlayerId(0),
            "wraps mod the seat count"
        );

        // During a commander's OWN turn the Horde side normalizes to THAT seat (the
        // active_player), regardless of where the cursor points next — so priority
        // and the turn recipient track the acting commander.
        state.active_horde_index = 1; // cursor points at Saruman for next time…
        state.active_player = PlayerId(0); // …but it is Sauron's turn right now.
        assert_eq!(
            normalize_shared_turn_recipient(&state, PlayerId(1)),
            PlayerId(0),
            "both Horde seats normalize to the acting commander (Sauron)"
        );
    }

    /// `has_shared_life` / `has_shared_poison` are per-side: the survivor side
    /// pools (2HG-style) while each Horde seat does not — the two allied Horde
    /// seats must NOT share a life pool with each other (the vestigial-pool bug
    /// a whole-game predicate had).
    #[test]
    fn two_horde_survivors_pool_life_hordes_do_not() {
        let state = two_horde_state();
        assert!(has_shared_life(&state, PlayerId(2)), "survivor pools");
        assert!(has_shared_life(&state, PlayerId(3)), "survivor pools");
        assert!(!has_shared_life(&state, PlayerId(0)), "Horde seat does not");
        assert!(!has_shared_life(&state, PlayerId(1)), "Horde seat does not");
        // Poison rides with the survivors' shared life; the Horde seats stay
        // out of the poison pool too.
        assert!(has_shared_poison(&state, PlayerId(2)), "survivor pools");
        assert!(has_shared_poison(&state, PlayerId(3)), "survivor pools");
        assert!(
            !has_shared_poison(&state, PlayerId(0)),
            "Horde seat does not"
        );
        assert!(
            !has_shared_poison(&state, PlayerId(1)),
            "Horde seat does not"
        );

        // The resource-aggregation members follow suit: a Horde seat aggregates
        // over itself alone (not both Hordes), a survivor over the survivor team.
        assert_eq!(shared_life_members(&state, PlayerId(0)), vec![PlayerId(0)]);
        let mut survivor_pool = shared_life_members(&state, PlayerId(2));
        survivor_pool.sort_by_key(|id| id.0);
        assert_eq!(survivor_pool, vec![PlayerId(2), PlayerId(3)]);
    }

    /// `has_shared_life` across the uniform formats: 2HG pools, Archenemy heroes
    /// stay independent (CR 904.5), free-for-all never pools.
    #[test]
    fn has_shared_life_matches_each_uniform_format() {
        let thg = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        assert!(thg.players.iter().all(|p| has_shared_life(&thg, p.id)));

        let arch = GameState::new(FormatConfig::archenemy(), 4, 42);
        assert!(
            arch.players.iter().all(|p| !has_shared_life(&arch, p.id)),
            "Archenemy heroes each keep their own 20 (CR 904.5)"
        );

        let ffa = GameState::new(FormatConfig::free_for_all(), 4, 42);
        assert!(ffa.players.iter().all(|p| !has_shared_life(&ffa, p.id)));
    }
}
