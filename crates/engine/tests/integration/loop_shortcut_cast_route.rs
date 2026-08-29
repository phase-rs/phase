//! CR 732.2a: a board carrying a FUNCTIONING cast-mode trigger routes an accepted object-growth
//! collapse to the concrete `DriveSequence` replay instead of the batched
//! `Tokens`/`Counters`/`Life` items. Every row runs on the REAL 4-player
//! `sprout_witherbloom_realistic_lands_4p` dump through the production restore chokepoint and the
//! public `apply()` boundary; grafted and ungrafted arms are ONE OBJECT apart.
//!
//! WHY THE ROUTE EXISTS. The batched arm never casts anything — the cast event belongs to the
//! ELIDED period, not to the collapse — so a batched accept re-performs a cast-sourced per-cycle
//! effect ZERO times where live play performs it once per cycle. CR 601.2i is the cast event, and
//! CR 113.6 / CR 113.6b are the zone gate keeping a library-resident cast trigger from counting.
//! The whole replay disjunction sits under `!batched.is_empty()`, so the replay route is only
//! ever the better version of a registration the batched arm would have made, never a
//! registration out of nothing (`mana_engine_with_cast_trigger_registers_nothing` is its pin).

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::analysis::resource::ResourceAxis;
use engine::game::engine::apply;
use engine::game::functioning_abilities::active_trigger_definitions;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::ability::TriggerDefinition;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, LoopDetectionMode, PayableResource, PersistentAxisMaterialization, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::loop_shortcut_mana_engine::{
    drive_one_period, mana_ability_index, setup, untap_ability_index,
};
use super::sprout_inalla_realistic_offer::{drive_sprout_cast, load_realistic_dump};
use super::support::shared_card_db;

const P0: PlayerId = PlayerId(0);
/// Sprout Swarm in P0's hand in the realistic 4p dump.
const SPROUT: ObjectId = ObjectId(405);
/// The fodder `drive_sprout_cast` convokes for the {G}. Recorded here because the two-accept row's
/// SECOND cast must use a different one.
const FIRST_CONVOKE_FODDER: ObjectId = ObjectId(406);
/// A second untapped P0 fodder Saproling (406–410, 412 are untapped in the dump).
const SECOND_CONVOKE_FODDER: ObjectId = ObjectId(407);
/// `game::engine::MAX_SHORTCUT_CYCLES`, mirrored because it is `pub(crate)` and this binary is an
/// external crate — the same mirror `fantastic_four_bounded_loop.rs` keeps. It is the LARGEST
/// count `handle_declare_shortcut` accepts (it refuses `Fixed(n)` for `n > MAX_SHORTCUT_CYCLES`
/// and for `n > schema.max_iterations`, and the object-growth mint publishes exactly this), so the
/// large-N arm below runs at the engine's own ceiling rather than at an arbitrary big number.
const MAX_SHORTCUT_CYCLES_MIRROR: u32 = 1_000;

/// Test-crate mirror of the engine's private `LoopCollapseRoute`. It exists because the production
/// enum is private and MUST STAY private — this is the OBSERVABLE, not a copy of the
/// decision. The mapping in [`route_of`] is the only place the proxy is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedRoute {
    /// registers `PersistentAxisMaterialization::DriveSequence { .. }`
    Replay,
    /// registers one of the batched axes (`Tokens` / `Counters` / `Life`)
    Batched,
}

/// Exhaustive, no wildcard: a future persistent axis must be classified here deliberately rather
/// than defaulting into `Batched` — the same obligation the production `match` carries.
fn route_of(m: &PersistentAxisMaterialization) -> ExpectedRoute {
    match m {
        PersistentAxisMaterialization::DriveSequence { .. } => ExpectedRoute::Replay,
        PersistentAxisMaterialization::Tokens(_)
        | PersistentAxisMaterialization::Counters(_)
        | PersistentAxisMaterialization::Life { .. } => ExpectedRoute::Batched,
    }
}

/// The registered discriminant as a short name, so a failure message names what was actually
/// observed instead of dumping a whole `CopiableValues` payload. Exhaustive for the same reason.
fn route_name(m: &PersistentAxisMaterialization) -> &'static str {
    match m {
        PersistentAxisMaterialization::DriveSequence { .. } => "DriveSequence",
        PersistentAxisMaterialization::Tokens(_) => "Tokens",
        PersistentAxisMaterialization::Counters(_) => "Counters",
        PersistentAxisMaterialization::Life { .. } => "Life",
    }
}

