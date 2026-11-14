//! Two real 4-player drain boards at a CR 732.2a loop-shortcut offer, and what each one
//! publishes there.
//!
//! `lethal_lifegain_loss_4p.json.gz` is derived from `LethalLifeGainLossLoop.zip` and
//! `weird_drain_4p.json.gz` from `weird-drain-behavior.zip`, both through
//! `scripts/migrate-dump-fixture.sh --effect-kind LoseLife --deck-size Exactly:100`, then
//! driven forward to the recorded offer. The committed artifact IS the gzip stream, so a
//! regeneration must re-gzip (`gzip -9 -n`), never commit a bare `.json`.
//!
//! Both boards' RESTORED offers are directly declarable; a live offer is reached by declining
//! and driving forward.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use engine::analysis::decision_template::{
    AnnouncementSubject, ChoicePoint, DecisionPoint, DecisionPointKind, DecisionSlot,
    IterationCount, PinnedDecision, RepetitionBound, TargetPin, TargetSchedule,
};
use engine::game::engine::apply;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, LoopDetectionMode, PersistedGameState, WaitingFor, YieldTarget,
};
use engine::types::identifiers::ObjectId;
use engine::types::PlayerId;

/// The proposer and the seat the drive aimed every re-aimable choice at.
///
/// Two same-typed fields, so they are named rather than positional. `pub(crate)` on the
/// fields as well as the head: later phases read both from sibling files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveOffer {
    pub(crate) proposer: PlayerId,
    pub(crate) aimed_at: PlayerId,
}

fn restore(gz: &[u8]) -> GameState {
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("the tracked fixture inflates to UTF-8 JSON");
    let mut envelope: serde_json::Value =
        serde_json::from_str(&json).expect("the dump envelope parses as JSON");
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets(&mut envelope);
    // `PersistedGameState`, never a bare `GameState` decode: the persisted path is what
    // runs the load-seam guards, including the CR 732.2a bound invariant.
    let mut state = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("the dump deserializes through the production decoder")
        .into_game_state()
        .expect("the persisted snapshot satisfies the checked restore contract");
    state.loop_detection = LoopDetectionMode::Interactive;
    state
}

pub(crate) fn lethal_lifegain_loss_board() -> GameState {
    restore(include_bytes!(
        "../fixtures/lethal_lifegain_loss_4p.json.gz"
    ))
}

pub(crate) fn weird_drain_board() -> GameState {
    restore(include_bytes!("../fixtures/weird_drain_4p.json.gz"))
}

/// CR 732.2a: a committed save whose offer carries a numeric `measured_repetition_bound` restores
/// bounded, and the views publish its capacity.
#[test]
fn a_committed_legacy_bound_restores_measured_through_the_production_seam() {
    let state = weird_drain_board();
    let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
        panic!(
            "reach: the save restores at its offer, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(schema.repetition_bound, RepetitionBound::Measured);
    assert!(schema.is_bounded());
    assert_eq!(
        engine::game::derived_views::derive_views(&state, None).bounded_loop_max_repetitions,
        Some(schema.deliverable_capacity)
    );
}

fn submit(state: &mut GameState, who: PlayerId, action: GameAction) {
    if let Err(error) = apply(state, who, action.clone()) {
        panic!("apply err ({action:?}): {error:?}");
    }
}

/// The seats a beat offers as player targets.
fn offered_player_targets(actions: &[GameAction]) -> BTreeSet<PlayerId> {
    actions
        .iter()
        .filter_map(|action| match action {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(seat)),
            } => Some(*seat),
            _ => None,
        })
        .collect()
}

/// One beat, every beat crossing the public `apply()` boundary: pass at priority, aim
/// every re-aimable choice at the LATCHED seat, take an optional-effect prompt, and answer any
/// other prompt with its first legal action.
///
/// The seat is latched at the first beat that offers one, as the LOWEST legal seat rather
/// than in publisher order, and re-asserted legal at every later beat — a drive that
/// silently re-aimed would move the certificate's losing seat under the rows that read it.
pub(crate) fn drive_one_beat(state: &mut GameState, aimed_at: &mut Option<PlayerId>) {
    let who = state
        .waiting_for
        .acting_player()
        .unwrap_or_else(|| panic!("no acting player at {:?}", state.waiting_for));
    let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, who);

    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        let pass = actions
            .iter()
            .find(|action| matches!(action, GameAction::PassPriority))
            .cloned()
            .unwrap_or_else(|| panic!("a priority beat offers no PassPriority: {actions:?}"));
        submit(state, who, pass);
        return;
    }

    let offered = offered_player_targets(&actions);
    if !offered.is_empty() {
        let seat = match *aimed_at {
            Some(latched) => {
                assert!(
                    offered.contains(&latched),
                    "the latched seat {latched:?} stopped being offered at {:?}; aiming elsewhere \
                     would move the losing seat under every row that re-derives it. offered: \
                     {offered:?}",
                    state.waiting_for
                );
                latched
            }
            None => {
                let lowest = *offered
                    .first()
                    .expect("non-empty by the guard immediately above");
                *aimed_at = Some(lowest);
                lowest
            }
        };
        submit(
            state,
            who,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(seat)),
            },
        );
        return;
    }

    let answer = actions
        .iter()
        .find(|action| matches!(action, GameAction::DecideOptionalEffect { accept: true }))
        .or_else(|| actions.first())
        .cloned()
        .unwrap_or_else(|| panic!("no legal action at {:?}", state.waiting_for));
    submit(state, who, answer);
}

