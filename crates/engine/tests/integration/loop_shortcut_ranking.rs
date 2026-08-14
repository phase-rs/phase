//! Cross-episode CR 732.2a ranking rows that need a whole board rather than a resolver fixture.
//!
//! **Row R2-d** lives here: a ranked `AnnouncementSubject::Seat` is judged as a CR 601.2c
//! TARGET, not merely as PRESENT — and the CR 115.10a CHOICE class keeps its existence-only
//! authority on the very same board. The resolver-level zone sampling for the same arm is
//! `analysis::decision_template`'s `r1ghi_*`; this file is the board-level statement, which is
//! where the two authorities can be contrasted on ONE state.

use engine::analysis::decision_template::{
    resolve, AnnouncementSubject, ConcreteDecision, ConcreteTarget, DecisionGroupKey, DecisionKind,
    DecisionSlot, DecisionTemplate, IterationCount, PinnedDecision, Ranking, ReplayFailure,
    ReplayMode, TargetPin, TargetSchedule,
};
use engine::game::scenario::GameScenario;
use engine::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
use engine::types::game_state::{GameState, LayersDirty, YieldTarget};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// A 3-player board carrying one P0-controlled ability source on the battlefield.
///
/// The source's ZONE is load-bearing, not scenery: the `Seat` arm asks
/// `player_is_legal_target(state, seat, src_id, src_controller)`, and both of its trailing
/// arguments come from re-binding the SLOT's source through `resolve_ability_instance`. A slot
/// whose source does not resolve fails closed before any hexproof question is asked, which would
/// make every arm below refuse for the wrong reason.
fn board_with_source() -> (GameState, ObjectId) {
    let mut state = GameScenario::new_n_player(3, 7).build().state().clone();
    // Production `zones::create_object`, never a raw `objects.insert`: a raw insert never joins
    // `state.battlefield`, so `game_functioning_statics` would not see it and the grant applied
    // in `grant_hexproof` below would silently never apply.
    let source = engine::game::zones::create_object(
        &mut state,
        CardId(950),
        P0,
        "Ranked Seat Ability Source".to_string(),
        Zone::Battlefield,
    );
    (state, source)
}

/// "You have hexproof" (the Leyline of Sanctity shape), on a permanent `player` controls.
fn grant_hexproof(state: &mut GameState, player: PlayerId) {
    let grantor = engine::game::zones::create_object(
        state,
        CardId(951),
        player,
        "You Have Hexproof Source".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&grantor)
        .expect("the grantor was just created")
        .static_definitions =
        vec![
            StaticDefinition::new(StaticMode::Hexproof).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        ]
        .into();
    // Fixture bookkeeping, not a rule: a new continuous-effect source needs a layer pass to be
    // seen, which an ETB would have requested. MEASURED on this lane:
    // `create_object` does NOT re-dirty `layers_dirty`, so on a board whose pass has already run
    // (`Clean`) a bare `flush_layers` returns immediately, `refresh_static_mode_presence` never
    // runs, and the O(1) `static_mode_presence` gate answers `false` for `Hexproof` regardless of
    // what the grantor carries. Marking `Full` is what an ETB would have requested.
    state.layers_dirty = LayersDirty::Full;
    engine::game::layers::flush_layers(state);
}

fn slot_for(source: ObjectId, state: &GameState) -> DecisionSlot {
    DecisionSlot {
        source: YieldTarget::ThisObject {
            source_id: source,
            // CR 400.7: bind the LIVE incarnation, read from state rather than hard-coded — a
            // hard-coded one would fail closed the moment the harness re-enters the object and
            // the row would refuse for a bookkeeping reason instead of the rules one.
            incarnation: Some(state.objects[&source].incarnation),
            trigger_description: None,
        },
        index: 0,
    }
}

fn one_pin_template(slot: DecisionSlot, pin: TargetPin) -> DecisionTemplate {
    let sources = vec![slot.source.clone()];
    DecisionTemplate {
        owner: P0,
        decisions: vec![PinnedDecision::Targets {
            slot,
            targets: vec![pin],
        }],
        replay: ReplayMode::Scheduled {
            count: IterationCount::Fixed(1),
        },
        key: DecisionGroupKey::from_sources(&sources, DecisionKind::LoopChoice),
    }
}

fn ranked_seat(player: PlayerId) -> TargetPin {
    TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(
        AnnouncementSubject::Seat(player),
    )))
}

