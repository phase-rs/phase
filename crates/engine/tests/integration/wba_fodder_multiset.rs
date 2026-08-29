//! CR 732.2a: the object-growth gate admits a HOMOGENEOUS k-multiset and carries `k` to the
//! boundary mint, while a period whose `k` was produced by a live `CreateToken` REPLACEMENT is
//! routed to the concrete replay instead of being batched.
//!
//! WHY THE ROUTE CONJUNCT EXISTS — elision ≡ performance. The boundary `Tokens` mint RE-RUNS the
//! replacement pipeline (`token_copy::drive_copy_token_batches` -> `ProposedEvent::CreateToken`
//! -> `replacement::replace_event`, and a different source's replacement still applies there).
//! `derived_fodder_class` is a before/after DIFF, so its `k` is the OBSERVED count, already
//! including the doubler's multiplication. Batching `k·N` would apply it twice: elision `2k·N`
//! against performance `k·N`. The route guard (`analysis::resource::token_growth_is_observed`,
//! gated on `k > 1`) sends exactly those periods to `DriveSequence`, where each cycle's
//! replacement applies once, by performance. Every arm runs on the REAL 4-player
//! `sprout_witherbloom_realistic_lands_4p` dump through the production restore chokepoint and the
//! public `GameRunner`/`apply()` boundary, ONE OBJECT away from the shipped-green board.

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::game::engine::apply;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::ability::{
    ControllerRef, CopiableValues, QuantityModification, ReplacementDefinition,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, PayableResource, PersistentAxisMaterialization, TokenGrowth, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use super::sprout_inalla_realistic_offer::{drive_sprout_cast, load_realistic_dump};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
/// Sprout Swarm in P0's hand in the realistic 4p dump.
const SPROUT: ObjectId = ObjectId(405);
/// P0's untapped fodder Saprolings in the dump (406–410 and 412 are untapped; 411 is TAPPED).
const UNTAPPED_FODDER: [ObjectId; 6] = [
    ObjectId(406),
    ObjectId(407),
    ObjectId(408),
    ObjectId(409),
    ObjectId(410),
    ObjectId(412),
];

// ===========================================================================
// Fixture construction
// ===========================================================================

/// Graft a Doubling-Season-shaped `CreateToken` count doubler onto a NEW permanent controlled by
/// `seat`. The one-object difference between every doubled and undoubled arm below.
fn graft_doubler(state: &mut GameState, seat: PlayerId) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let host = create_object(
        state,
        card_id,
        seat,
        "Doubling Season".to_string(),
        Zone::Battlefield,
    );
    // CR 614.1a: a token-count doubler is a replacement effect that modifies the number of tokens
    // created. `token_owner_scope(You)` is LOAD-BEARING and asserted below: the field is
    // `Option<ControllerRef>` whose builder default is `None`, and `None` means ANY owner — a
    // `None`-scoped doubler on P1 would match P0's creations and falsify the opponent-doubler
    // row's premise.
    let def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
        .token_owner_scope(ControllerRef::You)
        .quantity_modification(QuantityModification::DOUBLE);
    assert_eq!(
        def.token_owner_scope,
        Some(ControllerRef::You),
        "construction precondition: the doubler is scoped to ITS OWN controller's tokens"
    );
    {
        let obj = state
            .objects
            .get_mut(&host)
            .expect("the just-created doubler host is in `objects`");
        // BOTH vectors, or the layer reset drops the definition — the shape
        // `token_copy::copy_token_count_doubling_replacement_applies` builds.
        obj.base_replacement_definitions = std::sync::Arc::new(vec![def.clone()]);
        obj.replacement_definitions = vec![def].into();
    }
    host
}

/// Count the battlefield Saprolings `who` controls (tapped or not).
fn count_saprolings(state: &GameState, who: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == who && o.name == "Saproling")
        })
        .count()
}

/// The `CopiableValues` of P0's fodder class, read off a real board Saproling — the profile the
/// boundary mint copies. Used by the CONTENT assertion: the mint must reproduce the class, not
/// merely raise the object count.
fn fodder_profile(state: &GameState) -> CopiableValues {
    let saproling = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|o| o.controller == P0 && o.name == "Saproling")
        .expect("P0 controls a fodder Saproling");
    engine::game::printed_cards::intrinsic_copiable_values(saproling)
}

/// Everything P0's accepts have registered, in stash order.
fn registered(state: &GameState) -> &[PersistentAxisMaterialization] {
    state
        .pending_unbounded_materialization
        .get(&P0)
        .map_or(&[], Vec::as_slice)
}