fn legal_actions_at_offer(state: &GameState, proposer: PlayerId) -> Vec<GameAction> {
    engine::ai_support::legal_actions_for_viewer(state, proposer).0
}

/// Decline the restored offer and drive to the next CR 732.2a offer that is actually
/// DECLARABLE, returning the proposer and the seat the drive latched.
///
/// The declarability assertion before returning is this helper's liveness control: a caller
/// handed an undeclarable offer would be reading a dead board rather than a negative.
pub(crate) fn drive_to_live_declarable_offer(state: &mut GameState) -> LiveOffer {
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for else {
        panic!(
            "the board must start at its restored CR 732.2a offer, got {:?}",
            state.waiting_for
        );
    };
    submit(state, proposer, GameAction::DeclineShortcut);

    let mut aimed_at = None;
    for _ in 0..2_000u32 {
        if let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for {
            let actions = legal_actions_at_offer(state, proposer);
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, GameAction::DeclareShortcut { .. })),
                "liveness control: the offer this helper returns must be DECLARABLE, or every \
                 row built on it reads a dead board. offered: {actions:?}"
            );
            let aimed_at = aimed_at.expect(
                "the drive reached a declarable offer without ever aiming a choice, so no seat \
                 was latched and the rows have nothing to re-derive from",
            );
            return LiveOffer { proposer, aimed_at };
        }
        drive_one_beat(state, &mut aimed_at);
    }
    panic!(
        "the drive did not reach a declarable CR 732.2a offer, stuck at {:?}",
        state.waiting_for
    );
}

/// CR 704.5a: re-derive a live offer's repetition bound from PUBLISHED data alone — the
/// certificate's `per_cycle` delta, its `victim_slot` magnitudes and its `declarable_victims`,
/// plus the live board's lives and libraries — and the seat the caller's own drive aimed at.
///
/// The aim is the ONE input that is not on the certificate: a `NotProposerChoice` announcement
/// publishes no declaration at all, so it comes from the test's own record of what it drove —
/// a stronger source than the engine's, because it is not produced by the code under test.
///
/// The reach comes from `per_cycle.declarable_victims`, which IS the single charged slot's
/// reach whenever exactly one slot is charged. Every precondition that makes that reading true
/// is asserted loudly rather than assumed, so a board this helper cannot answer for fails as a
/// FIXTURE GAP and not as a wrong bound.
/// CR 732.2a + CR 704.5a: the published count, given every living seat's STRICT headroom in
/// whole repetitions. Mirrors `ResourceVector::elimination_bounds`' final reduction so the
/// three test-side re-derivations of that function share one statement of it instead of three.
///
/// The strict minimum is the answer while more than one seat holds it; when exactly one does,
/// the count reaches that seat's own crossing, at whatever magnitude that lands on — the
/// reduction applies no budget of its own, and the offer's producer is what derives a deliverable
/// capacity from one.
///
/// `ceiling` answers the arm where no seat is consumed at all, which the reduction reports as an
/// absence; every board this mirror is used on consumes one, and the parameter is what keeps the
/// arm total rather than panicking.
/// CR 732.2a + CR 704.5a: the count the PUBLISHED offer measures, given every living seat's
/// STRICT headroom in whole repetitions — the mirror of `PeriodicDelta::elimination_cascade`'s
/// reduction, where `relieve_strict_bound` beside it mirrors the divisor's.
///
/// A seat with strict headroom `s` crosses on repetition `s + 1`, and the cascade's count is its
/// LAST entry, so the count is the WIDEST of those crossings. The divisor answers the other
/// quantifier — the first crossing under any declaration — and the two coincide only where every
/// consumed seat crosses together.
///
/// `proposer_strict` truncates the walk at the proposer's own crossing: no repetition past it is
/// one the proposer is still in the game to take, so the cascade drops every later entry and its
/// count becomes that crossing. `None` is the usual untargeted shape, where the period GAINS the
/// proposer life. `ceiling` answers the arm where no seat is consumed at all, which the reduction
/// reports as an absence.
pub(crate) fn cascade_count_from_strict(
    strict: &[i64],
    proposer_strict: Option<i64>,
    ceiling: i64,
) -> i64 {
    let Some(&widest) = strict.iter().max() else {
        return ceiling;
    };
    let count = widest + 1;
    proposer_strict.map_or(count, |own| count.min(own + 1))
}

pub(crate) fn relieve_strict_bound(strict: &[i64], ceiling: i64) -> i64 {
    let Some(&floor) = strict.iter().min() else {
        return ceiling;
    };
    let relieved = floor + 1;
    if strict.iter().filter(|b| **b == floor).count() == 1 {
        relieved
    } else {
        floor.max(0)
    }
}