/// **Row R2-d.** CR 601.2c + CR 702.11c: a ranked `Seat` is judged as a TARGET. The same seat,
/// on the same board, at the same slot, is REFUSED in the TARGET-class spelling and ADMITTED in
/// the CR 115.10a CHOICE-class one — which is the whole content of the provenance split, stated
/// as one measurement.
///
/// # The three arms, and why none of them is redundant
///
/// * **(a) paired positive** — the ranked seat resolves to `ConcreteTarget::Player(P1)` on the
///   board with NO hexproof. Without it, arm (b) is equally explained by a `Seat` arm that
///   never resolves anything (a dead accessor, a slot source that does not re-bind, a
///   fail-closed branch taken for a bookkeeping reason).
/// * **(b) the claim** — the identical template on the identical board plus one "You have
///   hexproof" permanent P1 controls is `ReplayFailure::IllegalTarget`.
/// * **(c) the CHOICE-class sibling, on the SAME hexproofed board** — a `TargetPin::Player(P1)`
///   at the same slot still resolves. This is the arm that proves (b) is AUTHORITY SELECTION
///   and not a newly-strict engine: CR 115.10a says a player who is not identified by the word
///   "target" is not a target, so applying hexproof to a merely CHOSEN seat would be an
///   over-veto that refuses legal CR 732.2a proposals. Losing arm (c) is the failure mode the
///   shipped `a_shrouded_player_pin_is_still_published_by_the_offer_builder` guards from the
///   other side.
///
/// # Discrimination
///
/// * swap `evaluate_schedule`'s `Seat` arm from `targeting::player_is_legal_target` to
///   `players::player_exists_for_choice` ⇒ the hexproofed seat resolves ⇒ **(b) FAILS** while
///   (a) and (c) stay green — the exact asymmetry that makes this row about the AUTHORITY and
///   not about the board;
/// * conversely, route `resolve_target`'s `TargetPin::Player` arm through
///   `player_is_legal_target` ⇒ **(c) FAILS** ⇒ the over-veto is caught here too.
///
/// # Reach-guards
///
/// The hexproof is asserted to bite at the TARGET seam for THIS source and controller before
/// (b) is claimed, and asserted NOT to bite for a third seat on the same board — so the
/// exclusion is the hexproof rather than a blanket refusal. CR 702.11c is opponent-scoped, so
/// the source's controller is asserted to be P1's opponent.
#[test]
fn r2d_a_ranked_seat_is_judged_as_a_target_while_the_choice_class_keeps_existence_only() {
    let (clean, source) = board_with_source();
    let slot = slot_for(source, &clean);

    // ── (a) PAIRED POSITIVE: no hexproof ⇒ the ranked seat resolves ──
    let ranked = one_pin_template(slot.clone(), ranked_seat(P1));
    assert_eq!(
        resolve(&ranked, 0, &clean),
        Ok(vec![ConcreteDecision::Targets {
            slot: slot.clone(),
            targets: vec![ConcreteTarget::Player(P1)],
        }]),
        "CR 601.2c: a ranked seat whose slot source is a live battlefield object RESOLVES — \
         without this arm the refusal below is satisfied by a `Seat` arm that never resolves \
         at all"
    );

    // ── the hostile board: "You have hexproof" on a permanent P1 controls ──
    let mut hostile = clean.clone();
    grant_hexproof(&mut hostile, P1);
    let controller = hostile.objects[&source].controller;
    assert!(
        engine::game::players::is_opponent(&hostile, P1, controller),
        "reach-guard: CR 702.11c excludes only OPPONENTS' spells and abilities, so the ability \
         source's controller {controller:?} must be P1's opponent"
    );
    assert!(
        engine::game::static_abilities::player_cannot_be_targeted_by(
            &hostile, P1, source, controller
        ),
        "reach-guard: the grant must actually bite at the TARGET seam for THIS source, else \
         arm (b) proves nothing"
    );
    assert!(
        !engine::game::static_abilities::player_cannot_be_targeted_by(
            &hostile, P2, source, controller
        ),
        "reach-guard: a third seat on the same board is still targetable, so the exclusion is \
         the hexproof and not an empty legal space"
    );

    // ── (b) THE CLAIM: the TARGET-class spelling is refused ──
    assert_eq!(
        resolve(&ranked, 0, &hostile),
        Err(ReplayFailure::IllegalTarget {
            slot: slot.clone(),
            pin: ranked_seat(P1),
        }),
        "CR 601.2c + CR 702.11c: an ANNOUNCED seat is a target, so hexproof makes it an \
         ILLEGAL one. `player_exists_for_choice` would say yes here — that is the authority \
         this spelling exists to move off"
    );

    // ── (c) THE CHOICE-CLASS SIBLING on the SAME board: existence only, still admitted ──
    let chosen = one_pin_template(slot.clone(), TargetPin::Player(P1));
    assert_eq!(
        resolve(&chosen, 0, &hostile),
        Ok(vec![ConcreteDecision::Targets {
            slot,
            targets: vec![ConcreteTarget::Player(P1)],
        }]),
        "CR 115.10a: a seat that is CHOSEN rather than targeted is not subject to CR 702.11c, \
         so the choice class still resolves on the very board the target class refuses. \
         Applying the targeting exclusions here would be the over-veto — it would refuse legal \
         CR 732.2a proposals, e.g. a CR 701.34a proliferate choice"
    );
}