/// Everything P0's accepts have registered, in stash order.
fn registered_routes(state: &GameState) -> &[PersistentAxisMaterialization] {
    state
        .pending_unbounded_materialization
        .get(&P0)
        .map_or(&[], Vec::as_slice)
}

/// R-route-assert — the instrument standard. PANICS with the observed discriminants on a silent
/// fall to the batched route, so no row in this file can report a bound (or a fast number) without
/// having asserted its route first. The empty-stash assertion is what stops a vacuous pass on a
/// board that registered nothing at all.
fn assert_route(state: &GameState, expected: ExpectedRoute) {
    let stash = registered_routes(state);
    assert!(
        !stash.is_empty(),
        "R-route-assert: P0's accept registered NOTHING — there is no route to assert, so any \
         bound read after this point would be vacuous"
    );
    let observed: Vec<&'static str> = stash.iter().map(route_name).collect();
    assert!(
        stash.iter().all(|m| route_of(m) == expected),
        "R-route-assert: expected every registered materialization on the {expected:?} route, \
         observed {observed:?}"
    );
}

/// Graft a bare functioning cast-mode trigger onto a NEW P0 battlefield object — the one-object
/// difference between the grafted and ungrafted arms.
///
/// `TriggerMode::SpellCast` with no `valid_card` and no `execute`: the predicate under test keys on
/// `TriggerEventKey::SpellCast(_)` with the payload DISCARDED, so the bare mode is exactly what it
/// must see. Battlefield-resident with empty `trigger_zones`, so CR 113.6's default branch makes it
/// FUNCTION — which is the property the dump's own six library-resident cast triggers lack.
fn graft_cast_trigger(state: &mut GameState, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let host = create_object(state, card_id, P0, name.to_string(), Zone::Battlefield);
    state
        .objects
        .get_mut(&host)
        .expect("the just-created graft host is in `objects`")
        .trigger_definitions
        .push(TriggerDefinition::new(TriggerMode::SpellCast));
    // Positional read-back (`Definitions<T>` exposes no `iter()`): prove the graft actually landed,
    // so a row that later reads `Batched` is a route failure rather than a fixture failure.
    let entries = &state
        .objects
        .get(&host)
        .expect("graft host present")
        .trigger_definitions;
    assert_eq!(
        entries.len(),
        1,
        "the graft host carries exactly one trigger"
    );
    assert_eq!(
        entries
            .get(0)
            .expect("positional read-back of the grafted entry")
            .definition
            .mode,
        TriggerMode::SpellCast,
        "the grafted trigger is cast-mode"
    );
    host
}

/// The realistic board driven to its CR 732.2a offer by one real buyback+convoke recast.
fn offer_state(graft: bool) -> GameState {
    let mut state = load_realistic_dump();
    if graft {
        graft_cast_trigger(&mut state, "Cast Route Probe");
    }
    let outcome = drive_sprout_cast(state);
    let state = outcome.state().clone();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the live recast must surface P0's CR 732.2a offer{}, got {:?}",
        if graft {
            " EVEN WITH the cast trigger grafted"
        } else {
            ""
        },
        state.waiting_for
    );
    state
}

/// Proposer declares `Fixed(n)`; every living opponent accepts (APNAP).
fn declare_and_accept_all(state: &mut GameState, proposer: PlayerId, n: u32) {
    apply(
        state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        },
    )
    .expect("the proposer declares the object-growth shortcut");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
}

/// Pass priority through the real production path until the CR 500.5 step/phase boundary surfaces
/// a non-`Priority` prompt. Bounded so a wedge fails loudly instead of hanging.
fn drive_to_boundary(state: &mut GameState) {
    let start_phase = state.phase;
    for _ in 0..64 {
        let WaitingFor::Priority { player } = state.waiting_for.clone() else {
            return;
        };
        apply(state, player, GameAction::PassPriority)
            .expect("pass priority toward the next phase boundary");
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || state.phase != start_phase {
            return;
        }
    }
    panic!("drive_to_boundary: no CR 500.5 boundary within 64 passes");
}

/// The ceiling the CR 500.5 boundary prompt publishes to the loop's controller.
fn boundary_max(state: &GameState) -> u32 {
    let WaitingFor::PayAmountChoice {
        player,
        resource: PayableResource::LoopCollapse { .. },
        max,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "the CR 500.5 boundary must prompt P0 for the collapse count, got {:?}",
            state.waiting_for
        )
    };
    assert_eq!(*player, P0, "the loop controller is prompted");
    *max
}