pub(crate) fn rederive_live_offer_bound(state: &GameState, aimed_at: PlayerId) -> u32 {
    let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
        panic!("not at an offer: {:?}", state.waiting_for);
    };
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .expect("a bounded offer publishes its per-cycle signature");

    assert_eq!(
        per_cycle.victim_slot.len(),
        1,
        "FIXTURE GAP: this helper reads `declarable_victims` as the ONE charged slot's reach, \
         which is only the same set while exactly one slot is charged; got {:?}",
        per_cycle.victim_slot
    );
    let magnitude = per_cycle.victim_slot[0].1;
    let reaches = &per_cycle.declarable_victims;
    assert!(
        !reaches.is_empty(),
        "FIXTURE GAP: a RESTORED offer's `declarable_victims` deserialize empty, so no row may \
         hand this helper one — drive to a live offer first"
    );
    assert!(
        reaches.contains(&aimed_at),
        "FIXTURE GAP: the drive's aimed seat {aimed_at:?} must lie inside the charged slot's \
         reach {reaches:?}, else the caller and the offer disagree about what was announced"
    );

    let ceiling = i64::from(crate::fantastic_four_bounded_loop::MAX_SHORTCUT_CYCLES_MIRROR);
    let mut strict: Vec<i64> = Vec::new();
    for player in state.players.iter().filter(|p| !p.is_eliminated) {
        let mut seat: Option<i64> = None;
        let mut narrow = |headroom: i64, magnitude: i64| {
            if magnitude > 0 {
                let n = headroom.max(0) / magnitude;
                seat = Some(seat.map_or(n, |b: i64| b.min(n)));
            }
        };
        assert_eq!(
            per_cycle.delta.poison.get(&player.id).copied().unwrap_or(0),
            0,
            "FIXTURE GAP: this helper re-derives the CR 704.5a life and CR 104.3c library axes \
             only, so a living seat carrying a poison delta is a board it cannot answer for"
        );
        // CR 704.5a headroom is `life - 1`: a seat at exactly 0 has already lost, so the seat's
        // STRICT value stops one point above it — `relieve_strict_bound` below is what carries a
        // lone faller to its own crossing. The charge is the slot's magnitude on every seat it
        // REACHES, less what the window saw it aim AT that seat.
        let observed = -per_cycle.delta.life.get(&player.id).copied().unwrap_or(0);
        let aim = if player.id == aimed_at {
            magnitude.max(0)
        } else {
            0
        };
        let reach = if reaches.contains(&player.id) {
            magnitude.max(0)
        } else {
            0
        };
        let life_magnitude = (observed - aim).max(0) + reach;
        narrow(player.life as i64 - 1, life_magnitude);
        // CR 104.3c + CR 121.4: an empty library is only lethal on the next draw, so the
        // library axis divides the whole remaining library.
        let drain = -per_cycle
            .delta
            .library_delta
            .get(&player.id)
            .copied()
            .unwrap_or(0);
        narrow(player.library.len() as i64, drain);
        strict.extend(seat);
    }
    relieve_strict_bound(&strict, ceiling) as u32
}

/// The `DecisionSlot`s the certificate charges, and the magnitude charged to each.
fn charged_slots(state: &GameState) -> Vec<(DecisionSlot, i64)> {
    let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
        panic!("not at an offer: {:?}", state.waiting_for);
    };
    certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .expect("a bounded offer publishes its per-cycle signature")
        .victim_slot
        .clone()
}

/// The seat a declaration's target pins name, one entry per pinned target.
fn pinned_seats(decisions: &[PinnedDecision]) -> Vec<PlayerId> {
    decisions
        .iter()
        .flat_map(|decision| match decision {
            PinnedDecision::Targets { targets, .. } => targets.clone(),
            _ => Vec::new(),
        })
        .filter_map(|pin| match pin {
            TargetPin::Player(seat) => Some(seat),
            TargetPin::Scheduled(TargetSchedule::Constant(ranking)) => match ranking.head() {
                AnnouncementSubject::Seat(seat) => Some(*seat),
                AnnouncementSubject::Object(_) => None,
            },
            _ => None,
        })
        .collect()
}

/// The published `Targets` slots the certificate never charges — empty on a self-consistent
/// offer, and the containment holds in that direction ONLY.
///
/// CR 732.2a describes "a sequence of game choices", so only a CHOSEN announcement earns a
/// decision point, while the charge model serves CR 704.5a — a player at 0 or less life
/// loses whether or not anybody chose them. A withheld announcement is therefore charged
/// and unpublished, and equality would red on that correct behavior. Non-`Targets` points
/// are filtered out rather than compared: the charge model keys on ANNOUNCED targets, so
/// their slots are legitimately absent from the charged set too.
fn unbacked_published_target_slots(
    charged: &BTreeSet<DecisionSlot>,
    points: &[DecisionPoint],
) -> BTreeSet<DecisionSlot> {
    points
        .iter()
        .filter(|point| matches!(point.kind, DecisionPointKind::Targets { .. }))
        .map(|point| point.slot.clone())
        .filter(|slot| !charged.contains(slot))
        .collect()
}