/// The route the accept took, observed through the registered stash discriminant — the same
/// observable `loop_shortcut_cast_route`'s `route_of` uses, because `LoopCollapseRoute` is private
/// to `game::engine` and cannot be named from an external test crate.
fn assert_replay_route(state: &GameState, why: &str) {
    let stash = registered(state);
    assert!(
        !stash.is_empty(),
        "{why}: P0's accept registered NOTHING — any route claim after this is vacuous"
    );
    assert!(
        stash
            .iter()
            .all(|m| matches!(m, PersistentAxisMaterialization::DriveSequence { .. })),
        "{why}: expected the concrete replay, got {:?}",
        stash.iter().map(discriminant_name).collect::<Vec<_>>()
    );
}

fn assert_batched_route(state: &GameState, why: &str) {
    let stash = registered(state);
    assert!(
        !stash.is_empty(),
        "{why}: P0's accept registered NOTHING — any route claim after this is vacuous"
    );
    assert!(
        stash
            .iter()
            .all(|m| !matches!(m, PersistentAxisMaterialization::DriveSequence { .. })),
        "{why}: expected the batched route, got {:?}",
        stash.iter().map(discriminant_name).collect::<Vec<_>>()
    );
}

/// Exhaustive, no wildcard: a future persistent axis must be named here deliberately.
fn discriminant_name(m: &PersistentAxisMaterialization) -> &'static str {
    match m {
        PersistentAxisMaterialization::DriveSequence { .. } => "DriveSequence",
        PersistentAxisMaterialization::Tokens(_) => "Tokens",
        PersistentAxisMaterialization::Counters(_) => "Counters",
        PersistentAxisMaterialization::Life { .. } => "Life",
    }
}

/// The per-cycle token count the accept stashed, when it took the batched route.
fn stashed_per_cycle_delta(state: &GameState) -> Option<u32> {
    registered(state).iter().find_map(|m| match m {
        PersistentAxisMaterialization::Tokens(growth) => Some(growth.per_cycle_delta),
        _ => None,
    })
}

// ===========================================================================
// Driving
// ===========================================================================

/// Cast Sprout Swarm once, convoking `fodder`, and DECLINE any CR 732.2a offer the cast surfaces —
/// i.e. one REAL performed cycle. Returns the post-cycle state.
fn perform_one_cycle(state: GameState, fodder: ObjectId) -> GameState {
    let outcome = GameRunner::from_state(state)
        .cast(SPROUT)
        .accept_optional()
        .convoke_with(&[fodder])
        .commit()
        .resolve();
    let mut state = outcome.state().clone();
    if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
        apply(&mut state, P0, GameAction::DeclineShortcut)
            .expect("DeclineShortcut is legal at a LoopShortcut window");
    }
    state
}

/// The realistic board driven to its CR 732.2a offer by one real buyback+convoke recast.
fn offer_state(mut state: GameState) -> GameState {
    let outcome = drive_sprout_cast(state.clone());
    state = outcome.state().clone();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the live recast must surface P0's CR 732.2a offer, got {:?}",
        state.waiting_for
    );
    state
}

