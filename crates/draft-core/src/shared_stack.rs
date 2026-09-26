//! Shared-stack pile drafting: the [`PackDistribution::SharedStackPiles`]
//! reducer, its single legality authority, and the forced decision the
//! timeout/disconnect path drives a stalled seat with.
//!
//! Winston Draft has NO Comprehensive Rules section. Grep-verified against
//! docs/MagicCompRules.txt: CR 905 is Conspiracy Draft and CR 903.13 is
//! Commander Draft; neither covers a shared face-down stack dealt through
//! take-or-decline piles, and no other section does. The procedural authority
//! is Wizards of the Coast, "Casual Formats" (2008-08-11),
//! <https://magic.wizards.com/en/news/feature/casual-formats-2008-08-11> --
//! deliberately no CR citation, the same discipline
//! `PostDraftPlay::TournamentPairings` applies to MTR tournament structure.
//! Deck construction is the one part the CR does cover: CR 100.2b (40-card
//! limited minimum) and CR 100.4b (the rest of the pool is the sideboard).
//!
//! This module mirrors `crate::pick_pass`'s division of labour exactly:
//! [`refusal_for`] is to a shared-stack decision what
//! `pick_pass::required_pick_count` is to a CR 903.13b pick step -- the reducer
//! enforces it and the view publishes it, so no display layer and no transport
//! re-derives legality from pile sizes.
//!
//! # Hidden information, and the threat model this accepts
//!
//! The reveal rule governs what the ENGINE PUBLISHES INTO A VIEW. It does not,
//! and cannot, govern what a process holding the whole [`DraftSession`] can
//! read. Server-authoritative pods are safe by construction: the seed is
//! server-minted, the reducer runs server-side, and every seat receives only
//! `view::filter_for_player`'s projection. **P2P pods are not**, and that is
//! pre-existing rather than new: a P2P host is a player, picks the seed
//! client-side, and runs this reducer inside its own wasm module, so it holds
//! `main_stack` in draw order.
//!
//! **Accepted for P2P, deliberately, and not gated to server-authoritative
//! pods** -- a P2P host already holds `packs_by_seat` (every unopened pack for
//! every seat) plus every seat's pools, for every existing kind. Gating this
//! one kind would be an asymmetry no other kind carries and would remove the
//! format's central use case.
//!
//! **The degree, stated honestly rather than smoothed over.** "No new class of
//! exposure" is true and, alone, misleading. In pick-and-pass the leak tells a
//! dishonest host the FUTURE, while the pack it decides from is already fully
//! visible to it by the rules. Here the leak tells the host, for every pile,
//! exactly what a decline will add and exactly what the forced draw will be: it
//! solves the only decision the format contains. That is strictly stronger than
//! the pick-and-pass precedent.
//!
//! **A two-party seed commitment was adjudicated and REFUSED as a mitigation
//! for that exposure.** Each peer contributing 32 bits at the first-contact
//! version gate, with `rng_seed = host ^ guest`, does not close the leak: the
//! host computes the XOR, so it holds both halves and therefore the seed, and
//! it then runs this reducer and holds the stack in draw order. XOR removes the
//! host's ability to CHOOSE a seed; it does not remove its ability to READ the
//! stack that seed produces. What it does buy is the removal of seed GRINDING
//! (re-rolling until the stack looks good), which is a real but strictly
//! smaller and entirely kind-independent threat -- so it is a general P2P-draft
//! follow-up, not part of a draft kind.

use crate::types::{
    DraftDelta, DraftError, DraftSession, DraftStatus, SharedStackDecisionRecord,
    SharedStackPileDecision, SharedStackRefusal, SharedStackState, SHARED_STACK_HISTORY_CAPACITY,
};

/// How many piles a [`PackDistribution::SharedStackPiles`] row deals, or the
/// refusal a row that declares none deserves.
///
/// **Single authority for `pile_count == 0`.** Every site that turns a
/// `pile_count` into a length goes through here -- `apply_start_draft`'s
/// pre-flight arm, its dealing arm, and `validate_persisted_snapshot` -- so a
/// zero is refused once, before anything is built, rather than once per
/// indexing site.
///
/// Unreachable through any kind that exists today: the one
/// `SharedStackPiles` row is `pile_count: 3`, and
/// `max_shared_stack_piles_matches_procedure_table` folds `DraftKind::ALL` to
/// prove it. It exists because the variant is deliberately designed so that a
/// future row differing only in pile count is a different `pile_count` rather
/// than a sibling variant -- so a zero is a thing the NEXT author can write,
/// and what they get for it must be this refusal and not the
/// `inspected[0] = piles[0].len()` panic that `apply_start_draft` and
/// `apply_shared_stack_decision`'s turn-end reset would otherwise hand them.
///
/// [`PackDistribution::SharedStackPiles`]: crate::types::PackDistribution::SharedStackPiles
pub fn piles_needed(pile_count: u8) -> Result<usize, DraftError> {
    if pile_count == 0 {
        return Err(DraftError::InvalidSharedStackConfiguration {
            reason: "a shared-stack distribution must deal at least one pile".to_string(),
        });
    }
    Ok(usize::from(pile_count))
}

/// **Single authority** for shared-stack decision legality.
///
/// A **pure predicate over the state**, with no status term: the refusal for a
/// decision made outside [`DraftStatus::Drafting`] belongs to
/// [`apply_shared_stack_decision`]'s own guard alone, and a status term here
/// would mint a second authority for it. `None` means legal; the reducer wraps
/// a `Some` as [`DraftError::SharedStackDecisionRefused`] and nothing anywhere
/// else re-derives the verdict.
///
/// The adjudication, with piles 0-indexed, `n` = pile count, `c` = the cursor:
///
/// * **Take** `c` is legal iff `piles[c]` is non-empty.
/// * **Decline** `c` is legal iff some pile in `c+1..n` is non-empty, **or**
///   `c` is the final pile and `main_stack.len() >= 2`.
/// * Either requires `seat == active_seat && pile == cursor`.
///
/// The `>= 2` is exactly the resource cost the published procedure names: WotC
/// "Casual Formats" says "each time a player puts a pile back, the top card of
/// the main stack is added to it", and the final-pile order (corroborated by
/// draftsim) is that the card is added to the third pile AND THEN the declining
/// player drafts the new top card -- one card onto the pile, one card into the
/// pool.
///
/// Deadlock-freedom is structural rather than asserted: a pile is refilled the
/// instant it is taken, so while the main stack is non-empty every pile is
/// non-empty, and a pile can only be empty if the stack was already empty when
/// that pile was last taken (the stack never grows). With `stack > 0` a take is
/// always available; with `stack == 0` the decline predicate maintains
/// "SOME pile at or after the cursor is non-empty" across every advance, and at
/// the final pile that existential collapses to the final pile itself -- so a
/// take is available there.
///
/// State the invariant as the EXISTENTIAL, not as "the next pile is non-empty":
/// the predicate lets the cursor advance from pile 0 to pile 1 when only pile 2
/// holds cards, so `piles[1]` can legally be empty at the cursor. Weakening the
/// predicate to a next-pile test would look like it preserved this proof and
/// would not.
pub fn refusal_for(
    state: &SharedStackState,
    seat: u8,
    pile: u8,
    decision: SharedStackPileDecision,
) -> Option<SharedStackRefusal> {
    if seat != state.active_seat || pile != state.cursor {
        return Some(SharedStackRefusal::PileNotActive);
    }
    let index = usize::from(pile);
    let Some(cursor_pile) = state.piles.get(index) else {
        // A cursor past the end is a corrupt state, not a legal decision.
        return Some(SharedStackRefusal::PileNotActive);
    };
    match decision {
        SharedStackPileDecision::Take => {
            if cursor_pile.is_empty() {
                Some(SharedStackRefusal::PileEmpty)
            } else {
                None
            }
        }
        SharedStackPileDecision::Decline => {
            let later_pile_available = state.piles.iter().skip(index + 1).any(|p| !p.is_empty());
            let is_final_pile = index + 1 == state.piles.len();
            if later_pile_available || (is_final_pile && state.main_stack.len() >= 2) {
                None
            } else {
                Some(SharedStackRefusal::NoGuaranteedCard)
            }
        }
    }
}