/// Rows 6 through 9, re-derived from the offer's own certificate on whichever board is
/// handed in. No seat, magnitude or bound literal appears anywhere: the two boards differ
/// in charge magnitude and in legal-target count, so a leg that quietly hardcoded either
/// would fail on the sibling.
fn assert_live_offer_is_self_consistent(state: &GameState, offer: LiveOffer) {
    let WaitingFor::LoopShortcut {
        proposer,
        certificate,
        schema,
        declaration,
        ..
    } = &state.waiting_for
    else {
        panic!("not at an offer: {:?}", state.waiting_for);
    };
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .expect("a bounded offer publishes its per-cycle signature");

    assert_eq!(*proposer, offer.proposer);

    // Row 6 — the certificate the drive reached reserved elimination headroom for someone.
    assert!(
        !per_cycle.declarable_victims.is_empty(),
        "a live offer publishes the seats CR 704.5a headroom was reserved for"
    );

    // Row 7 — the proposer gains per cycle, the seat the drive latched loses, and the
    // charge matches what the certificate itself says that seat loses.
    let gaining: Vec<PlayerId> = per_cycle
        .delta
        .life
        .iter()
        .filter(|(_, delta)| **delta > 0)
        .map(|(seat, _)| *seat)
        .collect();
    let losing: Vec<PlayerId> = per_cycle
        .delta
        .life
        .iter()
        .filter(|(_, delta)| **delta < 0)
        .map(|(seat, _)| *seat)
        .collect();
    assert_eq!(
        gaining,
        vec![offer.proposer],
        "the seat the per-cycle life axis grows is the seat proposing"
    );
    assert_eq!(
        losing,
        vec![offer.aimed_at],
        "the seat the per-cycle life axis drains is the seat the drive aimed at"
    );

    let loss = -per_cycle.delta.life[&offer.aimed_at];
    let charged = charged_slots(state);
    assert!(
        !charged.is_empty(),
        "reach-guard: a targeted drain publishes its charged slots, so an empty set would make \
         both legs below vacuous"
    );
    for (slot, magnitude) in &charged {
        assert_eq!(
            *magnitude, loss,
            "the charge on {slot:?} disagrees with the certificate's own per-cycle life loss"
        );
    }
    let charged_set: BTreeSet<DecisionSlot> =
        charged.iter().map(|(slot, _)| slot.clone()).collect();
    let unbacked = unbacked_published_target_slots(&charged_set, &schema.points);
    assert!(
        unbacked.is_empty(),
        "the schema published a Targets point on a slot the certificate never charges: \
         {unbacked:?}"
    );

    // Row 8 — a declaration IS published here, and its pin names the latched seat.
    let declaration = declaration
        .as_ref()
        .expect("a live declarable offer publishes the declaration the engine can already specify");
    assert_eq!(declaration.owner, offer.proposer);
    assert_eq!(
        pinned_seats(&declaration.decisions),
        vec![offer.aimed_at],
        "the published declaration pins the seat the drive aimed at"
    );

    // Row 9 — the three published quantifiers, and the ORDER they stand in. The declaration's own
    // count is the cascade it implies, cut where it stops charging the seat it pinned. The CEILING
    // is CR 732.2a's existential — the widest count SOME legal declaration may specify. And the
    // DIVISOR (`rederive_live_offer_bound`) answers a third question, the FIRST crossing under any
    // declaration, which is why it is neither published field.
    let bound = schema.deliverable_capacity;
    assert_eq!(schema.repetition_bound, RepetitionBound::Measured);

    let entries = crate::loop_shortcut::cascade_from(
        state,
        *proposer,
        per_cycle,
        Some(declaration),
        &schema.points,
    );
    let pinned = pinned_seats(&declaration.decisions);
    let (cut, cut_seats) = entries
        .iter()
        .find(|(_, seats)| seats.iter().any(|seat| pinned.contains(seat)))
        .or_else(|| entries.last())
        .expect("a bounded offer's cascade carries at least one entry");
    // CR 104.2a: the offer's count eliminates every opponent, each taking ceil(life / loss)
    // repetitions, the trigger already on the stack (CR 603.3d) being one of its seat's.
    let winning: u32 = state
        .players
        .iter()
        .filter(|player| player.id != *proposer && !player.is_eliminated)
        .map(|player| (player.life as u32).div_ceil(loss as u32))
        .sum();
    // CR 603.3d + CR 601.2c: a cut that leaves one opponent leaves the trigger one legal target,
    // announced unasked, so the declaration drives on to the count that ends the game.
    let remaining = state
        .players
        .iter()
        .filter(|p| p.id != *proposer && !p.is_eliminated && !cut_seats.contains(&p.id))
        .count();
    let own = if remaining == 1 { winning } else { *cut };
    assert_eq!(
        declaration.replay,
        engine::analysis::decision_template::ReplayMode::Scheduled {
            count: IterationCount::Fixed(own)
        },
        "CR 732.2a: the published declaration is stamped at the count it drives; cascade \
         {entries:?} cut at the seats it pins {pinned:?}"
    );
    assert_eq!(schema.iteration_count, IterationCount::Fixed(winning));
    assert_eq!(
        schema
            .suggested_declaration
            .as_ref()
            .map(|suggestion| suggestion.replay.clone()),
        (own < winning).then_some(engine::analysis::decision_template::ReplayMode::Scheduled {
            count: IterationCount::Fixed(winning)
        }),
        "a suggestion is published only where the declaration's own count falls short of it"
    );

    let divisor = rederive_live_offer_bound(state, offer.aimed_at);
    assert!(
        own <= winning && winning <= bound,
        "the offer's count reaches the declaration's own and stays within the ceiling; \
         own {own}, offered {winning}, ceiling {bound}"
    );
    assert!(
        divisor <= own && own <= bound,
        "CR 704.5a: the first crossing under ANY declaration cannot outrun the last one under \
         THIS declaration, and neither outruns the widest count some declaration may specify; \
         divisor {divisor}, own {own}, ceiling {bound}"
    );
    assert!(
        divisor < own || own < bound,
        "LIVE INSTRUMENT: the three quantifiers must not collapse into one number on this board, \
         or the ordering above is satisfied by a producer that publishes one value three times; \
         divisor {divisor}, own {own}, ceiling {bound}"
    );
}