// ===========================================================================
// The route pair, written as ONE test so the two arms are structurally inseparable: the grafted
// arm alone is satisfiable by a blanket route change, and the ungrafted arm alone by never
// wiring the disjunct.
// ===========================================================================

/// **The grafted board routes to the concrete replay; the untouched shipped board, ONE OBJECT
/// AWAY, still routes batched.**
///
/// The ungrafted arm is THE discriminator, and its board is NON-TRIVIAL rather than empty: of the
/// dump's active trigger definitions exactly one passes the CR 113.6 zone gate (an ETB-keyed
/// def), while every `SpellCast`-keyed def it carries is library-resident with `trigger_zones`
/// naming only Battlefield or Stack. So that arm tests the ZONE GATE — a real zero with a live
/// same-gate control — not an absence of triggers.
///
/// The large-N arm is a COUNT-INDEPENDENCE pin, not a performance row: it re-runs the grafted arm
/// at `MAX_SHORTCUT_CYCLES`, the largest count the declare authority accepts. Today that is
/// structural — `materialize_object_growth_shortcut` never receives `n`, so no route decision can
/// read it — and the arm exists to keep it that way. It is cheap: the accept only REGISTERS a
/// `DriveSequence`, and the cycles replay later at a CR 500.5 boundary this arm never drives to.
#[test]
fn cast_trigger_board_routes_to_replay_untouched_board_stays_batched() {
    // ── the discriminator: untouched shipped board ⇒ batched ──
    let mut ungrafted = offer_state(false);
    declare_and_accept_all(&mut ungrafted, P0, 100);
    assert_route(&ungrafted, ExpectedRoute::Batched);

    // ── one grafted functioning cast trigger ⇒ concrete replay ──
    let mut grafted = offer_state(true);
    declare_and_accept_all(&mut grafted, P0, 100);
    assert_route(&grafted, ExpectedRoute::Replay);

    // ── large N: same board, the engine's maximum accepted count, same route ──
    let mut grafted_at_ceiling = offer_state(true);
    declare_and_accept_all(&mut grafted_at_ceiling, P0, MAX_SHORTCUT_CYCLES_MIRROR);
    assert_route(&grafted_at_ceiling, ExpectedRoute::Replay);
}

/// **The NON-BLANKET discriminator.** The per-axis `collapsed_axes` filter at the `Replay` arm
/// keeps every `DeferredAccrual` axis: this already-shipped realistic dump's `DriveSequence`
/// still names the loop's marked axis set EXACTLY, with nothing dropped.
///
/// It is its own row because the mixed-axis rows in `combo_infinite_pile` assert only that a
/// `Mana(_)` axis is EXCLUDED, which an over-aggressive implementation that empties every
/// `collapsed_axes` also satisfies. This row upgrades the sibling above's DISCRIMINANT-only route
/// assertion to an EXACT-SET assertion on the same board. Its reach-guard is the shipped
/// [`assert_route`] instrument on the same arm, which panics on an empty stash so a ROUTE
/// regression cannot quietly make the exact-set assertion unreachable.
///
/// REVERT PROBE: invert the filter at `game::engine::materialize_object_growth_shortcut`'s
/// `Replay` arm (keep only `StandingCapability`, or drop `DeferredAccrual`) ⇒ `collapsed_axes`
/// empties ⇒ RED here while the mixed-axis rows stay green.
#[test]
fn replay_collapse_names_every_deferred_axis_the_loop_marked() {
    let mut grafted = offer_state(true);
    declare_and_accept_all(&mut grafted, P0, 100);

    // REACH-GUARD: a real replay registration exists to read an exact set off.
    assert_route(&grafted, ExpectedRoute::Replay);

    // The ∞-mark set this accept wrote — the input the filter narrows. Sorted, because it comes
    // from a `BTreeSet`.
    let marked: Vec<ResourceAxis> = grafted
        .unbounded_resources
        .get(&P0)
        .expect("the accept marks P0's ∞ axes")
        .iter()
        .copied()
        .collect();
    assert!(
        !marked.is_empty(),
        "reach-guard: an empty ∞-mark set would make the exact-set assertion below vacuous"
    );

    let stash = registered_routes(&grafted);
    let [PersistentAxisMaterialization::DriveSequence { collapsed_axes, .. }] = stash else {
        panic!("assert_route already pinned the Replay route; got {stash:?}")
    };

    // EXACT SET, not `contains`: every axis this board marks is `DeferredAccrual`
    // (`ResourceAxis::unbounded_mark_kind`), so the accountability filter must drop NOTHING here.
    // `collapsed_axes` preserves `proposal.unbounded`'s order, so it is sorted for the comparison.
    let mut named = collapsed_axes.clone();
    named.sort();
    assert_eq!(
        named, marked,
        "CR 732.2c: this loop marks only DEFERRED axes, so the accountability filter is the \
         IDENTITY on it. A filter that empties or narrows `collapsed_axes` reds here while the \
         mixed-axis rows in `combo_infinite_pile` stay green — which is the whole reason this row \
         is separate from them. marked={marked:?}"
    );
}

