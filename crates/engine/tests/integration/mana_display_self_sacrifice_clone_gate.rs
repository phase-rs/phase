//! Mana-display sweep clone gate for **self-sacrifice** mana sources (engine tier).
//!
//! Permanent regression guard proving that a board of Treasure-class sources —
//! `{T}, Sacrifice this token: Add one mana of any color` (CR 111.10a) — answers
//! the per-source mana-display legality question **without cloning `GameState`**.
//! Before the fix each such source fell through
//! `can_activate_mana_ability_now_gated`'s cheap gate into
//! `can_activate_mana_ability_by_simulation`, which clones the whole state, so a
//! 193-Treasure board took 193 full-state clones per display sweep.
//!
//! Every test runs at **N and 2N** so the gate proves *non-scaling* rather than one
//! lucky value, and each carries a `has_mana_ability` reach-guard: a `0` clone
//! count is vacuous on a board that was never swept.
//!
//! `mana_display_sweep_still_clones_for_non_self_sacrifice_sources` is the
//! positive control — it holds every other axis fixed and changes only the
//! `Sacrifice` target, which is the discriminator the fix keys on.
//! `mana_display_sweep_declines_multi_leaf_self_sacrifice_costs` is the ANSWER
//! guard: it varies the cost's leaf ARITY instead, and pins that a shape the fast
//! path cannot decide is still handed to the simulation.
//!
//! DB-free by construction: `GameState::new_two_player` + `create_object` only,
//! never loading `client/public/card-data.json` (mirrors `token_storm_scaling_gate.rs`;
//! `scripts/check-test-card-data-load.sh` guards this). Under `cargo nextest` each
//! test runs in its own process, so the `thread_local!` perf counters cannot bleed
//! across tests and the exact `== 0` assertion is sound.

use engine::game::derived::derive_display_state;
use engine::game::layers::flush_layers;
use engine::game::perf_counters;
use engine::game::public_state::mark_mana_display_dirty;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    QuantityExpr, SacrificeCost, TargetFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Board size. `2 * N` is the paired size that turns "0 clones" from a lucky
/// value into a non-scaling claim.
const N: usize = 16;

/// `make_any_color_treasure` in `mana_abilities.rs` is `#[cfg(test)]`
/// crate-private, so an integration test must define its own equivalent — the
/// same idiom `token_storm_scaling_gate.rs` uses for a private targeting helper.
///
/// `cost` is the only axis that varies across the tests: `{T}` + a `SelfRef`
/// sacrifice is the shape the fast path decides, `{T}` + a `Typed(permanent)`
/// sacrifice is the shape it must decline on its target, and a multi-leaf tree is
/// the shape it must decline on its arity.
fn spawn_mana_source(
    state: &mut GameState,
    card: u64,
    player: PlayerId,
    cost: AbilityCost,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        player,
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    // CR 111.10a: a Treasure token is an *artifact*. Both boards carry the core
    // type so that a `TargetFilter::Typed(permanent)` sacrifice target finds the
    // source itself as a legal victim — without it the readiness gate rejects the
    // source and the sweep never reaches the decision seam at all. Setting it on
    // BOTH boards keeps the two tests differing only by the sacrifice target,
    // which is the discriminator the fix keys on.
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Artifact);
    let def = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: ManaColor::ALL.to_vec(),
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(cost);
    std::sync::Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def);
    id
}

/// `count` sources sharing one `sacrifice_target`, already flushed so the O(1)
/// `StaticModePresence` index is precise.
///
/// **The flush is load-bearing.** A fresh `GameState` seeds
/// `StaticModePresence::all_present()`, so the fast path's `CantPayCost` presence
/// guard declines everything until the layers pipeline has run — an unflushed
/// board measures an inert fast path. Production always flushes first
/// (`public_state::finalize_rules_state` -> `finalize_display_state`), so this
/// mirrors production rather than working around the guard.
fn mana_source_board(count: usize, cost: AbilityCost) -> (GameState, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(42);
    let ids = (0..count)
        .map(|i| spawn_mana_source(&mut state, 9500 + i as u64, PlayerId(0), cost.clone()))
        .collect();
    flush_layers(&mut state);
    (state, ids)
}

/// The Treasure-shaped board (`{T}`, `Sacrifice <target>`) both original tests
/// use, differing only in the sacrifice target.
fn sacrifice_source_board(
    count: usize,
    sacrifice_target: TargetFilter,
) -> (GameState, Vec<ObjectId>) {
    mana_source_board(
        count,
        AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(sacrifice_target, 1)),
            ],
        },
    )
}

/// Run one board-wide mana-display sweep and return its counters.
fn sweep(state: &mut GameState) -> perf_counters::PerfCounterSnapshot {
    mark_mana_display_dirty(state);
    perf_counters::reset();
    derive_display_state(state);
    perf_counters::snapshot()
}

