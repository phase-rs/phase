//! CR 611.2 + CR 613.1: `StaticSourceIndex` — candidate pre-filter for
//! `for_each_static_effect_source` (`layers.rs`). Replaces the full
//! `state.battlefield` (~thousands of permanents on a token-swarm board) +
//! `state.command_zone` scan with iteration over the handful of objects that
//! actually GENERATE ≥1 continuous effect, so the per-flush layer gather scales
//! with the number of generators, not with `|battlefield|`.
//!
//! # Correctness model
//!
//! The index keys on **GENERATORS** (objects whose `static_definitions` carry a
//! `StaticMode::Continuous` def, including `GrantStaticAbility` hosts), NOT on
//! recipients. A granted anthem's per-recipient fan-out happens inside the
//! host's own visit (`expand_granted_static_effects`), so iterating hosts
//! reproduces every granted effect; recipients need not be indexed. The
//! classification predicate is **condition-independent and zone-independent** —
//! it asks only "does this object carry a continuous static def?", never "does
//! that static currently pass its gate?". The per-def condition / zone-of-
//! function gating still runs inside the gather, so the index **over-includes,
//! never under-includes** (an over-included object that currently sources
//! nothing produces one wasted, empty visit). Output is byte-identical.
//!
//! # Authority — TOP-of-pass rebuild (placement differs from `TriggerIndex`)
//!
//! `TriggerIndex` is consulted by EXTERNAL event scans that run AFTER the layer
//! pass completes, so its rebuild is correctly placed at the END of the pass.
//! `StaticSourceIndex` is consulted INSIDE the layer pass (the Copy / main
//! gathers, per-layer ordering, the escalation probe, `refresh_static_gate_truth`),
//! so an end-of-pass rebuild would leave those mid-pass consults reading the
//! PREVIOUS pass's index — stale for any generator that entered since. The
//! rebuild therefore runs at the TOP of `evaluate_layers` /
//! `apply_layers_incremental`, AFTER the Step-1 base reset (so the predicate
//! reads base, not stale post-layer, definitions) and BEFORE the first gather.
//! Layer-6 grant products (recipient-becomes-generator) are caught by the NEXT
//! pass's top-of-pass rebuild via the `layers_dirty` fixpoint — the installing
//! pass marks dirty, the same mechanism `TriggerIndex` uses for granted triggers.
//!
//! # `layers_dirty` as a validity key — sound for THIS index
//!
//! The index stores the condition-INDEPENDENT generator set, which changes only
//! when some object's `static_definitions` changes. Every such change for the
//! two indexed buckets marks `layers_dirty` (battlefield enter/leave →
//! `zones.rs`/`change_zone.rs`; command-zone emblem creation →
//! `create_emblem.rs` `mark_layers_full`), and every production read of derived
//! state crosses `flush_layers` (hence a top-of-pass rebuild) first. So external
//! inter-flush callers always observe a fresh index for the indexed buckets.
//! The off-zone / opt-in-zone arm is NOT indexed (its generator-set changes are
//! not all marked `layers_dirty` — a self-milled Incarnation enters the
//! graveyard without a dirty mark) and keeps its live `state.objects` scan in
//! `for_each_static_effect_source`.

use crate::types::game_state::{GameState, StaticSourceIndex};
use crate::types::statics::StaticMode;

use super::game_object::GameObject;

/// CR 611.2 + CR 613.1: An object generates ≥1 continuous effect iff any of its
/// `static_definitions` is `StaticMode::Continuous`. This INCLUDES
/// `GrantStaticAbility` hosts — the grant modification lives on the host's own
/// continuous static, and the per-recipient fan-out happens inside
/// `expand_granted_static_effects` when the host is visited.
///
/// Condition-INDEPENDENT and zone-INDEPENDENT: asks only "does this object carry
/// a continuous static def?", not "does that static currently pass its gate?".
/// That is the soundness foundation — the index over-includes (condition-failing
/// statics) but never under-includes a real source.
pub(crate) fn object_sources_continuous_effect(obj: &GameObject) -> bool {
    obj.static_definitions
        .iter_all()
        .any(|def| def.mode == StaticMode::Continuous)
}

impl StaticSourceIndex {
    /// CR 611.2 + CR 613.1: Rebuild the index from the current battlefield +
    /// command zone. Scans `state.battlefield` in order (including phased-out
    /// objects — they're skipped at consult) and `state.command_zone` in order,
    /// pushing generators in that order so the visit order is byte-identical to
    /// the previous full `state.battlefield` / `state.command_zone` scan filtered
    /// to generators.
    ///
    /// O(battlefield) — runs once per full eval / incremental flush at the TOP
    /// of the pass (the same O(battlefield) cost the Step-1 reset and
    /// `TriggerIndex::rebuild_from_battlefield` already pay per flush). The
    /// savings are on the many repeat `for_each_static_effect_source` consults
    /// per flush, each of which was O(battlefield) and is now O(generators).
    ///
    /// Indexes ONLY the two `layers_dirty`-covered buckets; the off-zone /
    /// opt-in-zone arm is live-scanned in `for_each_static_effect_source` and is
    /// not rebuilt here.
    pub fn rebuild_from_state(state: &mut GameState) {
        let mut fresh = StaticSourceIndex::default();
        for &id in &state.battlefield {
            // Include phased-out objects; the consult-time `is_phased_out()`
            // skip excludes them (CR 702.26e). Phase-in/out does not change
            // `static_definitions`, so it needs no index mutation.
            if let Some(obj) = state.objects.get(&id) {
                if object_sources_continuous_effect(obj) {
                    fresh.battlefield_sources.push_back(id);
                }
            }
        }
        for &id in &state.command_zone {
            // CR 114.3: command-zone emblems carry static abilities. CR 905.4 +
            // CR 113.6b: a face-up conspiracy's abilities function from the
            // command zone too. CR 311.2 / CR 312.2: an active plane / phenomenon
            // remains in and functions from the command zone, contributing any
            // static that opts in via `active_zones.contains(Command)`. All route
            // through the single admission authority; commanders (is_emblem ==
            // false, not a conspiracy, no opt-in static) are not indexed.
            if let Some(obj) = state.objects.get(&id) {
                let is_command_zone_source =
                    super::functioning_abilities::object_sources_static_from_command_zone(obj);
                if is_command_zone_source && object_sources_continuous_effect(obj) {
                    fresh.command_sources.push_back(id);
                }
            }
        }
        // NO off-zone loop: that arm is not indexed (a self-milled Incarnation
        // enters the graveyard without marking `layers_dirty`, which would stale
        // a cached bucket) — `for_each_static_effect_source` live-scans it.
        state.static_source_index = fresh;
    }
}
