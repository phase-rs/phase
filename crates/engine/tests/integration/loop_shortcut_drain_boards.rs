//! Two real 4-player drain boards at a CR 732.2a loop-shortcut offer, and what each one
//! publishes there.
//!
//! `lethal_lifegain_loss_4p.json.gz` is derived from `LethalLifeGainLossLoop.zip` and
//! `weird_drain_4p.json.gz` from `weird-drain-behavior.zip`, both through
//! `scripts/migrate-dump-fixture.sh --effect-kind LoseLife --deck-size Exactly:100`. The
//! committed artifact IS the gzip stream, and that script's `--control` mode holds a
//! fresh run of the whole recipe against those exact bytes — so a regeneration must
//! re-gzip (`gzip -9 -n`), never commit a bare `.json`.
//!
//! The pair is its own positive-and-negative control: the weird-drain board's RESTORED
//! offer is directly declarable, and the lethal board's is not — its only legal answer is
//! to decline. A live declarable offer is reached on either board only by declining and
//! driving forward, which is why nothing here asserts against a restored offer.

use std::collections::BTreeSet;
use std::io::Read;

use engine::analysis::decision_template::{
    AnnouncementSubject, DecisionPoint, DecisionPointKind, DecisionSlot, IterationCount,
    PinnedDecision, TargetPin, TargetSchedule,
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
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("the dump envelope parses as JSON");
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
/// every re-aimable choice at the LATCHED seat, and take an optional-effect prompt.
///
/// The seat is latched at the first beat that offers one, as the LOWEST legal seat rather
/// than in publisher order, and re-asserted legal at every later beat — a drive that
/// silently re-aimed would move the certificate's losing seat under the rows that read it.
fn drive_one_beat(state: &mut GameState, aimed_at: &mut Option<PlayerId>) {
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

    let optional = actions
        .iter()
        .find(|action| matches!(action, GameAction::DecideOptionalEffect { accept: true }))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "this drive policy answers priority, a player-target choice and an optional-effect \
                 prompt; unhandled {:?}",
                state.waiting_for
            )
        });
    submit(state, who, optional);
}

fn legal_actions_at_offer(state: &GameState, proposer: PlayerId) -> Vec<GameAction> {
    engine::ai_support::legal_actions_for_viewer(state, proposer).0
}

/// Decline the restored offer and drive to the next CR 732.2a offer that is actually
/// DECLARABLE, returning the proposer and the seat the drive latched.
///
/// The declarability assertion before returning is this helper's liveness control: on the
/// lethal board the restored offer's only legal answer is to decline, so a caller handed
/// that offer would be reading a dead board rather than a negative.
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

/// CR 704.5a: re-derive a live offer's `max_iterations` from PUBLISHED data alone — the
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
/// the count reaches that seat's own crossing, unless doing so would mint the offer gate's
/// un-narrowed sentinel.
pub(crate) fn relieve_strict_bound(strict: &[i64], ceiling: i64) -> i64 {
    let Some(&floor) = strict.iter().min() else {
        return ceiling;
    };
    let relieved = floor + 1;
    if strict.iter().filter(|b| **b == floor).count() == 1 && relieved < ceiling {
        relieved.clamp(0, ceiling)
    } else {
        floor.clamp(0, ceiling)
    }
}