/// One `(kind × ephemerality)` cell of R3-b's grid. Ephemerality is a property of the KEY's
/// source — `ThisObject` is per-incarnation (CR 400.7) and therefore ephemeral, `AllCopies`
/// latches card identity and is persistent — so the cell is built by choosing the source, never
/// by setting a flag.
///
/// `pub(super)` because `fantastic_four_bounded_loop`'s cross-episode-carrier row plants the
/// same cells on the REAL 4-player board: two builders would be two definitions of
/// "ephemeral", and the one thing this grid must not have is a second opinion about its axis.
pub(super) fn grid_template(
    owner: PlayerId,
    kind: DecisionKind,
    ephemeral: bool,
    anchor: ObjectId,
) -> DecisionTemplate {
    let source = if ephemeral {
        YieldTarget::ThisObject {
            source_id: anchor,
            incarnation: Some(1),
            trigger_description: None,
        }
    } else {
        YieldTarget::AllCopies {
            card_id: CardId(9_002),
            trigger_description: None,
        }
    };
    DecisionTemplate {
        owner,
        decisions: vec![],
        replay: ReplayMode::Static,
        key: DecisionGroupKey::from_sources(&[source], kind),
    }
}

/// **Row R3-b.** The CROSS-EPISODE CARRIER: a `DecisionKind::LoopChoice` template SURVIVES the
/// CR 603.3b batch boundary, and it is owner-scoped per viewer under CR 723.4. This is P4's
/// precondition, pinned before P4 leans on it.
///
/// The seam is `GameState::clear_ephemeral_trigger_order_templates`, whose retain predicate is
/// `!(kind == TriggerOrdering && is_ephemeral())` — scoped on BOTH axes.
///
/// # Reachability was established before cause was attributed
///
/// The real 4-player drive reaches this boundary, and that is a SHIPPED ROW rather than a
/// retired probe: `fantastic_four_bounded_loop::r3b_driven_a_loop_choice_carrier_survives_a_
/// whole_accepted_f4_drive` plants the cells on the f4 dump and drives an accepted CR 732.2a
/// shortcut through `apply()` — MEASURED `3 → 2`, survivors
/// `[(LoopChoice, ephemeral), (TriggerOrdering, persistent)]`. So the boundary is reached in
/// production, and the grid below states WHICH cell it removes.
///
/// # The row is the 2×2 GRID, not one cell
///
/// A fixture planting only `LoopChoice` cannot distinguish "kind-scoped" from
/// "ephemerality-scoped": both readings keep it. All four `(kind × ephemerality)` cells are
/// planted and exactly one — `TriggerOrdering` + ephemeral — must be removed. The
/// `TriggerOrdering` EPHEMERAL cell is therefore also this row's paired positive: without it a
/// retain predicate that kept everything would pass.
///
/// # Discrimination — REVERT-PROBE, RUN
///
/// Widen the retain predicate to cover `LoopChoice` (drop the `kind ==` conjunct) ⇒ MEASURED:
/// the planted `LoopChoice` ephemeral template is gone, survivors
/// `[(TriggerOrdering, persistent)]` and the count drops `2 → 1` ⇒ the first assertion fails.
///
/// # The CR 603.5 journal half is pinned on the f4 dump, and that is a MEASURED constraint
///
/// The contrast this carrier lives inside is "the template survives, the answer journal does
/// not". The journal half is asserted by
/// `fantastic_four_bounded_loop::r3a_the_accepted_drive_ends_at_the_priority_point_with_the_window_cleared`
/// — with the `> 0` reach-guard that makes it non-vacuous — and NOT here. THE REASON IS THE
/// BOARD, NOT VISIBILITY, and the distinction matters to whoever reads this next: the READER
/// accessors are `pub` and reachable from this binary — `natural_balance.rs` calls
/// `runner.state().loop_answers_recorded()` (`:648`) and `runner.state().loop_answer(..)`
/// (`:654`). Only the WRITER, `GameState::record_loop_answer`, is `pub(crate)`, and the journal
/// field with it. What makes an assertion here vacuous is that this file's synthetic board
/// drives no answer through `apply()` at all, so `loop_answers_recorded() == 0` would be a
/// negative with no reachable paired positive — not an unreachable accessor.
///
/// # The hostile arm is MULTI-AUTHORITY, and it says so structurally
///
/// `viewer_has_private_access_to_player` is
/// `player == viewer || authorized_submitter_for_player(state, player) == viewer`, and
/// `authorized_submitter_for_player` has THREE arms (`LatchedController`, `SearcherFallback`,
/// `effective_authority_for_player`). A fixture that only says "no turn control" silences one
/// arm and leaves the search-decision arm live, which would read post-unification behaviour and
/// call it pre-unification. Both widening conjuncts are asserted absent below.
#[test]
fn r3b_a_loop_choice_carrier_survives_the_batch_boundary_and_is_owner_scoped_per_viewer() {
    let (mut state, source) = board_with_source();

    // ── the 2×2 grid: every (kind × ephemerality) cell, planted on one board ──
    state.decision_templates = vec![
        grid_template(P0, DecisionKind::LoopChoice, true, source),
        grid_template(P0, DecisionKind::TriggerOrdering, true, source),
        grid_template(P0, DecisionKind::TriggerOrdering, false, source),
        grid_template(P0, DecisionKind::LoopChoice, false, source),
    ];
    let cells = |state: &GameState| -> Vec<(DecisionKind, bool)> {
        state
            .decision_templates
            .iter()
            .map(|t| (t.key.kind, t.key.is_ephemeral()))
            .collect()
    };
    // Reach-guard on the INSTRUMENT: both axes must be genuinely distinguishable on this board,
    // else "exactly one cell removed" could be an artefact of four identical keys.
    assert_eq!(
        cells(&state),
        vec![
            (DecisionKind::LoopChoice, true),
            (DecisionKind::TriggerOrdering, true),
            (DecisionKind::TriggerOrdering, false),
            (DecisionKind::LoopChoice, false),
        ],
        "reach-guard: the planted grid must present all four cells, with `is_ephemeral()` \
         tracking the KEY SOURCE (CR 400.7 `ThisObject` vs latched `AllCopies`)"
    );
    assert!(
        state
            .decision_templates
            .iter()
            .all(|t| t.key.is_ephemeral() != t.key.is_persistent()),
        "reach-guard: the two predicates must be complementary on every planted cell, else the \
         boundary's second conjunct is being read off a degenerate axis"
    );

    state.clear_ephemeral_trigger_order_templates();

    assert_eq!(
        cells(&state),
        vec![
            (DecisionKind::LoopChoice, true),
            (DecisionKind::TriggerOrdering, false),
            (DecisionKind::LoopChoice, false),
        ],
        "CR 603.3b: the batch boundary drops EXACTLY the `TriggerOrdering` + ephemeral cell. \
         The ephemeral `LoopChoice` surviving is the CR 732.2a cross-episode carrier P4 rides; \
         the ephemeral `TriggerOrdering` being dropped is the paired positive that stops a \
         keep-everything predicate from passing. A `LoopChoice` missing here means the \
         predicate lost its KIND conjunct; a `TriggerOrdering`/ephemeral survivor means it lost \
         its EPHEMERALITY conjunct"
    );

    // ── the hostile arm: CR 723.4 owner scoping, on a board with NO second authority ──
    let (mut projected_board, anchor) = board_with_source();
    projected_board.decision_templates = vec![
        grid_template(P0, DecisionKind::LoopChoice, true, anchor),
        grid_template(P1, DecisionKind::LoopChoice, true, anchor),
    ];
    // BOTH widening conjuncts of `authorized_submitter_for_player` asserted absent
    // STRUCTURALLY. Asserting only "no turn control" leaves the search-decision arm live.
    assert!(
        projected_board.turn_decision_controller.is_none(),
        "reach-guard (arm 3): no player controls another's turn decisions on this board"
    );
    assert!(
        projected_board.active_search_decision_controls.is_empty()
            && projected_board.pending_search_found_batch.is_none(),
        "reach-guard (arms 1 and 2): no LATCHED search-decision controller and no pending \
         search batch, so `authorized_submitter_for_player` cannot widen private access \
         through the search path either"
    );
    let owners = |viewer: PlayerId| -> Vec<PlayerId> {
        engine::game::visibility::filter_state_for_viewer(&projected_board, viewer)
            .decision_templates
            .iter()
            .map(|t| t.owner)
            .collect()
    };
    assert_eq!(
        owners(P0),
        vec![P0],
        "CR 723.4: a viewer is projected their OWN carrier and not a non-owner's — the retain \
         is keyed to private access, and on this board the only access is self-access"
    );
    assert_eq!(
        owners(P1),
        vec![P1],
        "paired positive, taken on the same board: the other seat sees THEIR carrier, so the \
         absence above is owner scoping and not a sweep that dropped every template"
    );
}
