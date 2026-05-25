use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use engine::types::player::PlayerId;

use crate::pack_source::PackSource;
use crate::pick_pass;
use crate::types::*;
use crate::validation::validate_limited_deck;

impl DraftSession {
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
            pick_number: 0,
            picks_this_round: 0,
            packs_by_seat: vec![vec![]; pod_size],
            current_pack: vec![None; pod_size],
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
            card_instance_id,
        } => pick_pass::apply_pick(session, seat, card_instance_id),
        DraftAction::SubmitDeck { seat, main_deck } => apply_submit_deck(session, seat, main_deck),
        DraftAction::GeneratePairings { round } => apply_generate_pairings(session, round),
        DraftAction::ReportMatchResult {
            match_id,
            winner_seat,
        } => apply_report_match_result(session, match_id, winner_seat),
        DraftAction::AdvanceRound => apply_advance_round(session),
        DraftAction::ReplaceSeatWithBot { seat, name } => {
            apply_replace_seat_with_bot(session, seat, name)
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

fn apply_generate_pairings(
    session: &mut DraftSession,
    round: u8,
) -> Result<Vec<DraftDelta>, DraftError> {
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
    if session.config.tournament_format == TournamentFormat::SingleElimination
        && session.seats.len() != 8
    {
        return Err(DraftError::UnsupportedTournamentSize {
            format: TournamentFormat::SingleElimination,
            required: 8,
            actual: session.seats.len() as u8,
        });
    }

    let mut rng =
        ChaCha20Rng::seed_from_u64(session.config.rng_seed ^ (round as u64 * 0xDEAD_BEEF));

    let new_pairings = match session.config.tournament_format {
        TournamentFormat::Swiss => generate_swiss_pairings(session, round, &mut rng),
        TournamentFormat::SingleElimination => generate_se_pairings(session, round),
    };

    for p in &new_pairings {
        session.pairings.push(p.clone());
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

fn generate_swiss_pairings(
    session: &DraftSession,
    round: u8,
    rng: &mut ChaCha20Rng,
) -> Vec<DraftPairing> {
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

    // Greedy pair within brackets, carrying unpaired to next bracket
    let mut paired: Vec<(PlayerId, PlayerId)> = Vec::new();
    let mut carry: Option<(PlayerId, u8)> = None;

    for bracket in &brackets {
        let mut pool: Vec<(PlayerId, u8)> = bracket.clone();
        if let Some(c) = carry.take() {
            pool.insert(0, c);
        }

        while pool.len() >= 2 {
            let first = pool.remove(0);
            // Try to find a non-rematch partner
            let partner_idx = pool
                .iter()
                .position(|(pid, _)| !prior_pairs.contains(&(first.0, *pid)))
                .unwrap_or(0);
            let partner = pool.remove(partner_idx);
            paired.push((first.0, partner.0));
        }

        if pool.len() == 1 {
            carry = Some(pool[0]);
        }
    }

    // If there's still an unpaired player (odd count), they get a bye (no pairing generated)
    // For 8-player pods this shouldn't happen.

    // Generate DraftPairing structs
    paired
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
        .collect()
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

    let new_round = session.current_round + 1;
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

    let pack_source = pack_source.expect("StartDraft requires a PackSource");
    let pod_size = session.seats.len() as u8;
    let mut rng = ChaCha20Rng::seed_from_u64(session.config.rng_seed);

    let all_packs = pack_source.generate_packs(&mut rng, &session.config, pod_size)?;
    for (seat, mut seat_packs) in all_packs.into_iter().enumerate() {
        // First pack goes to current_pack, rest go to packs_by_seat
        session.current_pack[seat] = Some(seat_packs.remove(0));
        session.packs_by_seat[seat] = seat_packs;
    }

    session.status = DraftStatus::Drafting;
    session.pass_direction = PassDirection::for_pack(0);
    session.current_pack_number = 0;
    session.pick_number = 0;
    session.picks_this_round = 0;

    Ok(vec![DraftDelta::DraftStarted])
}

fn apply_submit_deck(
    session: &mut DraftSession,
    seat: u8,
    main_deck: Vec<String>,
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

    // Collect pool card names for validation
    let pool_names: Vec<String> = session.pools[seat as usize]
        .iter()
        .map(|c| c.name.clone())
        .collect();

    if let Err(errors) = validate_limited_deck(
        &main_deck,
        &pool_names,
        &session.config.addable_cards,
        session.config.min_deck_size,
    ) {
        return Err(DraftError::ValidationFailed { errors });
    }

    // Find the PlayerId for this seat
    let player_id = match &session.seats[seat as usize] {
        DraftSeat::Human { player_id, .. } => *player_id,
        DraftSeat::Bot { .. } => PlayerId(seat),
    };

    session
        .submitted_decks
        .insert(player_id, DraftDeckSubmission { seat, main_deck });

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
        // Premier/Traditional drafts transition to Pairing for tournament play.
        // Quick Draft (1 human) completes directly.
        let next_status = match session.kind {
            DraftKind::Quick => DraftStatus::Complete,
            DraftKind::Premier | DraftKind::Traditional => DraftStatus::Pairing,
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

    fn test_session(pod_size: u8) -> (DraftSession, FixturePackSource) {
        let config = DraftConfig {
            source: DraftSource::Set {
                code: "TST".to_string(),
            },
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
                connected: true,
            })
            .collect();
        let source = FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let session = DraftSession::new(config, seats, "TEST-001".to_string());
        (session, source)
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
            })
            .collect();

        let mut main_deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 17));

        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck { seat: 0, main_deck },
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
            })
            .collect();

        let main_deck: Vec<String> = (0..10).map(|i| format!("Card {i}")).collect();
        let result = apply(
            &mut session,
            DraftAction::SubmitDeck { seat: 0, main_deck },
            None,
        );

        assert!(matches!(result, Err(DraftError::ValidationFailed { .. })));
    }

    #[test]
    fn submit_deck_all_submitted_quick_draft_transitions_to_complete() {
        let config = DraftConfig {
            source: DraftSource::Set {
                code: "TST".to_string(),
            },
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
                connected: true,
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
            })
            .collect();

        let mut main_deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 17));

        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck { seat: 0, main_deck },
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
            },
            None,
        )
        .unwrap();
        assert!(deltas.contains(&DraftDelta::TransitionedTo {
            status: DraftStatus::Pairing,
        }));
        assert_eq!(session.status, DraftStatus::Pairing);
    }

    #[test]
    fn submit_deck_on_non_deckbuilding_returns_error() {
        let (mut session, _) = test_session(8);
        let result = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec![],
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

        let deltas = apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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
        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();
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

        let result = apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        );

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
    fn test_swiss_rematch_avoidance() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        // Generate round 1
        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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
        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 2 },
            None,
        )
        .unwrap();

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
    fn test_se_bracket_8_players() {
        let config = DraftConfig {
            source: DraftSource::Set {
                code: "TST".to_string(),
            },
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
                connected: true,
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "SE-TEST".to_string());
        session.status = DraftStatus::Deckbuilding;

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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
            source: DraftSource::Set {
                code: "TST".to_string(),
            },
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
                connected: true,
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "SE-TEST".to_string());
        session.status = DraftStatus::Deckbuilding;

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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
        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 2 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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
        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 2 },
            None,
        )
        .unwrap();

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

        apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

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
        let result = apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
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
}