/// **The multi-authority hostile fixture.** Two accepts by one controller in ONE phase produce
/// TWO route decisions sharing ONE stash and ONE boundary amount. That is what makes the route a
/// per-ACCEPT decision rather than a per-phase one: the cast trigger is grafted BETWEEN the two
/// accepts, so accept #1 is batched and accept #2 is replay on the same board in the same phase.
///
/// The stash-composition assertion is not replaceable by the bound alone — a bound-only row would
/// pass on a board where BOTH accepts took the same route. The boundary assertion is the CR
/// 732.2c property at row scale ("the shortcut is taken; the game advances to the last proposed
/// ending point"), so the single prompt the two accepts share must offer the count they were
/// accepted at, on BOTH routes. `boundary_max` panics unless exactly one collapse prompt
/// addressed to the loop's controller exists, so a route that published `MAX_SHORTCUT_CYCLES`,
/// zero, or a second prompt fails here.
#[test]
fn two_accepts_one_phase_one_batched_one_replay_share_one_boundary() {
    let mut state = offer_state(false);
    let phase_at_first_accept = state.phase;

    // ── accept #1: no cast trigger on the board yet ⇒ batched ──
    declare_and_accept_all(&mut state, P0, 100);
    assert_route(&state, ExpectedRoute::Batched);
    assert_eq!(
        registered_routes(&state).len(),
        1,
        "reach-guard: the first accept registered exactly one materialization"
    );

    // ── graft BETWEEN the accepts, then cast again in the SAME phase ──
    graft_cast_trigger(&mut state, "Cast Route Probe");
    let second = state
        .objects
        .get(&SECOND_CONVOKE_FODDER)
        .expect("the second convoke fodder is present");
    assert!(
        second.controller == P0 && !second.tapped,
        "fixture fact: the FIRST convoke tapped {FIRST_CONVOKE_FODDER:?}, so accept #2 needs \
         {SECOND_CONVOKE_FODDER:?} — it must still be an untapped P0 permanent"
    );
    let outcome = GameRunner::from_state(state)
        .cast(SPROUT)
        .accept_optional()
        .convoke_with(&[SECOND_CONVOKE_FODDER])
        .commit()
        .resolve();
    let mut state = outcome.state().clone();
    assert_eq!(
        state.phase, phase_at_first_accept,
        "R-mixed precondition: both accepts must land in ONE phase, so they share one CR 500.5 \
         boundary and one bound"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the second recast must surface a second offer, got {:?}",
        state.waiting_for
    );

    // ── accept #2: the cast trigger is now functioning ⇒ replay ──
    declare_and_accept_all(&mut state, P0, 100);

    // The stash is the multi-authority evidence: two items, ONE per route.
    let observed: Vec<&'static str> = registered_routes(&state).iter().map(route_name).collect();
    assert_eq!(
        observed,
        vec!["Tokens", "DriveSequence"],
        "R-mixed: one stash holding the batched accept #1 and the replay accept #2 — the route is \
         decided PER ACCEPT from the board as it stands at that instant"
    );

    // ── one boundary, one amount: `min(100, 100)`, the count both accepts were taken at ──
    drive_to_boundary(&mut state);
    assert_eq!(
        boundary_max(&state),
        100,
        "R-mixed: ONE boundary applies ONE amount to every stashed item, and CR 732.2c makes that \
         amount the accepted count on both routes — neither route lowers the ceiling its own \
         accept published"
    );
}

// ===========================================================================
// The ASYMMETRY row. A board whose BATCHED arm would register NOTHING must not be
// dragged onto the replay by the cast disjunct.
// ===========================================================================

