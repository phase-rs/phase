use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use engine::types::player::PlayerId;

use crate::pack_source::PackSource;
use crate::pick_pass;
use crate::types::*;
use crate::validation::{validate_limited_deck, LimitedDeckError};
// Deep-path import by design: `engine::game::mod` re-exports `deck_validation`'s
// public surface, but this phase must not edit that file.
use engine::game::deck_validation::{draft_set_concessions_for, DraftSetConcessions};

impl DraftSession {
    /// The round that pairings may next be generated for.
    ///
    /// Single authority. `AdvanceRound` deliberately leaves `current_round`
    /// untouched — it only opens the `Pairing` window — so `current_round` is
    /// always the last round whose pairings exist, and generating pairings is
    /// what commits the next one. Crate-internal on purpose: `view.rs`
    /// publishes its answer on `DraftPlayerView` so clients read it rather than
    /// re-deriving it; callers outside this crate must never recompute it.
    pub(crate) fn next_pairing_round(&self) -> u8 {
        self.current_round + 1
    }

    /// Create a new draft session in Lobby status.
    ///
    /// Timestamps are set to 0 -- callers set them externally since the pure
    /// reducer does not call the system clock.
    pub fn new(config: DraftConfig, seats: Vec<DraftSeat>, draft_code: String) -> Self {
        let pod_size = seats.len();
        DraftSession {
            set_code: config.set_code.clone(),
            kind: config.kind,
            status: DraftStatus::Lobby,
            pass_direction: PassDirection::for_pack(0),
            current_pack_number: 0,
            pack_sizes: Vec::new(),
            pick_number: 0,
            seats_picked_this_round: SeatFlags::all_false(pod_size as u8),
            connected_seats: SeatFlags::all_true(pod_size as u8),
            packs_by_seat: vec![vec![]; pod_size],
            current_pack: vec![None; pod_size],
            current_pack_origins: vec![None; pod_size],
            pools: vec![vec![]; pod_size],
            submitted_decks: HashMap::new(),
            match_records: HashMap::new(),
            pairings: Vec::new(),
            current_round: 0,
            config,
            seats,
            draft_code,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// Cards booster `pack_number` held when it was opened.
    ///
    /// The single read path for pack shape. Uses the sizes recorded at
    /// `StartDraft` when present, and falls back to the uniform
    /// `config.cards_per_pack` for snapshots written before per-pack sizes
    /// existed (and for sessions still in Lobby, which have opened nothing).
    pub fn cards_in_pack(&self, pack_number: u8) -> u8 {
        entry_for_pack(&self.pack_sizes, pack_number)
            .copied()
            .unwrap_or(self.config.cards_per_pack)
    }

    /// Cards a single seat opens across every booster of the session.
    pub fn total_pack_cards(&self) -> usize {
        if self.pack_sizes.is_empty() {
            return usize::from(self.config.pack_count) * usize::from(self.config.cards_per_pack);
        }
        self.pack_sizes.iter().copied().map(usize::from).sum()
    }

    /// Booster sizes for every pack of the session, in pack order. Derived so
    /// clients render progress without reconstructing pack shape themselves.
    pub fn pack_size_sequence(&self) -> Vec<u8> {
        (0..self.config.pack_count)
            .map(|pack| self.cards_in_pack(pack))
            .collect()
    }

    /// The set filling seat 0's boosters, in pack order.
    ///
    /// Uniform layouts assign the same set to every seat. Chaos has a
    /// seat-specific assignment, so callers that need another seat must use
    /// [`DraftSource::set_code_for_seat_and_pack`] rather than inferring it
    /// from this compatibility sequence.
    pub fn pack_set_code_sequence(&self) -> Vec<String> {
        (0..self.config.pack_count)
            .map(|pack| self.config.source.set_code_for_pack(pack))
            .collect()
    }

    /// CR 903.13b: pick STEPS in every booster of the session, in pack order.
    ///
    /// The per-pack counterpart of [`DraftProcedure::pick_steps_per_pack`].
    /// `pick_number` counts steps, not cards, so a progress display measures
    /// each booster against this rather than against
    /// [`Self::pack_size_sequence`] — a 14-card Commander pack is 7 steps. Both
    /// axes vary independently: multi-set drafts differ in cards per pack, and
    /// the kind's procedure decides how many cards one step takes.
    pub fn pack_pick_step_sequence(&self) -> Vec<u8> {
        let procedure = self.kind.procedure();
        (0..self.config.pack_count)
            .map(|pack| procedure.pick_steps_per_pack(self.cards_in_pack(pack)))
            .collect()
    }

    /// Validate the procedure and source invariants of a persisted draft event.
    ///
    /// This is intentionally an import/restore boundary check. Reducer-created
    /// sessions establish these properties at start and legacy `SeatFlags`
    /// snapshots remain compatible because their bitmap lengths are unrelated.
    pub fn validate_persisted_snapshot(&self) -> Result<(), DraftError> {
        if self.kind != self.config.kind {
            return Err(DraftError::InvalidSealedSnapshot {
                reason: "session kind does not match configuration".to_string(),
            });
        }
        if let DraftSource::Set { layout } = &self.config.source {
            layout
                .validate_for_draft(self.seats.len() as u8, self.config.pack_count)
                .map_err(|reason| DraftError::InvalidPackSequence { reason })?;
        }
        let procedure = self.kind.procedure();
        let pod_size = self.seats.len() as u8;
        let local_cube_size_is_allowed = matches!(self.config.source, DraftSource::Cube { .. })
            && procedure.allows_local_cube_pod_size(self.config.tournament_format, pod_size);
        if !procedure.allows_pod_size(self.config.tournament_format, pod_size)
            && !local_cube_size_is_allowed
        {
            return Err(DraftError::InvalidSealedSnapshot {
                reason: "pod size does not satisfy the draft procedure".to_string(),
            });
        }
        match procedure.distribution {
            PackDistribution::PickAndPass => return Ok(()),
            PackDistribution::AllAtOnce => {}
        }
        if !matches!(self.config.source, DraftSource::Set { .. }) {
            return Err(DraftError::SealedRequiresSetSource);
        }
        if self.config.source.set_code() != self.config.set_code
            || self.set_code != self.config.set_code
        {
            return Err(DraftError::InvalidSealedSnapshot {
                reason: "set source and session codes must match".to_string(),
            });
        }
        if self.config.pack_count != procedure.packs_per_player
            || self.config.min_deck_size != procedure.min_deck_size
        {
            return Err(DraftError::InvalidSealedSnapshot {
                reason: "sealed requires six packs and a 40-card minimum deck".to_string(),
            });
        }
        if self.status == DraftStatus::Drafting {
            return Err(DraftError::InvalidSealedSnapshot {
                reason: "sealed sessions cannot be in drafting status".to_string(),
            });
        }
        let seat_count = self.seats.len();
        if self.config.pod_size as usize != seat_count
            || self.pools.len() != seat_count
            || self.current_pack.len() != seat_count
            || self.packs_by_seat.len() != seat_count
        {
            return Err(DraftError::InvalidSealedSnapshot {
                reason: "per-seat vectors do not match the core seats".to_string(),
            });
        }
        let expected_pool_size = self.total_pack_cards();
        if self.status != DraftStatus::Lobby
            && self
                .pools
                .iter()
                .any(|pool| pool.len() != expected_pool_size)
        {
            return Err(DraftError::InvalidSealedSnapshot {
                reason:
                    "started sealed pools must contain exactly one card from each configured pack"
                        .to_string(),
            });
        }
        if self.status != DraftStatus::Lobby
            && (self.current_pack.iter().any(Option::is_some)
                || self.packs_by_seat.iter().any(|packs| !packs.is_empty()))
        {
            return Err(DraftError::InvalidSealedSnapshot {
                reason: "sealed packs must be retained only in player pools".to_string(),
            });
        }
        Ok(())
    }
}

/// Apply a draft action to the session, returning deltas or an error.
///
/// This is the main reducer: `apply(session, action) -> Result<Vec<DraftDelta>, DraftError>`.
/// A single action can produce multiple deltas (e.g., pick + pass + pack exhaustion + transition).
pub fn apply(
    session: &mut DraftSession,
    action: DraftAction,
    pack_source: Option<&dyn PackSource>,
) -> Result<Vec<DraftDelta>, DraftError> {
    match action {
        DraftAction::StartDraft => apply_start_draft(session, pack_source),
        DraftAction::Pick {
            seat,
            card_instance_ids,
        } => pick_pass::apply_pick(session, seat, card_instance_ids),
        DraftAction::PickWithDraftEffect {
            seat,
            effect_card_instance_id,
            card_instance_ids,
        } => pick_pass::apply_pick_with_draft_effect(
            session,
            seat,
            effect_card_instance_id,
            card_instance_ids,
        ),
        DraftAction::SubmitDeck {
            seat,
            main_deck,
            commanders,
        } => apply_submit_deck(session, seat, main_deck, commanders),
        DraftAction::GeneratePairings => apply_generate_pairings(session),
        DraftAction::ReportMatchResult {
            match_id,
            winner_seat,
        } => apply_report_match_result(session, match_id, winner_seat),
        DraftAction::AdvanceRound => apply_advance_round(session),
        DraftAction::ReplaceSeatWithBot { seat, name } => {
            apply_replace_seat_with_bot(session, seat, name)
        }
        DraftAction::SetSeatConnected { seat, connected } => {
            apply_set_seat_connected(session, seat, connected)
        }
    }
}

/// Map seat index to PlayerId.
fn seat_player_id(session: &DraftSession, seat: u8) -> PlayerId {
    match &session.seats[seat as usize] {
        DraftSeat::Human { player_id, .. } => *player_id,
        DraftSeat::Bot { .. } => PlayerId(seat),
    }
}

/// Ensure a match record exists for the player, returning a mutable reference.
fn ensure_match_record(
    records: &mut HashMap<PlayerId, DraftMatchRecord>,
    player: PlayerId,
) -> &mut DraftMatchRecord {
    records.entry(player).or_insert(DraftMatchRecord {
        player,
        wins: 0,
        losses: 0,
        draws: 0,
        match_wins: 0,
        match_losses: 0,
    })
}

/// Swiss round count for an 8-player pod.
const SWISS_ROUNDS: u8 = 3;

fn apply_generate_pairings(session: &mut DraftSession) -> Result<Vec<DraftDelta>, DraftError> {
    // Guard: valid status for pairing generation
    let valid = matches!(
        session.status,
        DraftStatus::Deckbuilding | DraftStatus::Pairing | DraftStatus::RoundComplete
    );
    if !valid {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "GeneratePairings".to_string(),
        });
    }
    let procedure = session.kind.procedure();
    if procedure.post_draft_play != PostDraftPlay::TournamentPairings {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "GeneratePairings".to_string(),
        });
    }
    if !procedure.allows_pod_size(session.config.tournament_format, session.seats.len() as u8) {
        return Err(DraftError::UnsupportedTournamentSize {
            format: session.config.tournament_format,
            required: *procedure
                .allowed_pod_size_range(session.config.tournament_format)
                .start(),
            actual: session.seats.len() as u8,
        });
    }
    // Single authority. The round is derived, never supplied, so the old
    // `round != current_round + 1` guard is unreachable and is gone.
    let round = session.next_pairing_round();

    let mut rng =
        ChaCha20Rng::seed_from_u64(session.config.rng_seed ^ (round as u64 * 0xDEAD_BEEF));

    let (new_pairings, swiss_bye) = match session.config.tournament_format {
        TournamentFormat::Swiss => generate_swiss_pairings(session, round, &mut rng),
        TournamentFormat::SingleElimination => (generate_se_pairings(session, round), None),
    };

    for p in &new_pairings {
        session.pairings.push(p.clone());
    }
    // A Swiss bye counts as a match win for the unpaired player; without this credit an
    // odd-pod bye scores nothing and Swiss standings (sorted by match_wins) are wrong.
    if let Some(bye) = swiss_bye {
        ensure_match_record(&mut session.match_records, bye).match_wins += 1;
    }
    session.status = DraftStatus::MatchInProgress;
    session.current_round = round;

    Ok(vec![
        DraftDelta::PairingsGenerated { round },
        DraftDelta::TransitionedTo {
            status: DraftStatus::MatchInProgress,
        },
    ])
}

/// Backtracking search for a perfect matching of `pool` in standings order.
///
/// The first unpaired player tries partners in pool order (same-bracket
/// first, since the pool is flattened bracket-by-bracket); a dead end
/// backtracks instead of settling for a rematch. With `allow_rematch: false`
/// it succeeds iff a rematch-free perfect matching exists; with `true` it is
/// an always-succeeding first-fit (even pool). `prior` holds both
/// orientations of every earlier pairing. Recursion depth is bounded by the
/// pod size (≤ 8 seats → ≤ 4 frames), not by game input.
fn pair_pool_avoiding_rematches(
    pool: &[PlayerId],
    prior: &HashSet<(PlayerId, PlayerId)>,
    allow_rematch: bool,
    out: &mut Vec<(PlayerId, PlayerId)>,
) -> bool {
    let Some((&first, candidates)) = pool.split_first() else {
        return true;
    };
    for (i, &partner) in candidates.iter().enumerate() {
        if !allow_rematch && prior.contains(&(first, partner)) {
            continue;
        }
        let mut rest: Vec<PlayerId> = Vec::with_capacity(candidates.len() - 1);
        rest.extend_from_slice(&candidates[..i]);
        rest.extend_from_slice(&candidates[i + 1..]);
        out.push((first, partner));
        if pair_pool_avoiding_rematches(&rest, prior, allow_rematch, out) {
            return true;
        }
        out.pop();
    }
    false
}