/// The decision a timed-out or disconnected seat is driven with. NOT an AI and
/// not a pile evaluation: it is the first ALWAYS-LEGAL move, asked of the one
/// legality authority rather than re-derived here.
///
/// `None` has TWO causes and they are not the same:
///
/// * `seat != state.active_seat` -- both entries are refused
///   [`SharedStackRefusal::PileNotActive`]. **REACHABLE AND CORRECT**: a
///   non-active seat has no turn to advance, and the caller must refuse rather
///   than drive somebody else's turn. This is the live case on the
///   disconnect-expiry path, which passes the DISCONNECTED seat, not the active
///   one.
/// * `seat == state.active_seat` and both entries refused -- unreachable while
///   drafting, and `some_decision_is_always_legal_for_the_active_seat_while_drafting`
///   proves it FOR THE ACTIVE SEAT, which is the only seat its walk asks about.
///
/// Deriving the answer from stack emptiness instead is wrong at TURN START,
/// where no decline has run: a reachable state is
/// `stack == 0, cursor == 0, piles[0].is_empty()` with a later pile non-empty,
/// where `Take` is refused `PileEmpty` and `Decline` is the legal move.
pub fn forced_decision(state: &SharedStackState, seat: u8) -> Option<SharedStackPileDecision> {
    SharedStackPileDecision::ALL
        .into_iter()
        .find(|decision| refusal_for(state, seat, state.cursor, *decision).is_none())
}

