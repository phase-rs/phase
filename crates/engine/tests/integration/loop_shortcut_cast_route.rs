//! CR 732.2a: a board carrying a FUNCTIONING cast-mode trigger makes an accepted object-growth
//! take perform its period instead of registering the batched `Tokens`/`Counters`/`Life` items. Every row runs on the REAL 4-player
//! `sprout_witherbloom_realistic_lands_4p` dump through the production restore chokepoint and the
//! public `apply()` boundary; grafted and ungrafted arms are ONE OBJECT apart.
//!
//! WHY THE ROUTE EXISTS. The batched arm never casts anything — the cast event belongs to the
//! ELIDED period, not to the collapse — so a batched accept re-performs a cast-sourced per-cycle
//! effect ZERO times where live play performs it once per cycle. CR 601.2i is the cast event, and
//! CR 113.6 / CR 113.6b are the zone gate keeping a library-resident cast trigger from counting.
//! The whole replay disjunction sits under `!batched.is_empty()`, so the replay route is only
//! ever the better version of a registration the batched arm would have made, never a
//! performance out of nothing (`mana_engine_with_cast_trigger_registers_nothing` is its pin).

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
/// The registered discriminant as a short name, so a failure message names what was actually
/// observed instead of dumping a whole `CopiableValues` payload. Exhaustive, no wildcard.
fn route_name(m: &PersistentAxisMaterialization) -> &'static str {
    match m {
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

/// R-route-assert: the accept registered the batched items.
fn assert_batched(state: &GameState) {
    assert!(
        !registered_routes(state).is_empty(),
        "R-route-assert: P0's accept registered NOTHING — expected the batched route"
    );
}

/// P0's battlefield Saprolings.
fn saprolings(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == P0 && o.name == "Saproling")
        })
        .count()
}

/// R-route-assert: the take performed `n` cycles of one Saproling each and registered nothing.
fn assert_performed(state: &GameState, offered: usize, n: u32) {
    let observed: Vec<&'static str> = registered_routes(state).iter().map(route_name).collect();
    assert_eq!(
        saprolings(state),
        offered + n as usize,
        "R-route-assert: the take performs its {n} cycles; registered {observed:?}"
    );
    assert!(
        observed.is_empty(),
        "R-route-assert: a performed take registers nothing, observed {observed:?}"
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

/// **The grafted board performs its take; the untouched shipped board, ONE OBJECT AWAY, still
/// routes batched.**
///
/// The ungrafted arm is THE discriminator, and its board is NON-TRIVIAL rather than empty: of the
/// dump's active trigger definitions exactly one passes the CR 113.6 zone gate (an ETB-keyed
/// def), while every `SpellCast`-keyed def it carries is library-resident with `trigger_zones`
/// naming only Battlefield or Stack. So that arm tests the ZONE GATE — a real zero with a live
/// same-gate control — not an absence of triggers.
#[test]
fn cast_trigger_board_routes_to_replay_untouched_board_stays_batched() {
    // ── the discriminator: untouched shipped board ⇒ batched ──
    let mut ungrafted = offer_state(false);
    declare_and_accept_all(&mut ungrafted, P0, 100);
    assert_batched(&ungrafted);

    // ── one grafted functioning cast trigger ⇒ performed at the take ──
    let mut grafted = offer_state(true);
    let offered = saprolings(&grafted);
    declare_and_accept_all(&mut grafted, P0, 2);
    assert_performed(&grafted, offered, 2);
}

/// **The multi-authority hostile fixture.** Two accepts by one controller in ONE phase produce
/// TWO route decisions. That is what makes the route a per-ACCEPT decision rather than a per-phase
/// one: the cast trigger is grafted BETWEEN the two accepts, so accept #1 is batched and accept #2
/// is performed at its take on the same board in the same phase.
///
/// The boundary assertion is the CR 732.2c property at row scale ("the shortcut is taken; the game
/// advances to the last proposed ending point"): the prompt the batched accept owns offers the
/// count it was accepted at, and the performed accept neither adds to its stash nor moves its
/// ceiling. `boundary_max` panics unless exactly one collapse prompt addressed to the loop's
/// controller exists.
#[test]
fn two_accepts_one_phase_one_batched_one_performed_at_the_take() {
    let mut state = offer_state(false);
    let phase_at_first_accept = state.phase;

    // ── accept #1: no cast trigger on the board yet ⇒ batched ──
    declare_and_accept_all(&mut state, P0, 100);
    assert_batched(&state);
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

    // ── accept #2, at a SMALLER count: the cast trigger is now functioning ⇒ performed at the
    // take, and a performed accept that lowered the bound would cap the boundary at 2 ──
    let offered = saprolings(&state);
    declare_and_accept_all(&mut state, P0, 2);
    assert_eq!(
        saprolings(&state),
        offered + 2,
        "R-mixed: accept #2 performs its cycles at the take"
    );

    // The stash is the multi-authority evidence: accept #1's item alone.
    let observed: Vec<&'static str> = registered_routes(&state).iter().map(route_name).collect();
    assert_eq!(
        observed,
        vec!["Tokens"],
        "R-mixed: the stash holds the batched accept #1 alone — the route is decided PER ACCEPT \
         from the board as it stands at that instant"
    );

    // ── one boundary, the batched accept's count ──
    drive_to_boundary(&mut state);
    assert_eq!(
        boundary_max(&state),
        100,
        "R-mixed: CR 732.2c makes the boundary's amount the batched accept's count, which the \
         performed accept does not lower"
    );
}

// ===========================================================================
// The ASYMMETRY row. A board whose BATCHED arm would register NOTHING must not be
// dragged onto the replay by the cast disjunct.
// ===========================================================================

/// **A real Basalt Monolith + Power Artifact mana engine carrying a functioning cast trigger
/// still stands on the mark and registers NOTHING**, rather than performing the period at the take
/// for growth no batched item would carry. The route seam's arms are ASYMMETRIC — the batched arm
/// registers CONDITIONALLY (token profile / counter growth / life growth, none of which a mana
/// engine has), and `cast_sourced` is the only route disjunct
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
    // ── (2) reach-guard: the offer carries its confirmed period, so the take routes on it ──
    assert!(
        matches!(
            &rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { period, .. } if !period.is_empty()
        ),
        "reach-guard: the multi-action mana period is confirmed, so the cast disjunct is the \
         only conjunct left to decide the route"
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