/// Returns the round's pairings plus the bye player, if any. An odd pod leaves one player
/// unpaired; in Swiss that player takes a bye, which counts as a match win — the caller
/// credits it (`Some(pid)`). `None` when every player was paired.
fn generate_swiss_pairings(
    session: &DraftSession,
    round: u8,
    rng: &mut ChaCha20Rng,
) -> (Vec<DraftPairing>, Option<PlayerId>) {
    let seat_indices: Vec<u8> = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, _)| i as u8)
        .collect();

    // Build player IDs and their match records
    let mut players_with_wins: Vec<(PlayerId, u8, u8)> = seat_indices
        .iter()
        .map(|&seat| {
            let pid = seat_player_id(session, seat);
            let record = session.match_records.get(&pid);
            let wins = record.map_or(0, |r| r.match_wins);
            (pid, wins, seat)
        })
        .collect();

    // Sort by match_wins descending to form brackets
    players_with_wins.sort_by_key(|p| std::cmp::Reverse(p.1));

    // Group by win count
    let mut brackets: Vec<Vec<(PlayerId, u8)>> = Vec::new();
    let mut current_wins = None;
    for (pid, wins, seat) in &players_with_wins {
        if current_wins != Some(*wins) {
            brackets.push(Vec::new());
            current_wins = Some(*wins);
        }
        brackets.last_mut().unwrap().push((*pid, *seat));
    }

    // Shuffle within each bracket
    for bracket in &mut brackets {
        bracket.shuffle(rng);
    }

    // Collect all prior opponent pairs for rematch avoidance
    let prior_pairs: HashSet<(PlayerId, PlayerId)> = session
        .pairings
        .iter()
        .flat_map(|p| [(p.players[0], p.players[1]), (p.players[1], p.players[0])])
        .collect();

    // Flatten the shuffled brackets into standings order. Partners are tried
    // in this order, so same-bracket pairings are still preferred — but a
    // dead end now backtracks instead of accepting an avoidable rematch
    // (the old head-first greedy's `unwrap_or(0)` paired a 4-pod's round 3
    // as two rematches even though the round-robin completion existed).
    let pool: Vec<PlayerId> = brackets
        .iter()
        .flat_map(|bracket| bracket.iter().map(|(pid, _)| *pid))
        .collect();

    let mut paired: Vec<(PlayerId, PlayerId)> = Vec::new();
    let mut bye: Option<PlayerId> = None;

    if pool.len().is_multiple_of(2) {
        if !pair_pool_avoiding_rematches(&pool, &prior_pairs, false, &mut paired) {
            // No rematch-free perfect matching exists (e.g. a 2-player pod
            // from round 2 on) — admit rematches; first-fit always succeeds
            // on an even pool.
            paired.clear();
            pair_pool_avoiding_rematches(&pool, &prior_pairs, true, &mut paired);
        }
    } else {
        // Odd pod: the bye goes as far down the standings as a rematch-free
        // matching of the remainder allows. Only if no candidate admits one
        // does the bottom seat take the bye and the rest pair with rematches.
        // The bye is reported to the caller so it can be credited as a
        // match win.
        for idx in (0..pool.len()).rev() {
            let mut rest = pool.clone();
            let cand = rest.remove(idx);
            paired.clear();
            if pair_pool_avoiding_rematches(&rest, &prior_pairs, false, &mut paired) {
                bye = Some(cand);
                break;
            }
        }
        if bye.is_none() {
            let mut rest = pool.clone();
            let cand = rest.pop().expect("odd pool is non-empty");
            paired.clear();
            pair_pool_avoiding_rematches(&rest, &prior_pairs, true, &mut paired);
            bye = Some(cand);
        }
    }

    // Generate DraftPairing structs
    let pairings = paired
        .iter()
        .enumerate()
        .map(|(table, (p1, p2))| DraftPairing {
            round,
            table: table as u8,
            players: [*p1, *p2],
            match_id: format!("r{round}-t{table}"),
            status: PairingStatus::Pending,
            winner: None,
        })
        .collect();

    (pairings, bye)
}