/// CR 111.10a + CR 701.21a: the board-wide mana-display sweep takes **zero**
/// legality clones over self-sacrifice sources, at both N and 2N.
///
/// REVERT-PROBE: drop the `has_unambiguous_self_sacrifice_component` disjunct
/// from `mana_abilities::legality_simulation_is_redundant` and this reports N
/// (then 2N) clones — the pre-fix clone storm.
#[test]
fn mana_display_sweep_is_clone_free_for_self_sacrifice_sources() {
    for count in [N, 2 * N] {
        let (mut state, ids) = sacrifice_source_board(count, TargetFilter::SelfRef);
        let snap = sweep(&mut state);

        assert_eq!(
            snap.mana_display_sweeps, 1,
            "count={count}: exactly one board-wide mana sweep"
        );
        assert_eq!(
            snap.mana_display_swept_objects, count as u64,
            "count={count}: the sweep visited every battlefield object"
        );
        // Reach-guard: `0` clones is vacuous unless the sources were really
        // classified as activatable mana sources by that sweep.
        for id in &ids {
            assert!(
                state.objects.get(id).unwrap().has_mana_ability,
                "count={count}: every self-sacrifice source must report an \
                 available mana ability, or the 0-clone count below is vacuous"
            );
        }
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "count={count}: a self-sacrifice mana cost is conclusively payable \
             without simulating (revert-failing: pre-fix = {count} clones)"
        );
    }
}

/// The positive control for the gate above, and the class boundary at sweep
/// scale. Only the `Sacrifice` target changes: a `Typed(permanent)` target may
/// have no legal victim in general, so its simulation is load-bearing and the
/// fast path must decline. Each source is its own legal victim here, so
/// readiness still passes and the decision seam is genuinely reached.
///
/// Without this test, `mana_display_sweep_is_clone_free_for_self_sacrifice_sources`
/// could pass on a sweep that never takes clones at all.
#[test]
fn mana_display_sweep_still_clones_for_non_self_sacrifice_sources() {
    for count in [N, 2 * N] {
        let (mut state, ids) =
            sacrifice_source_board(count, TargetFilter::Typed(TypedFilter::permanent()));
        let snap = sweep(&mut state);

        assert_eq!(
            snap.mana_display_sweeps, 1,
            "count={count}: exactly one board-wide mana sweep"
        );
        for id in &ids {
            assert!(
                state.objects.get(id).unwrap().has_mana_ability,
                "count={count}: each source is its own legal victim, so it is \
                 activatable — this is the reach-guard for the clone count below"
            );
        }
        assert!(
            snap.state_clone_for_legality >= count as u64,
            "count={count}: a non-self Sacrifice target must still simulate per \
             source (got {} clones)",
            snap.state_clone_for_legality
        );
    }
}

/// CR 118.3: the ANSWER guard for the classifier's leaf-arity bound, on the
/// production path and at sweep scale.
///
/// `Composite[{T}, {T}, Sacrifice this]` is whole-tree choice-free and does carry
/// a single self-sacrifice leaf, so an unbounded classifier accepts it and the
/// fast path answers `true`. The payment path disagrees: it flattens the tree and
/// pays one leaf at a time, so the second `{T}` finds the source already tapped,
/// `tap_source` errors, and the legality simulation answers `false`. Skipping a
/// simulation may only ever save work — never change the answer — so this shape
/// must stay on the simulating path.
///
/// REVERT-PROBE: drop the `<= 1` tap term from
/// `mana_sources::has_unambiguous_self_sacrifice_component` and every source here
/// flips to `has_mana_ability == true` with `0` legality clones, failing both
/// assertions below. Dropping the `== 1` self-sacrifice term instead is caught by
/// the same shape with two `Sacrifice` leaves, pinned as a unit row in
/// `mana_sources::tests::self_sacrifice_classifier_bounds_leaf_arity`.
#[test]
fn mana_display_sweep_declines_multi_leaf_self_sacrifice_costs() {
    for count in [N, 2 * N] {
        let (mut state, ids) = mana_source_board(
            count,
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            },
        );
        let snap = sweep(&mut state);

        assert_eq!(
            snap.mana_display_sweeps, 1,
            "count={count}: exactly one board-wide mana sweep"
        );
        // The ANSWER: the second {T} leaf cannot be paid (CR 118.3), so the
        // ability is not activatable and the sweep must report it as such.
        for id in &ids {
            assert!(
                !state.objects.get(id).unwrap().has_mana_ability,
                "count={count}: a two-{{T}} cost is unpayable, so the sweep must \
                 answer `false` exactly as the legality simulation does"
            );
        }
        // Non-vacuity pair for the negative above: one clone per source proves
        // readiness PASSED and each source really reached the legality seam, so
        // the `false` came from the simulation rather than from an upstream
        // short-circuit that would make the assertion meaningless.
        assert!(
            snap.state_clone_for_legality >= count as u64,
            "count={count}: each source must reach the legality simulation \
             (got {} clones) — otherwise the negative assertion above is vacuous",
            snap.state_clone_for_legality
        );
    }
}
