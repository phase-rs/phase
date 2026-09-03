use std::cmp::Reverse;

use crate::types::*;

/// Apply a pick action: remove this seat's drafted cards from its current pack
/// and add them to its pool. After all seats have picked, trigger pack passing.
///
/// One action carries the seat's whole pick step, however many cards that is.
pub fn apply_pick(
    session: &mut DraftSession,
    seat: u8,
    card_instance_ids: Vec<String>,
) -> Result<Vec<DraftDelta>, DraftError> {
    apply_pick_inner(session, seat, card_instance_ids)
}

/// CR 905.1a + CR 905.2: Draft two cards from the current booster in exchange
/// for returning the face-up effect card to that booster.
pub fn apply_pick_with_draft_effect(
    session: &mut DraftSession,
    seat: u8,
    effect_card_instance_id: String,
    card_instance_ids: Vec<String>,
) -> Result<Vec<DraftDelta>, DraftError> {
    if card_instance_ids.len() != 2 {
        return Err(DraftError::InvalidDraftEffectSelection {
            expected_cards: 2,
            actual_cards: card_instance_ids.len(),
        });
    }

    apply_pick_with_effect_inner(session, seat, effect_card_instance_id, card_instance_ids)
}

/// CR 903.13b: how many cards one pick step takes from this seat's pack.
///
/// `DraftProcedure::cards_per_pick` is 1 for the four CR 905.1a kinds and 2 for
/// `CommanderDraft`, clamped to what the pack still holds: CR 903.13b's
/// procedure "continues until all cards in that draft round have been drafted",
/// so an odd pack's final step takes the one card that remains. The clamp makes
/// that correct by construction rather than by special case.
///
/// **Single authority.** `apply_pick_inner` enforces this count and
/// `view::filter_for_player` publishes it as
/// `DraftPlayerView::required_pick_count`, so no display layer re-derives it.
///
/// This is a **pure clamp with no status term, and must not acquire one.**
/// Returns 0 when the seat has no pending pack — the refusal for a pick made
/// outside `DraftStatus::Drafting` is `apply_pick_inner`'s alone (its guard at
/// the top of that function), and a status term here would mint a second
/// authority for it.
pub fn required_pick_count(session: &DraftSession, seat: u8) -> usize {
    let pack_len = session
        .current_pack
        .get(seat as usize)
        .and_then(|pack| pack.as_ref())
        .map_or(0, |pack| pack.0.len());
    usize::from(session.kind.procedure().cards_per_pick).min(pack_len)
}

fn apply_pick_inner(
    session: &mut DraftSession,
    seat: u8,
    card_instance_ids: Vec<String>,
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

    let pack_len = session.current_pack[seat as usize]
        .as_ref()
        .map_or(0, |pack| pack.0.len());
    if pack_len == 0 {
        return Err(DraftError::NoPendingPack { seat });
    }

    let expected = required_pick_count(session, seat);
    if card_instance_ids.len() != expected {
        return Err(DraftError::WrongPickCardCount {
            seat,
            expected,
            actual: card_instance_ids.len(),
        });
    }

    // Resolve every id to a pack index BEFORE removing anything. Mirrors
    // `apply_pick_with_effect_inner`'s `missing_card` pre-check: a pick that
    // fails validation must leave the session untouched, and a per-id remove
    // loop would push the first card to the pool before erroring on the second.
    let pack = session.current_pack[seat as usize]
        .as_ref()
        .expect("pack length was measured above");
    let mut indices: Vec<usize> = Vec::with_capacity(card_instance_ids.len());
    for card_instance_id in &card_instance_ids {
        let index = pack
            .0
            .iter()
            .position(|card| card.instance_id == *card_instance_id)
            .ok_or_else(|| DraftError::CardNotInPack {
                card_instance_id: card_instance_id.clone(),
            })?;
        // Two equal ids resolve to the same index, so presence and distinctness
        // fall out of one pass.
        if indices.contains(&index) {
            return Err(DraftError::DuplicatePickCardId {
                seat,
                card_instance_id: card_instance_id.clone(),
            });
        }
        indices.push(index);
    }

    let picked = {
        let pack = session.current_pack[seat as usize]
            .as_mut()
            .expect("pack was present during pick validation");
        // Remove by descending index so an earlier removal cannot shift a later
        // one, then restore the caller's id order.
        let mut removal_order: Vec<(usize, usize)> = indices.into_iter().enumerate().collect();
        removal_order.sort_unstable_by_key(|(_, index)| Reverse(*index));
        let mut picked: Vec<(usize, DraftCardInstance)> = removal_order
            .into_iter()
            .map(|(slot, index)| (slot, pack.0.remove(index)))
            .collect();
        picked.sort_unstable_by_key(|&(slot, _)| slot);
        picked.into_iter().map(|(_, card)| card).collect::<Vec<_>>()
    };

    session.pools[seat as usize].extend(picked);

    finish_pick(session, seat, card_instance_ids)
}