/// **A real Basalt Monolith + Power Artifact mana engine carrying a functioning cast trigger
/// still registers NOTHING**, rather than scheduling a `DriveSequence` that would deliver
/// nothing: uncapped cubic replay cost plus a spurious CR 500.5 collapse prompt for a loop with
/// nothing to collapse. The route seam's arms are ASYMMETRIC — the batched arm registers
/// CONDITIONALLY (token profile / counter growth / life growth, none of which a mana engine has)
/// while the replay arm registers UNCONDITIONALLY, and `cast_sourced` is the only route disjunct
/// with no axis-shaped conjunct. Without `!batched.is_empty()` ANY functioning cast trigger
/// anywhere flips this rig: `functioning_board_trigger_defs` walks `state.objects.values()` with
/// NO controller filter, so an OPPONENT's is enough.
///
/// Guards (1)–(4) below prove reachability in-row rather than assuming it, since a fixture that
/// never reaches the disjunct passes vacuously. The paired POSITIVE is not on this rig — a mana
/// engine cannot be given a batched payload and stay one — it is the grafted arm of
/// [`cast_trigger_board_routes_to_replay_untouched_board_stays_batched`].
#[test]
fn mana_engine_with_cast_trigger_registers_nothing() {
    // DORMANT in a normal checkout (`integration_cards.json.gz` is tracked); it only fires in a
    // checkout without the card-data pipeline.
    let Some(db) = shared_card_db() else { return };
    // The mana rig is built on `game::scenario::P0`; this file's `P0` must be the same seat for
    // the graft to land on the loop controller's board at all.
    assert_eq!(
        P0,
        engine::game::scenario::P0,
        "fixture fact: both modules mean the same seat"
    );
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let host = graft_cast_trigger(rig.runner.state_mut(), "Mana Route Probe");

    // ── (1) reach-guard: the scan's per-object authority yields the grafted cast-mode def on THIS
    // board (the graft is not merely present in `objects`, it survives the CR 702.26b / CR 114.4
    // gate that `functioning_board_trigger_defs` applies before the zone gate) ──
    let state = rig.runner.state();
    let active: Vec<&TriggerDefinition> = active_trigger_definitions(
        state,
        state.objects.get(&host).expect("the graft host is present"),
    )
    .map(|entry| entry.definition)
    .collect();
    assert_eq!(
        active.len(),
        1,
        "reach-guard: the graft host carries exactly one ACTIVE trigger def on the mana rig's board"
    );
    assert_eq!(
        active[0].mode,
        TriggerMode::SpellCast,
        "reach-guard: the grafted cast trigger survives the gate `functioning_board_trigger_defs` \
         applies before the zone gate — without this the row never reaches the cast disjunct and \
         passes vacuously"
    );

    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt's {T}: Add {C}{C}{C} mana ability");
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt's {3}: Untap this artifact ability");
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "reach-guard: the mana-engine offer must still fire WITH the cast trigger grafted, got {:?}",
        rig.runner.state().waiting_for
    );
    // ── (2) reach-guard: the captured period is the two-activation Basalt+Power cycle, so the
    // route seam's `!sequence.is_empty()` conjunct is satisfied ──
    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
        2,
        "reach-guard: the multi-action mana period is captured, so `!sequence.is_empty()` holds \
         and the cast disjunct is the only conjunct left to decide the route"
    );

    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    // ── (3) reach-guard: the accept really ran the materialize path — only it marks the axis ──
    assert!(
        rig.runner
            .state()
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_)))),
        "reach-guard: the accept reaches materialize_object_growth_shortcut and marks Mana(_)"
    );

    // ── (4) DISCRIMINATOR: it registered NOTHING, preserving the ∞ mana mark ──
    let observed: Vec<&'static str> = rig
        .runner
        .state()
        .pending_unbounded_materialization
        .values()
        .flatten()
        .map(route_name)
        .collect();
    assert!(
        rig.runner
            .state()
            .pending_unbounded_materialization
            .is_empty(),
        "a mana engine registers NO deferred materialization even with a functioning cast trigger \
         on the board — the batched arm would register nothing, so there is nothing for the \
         replay to be a better version OF, and scheduling one buys uncapped cubic replay cost \
         plus a spurious CR 500.5 collapse prompt for a loop with nothing to collapse; observed \
         {observed:?}"
    );
}
