//! Integration test harness for full draft tournament simulation.
//! Runs at server-core crate level (D-06) -- no network, no tokio runtime needed.

use crate::draft_session::{DraftActor, DraftSessionManager, FifoStaged};
use draft_core::pack_source::FixturePackSource;
use draft_core::types::{
    DeckAddableCards, DraftAction, DraftConfig, DraftKind, DraftSource, DraftStatus, PodPolicy,
    SpectatorVisibility, TournamentFormat,
};
use draft_core::view::DraftPlayerView;

/// Reusable harness for full 8-player Premier draft simulation.
///
/// Wraps `DraftSessionManager` with convenience methods for driving
/// the entire draft lifecycle: start, pick all packs, submit decks,
/// and simulate disconnects/reconnects. Used by Plan 03 integration
/// tests and Plan 05 edge-case tests.
pub(crate) struct TournamentHarness {
    pub manager: DraftSessionManager,
    pub draft_code: String,
    pub tokens: Vec<String>,
    pub source: FixturePackSource,
}

impl TournamentHarness {
    /// Create an 8-player Premier Draft pod, all seats joined, ready to start.
    pub fn new_premier_draft() -> Self {
        let mut manager = DraftSessionManager::new();
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let (draft_code, host_token, _) = manager.create_draft(config, "Player0".into());
        let mut tokens = vec![host_token];
        for i in 1..8usize {
            let (tok, _, _) = manager
                .join_draft(&draft_code, format!("Player{i}"), None)
                .unwrap();
            tokens.push(tok);
        }
        let source = FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        Self {
            manager,
            draft_code,
            tokens,
            source,
        }
    }

    /// Start the draft (transition from Lobby -> Drafting).
    pub fn start(&mut self) {
        self.manager
            .apply_system_action(
                &self.draft_code,
                DraftAction::StartDraft,
                Some(&self.source),
            )
            .unwrap();
    }

    /// Pick the first available card for all seats that have a pending pack.
    pub fn pick_first_for_all_seats(&mut self) {
        for seat in 0..self.tokens.len() {
            let session = &self.manager.sessions[&self.draft_code];
            let view = session.view_for_seat(seat);
            if let Some(pack) = view.current_pack {
                if let Some(card) = pack.first() {
                    let _ = self.manager.handle_draft_action(
                        &self.draft_code,
                        &self.tokens[seat],
                        DraftAction::Pick {
                            seat: seat as u8,
                            card_instance_ids: vec![card.instance_id.clone()],
                        },
                        Some(&self.source),
                    );
                }
            }
        }
    }

    /// Run all picks for all packs (14 cards x 3 packs = 42 picks per seat).
    pub fn run_all_picks(&mut self) {
        let total_picks = self.source.cards_per_pack as usize * 3;
        for _ in 0..total_picks {
            self.pick_first_for_all_seats();
        }
    }

    /// Submit auto-generated decks for all seats.
    ///
    /// Uses 23 pool card names + 17 basic lands to reach the 40-card minimum
    /// required by limited deck validation.
    pub fn submit_all_decks(&mut self) {
        for seat in 0..self.tokens.len() {
            let session = &self.manager.sessions[&self.draft_code];
            let view = session.view_for_seat(seat);
            let mut main_deck: Vec<String> =
                view.pool.iter().take(23).map(|c| c.name.clone()).collect();
            main_deck.extend(std::iter::repeat_n("Plains".to_string(), 17));
            let _ = self.manager.handle_draft_action(
                &self.draft_code,
                &self.tokens[seat],
                DraftAction::SubmitDeck {
                    seat: seat as u8,
                    main_deck,
                    commanders: Vec::new(),
                    companion: None,
                    printings: Vec::new(),
                },
                Some(&self.source),
            );
        }
    }

    /// Simulate a seat disconnecting.
    pub fn disconnect_seat(&mut self, seat: usize) {
        self.manager
            .handle_disconnect(&self.draft_code, seat, std::time::Instant::now());
    }

    /// Simulate a seat reconnecting. Returns the refreshed view.
    pub fn reconnect_seat(&mut self, seat: usize) -> Result<DraftPlayerView, String> {
        self.manager.handle_reconnect(
            &self.draft_code,
            &self.tokens[seat],
            std::time::Instant::now(),
        )
    }

    /// Get current draft status.
    pub fn status(&self) -> DraftStatus {
        self.manager.sessions[&self.draft_code].session.status
    }