fn apply_pick_with_effect_inner(
    session: &mut DraftSession,
    seat: u8,
    effect_card_instance_id: String,
    card_instance_ids: Vec<String>,
) -> Result<Vec<DraftDelta>, DraftError> {
    if session.status != DraftStatus::Drafting {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "PickWithDraftEffect".to_string(),
        });
    }

    let pod_size = session.seats.len() as u8;
    if seat >= pod_size {
        return Err(DraftError::SeatOutOfRange { seat, pod_size });
    }
    session.seats_picked_this_round.ensure_len(pod_size, false);
    session.connected_seats.ensure_len(pod_size, true);
    if session.seats_picked_this_round.get(seat) {
        return Err(DraftError::SeatAlreadyPickedThisRound { seat });
    }
    if card_instance_ids[0] == card_instance_ids[1] {
        return Err(DraftError::InvalidDraftEffectSelection {
            expected_cards: 2,
            actual_cards: 1,
        });
    }

    let effect_index = session.pools[seat as usize]
        .iter()
        .position(|card| {
            card.instance_id == effect_card_instance_id
                && matches!(
                    card.draft_effect,
                    Some(engine::types::card::DraftEffect::AdditionalPick)
                )
        })
        .ok_or_else(|| DraftError::DraftEffectCardNotInPool {
            card_instance_id: effect_card_instance_id.clone(),
        })?;

    let pack = session.current_pack[seat as usize]
        .as_ref()
        .ok_or(DraftError::NoPendingPack { seat })?;
    let missing_card = card_instance_ids
        .iter()
        .find(|card_id| !pack.0.iter().any(|card| card.instance_id == **card_id));
    if let Some(missing_card) = missing_card {
        return Err(DraftError::CardNotInPack {
            card_instance_id: missing_card.clone(),
        });
    }

    let picked_cards = {
        let pack = session.current_pack[seat as usize]
            .as_mut()
            .expect("pack was present during draft-effect validation");
        card_instance_ids
            .iter()
            .map(|card_id| {
                let index = pack
                    .0
                    .iter()
                    .position(|card| card.instance_id == *card_id)
                    .expect("validated draft-effect card must remain in the pack");
                pack.0.remove(index)
            })
            .collect::<Vec<_>>()
    };

    let effect_card = session.pools[seat as usize].remove(effect_index);
    session.pools[seat as usize].extend(picked_cards);
    session.current_pack[seat as usize]
        .as_mut()
        .expect("pack was present while selecting the extra cards")
        .0
        .push(effect_card);

    finish_pick(session, seat, card_instance_ids)
}