pub(crate) fn rederive_live_offer_bound(state: &GameState, aimed_at: PlayerId) -> u32 {
    let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
        panic!("not at an offer: {:?}", state.waiting_for);
    };
    let per_cycle = certificate
        .per_cycle
        .as_ref()
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
        .expect("a bounded offer publishes its per-cycle signature");

    assert_eq!(*proposer, offer.proposer);

    // Row 6 — the certificate the drive reached reserved elimination headroom for someone.
    // A RESTORED offer's set deserializes empty (`#[serde(default)]` on a field older
    // saves lack), which is why no row may read one.
    assert!(
        !per_cycle.declarable_victims.is_empty(),
        "a live offer publishes the seats CR 704.5a headroom was reserved for; empty is the \
         restored-offer shape"
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

    // Row 9 — the two published count fields agree, and the bound's VALUE is the one
    // `rederive_live_offer_bound` computes from this offer's own published data and the seat
    // the drive latched. Dropping the aim subtraction moves the published bound off this
    // re-derivation on every board that charges an aimed slot.
    let bound = schema.max_iterations;
    assert_eq!(schema.iteration_count, IterationCount::Fixed(bound));
    assert_eq!(
        bound,
        rederive_live_offer_bound(state, offer.aimed_at),
        "CR 704.5a: `max_iterations` is the MIN over every living seat's headroom divided by \
         what one repetition charges it — the slot's magnitude on every seat it reaches, less \
         what the window saw it aim at that seat"
    );
}

/// Row 5 — the control pair. Each board is the other's control, and the difference is
/// exactly the property every later row depends on.
#[test]
fn the_two_restored_offers_disagree_on_declarability() {
    let weird = weird_drain_board();
    let WaitingFor::LoopShortcut {
        proposer: weird_proposer,
        declaration: weird_declaration,
        ..
    } = &weird.waiting_for
    else {
        panic!(
            "the weird-drain fixture restores at its offer: {:?}",
            weird.waiting_for
        );
    };
    let weird_actions = legal_actions_at_offer(&weird, *weird_proposer);
    assert!(
        weird_actions
            .iter()
            .any(|action| matches!(action, GameAction::DeclareShortcut { .. })),
        "the weird-drain board's restored offer is directly declarable: {weird_actions:?}"
    );
    assert!(
        weird_declaration.is_some(),
        "and it publishes the declaration the engine can already specify"
    );

    let lethal = lethal_lifegain_loss_board();
    let WaitingFor::LoopShortcut {
        proposer: lethal_proposer,
        declaration: lethal_declaration,
        ..
    } = &lethal.waiting_for
    else {
        panic!(
            "the lethal fixture restores at its offer: {:?}",
            lethal.waiting_for
        );
    };
    let lethal_actions = legal_actions_at_offer(&lethal, *lethal_proposer);
    assert!(
        lethal_actions
            .iter()
            .any(|action| matches!(action, GameAction::DeclineShortcut)),
        "reach-guard: the lethal board's restored offer IS an offer with a legal answer, so the \
         refusal below is a negative and not a dead read: {lethal_actions:?}"
    );
    assert!(
        !lethal_actions
            .iter()
            .any(|action| matches!(action, GameAction::DeclareShortcut { .. })),
        "the lethal board's restored offer can only be declined: {lethal_actions:?}"
    );
    assert!(
        lethal_declaration.is_none(),
        "and it publishes no declaration"
    );
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

/// The restored/live pair on the SAME board, which is what licenses the rule that no row
/// asserts against a restored offer: `declarable_victims` is snapshotted at the mint and
/// lossy across the wire for a save written before the field existed.
#[test]
fn declarable_victims_are_empty_when_restored_and_populated_when_live() {
    for mut state in [lethal_lifegain_loss_board(), weird_drain_board()] {
        let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
            panic!("restores at its offer: {:?}", state.waiting_for);
        };
        let restored = certificate
            .per_cycle
            .as_ref()
            .expect("the restored offer carries its per-cycle signature")
            .declarable_victims
            .clone();
        assert!(
            restored.is_empty(),
            "a restored certificate's declarable_victims deserialize empty: {restored:?}"
        );

        drive_to_live_declarable_offer(&mut state);
        let WaitingFor::LoopShortcut { certificate, .. } = &state.waiting_for else {
            panic!("drove to an offer: {:?}", state.waiting_for);
        };
        assert!(
            !certificate
                .per_cycle
                .as_ref()
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
        DecisionSlot::target(YieldTarget::ThisObject {
            source_id: ObjectId(source_id),
            incarnation: Some(0),
            trigger_description: None,
        })
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