    /// Simulate a server crash by serializing all sessions, creating a fresh manager,
    /// and restoring from the serialized data. Returns a new harness with restored state.
    pub fn simulate_crash_and_restore(&self) -> Self {
        let session = &self.manager.sessions[&self.draft_code];
        let persisted = session.to_persisted();
        let json = serde_json::to_string(&persisted).unwrap();

        // Create fresh manager (simulates server restart)
        let mut new_manager = DraftSessionManager::new();
        let restored: crate::persist::PersistedDraftSession = serde_json::from_str(&json).unwrap();
        new_manager
            .restore_session(restored)
            .expect("valid legacy tournament snapshot");

        // Reconstruct source (not serializable — test fixture only)
        let source = FixturePackSource {
            set_code: self.source.set_code.clone(),
            cards_per_pack: self.source.cards_per_pack,
        };

        Self {
            manager: new_manager,
            draft_code: self.draft_code.clone(),
            tokens: self.tokens.clone(),
            source,
        }
    }
}

/// Harness for a private FIFO pod: 8 seats, `TST`, 2-card packs, 3 packs, a
/// fixed 1000 ms pick clock and a 1000 ms Review, installed through the private
/// stager. Every operation goes through the manager's FIFO staging and is
/// committed as a saved result would be.
pub(crate) struct PrivateFifoHarness {
    pub manager: DraftSessionManager,
    pub draft_code: String,
    pub tokens: Vec<String>,
    pub source: FixturePackSource,
    pub now_ms: u64,
}

impl PrivateFifoHarness {
    pub fn new() -> Self {
        let mut manager = DraftSessionManager::new();
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::FifoAsync,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy {
                pick: draft_core::types::PickClockPolicy::Fixed { duration_ms: 1000 },
                review_duration_ms: 1000,
            },
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 2,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let names = (0..8).map(|seat| format!("Player{seat}")).collect();
        let staged = manager
            .stage_private_fifo(config, names)
            .expect("a valid private FIFO pod");
        let draft_code = staged.draft_code().to_string();
        let tokens = staged.player_tokens().to_vec();
        manager.install_fifo(staged).expect("a free code");
        Self {
            manager,
            draft_code,
            tokens,
            source: FixturePackSource {
                set_code: "TST".to_string(),
                cards_per_pack: 2,
            },
            now_ms: 1000,
        }
    }

    fn apply(&mut self, actor: DraftActor<'_>, action: DraftAction) -> FifoStaged {
        let staged = self
            .manager
            .stage_fifo_operation(
                &self.draft_code,
                actor,
                action,
                Some(&self.source),
                self.now_ms,
            )
            .expect("an accepted FIFO operation");
        if staged == FifoStaged::Pending {
            self.manager
                .commit_fifo_result(&self.draft_code)
                .expect("a staged result");
        }
        staged
    }

    /// Start the draft at the harness clock.
    pub fn start(&mut self) {
        self.apply(DraftActor::Server, DraftAction::StartDraft);
    }

    /// The seat takes its head's legal selection, through its own token.
    /// Returns whether the seat had a head.
    pub fn select_head(&mut self, seat: u8) -> bool {
        let session = &self.manager.sessions[&self.draft_code].session;
        let Some(action) = draft_core::pick_pass::legal_auto_selection(session, seat) else {
            return false;
        };
        let token = self.tokens[usize::from(seat)].clone();
        self.apply(DraftActor::Seat { token: &token }, action);
        true
    }

    /// Run core's clock expiry at `now_ms`.
    pub fn advance_clock(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        self.apply(DraftActor::Server, DraftAction::AdvanceClock);
    }

    /// Get current draft status.
    pub fn status(&self) -> DraftStatus {
        self.manager.sessions[&self.draft_code].session.status
    }

    /// Simulate a crash: serialize the session, then restore it through the
    /// internal FIFO restore into a fresh manager.
    pub fn simulate_crash_and_restore(&self) -> Self {
        let json =
            serde_json::to_string(&self.manager.sessions[&self.draft_code].to_persisted()).unwrap();
        let restored: crate::persist::PersistedDraftSession = serde_json::from_str(&json).unwrap();
        let mut manager = DraftSessionManager::new();
        let staged = manager
            .stage_restored_fifo(restored)
            .expect("a valid FIFO row");
        manager.install_fifo(staged).expect("a free code");
        Self {
            manager,
            draft_code: self.draft_code.clone(),
            tokens: self.tokens.clone(),
            source: FixturePackSource {
                set_code: self.source.set_code.clone(),
                cards_per_pack: self.source.cards_per_pack,
            },
            now_ms: self.now_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_8_player_premier_draft_picks_complete() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        assert_eq!(h.status(), DraftStatus::Drafting);
        h.run_all_picks();
        assert_eq!(h.status(), DraftStatus::Deckbuilding);
    }

    #[test]
    fn full_lifecycle_picks_then_deckbuild() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        h.run_all_picks();
        h.submit_all_decks();
        // After all decks submitted, status should advance past Deckbuilding
        assert!(matches!(
            h.status(),
            DraftStatus::Pairing | DraftStatus::MatchInProgress | DraftStatus::RoundComplete
        ));
    }