fn finish_pick(
    session: &mut DraftSession,
    seat: u8,
    card_instance_ids: Vec<String>,
) -> Result<Vec<DraftDelta>, DraftError> {
    let pod_size = session.seats.len() as u8;
    session
        .current_pack_origins
        .resize(usize::from(pod_size), None);
    session.seats_picked_this_round.set(seat, true);

    let mut deltas: Vec<DraftDelta> = card_instance_ids
        .into_iter()
        .map(|card_instance_id| DraftDelta::CardPicked {
            seat,
            card_instance_id,
        })
        .collect();

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
                session
                    .current_pack_origins
                    .resize(usize::from(pod_size), None);
                session.current_pack_origins.fill(None);

                for s in 0..pod_size as usize {
                    if !session.packs_by_seat[s].is_empty() {
                        session.current_pack[s] = Some(session.packs_by_seat[s].remove(0));
                        session.current_pack_origins[s] = Some(s as u8);
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
            let mut new_origins: Vec<Option<u8>> = vec![None; pod_size as usize];
            for i in 0..pod_size {
                let dest = session.pass_direction.next_seat(i, pod_size);
                new_packs[dest as usize] = session.current_pack[i as usize].take();
                new_origins[dest as usize] = session
                    .current_pack_origins
                    .get_mut(i as usize)
                    .and_then(Option::take);
            }
            session.current_pack = new_packs;
            session.current_pack_origins = new_origins;
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
        let s = DraftSession::new(config, seats, "TEST-001".to_string());
        (s, source)
    }

    fn start_draft(session: &mut DraftSession, source: &FixturePackSource) {
        session::apply(session, DraftAction::StartDraft, Some(source)).unwrap();
    }

    /// Pick this seat's whole pick step from the front of its current pack.
    ///
    /// Reads `cards_per_pick` so the helper stays kind-agnostic: one card for
    /// the four CR 905.1a kinds, two for CommanderDraft (CR 903.13b), clamped
    /// to whatever the pack still holds.
    fn pick_first(session: &mut DraftSession, seat: u8) -> Vec<DraftDelta> {
        let card_instance_ids: Vec<String> = {
            let pack = &session.current_pack[seat as usize].as_ref().unwrap().0;
            let count = usize::from(session.kind.procedure().cards_per_pick).min(pack.len());
            pack[..count]
                .iter()
                .map(|card| card.instance_id.clone())
                .collect()
        };
        session::apply(
            session,
            DraftAction::Pick {
                seat,
                card_instance_ids,
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
                card_instance_ids: vec![card_id.clone()],
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
                card_instance_ids: vec!["nonexistent".to_string()],
            },
            None,
        );
        assert!(matches!(result, Err(DraftError::CardNotInPack { .. })));
    }

    #[test]
    fn draft_effect_pick_returns_effect_card_to_pack() {
        let (mut session, source) = test_session(2);
        start_draft(&mut session, &source);

        let effect_card = DraftCardInstance {
            instance_id: "cogwork-1".to_string(),
            name: "Cogwork Librarian".to_string(),
            set_code: "CNS".to_string(),
            collector_number: "58".to_string(),
            rarity: "common".to_string(),
            colors: Vec::new(),
            cmc: 4,
            type_line: "Artifact Creature — Construct".to_string(),
            draft_effect: Some(engine::types::card::DraftEffect::AdditionalPick),
        };
        session.pools[0].push(effect_card.clone());
        let first_card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let second_card_id = session.current_pack[0].as_ref().unwrap().0[1]
            .instance_id
            .clone();
        let pack_len = session.current_pack[0].as_ref().unwrap().0.len();

        let deltas = session::apply(
            &mut session,
            DraftAction::PickWithDraftEffect {
                seat: 0,
                effect_card_instance_id: effect_card.instance_id.clone(),
                card_instance_ids: vec![first_card_id.clone(), second_card_id.clone()],
            },
            None,
        )
        .unwrap();

        let pack = session.current_pack[0].as_ref().unwrap();
        assert_eq!(pack.0.len(), pack_len - 1);
        assert!(pack
            .0
            .iter()
            .any(|card| card.instance_id == effect_card.instance_id));
        assert!(!session.pools[0]
            .iter()
            .any(|card| card.instance_id == effect_card.instance_id));
        assert!(session.pools[0]
            .iter()
            .any(|card| card.instance_id == first_card_id));
        assert!(session.pools[0]
            .iter()
            .any(|card| card.instance_id == second_card_id));
        assert!(deltas.contains(&DraftDelta::CardPicked {
            seat: 0,
            card_instance_id: first_card_id,
        }));
        assert!(deltas.contains(&DraftDelta::CardPicked {
            seat: 0,
            card_instance_id: second_card_id,
        }));
        assert!(!deltas.contains(&DraftDelta::PackPassed));
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
                card_instance_ids: vec!["any".to_string()],
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
                card_instance_ids: vec!["any".to_string()],
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
                card_instance_ids: vec![card_id],
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
                card_instance_ids: vec![next_card_id],
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
                card_instance_ids: vec![card_id],
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
                card_instance_ids: vec![attempt2],
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
                card_instance_ids: vec![card_id],
            },
            None,
        );

        assert!(result.is_ok());
        assert!(session.seats_picked_this_round.get(1));
        assert!(!session.seats_picked_this_round.get(0));
    }

    /// A CommanderDraft pod at the kind's own defaults, with a caller-chosen
    /// pack size so the odd-leftover case is expressible.
    fn commander_session(cards_per_pack: u8) -> (DraftSession, FixturePackSource) {
        let procedure = DraftKind::CommanderDraft.procedure();
        let pod_size = procedure.pod_size;
        let config = DraftConfig {
            source: DraftSource::single_set("TST"),
            set_code: "TST".to_string(),
            kind: DraftKind::CommanderDraft,
            pod_size,
            cards_per_pack,
            pack_count: procedure.packs_per_player,
            min_deck_size: procedure.min_deck_size,
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
            cards_per_pack,
        };
        let session = DraftSession::new(config, seats, "TEST-CMD".to_string());
        (session, source)
    }

    /// Submit a pick of the first `n` cards in this seat's pack, without
    /// consulting `cards_per_pick` — these tests are about what the reducer
    /// does with a count, so the count must be the test's to choose.
    fn pick_n(
        session: &mut DraftSession,
        seat: u8,
        n: usize,
    ) -> Result<Vec<DraftDelta>, DraftError> {
        let card_instance_ids: Vec<String> =
            session.current_pack[seat as usize].as_ref().unwrap().0[..n]
                .iter()
                .map(|card| card.instance_id.clone())
                .collect();
        session::apply(
            session,
            DraftAction::Pick {
                seat,
                card_instance_ids,
            },
            None,
        )
    }

    /// CR 903.13b: one Commander Draft pick step takes two cards, and the round
    /// advances exactly once for it.
    #[test]
    fn two_card_pick_advances_round_once() {
        let (mut session, source) = commander_session(14);
        start_draft(&mut session, &source);

        let before = session.current_pack[0].as_ref().unwrap().0.len();
        let deltas = pick_n(&mut session, 0, 2).unwrap();
        let after = session.current_pack[0].as_ref().unwrap().0.len();

        // Positive reach-guard: the pick really happened, so the negative
        // assertions below cannot pass by way of an early error return.
        assert_eq!(before - after, 2, "one step removes two cards");
        assert_eq!(session.pools[0].len(), 2);
        assert_eq!(
            deltas
                .iter()
                .filter(|delta| matches!(delta, DraftDelta::CardPicked { .. }))
                .count(),
            2,
            "one delta per card"
        );

        // Multi-authority: seats 1-3 still owe picks, so the pack must not pass.
        assert!(
            !deltas
                .iter()
                .any(|delta| matches!(delta, DraftDelta::PackPassed)),
            "the pack cannot pass while three seats still owe a pick"
        );

        // The seat is done for the round only after the whole pair, not after
        // the first card.
        assert!(matches!(
            pick_n(&mut session, 0, 2),
            Err(DraftError::SeatAlreadyPickedThisRound { seat: 0 })
        ));
    }

    /// The count is read from the procedure, not hardcoded to two: the same
    /// code path must reject a two-card pick from a Premier seat.
    ///
    /// This is the assertion that distinguishes "reads `cards_per_pick`" from
    /// "Commander Draft takes two cards".
    #[test]
    fn premier_seat_cannot_pick_two_cards() {
        let (mut session, source) = test_session(4);
        start_draft(&mut session, &source);

        let result = pick_n(&mut session, 0, 2);

        assert!(
            matches!(
                result,
                Err(DraftError::WrongPickCardCount {
                    seat: 0,
                    expected: 1,
                    actual: 2
                })
            ),
            "got {result:?}"
        );
        // Nothing moved.
        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 14);
        assert!(session.pools[0].is_empty());
    }

    /// CR 903.13b with an odd pack: the final step of the pack takes the one
    /// card that remains, because the count is
    /// `min(cards_per_pick, remaining_pack_len)`.
    #[test]
    fn odd_pack_final_pick_takes_single_card() {
        // 13 = 6 pairs + 1 leftover.
        let (mut session, source) = commander_session(13);
        start_draft(&mut session, &source);

        // Six two-card steps for every seat, leaving one card in each pack.
        for _ in 0..6 {
            for seat in 0..4 {
                pick_n(&mut session, seat, 2).unwrap();
            }
        }
        assert_eq!(
            session.current_pack[0].as_ref().unwrap().0.len(),
            1,
            "reach-guard: the odd leftover is what the next pick faces"
        );

        // Asking for two now is wrong — the pack holds one. Built explicitly
        // rather than through `pick_n`, which cannot slice two ids out of a
        // one-card pack; the second id is deliberately absent from the pack,
        // which also pins that the count guard runs BEFORE presence resolution.
        let leftover_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let overreach = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![leftover_id, "nonexistent".to_string()],
            },
            None,
        );
        assert!(
            matches!(
                overreach,
                Err(DraftError::WrongPickCardCount {
                    seat: 0,
                    expected: 1,
                    actual: 2
                })
            ),
            "got {overreach:?}"
        );

        // Taking the single leftover succeeds.
        let deltas = pick_n(&mut session, 0, 1).unwrap();
        assert_eq!(
            deltas
                .iter()
                .filter(|delta| matches!(delta, DraftDelta::CardPicked { .. }))
                .count(),
            1
        );
        assert_eq!(session.pools[0].len(), 13);
    }

    /// U13a: the count `filter_for_player` publishes is the count
    /// `apply_pick_inner` enforces — for every kind, for CR 903.13b's odd-pack
    /// leftover, and for an emptied pack.
    ///
    /// The odd-leftover row is the discriminator: a constant `2`, or any
    /// kind-keyed lookup, publishes 2 where the pack holds 1. The clamp row
    /// reds an unclamped `cards_per_pick`, and the paused row reds a helper
    /// that grows a status term.
    #[test]
    fn view_publishes_the_count_the_reducer_enforces() {
        /// One card more than the published count is refused, one fewer is
        /// refused (where that is still a count), and exactly the published
        /// count succeeds and moves exactly that many cards.
        fn assert_published_count_is_enforced(session: &mut DraftSession, seat: u8) {
            let n = crate::view::filter_for_player(session, seat).required_pick_count;
            // Reach-guard: the published count is a real step. A published 0
            // would satisfy every refusal assertion below vacuously.
            assert!(n > 0, "expected a live pick step, got {n}");

            // Too many is refused. Built explicitly rather than through
            // `pick_n`, which cannot slice `n + 1` ids out of a pack holding
            // `n`; the extra id is deliberately absent from the pack, which
            // also pins that the count guard runs BEFORE presence resolution.
            let mut card_instance_ids: Vec<String> = session.current_pack[seat as usize]
                .as_ref()
                .unwrap()
                .0
                .iter()
                .take(n)
                .map(|card| card.instance_id.clone())
                .collect();
            card_instance_ids.push("nonexistent".to_string());
            let overreach = session::apply(
                session,
                DraftAction::Pick {
                    seat,
                    card_instance_ids,
                },
                None,
            );
            assert!(
                matches!(
                    overreach,
                    Err(DraftError::WrongPickCardCount {
                        seat: refused_seat,
                        expected,
                        actual,
                    }) if refused_seat == seat && expected == n && actual == n + 1
                ),
                "got {overreach:?}"
            );

            // Too few is refused, where "one fewer" is still a count.
            if n > 1 {
                let shortfall = pick_n(session, seat, n - 1);
                assert!(
                    matches!(shortfall, Err(DraftError::WrongPickCardCount { .. })),
                    "got {shortfall:?}"
                );
            }

            // Exactly `n` succeeds and moves exactly `n` cards.
            let before = session.pools[seat as usize].len();
            pick_n(session, seat, n).unwrap();
            assert_eq!(session.pools[seat as usize].len() - before, n);
        }

        // 1. CommanderDraft, full pack: two.
        let (mut session, source) = commander_session(14);
        start_draft(&mut session, &source);
        assert_eq!(
            crate::view::filter_for_player(&session, 0).required_pick_count,
            2
        );
        assert_published_count_is_enforced(&mut session, 0);

        // 2. CommanderDraft, odd leftover: one. THE DISCRIMINATOR — the kind
        // still says two, the pack says one, and CR 903.13b's "until all cards
        // in that draft round have been drafted" makes one correct.
        let (mut session, source) = commander_session(13); // 6 pairs + 1 leftover
        start_draft(&mut session, &source);
        for _ in 0..6 {
            for seat in 0..4 {
                pick_n(&mut session, seat, 2).unwrap();
            }
        }
        assert_eq!(
            crate::view::filter_for_player(&session, 0).required_pick_count,
            1
        );
        assert_eq!(
            crate::view::filter_for_player(&session, 0).pick_selection_mode,
            crate::types::PickSelectionMode::Ordered,
            "Commander Draft retains ordered selection on its one-card final step"
        );
        assert_published_count_is_enforced(&mut session, 0);

        // 3. Premier (CR 905.1a): one.
        let (mut premier, premier_source) = test_session(4);
        start_draft(&mut premier, &premier_source);
        assert_eq!(
            crate::view::filter_for_player(&premier, 0).required_pick_count,
            1
        );
        assert_published_count_is_enforced(&mut premier, 0);

        // 4. The clamp row: a `Drafting` seat whose pack is emptied publishes
        // 0, which reds a helper that returns `cards_per_pick` unclamped.
        // Seat 0 has just taken its leftover (scenario 2 above) while seats
        // 1-3 still hold theirs, so the round has NOT completed and the
        // session is still `Drafting` — the zero is the clamp firing on an
        // emptied pack, not a dead session.
        assert_eq!(session.status, DraftStatus::Drafting);
        assert_eq!(
            crate::view::filter_for_player(&session, 0).required_pick_count,
            0
        );
        // Paired positive, same session, same status.
        assert!(crate::view::filter_for_player(&session, 1).required_pick_count > 0);

        // 5. The no-status-gate pin. The count's authority is the clamp; the
        // status refusal's authority is `apply_pick_inner`'s guard. This row
        // is the only shape that distinguishes a pure `min` from a
        // status-aware helper, and it exists to keep those two authorities
        // from merging.
        let (mut paused, paused_source) = commander_session(14);
        start_draft(&mut paused, &paused_source);
        assert_eq!(
            crate::view::filter_for_player(&paused, 0).required_pick_count,
            2,
            "reach-guard"
        );
        paused.status = DraftStatus::Paused;
        assert_eq!(
            crate::view::filter_for_player(&paused, 0).required_pick_count,
            2,
            "the helper is a pure clamp: the status refusal belongs to apply_pick_inner"
        );
    }

    /// A pick whose count is wrong must leave the session exactly as it was —
    /// the validation-before-mutation discipline
    /// `apply_pick_with_effect_inner` already follows.
    #[test]
    fn wrong_pick_count_is_rejected_before_any_mutation() {
        let (mut session, source) = commander_session(14);
        start_draft(&mut session, &source);

        // Pre-state.
        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 14);
        assert!(session.pools[0].is_empty());

        assert!(matches!(
            pick_n(&mut session, 0, 1),
            Err(DraftError::WrongPickCardCount {
                seat: 0,
                expected: 2,
                actual: 1
            })
        ));
        assert!(matches!(
            pick_n(&mut session, 0, 3),
            Err(DraftError::WrongPickCardCount {
                seat: 0,
                expected: 2,
                actual: 3
            })
        ));

        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 14);
        assert!(session.pools[0].is_empty());
        assert!(!session.seats_picked_this_round.get(0));

        // Positive reach-guard: a valid pick from the same seat still works, so
        // the untouched state above is not the state of a dead session.
        pick_n(&mut session, 0, 2).unwrap();
        assert_eq!(session.pools[0].len(), 2);
    }

    /// The same discipline for a repeated id: `["a", "a"]` passes the count
    /// check, so a per-id removal loop would push "a" to the pool and only then
    /// fail. Nothing may move.
    #[test]
    fn duplicate_pick_ids_are_rejected_before_any_mutation() {
        let (mut session, source) = commander_session(14);
        start_draft(&mut session, &source);

        let first_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let second_id = session.current_pack[0].as_ref().unwrap().0[1]
            .instance_id
            .clone();

        let duplicate = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![first_id.clone(), first_id.clone()],
            },
            None,
        );
        assert!(
            matches!(
                duplicate,
                Err(DraftError::DuplicatePickCardId { seat: 0, .. })
            ),
            "got {duplicate:?}"
        );
        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 14);
        assert!(session.pools[0].is_empty());

        // A real id paired with a missing one passes count and distinctness and
        // fails presence — the first id must still be in the pack afterwards.
        let missing = session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![first_id.clone(), "nonexistent".to_string()],
            },
            None,
        );
        assert!(
            matches!(missing, Err(DraftError::CardNotInPack { .. })),
            "got {missing:?}"
        );
        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 14);
        assert!(session.pools[0].is_empty());

        // Positive reach-guard: the same two ids succeed when distinct.
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![first_id, second_id],
            },
            None,
        )
        .unwrap();
        assert_eq!(session.pools[0].len(), 2);
    }
}