/// Proposer declares `Fixed(n)`; every living opponent accepts (APNAP).
fn declare_and_accept_all(state: &mut GameState, n: u32) {
    apply(
        state,
        P0,
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

/// Pass priority through the real production path until the CR 500.5 boundary raises the collapse
/// prompt. Bounded so a wedge fails loudly instead of hanging.
fn drive_to_collapse_boundary(state: &mut GameState) {
    for _ in 0..64 {
        if matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice {
                resource: PayableResource::LoopCollapse { .. },
                ..
            }
        ) {
            return;
        }
        let WaitingFor::Priority { player } = state.waiting_for.clone() else {
            panic!(
                "drive_to_collapse_boundary: unexpected non-Priority prompt {:?}",
                state.waiting_for
            )
        };
        apply(state, player, GameAction::PassPriority)
            .expect("pass priority toward the CR 500.5 boundary");
    }
    panic!("drive_to_collapse_boundary: no collapse prompt within 64 passes");
}

// ===========================================================================
// The matched route pair
// ===========================================================================

/// **The route conjunct is LIVE and NARROW**, written as ONE test so the two arms are
/// structurally inseparable: the doubled arm alone is satisfiable by a blanket route change, the
/// undoubled arm alone by never wiring the conjunct at all. UNDOUBLED (the discriminator): the
/// untouched shipped board reproduces ONE Saproling per cycle (`k == 1`), so the
/// `per_cycle_delta > 1` gate is OFF and the accept keeps the O(1) batched route — even though
/// this dump DOES carry a functioning ETB trigger, i.e. `token_growth_is_observed` is `true` on
/// it. That is what makes the k-gate load-bearing rather than decorative, and why every
/// currently-green `LoopShortcut` row keeps its route. DOUBLED: grafting ONE P0 doubler makes
/// the observed `k == 2`, and the accept switches to the concrete replay.
///
/// REVERT PROBES: delete the `token_growth_needs_replay` disjunct at the `game::engine` route
/// seam ⇒ the doubled arm falls back to `Tokens` and reds, undoubled stays green; delete the
/// `per_cycle_delta > 1` gate ⇒ the UNDOUBLED arm routes to `Replay` through the board's
/// ETB-trigger conjunct and reds, doubled stays green.
#[test]
fn doubled_board_routes_to_replay_undoubled_board_stays_batched() {
    // ── UNDOUBLED: the untouched shipped board, k == 1 ⇒ batched ──
    let mut undoubled = offer_state(load_realistic_dump());
    declare_and_accept_all(&mut undoubled, 100);
    assert_batched_route(
        &undoubled,
        "A-4: k == 1 keeps the shipped batched route (the k-gate is what makes this narrow)",
    );
    assert_eq!(
        stashed_per_cycle_delta(&undoubled),
        Some(1),
        "A-4: the undoubled board reproduces exactly one fodder member per cycle"
    );

    // ── DOUBLED: ONE grafted P0 doubler, k == 2 ⇒ concrete replay ──
    let mut doubled_board = load_realistic_dump();
    graft_doubler(&mut doubled_board, P0);
    let mut doubled = offer_state(doubled_board);
    declare_and_accept_all(&mut doubled, 100);
    assert_replay_route(
        &doubled,
        "A-3: a replacement-sourced k == 2 must NOT be batched — the mint would re-run the \
         doubler and apply it twice (#7045)",
    );
}

/// An OPPONENT's doubler does not move P0's route.
///
/// A `CreateToken` replacement matches by `token_owner_scope` (gated in PRODUCTION at
/// `game::replacement`'s floating and object paths, both `if let Some(ref scope) =
/// repl_def.token_owner_scope`), so a P1-controlled `Some(You)` doubler contributes NOTHING to
/// P0's observed `k`. P0's loop therefore stays at `k == 1` and keeps its batched route: the
/// over-fire this row exists to exclude does not happen.
///
/// REVERT PROBE: delete the `per_cycle_delta > 1` gate ⇒ the board-level
/// `token_growth_is_observed` sees the (irrelevant, opponent-scoped) `CreateToken` replacement ⇒
/// P0 routes to the cubic replay ⇒ RED.
#[test]
fn opponent_controlled_doubler_does_not_move_p0_route() {
    let mut board = load_realistic_dump();
    graft_doubler(&mut board, P1);
    let mut state = offer_state(board);
    declare_and_accept_all(&mut state, 100);
    assert_eq!(
        stashed_per_cycle_delta(&state),
        Some(1),
        "A-4b: an opponent-scoped doubler cannot multiply P0's own creations, so P0's observed \
         k stays 1"
    );
    assert_batched_route(
        &state,
        "A-4b: P0's currently-batching loop keeps the O(1) route when the doubler is an \
         opponent's",
    );
}

/// **Elision ≡ performance on a board whose `k` is REPLACEMENT-sourced.** Arm P: four REAL
/// cycles, declining each offer. Arm E: one real cycle, then accept and name N = 3 at the CR
/// 500.5 boundary. Both cover 1 + 3 cycles and must land on the same board. The DISCRIMINATING
/// QUANTITY is how many times the `Times{factor: 2}` replacement applies: once per cycle in both
/// arms. A batched arm would propose `per_cycle_delta * N = 6` and let the doubler multiply it to
/// 12 — the factor-of-k divergence the route conjunct prevents.
///
/// SEED CONFOUNDER: `seed_representative_fodder` runs on BOTH routes, but only when the period
/// taps a fodder and the board has NO tapped fodder yet. P0 already controls a TAPPED Saproling
/// here, asserted below as a named precondition, so it cannot fire on either arm.
///
/// REVERT PROBE: delete the `token_growth_needs_replay` disjunct at the `game::engine` route seam
/// ⇒ arm E takes the batched route, the mint proposes 2·3 = 6, the doubler re-applies, and arm E
/// lands 12 against arm P's 6 ⇒ RED by exactly a factor of k = 2.
#[test]
fn doubled_board_elision_equals_performance() {
    const N: u32 = 3;

    let mut board = load_realistic_dump();
    graft_doubler(&mut board, P0);

    // ── Named precondition: the seed's gate is FALSE on this board (411 is a TAPPED P0 fodder) ──
    let tapped_fodder = board
        .battlefield
        .iter()
        .filter_map(|id| board.objects.get(id))
        .filter(|o| o.controller == P0 && o.name == "Saproling" && o.tapped)
        .count();
    assert!(
        tapped_fodder > 0,
        "A-3 precondition: P0 already controls a TAPPED fodder Saproling, so \
         `seed_representative_fodder`'s `tapped_fodder_members(..).is_empty()` gate is FALSE and \
         the seed cannot fire on EITHER arm"
    );

    let start = count_saprolings(&board, P0);
    assert_eq!(start, 7, "fixture: P0 controls 7 Saprolings (406–412)");
    let class = fodder_profile(&board);

    // ── Arm P — four real performed cycles ──
    let mut performance = board.clone();
    for fodder in UNTAPPED_FODDER.iter().take(1 + N as usize) {
        performance = perform_one_cycle(performance, *fodder);
    }
    let performed = count_saprolings(&performance, P0);
    assert_eq!(
        performed - start,
        2 * (1 + N as usize),
        "arm P reach-guard: each REAL cycle creates one Saproling which the doubler makes two"
    );

    // ── Arm E — one real cycle, then the accepted collapse names N ──
    let mut elision = offer_state(board);
    let after_first = count_saprolings(&elision, P0);
    assert_eq!(
        after_first - start,
        2,
        "arm E reach-guard: the priming cycle is the SAME real cycle arm P performs"
    );
    declare_and_accept_all(&mut elision, N);
    assert_replay_route(
        &elision,
        "A-3: the replacement-sourced k == 2 period must take the concrete replay",
    );
    drive_to_collapse_boundary(&mut elision);
    let ids_before: Vec<ObjectId> = elision.battlefield.iter().copied().collect();
    apply(&mut elision, P0, GameAction::SubmitPayAmount { amount: N })
        .expect("P0 submits the finite loop-collapse count");
    let elided = count_saprolings(&elision, P0);

    // ── THE FIDELITY ASSERTION ──
    assert_eq!(
        elided, performed,
        "#7045: elision must equal performance. Arm P (four real cycles) = {performed}, arm E \
         (one real cycle + a collapse of N = {N}) = {elided}. A factor-of-2 excess here means the \
         doubler was applied to a count that already included it."
    );
    assert_eq!(
        elided - after_first,
        2 * N as usize,
        "the collapse of N = {N} cycles produced 2N Saprolings — the replacement applied ONCE per \
         cycle, not once per cycle AND once on the lump"
    );

    // ── CONTENT: every object the collapse produced IS the fodder class ──
    let produced: Vec<ObjectId> = elision
        .battlefield
        .iter()
        .copied()
        .filter(|id| !ids_before.contains(id))
        .collect();
    assert_eq!(
        produced.len(),
        2 * N as usize,
        "the collapse produced exactly 2N new battlefield objects"
    );
    for id in produced {
        let obj = elision
            .objects
            .get(&id)
            .expect("produced object is present");
        assert_eq!(
            obj.name, class.name,
            "produced object carries the class name"
        );
        assert_eq!(
            obj.card_types, class.card_types,
            "produced object carries the class card types (CR 707.2) — a count-only \
             implementation that raised the tally with unrelated objects fails here"
        );
    }
}

// ===========================================================================
// The arithmetic pin
// ===========================================================================

/// The batched mint really multiplies by the STASHED delta. A matched pair whose two
/// arms are byte-identical apart from one integer: same variant, same profile, same N.
///
/// Registered through the shipped `register_pending_materialization` writer, exactly as
/// `combo_infinite_pile` registers `Counters`/`Life` deltas.
///
/// REVERT PROBE: change the mint (`game::engine_resolution_choices`' `Tokens` arm) back to
/// `count: amount` ⇒ the `delta: 2` arm mints 3 instead of 6 ⇒ RED. The `delta: 1` arm is the
/// reach-guard and stays GREEN under that revert, which proves the row discriminates on the delta
/// and not on the harness.
#[test]
fn batched_mint_multiplies_by_the_stashed_per_cycle_delta() {
    const N: u32 = 3;

    let minted_with = |per_cycle_delta: u32| -> usize {
        let mut state = offer_state(load_realistic_dump());
        let profile = Box::new(fodder_profile(&state));
        declare_and_accept_all(&mut state, 100);
        // Overwrite the accept's own stash with the arm's delta — the ONE field under test.
        state.pending_unbounded_materialization.remove(&P0);
        state.register_pending_materialization(
            P0,
            PersistentAxisMaterialization::Tokens(Box::new(TokenGrowth {
                profile,
                per_cycle_delta,
            })),
        );
        drive_to_collapse_boundary(&mut state);
        let before = count_saprolings(&state, P0);
        apply(&mut state, P0, GameAction::SubmitPayAmount { amount: N })
            .expect("P0 submits the finite loop-collapse count");
        count_saprolings(&state, P0) - before
    };

    assert_eq!(
        minted_with(1),
        N as usize,
        "A-3b reach-guard: per_cycle_delta 1 mints N"
    );
    assert_eq!(
        minted_with(2),
        2 * N as usize,
        "A-3b: per_cycle_delta 2 mints 2N — the mint reads the stashed delta"
    );
}

// ===========================================================================
// The mint produces real ENTRIES, not merely real objects
// ===========================================================================

/// **The boundary mint's `k·N` tokens raise `k·N` REAL CR 603.6a battlefield-entry events**,
/// measured by an entry-triggered observable rather than by object existence: an implementation
/// that inserts objects without raising entry events satisfies "2N distinct `ObjectId`s exist"
/// while STRANDING every entry-sourced mechanism downstream. So the trigger delta is the primary
/// assertion and the object count the secondary reach-guard.
///
/// A board carrying a broad matching ETB observer at OFFER time does not offer at all
/// (`sprout_broad_matching_observer_still_vetoes_offer`), so the observer is grafted AFTER the
/// accept: this row measures the MINT, not the offer firewall, and the "observer present from the
/// start" form is not covered here. The stash is registered directly to force the BATCHED route,
/// since a natural `k > 1` board takes the replay where entries are trivially real. REVERT PROBE:
/// make the mint insert objects without routing through `game::effects::token_copy`'s
/// `ProposedEvent::CreateToken` pipeline ⇒ no entry events ⇒ the trigger delta reads 0 ⇒ RED,
/// while the secondary object-count guard would still pass.
#[test]
fn batched_mint_raises_real_entry_events_per_minted_token() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
    };
    use engine::types::triggers::TriggerMode;

    const N: u32 = 3;
    const DELTA: u32 = 2;

    let mut state = offer_state(load_realistic_dump());
    let profile = Box::new(fodder_profile(&state));
    declare_and_accept_all(&mut state, 100);
    state.pending_unbounded_materialization.remove(&P0);
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Tokens(Box::new(TokenGrowth {
            profile,
            per_cycle_delta: DELTA,
        })),
    );

    // The entry-sourced observable, grafted AFTER the accept (see the firewall note above): an
    // Altar-of-the-Brood-shaped "whenever a permanent enters, mill one" on a P0 permanent.
    let card_id = CardId(state.next_object_id);
    let observer = create_object(
        &mut state,
        card_id,
        P0,
        "Entry Observer".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&observer)
        .expect("the just-created observer host is in `objects`")
        .push_printed_trigger(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Mill {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                        destination: Zone::Graveyard,
                    },
                )),
        );

    drive_to_collapse_boundary(&mut state);
    let stack_before = state.stack.len();
    let saprolings_before = count_saprolings(&state, P0);
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: N })
        .expect("P0 submits the finite loop-collapse count");

    // PRIMARY: one real CR 603.6a entry event per minted token, each firing the observer.
    assert_eq!(
        state.stack.len() - stack_before,
        (DELTA * N) as usize,
        "W3.17: the mint must raise {} REAL battlefield entries — an implementation that inserts \
         objects without entry events reads 0 here and strands every entry-sourced mechanism",
        DELTA * N
    );
    // SECONDARY reach-guard (never the primary): the objects exist too.
    assert_eq!(
        count_saprolings(&state, P0) - saprolings_before,
        (DELTA * N) as usize,
        "reach-guard: the mint produced delta*N tokens"
    );
}
