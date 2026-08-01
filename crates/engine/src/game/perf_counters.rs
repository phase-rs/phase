use std::cell::Cell;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PerfCounterSnapshot {
    pub state_clone_for_legality: u64,
    pub static_full_scans: u64,
    pub spell_keyword_grant_scans: u64,
    pub layers_full_eval: u64,
    pub layers_incremental: u64,
    pub layers_escalated: u64,
    pub mana_display_sweeps: u64,
    pub mana_display_swept_objects: u64,
    pub stack_batch_candidates: u64,
    pub stack_batch_plans: u64,
    pub stack_batch_observer_refusals: u64,
    pub stack_batched_entries: u64,
    pub stack_inert_noop_batches: u64,
    pub stack_inert_noop_entries: u64,
    pub legal_actions_spell_cost_sweeps: u64,
    pub priority_cast_probe_builds: u64,
    pub auto_tap_source_cache_builds: u64,
    pub cached_auto_tap_source_reuses: u64,
    pub cached_auto_tap_source_rejects: u64,
    pub mana_aura_trigger_scans: u64,
    pub crew_eligibility_scans: u64,
    pub attackable_player_sweeps: u64,
    pub combat_shadow_block_scans: u64,
    pub granted_ability_provider_scans: u64,
    pub restriction_static_exact_scans: u64,
    pub restriction_static_mode_gate_scans: u64,
    pub legend_rule_mode_gate_scans: u64,
    pub sba_battlefield_snapshot_builds: u64,
    pub sba_empty_battlefield_short_circuits: u64,
}

thread_local! {
    /// Per-thread (NOT process-global) so parallel `cargo test` runs do not
    /// cross-pollute counters between a test's `reset()` and `snapshot()`.
    ///
    /// The counted legality / delve / cost paths all run entirely on the
    /// calling thread (no rayon or spawned threads), so a thread-local sees
    /// exactly the clones its own code performs — preserving the #3663
    /// per-candidate-clone regression guards. The only consumers are these
    /// engine unit tests plus the single-threaded `legal_actions_bench` and
    /// `resolve_bench` dev binaries; there is NO production or CI telemetry
    /// that needs a cross-thread aggregate. Do not "fix" this back to a global
    /// `AtomicU64`: that reintroduces the parallel-test flakiness this replaces.
    static COUNTERS: Cell<PerfCounterSnapshot> = const { Cell::new(PerfCounterSnapshot {
        state_clone_for_legality: 0,
        static_full_scans: 0,
        spell_keyword_grant_scans: 0,
        layers_full_eval: 0,
        layers_incremental: 0,
        layers_escalated: 0,
        mana_display_sweeps: 0,
        mana_display_swept_objects: 0,
        stack_batch_candidates: 0,
        stack_batch_plans: 0,
        stack_batch_observer_refusals: 0,
        stack_batched_entries: 0,
        stack_inert_noop_batches: 0,
        stack_inert_noop_entries: 0,
        legal_actions_spell_cost_sweeps: 0,
        priority_cast_probe_builds: 0,
        auto_tap_source_cache_builds: 0,
        cached_auto_tap_source_reuses: 0,
        cached_auto_tap_source_rejects: 0,
        mana_aura_trigger_scans: 0,
        crew_eligibility_scans: 0,
        attackable_player_sweeps: 0,
        combat_shadow_block_scans: 0,
        granted_ability_provider_scans: 0,
        restriction_static_exact_scans: 0,
        restriction_static_mode_gate_scans: 0,
        legend_rule_mode_gate_scans: 0,
        sba_battlefield_snapshot_builds: 0,
        sba_empty_battlefield_short_circuits: 0,
    }) };
}

fn with_mut(f: impl FnOnce(&mut PerfCounterSnapshot)) {
    COUNTERS.with(|c| {
        let mut s = c.get();
        f(&mut s);
        c.set(s);
    });
}

pub fn record_state_clone_for_legality() {
    with_mut(|s| s.state_clone_for_legality += 1);
}

/// Counts every whole-battlefield / command-zone static sweep done for legality
/// (each `check_static_ability` call). Combat/untap legality loops hoist a
/// once-computed existence gate to drive this toward zero, collapsing O(N^2)
/// per-loop scans to O(N).
///
/// Also incremented at the two hexproof scan gates in `static_abilities`
/// (`player_ignores_hexproof`, `target_ignores_hexproof`), which each guard their
/// O(battlefield) `.any()` behind the O(1) `static_kind_present(IgnoreHexproof)`
/// presence index — so on a board with zero functioning `IgnoreHexproof` statics this
/// counter stays at 0 across an entire target enumeration.
pub fn record_static_full_scan() {
    with_mut(|s| s.static_full_scans += 1);
}

/// Counts full `game_active_statics` scans for `CastWithKeyword` spell grants.
/// The O(1) `static_kind_present(CastWithKeyword)` gate should keep this at zero
/// during candidate generation when no functioning spell-keyword grant exists.
pub fn record_spell_keyword_grant_scan() {
    with_mut(|s| s.spell_keyword_grant_scans += 1);
}

