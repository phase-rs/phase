use engine::types::player::PlayerId;
use serde::{Deserialize, Serialize};

use crate::types::*;

/// A single entry in the standings table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StandingEntry {
    pub seat_index: u8,
    pub display_name: String,
    pub match_wins: u8,
    pub match_losses: u8,
    pub game_wins: u8,
    pub game_losses: u8,
}

/// A pairing visible to all players for the current round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingView {
    pub round: u8,
    pub table: u8,
    pub seat_a: u8,
    pub name_a: String,
    pub seat_b: u8,
    pub name_b: String,
    pub match_id: String,
    pub status: PairingStatus,
    pub winner_seat: Option<u8>,
    /// Game wins for seat A in the current match (Bo3 tracking).
    pub score_a: Option<u8>,
    /// Game wins for seat B in the current match (Bo3 tracking).
    pub score_b: Option<u8>,
}

/// Public seat info visible to all players.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatPublicView {
    pub seat_index: u8,
    pub display_name: String,
    pub is_bot: bool,
    pub connected: bool,
    pub has_submitted_deck: bool,
    pub pick_status: PickStatus,
}

/// Filtered draft state for a specific player. Built from scratch (not a reference
/// into DraftSession) to prevent accidental hidden state leakage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftPlayerView {
    /// Current draft status
    pub status: DraftStatus,
    /// Draft kind (Quick/Premier/Traditional)
    pub kind: DraftKind,
    /// Which pack round (0, 1, 2)
    pub current_pack_number: u8,
    /// Which pick within the current pack
    pub pick_number: u8,
    /// Current pass direction
    pub pass_direction: PassDirection,
    /// The viewer's current pack (None if between packs or not their turn)
    pub current_pack: Option<Vec<DraftCardInstance>>,
    /// The viewer's drafted pool
    pub pool: Vec<DraftCardInstance>,
    /// Public info for all seats
    pub seats: Vec<SeatPublicView>,
    /// Total cards per pack (for UI progress display)
    pub cards_per_pack: u8,
    /// Total pack count (for UI progress display)
    pub pack_count: u8,
    /// Milliseconds remaining on the pick timer. Always None from the reducer;
    /// the P2P host injects the authoritative value on the wire.
    pub timer_remaining_ms: Option<u32>,
    /// Tournament standings, sorted by match_wins descending. Empty before pairings.
    pub standings: Vec<StandingEntry>,
    /// Current tournament round (0 = not started).
    pub current_round: u8,
    /// Tournament format from config.
    pub tournament_format: TournamentFormat,
    /// Pod policy from config.
    pub pod_policy: PodPolicy,
    /// Pairings for the current round.
    pub pairings: Vec<PairingView>,
}

/// Re-export SpectatorVisibility from types for convenience.
pub use crate::types::SpectatorVisibility;

/// Filtered view for spectators watching a draft.
///
/// Public mode hides all private information (pools, packs).
/// Omniscient mode exposes all pools and current packs for all seats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorDraftView {
    pub status: DraftStatus,
    pub kind: DraftKind,
    pub current_pack_number: u8,
    pub pick_number: u8,
    pub pass_direction: PassDirection,
    pub seats: Vec<SeatPublicView>,
    pub cards_per_pack: u8,
    pub pack_count: u8,
    pub standings: Vec<StandingEntry>,
    pub current_round: u8,
    pub tournament_format: TournamentFormat,
    pub pod_policy: PodPolicy,
    pub pairings: Vec<PairingView>,
    /// Populated only in `Omniscient` mode. Each inner Vec is a seat's pool.
    pub pools: Option<Vec<Vec<DraftCardInstance>>>,
    /// Populated only in `Omniscient` mode. Each entry is a seat's current pack.
    pub current_packs: Option<Vec<Option<Vec<DraftCardInstance>>>>,
}