    /// A completed Swiss round must be followed by round two.
    ///
    /// `AdvanceRound` deliberately leaves `current_round` at the round just
    /// finished; only pairing generation commits the next one. Any caller that
    /// derives the round itself gets this wrong (it did: `current_round.max(1)`
    /// pinned every pod to round one forever).
    #[test]
    fn advance_round_then_generate_pairs_round_two() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        h.run_all_picks();
        h.submit_all_decks();

        h.manager
            .ensure_pairings_generated(&h.draft_code)
            .expect("round one pairings");
        assert_eq!(h.status(), DraftStatus::MatchInProgress);

        let round_one: Vec<(String, u8)> = h.manager.sessions[&h.draft_code]
            .session
            .pairings
            .iter()
            .filter(|p| p.round == 1)
            .map(|p| (p.match_id.clone(), p.players[0].0))
            .collect();
        assert_eq!(round_one.len(), 4, "an 8-player pod pairs four tables");

        for (match_id, winner_seat) in round_one {
            h.manager
                .apply_system_action(
                    &h.draft_code,
                    DraftAction::ReportMatchResult {
                        match_id,
                        winner_seat: Some(winner_seat),
                    },
                    None,
                )
                .expect("report round one result");
        }
        assert_eq!(h.status(), DraftStatus::RoundComplete);

        h.manager
            .apply_system_action(&h.draft_code, DraftAction::AdvanceRound, None)
            .expect("advance round");
        assert_eq!(h.status(), DraftStatus::Pairing);

        // REVERT-FAILING ASSERTION. Pre-fix `ensure_pairings_generated` derives
        // `current_round.max(1) == 1`, the guard rejects `1 != 1 + 1`, and this
        // `.expect` panics.
        h.manager
            .ensure_pairings_generated(&h.draft_code)
            .expect("round two pairings");