/// Counts every full-body execution of `blocker_can_block_shadow` (each a
/// whole-battlefield `check_static_ability(CanBlockShadow)` sweep). The combat
/// block-legality loops hoist a once-computed `CanBlockShadow` existence gate to
/// drive this toward zero, collapsing the O(attackers×blockers×N) per-blocker
/// scan to O(N) when no such static exists.
pub fn record_combat_shadow_block_scan() {
    with_mut(|s| s.combat_shadow_block_scans += 1);
}

/// Counts every per-provider `matches_target_filter` evaluation done while
/// populating the per-controller provider cache in
/// `expand_granted_activated_abilities`. Memoizing the matching-provider set by
/// recipient controller collapses the O(recipients×objects) filter sweep to
/// O(controllers×objects).
pub fn record_granted_ability_provider_scan() {
    with_mut(|s| s.granted_ability_provider_scans += 1);
}

/// Counts the two exact restriction scans that walk
/// `battlefield_active_statics`: activation-limit modifiers and
/// activate-as-instant permissions. Their callers gate by mode first, so absent
/// modes should leave this at zero.
pub fn record_restriction_static_exact_scan() {
    with_mut(|s| s.restriction_static_exact_scans += 1);
}

/// Counts once-computed activation-restriction mode gates. Board-wide legal
/// action production should compute this once and thread it through every
/// candidate; direct activation legality computes it locally for the one call.
pub fn record_restriction_static_mode_gate_scan() {
    with_mut(|s| s.restriction_static_mode_gate_scans += 1);
}

/// Counts legend-rule mode gate computations. The SBA legend-rule pass should
/// compute this once before testing every legendary permanent.
pub fn record_legend_rule_mode_gate_scan() {
    with_mut(|s| s.legend_rule_mode_gate_scans += 1);
}

/// Counts one shared battlefield snapshot built for each SBA fixpoint iteration.
pub fn record_sba_battlefield_snapshot_build() {
    with_mut(|s| s.sba_battlefield_snapshot_builds += 1);
}

/// Counts SBA fixpoint iterations where an empty battlefield lets battlefield-only
/// SBAs short-circuit while nonbattlefield SBAs still run.
pub fn record_sba_empty_battlefield_short_circuit() {
    with_mut(|s| s.sba_empty_battlefield_short_circuits += 1);
}

pub fn record_layers_full_eval() {
    with_mut(|s| s.layers_full_eval += 1);
}

pub fn record_layers_incremental() {
    with_mut(|s| s.layers_incremental += 1);
}

pub fn record_layers_escalated() {
    with_mut(|s| s.layers_escalated += 1);
}

pub fn record_mana_display_sweep(swept_objects: usize) {
    with_mut(|s| {
        s.mana_display_sweeps += 1;
        s.mana_display_swept_objects += swept_objects as u64;
    });
}

pub fn record_stack_batch_candidate() {
    with_mut(|s| s.stack_batch_candidates += 1);
}

pub fn record_stack_batch_plan() {
    with_mut(|s| s.stack_batch_plans += 1);
}

pub fn record_stack_batch_observer_refusal() {
    with_mut(|s| s.stack_batch_observer_refusals += 1);
}

pub fn record_stack_batched_entries(entries: u32) {
    with_mut(|s| s.stack_batched_entries += u64::from(entries));
}

pub fn record_stack_inert_noop_batch(entries: u32) {
    with_mut(|s| {
        s.stack_inert_noop_batches += 1;
        s.stack_inert_noop_entries += u64::from(entries);
    });
}

pub fn record_legal_actions_spell_cost_sweep() {
    with_mut(|s| s.legal_actions_spell_cost_sweeps += 1);
}

pub fn record_priority_cast_probe_build() {
    with_mut(|s| s.priority_cast_probe_builds += 1);
}

pub fn record_auto_tap_source_cache_build() {
    with_mut(|s| s.auto_tap_source_cache_builds += 1);
}

pub fn record_cached_auto_tap_source_reuse() {
    with_mut(|s| s.cached_auto_tap_source_reuses += 1);
}

pub fn record_cached_auto_tap_source_reject() {
    with_mut(|s| s.cached_auto_tap_source_rejects += 1);
}

pub fn record_mana_aura_trigger_scan() {
    with_mut(|s| s.mana_aura_trigger_scans += 1);
}

pub fn record_crew_eligibility_scan() {
    with_mut(|s| s.crew_eligibility_scans += 1);
}

pub fn record_attackable_player_sweep() {
    with_mut(|s| s.attackable_player_sweeps += 1);
}

pub fn snapshot() -> PerfCounterSnapshot {
    COUNTERS.with(|c| c.get())
}

pub fn reset() {
    COUNTERS.with(|c| c.set(PerfCounterSnapshot::default()));
}