/// Row 5 — each board's restored offer is directly declarable and publishes its declaration.
#[test]
fn both_restored_offers_are_declarable() {
    for (label, state) in [
        ("weird-drain", weird_drain_board()),
        ("lethal", lethal_lifegain_loss_board()),
    ] {
        let WaitingFor::LoopShortcut {
            proposer,
            declaration,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "the {label} fixture restores at its offer: {:?}",
                state.waiting_for
            );
        };
        let actions = legal_actions_at_offer(&state, *proposer);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, GameAction::DeclareShortcut { .. })),
            "the {label} board's restored offer is directly declarable: {actions:?}"
        );
        assert!(
            declaration.is_some(),
            "and it publishes the declaration the engine can already specify"
        );
    }
}

#[test]
fn lethal_lifegain_loss_board_live_offer_is_self_consistent() {
    let mut state = lethal_lifegain_loss_board();
    let offer = drive_to_live_declarable_offer(&mut state);
    assert_live_offer_is_self_consistent(&state, offer);
}

#[test]
fn weird_drain_board_live_offer_is_self_consistent() {
    let mut state = weird_drain_board();
    let offer = drive_to_live_declarable_offer(&mut state);
    assert_live_offer_is_self_consistent(&state, offer);
}

/// The restored/live pair on the SAME board: `declarable_victims` is snapshotted at the mint and
/// survives the wire.
#[test]
fn declarable_victims_are_populated_when_restored_and_when_live() {
    for mut state in [lethal_lifegain_loss_board(), weird_drain_board()] {
        let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
            panic!("restores at its offer: {:?}", state.waiting_for);
        };
        let restored = certificate
            .per_cycle
            .as_ref()
            .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
            .expect("the restored offer carries its per-cycle signature")
            .declarable_victims
            .clone();
        assert!(
            !restored.is_empty(),
            "a restored certificate carries its declarable_victims"
        );

        drive_to_live_declarable_offer(&mut state);
        let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
            panic!("drove to an offer: {:?}", state.waiting_for);
        };
        assert!(
            !certificate
                .per_cycle
                .as_ref()
                .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
                .expect("the live offer carries its per-cycle signature")
                .declarable_victims
                .is_empty(),
            "the live offer's are populated"
        );
    }
}

/// The containment, driven at the comparison itself. Both boards publish ONE `Targets`
/// point against the ONE slot they charge, so no board here can separate a subset from an
/// equality, and the direction would hold by cardinality rather than by rule.
#[test]
fn only_a_published_targets_slot_off_the_charged_set_is_unbacked() {
    fn slot(source_id: u64) -> DecisionSlot {
        DecisionSlot::first(
            YieldTarget::ThisObject {
                source_id: ObjectId(source_id),
                incarnation: Some(0),
                trigger_description: None,
            },
            ChoicePoint::AnnouncedTarget,
        )
    }
    fn targets(slot: DecisionSlot) -> DecisionPoint {
        DecisionPoint {
            slot,
            kind: DecisionPointKind::Targets {
                legal_targets: Vec::new(),
                min_targets: 1,
                max_targets: 1,
                ordered: false,
            },
        }
    }

    let published = slot(1);
    let withheld = slot(2);
    let uncharged = slot(3);
    let charged: BTreeSet<DecisionSlot> = [published.clone(), withheld.clone()].into();

    // A STRICT superset — a CR 732.2a withhold on a slot CR 704.5a still charges.
    assert!(unbacked_published_target_slots(&charged, &[targets(published.clone())]).is_empty());

    // The guarded direction: a published `Targets` point nothing charges.
    assert_eq!(
        unbacked_published_target_slots(
            &charged,
            &[targets(published.clone()), targets(uncharged.clone())]
        ),
        BTreeSet::from([uncharged.clone()])
    );

    // ADMITTED: the charge model does not key on a non-`Targets` point, so its slot is
    // legitimately absent from the charged set.
    assert!(unbacked_published_target_slots(
        &charged,
        &[DecisionPoint {
            slot: uncharged,
            kind: DecisionPointKind::MayChoice,
        }]
    )
    .is_empty());
}

