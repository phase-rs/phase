use crate::types::*;

/// Apply a pick action: remove a card from the seat's current pack and add it to their pool.
/// After all seats have picked, trigger pack passing.
pub fn apply_pick(
    session: &mut DraftSession,
    seat: u8,
    card_instance_id: String,
) -> Result<Vec<DraftDelta>, DraftError> {
    if session.status != DraftStatus::Drafting {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "Pick".to_string(),
        });
    }

    let pod_size = session.seats.len() as u8;
    if seat >= pod_size {
        return Err(DraftError::SeatOutOfRange { seat, pod_size });
    }

    // Lazily reshape both bitmaps on first access — handles in-flight saves
    // written by pre-fix code that lacked these fields. `ensure_len` uses
    // Vec::resize semantics so any post-fix entries are preserved.
    session.seats_picked_this_round.ensure_len(pod_size, false);
    session.connected_seats.ensure_len(pod_size, true);

    // Reject duplicate picks from the same seat in one round. This is the
    // engine-side gate for Bug #1 (auto-pick by one seat forcing pack-pass).
    if session.seats_picked_this_round.get(seat) {
        return Err(DraftError::SeatAlreadyPickedThisRound { seat });
    }

    let pack = session.current_pack[seat as usize]
        .as_mut()
        .ok_or(DraftError::NoPendingPack { seat })?;

    let card_index = pack
        .0
        .iter()
        .position(|c| c.instance_id == card_instance_id)
        .ok_or_else(|| DraftError::CardNotInPack {
            card_instance_id: card_instance_id.clone(),
        })?;

    let picked = pack.0.remove(card_index);
    session.pools[seat as usize].push(picked);
    session.seats_picked_this_round.set(seat, true);

    let mut deltas = vec![DraftDelta::CardPicked {
        seat,
        card_instance_id,
    }];

    // Round complete when every seat that still owes a pick has picked.
    // A seat owes a pick iff its current_pack is Some and non-empty. Seats
    // with no remaining pack (e.g. last card of a pack just taken) are
    // excluded from the "must pick" set so the round can still advance.
    // Disconnected human seats are NOT excluded here — the host adapter's
    // `autoPickAllPending` picks on their behalf on timer expiry.
    let round_complete = (0..pod_size).all(|i| {
        let owes_pick = session.current_pack[i as usize]
            .as_ref()
            .is_some_and(|p| !p.0.is_empty());
        !owes_pick || session.seats_picked_this_round.get(i)
    });

    if round_complete {
        session.seats_picked_this_round.clear();

        // Check if current packs are empty (pack round complete)
        let packs_empty = session
            .current_pack
            .iter()
            .all(|p| p.as_ref().is_none_or(|pack| pack.0.is_empty()));

        if packs_empty {
            session.current_pack_number += 1;

            if session.current_pack_number >= session.config.pack_count {
                // All packs exhausted -- transition to Deckbuilding
                session.status = DraftStatus::Deckbuilding;
                deltas.push(DraftDelta::TransitionedTo {
                    status: DraftStatus::Deckbuilding,
                });
            } else {
                // Start new pack round
                session.pass_direction = PassDirection::for_pack(session.current_pack_number);
                session.pick_number = 0;

                for s in 0..pod_size as usize {
                    if !session.packs_by_seat[s].is_empty() {
                        session.current_pack[s] = Some(session.packs_by_seat[s].remove(0));
                    }
                }

                deltas.push(DraftDelta::PackExhausted {
                    new_pack_number: session.current_pack_number,
                });
            }
        } else {
            // Pass packs around
            session.pick_number += 1;
            deltas.push(DraftDelta::PackPassed);

            let mut new_packs: Vec<Option<DraftPack>> = vec![None; pod_size as usize];
            for i in 0..pod_size {
                let dest = session.pass_direction.next_seat(i, pod_size);
                new_packs[dest as usize] = session.current_pack[i as usize].take();
            }
            session.current_pack = new_packs;
        }
    }

    Ok(deltas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_source::FixturePackSource;
    use crate::session;

    use engine::types::player::PlayerId;

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
            })
            .collect();
        let source = FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let s = DraftSession::new(config, seats, "TEST-001".to_string());
        (s, source)
    }

    fn start_draft(session: &mut DraftSession, source: &FixturePackSource) {
        session::apply(session, DraftAction::StartDraft, Some(source)).unwrap();
    }

    /// Pick the first card from the specified seat's current pack.
    fn pick_first(session: &mut DraftSession, seat: u8) -> Vec<DraftDelta> {
        let card_id = session.current_pack[seat as usize].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            session,
            DraftAction::Pick {
                seat,
                card_instance_id: card_id,
            },
            None,
        )
        .unwrap()
    }

    /// Have all seats pick their first card (one full round).
    fn pick_round(session: &mut DraftSession, pod_size: u8) -> Vec<DraftDelta> {
        let mut all_deltas = Vec::new();
        for seat in 0..pod_size {
            all_deltas.extend(pick_first(session, seat));
        }
        all_deltas
    }

    fn assert_pack_conservation(session: &DraftSession, expected_total: usize) {
        let mut total = 0;
        for pack in session.current_pack.iter().flatten() {
            total += pack.0.len();
        }
        for seat_packs in &session.packs_by_seat {
            for pack in seat_packs {
                total += pack.0.len();
            }
        }
        for pool in &session.pools {
            total += pool.len();
        }
        assert_eq!(total, expected_total, "pack conservation violated");
    }

    #[test]
    fn pick_removes_card_from_pack_and_adds_to_pool() {
        let (mut session, source) = test_session(8);
        start_draft(&mut session, &source);

        let card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let deltas = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: card_id.clone(),
            },
            None,
        )
        .unwrap();

        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 13);
        assert_eq!(session.pools[0].len(), 1);
        assert_eq!(session.pools[0][0].instance_id, card_id);
        assert!(deltas.contains(&DraftDelta::CardPicked {
            seat: 0,
            card_instance_id: card_id,
        }));
    }

    #[test]
    fn pick_invalid_card_returns_error() {
        let (mut session, source) = test_session(8);
        start_draft(&mut session, &source);

        let result = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: "nonexistent".to_string(),
            },
            None,
        );
        assert!(matches!(result, Err(DraftError::CardNotInPack { .. })));
    }

    #[test]
    fn pick_no_pending_pack_returns_error() {
        let (mut session, source) = test_session(8);
        start_draft(&mut session, &source);

        // Manually clear the pack
        session.current_pack[0] = None;
        let result = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: "any".to_string(),
            },
            None,
        );
        assert!(matches!(result, Err(DraftError::NoPendingPack { seat: 0 })));
    }

    #[test]
    fn pick_on_non_drafting_returns_error() {
        let (mut session, _) = test_session(8);
        // Session is still in Lobby
        let result = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: "any".to_string(),
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
    fn packs_pass_left_for_pack_0() {
        let (mut session, source) = test_session(8);
        start_draft(&mut session, &source);

        // Record seat 0's pack card IDs before picks
        let seat0_pack_ids: Vec<String> = session.current_pack[0]
            .as_ref()
            .unwrap()
            .0
            .iter()
            .map(|c| c.instance_id.clone())
            .collect();

        // All 8 seats pick their first card
        let deltas = pick_round(&mut session, 8);
        assert!(deltas.contains(&DraftDelta::PackPassed));

        // Pack 0 passes LEFT: seat 0's remaining 13 cards should now be at seat 1
        let seat1_pack = session.current_pack[1].as_ref().unwrap();
        assert_eq!(seat1_pack.0.len(), 13);
        // The remaining cards from seat 0's original pack (minus the first) should be at seat 1
        for card in &seat1_pack.0 {
            assert!(seat0_pack_ids.contains(&card.instance_id));
        }
    }

    #[test]
    fn packs_pass_right_for_pack_1() {
        let (mut session, source) = test_session(8);
        start_draft(&mut session, &source);

        // Complete all 14 rounds of pack 0
        for _ in 0..14 {
            pick_round(&mut session, 8);
        }

        assert_eq!(session.current_pack_number, 1);
        assert_eq!(session.pass_direction, PassDirection::Right);

        // Record seat 0's pack 1 card IDs
        let seat0_pack_ids: Vec<String> = session.current_pack[0]
            .as_ref()
            .unwrap()
            .0
            .iter()
            .map(|c| c.instance_id.clone())
            .collect();

        // One round of picks
        pick_round(&mut session, 8);

        // Pack 1 passes RIGHT: seat 0's remaining goes to seat 7
        let seat7_pack = session.current_pack[7].as_ref().unwrap();
        assert_eq!(seat7_pack.0.len(), 13);
        for card in &seat7_pack.0 {
            assert!(seat0_pack_ids.contains(&card.instance_id));
        }
    }

    #[test]
    fn packs_pass_left_for_pack_2() {
        let (mut session, source) = test_session(8);
        start_draft(&mut session, &source);

        // Complete pack 0 (14 rounds) + pack 1 (14 rounds) = 28 rounds
        for _ in 0..28 {
            pick_round(&mut session, 8);
        }

        assert_eq!(session.current_pack_number, 2);
        assert_eq!(session.pass_direction, PassDirection::Left);
    }

    #[test]
    fn full_draft_transitions_to_deckbuilding() {
        let (mut session, source) = test_session(8);
        start_draft(&mut session, &source);

        let total_cards = 8 * 3 * 14; // 336 total
        assert_pack_conservation(&session, total_cards);

        // 3 packs * 14 picks per pack = 42 rounds
        for round in 0..42 {
            pick_round(&mut session, 8);
            assert_pack_conservation(&session, total_cards);

            if round < 41 {
                // Not done yet
                assert_ne!(
                    session.status,
                    DraftStatus::Deckbuilding,
                    "unexpected deckbuilding at round {round}"
                );
            }
        }

        assert_eq!(session.status, DraftStatus::Deckbuilding);

        // Each seat's pool should have 42 cards
        for (i, pool) in session.pools.iter().enumerate() {
            assert_eq!(pool.len(), 42, "seat {i} pool should have 42 cards");
        }

        // No cards remaining in packs
        for pack_opt in &session.current_pack {
            assert!(
                pack_opt.is_none() || pack_opt.as_ref().unwrap().0.is_empty(),
                "current packs should be empty"
            );
        }
        for seat_packs in &session.packs_by_seat {
            assert!(seat_packs.is_empty(), "packs_by_seat should be empty");
        }
    }

    #[test]
    fn pack_conservation_after_every_pick() {
        let (mut session, source) = test_session(4);
        start_draft(&mut session, &source);

        let total_cards = 4 * 3 * 14; // 168

        // Do every single pick individually, checking conservation after each
        let mut picks_done = 0;
        while session.status == DraftStatus::Drafting {
            for seat in 0..4u8 {
                if session.current_pack[seat as usize].is_some()
                    && !session.current_pack[seat as usize]
                        .as_ref()
                        .unwrap()
                        .0
                        .is_empty()
                {
                    pick_first(&mut session, seat);
                    picks_done += 1;
                    assert_pack_conservation(&session, total_cards);
                }
            }
        }

        assert_eq!(picks_done, 4 * 3 * 14); // 168 picks total
        assert_eq!(session.status, DraftStatus::Deckbuilding);
    }

    // ── Regression coverage for the per-seat round-tracking fix ───────────

    /// The engine must reject a second pick from a seat that has already
    /// picked this round (Bug #1's per-seat gate).
    #[test]
    fn pick_twice_from_same_seat_returns_error() {
        let (mut session, source) = test_session(2);
        start_draft(&mut session, &source);

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

        let next_card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let result = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: next_card_id,
            },
            None,
        );

        assert!(matches!(
            result,
            Err(DraftError::SeatAlreadyPickedThisRound { seat: 0 })
        ));
    }

    /// Direct regression for the reported user bug: a single seat clicking
    /// auto-pick repeatedly used to drive `picks_this_round` >= pod_size and
    /// force pack-passing despite the other seat never picking. After the
    /// fix, the second host pick errors and seat 1's pack is untouched.
    #[test]
    fn single_seat_cannot_force_pack_pass() {
        let (mut session, source) = test_session(2);
        start_draft(&mut session, &source);

        let seat1_pack_len_before = session.current_pack[1].as_ref().unwrap().0.len();

        // Seat 0 picks once — fine.
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

        // Seat 0 picks again — engine rejects, pack unchanged.
        let attempt2 = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let result = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: attempt2,
            },
            None,
        );
        assert!(matches!(
            result,
            Err(DraftError::SeatAlreadyPickedThisRound { seat: 0 })
        ));

        // Seat 1's pack is untouched, no pack pass occurred, pack number unchanged.
        assert_eq!(
            session.current_pack[1].as_ref().unwrap().0.len(),
            seat1_pack_len_before
        );
        assert_eq!(session.current_pack_number, 0);
        assert!(session.seats_picked_this_round.get(0));
        assert!(!session.seats_picked_this_round.get(1));
    }

    /// Round completes only after every seat with a non-empty current_pack
    /// has picked. With pod_size=4 and only 3 seats having picked, the round
    /// does not advance.
    #[test]
    fn round_completes_only_when_all_seats_with_packs_pick() {
        let (mut session, source) = test_session(4);
        start_draft(&mut session, &source);

        for seat in 0..3u8 {
            pick_first(&mut session, seat);
        }
        // Not yet complete — seat 3 still owes a pick.
        assert!(session.seats_picked_this_round.get(0));
        assert!(!session.seats_picked_this_round.get(3));
        assert_eq!(session.pick_number, 0);

        pick_first(&mut session, 3);
        // Round advanced — all flags cleared, pick_number bumped.
        for i in 0..4u8 {
            assert!(!session.seats_picked_this_round.get(i));
        }
        assert_eq!(session.pick_number, 1);
    }

    /// A bot seat that picks must satisfy the round-complete predicate just
    /// like a human. Bots don't get special-cased.
    #[test]
    fn bot_seat_satisfies_round_complete_predicate() {
        let (mut session, source) = test_session(2);
        session.seats[1] = DraftSeat::Bot {
            name: "TestBot".to_string(),
        };
        start_draft(&mut session, &source);

        pick_first(&mut session, 0);
        assert_eq!(session.pick_number, 0); // round not done — bot hasn't picked

        pick_first(&mut session, 1);
        assert_eq!(session.pick_number, 1); // round advanced
    }

    /// In-flight host saves written by pre-fix code carried a `picks_this_round`
    /// counter but no `seats_picked_this_round` bitmap. On upgrade the lazy
    /// `ensure_len(false)` initialises the bitmap as "nobody has picked yet" —
    /// the old counter's identity is unrecoverable, so we conservatively let
    /// every seat pick. Worst case is one duplicate card per affected seat;
    /// the round-complete predicate self-heals on the next round.
    #[test]
    fn mid_round_resume_treats_all_seats_as_not_yet_picked() {
        // Build an old-shape session JSON: has `picks_this_round` but no new fields.
        let (template, source) = test_session(2);
        let mut session = template.clone();
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();
        // Reset to simulate the deserialized-from-old-shape state.
        session.seats_picked_this_round = SeatFlags::default(); // empty Vec<bool>
        session.connected_seats = SeatFlags::default();

        let card_id = session.current_pack[1].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let result = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 1,
                card_instance_id: card_id,
            },
            None,
        );

        assert!(result.is_ok());
        assert!(session.seats_picked_this_round.get(1));
        assert!(!session.seats_picked_this_round.get(0));
    }
}