fn generate_se_pairings(session: &DraftSession, round: u8) -> Vec<DraftPairing> {
    if round == 1 {
        // Standard seeded bracket: 0v7, 1v6, 2v5, 3v4
        let bracket_pairs: [(u8, u8); 4] = [(0, 7), (1, 6), (2, 5), (3, 4)];
        bracket_pairs
            .iter()
            .enumerate()
            .map(|(table, (a, b))| {
                let p1 = seat_player_id(session, *a);
                let p2 = seat_player_id(session, *b);
                DraftPairing {
                    round,
                    table: table as u8,
                    players: [p1, p2],
                    match_id: format!("r{round}-t{table}"),
                    status: PairingStatus::Pending,
                    winner: None,
                }
            })
            .collect()
    } else {
        // Pair winners of adjacent matches from the previous round
        let prev_round = round - 1;
        let prev_pairings: Vec<&DraftPairing> = session
            .pairings
            .iter()
            .filter(|p| p.round == prev_round && p.status == PairingStatus::Complete)
            .collect();

        let winners: Vec<PlayerId> = prev_pairings
            .iter()
            .filter_map(|p| p.result_winner(&session.match_records))
            .collect();

        // Pair adjacent winners
        winners
            .chunks(2)
            .enumerate()
            .filter_map(|(table, chunk)| {
                if chunk.len() == 2 {
                    Some(DraftPairing {
                        round,
                        table: table as u8,
                        players: [chunk[0], chunk[1]],
                        match_id: format!("r{round}-t{table}"),
                        status: PairingStatus::Pending,
                        winner: None,
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

fn apply_match_record_result(
    records: &mut HashMap<PlayerId, DraftMatchRecord>,
    players: [PlayerId; 2],
    winner: Option<PlayerId>,
) {
    match winner {
        Some(winner_pid) => {
            let loser_pid = if players[0] == winner_pid {
                players[1]
            } else {
                players[0]
            };
            ensure_match_record(records, winner_pid).match_wins += 1;
            ensure_match_record(records, winner_pid).wins += 1;
            ensure_match_record(records, loser_pid).match_losses += 1;
            ensure_match_record(records, loser_pid).losses += 1;
        }
        None => {
            for pid in players {
                ensure_match_record(records, pid).draws += 1;
            }
        }
    }
}

fn undo_match_record_result(
    records: &mut HashMap<PlayerId, DraftMatchRecord>,
    players: [PlayerId; 2],
    winner: Option<PlayerId>,
) {
    match winner {
        Some(winner_pid) => {
            let loser_pid = if players[0] == winner_pid {
                players[1]
            } else {
                players[0]
            };
            if let Some(record) = records.get_mut(&winner_pid) {
                record.match_wins = record.match_wins.saturating_sub(1);
                record.wins = record.wins.saturating_sub(1);
            }
            if let Some(record) = records.get_mut(&loser_pid) {
                record.match_losses = record.match_losses.saturating_sub(1);
                record.losses = record.losses.saturating_sub(1);
            }
        }
        None => {
            for pid in players {
                if let Some(record) = records.get_mut(&pid) {
                    record.draws = record.draws.saturating_sub(1);
                }
            }
        }
    }
}

fn apply_report_match_result(
    session: &mut DraftSession,
    match_id: String,
    winner_seat: Option<u8>,
) -> Result<Vec<DraftDelta>, DraftError> {
    if !matches!(
        session.status,
        DraftStatus::MatchInProgress | DraftStatus::RoundComplete
    ) {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "ReportMatchResult".to_string(),
        });
    }

    // Find and update the pairing
    let pairing_idx = session
        .pairings
        .iter()
        .position(|p| p.match_id == match_id)
        .ok_or_else(|| DraftError::PairingNotFound {
            match_id: match_id.clone(),
        })?;

    let pairing_round = session.pairings[pairing_idx].round;
    if pairing_round != session.current_round {
        return Err(DraftError::PairingNotInCurrentRound {
            match_id,
            current_round: session.current_round,
        });
    }

    if session.config.tournament_format == TournamentFormat::SingleElimination
        && winner_seat.is_none()
    {
        return Err(DraftError::MatchWinnerRequired { match_id });
    }

    let players = session.pairings[pairing_idx].players;
    let previous_status = session.pairings[pairing_idx].status;
    let previous_winner = session.pairings[pairing_idx].result_winner(&session.match_records);
    let winner_pid = match winner_seat {
        Some(winner) => {
            let pod_size = session.seats.len() as u8;
            if winner >= pod_size {
                return Err(DraftError::SeatOutOfRange {
                    seat: winner,
                    pod_size,
                });
            }
            let pid = seat_player_id(session, winner);
            if !players.contains(&pid) {
                return Err(DraftError::SeatNotInPairing {
                    seat: winner,
                    match_id,
                });
            }
            Some(pid)
        }
        None => None,
    };

    if previous_status == PairingStatus::Complete {
        undo_match_record_result(&mut session.match_records, players, previous_winner);
    }

    session.pairings[pairing_idx].status = PairingStatus::Complete;
    session.pairings[pairing_idx].winner = winner_pid;
    apply_match_record_result(&mut session.match_records, players, winner_pid);

    let mut deltas = vec![DraftDelta::MatchResultRecorded {
        match_id,
        winner_seat,
    }];

    // Check if all pairings for the current round are complete
    let current_round = session.current_round;
    let all_complete = session
        .pairings
        .iter()
        .filter(|p| p.round == current_round)
        .all(|p| p.status == PairingStatus::Complete);

    if all_complete {
        // Determine if tournament is over
        let tournament_over = match session.config.tournament_format {
            TournamentFormat::Swiss => current_round >= SWISS_ROUNDS,
            TournamentFormat::SingleElimination => {
                // SE is over when only 1 player remains (round 3 for 8 players)
                let round_pairings: Vec<_> = session
                    .pairings
                    .iter()
                    .filter(|p| p.round == current_round)
                    .collect();
                round_pairings.len() == 1 // Final match
            }
        };

        if tournament_over {
            session.status = DraftStatus::Complete;
            deltas.push(DraftDelta::TransitionedTo {
                status: DraftStatus::Complete,
            });
        } else {
            session.status = DraftStatus::RoundComplete;
            deltas.push(DraftDelta::TransitionedTo {
                status: DraftStatus::RoundComplete,
            });
        }
    }

    Ok(deltas)
}

fn apply_advance_round(session: &mut DraftSession) -> Result<Vec<DraftDelta>, DraftError> {
    if session.status != DraftStatus::RoundComplete {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "AdvanceRound".to_string(),
        });
    }

    let new_round = session.next_pairing_round();
    session.status = DraftStatus::Pairing;

    Ok(vec![DraftDelta::RoundAdvanced { new_round }])
}

fn apply_replace_seat_with_bot(
    session: &mut DraftSession,
    seat: u8,
    name: Option<String>,
) -> Result<Vec<DraftDelta>, DraftError> {
    let pod_size = session.seats.len() as u8;
    if seat >= pod_size {
        return Err(DraftError::SeatOutOfRange { seat, pod_size });
    }

    session.seats[seat as usize] = DraftSeat::Bot {
        name: name.unwrap_or_else(|| format!("Seat {}", seat + 1)),
    };

    Ok(vec![DraftDelta::SeatReplacedWithBot { seat }])
}

/// Mark a human seat as connected or disconnected. The new flag becomes the
/// authoritative source for `DraftPlayerView.seats[*].connected` via
/// [`crate::view::filter_for_player`]. Bot seats reject — flipping a bot
/// connection bit is nonsensical (bots are always connected by construction).
fn apply_set_seat_connected(
    session: &mut DraftSession,
    seat: u8,
    connected: bool,
) -> Result<Vec<DraftDelta>, DraftError> {
    let pod_size = session.seats.len() as u8;
    if seat >= pod_size {
        return Err(DraftError::SeatOutOfRange { seat, pod_size });
    }
    if matches!(session.seats[seat as usize], DraftSeat::Bot { .. }) {
        return Err(DraftError::SeatIsBot { seat });
    }
    session.connected_seats.ensure_len(pod_size, true);
    session.connected_seats.set(seat, connected);
    Ok(vec![DraftDelta::SeatConnectionChanged { seat, connected }])
}

fn apply_start_draft(
    session: &mut DraftSession,
    pack_source: Option<&dyn PackSource>,
) -> Result<Vec<DraftDelta>, DraftError> {
    if session.status != DraftStatus::Lobby {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "StartDraft".to_string(),
        });
    }

    let seat_count = session.seats.len() as u8;
    let procedure = session.kind.procedure();

    if let DraftSource::Set { layout } = &session.config.source {
        layout
            .validate_for_draft(seat_count, session.config.pack_count)
            .map_err(|reason| DraftError::InvalidPackSequence { reason })?;
    }

    // CR 903.13a + CR 800.1: the smallest pod that can still deliver the
    // multiplayer game this kind is defined as. The procedure owns the full
    // allowed range and the pairing-only single-elimination exact size.
    let local_cube_size_is_allowed = matches!(session.config.source, DraftSource::Cube { .. })
        && procedure.allows_local_cube_pod_size(session.config.tournament_format, seat_count);
    if !procedure.allows_pod_size(session.config.tournament_format, seat_count)
        && !local_cube_size_is_allowed
    {
        if seat_count < procedure.min_pod_size {
            return Err(DraftError::PodBelowMinimumSize {
                kind: session.kind,
                required: procedure.min_pod_size,
                actual: seat_count,
            });
        }
        return Err(DraftError::UnsupportedTournamentSize {
            format: session.config.tournament_format,
            required: *procedure
                .allowed_pod_size_range(session.config.tournament_format)
                .start(),
            actual: seat_count,
        });
    }
    // The session's kind and its configuration's kind must agree on how packs
    // reach the seats; a mismatched pair is a corrupt configuration. Testing the
    // two distributions for disagreement is equivalent to the previous pair of
    // `== DraftKind::Sealed` tests *because* Sealed is currently the only
    // `AllAtOnce` kind, so "exactly one is Sealed" is "the distributions differ".
    // Stating the invariant once, rather than enumerating the pairs that violate
    // it, leaves the match below exhaustive over a single axis: a new
    // `PackDistribution` is an `E0004` here with exactly one arm to decide.
    if procedure.distribution != session.config.kind.procedure().distribution {
        return Err(DraftError::InvalidSealedConfiguration {
            reason: "session kind does not match configuration".to_string(),
        });
    }
    match procedure.distribution {
        PackDistribution::AllAtOnce => {
            if !matches!(session.config.source, DraftSource::Set { .. }) {
                return Err(DraftError::SealedRequiresSetSource);
            }
            // A multi-set source labels itself with its joined distinct codes,
            // so the session label is compared against `set_code()` rather than
            // against any single pack's set. Mirrors `validate_persisted_snapshot`.
            if session.config.source.set_code() != session.config.set_code
                || session.set_code != session.config.set_code
            {
                return Err(DraftError::InvalidSealedConfiguration {
                    reason: "set source and session codes must match".to_string(),
                });
            }
            if session.config.pack_count != procedure.packs_per_player
                || session.config.min_deck_size != procedure.min_deck_size
            {
                return Err(DraftError::InvalidSealedConfiguration {
                    reason: "sealed requires six packs and a 40-card minimum deck".to_string(),
                });
            }
        }
        PackDistribution::PickAndPass => {}
    }

    let pack_source = pack_source.expect("StartDraft requires a PackSource");
    let pod_size = seat_count;
    let mut rng = ChaCha20Rng::seed_from_u64(session.config.rng_seed);

    let all_packs = pack_source.generate_packs(&mut rng, &session.config, pod_size)?;

    // Record the shape of the boosters the source actually produced, in pack
    // order. Every seat opens the same set in the same pack round, so seat 0's
    // packs describe the session. Both distributions consume `all_packs` below,
    // and picking mutates the packs from there on, so this is the only moment
    // the original sizes are observable.
    session.pack_sizes = all_packs
        .first()
        .map(|seat_packs| {
            seat_packs
                .iter()
                .map(|pack| u8::try_from(pack.0.len()).unwrap_or(u8::MAX))
                .collect()
        })
        .unwrap_or_default();

    match procedure.distribution {
        // Every pack goes straight to its own seat; there is no pick step, so
        // the event opens directly in deckbuilding.
        PackDistribution::AllAtOnce => {
            let packs_per_seat = usize::from(procedure.packs_per_player);
            if all_packs.len() != session.seats.len()
                || all_packs.iter().any(|packs| packs.len() != packs_per_seat)
            {
                return Err(DraftError::InvalidSealedConfiguration {
                    reason: "pack source did not generate six packs per seat".to_string(),
                });
            }
            let pools = all_packs
                .into_iter()
                .map(|packs| packs.into_iter().flat_map(|pack| pack.0).collect())
                .collect();
            session.pools = pools;
            session.current_pack.fill(None);
            session.current_pack_origins.fill(None);
            session.packs_by_seat.iter_mut().for_each(Vec::clear);
            session.status = DraftStatus::Deckbuilding;
            return Ok(vec![
                DraftDelta::DraftStarted,
                DraftDelta::TransitionedTo {
                    status: DraftStatus::Deckbuilding,
                },
            ]);
        }
        PackDistribution::PickAndPass => {}
    }

    session
        .current_pack_origins
        .resize(usize::from(pod_size), None);
    for (seat, mut seat_packs) in all_packs.into_iter().enumerate() {
        // First pack goes to current_pack, rest go to packs_by_seat
        session.current_pack[seat] = Some(seat_packs.remove(0));
        session.current_pack_origins[seat] = Some(seat as u8);
        session.packs_by_seat[seat] = seat_packs;
    }

    session.status = DraftStatus::Drafting;
    session.pass_direction = PassDirection::for_pack(0);
    session.current_pack_number = 0;
    session.pick_number = 0;
    // Reset per-round pick tracking; `connected_seats` is left intact so any
    // pre-draft disconnects persist into the drafting phase.
    session.seats_picked_this_round = SeatFlags::all_false(pod_size);

    Ok(vec![DraftDelta::DraftStarted])
}

/// CR 903.13e + CR 903.13f(3): the deck-construction concessions this session's
/// booster sets make, LATCHED from `config.source` at session creation and
/// never re-derived from pool contents.
///
/// Pool contents are not evidence of the grant IN EITHER DIRECTION: the
/// CR 903.13e filler cards are themselves PRINTED in the granting sets' draft
/// boosters, so a drafted copy does not prove a grant, and a pool without one
/// does not disprove it. `config.source` carries the only authority
/// CR 903.13e names -- what the DRAFT CONTAINED.
///
/// `pub(crate)`, NOT private: `view.rs`'s two builders call it. It must not
/// become `pub` -- outside draft-core the concessions are consumed from the
/// published view field, never re-derived.
pub(crate) fn session_concessions(session: &DraftSession) -> DraftSetConcessions {
    draft_set_concessions_for(concession_set_codes(session))
}

/// CR 903.13e + CR 903.13f(3): every set whose draft boosters this session
/// CONTAINED, LATCHED from `config.source` at session creation. EMPTY when the
/// rules concede nothing -- a cube (which contains no draft boosters from any
/// set) and every kind outside CR 903.13's scope.
///
/// The single authority for "which sets did this draft contain". Both
/// `session_concessions` above and `view::filter_for_player`'s published
/// `draft_set_codes` read it, so the two can never disagree about a cube.
///
/// Both rules live in CR 903.13, which scopes them to Commander Draft, so every
/// other kind concedes nothing. Both `match`es are wildcard-free: a sixth
/// `DraftKind`, or a third `DraftSource`, must state its answer.
///
/// PLURAL, and that is the rules-correct shape rather than a convenience.
/// CR 903.13e/f condition each grant on whether "the draft contained draft
/// boosters from" a named set, so a multi-set Commander Draft satisfies each
/// named set's condition independently: a CMM+CLB draft concedes The Prismatic
/// Piper AND Faceless One, and grants the CR 903.13f(3) partner ability because
/// it contained Commander Masters boosters. Returning one representative code
/// would drop the other set's grant; returning none would drop both. The union
/// is taken by `draft_set_concessions_for`, which reads containment only -- so
/// pack ORDER, repetition and casing cannot move the answer.
///
/// Distinct, in first-appearance order: a set names its condition once however
/// many boosters it filled. This is a latched list of what the draft contained,
/// not a per-pack sequence -- `DraftSource::set_code_for_pack` owns that axis.
pub(crate) fn concession_set_codes(session: &DraftSession) -> Vec<&str> {
    match session.kind {
        DraftKind::CommanderDraft => match &session.config.source {
            DraftSource::Set { .. } => session.config.source.actual_set_codes(),
            // A cube contains no draft boosters from any set.
            DraftSource::Cube { .. } => Vec::new(),
        },
        DraftKind::Quick | DraftKind::Premier | DraftKind::Traditional | DraftKind::Sealed => {
            Vec::new()
        }
    }
}

fn apply_submit_deck(
    session: &mut DraftSession,
    seat: u8,
    main_deck: Vec<String>,
    commanders: Vec<String>,
) -> Result<Vec<DraftDelta>, DraftError> {
    if session.status != DraftStatus::Deckbuilding {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "SubmitDeck".to_string(),
        });
    }

    let pod_size = session.seats.len() as u8;
    if seat >= pod_size {
        return Err(DraftError::SeatOutOfRange { seat, pod_size });
    }

    // CR 702.124g: "no partner ability or combination of partner abilities can
    // ever let a player have more than two commanders." Pure arithmetic on the
    // payload -- it needs neither the deck nor the pool, so it belongs here
    // rather than in the pool validator. This is a second, independent
    // authority to the server's wire guard: a payload arriving by any other
    // route (draft-wasm's local/P2P submit, a future transport) is still bound.
    if commanders.len() > MAX_COMMANDER_DESIGNATIONS {
        return Err(DraftError::ValidationFailed {
            errors: vec![LimitedDeckError::TooManyCommanders {
                designated: commanders.len(),
                maximum: MAX_COMMANDER_DESIGNATIONS,
            }],
        });
    }

    // Collect pool card names for validation
    let pool_names: Vec<String> = session.pools[seat as usize]
        .iter()
        .map(|c| c.name.clone())
        .collect();

    // CR 903.13e: the grant is latched from what the draft contained, never
    // re-derived from what the pool happens to hold.
    let concessions = session_concessions(session);

    if let Err(errors) = validate_limited_deck(
        &main_deck,
        &pool_names,
        &session.config.addable_cards,
        session.config.min_deck_size,
        &concessions.fillers,
        &commanders,
        // CR 903.3: the floor is the kind's, read from the procedure table.
        // This is the line that makes the value kind-derived rather than
        // assumed -- `0` for the four CR 905.1a kinds, `1` for CommanderDraft.
        usize::from(session.kind.procedure().commanders_required),
    ) {
        return Err(DraftError::ValidationFailed { errors });
    }

    // Find the PlayerId for this seat
    let player_id = match &session.seats[seat as usize] {
        DraftSeat::Human { player_id, .. } => *player_id,
        DraftSeat::Bot { .. } => PlayerId(seat),
    };

    session.submitted_decks.insert(
        player_id,
        DraftDeckSubmission {
            seat,
            main_deck,
            // CR 903.3: snapshotted, not re-derived. A later pool change
            // must never silently re-designate this seat's commander(s).
            commanders,
        },
    );

    let mut deltas = vec![DraftDelta::DeckSubmitted { seat }];

    // Check if all human seats have submitted
    let human_count = session
        .seats
        .iter()
        .filter(|s| matches!(s, DraftSeat::Human { .. }))
        .count();

    let submitted_human_count = session
        .seats
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, DraftSeat::Human { .. }))
        .filter(|(i, _)| {
            let pid = match &session.seats[*i] {
                DraftSeat::Human { player_id, .. } => *player_id,
                DraftSeat::Bot { .. } => unreachable!(),
            };
            session.submitted_decks.contains_key(&pid)
        })
        .count();

    if submitted_human_count >= human_count {
        // Tournament-shaped events transition to Pairing for in-session play.
        // Quick Draft (1 human) completes directly, and CR 903.13a puts
        // Commander Draft in the same shape: "a draft ... followed by a
        // multiplayer game" — the game is arranged outside the draft session,
        // not as a bracket inside it.
        let next_status = match session.kind.procedure().post_draft_play {
            PostDraftPlay::CompleteImmediately => DraftStatus::Complete,
            PostDraftPlay::TournamentPairings => DraftStatus::Pairing,
        };
        session.status = next_status;
        deltas.push(DraftDelta::TransitionedTo {
            status: next_status,
        });
    }

    Ok(deltas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_source::FixturePackSource;
    // The one-set row of the concession table. The production latch reads the
    // UNION (`draft_set_concessions_for`); these rows assert the union against
    // its parts, so they need the per-set answer too.
    use engine::game::deck_validation::draft_set_concessions;

    fn test_session(pod_size: u8) -> (DraftSession, FixturePackSource) {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..pod_size)
            .map(|i| DraftSeat::Human {
                player_id: PlayerId(i),
                display_name: format!("Player {i}"),
            })
            .collect();
        let source = FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let session = DraftSession::new(config, seats, "TEST-001".to_string());
        (session, source)
    }

    /// A source whose boosters differ in size by pack number — the shape a
    /// multi-set draft produces when its sets have different MTGJSON booster
    /// sizes. Sizes shorter than the pack count repeat their last entry, the
    /// same rule the rest of the pack-ordered sequences follow.
    struct MixedSizePackSource {
        sizes: Vec<u8>,
    }

    impl PackSource for MixedSizePackSource {
        fn generate_pack(
            &self,
            _rng: &mut dyn rand::RngCore,
            seat: u8,
            pack_number: u8,
        ) -> DraftPack {
            let size = entry_for_pack(&self.sizes, pack_number)
                .copied()
                .unwrap_or(0);
            DraftPack(
                (0..size)
                    .map(|i| DraftCardInstance {
                        instance_id: format!("MIX-{seat}-{pack_number}-{i}"),
                        name: format!("Mixed {seat}-{pack_number}-{i}"),
                        set_code: "MIX".to_string(),
                        collector_number: format!("{}", i + 1),
                        rarity: "common".to_string(),
                        colors: Vec::new(),
                        cmc: 0,
                        type_line: String::new(),
                        draft_effect: None,
                    })
                    .collect(),
            )
        }
    }

    #[test]
    fn starting_a_draft_records_the_size_of_every_booster_it_opened() {
        let (mut session, _) = test_session(2);
        session.config.source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["AAA".to_string(), "BBB".to_string(), "AAA".to_string()],
            },
        };
        let source = MixedSizePackSource {
            sizes: vec![15, 14, 15],
        };

        apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        assert_eq!(session.pack_sizes, vec![15, 14, 15]);
        assert_eq!(session.cards_in_pack(0), 15);
        assert_eq!(session.cards_in_pack(1), 14);
        assert_eq!(session.total_pack_cards(), 44);
        assert_eq!(
            session.pack_set_code_sequence(),
            vec!["AAA".to_string(), "BBB".to_string(), "AAA".to_string()]
        );
    }

    #[test]
    fn a_session_with_no_recorded_pack_sizes_falls_back_to_the_uniform_size() {
        // Snapshots written before per-pack sizes were recorded leave the
        // sequence empty; the uniform config value still describes them.
        let (session, _) = test_session(2);

        assert!(session.pack_sizes.is_empty());
        assert_eq!(session.cards_in_pack(0), 14);
        assert_eq!(session.cards_in_pack(2), 14);
        assert_eq!(session.total_pack_cards(), 42);
    }

    #[test]
    fn a_mixed_size_sealed_pool_satisfies_the_snapshot_invariant() {
        let (mut session, _) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = SEALED_PACK_COUNT;
        session.config.source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["AAA".to_string(); 3]
                    .into_iter()
                    .chain(vec!["BBB".to_string(); 3])
                    .collect(),
            },
        };
        session.set_code = session.config.source.set_code();
        session.config.set_code = session.set_code.clone();
        let source = MixedSizePackSource {
            sizes: vec![15, 15, 15, 14, 14, 14],
        };

        apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        assert_eq!(session.status, DraftStatus::Deckbuilding);
        assert_eq!(session.total_pack_cards(), 87);
        assert!(session.pools.iter().all(|pool| pool.len() == 87));
        // The uniform scalar would have expected 6 × 14 = 84 and rejected this.
        session
            .validate_persisted_snapshot()
            .expect("a mixed-size sealed pool is valid");
    }

    #[test]
    fn sealed_atomically_allocates_six_packs_per_seat_and_enters_deckbuilding() {
        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;

        let deltas = apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        assert_eq!(session.status, DraftStatus::Deckbuilding);
        assert_eq!(session.pools.len(), 2);
        assert!(session.pools.iter().all(|pool| pool.len() == 84));
        assert!(session.current_pack.iter().all(Option::is_none));
        assert!(session.packs_by_seat.iter().all(Vec::is_empty));
        assert_eq!(
            deltas,
            vec![
                DraftDelta::DraftStarted,
                DraftDelta::TransitionedTo {
                    status: DraftStatus::Deckbuilding,
                },
            ]
        );
    }

    #[test]
    fn sealed_rejects_cube_source_without_mutating_pools() {
        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;
        session.config.source = DraftSource::Cube {
            id: "cube".to_string(),
            name: "Cube".to_string(),
        };

        let before = session.pools.clone();
        assert_eq!(
            apply(&mut session, DraftAction::StartDraft, Some(&source)),
            Err(DraftError::SealedRequiresSetSource)
        );
        assert_eq!(session.status, DraftStatus::Lobby);
        assert_eq!(session.pools, before);
    }

    #[test]
    fn tournament_size_limits_apply_to_sealed_boundaries() {
        for (format, pod_size, accepted) in [
            (TournamentFormat::Swiss, 1, false),
            (TournamentFormat::Swiss, 2, true),
            (TournamentFormat::Swiss, 8, true),
            (TournamentFormat::Swiss, 9, false),
            (TournamentFormat::SingleElimination, 7, false),
            (TournamentFormat::SingleElimination, 8, true),
            (TournamentFormat::SingleElimination, 9, false),
        ] {
            let (mut session, source) = test_session(pod_size);
            session.kind = DraftKind::Sealed;
            session.config.kind = DraftKind::Sealed;
            session.config.pack_count = 6;
            session.config.tournament_format = format;

            assert_eq!(
                apply(&mut session, DraftAction::StartDraft, Some(&source)).is_ok(),
                accepted,
                "{format:?} with {pod_size} seats"
            );
        }
    }

    #[test]
    fn quick_cube_allows_single_and_large_pods() {
        for pod_size in [1, 9, 16] {
            let (mut session, source) = test_session(pod_size);
            session.kind = DraftKind::Quick;
            session.config.kind = DraftKind::Quick;
            session.config.source = DraftSource::Cube {
                id: "cube".to_string(),
                name: "Cube".to_string(),
            };

            assert!(
                apply(&mut session, DraftAction::StartDraft, Some(&source)).is_ok(),
                "Quick Cube with {pod_size} seats should start"
            );
        }
    }

    #[test]
    fn sealed_rejects_mismatched_set_code_before_generating_packs() {
        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;
        session.config.source = DraftSource::single_set("OTHER".to_string());

        assert!(matches!(
            apply(&mut session, DraftAction::StartDraft, Some(&source)),
            Err(DraftError::InvalidSealedConfiguration { .. })
        ));
        assert_eq!(session.status, DraftStatus::Lobby);
    }

    #[test]
    fn sealed_snapshot_rejects_retained_packs_but_allows_legacy_seat_flags() {
        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;
        apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        session.seats_picked_this_round = SeatFlags::default();
        session.connected_seats = SeatFlags::default();
        assert!(session.validate_persisted_snapshot().is_ok());

        session.packs_by_seat[0].push(DraftPack(Vec::new()));
        assert!(matches!(
            session.validate_persisted_snapshot(),
            Err(DraftError::InvalidSealedSnapshot { .. })
        ));
    }

    #[test]
    fn sealed_snapshot_rejects_started_pool_with_wrong_card_count() {
        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;
        apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        session.pools[0].pop();

        assert!(matches!(
            session.validate_persisted_snapshot(),
            Err(DraftError::InvalidSealedSnapshot { .. })
        ));
    }

    #[test]
    fn sealed_snapshot_rejects_session_and_configuration_kind_mismatch() {
        let (mut session, _) = test_session(2);
        session.kind = DraftKind::Sealed;

        assert!(matches!(
            session.validate_persisted_snapshot(),
            Err(DraftError::InvalidSealedSnapshot { .. })
        ));
    }

    #[test]
    fn sealed_snapshot_rejects_invalid_tournament_size_and_drafting_status() {
        let (mut session, _source) = test_session(1);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;
        assert!(matches!(
            session.validate_persisted_snapshot(),
            Err(DraftError::InvalidSealedSnapshot { .. })
        ));

        let (mut session, source) = test_session(2);
        session.kind = DraftKind::Sealed;
        session.config.kind = DraftKind::Sealed;
        session.config.pack_count = 6;
        apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        session.status = DraftStatus::Drafting;
        assert!(matches!(
            session.validate_persisted_snapshot(),
            Err(DraftError::InvalidSealedSnapshot { .. })
        ));
    }

    #[test]
    fn persisted_snapshot_uses_the_procedure_pod_size_policy_for_every_kind() {
        let (session, _) = test_session(9);
        assert!(matches!(
            session.validate_persisted_snapshot(),
            Err(DraftError::InvalidSealedSnapshot { .. })
        ));

        let (mut commander, _) = test_session(3);
        commander.kind = DraftKind::CommanderDraft;
        commander.config.kind = DraftKind::CommanderDraft;
        commander.config.tournament_format = TournamentFormat::SingleElimination;
        assert!(commander.validate_persisted_snapshot().is_ok());
    }

    #[test]
    fn new_session_starts_in_lobby() {
        let (session, _) = test_session(8);
        assert_eq!(session.status, DraftStatus::Lobby);
        assert_eq!(session.seats.len(), 8);
        assert_eq!(session.pools.len(), 8);
        assert!(session.pools.iter().all(|p| p.is_empty()));
        assert!(session.current_pack.iter().all(|p| p.is_none()));
        assert_eq!(session.draft_code, "TEST-001");
    }

    #[test]
    fn start_draft_transitions_to_drafting() {
        let (mut session, source) = test_session(8);
        let deltas = apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        assert_eq!(session.status, DraftStatus::Drafting);
        assert_eq!(deltas, vec![DraftDelta::DraftStarted]);
        // Each seat should have a current pack with 14 cards
        for pack in &session.current_pack {
            assert!(pack.is_some());
            assert_eq!(pack.as_ref().unwrap().0.len(), 14);
        }
        // Each seat should have 2 remaining packs in packs_by_seat
        for seat_packs in &session.packs_by_seat {
            assert_eq!(seat_packs.len(), 2);
        }
    }

    #[test]
    fn start_draft_on_non_lobby_returns_error() {
        let (mut session, source) = test_session(8);
        apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        // Try again -- should fail
        let result = apply(&mut session, DraftAction::StartDraft, Some(&source));
        assert!(matches!(
            result,
            Err(DraftError::InvalidTransition {
                from: DraftStatus::Drafting,
                ..
            })
        ));
    }

    #[test]
    fn submit_deck_on_deckbuilding_stores_submission() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;
        // Give seat 0 a pool of 42 cards
        session.pools[0] = (0..42)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: "TST".to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();

        let mut main_deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 17));

        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();

        assert!(deltas.contains(&DraftDelta::DeckSubmitted { seat: 0 }));
        assert!(session.submitted_decks.contains_key(&PlayerId(0)));
    }

    #[test]
    fn submit_deck_invalid_too_few_cards() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;
        session.pools[0] = (0..42)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: "TST".to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();

        let main_deck: Vec<String> = (0..10).map(|i| format!("Card {i}")).collect();
        let result = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders: Vec::new(),
            },
            None,
        );

        assert!(matches!(result, Err(DraftError::ValidationFailed { .. })));
    }

    #[test]
    fn submit_deck_all_submitted_quick_draft_transitions_to_complete() {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Quick,
            pod_size: 2,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats = vec![
            DraftSeat::Human {
                player_id: PlayerId(0),
                display_name: "Player 0".to_string(),
            },
            DraftSeat::Bot {
                name: "Bot 1".to_string(),
            },
        ];
        let mut session = DraftSession::new(config, seats, "TEST-QD".to_string());
        session.status = DraftStatus::Deckbuilding;

        session.pools[0] = (0..42)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: "TST".to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();

        let mut main_deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 17));

        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();
        assert!(deltas.contains(&DraftDelta::TransitionedTo {
            status: DraftStatus::Complete,
        }));
        assert_eq!(session.status, DraftStatus::Complete);
    }

    #[test]
    fn submit_deck_all_submitted_premier_transitions_to_pairing() {
        let (mut session, _) = test_session(2);
        session.status = DraftStatus::Deckbuilding;

        for seat in 0..2 {
            session.pools[seat] = (0..42)
                .map(|i| DraftCardInstance {
                    instance_id: format!("s{seat}-card-{i}"),
                    name: format!("Card {i}"),
                    set_code: "TST".to_string(),
                    collector_number: format!("{i}"),
                    rarity: "common".to_string(),
                    colors: Vec::new(),
                    cmc: 0,
                    type_line: String::new(),
                    draft_effect: None,
                })
                .collect();
        }

        let make_deck = || {
            let mut deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
            deck.extend(std::iter::repeat_n("Plains".to_string(), 17));
            deck
        };

        // Seat 0 submits
        apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: make_deck(),
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();

        // Seat 1 submits -- Premier draft transitions to Pairing
        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 1,
                main_deck: make_deck(),
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();
        assert!(deltas.contains(&DraftDelta::TransitionedTo {
            status: DraftStatus::Pairing,
        }));
        assert_eq!(session.status, DraftStatus::Pairing);
    }

    /// CR 903.13a: Commander Draft is "a draft ... followed by a multiplayer
    /// game", so the session ends at `Complete` and the game is arranged
    /// outside it — it must NOT enter an in-session tournament bracket.
    ///
    /// The sibling case (a Premier pod reaching `Pairing`) is
    /// `submit_deck_all_submitted_premier_transitions_to_pairing` above; the
    /// two together are what show this reads `post_draft_play` rather than
    /// having replaced one hardcoded status with another.
    #[test]
    fn commander_draft_completes_without_tournament_pairings() {
        let (mut session, _) = test_session(4);
        session.kind = DraftKind::CommanderDraft;
        session.config.kind = DraftKind::CommanderDraft;
        // CR 903.13f(1): the Commander Draft pool floor.
        session.config.min_deck_size = 60;
        // 1 human + 3 bots, the kind's default seat shape.
        session.seats = vec![
            DraftSeat::Human {
                player_id: PlayerId(0),
                display_name: "Player 0".to_string(),
            },
            DraftSeat::Bot {
                name: "Bot 1".to_string(),
            },
            DraftSeat::Bot {
                name: "Bot 2".to_string(),
            },
            DraftSeat::Bot {
                name: "Bot 3".to_string(),
            },
        ];
        session.status = DraftStatus::Deckbuilding;
        session.pools[0] = (0..42)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: "TST".to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();

        // Positive reach-guard: the session really is in the state that makes
        // the terminal-status branch reachable, so `Complete` below cannot be
        // an artifact of a branch that never ran.
        assert_eq!(session.status, DraftStatus::Deckbuilding);
        assert!(session.submitted_decks.is_empty());

        let mut main_deck: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 20));

        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                // CR 903.3: a Commander deck designates a commander. `Card 0`
                // is in both `main_deck` (0..40) and this seat's pool (0..42),
                // so `CommanderNotInDeck` cannot fire. The row's subject is
                // unchanged -- only what makes a Commander deck legal is.
                commanders: vec!["Card 0".to_string()],
            },
            None,
        )
        .unwrap();

        assert!(
            !session.submitted_decks.is_empty(),
            "reach-guard: the deck submission itself must have landed"
        );
        assert!(deltas.contains(&DraftDelta::TransitionedTo {
            status: DraftStatus::Complete,
        }));
        assert_eq!(session.status, DraftStatus::Complete);
        assert_ne!(session.status, DraftStatus::Pairing);
    }

    /// PF2 row 3c — the premise pin for the client's two-clock adapter stub.
    ///
    /// This is a PREMISE PIN, not a discriminating test: it asserts current,
    /// correct reducer behaviour and its value is identical on the fixed and
    /// unfixed client trees. Its job is to RED if a future edit changes the
    /// reducer out from under
    /// `client/src/adapter/__tests__/p2pDraftPodComplete.test.ts` and
    /// `client/src/pages/__tests__/DraftPodPage.podComplete.test.tsx`, whose
    /// stubs assert these transitions rather than measure them. Without it the
    /// stub's premise would be invented, and every row above it green against a
    /// fiction.
    ///
    /// Three of the four pinned halves are here; the fourth (the
    /// `TournamentPairings` split and the `MatchInProgress` transition) is
    /// `premier_pod_reaches_pairing_then_match_in_progress` below.
    ///
    /// Half (a) — `:895`'s outstanding-human gate: no transition while a human
    ///            seat has not submitted. This is what makes the client stub's
    ///            ACCUMULATING submitted-seat set a measured premise.
    /// Half (b) — `:902`: `PostDraftPlay::CompleteImmediately` yields
    ///            `Complete` (CR 903.13a — the game is arranged outside the
    ///            session, not as a bracket inside it).
    /// Half (c) — `:214-217`/`:218-223`: generating pairings for a `Complete`
    ///            session is REFUSED. This is the premise the client row
    ///            "emits a viewUpdated carrying Complete" rests its PRIMARY
    ///            assertion on: widen this admit set to include `Complete` and
    ///            that row silently stops discriminating, with nothing red.
    ///            `test_generate_pairings_wrong_status` above pins
    ///            `from: Lobby`, not `from: Complete`.
    #[test]
    fn commander_pod_reaches_complete_and_generate_pairings_is_refused() {
        let (mut session, _) = test_session(4);
        session.kind = DraftKind::CommanderDraft;
        session.config.kind = DraftKind::CommanderDraft;
        // CR 903.13f(1): the Commander Draft deck floor.
        session.config.min_deck_size = 60;
        // TWO humans, so the outstanding-human gate has something to gate on.
        session.seats = vec![
            DraftSeat::Human {
                player_id: PlayerId(0),
                display_name: "Player 0".to_string(),
            },
            DraftSeat::Human {
                player_id: PlayerId(1),
                display_name: "Player 1".to_string(),
            },
            DraftSeat::Bot {
                name: "Bot 2".to_string(),
            },
            DraftSeat::Bot {
                name: "Bot 3".to_string(),
            },
        ];
        session.status = DraftStatus::Deckbuilding;
        for seat in 0..2 {
            session.pools[seat] = (0..42)
                .map(|i| DraftCardInstance {
                    instance_id: format!("s{seat}-card-{i}"),
                    name: format!("Card {i}"),
                    set_code: "TST".to_string(),
                    collector_number: format!("{i}"),
                    rarity: "common".to_string(),
                    colors: Vec::new(),
                    cmc: 0,
                    type_line: String::new(),
                    draft_effect: None,
                })
                .collect();
        }

        let make_deck = || {
            let mut deck: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();
            deck.extend(std::iter::repeat_n("Plains".to_string(), 20));
            deck
        };

        // Reach guard: the terminal branch is reachable at all.
        assert_eq!(session.status, DraftStatus::Deckbuilding);

        apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: make_deck(),
                // CR 903.3: a Commander deck designates a commander. `Card 0`
                // is in both `main_deck` (0..40) and this seat's pool (0..42),
                // so `CommanderNotInDeck` cannot fire. The row's subject is
                // unchanged -- only what makes a Commander deck legal is.
                commanders: vec!["Card 0".to_string()],
            },
            None,
        )
        .unwrap();

        // HALF (a) — session.rs:895. Seat 1 is still outstanding, so nothing
        // transitions. The client stub projects exactly this.
        assert!(
            session.submitted_decks.contains_key(&PlayerId(0)),
            "reach-guard: seat 0's submission must have landed"
        );
        assert_eq!(session.status, DraftStatus::Deckbuilding);

        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 1,
                main_deck: make_deck(),
                // CR 903.3: a Commander deck designates a commander. `Card 0`
                // is in both `main_deck` (0..40) and this seat's pool (0..42),
                // so `CommanderNotInDeck` cannot fire. The row's subject is
                // unchanged -- only what makes a Commander deck legal is.
                commanders: vec!["Card 0".to_string()],
            },
            None,
        )
        .unwrap();

        // HALF (b) — session.rs:902, assigned at :905.
        assert!(deltas.contains(&DraftDelta::TransitionedTo {
            status: DraftStatus::Complete,
        }));
        assert_eq!(session.status, DraftStatus::Complete);
        assert_ne!(session.status, DraftStatus::Pairing);

        // HALF (c) — session.rs:214-217 admits only
        // Deckbuilding | Pairing | RoundComplete, so :218-223 refuses this.
        let result = apply(&mut session, DraftAction::GeneratePairings, None);
        assert!(matches!(
            result,
            Err(DraftError::InvalidTransition {
                from: DraftStatus::Complete,
                ..
            })
        ));
    }

    /// PF2 row 3c, half (d) — the `TournamentPairings` arm of the same pin.
    ///
    /// Also a PREMISE PIN: (1) = (2) on both client trees. It pins the two
    /// reducer facts the client stub's clock (b) encodes — `session.rs:903`'s
    /// `Pairing` and `:254`'s overwrite to `MatchInProgress` — which is what
    /// makes "the FIRST viewUpdated after allDecksSubmitted carries Pairing" a
    /// measured claim rather than an invented one.
    #[test]
    fn premier_pod_reaches_pairing_then_match_in_progress() {
        let (mut session, _) = test_session(2);
        session.status = DraftStatus::Deckbuilding;
        for seat in 0..2 {
            session.pools[seat] = (0..42)
                .map(|i| DraftCardInstance {
                    instance_id: format!("s{seat}-card-{i}"),
                    name: format!("Card {i}"),
                    set_code: "TST".to_string(),
                    collector_number: format!("{i}"),
                    rarity: "common".to_string(),
                    colors: Vec::new(),
                    cmc: 0,
                    type_line: String::new(),
                    draft_effect: None,
                })
                .collect();
        }

        let make_deck = || {
            let mut deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
            deck.extend(std::iter::repeat_n("Plains".to_string(), 17));
            deck
        };

        apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: make_deck(),
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();

        // The same outstanding-human gate (:895), on the other arm.
        assert_eq!(session.status, DraftStatus::Deckbuilding);

        apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 1,
                main_deck: make_deck(),
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();

        // session.rs:903, assigned at :905.
        assert_eq!(session.status, DraftStatus::Pairing);

        // session.rs:254 — generating OVERWRITES Pairing with MatchInProgress,
        // and nothing else republishes Pairing. That is why the host must
        // broadcast BEFORE it generates.
        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();
        assert_eq!(session.status, DraftStatus::MatchInProgress);
    }

    #[test]
    fn submit_deck_on_non_deckbuilding_returns_error() {
        let (mut session, _) = test_session(8);
        let result = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec![],
                commanders: Vec::new(),
            },
            None,
        );
        assert!(matches!(
            result,
            Err(DraftError::InvalidTransition {
                from: DraftStatus::Lobby,
                ..
            })
        ));
    }

    #[test]
    fn test_swiss_pairings_8_players() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        let deltas = apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        assert!(deltas.contains(&DraftDelta::PairingsGenerated { round: 1 }));
        assert!(deltas.contains(&DraftDelta::TransitionedTo {
            status: DraftStatus::MatchInProgress,
        }));
        assert_eq!(session.status, DraftStatus::MatchInProgress);
        assert_eq!(session.current_round, 1);

        // Should have 4 pairings (8 players / 2)
        let round_pairings: Vec<_> = session.pairings.iter().filter(|p| p.round == 1).collect();
        assert_eq!(round_pairings.len(), 4);

        // All 8 players should be paired, no duplicates
        let mut paired_players: Vec<PlayerId> = round_pairings
            .iter()
            .flat_map(|p| p.players.iter().copied())
            .collect();
        paired_players.sort_by_key(|p| p.0);
        paired_players.dedup();
        assert_eq!(paired_players.len(), 8);
    }

    #[test]
    fn swiss_pairings_include_bot_filled_seats() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;
        for seat in 2..8 {
            session.seats[seat] = DraftSeat::Bot {
                name: format!("Bot {seat}"),
            };
        }

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let round_pairings: Vec<_> = session.pairings.iter().filter(|p| p.round == 1).collect();
        assert_eq!(round_pairings.len(), 4);
        let mut paired_players: Vec<PlayerId> = round_pairings
            .iter()
            .flat_map(|p| p.players.iter().copied())
            .collect();
        paired_players.sort_by_key(|p| p.0);
        paired_players.dedup();
        assert_eq!(paired_players, (0u8..8).map(PlayerId).collect::<Vec<_>>());
    }

    #[test]
    fn report_match_result_updates_bot_records() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;
        session.seats[7] = DraftSeat::Bot {
            name: "Bot 7".to_string(),
        };
        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();
        let pairing = session
            .pairings
            .iter()
            .find(|p| p.players.contains(&PlayerId(7)))
            .unwrap()
            .clone();

        apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: pairing.match_id,
                winner_seat: Some(7),
            },
            None,
        )
        .unwrap();

        let record = session.match_records.get(&PlayerId(7)).unwrap();
        assert_eq!(record.match_wins, 1);
    }

    #[test]
    fn single_elimination_rejects_non_eight_player_pods() {
        let (mut session, _) = test_session(4);
        session.status = DraftStatus::Deckbuilding;
        session.config.tournament_format = TournamentFormat::SingleElimination;

        let result = apply(&mut session, DraftAction::GeneratePairings, None);

        assert!(matches!(
            result,
            Err(DraftError::UnsupportedTournamentSize {
                format: TournamentFormat::SingleElimination,
                required: 8,
                actual: 4,
            })
        ));
        assert_eq!(session.status, DraftStatus::Deckbuilding);
        assert!(session.pairings.is_empty());
    }

    #[test]
    fn complete_immediately_procedure_never_generates_pairings() {
        let (mut session, _) = commander_session(3);
        session.status = DraftStatus::Deckbuilding;
        // Deckbuilding is normally valid for pairing generation, and this
        // procedure's Swiss range admits a three-seat pod. The rejection below
        // therefore depends specifically on the CompleteImmediately guard,
        // rather than on a terminal status or an unsupported bracket size.
        session.config.tournament_format = TournamentFormat::Swiss;
        assert_eq!(
            session.kind.procedure().post_draft_play,
            PostDraftPlay::CompleteImmediately
        );

        assert!(matches!(
            apply(&mut session, DraftAction::GeneratePairings, None),
            Err(DraftError::InvalidTransition {
                from: DraftStatus::Deckbuilding,
                ..
            })
        ));
        assert!(session.pairings.is_empty());
    }

    #[test]
    fn test_swiss_rematch_avoidance() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        // Generate round 1
        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        // Record all round 1 pairings as opponent pairs
        let round1_pairs: Vec<[PlayerId; 2]> = session
            .pairings
            .iter()
            .filter(|p| p.round == 1)
            .map(|p| p.players)
            .collect();

        // Complete all round 1 pairings with alternating winners
        for (i, pairing) in session
            .pairings
            .iter_mut()
            .filter(|p| p.round == 1)
            .enumerate()
        {
            pairing.status = PairingStatus::Complete;
            let winner = pairing.players[i % 2];
            pairing.winner = Some(winner);
            ensure_match_record(&mut session.match_records, winner).match_wins += 1;
            let loser = pairing.players[(i + 1) % 2];
            ensure_match_record(&mut session.match_records, loser).match_losses += 1;
        }

        session.status = DraftStatus::RoundComplete;

        // Generate round 2
        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let round2_pairs: Vec<[PlayerId; 2]> = session
            .pairings
            .iter()
            .filter(|p| p.round == 2)
            .map(|p| p.players)
            .collect();

        // Verify no rematches (when avoidable)
        let mut rematch_count = 0;
        for r2 in &round2_pairs {
            for r1 in &round1_pairs {
                if (r2[0] == r1[0] && r2[1] == r1[1]) || (r2[0] == r1[1] && r2[1] == r1[0]) {
                    rematch_count += 1;
                }
            }
        }
        assert_eq!(
            rematch_count, 0,
            "round 2 should avoid rematches with 8 players"
        );
    }

    #[test]
    fn swiss_four_pod_round_three_completes_the_round_robin() {
        // #7937: with 4 players and 3 rounds every rematch is avoidable —
        // round 3 is exactly the two pairs that have not met yet. The
        // history is CRAFTED (not driven through the rng) so the forcing
        // shape holds in every shuffle order: the 2-win leader A has already
        // faced both 1-win players, so any head-first pick without
        // backtracking rematches deterministically. The only rematch-free
        // completion is A–D / B–C.
        let (mut session, _) = test_session(4);
        let [a, b, c, d] = [0u8, 1, 2, 3].map(|seat| seat_player_id(&session, seat));

        let mk = |round: u8, table: u8, players: [PlayerId; 2], winner: PlayerId| DraftPairing {
            round,
            table,
            players,
            match_id: format!("r{round}-t{table}"),
            status: PairingStatus::Complete,
            winner: Some(winner),
        };
        session.pairings = vec![
            mk(1, 0, [a, b], a),
            mk(1, 1, [c, d], c),
            mk(2, 0, [a, c], a),
            mk(2, 1, [b, d], b),
        ];
        for (pid, wins, losses) in [(a, 2, 0), (b, 1, 1), (c, 1, 1), (d, 0, 2)] {
            let record = ensure_match_record(&mut session.match_records, pid);
            record.match_wins = wins;
            record.match_losses = losses;
        }
        session.current_round = 2;
        session.status = DraftStatus::RoundComplete;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let round3: HashSet<(PlayerId, PlayerId)> = session
            .pairings
            .iter()
            .filter(|p| p.round == 3)
            .map(|p| {
                let [x, y] = p.players;
                if x.0 <= y.0 {
                    (x, y)
                } else {
                    (y, x)
                }
            })
            .collect();
        let want: HashSet<(PlayerId, PlayerId)> = [(a, d), (b, c)]
            .into_iter()
            .map(|(x, y)| if x.0 <= y.0 { (x, y) } else { (y, x) })
            .collect();
        assert_eq!(
            round3, want,
            "round 3 must be the round-robin completion A–D / B–C — anything \
             else contains an avoidable rematch"
        );
    }

    /// Crafted-history session for direct `generate_swiss_pairings` calls.
    /// Distinct win counts put every player in a singleton bracket, so the
    /// in-bracket shuffle is the identity and the pool order is exactly the
    /// standings order — the scenarios below are deterministic in every rng.
    fn crafted_swiss_session(
        pod_size: u8,
        wins: &[u8],
        prior_round_one: &[[usize; 2]],
    ) -> (DraftSession, Vec<PlayerId>) {
        let (mut session, _) = test_session(pod_size);
        let pids: Vec<PlayerId> = (0..pod_size)
            .map(|seat| seat_player_id(&session, seat))
            .collect();
        for (pid, &w) in pids.iter().zip(wins) {
            ensure_match_record(&mut session.match_records, *pid).match_wins = w;
        }
        session.pairings = prior_round_one
            .iter()
            .enumerate()
            .map(|(t, &[x, y])| DraftPairing {
                round: 1,
                table: t as u8,
                players: [pids[x], pids[y]],
                match_id: format!("r1-t{t}"),
                status: PairingStatus::Complete,
                winner: Some(pids[x]),
            })
            .collect();
        (session, pids)
    }

    fn unordered([x, y]: [PlayerId; 2]) -> (PlayerId, PlayerId) {
        if x.0 <= y.0 {
            (x, y)
        } else {
            (y, x)
        }
    }

    #[test]
    fn swiss_backtracks_when_the_first_legal_partner_dead_ends() {
        // Pool order A,B,C,D (wins 3/2/1/0); only C–D is prior. A's FIRST
        // legal partner is B, but pairing A–B leaves C–D as a forced
        // rematch — the search must back out and land on A–C / B–D. A
        // first-legal-partner greedy without backtracking cannot reach
        // this answer.
        let (session, p) = crafted_swiss_session(4, &[3, 2, 1, 0], &[[2, 3]]);
        let mut rng = ChaCha20Rng::seed_from_u64(7);
        let (pairings, bye) = generate_swiss_pairings(&session, 2, &mut rng);
        assert_eq!(bye, None);
        let got: HashSet<(PlayerId, PlayerId)> =
            pairings.iter().map(|pr| unordered(pr.players)).collect();
        let want = HashSet::from([unordered([p[0], p[2]]), unordered([p[1], p[3]])]);
        assert_eq!(
            got, want,
            "the dead end behind A–B must back out to A–C / B–D"
        );
    }

    #[test]
    fn swiss_two_player_round_two_admits_the_unavoidable_rematch() {
        // With two players every later round repeats the only possible
        // pair — the rematch-free search fails and the fallback must still
        // produce the pairing instead of an empty round.
        let (session, p) = crafted_swiss_session(2, &[1, 0], &[[0, 1]]);
        let mut rng = ChaCha20Rng::seed_from_u64(7);
        let (pairings, bye) = generate_swiss_pairings(&session, 2, &mut rng);
        assert_eq!(bye, None);
        assert_eq!(
            pairings.len(),
            1,
            "the unavoidable rematch must be admitted"
        );
        assert_eq!(unordered(pairings[0].players), unordered([p[0], p[1]]));
    }

    #[test]
    fn swiss_bye_walks_up_when_the_bottom_bye_forces_a_rematch() {
        // 3 players, wins 2/1/0, prior A–B. Handing the bye to bottom C
        // leaves A–B as a forced rematch, so the bye must walk up to B and
        // pair A–C fresh.
        let (session, p) = crafted_swiss_session(3, &[2, 1, 0], &[[0, 1]]);
        let mut rng = ChaCha20Rng::seed_from_u64(7);
        let (pairings, bye) = generate_swiss_pairings(&session, 2, &mut rng);
        assert_eq!(
            bye,
            Some(p[1]),
            "the bye walks past the bottom seat to keep the round rematch-free"
        );
        assert_eq!(pairings.len(), 1);
        assert_eq!(unordered(pairings[0].players), unordered([p[0], p[2]]));
    }

    #[test]
    fn test_se_bracket_8_players() {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::SingleElimination,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..8)
            .map(|i| DraftSeat::Human {
                player_id: PlayerId(i),
                display_name: format!("Player {i}"),
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "SE-TEST".to_string());
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let pairings: Vec<_> = session.pairings.iter().filter(|p| p.round == 1).collect();
        assert_eq!(pairings.len(), 4);

        // Standard seeded bracket: 0v7, 1v6, 2v5, 3v4
        assert_eq!(pairings[0].players, [PlayerId(0), PlayerId(7)]);
        assert_eq!(pairings[1].players, [PlayerId(1), PlayerId(6)]);
        assert_eq!(pairings[2].players, [PlayerId(2), PlayerId(5)]);
        assert_eq!(pairings[3].players, [PlayerId(3), PlayerId(4)]);
    }

    #[test]
    fn single_elimination_advances_pairing_winners() {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::SingleElimination,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..8)
            .map(|i| DraftSeat::Human {
                player_id: PlayerId(i),
                display_name: format!("Player {i}"),
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "SE-TEST".to_string());
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        for (match_id, winner_seat) in [("r1-t0", 7), ("r1-t1", 6), ("r1-t2", 2), ("r1-t3", 4)] {
            apply(
                &mut session,
                DraftAction::ReportMatchResult {
                    match_id: match_id.to_string(),
                    winner_seat: Some(winner_seat),
                },
                None,
            )
            .unwrap();
        }

        assert_eq!(session.status, DraftStatus::RoundComplete);

        apply(&mut session, DraftAction::AdvanceRound, None).unwrap();
        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let pairings: Vec<_> = session.pairings.iter().filter(|p| p.round == 2).collect();
        assert_eq!(pairings.len(), 2);
        assert_eq!(pairings[0].players, [PlayerId(7), PlayerId(6)]);
        assert_eq!(pairings[1].players, [PlayerId(2), PlayerId(4)]);
    }

    #[test]
    fn single_elimination_rejects_match_without_winner() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;
        session.config.tournament_format = TournamentFormat::SingleElimination;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let result = apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: "r1-t0".to_string(),
                winner_seat: None,
            },
            None,
        );

        assert!(matches!(
            result,
            Err(DraftError::MatchWinnerRequired { .. })
        ));
    }

    #[test]
    fn test_report_result_updates_records() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap()
            .clone();
        let winner_pid = pairing.players[0];

        apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: "r1-t0".to_string(),
                winner_seat: Some(winner_pid.0),
            },
            None,
        )
        .unwrap();

        let winner_record = session.match_records.get(&winner_pid).unwrap();
        assert_eq!(winner_record.match_wins, 1);
        assert_eq!(winner_record.wins, 1);

        let pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap();
        assert_eq!(pairing.winner, Some(winner_pid));
        let loser_pid = if pairing.players[0] == winner_pid {
            pairing.players[1]
        } else {
            pairing.players[0]
        };
        let loser_record = session.match_records.get(&loser_pid).unwrap();
        assert_eq!(loser_record.match_losses, 1);
        assert_eq!(loser_record.losses, 1);
    }

    #[test]
    fn report_match_result_replaces_previous_result() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap()
            .clone();
        let first_winner = pairing.players[0];
        let second_winner = pairing.players[1];

        apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: pairing.match_id.clone(),
                winner_seat: Some(first_winner.0),
            },
            None,
        )
        .unwrap();
        apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: pairing.match_id.clone(),
                winner_seat: Some(second_winner.0),
            },
            None,
        )
        .unwrap();

        let first_record = session.match_records.get(&first_winner).unwrap();
        assert_eq!(first_record.match_wins, 0);
        assert_eq!(first_record.match_losses, 1);
        assert_eq!(first_record.wins, 0);
        assert_eq!(first_record.losses, 1);

        let second_record = session.match_records.get(&second_winner).unwrap();
        assert_eq!(second_record.match_wins, 1);
        assert_eq!(second_record.match_losses, 0);
        assert_eq!(second_record.wins, 1);
        assert_eq!(second_record.losses, 0);

        let updated_pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == pairing.match_id)
            .unwrap();
        assert_eq!(updated_pairing.winner, Some(second_winner));
    }

    #[test]
    fn report_match_result_replaces_legacy_completed_result() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap()
            .clone();
        let first_winner = pairing.players[0];
        let second_winner = pairing.players[1];

        session
            .pairings
            .iter_mut()
            .find(|p| p.match_id == pairing.match_id)
            .unwrap()
            .status = PairingStatus::Complete;
        ensure_match_record(&mut session.match_records, first_winner).match_wins = 1;
        ensure_match_record(&mut session.match_records, first_winner).wins = 1;
        ensure_match_record(&mut session.match_records, second_winner).match_losses = 1;
        ensure_match_record(&mut session.match_records, second_winner).losses = 1;

        apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: pairing.match_id.clone(),
                winner_seat: Some(second_winner.0),
            },
            None,
        )
        .unwrap();

        let first_record = session.match_records.get(&first_winner).unwrap();
        assert_eq!(first_record.match_wins, 0);
        assert_eq!(first_record.wins, 0);
        assert_eq!(first_record.match_losses, 1);
        assert_eq!(first_record.losses, 1);

        let second_record = session.match_records.get(&second_winner).unwrap();
        assert_eq!(second_record.match_wins, 1);
        assert_eq!(second_record.wins, 1);
        assert_eq!(second_record.match_losses, 0);
        assert_eq!(second_record.losses, 0);
    }

    #[test]
    fn report_match_result_can_override_after_round_complete() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let results: Vec<(String, u8)> = session
            .pairings
            .iter()
            .filter(|p| p.round == 1)
            .map(|p| (p.match_id.clone(), p.players[0].0))
            .collect();

        for (match_id, winner_seat) in results {
            apply(
                &mut session,
                DraftAction::ReportMatchResult {
                    match_id,
                    winner_seat: Some(winner_seat),
                },
                None,
            )
            .unwrap();
        }

        assert_eq!(session.status, DraftStatus::RoundComplete);

        let pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == "r1-t0")
            .unwrap()
            .clone();

        apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: pairing.match_id.clone(),
                winner_seat: Some(pairing.players[1].0),
            },
            None,
        )
        .unwrap();

        assert_eq!(session.status, DraftStatus::RoundComplete);
        let updated_pairing = session
            .pairings
            .iter()
            .find(|p| p.match_id == pairing.match_id)
            .unwrap();
        assert_eq!(updated_pairing.winner, Some(pairing.players[1]));
    }

    #[test]
    fn report_match_result_rejects_non_current_round_pairing() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let results: Vec<(String, u8)> = session
            .pairings
            .iter()
            .filter(|p| p.round == 1)
            .map(|p| (p.match_id.clone(), p.players[0].0))
            .collect();

        for (match_id, winner_seat) in results {
            apply(
                &mut session,
                DraftAction::ReportMatchResult {
                    match_id,
                    winner_seat: Some(winner_seat),
                },
                None,
            )
            .unwrap();
        }

        apply(&mut session, DraftAction::AdvanceRound, None).unwrap();
        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let result = apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: "r1-t0".to_string(),
                winner_seat: Some(0),
            },
            None,
        );

        assert!(matches!(
            result,
            Err(DraftError::PairingNotInCurrentRound { .. })
        ));
    }

    #[test]
    fn test_all_results_transitions_round_complete() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        apply(&mut session, DraftAction::GeneratePairings, None).unwrap();

        let results: Vec<(String, u8)> = session
            .pairings
            .iter()
            .filter(|p| p.round == 1)
            .map(|p| (p.match_id.clone(), p.players[0].0))
            .collect();

        for (match_id, winner_seat) in results {
            apply(
                &mut session,
                DraftAction::ReportMatchResult {
                    match_id,
                    winner_seat: Some(winner_seat),
                },
                None,
            )
            .unwrap();
        }

        assert_eq!(session.status, DraftStatus::RoundComplete);
    }

    #[test]
    fn test_advance_round_from_round_complete() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::RoundComplete;
        session.current_round = 1;

        let deltas = apply(&mut session, DraftAction::AdvanceRound, None).unwrap();

        assert_eq!(session.status, DraftStatus::Pairing);
        assert!(deltas.contains(&DraftDelta::RoundAdvanced { new_round: 2 }));
    }

    #[test]
    fn test_advance_round_wrong_status() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::MatchInProgress;

        let result = apply(&mut session, DraftAction::AdvanceRound, None);
        assert!(matches!(
            result,
            Err(DraftError::InvalidTransition {
                from: DraftStatus::MatchInProgress,
                ..
            })
        ));
    }

    #[test]
    fn test_replace_seat_with_bot() {
        let (mut session, _) = test_session(8);

        let deltas = apply(
            &mut session,
            DraftAction::ReplaceSeatWithBot {
                seat: 3,
                name: Some("Chandra".to_string()),
            },
            None,
        )
        .unwrap();

        assert!(deltas.contains(&DraftDelta::SeatReplacedWithBot { seat: 3 }));
        assert!(matches!(
            &session.seats[3],
            DraftSeat::Bot { name } if name == "Chandra"
        ));
    }

    #[test]
    fn test_replace_seat_out_of_range() {
        let (mut session, _) = test_session(8);

        let result = apply(
            &mut session,
            DraftAction::ReplaceSeatWithBot {
                seat: 10,
                name: None,
            },
            None,
        );
        assert!(matches!(
            result,
            Err(DraftError::SeatOutOfRange {
                seat: 10,
                pod_size: 8
            })
        ));
    }

    #[test]
    fn test_generate_pairings_wrong_status() {
        let (mut session, _) = test_session(8);
        // session is in Lobby status
        let result = apply(&mut session, DraftAction::GeneratePairings, None);
        assert!(matches!(
            result,
            Err(DraftError::InvalidTransition {
                from: DraftStatus::Lobby,
                ..
            })
        ));
    }

    #[test]
    fn test_report_result_pairing_not_found() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::MatchInProgress;

        let result = apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: "nonexistent".to_string(),
                winner_seat: Some(0),
            },
            None,
        );
        assert!(matches!(result, Err(DraftError::PairingNotFound { .. })));
    }

    // ── SetSeatConnected coverage ────────────────────────────────────────

    #[test]
    fn set_seat_connected_updates_state_and_emits_delta() {
        let (mut session, _) = test_session(4);

        let deltas = apply(
            &mut session,
            DraftAction::SetSeatConnected {
                seat: 1,
                connected: false,
            },
            None,
        )
        .unwrap();

        assert!(deltas.contains(&DraftDelta::SeatConnectionChanged {
            seat: 1,
            connected: false,
        }));
        assert!(!session.connected_seats.get(1));
        // The other seats remain connected (default true).
        assert!(session.connected_seats.get(0));
        assert!(session.connected_seats.get(2));

        // View now reflects the change.
        let view = crate::view::filter_for_player(&session, 0);
        assert!(!view.seats[1].connected);
        assert!(view.seats[0].connected);
    }

    #[test]
    fn set_seat_connected_out_of_range_errors() {
        let (mut session, _) = test_session(4);

        let result = apply(
            &mut session,
            DraftAction::SetSeatConnected {
                seat: 99,
                connected: false,
            },
            None,
        );
        assert!(matches!(
            result,
            Err(DraftError::SeatOutOfRange {
                seat: 99,
                pod_size: 4
            })
        ));
    }

    #[test]
    fn set_seat_connected_on_bot_seat_errors() {
        let (mut session, _) = test_session(4);
        session.seats[2] = DraftSeat::Bot {
            name: "TestBot".to_string(),
        };

        let result = apply(
            &mut session,
            DraftAction::SetSeatConnected {
                seat: 2,
                connected: false,
            },
            None,
        );
        assert!(matches!(result, Err(DraftError::SeatIsBot { seat: 2 })));
    }

    #[test]
    fn seat_flags_resize_preserves_existing_entries() {
        // SeatFlags::ensure_len uses Vec::resize semantics — existing entries
        // survive on grow, new slots default to the passed-in default.
        let mut flags = SeatFlags::all_false(2);
        flags.set(0, true);
        flags.ensure_len(4, true);

        assert!(flags.get(0)); // preserved
        assert!(!flags.get(1)); // preserved
        assert!(flags.get(2)); // new slot, default true
        assert!(flags.get(3)); // new slot, default true
    }

    #[test]
    fn swiss_bye_in_odd_pod_is_credited_a_match_win() {
        // Odd pod -> exactly one player takes a bye each round.
        let (mut session, _) = test_session(3);
        session.status = DraftStatus::Deckbuilding; // satisfy the pairing-generation guard
        apply_generate_pairings(&mut session).unwrap();

        // Three players: one two-player pairing plus one bye.
        assert_eq!(
            session.pairings.iter().filter(|p| p.round == 1).count(),
            1,
            "the two paired players get exactly one pairing",
        );
        let paired_players = session
            .pairings
            .iter()
            .find(|pairing| pairing.round == 1)
            .expect("round one pairing")
            .players;
        let bye = (0..session.seats.len() as u8)
            .map(|seat| seat_player_id(&session, seat))
            .find(|player| !paired_players.contains(player))
            .expect("one player is unpaired in a three-player pod");
        assert_eq!(
            session
                .match_records
                .get(&bye)
                .map(|record| record.match_wins),
            Some(1),
            "the specific unpaired player earns exactly one match win",
        );
        for paired_player in paired_players {
            assert_eq!(
                session
                    .match_records
                    .get(&paired_player)
                    .map_or(0, |record| record.match_wins),
                0,
                "a paired player must not receive the bye win",
            );
        }

        // With the round guard gone, a RoundComplete session generates the NEXT round.
        session.status = DraftStatus::RoundComplete;
        apply_generate_pairings(&mut session).expect("round two generates from RoundComplete");
        assert_eq!(
            session
                .pairings
                .iter()
                .filter(|pairing| pairing.round == 1)
                .count(),
            1,
            "generating round two must not append to round one",
        );
        assert_eq!(
            session
                .pairings
                .iter()
                .filter(|pairing| pairing.round == 2)
                .count(),
            1,
            "a three-player pod pairs exactly one table in round two",
        );
        assert_eq!(
            session
                .match_records
                .get(&bye)
                .map(|record| record.match_wins),
            Some(1),
            "the round-one bye is not re-credited by round-two generation",
        );
    }

    // ---------------------------------------------------------------------
    // CR 903.13e / CR 903.13f(3): the concession LATCH, and the reducer
    // wiring that consumes it.
    // ---------------------------------------------------------------------

    /// A Commander Draft session whose packs came from `set_code`.
    fn commander_draft_session(set_code: &str) -> DraftSession {
        let (mut session, _) = test_session(4);
        session.kind = DraftKind::CommanderDraft;
        session.config.kind = DraftKind::CommanderDraft;
        session.config.source = DraftSource::single_set(set_code);
        session
    }

    /// U7 row 10 -- set gating, asserted as a pair on one axis. Only the set
    /// code differs between the two halves, and the `Some` half is the reach
    /// guard: without it, "grants nothing" would be satisfied by a latch that
    /// hard-codes the default.
    #[test]
    fn commander_draft_latches_concessions_from_the_granting_set_only() {
        assert_eq!(
            session_concessions(&commander_draft_session("CMM")),
            draft_set_concessions("CMM"),
            "a CMM Commander Draft must concede exactly what CR 903.13e/f say CMM concedes"
        );
        assert!(
            !session_concessions(&commander_draft_session("CMM"))
                .fillers
                .is_empty(),
            "reach guard: CR 903.13e names Commander Masters as a granting set"
        );
        assert_eq!(
            session_concessions(&commander_draft_session("NEO")),
            DraftSetConcessions::default(),
            "CR 903.13e names no set outside its own list"
        );
    }

    /// Multi-set sources -- CR 903.13e/f condition each grant on what the draft
    /// CONTAINED, so a Commander Draft whose boosters came from several sets
    /// carries every grant those sets make.
    ///
    /// A repeated granting code is still one set, so it concedes exactly what
    /// the single-set draft does -- that half proves the latch reads set
    /// IDENTITY and not sequence length, and it is the shape a multi-set
    /// selection produces for an ordinary three-pack CMM draft.
    ///
    /// The mixed half is the rules-correct union: a CMM+CLB draft concedes The
    /// Prismatic Piper AND Faceless One, and keeps CR 903.13f(3)'s partner
    /// grant because it contained Commander Masters boosters. Both a latch that
    /// answers with pack 1's set (dropping CLB's filler) and one that refuses
    /// to answer when the sets disagree (dropping both) red here.
    ///
    /// Reachable: `create_multiplayer_draft` resolves its Commander pool input
    /// through `ResolvedSetSelection`, which builds `DraftSource::Set` from the
    /// host's whole pack sequence.
    #[test]
    fn a_mixed_set_commander_draft_concedes_every_contained_sets_grants() {
        let mut repeated = commander_draft_session("CMM");
        repeated.config.source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["CMM".to_string(), "cmm".to_string(), "CMM".to_string()],
            },
        };
        assert_eq!(
            session_concessions(&repeated),
            draft_set_concessions("CMM"),
            "one set named three times is still one set, casing included"
        );

        let mut mixed = commander_draft_session("CMM");
        mixed.config.source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["CMM".to_string(), "CLB".to_string(), "CMM".to_string()],
            },
        };
        assert_eq!(
            session_concessions(&mixed),
            draft_set_concessions_for(["CMM", "CLB"]),
            "CR 903.13e: the draft contained both sets, so both grants stand"
        );
        assert_eq!(
            session_concessions(&mixed).fillers.len(),
            2,
            "reach guard: CMM and CLB name DIFFERENT cards, so the union holds two"
        );
        assert_eq!(
            session_concessions(&mixed).partner_grant,
            draft_set_concessions("CMM").partner_grant,
            "CR 903.13f(3): the draft contained Commander Masters boosters"
        );
    }

    #[test]
    fn chaos_commander_concessions_use_assignments_not_unselected_candidates() {
        let mut session = commander_draft_session("CMM");
        session.config.source = DraftSource::Set {
            layout: SetLayout::Chaos {
                candidate_codes: vec!["CMM".to_string(), "CLB".to_string()],
                assignments: vec![
                    vec!["CLB".to_string(); 3],
                    vec!["CLB".to_string(); 3],
                    vec!["CLB".to_string(); 3],
                    vec!["CLB".to_string(); 3],
                ],
            },
        };

        assert_eq!(concession_set_codes(&session), vec!["CLB"]);
    }

    /// The latched set codes are what the draft CONTAINED -- distinct, and
    /// scoped to Commander Draft. This is the value `filter_for_player`
    /// publishes, so it is asserted at its own seam rather than only through
    /// the concessions it feeds.
    #[test]
    fn the_latched_set_codes_are_the_distinct_sets_a_commander_draft_contained() {
        let mut mixed = commander_draft_session("CMM");
        mixed.config.source = DraftSource::Set {
            layout: SetLayout::UniformByRound {
                codes: vec!["CMM".to_string(), "CLB".to_string(), "cmm".to_string()],
            },
        };
        assert_eq!(
            concession_set_codes(&mixed),
            vec!["CMM", "CLB"],
            "a set names its CR 903.13e condition once however many boosters it filled"
        );

        let mut cube = commander_draft_session("CMM");
        cube.config.source = DraftSource::Cube {
            id: "cube-1".to_string(),
            name: "Cube".to_string(),
        };
        assert!(
            concession_set_codes(&cube).is_empty(),
            "a cube contains no draft boosters from any set"
        );

        let mut premier = commander_draft_session("CMM");
        premier.kind = DraftKind::Premier;
        assert!(
            concession_set_codes(&premier).is_empty(),
            "CR 903.13 scopes both rules to Commander Draft"
        );
    }

    /// U7 row 11 -- wrong draft kind. CR 903.13e lives in CR 903.13, which
    /// scopes it to Commander Draft; the same set code under a Premier draft
    /// concedes nothing.
    #[test]
    fn a_premier_draft_of_a_granting_set_concedes_nothing() {
        let mut session = commander_draft_session("CMM");
        session.kind = DraftKind::Premier;
        session.config.kind = DraftKind::Premier;
        assert_eq!(
            session_concessions(&session),
            DraftSetConcessions::default()
        );
    }

    /// U7 row 12 -- cube source. A cube contains no draft boosters from any
    /// set, so it exercises the `None`-shaped arm of the latch's inner match.
    #[test]
    fn a_cube_commander_draft_concedes_nothing() {
        let mut session = commander_draft_session("CMM");
        session.config.source = DraftSource::Cube {
            id: "CMM".to_string(),
            name: "A cube that happens to be named for the set".to_string(),
        };
        assert_eq!(
            session_concessions(&session),
            DraftSetConcessions::default(),
            "the authority is what the DRAFT CONTAINED, not what the source is called"
        );
    }

    /// Seat a Commander Draft in deckbuilding with a `pool_size`-card pool for
    /// seat 0, none of which is the filler.
    fn deckbuilding_commander_draft(set_code: &str, pool_size: usize) -> DraftSession {
        let mut session = commander_draft_session(set_code);
        session.status = DraftStatus::Deckbuilding;
        session.config.min_deck_size = 60;
        session.pools[0] = (0..pool_size)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: set_code.to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();
        session
    }

    /// U7 row 13 -- end to end through the reducer. This is the assertion that
    /// proves the latch is WIRED into `apply_submit_deck` and not merely
    /// defined: the accept and the reject differ only in the filler count, and
    /// a latch that never reached the validator would accept both.
    #[test]
    fn submit_deck_applies_the_latched_filler_grant() {
        let filler = draft_set_concessions("CMM").fillers.remove(0);

        let deck_with = |copies: usize| {
            let mut deck: Vec<String> = (0..60 - copies).map(|i| format!("Card {i}")).collect();
            deck.extend(std::iter::repeat_n(filler.card_name.clone(), copies));
            deck
        };
        let designations = vec![filler.card_name.clone(), filler.card_name.clone()];

        // Two added copies, both designated -> accepted.
        let mut session = deckbuilding_commander_draft("CMM", 60);
        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: deck_with(2),
                commanders: designations.clone(),
            },
            None,
        )
        .unwrap();
        assert!(deltas.contains(&DraftDelta::DeckSubmitted { seat: 0 }));

        // Three -> rejected by CR 903.13e's cap.
        let mut session = deckbuilding_commander_draft("CMM", 60);
        let result = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: deck_with(3),
                commanders: designations,
            },
            None,
        );
        assert!(
            matches!(result, Err(DraftError::ValidationFailed { .. })),
            "expected ValidationFailed, got {result:?}"
        );
    }

    /// The designation is SNAPSHOTTED onto the submission record, not dropped
    /// at the reducer seam. Without this, everything above could pass while
    /// the P9 handoff received an empty list.
    #[test]
    fn submit_deck_snapshots_the_designation_onto_the_submission() {
        let filler = draft_set_concessions("CMM").fillers.remove(0);
        let mut session = deckbuilding_commander_draft("CMM", 60);
        let mut main_deck: Vec<String> = (0..59).map(|i| format!("Card {i}")).collect();
        main_deck.push(filler.card_name.clone());

        apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders: vec![filler.card_name.clone()],
            },
            None,
        )
        .unwrap();

        assert_eq!(
            session.submitted_decks[&PlayerId(0)].commanders,
            vec![filler.card_name],
            "the designation must survive the reducer, not be dropped at it"
        );
    }

    /// U9 row 8b -- the REDUCER bound (CR 702.124g), independent of any wire
    /// guard. This is the half that proves a payload arriving by a route the
    /// server never sees -- draft-wasm's local/P2P submit, a future transport
    /// -- is still bounded. Written off the constant, never the literal 3.
    #[test]
    fn submit_deck_rejects_more_than_max_commander_designations() {
        let mut session = deckbuilding_commander_draft("CMM", 60);
        let main_deck: Vec<String> = (0..60).map(|i| format!("Card {i}")).collect();
        let over_bound: Vec<String> = (0..=MAX_COMMANDER_DESIGNATIONS)
            .map(|i| format!("Card {i}"))
            .collect();

        let result = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: main_deck.clone(),
                commanders: over_bound,
            },
            None,
        );
        let Err(DraftError::ValidationFailed { errors }) = result else {
            panic!("expected ValidationFailed, got {result:?}");
        };
        assert!(
            errors.iter().any(|e| matches!(
                e,
                LimitedDeckError::TooManyCommanders { maximum, .. }
                    if *maximum == MAX_COMMANDER_DESIGNATIONS
            )),
            "expected TooManyCommanders, got {errors:?}"
        );

        // Paired positive reach-guard: exactly the bound is accepted, so the
        // rejection above cannot pass by the whole path being closed.
        let mut session = deckbuilding_commander_draft("CMM", 60);
        let at_bound: Vec<String> = (0..MAX_COMMANDER_DESIGNATIONS)
            .map(|i| format!("Card {i}"))
            .collect();
        assert!(apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders: at_bound,
            },
            None,
        )
        .is_ok());
    }

    /// U9 row 9 -- the WIRING of the CR 702.124h multiset guard: that
    /// `apply_submit_deck` surfaces it as `DraftError::ValidationFailed`
    /// rather than swallowing it. The RULE itself (that the comparison is a
    /// multiset and not a membership test) is asserted one layer down, in
    /// `validation.rs`, where `validate_limited_deck` is directly callable and
    /// the `(0,2,2)`/`(0,1,2)` pair isolates the single axis. Neither
    /// substitutes for the other: this test passes under a membership
    /// implementation, and that one passes with the guard unwired from here.
    #[test]
    fn submit_deck_rejects_a_designation_the_deck_does_not_contain() {
        let mut session = deckbuilding_commander_draft("CMM", 60);
        let main_deck: Vec<String> = (0..60).map(|i| format!("Card {i}")).collect();

        let result = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: main_deck.clone(),
                commanders: vec!["Card 999".to_string()],
            },
            None,
        );
        let Err(DraftError::ValidationFailed { errors }) = result else {
            panic!("expected ValidationFailed, got {result:?}");
        };
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, LimitedDeckError::CommanderNotInDeck { .. })),
            "expected CommanderNotInDeck, got {errors:?}"
        );

        // Paired positive: the same shape with a name the deck does contain.
        let mut session = deckbuilding_commander_draft("CMM", 60);
        assert!(apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders: vec!["Card 0".to_string()],
            },
            None,
        )
        .is_ok());
    }
    // -------------------------------------------------------------------
    // PF3 / U23 — CR 903.13a + CR 800.1: the kind's seat floor, enforced in
    // the reducer, which is the path every entry point actually takes.
    // -------------------------------------------------------------------

    /// A `CommanderDraft` session with `pod_size` seats, still in `Lobby`.
    fn commander_session(pod_size: u8) -> (DraftSession, FixturePackSource) {
        let (mut session, source) = test_session(pod_size);
        session.kind = DraftKind::CommanderDraft;
        session.config.kind = DraftKind::CommanderDraft;
        // CR 903.13f(1): the Commander Draft deck floor.
        session.config.min_deck_size = 60;
        (session, source)
    }

    /// VM row 1 — the floor is READ FROM THE PROCEDURE TABLE, not hard-coded.
    ///
    /// Revert the `seat_count < procedure.min_pod_size` guard in
    /// `apply_start_draft` and the 2-seat Commander pod below starts happily.
    #[test]
    fn start_draft_refuses_a_pod_below_the_kinds_seat_floor() {
        let (mut session, source) = commander_session(2);
        let result = apply(&mut session, DraftAction::StartDraft, Some(&source));

        assert!(
            matches!(result, Err(DraftError::PodBelowMinimumSize { .. })),
            "CR 903.13a + CR 800.1: a 2-seat Commander pod is below the floor: {result:?}"
        );
        assert_eq!(
            session.status,
            DraftStatus::Lobby,
            "the refusal must leave the session un-started"
        );

        // Paired positive reach-guard: AT the floor the same session starts.
        // Without this, a guard that refused every pod would pass the negative.
        let (mut session, source) = commander_session(3);
        apply(&mut session, DraftAction::StartDraft, Some(&source))
            .expect("a 3-seat Commander pod is exactly at its floor");
        assert_ne!(session.status, DraftStatus::Lobby);

        // Hostile sibling — THE KIND AXIS. Premier's floor is 2 and Swiss
        // admits 2..=8, so a floor written as a kind-blind `>= 3` reds here.
        // This is the fixture that distinguishes "reads the procedure" from
        // "hard-codes 3".
        let (mut session, source) = test_session(2);
        apply(&mut session, DraftAction::StartDraft, Some(&source))
            .expect("a 2-seat Premier pod is at ITS floor and must still start");
    }

    /// VM row 2 — the error names the KIND, and the two size guards COEXIST.
    ///
    /// Reverting to `UnsupportedTournamentSize` reds the first assertion:
    /// a `TournamentFormat` payload is exactly the kind-blindness this guard
    /// exists to remove.
    #[test]
    fn the_seat_floor_error_names_the_kind_not_a_tournament_format() {
        let (mut session, source) = commander_session(2);
        let err = apply(&mut session, DraftAction::StartDraft, Some(&source))
            .expect_err("below the floor");
        assert!(
            matches!(
                err,
                DraftError::PodBelowMinimumSize {
                    kind: DraftKind::CommanderDraft,
                    required: 3,
                    actual: 2,
                }
            ),
            "the error must carry the kind and both counts: {err:?}"
        );

        // Multi-authority: a 1-seat Premier pod violates BOTH the kind floor
        // (min_pod_size 2) and Swiss's 2..=8 bracket. This pins the guard
        // ORDER this phase chose — the floor is the more fundamental
        // precondition, and it is kind-general where the bracket is scoped to
        // `PostDraftPlay::TournamentPairings`.
        let (mut session, source) = test_session(1);
        let err = apply(&mut session, DraftAction::StartDraft, Some(&source))
            .expect_err("below Premier's floor");
        assert!(
            matches!(
                err,
                DraftError::PodBelowMinimumSize {
                    kind: DraftKind::Premier,
                    required: 2,
                    actual: 1,
                }
            ),
            "{err:?}"
        );

        // Sibling that must NOT change: 9 seats PASSES the floor and trips the
        // bracket. The two guards coexist rather than alternate, which is what
        // makes them two rules and not two spellings of one.
        let (mut session, source) = test_session(9);
        let err = apply(&mut session, DraftAction::StartDraft, Some(&source))
            .expect_err("above Swiss's bracket");
        assert!(
            matches!(
                err,
                DraftError::UnsupportedTournamentSize {
                    format: TournamentFormat::Swiss,
                    actual: 9,
                    ..
                }
            ),
            "a 9-seat Premier pod must still report the BRACKET rule: {err:?}"
        );
    }

    // -------------------------------------------------------------------
    // PF3 / U26 — CR 903.3's designation floor, on the PRODUCTION path.
    // -------------------------------------------------------------------

    /// A session of `kind` parked in `Deckbuilding` with seat 0's pool seeded.
    fn deckbuilding_session(kind: DraftKind) -> DraftSession {
        let (mut session, _) = test_session(4);
        session.kind = kind;
        session.config.kind = kind;
        session.config.min_deck_size = kind.procedure().min_deck_size;
        session.status = DraftStatus::Deckbuilding;
        session.pools[0] = (0..42)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: "TST".to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();
        session
    }

    /// A deck of exactly `min_deck_size` cards drawn from that seeded pool,
    /// padded with basic lands (available in unlimited quantity).
    fn pooled_deck(min_deck_size: usize) -> Vec<String> {
        let mut deck: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();
        deck.extend(std::iter::repeat_n(
            "Plains".to_string(),
            min_deck_size - 40,
        ));
        deck
    }

    /// VM row 9 — the PRODUCTION-PATH row for CR 903.3's floor.
    ///
    /// `validation.rs`'s rows enter at `validate_limited_deck` directly and so
    /// cannot see `session.rs`'s 7th argument at all: revert that argument to a
    /// literal `0` and every one of them stays green while real submissions
    /// silently accept an undesignated Commander deck. This row enters through
    /// `apply`, which is the entry real submissions use.
    #[test]
    fn apply_submit_deck_enforces_the_kinds_designation_floor() {
        let mut session = deckbuilding_session(DraftKind::CommanderDraft);
        let deck = pooled_deck(60);

        let err = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: deck.clone(),
                commanders: Vec::new(),
            },
            None,
        )
        .expect_err("CR 903.3: a Commander deck must designate a commander");
        let DraftError::ValidationFailed { errors } = &err else {
            panic!("expected ValidationFailed, got {err:?}");
        };
        assert!(
            errors.contains(&LimitedDeckError::TooFewCommanders {
                designated: 0,
                minimum: 1,
            }),
            "{errors:?}"
        );
        assert!(
            session.submitted_decks.is_empty(),
            "the refusal must land before `submitted_decks.insert`"
        );

        // Paired positive reach-guard: one BACKED designation is accepted and
        // reaches the insert. Without it, a reducer that refused every
        // Commander submission would satisfy the negative above.
        apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: deck,
                commanders: vec!["Card 0".to_string()],
            },
            None,
        )
        .expect("one backed designation satisfies the floor");
        assert_eq!(session.submitted_decks.len(), 1);
    }

    /// VM row 9's hostile siblings — the KIND axis and the CAP axis.
    #[test]
    fn the_designation_floor_is_the_kinds_and_does_not_displace_the_cap() {
        // KIND axis: Premier's `commanders_required` is 0, so an empty
        // designation must still be accepted. This is what proves the value is
        // read from `session.kind.procedure()` rather than hardcoded to 1.
        let mut session = deckbuilding_session(DraftKind::Premier);
        apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: pooled_deck(40),
                commanders: Vec::new(),
            },
            None,
        )
        .expect("CR 905.1a kinds designate no commander");
        assert_eq!(session.submitted_decks.len(), 1);

        // CAP axis: CR 702.124g's cap is raised by `apply_submit_deck`'s own
        // early `return`, upstream of the validator. Adding the floor must not
        // displace it.
        let mut session = deckbuilding_session(DraftKind::CommanderDraft);
        let err = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: pooled_deck(60),
                commanders: vec![
                    "Card 0".to_string(),
                    "Card 1".to_string(),
                    "Card 2".to_string(),
                ],
            },
            None,
        )
        .expect_err("CR 702.124g: at most two commanders");
        let DraftError::ValidationFailed { errors } = &err else {
            panic!("expected ValidationFailed, got {err:?}");
        };
        assert!(
            errors.contains(&LimitedDeckError::TooManyCommanders {
                designated: 3,
                maximum: MAX_COMMANDER_DESIGNATIONS,
            }),
            "{errors:?}"
        );
    }
}