/// CR 603.3d + CR 601.2c: once a seat leaves, the trigger whose segment outlives it has one legal
/// target, announced as a spell's targets are, so a pin naming the departed seat contradicts no
/// game choice. The suggested allocation's segments in either order mint a declaration at the
/// published count, through `AcceptSuggested` and `Fixed` alike.
#[test]
fn an_accept_suggested_pin_outliving_its_seat_names_no_choice_and_is_accepted() {
    use engine::game::interaction::{
        bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
    };
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::interaction::{
        AmountAssignment, InteractionOpportunityResponse, InteractionResponse,
        InteractionResponseSpec, InteractionSessionId, InteractionShortcutCountSpec,
        InteractionShortcutDecision, InteractionShortcutPin, InteractionShortcutPointKind,
        InteractionSubmission,
    };

    let mut state = weird_drain_board();
    drive_to_live_declarable_offer(&mut state);
    let WaitingFor::LoopShortcut {
        proposer, schema, ..
    } = &state.waiting_for
    else {
        panic!("the helper returns at a declarable offer");
    };
    let (proposer, schema) = (*proposer, schema.clone());

    let target_group = schema
        .points
        .iter()
        .position(|point| matches!(point.kind, DecisionPointKind::Targets { .. }))
        .expect("reach-guard: this board publishes a CR 601.2c Targets point");
    let DecisionPointKind::Targets { legal_targets, .. } = &schema.points[target_group].kind else {
        unreachable!("the position above selected a Targets point");
    };
    let legal_seats: Vec<PlayerId> = legal_targets
        .iter()
        .map(|target| match target {
            TargetRef::Player(seat) => *seat,
            other => panic!("this board's victims are seats, got {other:?}"),
        })
        .collect();
    assert!(
        legal_seats.len() > 1,
        "reach-guard: the straddle below needs two published legal victims; got {legal_seats:?}"
    );

    let mut probe = state.clone();
    bind_interaction_authority(
        &mut probe,
        InteractionSessionId("p3-accept-suggested".into()),
    )
    .expect("valid interaction authority binding");
    let filtered = filter_state_for_viewer(&probe, proposer);
    let view = derive_viewer_interaction(&probe, &filtered, proposer);
    let opportunity = view
        .opportunities
        .iter()
        .find(|o| {
            matches!(
                &o.response,
                InteractionOpportunityResponse::Schema {
                    spec: InteractionResponseSpec::Shortcut { .. },
                    ..
                }
            )
        })
        .expect("reach-guard: the offer is published as a shortcut schema");
    let InteractionOpportunityResponse::Schema {
        spec:
            InteractionResponseSpec::Shortcut {
                count,
                points,
                preview,
                ..
            },
        ..
    } = &opportunity.response
    else {
        unreachable!("the find above selected a shortcut schema");
    };
    let InteractionShortcutCountSpec::Fixed { suggested, .. } = count else {
        panic!("this board publishes a Fixed count window, got {count:?}");
    };
    let suggested = *suggested;
    assert_eq!(
        points[target_group].kind,
        InteractionShortcutPointKind::Targets,
        "reach-guard: the published point at the schema's Targets index is a Targets point"
    );
    assert_eq!(
        points[target_group].candidate_ids.len(),
        legal_seats.len(),
        "reach-guard: candidate ids are positionally aligned with the published legal victims, \
         which is how the pins below name a seat without spelling an id"
    );

    // The suggestion's own allocation, read back as seats: the published element at the
    // suggested count states it.
    let suggestion: Vec<(PlayerId, u32)> = preview
        .iter()
        .find(|element| element.count == suggested)
        .expect("the window's suggested count is published")
        .allocation
        .iter()
        .map(|assignment| {
            let at = points[target_group]
                .candidate_ids
                .iter()
                .position(|id| *id == assignment.choice_id)
                .expect("the allocation speaks the point's own candidate ids");
            (legal_seats[at], assignment.amount)
        })
        .collect();
    assert_eq!(
        suggestion.len(),
        2,
        "reach-guard: the suggestion re-aims once, so reversing its segments changes which \
         victim each aims at; got {suggestion:?}"
    );
    let reversed: Vec<(PlayerId, u32)> = suggestion.iter().rev().copied().collect();

    // One pin per non-read-only published point; the Targets point's pin carries `allocation`.
    let pins_allocating = |allocation: &[(PlayerId, u32)]| -> Vec<InteractionShortcutPin> {
        let id = |seat: PlayerId| {
            let at = legal_seats
                .iter()
                .position(|candidate| *candidate == seat)
                .expect("a published legal victim");
            points[target_group].candidate_ids[at].clone()
        };
        points
            .iter()
            .filter(|point| !point.read_only)
            .map(|point| {
                if point.group == points[target_group].group {
                    InteractionShortcutPin {
                        group: point.group,
                        choice_ids: allocation.iter().map(|(seat, _)| id(*seat)).collect(),
                        amounts: allocation
                            .iter()
                            .map(|(seat, amount)| AmountAssignment {
                                choice_id: id(*seat),
                                amount: *amount,
                            })
                            .collect(),
                    }
                } else {
                    InteractionShortcutPin {
                        group: point.group,
                        choice_ids: point
                            .candidate_ids
                            .iter()
                            .take(point.min as usize)
                            .cloned()
                            .collect(),
                        amounts: Vec::new(),
                    }
                }
            })
            .collect()
    };
    let submit_decision = |decision: InteractionShortcutDecision,
                           allocation: &[(PlayerId, u32)]| {
        resolve_interaction_response(
            &probe,
            proposer,
            &InteractionSubmission {
                interaction_id: opportunity.interaction_id.clone(),
                response: InteractionResponse::Shortcut {
                    decision,
                    pins: pins_allocating(allocation),
                },
            },
        )
    };

    for (label, decision, allocation) in [
        (
            "suggested order",
            InteractionShortcutDecision::AcceptSuggested,
            &suggestion,
        ),
        (
            "reversed",
            InteractionShortcutDecision::AcceptSuggested,
            &reversed,
        ),
        (
            "reversed, Fixed",
            InteractionShortcutDecision::Fixed {
                iterations: suggested,
            },
            &reversed,
        ),
    ] {
        let action = submit_decision(decision, allocation)
            .unwrap_or_else(|error| panic!("{label}: CR 732.2a: refused with {:?}", error.code));
        let GameAction::DeclareShortcut { count, template } = &action else {
            panic!("{label}: the ingress mints a DeclareShortcut, got {action:?}");
        };
        assert_eq!(*count, IterationCount::Fixed(suggested), "{label}");
        assert!(
            template.is_some(),
            "{label}: the client's pins are the declaration"
        );
    }
}