/// Apply one whole shared-stack turn decision.
///
/// Validates completely before mutating anything, as `pick_pass::apply_pick_inner`
/// does: a refused decision must leave the session byte-identical.
pub fn apply_shared_stack_decision(
    session: &mut DraftSession,
    seat: u8,
    pile: u8,
    decision: SharedStackPileDecision,
) -> Result<Vec<DraftDelta>, DraftError> {
    // The status guard is this function's own authority. `refusal_for` is
    // deliberately status-free, so this is the only place a non-`Drafting`
    // session is refused.
    if session.status != DraftStatus::Drafting {
        return Err(DraftError::InvalidTransition {
            from: session.status,
            action: "SharedStackDecision".to_string(),
        });
    }

    let pod_size = session.seats.len() as u8;
    if seat >= pod_size {
        return Err(DraftError::SeatOutOfRange { seat, pod_size });
    }

    // The accessor's `Err` surfaces unchanged: a shared-stack session with no
    // stack is reachable only from a hand-built or corrupt session, which is
    // why `validate_persisted_snapshot` refuses it at import.
    if let Some(reason) = refusal_for(session.shared_stack()?, seat, pile, decision) {
        return Err(DraftError::SharedStackDecisionRefused { seat, pile, reason });
    }

    let pile_index = usize::from(pile);
    let seat_count = session.seats.len() as u8;
    // Disjoint field borrows: the pile state and the receiving pool are
    // different fields of the same session.
    let state = session
        .shared_stack
        .as_mut()
        .expect("`refusal_for` above read the state through the accessor");
    let pool = &mut session.pools[usize::from(seat)];

    // Captured BEFORE the `match`, which is the whole content of the field: a
    // `Take` empties the pile and refills it with one card, and a `Decline`
    // appends one, so after the `match` the real height at the moment of the
    // decision is gone. For a decline this is also exactly the prefix the
    // deciding seat had inspected.
    let pile_size = state.piles[pile_index].len();

    // GROW BEFORE WRITING, so a snapshot that predates the field becomes a
    // session that records notices rather than one that silently never will.
    // `forced_draws` carries `#[serde(default)]`, and
    // `validate_persisted_snapshot` therefore ACCEPTS an empty vector as "no
    // seat has a pending notice" — but nothing else would ever resize it, and a
    // `get_mut` on an empty vector no-ops forever. This is the one write path
    // every decision takes, so it is where the migration belongs. A vector
    // already the right length is untouched.
    state.forced_draws.resize(usize::from(seat_count), None);
    // This seat is acting again, so whatever its last forced draw was, it has
    // now been seen -- or was never going to be. Cleared BEFORE the `match`, so
    // the forced-draw arm below can write this turn's card into a slot it knows
    // is empty rather than racing its own clear.
    if let Some(pending) = state.forced_draws.get_mut(usize::from(seat)) {
        *pending = None;
    }

    let turn_ends = match decision {
        // WotC: "Each time a player takes a pile, it's replaced by the top card
        // of the main stack to form a new one-card pile."
        SharedStackPileDecision::Take => {
            pool.append(&mut state.piles[pile_index]);
            if let Some(card) = state.main_stack.pop() {
                state.piles[pile_index].push(card);
            }
            true
        }
        // WotC: "each time a player puts a pile back, the top card of the main
        // stack is added to it."
        SharedStackPileDecision::Decline => {
            if let Some(card) = state.main_stack.pop() {
                // APPEND, never insert: this is what puts the new card BEYOND
                // `inspected[pile_index]` structurally, so the declining seat
                // cannot see what it just added. A structural guarantee, not a
                // filter that could be forgotten.
                state.piles[pile_index].push(card);
            }
            if pile_index + 1 < state.piles.len() {
                state.cursor += 1;
                let cursor = usize::from(state.cursor);
                // MANDATORY second write site of the `inspected` contract.
                // Without it the active seat's view publishes an empty
                // `revealed` for the pile it is currently inspecting.
                state.inspected[cursor] = state.piles[cursor].len();
                false
            } else {
                // WotC: "If Player A didn't choose any of the three piles, that
                // player must take the top card from the main stack, no matter
                // what it is." `refusal_for` proved `main_stack.len() >= 2`
                // before the pop above, so one card remains here; the `if let`
                // keeps the reducer panic-free rather than asserting it.
                if let Some(card) = state.main_stack.pop() {
                    // Recorded before the move into the pool, because this is
                    // the ONE card this seat receives without having seen it:
                    // the format takes it "no matter what it is". Every other
                    // card a seat drafts was face up in the pile it took. See
                    // `SharedStackState::forced_draws`.
                    if let Some(pending) = state.forced_draws.get_mut(usize::from(seat)) {
                        *pending = Some(card.clone());
                    }
                    pool.push(card);
                }
                true
            }
        }
    };

    // EVERY APPLIED DECISION, not every completed turn, because that is what
    // the field's contract says and what its acknowledgement consumer needs:
    // the predicate is `after.decisions > before.decisions`, and the most
    // common Winston action -- a non-final decline -- names no cards and adds
    // nothing to a pool, so this counter is the ONLY thing it moves. Counting
    // turns here would leave that decision unacknowledgeable.
    //
    // Placed after the `match` rather than before it because the `match` is
    // what APPLIES the decision; a decision refused above returned early and
    // never reaches this line, so the counter stays a count of applied
    // decisions and a refusal still leaves the session byte-identical.
    state.decisions += 1;

    // The SECOND field this one event moves, and it sits here for exactly the
    // reason `decisions` does: a refused decision returned above and never
    // reaches this line, so a refusal still leaves the session byte-identical
    // and the history stays a record of APPLIED decisions.
    //
    // PUBLIC INFORMATION ONLY -- seat, pile, decision, height. See
    // `SharedStackDecisionRecord`: the pile's contents are the format's only
    // secret and must never be recorded here.
    state.history.push(SharedStackDecisionRecord {
        seat,
        pile,
        decision,
        pile_size,
    });
    // Trim the FRONT so the retained window is the most recent decisions,
    // oldest first. `drain` rather than `remove` so a history that somehow
    // arrived over capacity (an imported snapshot is validated, but a
    // hand-built session is not) converges in one step instead of one decision
    // at a time.
    if state.history.len() > SHARED_STACK_HISTORY_CAPACITY {
        let excess = state.history.len() - SHARED_STACK_HISTORY_CAPACITY;
        state.history.drain(..excess);
    }

    if turn_ends {
        // Modulo `seats.len()`, NEVER `config.pod_size`, whose serde default is
        // an unconditional 8 regardless of kind.
        state.active_seat = (state.active_seat + 1) % seat_count;
        state.cursor = 0;
        // The previous turn's entitlement lapses: the piles are face down again
        // and belong to nobody, so the engine never re-publishes a previous
        // turn's prefix. First write site of the `inspected` contract.
        state.inspected.iter_mut().for_each(|seen| *seen = 0);
        // Index 0 exists on both paths a session can arrive by: `StartDraft`
        // refuses a zero-pile row through `piles_needed`, and an imported
        // snapshot is held to `piles.len() == piles_needed(pile_count)` by
        // `validate_persisted_snapshot`.
        state.inspected[0] = state.piles[0].len();
    }

    let mut deltas = vec![DraftDelta::SharedStackDecisionApplied {
        seat,
        pile,
        decision,
    }];

    // The draft ends exactly when the stack is empty and every pile is empty,
    // which is also exactly when every card is in some player's pool.
    if state.main_stack.is_empty() && state.piles.iter().all(Vec::is_empty) {
        session.status = DraftStatus::Deckbuilding;
        deltas.push(DraftDelta::TransitionedTo {
            status: DraftStatus::Deckbuilding,
        });
    }

    Ok(deltas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_source::FixturePackSource;
    use crate::session;
    use crate::types::*;
    use engine::types::player::PlayerId;

    /// 2 seats x 3 packs x 15 cards = 90 cards, which is the size V5's
    /// termination leg asserts against.
    const CARDS_PER_PACK: u8 = 15;

    /// V14 leg (c)'s pinned golden pair for seed 20_260_913. Recorded from the
    /// implementation, and re-recordable ONLY together with a deliberate change
    /// to the seeded draw order (shuffle first, starting-seat draw second).
    const GOLDEN_STARTING_SEAT: u8 = 0;
    const GOLDEN_STACK_TOP_THREE: [&str; 3] = ["TST-0-2-14", "TST-0-2-4", "TST-0-2-12"];

    fn winston_session(pod_size: u8, rng_seed: u64) -> (DraftSession, FixturePackSource) {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Winston,
            pod_size,
            cards_per_pack: CARDS_PER_PACK,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed,
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
            cards_per_pack: CARDS_PER_PACK,
        };
        (
            DraftSession::new(config, seats, "WIN-001".to_string()),
            source,
        )
    }

    fn started(pod_size: u8, rng_seed: u64) -> DraftSession {
        let (mut session, source) = winston_session(pod_size, rng_seed);
        session::apply(&mut session, DraftAction::StartDraft, Some(&source))
            .expect("a human-only Winston pod starts");
        session
    }

    fn state(session: &DraftSession) -> &SharedStackState {
        session.shared_stack.as_ref().expect("shared stack present")
    }

    fn decide(
        session: &mut DraftSession,
        decision: SharedStackPileDecision,
    ) -> Result<Vec<DraftDelta>, DraftError> {
        let (seat, pile) = {
            let s = state(session);
            (s.active_seat, s.cursor)
        };
        session::apply(
            session,
            DraftAction::SharedStackDecision {
                seat,
                pile,
                decision,
            },
            None,
        )
    }

    /// Decline every pile, which is the only way a seat drafts a card it has
    /// not seen. Returns the seat that took the forced draw.
    fn decline_through_the_forced_draw(session: &mut DraftSession) -> u8 {
        let seat = state(session).active_seat;
        let piles = state(session).piles.len();
        for _ in 0..piles {
            decide(session, SharedStackPileDecision::Decline)
                .expect("declining a pile with a later pile or a stocked stack behind it");
        }
        seat
    }

    /// Total cards across the stack, every pile and every pool. Models
    /// `pick_pass`'s conservation helper.
    fn assert_winston_conservation(session: &DraftSession, expected_total: usize) {
        let s = state(session);
        let live: usize = s.main_stack.len() + s.piles.iter().map(Vec::len).sum::<usize>();
        let pooled: usize = session.pools.iter().map(Vec::len).sum();
        assert_eq!(
            live + pooled,
            expected_total,
            "every card is in the stack, a pile, or exactly one pool"
        );
        let mut ids: Vec<&str> = s
            .main_stack
            .iter()
            .chain(s.piles.iter().flatten())
            .chain(session.pools.iter().flatten())
            .map(|card| card.instance_id.as_str())
            .collect();
        ids.sort_unstable();
        let distinct = ids.len();
        ids.dedup();
        assert_eq!(distinct, ids.len(), "no card was copied instead of moved");
    }

    /// Which decision a driving walk prefers when BOTH are legal.
    ///
    /// A typed axis rather than a `bool`, and it exists because a take-first
    /// walk is DEGENERATE for half this module: `Take` is legal at the cursor
    /// for almost every turn, so a take-first walk never declines, never
    /// advances the cursor, and NEVER REACHES THE FINAL-PILE DECLINE or the
    /// forced draw it triggers. Measured: a mutation that made the forced draw
    /// both pool a card and leave a copy on the pile survived a take-first
    /// walk and died under `DeclineFirst`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum WalkPolicy {
        TakeFirst,
        DeclineFirst,
    }

    impl WalkPolicy {
        /// The two decisions in this policy's preference order. Derived from
        /// `SharedStackPileDecision::ALL`, so it cannot go narrow.
        const ALL: [WalkPolicy; 2] = [WalkPolicy::TakeFirst, WalkPolicy::DeclineFirst];

        fn preference(self) -> [SharedStackPileDecision; 2] {
            let mut order = SharedStackPileDecision::ALL;
            if self == WalkPolicy::DeclineFirst {
                order.reverse();
            }
            order
        }
    }

    /// The first legal move for the active seat in this policy's order, chosen
    /// by the single authority so the walk never re-derives legality itself.
    ///
    /// `WalkPolicy::TakeFirst` is exactly `forced_decision`'s own order, which
    /// is asserted below so the two cannot drift.
    fn drive_one_decision(
        session: &mut DraftSession,
        policy: WalkPolicy,
    ) -> SharedStackPileDecision {
        let (seat, decision) = {
            let s = state(session);
            let chosen = policy
                .preference()
                .into_iter()
                .find(|d| refusal_for(s, s.active_seat, s.cursor, *d).is_none())
                .expect("the active seat always has a legal move");
            if policy == WalkPolicy::TakeFirst {
                assert_eq!(
                    Some(chosen),
                    forced_decision(s, s.active_seat),
                    "TakeFirst must agree with `forced_decision`'s own order"
                );
            }
            (s.active_seat, chosen)
        };
        let pile = state(session).cursor;
        session::apply(
            session,
            DraftAction::SharedStackDecision {
                seat,
                pile,
                decision,
            },
            None,
        )
        .expect("the forced decision is legal by construction");
        decision
    }

    /// V3 + V4 + V5 + V7, driven through the REAL reducer to completion.
    ///
    /// Four claims share one walk because each is an assertion after every
    /// decision of the same full draft; splitting them would run the same
    /// 90-card draft four times to assert four invariants of one trajectory.
    #[test]
    fn full_two_seat_winston_draft_reaches_deckbuilding() {
        for policy in WalkPolicy::ALL {
            let mut session = started(2, 4242);
            let total = 2 * 3 * usize::from(CARDS_PER_PACK);
            assert_eq!(total, 90);
            assert_eq!(total, session.total_pack_cards() * session.seats.len());
            assert_winston_conservation(&session, total);

            let mut states_with_nonempty_stack = 0usize;
            let mut decisions = 0usize;
            let mut taken = 0usize;
            let mut declined = 0usize;
            let mut cursor_reached_the_final_pile = 0usize;
            // A hard cap that FAILS on non-termination rather than hanging.
            // Every turn moves at least one card into a pool and costs at most
            // `pile_count` decisions, so `total * 4` is generous and finite.
            let cap = total * 4;
            while session.status == DraftStatus::Drafting {
                assert!(decisions < cap, "{policy:?}: the draft did not terminate");
                if usize::from(state(&session).cursor) + 1 == state(&session).piles.len() {
                    cursor_reached_the_final_pile += 1;
                }
                // V7: every state of this walk has a legal move for the active
                // seat. `drive_one_decision` asserts it by construction.
                match drive_one_decision(&mut session, policy) {
                    SharedStackPileDecision::Take => taken += 1,
                    SharedStackPileDecision::Decline => declined += 1,
                }
                decisions += 1;

                // The `decisions` contract, asserted after EVERY decision: it
                // counts APPLIED DECISIONS, so it equals this walk's own count
                // exactly. Under `DeclineFirst` the FIRST non-turn-ending
                // decline reds this if the increment sits inside the
                // turn-ending branch -- which is what an acknowledgement
                // predicate of `after.decisions > before.decisions` needs,
                // since that decline moves nothing else observable.
                assert_eq!(
                    usize::try_from(state(&session).decisions).unwrap(),
                    decisions,
                    "{policy:?}: `decisions` must count every applied decision"
                );

                let s = state(&session);
                // V3: the pile-refill invariant, asserted after EVERY decision.
                if !s.main_stack.is_empty() {
                    states_with_nonempty_stack += 1;
                    assert!(
                        s.piles.iter().all(|pile| !pile.is_empty()),
                        "{policy:?}: a non-empty main stack implies every pile is non-empty"
                    );
                }
                // V4: conservation, asserted after EVERY decision.
                assert_winston_conservation(&session, total);
            }

            // Reach-guards, so no assertion above can pass vacuously.
            assert!(
                states_with_nonempty_stack > 0,
                "{policy:?}: the refill invariant was never actually observed"
            );
            assert!(taken > 0, "{policy:?}: no take was ever exercised");
            assert!(declined > 0, "{policy:?}: no decline was ever exercised");
            // The whole-draft total, with `declined > 0` and `taken > 0` above
            // as its reach-guards: a walk of nothing but turn-ending decisions
            // could not tell the two increment sites apart.
            assert_eq!(
                usize::try_from(state(&session).decisions).unwrap(),
                decisions,
                "{policy:?}: the final count is every applied decision"
            );
            if policy == WalkPolicy::DeclineFirst {
                // The branch a take-first walk cannot reach: the forced draw
                // that a final-pile decline triggers.
                assert!(
                    cursor_reached_the_final_pile > 0,
                    "the decline-first walk must reach the final pile"
                );
            }

            // V5: termination with every card in exactly one pool.
            assert_eq!(session.status, DraftStatus::Deckbuilding);
            assert_eq!(session.pools[0].len() + session.pools[1].len(), total);
            assert!(!session.pools[0].is_empty());
            assert!(!session.pools[1].is_empty());
            let s = state(&session);
            assert!(s.main_stack.is_empty());
            assert!(s.piles.iter().all(Vec::is_empty));
            // Retained, not cleared, past the terminal transition.
            assert!(session.shared_stack.is_some());
        }
    }

    /// V7's scope, stated in the name: this walk only ever asks about the
    /// ACTIVE seat, so it does not prove `forced_decision` non-`None` for a
    /// non-active seat -- `disconnect_expiry_refuses_a_non_active_winston_seat`
    /// in `server-core` covers that case and asserts the opposite.
    /// THE FORCED MOVE IS `SharedStackPileDecision::ALL`'s FIRST LEGAL ENTRY,
    /// IN DECLARATION ORDER.
    ///
    /// This is the whole contract a timed-out or disconnected seat's move rests
    /// on, and it is now the ONLY authority for it: the P2P host used to scan
    /// the published `legality` vector for its first unrefused entry, which is
    /// the same algorithm over a different ordering source -- the engine folds
    /// `ALL`, the client folded whatever order the view serialized. They agreed
    /// by coincidence. The host asks `shared_stack_forced_decision` now, so
    /// reordering `ALL` changes which move every timed-out seat makes, and that
    /// should redden a test rather than ship.
    ///
    /// Both arms are MEASURED over the walk, not assumed: the counters assert
    /// the fixture actually reached a state where both moves were legal (so
    /// "first in order" is a real choice and not a forced one) AND a state where
    /// only the second was (so the fold genuinely advances past a refusal).
    #[test]
    fn a_forced_decision_is_the_first_legal_entry_of_all_in_declaration_order() {
        let mut session = started(2, 7);
        let mut both_legal = 0usize;
        let mut only_the_second = 0usize;

        while session.status == DraftStatus::Drafting {
            let (seat, pile, forced) = {
                let s = state(&session);
                let legal: Vec<SharedStackPileDecision> = SharedStackPileDecision::ALL
                    .into_iter()
                    .filter(|d| refusal_for(s, s.active_seat, s.cursor, *d).is_none())
                    .collect();
                let forced =
                    forced_decision(s, s.active_seat).expect("the active seat always has a move");
                assert_eq!(
                    forced, legal[0],
                    "forced_decision must return ALL's first legal entry, not any legal entry"
                );
                if legal.len() == SharedStackPileDecision::ALL.len() {
                    both_legal += 1;
                    // THE LITERAL, deliberately -- not `ALL[0]`. Writing `ALL[0]`
                    // here makes the assertion tautological: reorder `ALL` and
                    // both sides move together, so the one change this test most
                    // needs to catch sails through. (Measured: it did.) With
                    // nothing to separate the two moves, a timed-out seat TAKES.
                    assert_eq!(forced, SharedStackPileDecision::Take);
                } else {
                    only_the_second += 1;
                }
                (s.active_seat, s.cursor, forced)
            };
            // The answer is always one the reducer itself accepts -- a forced
            // move the reducer would refuse is the failure this guards.
            apply_shared_stack_decision(&mut session, seat, pile, forced)
                .expect("a forced decision must be applicable");
        }

        assert!(
            both_legal > 0,
            "the walk never reached a state with two legal moves, so ordering was never exercised"
        );
        assert!(
            only_the_second > 0,
            "the walk never reached a state where ALL[0] was refused, so the fold never advanced"
        );
    }

    #[test]
    fn some_decision_is_always_legal_for_the_active_seat_while_drafting() {
        for policy in WalkPolicy::ALL {
            let mut session = started(2, 7);
            let mut observed = 0usize;
            while session.status == DraftStatus::Drafting {
                let s = state(&session);
                assert!(
                    forced_decision(s, s.active_seat).is_some(),
                    "{policy:?}: no legal decision for the active seat at cursor {} (stack {}, piles {:?})",
                    s.cursor,
                    s.main_stack.len(),
                    s.piles.iter().map(Vec::len).collect::<Vec<_>>()
                );
                observed += 1;
                drive_one_decision(&mut session, policy);
                assert!(observed < 1024, "{policy:?}: the draft did not terminate");
            }
            assert!(
                observed > 0,
                "{policy:?}: the walk must reach at least one live state"
            );
        }
    }

    /// V6. BOTH directions in one test: refused at `stack == 1`, accepted at
    /// `stack == 2` with the declining seat's pool growing by exactly one. The
    /// paired positive is what stops a blanket refusal from passing.
    #[test]
    fn final_pile_decline_needs_two_stack_cards() {
        for (stack_len, expect_legal) in [(1usize, false), (2usize, true)] {
            let mut session = started(2, 11);
            let total = 2 * 3 * usize::from(CARDS_PER_PACK);
            {
                let s = session.shared_stack.as_mut().unwrap();
                // Put the cursor on the FINAL pile with every later pile
                // exhausted, so the `>= 2` disjunct is the only thing that can
                // make the decline legal.
                let last = s.piles.len() - 1;
                s.cursor = last as u8;
                s.inspected[last] = s.piles[last].len();
                let surplus: Vec<DraftCardInstance> = s.main_stack.drain(stack_len..).collect();
                // Conservation is an invariant of the fixture too: the cards
                // removed from the stack go into a pool rather than vanishing.
                session.pools[0].extend(surplus);
            }
            assert_winston_conservation(&session, total);
            let seat = state(&session).active_seat;
            let pool_before = session.pools[usize::from(seat)].len();
            let result = decide(&mut session, SharedStackPileDecision::Decline);
            if expect_legal {
                result.expect("a final-pile decline is legal with two stack cards");
                assert_eq!(
                    session.pools[usize::from(seat)].len(),
                    pool_before + 1,
                    "the forced draw put exactly one card into the declining seat's pool"
                );
                assert_winston_conservation(&session, total);
            } else {
                let pile = (state(&session).piles.len() - 1) as u8;
                assert_eq!(
                    result,
                    Err(DraftError::SharedStackDecisionRefused {
                        seat,
                        pile,
                        reason: SharedStackRefusal::NoGuaranteedCard,
                    })
                );
                assert_eq!(session.pools[usize::from(seat)].len(), pool_before);
                assert_eq!(state(&session).main_stack.len(), 1);
            }
        }
    }

    /// V12. Nothing moves on a refusal, and the paired positive proves the very
    /// same decision succeeds for the seat whose turn it is.
    #[test]
    fn non_active_seat_decision_is_refused() {
        let mut session = started(2, 3);
        let active = state(&session).active_seat;
        let intruder = (active + 1) % 2;
        let cursor = state(&session).cursor;
        let before = session.shared_stack.clone();
        let pools_before = session.pools.clone();

        let refused = session::apply(
            &mut session,
            DraftAction::SharedStackDecision {
                seat: intruder,
                pile: cursor,
                decision: SharedStackPileDecision::Take,
            },
            None,
        );
        assert_eq!(
            refused,
            Err(DraftError::SharedStackDecisionRefused {
                seat: intruder,
                pile: cursor,
                reason: SharedStackRefusal::PileNotActive,
            })
        );
        assert_eq!(session.shared_stack, before);
        assert_eq!(session.pools, pools_before);

        // Paired positive: the identical decision from the active seat works.
        session::apply(
            &mut session,
            DraftAction::SharedStackDecision {
                seat: active,
                pile: cursor,
                decision: SharedStackPileDecision::Take,
            },
            None,
        )
        .expect("the active seat's identical decision succeeds");
        assert_ne!(session.shared_stack, before);
    }

    /// V13. The double-send shape specifically: `pile` is a check, not a
    /// selector, so the second frame of one laggy click cannot land on the next
    /// pile.
    #[test]
    fn decision_on_a_non_cursor_pile_is_refused() {
        let mut session = started(2, 5);
        let seat = state(&session).active_seat;
        assert_eq!(state(&session).cursor, 0);

        session::apply(
            &mut session,
            DraftAction::SharedStackDecision {
                seat,
                pile: 0,
                decision: SharedStackPileDecision::Decline,
            },
            None,
        )
        .expect("the first decline is legal");
        assert_eq!(state(&session).cursor, 1);

        let piles_before = state(&session).piles.clone();
        let second = session::apply(
            &mut session,
            DraftAction::SharedStackDecision {
                seat,
                pile: 0,
                decision: SharedStackPileDecision::Decline,
            },
            None,
        );
        assert_eq!(
            second,
            Err(DraftError::SharedStackDecisionRefused {
                seat,
                pile: 0,
                reason: SharedStackRefusal::PileNotActive,
            })
        );
        assert_eq!(
            state(&session).cursor,
            1,
            "the cursor did not advance twice"
        );
        assert_eq!(state(&session).piles, piles_before);
    }

    /// V14, in three legs.
    ///
    /// `FixturePackSource::generate_pack` takes `_rng`, so pack generation
    /// consumes nothing from the stream and leg (c) discriminates the
    /// shuffle-then-draw ORDER and nothing else.
    #[test]
    fn same_seed_yields_same_stack_and_starting_seat() {
        // (a) one seed, two runs, identical stack order and starting seat.
        let left = started(2, 123_456);
        let right = started(2, 123_456);
        assert_eq!(state(&left).main_stack, state(&right).main_stack);
        assert_eq!(state(&left).piles, state(&right).piles);
        assert_eq!(state(&left).starting_seat, state(&right).starting_seat);
        assert_eq!(state(&left).starting_seat, state(&left).active_seat);

        // (b) over >= 8 distinct seeds neither is a constant.
        let mut starting_seats = std::collections::HashSet::new();
        let mut stack_orders = std::collections::HashSet::new();
        for seed in 0..12u64 {
            let session = started(2, seed);
            starting_seats.insert(state(&session).starting_seat);
            stack_orders.insert(
                state(&session)
                    .main_stack
                    .iter()
                    .map(|card| card.instance_id.clone())
                    .collect::<Vec<_>>(),
            );
        }
        assert!(
            starting_seats.len() >= 2,
            "the starting seat is a constant: {starting_seats:?}"
        );
        assert!(stack_orders.len() >= 2, "the stack order is a constant");

        // (c) THE PINNED GOLDEN PAIR. This leg encodes the ORDER in which the
        // one `ChaCha20Rng` stream is consumed -- `stack.shuffle(&mut rng)`
        // FIRST, `rng.random_range(0..seat_count)` SECOND. Swapping those two
        // draws changes both values for a given seed, so this pair may be
        // re-recorded ONLY together with a deliberate change to that order.
        let pinned = started(2, 20_260_913);
        let top_three: Vec<&str> = state(&pinned)
            .main_stack
            .iter()
            .rev()
            .take(3)
            .map(|card| card.instance_id.as_str())
            .collect();
        assert_eq!(
            (state(&pinned).starting_seat, top_three),
            (GOLDEN_STARTING_SEAT, GOLDEN_STACK_TOP_THREE.to_vec()),
            "the seeded draw order changed"
        );
    }

    /// V15 + V16, inverted: a shared-stack pod ADMITS a bot seat, at both
    /// seams the two deleted refusal tests guarded.
    ///
    /// Replaces `winston_refuses_a_bot_seat` and
    /// `winston_refuses_replace_seat_with_bot`, and carries both of their
    /// paired positives forward unchanged -- a Premier pod still admits a bot
    /// at both seams, and a 4-seat all-human Winston pod still starts with
    /// three piles (the leg that catches a guard written against the
    /// `human_seats` scalar, a per-kind `2` that stops matching at pod 4).
    ///
    /// The bot seat is not decorative here: `StartDraft` must still deal, and
    /// the started session must still hand the pod a live turn, so this asserts
    /// the dealt shape rather than just the absence of an `Err`.
    #[test]
    fn a_winston_pod_admits_a_bot_seat() {
        let (mut session, source) = winston_session(2, 1);
        session.seats[1] = DraftSeat::Bot {
            name: "Bot".to_string(),
        };
        session::apply(&mut session, DraftAction::StartDraft, Some(&source))
            .expect("a Winston pod admits a bot seat");
        assert_eq!(session.status, DraftStatus::Drafting);
        assert_eq!(state(&session).piles.len(), 3);
        // Dealt, not merely started: every pile holds its opening card and the
        // rest of the 90-card pod is face down.
        assert!(state(&session).piles.iter().all(|pile| pile.len() == 1));
        assert_eq!(state(&session).main_stack.len(), 90 - 3);
        // The turn is live and addressed at the cursor, so a bot seat that owns
        // it has something to decide.
        assert_eq!(state(&session).cursor, 0);

        // A mid-draft swap is accepted too -- the seam `winston_refuses_replace_seat_with_bot`
        // guarded. A live pod converts, and the converted seat keeps its turn
        // order.
        let mut live = started(2, 2);
        let active_before = state(&live).active_seat;
        session::apply(
            &mut live,
            DraftAction::ReplaceSeatWithBot {
                seat: 1,
                name: None,
            },
            None,
        )
        .expect("a live Winston pod converts a seat to a bot");
        assert!(matches!(live.seats[1], DraftSeat::Bot { .. }));
        assert_eq!(state(&live).active_seat, active_before);

        // Paired positive 1 (carried forward): the same seat list starts fine
        // as a pick-and-pass kind, so nothing about the acceptance is
        // distribution-specific.
        let (mut premier, premier_source) = winston_session(2, 1);
        premier.kind = DraftKind::Premier;
        premier.config.kind = DraftKind::Premier;
        premier.seats[1] = DraftSeat::Bot {
            name: "Bot".to_string(),
        };
        session::apply(&mut premier, DraftAction::StartDraft, Some(&premier_source))
            .expect("a Premier pod admits a bot seat");
        assert_eq!(premier.status, DraftStatus::Drafting);
        session::apply(
            &mut premier,
            DraftAction::ReplaceSeatWithBot {
                seat: 1,
                name: None,
            },
            None,
        )
        .expect("Premier still accepts a bot replacement");

        // Paired positive 2 (carried forward): a 4-seat ALL-HUMAN Winston pod
        // still starts, and still with three piles.
        let four = started(4, 1);
        assert_eq!(four.status, DraftStatus::Drafting);
        assert_eq!(state(&four).piles.len(), 3);
    }

    /// A `pile_count` of zero is REFUSED, not indexed.
    ///
    /// A unit test on the authority rather than a `StartDraft` walk, and the
    /// reason is the point of the guard: no `DraftKind` can declare a zero
    /// today (`max_shared_stack_piles_matches_procedure_table` folds
    /// `DraftKind::ALL` and finds the single row at `3`), so there IS no
    /// production path to drive it down -- which is exactly why the panic it
    /// replaces would have been the next author's to discover, inside
    /// `StartDraft`, rather than a refusal at the boundary.
    ///
    /// The production path is asserted in the other direction instead: every
    /// pile count a kind can actually declare goes through this function and
    /// comes back as the length a started pod really has.
    #[test]
    fn a_zero_pile_count_is_refused_rather_than_indexed() {
        assert_eq!(
            piles_needed(0),
            Err(DraftError::InvalidSharedStackConfiguration {
                reason: "a shared-stack distribution must deal at least one pile".to_string(),
            })
        );

        // Paired positive: this refuses ZERO, not the function -- one pile is
        // as legal as the published three.
        assert_eq!(piles_needed(1), Ok(1));
        assert_eq!(piles_needed(u8::MAX), Ok(usize::from(u8::MAX)));

        // Every row the procedure table actually declares, with the fold's own
        // reach-guard so a table that declared none could not pass vacuously.
        let mut observed = 0usize;
        for kind in DraftKind::ALL {
            if let PackDistribution::SharedStackPiles { pile_count } = kind.procedure().distribution
            {
                observed += 1;
                assert_eq!(
                    piles_needed(pile_count),
                    Ok(usize::from(pile_count)),
                    "{kind:?}"
                );
            }
        }
        assert!(observed >= 1, "the fold observed no shared-stack kind");

        // And the length a REAL started pod has is the one this function
        // answers, for both vectors the zero would have made empty.
        let session = started(2, 17);
        let s = state(&session);
        assert_eq!(Ok(s.piles.len()), piles_needed(3));
        assert_eq!(Ok(s.inspected.len()), piles_needed(3));
    }

    /// BOTH write sites of the `inspected` contract, in one turn's trajectory,
    /// because they are one contract: a prefix is published as the cursor
    /// reaches a pile and LAPSES when the turn ends.
    ///
    /// * Legs 1-2 -- the SECOND write site, which is the one that was omitted
    ///   in an earlier draft of this design: a decline that advances the cursor
    ///   must publish the new cursor pile's height, and the card the decline
    ///   appended must sit BEYOND the declined pile's inspected prefix.
    /// * Leg 3 -- the FIRST write site, at turn end: every prefix this seat
    ///   earned is zeroed and only the new seat's pile 0 is published. Before
    ///   this leg existed, deleting that reset left EVERY test in draft-core,
    ///   server-core, draft-wasm, lobby-broker and phase-server green -- it is
    ///   the line whose loss has no other symptom until a view projects
    ///   `revealed` from `inspected`.
    #[test]
    fn a_decline_inspects_the_new_cursor_and_hides_its_own_refill() {
        let mut session = started(2, 9);
        let declined_height_before = state(&session).piles[0].len();
        assert_eq!(state(&session).inspected[0], declined_height_before);
        let top_of_stack = state(&session)
            .main_stack
            .last()
            .expect("the stack is non-empty at turn start")
            .instance_id
            .clone();

        decide(&mut session, SharedStackPileDecision::Decline).expect("legal at turn start");

        let s = state(&session);
        assert_eq!(s.cursor, 1);
        // Second write site: the new cursor pile's height is published.
        assert_eq!(s.inspected[1], s.piles[1].len());
        assert!(s.inspected[1] > 0, "the new cursor pile is non-empty");
        // The declined pile grew, but its inspected prefix did not.
        assert_eq!(s.piles[0].len(), declined_height_before + 1);
        assert_eq!(s.inspected[0], declined_height_before);
        assert_eq!(
            s.piles[0].last().unwrap().instance_id,
            top_of_stack,
            "the appended card is the one the stack popped"
        );
        assert!(
            !s.piles[0][..s.inspected[0]]
                .iter()
                .any(|card| card.instance_id == top_of_stack),
            "the refill sits beyond the inspected prefix by construction"
        );

        // SECOND DECLINE, at a NON-ZERO cursor. This leg is what pins the
        // refill TARGET: at cursor 0 a refill that always writes `piles[0]` is
        // indistinguishable from one that writes the cursor pile, so a
        // wrong-pile refill survives a cursor-0-only fixture. It is also what
        // pins the refill COUNT at exactly one card.
        let pile_zero_before = state(&session).piles[0].len();
        let pile_one_before = state(&session).piles[1].len();
        let pile_two_before = state(&session).piles[2].len();
        let next_top = state(&session)
            .main_stack
            .last()
            .expect("the stack is still non-empty")
            .instance_id
            .clone();

        decide(&mut session, SharedStackPileDecision::Decline).expect("legal at cursor 1");

        let s = state(&session);
        assert_eq!(s.cursor, 2);
        assert_eq!(
            s.piles[1].len(),
            pile_one_before + 1,
            "the CURSOR pile gained exactly one card"
        );
        assert_eq!(s.piles[1].last().unwrap().instance_id, next_top);
        assert_eq!(s.piles[0].len(), pile_zero_before, "pile 0 did not move");
        assert_eq!(s.piles[2].len(), pile_two_before, "pile 2 did not move");
        assert_eq!(s.inspected[2], s.piles[2].len());
        assert_eq!(
            s.inspected[1], pile_one_before,
            "the declined pile's prefix still hides its own refill"
        );

        // THIRD LEG: the TURN-END reset, which is the `inspected` contract's
        // FIRST write site and the one that makes the previous seat's
        // entitlement LAPSE. Without it the piles this seat inspected stay
        // published, and once a view projects `revealed` from `inspected` the
        // next seat silently inherits the previous seat's prefixes -- a
        // hidden-information leak with no other symptom, which is why it needs
        // an assertion of its own rather than riding on a pile-contents check.
        let previous_seat = s.active_seat;
        let inspected_before_turn_end = s.inspected.clone();
        // Reach-guard: the entitlement about to lapse is a REAL one. Zeros
        // asserted below are meaningless unless something non-zero preceded
        // them.
        assert!(
            inspected_before_turn_end[1] > 0 && inspected_before_turn_end[2] > 0,
            "this seat really had inspected piles 1 and 2 this turn: {inspected_before_turn_end:?}"
        );

        decide(&mut session, SharedStackPileDecision::Take)
            .expect("taking the final pile is legal and ends the turn");

        let s = state(&session);
        assert_ne!(
            s.active_seat, previous_seat,
            "the take ended the turn and passed the seat"
        );
        assert_eq!(s.cursor, 0, "the new turn starts at pile 0");
        // The SECOND reach-guard, and the one that stops the zeros below from
        // passing vacuously: both later piles are non-empty, so a `0` is a
        // WITHHELD prefix rather than an empty pile.
        assert!(
            !s.piles[1].is_empty() && !s.piles[2].is_empty(),
            "piles 1 and 2 still hold cards: {:?}",
            s.piles.iter().map(Vec::len).collect::<Vec<_>>()
        );
        assert_eq!(
            s.inspected,
            vec![s.piles[0].len(), 0, 0],
            "at turn start the new seat sees pile 0 and NOTHING of the previous seat's turn"
        );
        assert!(
            s.inspected[0] > 0,
            "and the new seat's own pile-0 entitlement was published"
        );
    }

    /// V-B1: the published decision history records PUBLIC facts only, keeps
    /// them faithful to the moment they happened, and stays bounded.
    ///
    /// Driven through the REAL reducer (`session::apply`, via
    /// `drive_one_decision`) rather than by hand-building a `SharedStackState`:
    /// the claim is about what the reducer writes, and a hand-built state would
    /// assert only that a struct literal holds what the literal put in it.
    ///
    /// Four legs, and the third is the one the type exists for:
    ///   (a) FIDELITY -- after every decision of two full walks, the retained
    ///       window equals the decisions actually applied, with `pile_size`
    ///       captured BEFORE the pile moved.
    ///   (b) BOUNDEDNESS -- a walk longer than the capacity keeps exactly the
    ///       capacity, and the OLDEST record is the one that is gone.
    ///   (c) NO CARDS -- a record's serialized key set is exactly the four
    ///       public fields, and no card's `instance_id` appears anywhere in the
    ///       serialized history of a completed draft.
    ///   (d) PUBLICATION -- the same history reaches every seat's projection,
    ///       verbatim, which is the half `view::shared_stack_view` owns.
    #[test]
    fn history_records_sizes_and_decisions_and_never_cards() {
        // (a) + (b): both walk policies, because `TakeFirst` never declines and
        // a decline is the record whose `pile_size` is the interesting one.
        for policy in WalkPolicy::ALL {
            // 4 seats x 3 packs x 15 cards = 180 cards. Chosen over the 2-seat
            // fixture deliberately: a 2-seat 90-card draft costs fewer
            // decisions than `SHARED_STACK_HISTORY_CAPACITY`, so leg (b) would
            // never reach the trim and would pass vacuously. The reach-guard
            // below fails rather than skipping if that ever stops being true.
            let mut session = started(4, 4242);
            let total = 4 * 3 * usize::from(CARDS_PER_PACK);
            let mut applied: Vec<SharedStackDecisionRecord> = Vec::new();
            let mut card_ids: Vec<String> = {
                let s = state(&session);
                s.main_stack
                    .iter()
                    .chain(s.piles.iter().flatten())
                    .map(|card| card.instance_id.clone())
                    .collect()
            };
            card_ids.sort_unstable();
            assert_eq!(card_ids.len(), total, "every card is live at turn one");

            let cap = total * 4;
            while session.status == DraftStatus::Drafting {
                assert!(
                    applied.len() < cap,
                    "{policy:?}: the draft did not terminate"
                );
                // Read from the state BEFORE applying, so `pile_size` is
                // compared against the height the reducer had to capture
                // before its own `match` moved the pile.
                let (seat, pile, pile_size) = {
                    let s = state(&session);
                    (
                        s.active_seat,
                        s.cursor,
                        s.piles[usize::from(s.cursor)].len(),
                    )
                };
                let decision = drive_one_decision(&mut session, policy);
                applied.push(SharedStackDecisionRecord {
                    seat,
                    pile,
                    decision,
                    pile_size,
                });

                // (b) asserted after EVERY decision, not only at the end: the
                // length is the capacity once the walk passes it, and never
                // more.
                let kept = applied.len().min(SHARED_STACK_HISTORY_CAPACITY);
                let history = &state(&session).history;
                assert_eq!(
                    history.len(),
                    kept,
                    "{policy:?}: the history keeps at most {SHARED_STACK_HISTORY_CAPACITY}"
                );
                // (a): the retained window is the MOST RECENT `kept` decisions,
                // oldest first -- which pins both the contents and the
                // direction of the trim. A trim from the back would keep the
                // oldest and red this.
                //
                // Compared entry by entry rather than slice against slice, for
                // the diagnostic: a whole-window `assert_eq!` prints 128
                // records on failure (measured at ~11 KB under the
                // capture-after-the-match mutation), which buries the one
                // record that moved.
                let window = &applied[applied.len() - kept..];
                if let Some((i, (got, want))) = history
                    .iter()
                    .zip(window.iter())
                    .enumerate()
                    .find(|(_, (got, want))| got != want)
                {
                    panic!(
                        "{policy:?}: history[{i}] of {kept} is {got:?}, but decision \
                         {} applied {want:?}",
                        applied.len() - kept + i
                    );
                }
            }

            // Reach-guards for (b), so a shorter draft cannot make the trim
            // untested: the walk really did exceed the capacity, and the oldest
            // record really is absent.
            assert!(
                applied.len() > SHARED_STACK_HISTORY_CAPACITY,
                "{policy:?}: {} decisions must exceed the {SHARED_STACK_HISTORY_CAPACITY}-record \
                 capacity, or leg (b) never reaches the trim",
                applied.len()
            );
            let history = state(&session).history.clone();
            assert_eq!(history.len(), SHARED_STACK_HISTORY_CAPACITY);
            assert_eq!(
                history.first().copied(),
                applied
                    .get(applied.len() - SHARED_STACK_HISTORY_CAPACITY)
                    .copied(),
                "{policy:?}: the OLDEST retained record is the one at the window's head"
            );
            // "The oldest is GONE", stated the only way that is well posed:
            // records are compared by VALUE, and an early `(seat 0, pile 0,
            // Take, size 1)` recurs verbatim later in any draft -- so a
            // `!history.contains(&applied[0])` check would be asserting a
            // coincidence, not a trim. The monotone `decisions` counter is the
            // identity-free witness: it counted every decision, the history
            // retained strictly fewer, and the difference is exactly what the
            // trim dropped.
            assert_eq!(
                usize::try_from(state(&session).decisions).unwrap(),
                applied.len(),
                "{policy:?}: `decisions` counted every applied decision"
            );
            assert_eq!(
                applied.len() - history.len(),
                applied.len() - SHARED_STACK_HISTORY_CAPACITY,
                "{policy:?}: exactly the overflow was dropped"
            );
            assert!(
                history.len() < applied.len(),
                "{policy:?}: the history is strictly shorter than the decisions made"
            );

            // Reach-guard for (a): both decisions occur, so `pile_size` is
            // pinned for a `Take` (which empties the pile) and for a `Decline`
            // (which appends to it).
            assert!(
                applied
                    .iter()
                    .any(|r| r.decision == SharedStackPileDecision::Take),
                "{policy:?}: the walk took at least one pile"
            );
            assert!(
                applied
                    .iter()
                    .any(|r| r.decision == SharedStackPileDecision::Decline),
                "{policy:?}: the walk declined at least one pile"
            );
            // And the sizes are not all the same number, which is what would
            // make the `pile_size` assertions satisfiable by a constant.
            let sizes: std::collections::BTreeSet<usize> =
                applied.iter().map(|r| r.pile_size).collect();
            assert!(
                sizes.len() > 1,
                "{policy:?}: piles reached more than one height: {sizes:?}"
            );

            // (c) the negative that matters: no card's identity is anywhere in
            // the serialized history of a whole draft.
            let serialized = serde_json::to_string(&history).expect("history serializes");
            for id in &card_ids {
                assert!(
                    !serialized.contains(id.as_str()),
                    "{policy:?}: the history named a card ({id})"
                );
            }
            // Paired positive for that scan, so a scan that could never match
            // cannot pass vacuously: the same instrument DOES find a card id in
            // the session's own pools.
            let pooled = serde_json::to_string(&session.pools).expect("pools serialize");
            assert!(
                card_ids.iter().any(|id| pooled.contains(id.as_str())),
                "{policy:?}: the substring scan finds a card id when one is present"
            );
        }

        // (c) the structural half: the record's serialized shape is exactly the
        // four public fields. A future author adding a card-bearing field reds
        // this, which is the whole point of the type's doc.
        let record = SharedStackDecisionRecord {
            seat: 1,
            pile: 2,
            decision: SharedStackPileDecision::Decline,
            pile_size: 4,
        };
        let value = serde_json::to_value(record).expect("a record serializes");
        // Compared as a SET: `serde_json::Map` is a `BTreeMap` here (the
        // `preserve_order` feature is off), so its iteration order is
        // alphabetical and asserting declaration order would pin the
        // serializer's internals instead of the field list.
        let keys: std::collections::BTreeSet<&str> = value
            .as_object()
            .expect("a record is a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["decision", "pile", "pile_size", "seat"]
                .into_iter()
                .collect::<std::collections::BTreeSet<&str>>(),
            "a record carries the four PUBLIC fields and nothing else"
        );

        // (d): the projection publishes the same history to every seat,
        // verbatim, including seats that are not the active one.
        let mut session = started(2, 4242);
        for _ in 0..5 {
            drive_one_decision(&mut session, WalkPolicy::DeclineFirst);
        }
        let expected = state(&session).history.clone();
        assert!(
            expected.len() >= 5,
            "reach-guard: there is a history to publish"
        );
        for seat in 0..session.seats.len() as u8 {
            let view = crate::view::filter_for_player(&session, seat);
            let published = view
                .shared_stack
                .expect("a live pile turn publishes a shared-stack view");
            assert_eq!(
                published.history, expected,
                "seat {seat} receives the history verbatim -- it is a record of public events"
            );
        }
    }

    /// The import boundary's half of the history's contract: a snapshot is the
    /// one place an over-long or out-of-range history can arrive from, because
    /// the reducer cannot build one.
    ///
    /// Paired positive first, so the three refusals cannot be satisfied by a
    /// validator that refuses everything.
    #[test]
    fn a_persisted_history_must_be_bounded_and_in_range() {
        let mut session = started(2, 4242);
        for _ in 0..4 {
            drive_one_decision(&mut session, WalkPolicy::DeclineFirst);
        }
        let record = *state(&session)
            .history
            .first()
            .expect("the walk wrote records");

        // Paired positive (a): what the reducer itself produces validates.
        session
            .validate_persisted_snapshot()
            .expect("a reducer-built history validates");
        // Paired positive (b): a history exactly AT capacity validates, so the
        // bound below is `>` and not `>=`.
        let at_capacity = {
            let mut s = session.clone();
            let state = s.shared_stack.as_mut().expect("shared stack present");
            state.history = vec![record; SHARED_STACK_HISTORY_CAPACITY];
            s
        };
        at_capacity
            .validate_persisted_snapshot()
            .expect("a history exactly at capacity validates");

        let over_capacity = {
            let mut s = session.clone();
            let state = s.shared_stack.as_mut().expect("shared stack present");
            state.history = vec![record; SHARED_STACK_HISTORY_CAPACITY + 1];
            s
        };
        assert!(
            matches!(
                over_capacity.validate_persisted_snapshot(),
                Err(DraftError::InvalidSharedStackConfiguration { .. })
            ),
            "one record over capacity is refused at import"
        );

        let bad_pile = {
            let mut s = session.clone();
            let state = s.shared_stack.as_mut().expect("shared stack present");
            let piles = state.piles.len() as u8;
            state.history = vec![SharedStackDecisionRecord {
                pile: piles,
                ..record
            }];
            s
        };
        assert!(
            matches!(
                bad_pile.validate_persisted_snapshot(),
                Err(DraftError::InvalidSharedStackConfiguration { .. })
            ),
            "a record naming a pile the session does not have is refused"
        );

        let bad_seat = {
            let mut s = session.clone();
            let seats = s.seats.len() as u8;
            let state = s.shared_stack.as_mut().expect("shared stack present");
            state.history = vec![SharedStackDecisionRecord {
                seat: seats,
                ..record
            }];
            s
        };
        assert!(
            matches!(
                bad_seat.validate_persisted_snapshot(),
                Err(DraftError::InvalidSharedStackConfiguration { .. })
            ),
            "a record naming a seat the session does not have is refused"
        );
    }

    /// The wire cost of [`SHARED_STACK_HISTORY_CAPACITY`], measured rather than
    /// argued: the history rides every `SharedStackView` broadcast, so the
    /// capacity is only defensible with a number beside it.
    ///
    /// The ceilings are generous headroom over the measured figures, not a
    /// second copy of them -- this test exists to catch a record that starts
    /// carrying something big (a card), not to fail on a byte of drift.
    #[test]
    fn history_at_capacity_has_a_measured_wire_size() {
        let mut session = started(4, 4242);
        while session.status == DraftStatus::Drafting
            && state(&session).history.len() < SHARED_STACK_HISTORY_CAPACITY
        {
            drive_one_decision(&mut session, WalkPolicy::DeclineFirst);
        }
        let full = state(&session).history.clone();
        assert_eq!(
            full.len(),
            SHARED_STACK_HISTORY_CAPACITY,
            "reach-guard: the history really is at capacity"
        );

        let history_bytes = serde_json::to_string(&full)
            .expect("history serializes")
            .len();
        let view = crate::view::filter_for_player(&session, state(&session).active_seat)
            .shared_stack
            .expect("a live pile turn publishes a shared-stack view");
        let full_view_bytes = serde_json::to_string(&view).expect("view serializes").len();
        let mut empty_view = view.clone();
        empty_view.history.clear();
        let empty_view_bytes = serde_json::to_string(&empty_view)
            .expect("view serializes")
            .len();

        println!(
            "MEASURED wire size at capacity {SHARED_STACK_HISTORY_CAPACITY}: \
             history {history_bytes} B; SharedStackView {full_view_bytes} B with it, \
             {empty_view_bytes} B without ({} B attributable)",
            full_view_bytes - empty_view_bytes
        );

        assert!(
            history_bytes < 16_000,
            "a full history is small enough to ride every broadcast: {history_bytes} B"
        );
        assert!(
            full_view_bytes - empty_view_bytes < 16_000,
            "the history's share of a view is bounded: {} B",
            full_view_bytes - empty_view_bytes
        );
    }

    /// The card a final-pile decline draws is the ONE card a seat receives
    /// without seeing it, so the engine records which card that was.
    ///
    /// The paired negative is the whole point: a `Take` leaves no notice,
    /// because the cards a take collects were face up in the pile the seat was
    /// looking at. A test that only asserted the positive would pass against a
    /// reducer that recorded every card a seat ever drafted.
    #[test]
    fn a_forced_draw_records_the_card_the_seat_never_saw() {
        let mut session = started(2, 20_260_913);

        // Paired negative FIRST: a take is not a forced draw.
        decide(&mut session, SharedStackPileDecision::Take).expect("taking pile 0");
        assert!(
            state(&session).forced_draws.iter().all(Option::is_none),
            "a take collects cards the seat was already looking at"
        );

        // The pool BEFORE the declining turn. Deliberately not "the top of the
        // stack before the turn": the three declines each pop a card onto the
        // pile they refuse, so the card the fourth pop draws is four deep at
        // this point. Reading the pool instead asks the question that actually
        // matters — did the notice name the card this seat received — without
        // re-implementing the reducer's draw order in the assertion.
        let seat = state(&session).active_seat;
        let pool_before: Vec<String> = session.pools[usize::from(seat)]
            .iter()
            .map(|card| card.instance_id.clone())
            .collect();
        let stack_before: Vec<String> = state(&session)
            .main_stack
            .iter()
            .map(|card| card.instance_id.clone())
            .collect();
        assert_eq!(seat, decline_through_the_forced_draw(&mut session));

        let gained: Vec<&DraftCardInstance> = session.pools[usize::from(seat)]
            .iter()
            .filter(|card| !pool_before.contains(&card.instance_id))
            .collect();
        assert_eq!(gained.len(), 1, "a declining turn drafts exactly one card");
        let recorded = state(&session).forced_draws[usize::from(seat)]
            .as_ref()
            .expect("a final-pile decline draws, so it records");
        assert_eq!(recorded.instance_id, gained[0].instance_id);
        // And it came off the stack, rather than out of a pile: the whole point
        // is that nobody had looked at it.
        assert!(stack_before.contains(&recorded.instance_id));
        assert!(!state(&session)
            .main_stack
            .iter()
            .any(|card| card.instance_id == recorded.instance_id));
        // Only the seat that drew has a notice.
        assert!(state(&session)
            .forced_draws
            .iter()
            .enumerate()
            .all(|(index, pending)| (index == usize::from(seat)) == pending.is_some()));
    }

    /// A notice survives the turn that produced it — the player has to be able
    /// to read it after their turn ends — and is cleared by that seat's NEXT
    /// decision, not by the opponent's.
    #[test]
    fn a_forced_draw_notice_outlives_the_turn_and_ends_at_the_seats_next_decision() {
        let mut session = started(2, 20_260_913);
        let seat = decline_through_the_forced_draw(&mut session);
        assert!(state(&session).forced_draws[usize::from(seat)].is_some());

        // The opponent's whole turn passes. The notice is still there, which is
        // what makes it readable at all: by now the active seat is not its owner.
        assert_ne!(state(&session).active_seat, seat);
        decide(&mut session, SharedStackPileDecision::Take).expect("the opponent takes a pile");
        assert_eq!(state(&session).active_seat, seat);
        assert!(
            state(&session).forced_draws[usize::from(seat)].is_some(),
            "the notice must outlive the turn that produced it"
        );

        // The owner acts again, so the notice has served its purpose.
        decide(&mut session, SharedStackPileDecision::Take).expect("the owner takes a pile");
        assert!(state(&session).forced_draws[usize::from(seat)].is_none());
    }
}