        let view = h.manager.sessions[&h.draft_code].view_for_seat(0);
        assert_eq!(view.current_round, 2, "generation commits round two");
        assert_eq!(
            view.pairings.len(),
            4,
            "the view exposes the four round-two tables",
        );
        assert_eq!(h.status(), DraftStatus::MatchInProgress);
    }

    #[test]
    fn disconnect_and_reconnect_during_drafting() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        h.disconnect_seat(3);
        // Seat 3 can still reconnect
        let view = h.reconnect_seat(3);
        assert!(view.is_ok());
    }

    #[test]
    fn all_seats_have_42_cards_after_all_picks() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        h.run_all_picks();
        for seat in 0..8 {
            let view = h.manager.sessions[&h.draft_code].view_for_seat(seat);
            assert_eq!(view.pool.len(), 42, "seat {seat} should have 42 cards");
        }
    }

    #[test]
    fn disconnect_multiple_seats_during_drafting() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        h.disconnect_seat(0);
        h.disconnect_seat(5);
        // Both can reconnect independently
        assert!(h.reconnect_seat(0).is_ok());
        assert!(h.reconnect_seat(5).is_ok());
    }

    #[test]
    fn picks_continue_after_disconnect_reconnect() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        // Do a few picks
        h.pick_first_for_all_seats();
        // Disconnect seat 2, pick some more
        h.disconnect_seat(2);
        h.pick_first_for_all_seats();
        // Reconnect seat 2
        let view = h.reconnect_seat(2).unwrap();
        assert_eq!(view.status, DraftStatus::Drafting);
    }

    #[test]
    fn crash_and_restore_during_drafting_preserves_state() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        // Pick 10 rounds
        for _ in 0..10 {
            h.pick_first_for_all_seats();
        }

        // Crash and restore
        let mut restored = h.simulate_crash_and_restore();
        assert_eq!(restored.status(), DraftStatus::Drafting);

        // Verify pools were preserved (each seat should have 10 cards)
        for seat in 0..8 {
            let view = restored.manager.sessions[&restored.draft_code].view_for_seat(seat);
            assert_eq!(
                view.pool.len(),
                10,
                "seat {seat} pool preserved after crash"
            );
        }

        // Continue drafting after restore
        for _ in 10..42 {
            restored.pick_first_for_all_seats();
        }
        assert_eq!(restored.status(), DraftStatus::Deckbuilding);
    }

    #[test]
    fn crash_and_restore_during_deckbuilding_preserves_pools() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        h.run_all_picks();
        assert_eq!(h.status(), DraftStatus::Deckbuilding);

        let mut restored = h.simulate_crash_and_restore();
        assert_eq!(restored.status(), DraftStatus::Deckbuilding);

        // Can still submit decks after restore
        restored.submit_all_decks();
        assert!(matches!(
            restored.status(),
            DraftStatus::Pairing | DraftStatus::MatchInProgress | DraftStatus::RoundComplete
        ));
    }

    #[test]
    fn simultaneous_disconnect_of_multiple_seats_during_drafting() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();
        // Pick a few rounds
        for _ in 0..3 {
            h.pick_first_for_all_seats();
        }

        // Disconnect seats 2, 5, 7 simultaneously
        h.disconnect_seat(2);
        h.disconnect_seat(5);
        h.disconnect_seat(7);

        // Verify disconnected state
        let session = &h.manager.sessions[&h.draft_code];
        assert!(!session.connected[2]);
        assert!(!session.connected[5]);
        assert!(!session.connected[7]);

        // Reconnect all
        assert!(h.reconnect_seat(2).is_ok());
        assert!(h.reconnect_seat(5).is_ok());
        assert!(h.reconnect_seat(7).is_ok());
    }

    #[test]
    fn restore_rebuilds_token_to_draft_index() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();

        let restored = h.simulate_crash_and_restore();

        // All tokens should resolve to the draft code
        for token in &restored.tokens {
            assert_eq!(
                restored.manager.draft_for_token(token),
                Some(restored.draft_code.as_str()),
                "token_to_draft index rebuilt for {token}"
            );
        }
    }

    #[test]
    fn all_connected_false_after_restore() {
        let mut h = TournamentHarness::new_premier_draft();
        h.start();

        let restored = h.simulate_crash_and_restore();
        let session = &restored.manager.sessions[&restored.draft_code];
        assert!(
            session.connected.iter().all(|&c| !c),
            "all seats disconnected after restore"
        );
    }

    #[test]
    fn private_fifo_crash_and_restore_preserves_queues() {
        let mut h = PrivateFifoHarness::new();
        h.start();
        assert_eq!(h.status(), DraftStatus::Drafting);
        // Mid-round 1: half the seats take their head.
        h.now_ms = 1200;
        for seat in 0..4 {
            assert!(h.select_head(seat));
        }
        let before = h.manager.sessions[&h.draft_code].session.clone();
        let restored = h.simulate_crash_and_restore();
        let after = &restored.manager.sessions[&restored.draft_code].session;
        let (before_fifo, after_fifo) =
            (before.fifo.as_ref().unwrap(), after.fifo.as_ref().unwrap());
        for seat in 0..8 {
            assert_eq!(after_fifo.seats[seat].queue, before_fifo.seats[seat].queue);
            assert_eq!(
                after_fifo.seats[seat].prompt,
                before_fifo.seats[seat].prompt
            );
            assert_eq!(after.pools[seat], before.pools[seat]);
        }
        assert_eq!(*after, before);

        // Continue to the end: head selections, and core's clock at each
        // Review expiry, until deckbuilding.
        let mut h = restored;
        let mut reviews = 0;
        while h.status() != DraftStatus::Deckbuilding {
            match h.status() {
                DraftStatus::Drafting => {
                    let mut selected = false;
                    for seat in 0..8 {
                        selected |= h.select_head(seat);
                    }
                    assert!(selected, "a drafting pod has a head to select");
                }
                DraftStatus::Review => {
                    let review = h.manager.sessions[&h.draft_code]
                        .session
                        .fifo
                        .as_ref()
                        .unwrap()
                        .review
                        .clone()
                        .unwrap();
                    let draft_core::types::DraftClockState::Running { deadline_ms } =
                        review.clock.state
                    else {
                        panic!("Review runs");
                    };
                    if reviews == 0 {
                        // A crash during Review keeps its deadline.
                        let again = h.simulate_crash_and_restore();
                        assert_eq!(
                            again.manager.sessions[&again.draft_code]
                                .session
                                .fifo
                                .as_ref()
                                .unwrap()
                                .review,
                            Some(review.clone())
                        );
                        assert_eq!(
                            again.manager.fifo_next_due(&again.draft_code),
                            Some(deadline_ms)
                        );
                        h = again;
                    }
                    reviews += 1;
                    h.advance_clock(deadline_ms);
                }
                other => panic!("unexpected status {other:?}"),
            }
        }
        assert_eq!(reviews, 2);
        assert_eq!(h.status(), DraftStatus::Deckbuilding);
        assert!(h.manager.sessions[&h.draft_code]
            .session
            .pools
            .iter()
            .all(|pool| pool.len() == 6));
    }
}