/// The seats an offer publishes as aimable — the player targets its `Targets` points admit.
fn published_aimable_seats(points: &[DecisionPoint]) -> Vec<PlayerId> {
    points
        .iter()
        .filter_map(|point| match &point.kind {
            DecisionPointKind::Targets { legal_targets, .. } => Some(legal_targets),
            _ => None,
        })
        .flatten()
        .filter_map(|target| match target {
            TargetRef::Player(seat) => Some(*seat),
            _ => None,
        })
        .collect::<BTreeSet<PlayerId>>()
        .into_iter()
        .collect()
}

/// **The aim-independent charge is zero on both committed drain boards.** CR 704.5a: every seat
/// these offers publish as aimable is charged nothing a declaration's aim cannot move, so a count
/// partitioned across their seats crosses where the partition says and not where the order does.
///
/// The engine's own rule, read on the boards this file already drives, through the live offer
/// `drive_to_live_declarable_offer` returns. The ceiling control, the bounded assertion and the two-seat floor all live in
/// [`crate::fantastic_four_bounded_loop::assert_no_aim_independent_charge`] and its caller here,
/// so a board demoted out of this shape reds rather than passing vacuously.
#[test]
fn every_certified_offer_dump_publishes_no_aim_independent_charge_at_its_live_offer() {
    for mut state in [
        lethal_lifegain_loss_board(),
        weird_drain_board(),
        crate::three_opponent_drain_shortcut::load_s(),
        crate::three_opponent_drain_shortcut::load_fc(),
    ] {
        drive_to_live_declarable_offer(&mut state);
        let WaitingFor::LoopShortcut {
            certificate,
            schema,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "the helper returns at a declarable offer: {:?}",
                state.waiting_for
            );
        };
        assert!(
            schema.is_bounded(),
            "reach-guard: an unbounded offer publishes no per-repetition charge to read"
        );
        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
            .expect("a bounded offer publishes its per-period signature");
        let seats = published_aimable_seats(&schema.points);
        crate::fantastic_four_bounded_loop::assert_no_aim_independent_charge(
            &state,
            per_cycle,
            &schema.points,
            &seats,
        );
    }
}

/// A committed dump's whole JSON document, inflated and parsed.
pub(crate) fn committed_document(path: &Path) -> serde_json::Value {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut json = String::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut json)
        .unwrap_or_else(|error| panic!("{} must inflate to UTF-8 JSON: {error}", path.display()));
    let mut document: serde_json::Value = serde_json::from_str(&json)
        .unwrap_or_else(|error| panic!("{} must parse as JSON: {error}", path.display()));
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets(&mut document);
    document
}

pub(crate) fn collect_gz(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("read dir entry").path();
        if path.is_dir() {
            collect_gz(&path, out);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".json.gz"))
        {
            out.push(path);
        }
    }
}