/// Generate a spectator view of the draft session.
///
/// Visibility is read from session.config.spectator_visibility (set by host at creation).
/// Public mode hides all private information (pools, packs).
/// Omniscient mode exposes all pools and current packs for all seats.
pub fn filter_for_spectator(
    session: &DraftSession,
    visibility: SpectatorVisibility,
) -> SpectatorDraftView {
    let is_drafting = session.status == DraftStatus::Drafting;

    let seats = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let player_id_for_seat = match seat {
                DraftSeat::Human { player_id, .. } => Some(*player_id),
                DraftSeat::Bot { .. } => None,
            };

            let pick_status = if !is_drafting {
                PickStatus::NotDrafting
            } else if session.current_pack[i].is_some() {
                PickStatus::Pending
            } else {
                PickStatus::Picked
            };

            SeatPublicView {
                seat_index: i as u8,
                display_name: match seat {
                    DraftSeat::Human { display_name, .. } => display_name.clone(),
                    DraftSeat::Bot { name, .. } => name.clone(),
                },
                is_bot: matches!(seat, DraftSeat::Bot { .. }),
                connected: match seat {
                    DraftSeat::Human { connected, .. } => *connected,
                    DraftSeat::Bot { .. } => true,
                },
                has_submitted_deck: player_id_for_seat
                    .map(|pid| session.submitted_decks.contains_key(&pid))
                    .unwrap_or(false),
                pick_status,
            }
        })
        .collect();

    let standings = compute_standings(session);
    let pairings = compute_pairing_views(session);

    let (pools, current_packs) = match visibility {
        SpectatorVisibility::Public => (None, None),
        SpectatorVisibility::Omniscient => {
            let pools = Some(session.pools.clone());
            let packs = Some(
                session
                    .current_pack
                    .iter()
                    .map(|p| p.as_ref().map(|pack| pack.0.clone()))
                    .collect(),
            );
            (pools, packs)
        }
    };

    SpectatorDraftView {
        status: session.status,
        kind: session.kind,
        current_pack_number: session.current_pack_number,
        pick_number: session.pick_number,
        pass_direction: session.pass_direction,
        seats,
        cards_per_pack: session.config.cards_per_pack,
        pack_count: session.config.pack_count,
        standings,
        current_round: session.current_round,
        tournament_format: session.config.tournament_format,
        pod_policy: session.config.pod_policy,
        pairings,
        pools,
        current_packs,
    }
}

/// Produce a filtered view of the draft session for a specific seat.
///
/// The viewer sees:
/// - Their own current pack and pool
/// - Public draft status, kind, pack/pick numbers, direction
/// - Public seat info (names, connected status, submission status)
///
/// The viewer does NOT see:
/// - Other players' packs or pools
/// - RNG seed
/// - Bot rankings or archetypes
/// - Unopened packs (packs_by_seat)
/// - Other players' deck submissions
pub fn filter_for_player(session: &DraftSession, seat_index: u8) -> DraftPlayerView {
    let idx = seat_index as usize;

    let current_pack = session
        .current_pack
        .get(idx)
        .and_then(|p| p.as_ref())
        .map(|p| p.0.clone());

    let pool = session.pools.get(idx).cloned().unwrap_or_default();

    let is_drafting = session.status == DraftStatus::Drafting;

    let seats = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let player_id_for_seat = match seat {
                DraftSeat::Human { player_id, .. } => Some(*player_id),
                DraftSeat::Bot { .. } => None,
            };

            let pick_status = if !is_drafting {
                PickStatus::NotDrafting
            } else if session.current_pack[i].is_some() {
                PickStatus::Pending
            } else {
                PickStatus::Picked
            };

            SeatPublicView {
                seat_index: i as u8,
                display_name: match seat {
                    DraftSeat::Human { display_name, .. } => display_name.clone(),
                    DraftSeat::Bot { name, .. } => name.clone(),
                },
                is_bot: matches!(seat, DraftSeat::Bot { .. }),
                connected: match seat {
                    DraftSeat::Human { connected, .. } => *connected,
                    DraftSeat::Bot { .. } => true,
                },
                has_submitted_deck: player_id_for_seat
                    .map(|pid| session.submitted_decks.contains_key(&pid))
                    .unwrap_or(false),
                pick_status,
            }
        })
        .collect();

    // Compute standings from match records
    let standings = compute_standings(session);

    // Compute pairings for the current round
    let pairings = compute_pairing_views(session);

    DraftPlayerView {
        status: session.status,
        kind: session.kind,
        current_pack_number: session.current_pack_number,
        pick_number: session.pick_number,
        pass_direction: session.pass_direction,
        current_pack,
        pool,
        seats,
        cards_per_pack: session.config.cards_per_pack,
        pack_count: session.config.pack_count,
        timer_remaining_ms: None,
        standings,
        current_round: session.current_round,
        tournament_format: session.config.tournament_format,
        pod_policy: session.config.pod_policy,
        pairings,
    }
}

