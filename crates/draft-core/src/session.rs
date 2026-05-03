use std::collections::HashMap;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use engine::types::player::PlayerId;

use crate::pack_source::PackSource;
use crate::pick_pass;
use crate::types::*;
use crate::validation::{validate_limited_deck, STANDARD_BASIC_LANDS};

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
        DraftAction::SubmitDeck { seat, main_deck } => {
            apply_submit_deck(session, seat, main_deck)
        }
        DraftAction::GeneratePairings { round } => {
            apply_generate_pairings(session, round)
        }
        DraftAction::ReportMatchResult {
            match_id,
            winner_seat,
        } => apply_report_match_result(session, match_id, winner_seat),
        DraftAction::AdvanceRound => apply_advance_round(session),
        DraftAction::ReplaceSeatWithBot { seat } => {
            apply_replace_seat_with_bot(session, seat)
        }
    }
}

fn apply_generate_pairings(
    _session: &mut DraftSession,
    _round: u8,
) -> Result<Vec<DraftDelta>, DraftError> {
    // Implemented in Task 2
    todo!("apply_generate_pairings")
}

fn apply_report_match_result(
    _session: &mut DraftSession,
    _match_id: String,
    _winner_seat: Option<u8>,
) -> Result<Vec<DraftDelta>, DraftError> {
    // Implemented in Task 2
    todo!("apply_report_match_result")
}

fn apply_advance_round(
    _session: &mut DraftSession,
) -> Result<Vec<DraftDelta>, DraftError> {
    // Implemented in Task 2
    todo!("apply_advance_round")
}

fn apply_replace_seat_with_bot(
    _session: &mut DraftSession,
    _seat: u8,
) -> Result<Vec<DraftDelta>, DraftError> {
    // Implemented in Task 2
    todo!("apply_replace_seat_with_bot")
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

    // Generate all packs for all seats
    for seat in 0..pod_size {
        let mut seat_packs = Vec::new();
        for pack_num in 0..session.config.pack_count {
            seat_packs.push(pack_source.generate_pack(&mut rng, seat, pack_num));
        }
        // First pack goes to current_pack, rest go to packs_by_seat
        session.current_pack[seat as usize] = Some(seat_packs.remove(0));
        session.packs_by_seat[seat as usize] = seat_packs;
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

    if let Err(errors) = validate_limited_deck(&main_deck, &pool_names, STANDARD_BASIC_LANDS, 40)
    {
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
        session.status = DraftStatus::Complete;
        deltas.push(DraftDelta::TransitionedTo {
            status: DraftStatus::Complete,
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
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            cards_per_pack: 14,
            pack_count: 3,
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
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
            Err(DraftError::InvalidTransition { from: DraftStatus::Drafting, .. })
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
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
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
            })
            .collect();

        let main_deck: Vec<String> = (0..10).map(|i| format!("Card {i}")).collect();
        let result = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
            },
            None,
        );

        assert!(matches!(result, Err(DraftError::ValidationFailed { .. })));
    }

    #[test]
    fn submit_deck_all_submitted_transitions_to_complete() {
        let (mut session, _) = test_session(2);
        session.status = DraftStatus::Deckbuilding;

        // Give both seats pools
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
        let deltas = apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: make_deck(),
            },
            None,
        )
        .unwrap();
        assert!(!deltas.contains(&DraftDelta::TransitionedTo {
            status: DraftStatus::Complete,
        }));

        // Seat 1 submits -- should transition to Complete
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
            status: DraftStatus::Complete,
        }));
        assert_eq!(session.status, DraftStatus::Complete);
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
            Err(DraftError::InvalidTransition { from: DraftStatus::Lobby, .. })
        ));
    }
}