/// Whether a committed dump's own JSON says it restores AT a CR 732.2a offer carrying a certified
/// period and a published decision point — decided by reading the persisted document, with no
/// restore and no drive; a choice-free offer has no aimable seat for the row above to read.
///
/// Both envelope shapes are tried, because committed dumps use each and an envelope-only read
/// would drop a real board by name.
fn restores_at_a_certified_offer(path: &Path) -> bool {
    let document = committed_document(path);
    let board = document.get("gameState").unwrap_or(&document);
    let Some(waiting) = board.get("waiting_for") else {
        return false;
    };
    waiting.get("type").and_then(serde_json::Value::as_str) == Some("LoopShortcut")
        && waiting
            .pointer("/data/certificate/per_cycle")
            .is_some_and(|per_cycle| !per_cycle.is_null())
        && waiting
            .pointer("/data/schema/points")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|points| !points.is_empty())
}

/// **Which committed dumps the row above is the whole population of.** The boards it drives are
/// exactly the committed dumps whose own persisted JSON restores at a CR 732.2a offer carrying a
/// certified period and a decision point; a newcomer reds here and prints its own name, so it is added to that row
/// rather than found in playtesting.
///
/// # What kind of claim this is, which is NOT the kind the row above makes
///
/// The row above guards the ENGINE'S RULE on named boards. This one is CORPUS SURVEILLANCE: it
/// enforces a claim about the committed FIXTURE POPULATION, never about engine behaviour, and
/// nothing it asserts would change if the charge model changed.
///
/// # What it catches, and what it does not
///
/// It catches a dump that RESTORES at a bounded offer and misses one that must be DRIVEN to reach
/// its offer — the population it walks is every committed `*.json.gz` under `crates/`, classified
/// from the persisted document alone.
///
/// # A second walk over those same files, disclosed rather than optimised away
///
/// `fixture_deck_size_conformance` already gunzips and parses every one of them, so folding this
/// predicate in there would cost no additional suite time. That consolidation was NOT taken, on a
/// stated ground: that file lies outside this change's scope, so extending it would be a scope
/// extension for about seven seconds of walk. A later reader holding both files in one scope can
/// take it cheaply.
#[test]
fn the_committed_dumps_that_restore_at_a_certified_offer_are_the_aim_independent_rows_boards() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    collect_gz(&root, &mut files);

    let restoring: BTreeSet<String> = files
        .iter()
        .filter(|path| restores_at_a_certified_offer(path))
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    assert_eq!(
        restoring,
        BTreeSet::from([
            "exquisite_blood_tenacity_alternating_offer_4p.json.gz".to_string(),
            "exquisite_blood_tenacity_three_opponents_4p.json.gz".to_string(),
            "lethal_lifegain_loss_4p.json.gz".to_string(),
            "weird_drain_4p.json.gz".to_string(),
        ]),
        "a committed dump restoring at a certified CR 732.2a offer is one of the boards \
         `every_certified_offer_dump_publishes_no_aim_independent_charge_at_its_live_offer` \
         reads, and the difference above names the newcomer: add it to that row, or move it out \
         of the corpus"
    );
}

/// CR 800.4a: P1 stole a creature P0 owns, with a +1/+1 counter on it. P1's departure ends that
/// control effect and the creature stays under P0, so the repetition P1 leaves on conforms and
/// accepting the weird-drain board's published declaration still eliminates every opponent.
#[test]
fn a_creature_the_departing_seat_stole_returns_and_the_published_take_still_wins() {
    use engine::types::ability::{ContinuousModification, Duration, TargetFilter};
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    for stolen in [false, true] {
        let mut state = weird_drain_board();
        let live = drive_to_live_declarable_offer(&mut state);
        let id = engine::game::zones::create_object(
            &mut state,
            CardId(1),
            live.proposer,
            "Stolen Bear".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types = vec![CoreType::Creature];
        object.counters.insert(CounterType::Plus1Plus1, 1);
        if stolen {
            state.add_transient_continuous_effect(
                id,
                PlayerId(1),
                Duration::Permanent,
                TargetFilter::SpecificObject { id },
                vec![ContinuousModification::ChangeController],
                None,
            );
            engine::game::layers::mark_layers_full(&mut state);
            engine::game::layers::evaluate_layers(&mut state);
        }
        assert_eq!(
            state.objects[&id].controller,
            if stolen { PlayerId(1) } else { live.proposer }
        );

        let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
            panic!("the board stands at its offer: {:?}", state.waiting_for);
        };
        assert!(
            schema.suggested_declaration.is_none(),
            "stolen={stolen}: the published declaration already reaches the count, so no \
             suggestion is published"
        );
        let count = schema.iteration_count.clone();
        apply(
            &mut state,
            live.proposer,
            GameAction::DeclareShortcut {
                count,
                template: None,
            },
        )
        .expect("the declaration is dispatched");
        while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
            apply(
                &mut state,
                player,
                GameAction::RespondToShortcut {
                    response: engine::analysis::loop_check::ShortcutResponse::Accept,
                },
            )
            .expect("a shortcut response");
        }
        let lives: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        assert_eq!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(live.proposer)
            },
            "stolen={stolen}"
        );
        assert_eq!(lives, vec![116, 0, 0, 0], "stolen={stolen}");
        assert!(state.battlefield.contains(&id), "stolen={stolen}");
        assert_eq!(
            state.objects[&id].controller, live.proposer,
            "stolen={stolen}"
        );
    }
}