fn compute_standings(session: &DraftSession) -> Vec<StandingEntry> {
    if session.pairings.is_empty() {
        return Vec::new();
    }

    let mut entries: Vec<StandingEntry> = session
        .seats
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, DraftSeat::Human { .. }))
        .map(|(i, seat)| {
            let pid = match seat {
                DraftSeat::Human { player_id, .. } => *player_id,
                DraftSeat::Bot { .. } => unreachable!(),
            };
            let record = session.match_records.get(&pid);
            StandingEntry {
                seat_index: i as u8,
                display_name: match seat {
                    DraftSeat::Human { display_name, .. } => display_name.clone(),
                    DraftSeat::Bot { .. } => unreachable!(),
                },
                match_wins: record.map_or(0, |r| r.match_wins),
                match_losses: record.map_or(0, |r| r.match_losses),
                game_wins: record.map_or(0, |r| r.wins),
                game_losses: record.map_or(0, |r| r.losses),
            }
        })
        .collect();

    entries.sort_by_key(|e| std::cmp::Reverse(e.match_wins));
    entries
}

fn compute_pairing_views(session: &DraftSession) -> Vec<PairingView> {
    let current_round = session.current_round;
    if current_round == 0 {
        return Vec::new();
    }

    // Build a PlayerId -> (seat_index, name) lookup
    let player_seat_map: std::collections::HashMap<PlayerId, (u8, String)> = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let (pid, name) = match seat {
                DraftSeat::Human {
                    player_id,
                    display_name,
                    ..
                } => (*player_id, display_name.clone()),
                DraftSeat::Bot { name } => (PlayerId(i as u8), name.clone()),
            };
            (pid, (i as u8, name))
        })
        .collect();

    session
        .pairings
        .iter()
        .filter(|p| p.round == current_round)
        .map(|p| {
            let (seat_a, name_a) = player_seat_map
                .get(&p.players[0])
                .cloned()
                .unwrap_or((0, "Unknown".to_string()));
            let (seat_b, name_b) = player_seat_map
                .get(&p.players[1])
                .cloned()
                .unwrap_or((0, "Unknown".to_string()));

            // Determine winner seat from the match status + records
            let winner_seat = if p.status == PairingStatus::Complete {
                let r0 = session.match_records.get(&p.players[0]);
                let r1 = session.match_records.get(&p.players[1]);
                let w0 = r0.map_or(0, |r| r.match_wins);
                let w1 = r1.map_or(0, |r| r.match_wins);
                if w0 > w1 {
                    Some(seat_a)
                } else if w1 > w0 {
                    Some(seat_b)
                } else {
                    None // draw or equal
                }
            } else {
                None
            };

            PairingView {
                round: p.round,
                table: p.table,
                seat_a,
                name_a,
                seat_b,
                name_b,
                match_id: p.match_id.clone(),
                status: p.status,
                winner_seat,
                score_a: None,
                score_b: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_source::FixturePackSource;
    use crate::session;

    use engine::types::player::PlayerId;

    fn test_session(pod_size: u8) -> (DraftSession, FixturePackSource) {
        let config = DraftConfig {
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            cards_per_pack: 14,
            pack_count: 3,
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
        let s = DraftSession::new(config, seats, "TEST-001".to_string());
        (s, source)
    }

    fn start_and_pick(session: &mut DraftSession, source: &FixturePackSource) {
        session::apply(session, DraftAction::StartDraft, Some(source)).unwrap();
        // Make a pick for seat 0 so they have something in their pool
        let card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: card_id,
            },
            None,
        )
        .unwrap();
    }

    #[test]
    fn view_contains_viewers_current_pack() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let pack = view.current_pack.unwrap();
        assert_eq!(pack.len(), 14);

        // Verify it matches the actual session data
        let actual_pack = &session.current_pack[0].as_ref().unwrap().0;
        for (i, card) in pack.iter().enumerate() {
            assert_eq!(card.instance_id, actual_pack[i].instance_id);
        }
    }

    #[test]
    fn view_contains_viewers_pool() {
        let (mut session, source) = test_session(8);
        start_and_pick(&mut session, &source);

        let view = filter_for_player(&session, 0);
        assert_eq!(view.pool.len(), 1);
        assert_eq!(view.pool[0].instance_id, session.pools[0][0].instance_id);
    }

    #[test]
    fn view_contains_public_status_fields() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        assert_eq!(view.status, DraftStatus::Drafting);
        assert_eq!(view.kind, DraftKind::Premier);
        assert_eq!(view.current_pack_number, 0);
        assert_eq!(view.pick_number, 0);
        assert_eq!(view.pass_direction, PassDirection::Left);
        assert_eq!(view.cards_per_pack, 14);
        assert_eq!(view.pack_count, 3);
    }

    #[test]
    fn view_does_not_contain_other_players_packs() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // Check that no other seat's card instance IDs appear in the view
        for seat in 1..8u8 {
            let other_pack = session.current_pack[seat as usize].as_ref().unwrap();
            for card in &other_pack.0 {
                assert!(
                    !json.contains(&card.instance_id),
                    "view for seat 0 leaks seat {seat}'s card {}",
                    card.instance_id
                );
            }
        }
    }

    #[test]
    fn view_does_not_contain_other_players_pools() {
        let (mut session, source) = test_session(8);
        start_and_pick(&mut session, &source);

        // Make a pick for seat 1 too
        let card_id = session.current_pack[1].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 1,
                card_instance_id: card_id,
            },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // Seat 1's pool card should not appear
        for card in &session.pools[1] {
            assert!(
                !json.contains(&card.instance_id),
                "view for seat 0 leaks seat 1's pool card {}",
                card.instance_id
            );
        }
    }

    #[test]
    fn view_does_not_contain_rng_seed() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // The seed is 42, check it doesn't appear as "rng_seed" anywhere
        assert!(
            !json.contains("rng_seed"),
            "view should not contain rng_seed field"
        );
    }

    #[test]
    fn view_shows_seat_public_info() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        assert_eq!(view.seats.len(), 8);

        for (i, seat_view) in view.seats.iter().enumerate() {
            assert_eq!(seat_view.seat_index, i as u8);
            assert_eq!(seat_view.display_name, format!("Player {i}"));
            assert!(!seat_view.is_bot);
            assert!(seat_view.connected);
            assert!(!seat_view.has_submitted_deck);
        }
    }

    #[test]
    fn view_shows_submission_status_without_deck_contents() {
        let (mut session, _) = test_session(2);
        session.status = DraftStatus::Deckbuilding;

        // Give seat 0 a pool
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
        session.pools[1] = session.pools[0].clone();

        // Seat 0 submits a deck
        let mut main_deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        main_deck.extend(std::iter::repeat_n("Plains".to_string(), 17));

        session::apply(
            &mut session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: main_deck.clone(),
            },
            None,
        )
        .unwrap();

        // View from seat 1 should show seat 0 has submitted
        let view = filter_for_player(&session, 1);
        assert!(view.seats[0].has_submitted_deck);
        assert!(!view.seats[1].has_submitted_deck);

        // But the view should not contain the deck card names as a "main_deck" field
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            !json.contains("main_deck"),
            "view should not contain submitted deck contents"
        );
    }

    #[test]
    fn view_does_not_contain_unopened_packs() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_player(&session, 0);
        let json = serde_json::to_string(&view).unwrap();

        // packs_by_seat should not appear in the view
        assert!(
            !json.contains("packs_by_seat"),
            "view should not contain unopened packs"
        );

        // Verify unopened packs exist in the session but not in the view
        assert!(!session.packs_by_seat[0].is_empty());
    }

    #[test]
    fn view_bot_seat_shows_as_bot() {
        let config = DraftConfig {
            set_code: "TST".to_string(),
            kind: DraftKind::Quick,
            cards_per_pack: 14,
            pack_count: 3,
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let mut seats = vec![DraftSeat::Human {
            player_id: PlayerId(0),
            display_name: "Human".to_string(),
            connected: true,
        }];
        for i in 1..8u8 {
            seats.push(DraftSeat::Bot {
                name: format!("Bot {i}"),
            });
        }
        let session = DraftSession::new(config, seats, "BOT-TEST".to_string());

        let view = filter_for_player(&session, 0);
        assert!(!view.seats[0].is_bot);
        assert!(view.seats[0].connected);
        for i in 1..8 {
            assert!(view.seats[i].is_bot);
            assert!(view.seats[i].connected); // bots always connected
        }
    }

    #[test]
    fn view_pick_status_during_drafting() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        // During drafting, all seats with packs show as Pending
        let view = filter_for_player(&session, 0);
        for seat in &view.seats {
            assert_eq!(seat.pick_status, PickStatus::Pending);
        }

        // After seat 0 picks, the pack still exists (with one fewer card).
        // Picks only resolve when ALL seats pick, so individual pick status
        // during a round is tracked by the P2P host, not the session reducer.
        let card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: card_id,
            },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        // Seat 0 still has a current_pack (13 cards remain), so shows as Pending
        assert_eq!(view.seats[0].pick_status, PickStatus::Pending);
    }

    #[test]
    fn view_pick_status_not_drafting() {
        let (session, _) = test_session(8);
        // Lobby status
        let view = filter_for_player(&session, 0);
        for seat in &view.seats {
            assert_eq!(seat.pick_status, PickStatus::NotDrafting);
        }
    }

    #[test]
    fn view_standings_after_pairings() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        // Generate pairings
        session::apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

        // Report seat 0 wins
        session::apply(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: "r1-t0".to_string(),
                winner_seat: Some(0),
            },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        assert!(!view.standings.is_empty());

        // Player 0 should have match_wins = 1
        let p0_standing = view.standings.iter().find(|s| s.seat_index == 0).unwrap();
        assert_eq!(p0_standing.match_wins, 1);
        assert_eq!(p0_standing.match_losses, 0);

        // Standings should be sorted by match_wins descending
        for window in view.standings.windows(2) {
            assert!(window[0].match_wins >= window[1].match_wins);
        }
    }

    #[test]
    fn view_standings_empty_before_pairings() {
        let (session, _) = test_session(8);
        let view = filter_for_player(&session, 0);
        assert!(view.standings.is_empty());
    }

    #[test]
    fn view_has_config_fields() {
        let (session, _) = test_session(8);
        let view = filter_for_player(&session, 0);
        assert_eq!(view.tournament_format, TournamentFormat::Swiss);
        assert_eq!(view.pod_policy, PodPolicy::Competitive);
        assert_eq!(view.current_round, 0);
        assert!(view.timer_remaining_ms.is_none());
    }

    #[test]
    fn view_pairings_for_current_round() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        session::apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

        let view = filter_for_player(&session, 0);
        assert_eq!(view.pairings.len(), 4);
        for pv in &view.pairings {
            assert_eq!(pv.round, 1);
            assert_eq!(pv.status, PairingStatus::Pending);
            assert!(pv.winner_seat.is_none());
        }
    }

    #[test]
    fn pairing_view_score_fields_default_to_none() {
        // BO3-06: PairingView score_a/score_b are None when match not started.
        // This test deliberately references score_a/score_b to create a compile
        // error until Plan 01 adds these fields to PairingView.
        let view = PairingView {
            round: 1,
            table: 1,
            seat_a: 0,
            name_a: "Alice".to_string(),
            seat_b: 1,
            name_b: "Bob".to_string(),
            match_id: "m1".to_string(),
            status: PairingStatus::Pending,
            winner_seat: None,
            score_a: None, // Compile-fails until Plan 01 adds this field
            score_b: None, // Compile-fails until Plan 01 adds this field
        };
        assert_eq!(view.score_a, None);
        assert_eq!(view.score_b, None);
    }

    #[test]
    fn spectator_public_view_hides_pools_and_packs() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_spectator(&session, SpectatorVisibility::Public);
        assert!(view.pools.is_none());
        assert!(view.current_packs.is_none());
        assert_eq!(view.seats.len(), 8);
        assert_eq!(view.status, DraftStatus::Drafting);
        assert_eq!(view.kind, DraftKind::Premier);
    }

    #[test]
    fn spectator_omniscient_view_exposes_all_pools() {
        let (mut session, source) = test_session(8);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let view = filter_for_spectator(&session, SpectatorVisibility::Omniscient);
        assert!(view.pools.is_some());
        assert_eq!(view.pools.as_ref().unwrap().len(), 8);
        assert!(view.current_packs.is_some());
        assert_eq!(view.current_packs.as_ref().unwrap().len(), 8);
        // All seats should have a current pack during drafting
        for pack in view.current_packs.as_ref().unwrap() {
            assert!(pack.is_some());
        }
    }

    #[test]
    fn spectator_public_view_has_standings_and_pairings() {
        let (mut session, _) = test_session(8);
        session.status = DraftStatus::Deckbuilding;

        session::apply(
            &mut session,
            DraftAction::GeneratePairings { round: 1 },
            None,
        )
        .unwrap();

        let view = filter_for_spectator(&session, SpectatorVisibility::Public);
        assert_eq!(view.pairings.len(), 4);
        assert!(view.pools.is_none());
    }
}
