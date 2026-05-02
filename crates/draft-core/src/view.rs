use serde::{Deserialize, Serialize};

use crate::types::*;

/// Public seat info visible to all players.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatPublicView {
    pub seat_index: u8,
    pub display_name: String,
    pub is_bot: bool,
    pub connected: bool,
    pub has_submitted_deck: bool,
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

    let current_pack = session.current_pack.get(idx).and_then(|p| p.as_ref()).map(|p| p.0.clone());

    let pool = session.pools.get(idx).cloned().unwrap_or_default();

    let seats = session
        .seats
        .iter()
        .enumerate()
        .map(|(i, seat)| {
            let player_id_for_seat = match seat {
                DraftSeat::Human { player_id, .. } => Some(*player_id),
                DraftSeat::Bot { .. } => None,
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
            }
        })
        .collect();

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
    }
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
        let card_id = session.current_pack[0]
            .as_ref()
            .unwrap()
            .0[0]
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
        let card_id = session.current_pack[1]
            .as_ref()
            .unwrap()
            .0[0]
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
}
