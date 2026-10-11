use std::cell::Cell;
#[cfg(feature = "test-support")]
use std::cell::RefCell;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PerfCounterSnapshot {
    pub state_clone_for_legality: u64,
    pub generation_state_clones: u64,
    pub strict_fast_path_state_clones: u64,
    pub strict_fast_path_mana_readiness_state_clones: u64,
    pub raw_validation_state_clones: u64,
    pub grouped_mana_readiness_state_clones: u64,
    pub post_apply_auto_payment_core_state_clones: u64,
    pub priority_cast_probe_state_clones: u64,
    pub auto_payment_borrowed_wrapper_calls: u64,
    pub auto_payment_owned_state_clones: u64,
    pub generation_auto_payment_wrapper_calls: u64,
    pub strict_fast_path_auto_payment_wrapper_calls: u64,
    pub post_apply_auto_payment_core_calls: u64,
    pub post_apply_uncached_source_collections: u64,
    pub static_full_scans: u64,
    pub spell_keyword_grant_scans: u64,
    pub layers_full_eval: u64,
    pub layers_incremental: u64,
    pub layers_escalated: u64,
    pub mana_display_sweeps: u64,
    pub mana_display_swept_objects: u64,
    /// CR 602.5 + CR 118.3: how many times the shared activation-legality core
    /// (`casting::activation_verdict`) ran, across BOTH the enforcement shim and
    /// the display read-out. Pins "one core evaluation per examined ability".
    pub activation_verdict_passes: u64,
    /// CR 118.3: activated abilities examined by
    /// `ai_support::activation_block_reasons`. Paired with the counter above so
    /// a passes-per-ability ratio is attributable rather than coincidental.
    pub activation_block_display_abilities_examined: u64,
    /// CR 613.1: whole-state flush clones taken by `casting::activation_verdict`'s
    /// target-legality tail, which now clones only when `layers_dirty` is dirty
    /// (it cloned unconditionally before, and incremented no counter at all).
    ///
    /// Deliberately its OWN field rather than `state_clone_for_legality`. That
    /// field is a per-candidate legality-clone budget consumed by shipped memo
    /// tests in `ai_support::filter`; folding a previously-uncounted clone into
    /// it would silently double their expected budgets and turn a pure
    /// instrumentation change into a behavioural-looking regression. Counting it
    /// separately keeps both numbers attributable.
    pub activation_verdict_flush_clones: u64,
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

/// Test-only cache-use counters for the homogeneous target-selection walk.
///
/// Kept separate from [`PerfCounterSnapshot`], whose serialized field set powers
/// the AI performance baseline rather than integration-test instrumentation.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HomogeneousTargetWalkCacheCounters {
    pub initializations: u64,
    pub advances: u64,
}

/// Test-only counters for the prior-target-binding seams (cross-slot
/// object-relative target binding, e.g. Puca's Mischief / Spawnbroker /
/// Daring Thief). Kept out of [`PerfCounterSnapshot`] for the same reason the
/// homogeneous-walk counters are: that struct's serialized field set powers
/// the AI performance baseline.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PriorTargetBindingCounters {
    pub static_union_enumerations: u64,
    pub selection_bindings: u64,
}

/// Test-only counters for the CR 601.2f + CR 602.2b activation cost-determination
/// routes. They pin that a target-first activation's mana leg is priced at the
/// settlement write-back, and that the separate "is there mana left to pay?"
/// decision (`try_finalize_pending_activation_mana_leg`'s zero-leg skip) is a
/// distinct site. A change to one can't silently move into the other. Kept out
/// of [`PerfCounterSnapshot`] for the same reason as the counter sets above: that
/// struct's serialized field set powers the AI performance baseline.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActivationCostRouteCounters {
    /// Times the settlement write-back chose which carrier holds the unpaid mana.
    pub settlement_writebacks: u64,
    /// Times the mana-leg finalizer found a zero leg and skipped payment.
    pub zero_mana_leg_skips: u64,
    /// Times `settle_activation_cost` ran, whatever its carrier.
    pub settlements: u64,
    /// Of those, the ones that found an `Open` carrier (a deferred lock).
    pub open_settlements: u64,
    /// Of those, the ones that found no carrier at all.
    pub carrierless_settlements: u64,
    /// Times pre-activation feasibility had to walk target assignments because
    /// the target-free bounds didn't decide it.
    pub window_searches: u64,
    /// Reaches of each unlocked-cost guard, indexed by `ActivationCostGuardSite`.
    pub guard_reaches: [u64; crate::game::casting::ActivationCostGuardSite::COUNT],
}

/// Test-only count of work the target-completion walk actually PERFORMED, per
/// [`WalkOp`](crate::game::ability_utils::WalkOp), recorded inside each
/// operation rather than at its budget charge. Comparing it with the budget's
/// own `charged` counts is what proves every unit of work was paid for.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompletionWalkWork {
    pub per_op: [u32; crate::game::ability_utils::WalkOp::COUNT],
}

/// Test-only work counter for the CR 610.3b latch pass in
/// `engine::check_exile_returns`. Kept out of [`PerfCounterSnapshot`] for the
/// same reason as the counter sets above: that struct's serialized field set
/// powers the AI performance baseline.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExileReturnLatchCounters {
    /// Searches of the pass's event buffer for a holder's trigger event.
    pub trigger_origin_lookups: u64,
}

/// Test-only counters for the stack's bulk token executor
/// (`stack::resolve_bulk_token_run`) and the post-action pipeline it elides.
/// Kept out of [`PerfCounterSnapshot`] for the same reason as the counter sets
/// above: that struct's serialized field set powers the AI performance baseline.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StackBulkCounters {
    /// Stack entries consumed by a committed bulk token run.
    pub bulk_entries: u64,
    /// Entries into `engine_priority::run_post_action_pipeline_from_with_policy`.
    pub post_action_pipeline_passes: u64,
}

/// Test-only counters for the CR 508.1d attack-declaration solver
/// (`combat::selectable_targets_by_attacker` and the strict validator it
/// drives). Kept out of [`PerfCounterSnapshot`] for the same reason the two
/// counter sets above are: that struct's serialized field set powers the AI
/// performance baseline (`phase-ai::duel_suite::perf`), which these
/// declare-attackers-prompt guards have no business perturbing.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttackDeclarationSolverCounters {
    /// Times the solver built its per-candidate target table
    /// (`SolverTargetTable::build`). The table depends only on the constraints
    /// model + target universe, never on the forced pair, so the prompt builder
    /// must build ONE per universe rather than one per (attacker, target) pair.
    pub target_table_builds: u64,
    /// Whole-battlefield static sweeps taken to derive an attacker cap
    /// (`max_attackers_each_combat` / `per_defender_caps` /
    /// `per_permanent_defender_caps`). The constraints model caches all three at
    /// build time, so a validation run against a prebuilt model must add none.
    pub cap_static_sweeps: u64,
    /// CR 508.1b: per-(attacker, defender) pairability evaluations
    /// (`combat::attacker_can_attack_target`), the single predicate behind both
    /// views of `combat::legal_attack_targets_iter`.
    ///
    /// Pins that the EXISTENTIAL view (CR 508.1d "if able", asked once per
    /// must-attack creature by `AttackDeclarationConstraints::build` and by the
    /// AI mandatory-attacker filter) short-circuits on the first legal pairing
    /// instead of evaluating — and sorting — the whole defender universe. Only
    /// the LIST view may spend one evaluation per defender.
    pub pairability_evaluations: u64,
    /// CR 702.3b: full-body executions of
    /// `combat::creature_can_attack_despite_defender` — i.e. defender-permission
    /// lookups that got PAST the `!Defender` guard.
    ///
    /// The attack-side twin of [`record_combat_shadow_block_scan`], which counts
    /// the same thing for `CanBlockShadow`. Placement differs deliberately: the
    /// block-side helper has no early return, so "first statement" and "first
    /// statement past the keyword gate" coincide there; here they do not, and
    /// only the post-gate placement counts BODY work. That is what makes
    /// "`!Defender` is the FIRST conjunct" revert-failing: move the guard below
    /// the two arms and every vanilla creature registers, once per pairing.
    pub defender_permission_lookups: u64,
}

/// Test-only counters for the work a replay take does on the turn's growing history: history
/// records deep-copied, map-backed history entries a state copy does not share, state copies,
/// keyed journal reads and the records they examine, the replay drive's whole-state
/// snapshots, and the mana-pool entries and units walked. Kept out of [`PerfCounterSnapshot`]
/// for the same reason as the counter sets above.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TakeCostCounters {
    pub history_entries_copied: u64,
    pub history_map_entries_unshared: u64,
    pub state_copies: u64,
    pub journal_keyed_reads: u64,
    pub journal_records_examined: u64,
    pub drive_snapshots: u64,
    pub pool_entries_walked: u64,
}

#[cfg(feature = "test-support")]
impl TakeCostCounters {
    /// The counts accrued since `earlier`, a snapshot taken on the same thread.
    pub fn since(self, earlier: Self) -> Self {
        Self {
            history_entries_copied: self.history_entries_copied - earlier.history_entries_copied,
            history_map_entries_unshared: self.history_map_entries_unshared
                - earlier.history_map_entries_unshared,
            state_copies: self.state_copies - earlier.state_copies,
            journal_keyed_reads: self.journal_keyed_reads - earlier.journal_keyed_reads,
            journal_records_examined: self.journal_records_examined
                - earlier.journal_records_examined,
            drive_snapshots: self.drive_snapshots - earlier.drive_snapshots,
            pool_entries_walked: self.pool_entries_walked - earlier.pool_entries_walked,
        }
    }
}

/// Test-only counters for the play trace's work: action boundaries entered (outermost, nested,
/// inside a probe), entries recorded, the trace's node-map and node-key work, whole-state copies
/// its own code makes, the windows and legality reads its naming makes, and the spans the
/// producer asks the confirmer and the replays it drives for them.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayTraceCounters {
    pub boundary_entries: u64,
    pub nested_applies: u64,
    pub probe_entries: u64,
    pub actions_recorded: u64,
    pub resolutions_recorded: u64,
    pub resolution_hooks_in_probe: u64,
    pub node_map_reads: u64,
    pub node_map_writes: u64,
    pub node_key_compares: u64,
    pub trace_state_copies: u64,
    pub windows: u64,
    pub legality_reads: u64,
    pub legality_read_copies: u64,
    pub confirm_asks: u64,
    pub confirm_drives: u64,
    /// Entries appended to carried spans.
    pub carry_appends: u64,
    /// Old-window nodes looked up in the carrying index at a step's end.
    pub step_end_lookups: u64,
    /// Engine B runs that rebuilt a seat's carrying index.
    pub engine_b_rebuilds: u64,
    /// Objects visited by the face-set scans that fingerprint a seat's printed faces.
    pub face_set_scans: u64,
}

#[cfg(feature = "test-support")]
impl PlayTraceCounters {
    /// The counts accrued since `earlier`, a snapshot taken on the same thread.
    pub fn since(self, earlier: Self) -> Self {
        Self {
            boundary_entries: self.boundary_entries - earlier.boundary_entries,
            nested_applies: self.nested_applies - earlier.nested_applies,
            probe_entries: self.probe_entries - earlier.probe_entries,
            actions_recorded: self.actions_recorded - earlier.actions_recorded,
            resolutions_recorded: self.resolutions_recorded - earlier.resolutions_recorded,
            resolution_hooks_in_probe: self.resolution_hooks_in_probe
                - earlier.resolution_hooks_in_probe,
            node_map_reads: self.node_map_reads - earlier.node_map_reads,
            node_map_writes: self.node_map_writes - earlier.node_map_writes,
            node_key_compares: self.node_key_compares - earlier.node_key_compares,
            trace_state_copies: self.trace_state_copies - earlier.trace_state_copies,
            windows: self.windows - earlier.windows,
            legality_reads: self.legality_reads - earlier.legality_reads,
            legality_read_copies: self.legality_read_copies - earlier.legality_read_copies,
            confirm_asks: self.confirm_asks - earlier.confirm_asks,
            confirm_drives: self.confirm_drives - earlier.confirm_drives,
            carry_appends: self.carry_appends - earlier.carry_appends,
            step_end_lookups: self.step_end_lookups - earlier.step_end_lookups,
            engine_b_rebuilds: self.engine_b_rebuilds - earlier.engine_b_rebuilds,
            face_set_scans: self.face_set_scans - earlier.face_set_scans,
        }
    }
}

/// One replay take: the count it was driven at, the cycles it delivered, the whole take's counts,
/// and each delivered cycle's counts in order.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TakeCostRecord {
    pub count: u32,
    pub delivered: u32,
    pub whole_take: TakeCostCounters,
    pub cycles: Vec<TakeCostCounters>,
}

/// Entries of a map-backed history field that a state copy does not share with its original.
#[cfg(feature = "test-support")]
pub(crate) trait Unshared {
    fn unshared(&self, copy: &Self) -> u64;
}

// An `im` copy shares every entry while it is `ptr_eq` to its original, and none after a write.
#[cfg(feature = "test-support")]
impl<K, V, S> Unshared for im::HashMap<K, V, S>
where
    K: std::hash::Hash + Eq + Clone,
    V: Clone,
    S: std::hash::BuildHasher,
{
    fn unshared(&self, copy: &Self) -> u64 {
        if self.ptr_eq(copy) {
            0
        } else {
            self.len() as u64
        }
    }
}

#[cfg(feature = "test-support")]
impl<K, V> Unshared for im::OrdMap<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    fn unshared(&self, copy: &Self) -> u64 {
        if self.ptr_eq(copy) {
            0
        } else {
            self.len() as u64
        }
    }
}

#[cfg(feature = "test-support")]
impl<T, S> Unshared for im::HashSet<T, S>
where
    T: std::hash::Hash + Eq + Clone,
    S: std::hash::BuildHasher,
{
    fn unshared(&self, copy: &Self) -> u64 {
        if self.ptr_eq(copy) {
            0
        } else {
            self.len() as u64
        }
    }
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
        generation_state_clones: 0,
        strict_fast_path_state_clones: 0,
        strict_fast_path_mana_readiness_state_clones: 0,
        raw_validation_state_clones: 0,
        grouped_mana_readiness_state_clones: 0,
        post_apply_auto_payment_core_state_clones: 0,
        priority_cast_probe_state_clones: 0,
        auto_payment_borrowed_wrapper_calls: 0,
        auto_payment_owned_state_clones: 0,
        generation_auto_payment_wrapper_calls: 0,
        strict_fast_path_auto_payment_wrapper_calls: 0,
        post_apply_auto_payment_core_calls: 0,
        post_apply_uncached_source_collections: 0,
        static_full_scans: 0,
        spell_keyword_grant_scans: 0,
        layers_full_eval: 0,
        layers_incremental: 0,
        layers_escalated: 0,
        mana_display_sweeps: 0,
        mana_display_swept_objects: 0,
        activation_verdict_passes: 0,
        activation_block_display_abilities_examined: 0,
        activation_verdict_flush_clones: 0,
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
    #[cfg(feature = "test-support")]
    static HOMOGENEOUS_TARGET_WALK_CACHE_COUNTERS: Cell<HomogeneousTargetWalkCacheCounters> = const {
        Cell::new(HomogeneousTargetWalkCacheCounters {
            initializations: 0,
            advances: 0,
        })
    };
    #[cfg(feature = "test-support")]
    static PRIOR_TARGET_BINDING_COUNTERS: Cell<PriorTargetBindingCounters> = const {
        Cell::new(PriorTargetBindingCounters {
            static_union_enumerations: 0,
            selection_bindings: 0,
        })
    };
    #[cfg(feature = "test-support")]
    static ATTACK_DECLARATION_SOLVER_COUNTERS: Cell<AttackDeclarationSolverCounters> = const {
        Cell::new(AttackDeclarationSolverCounters {
            target_table_builds: 0,
            cap_static_sweeps: 0,
            pairability_evaluations: 0,
            defender_permission_lookups: 0,
        })
    };
    #[cfg(feature = "test-support")]
    static ACTIVATION_COST_ROUTE_COUNTERS: Cell<ActivationCostRouteCounters> = const {
        Cell::new(ActivationCostRouteCounters {
            settlement_writebacks: 0,
            zero_mana_leg_skips: 0,
            settlements: 0,
            open_settlements: 0,
            carrierless_settlements: 0,
            window_searches: 0,
            guard_reaches: [0; crate::game::casting::ActivationCostGuardSite::COUNT],
        })
    };
    #[cfg(feature = "test-support")]
    static COMPLETION_WALK_WORK: Cell<CompletionWalkWork> = const {
        Cell::new(CompletionWalkWork {
            per_op: [0; crate::game::ability_utils::WalkOp::COUNT],
        })
    };
    #[cfg(feature = "test-support")]
    static TAKE_COST_COUNTERS: Cell<TakeCostCounters> = const {
        Cell::new(TakeCostCounters {
            history_entries_copied: 0,
            history_map_entries_unshared: 0,
            state_copies: 0,
            journal_keyed_reads: 0,
            journal_records_examined: 0,
            drive_snapshots: 0,
            pool_entries_walked: 0,
        })
    };
    #[cfg(feature = "test-support")]
    static TAKE_COST_RECORDS: RefCell<Vec<TakeCostRecord>> = const { RefCell::new(Vec::new()) };
    #[cfg(feature = "test-support")]
    static PLAY_TRACE_COUNTERS: Cell<PlayTraceCounters> = const {
        Cell::new(PlayTraceCounters {
            boundary_entries: 0,
            nested_applies: 0,
            probe_entries: 0,
            actions_recorded: 0,
            resolutions_recorded: 0,
            resolution_hooks_in_probe: 0,
            node_map_reads: 0,
            node_map_writes: 0,
            node_key_compares: 0,
            trace_state_copies: 0,
            windows: 0,
            legality_reads: 0,
            legality_read_copies: 0,
            confirm_asks: 0,
            confirm_drives: 0,
            carry_appends: 0,
            step_end_lookups: 0,
            engine_b_rebuilds: 0,
            face_set_scans: 0,
        })
    };
    #[cfg(feature = "test-support")]
    static EXILE_RETURN_LATCH_COUNTERS: Cell<ExileReturnLatchCounters> = const {
        Cell::new(ExileReturnLatchCounters {
            trigger_origin_lookups: 0,
        })
    };
    #[cfg(feature = "test-support")]
    static STACK_BULK_COUNTERS: Cell<StackBulkCounters> = const {
        Cell::new(StackBulkCounters {
            bulk_entries: 0,
            post_action_pipeline_passes: 0,
        })
    };
    static LEGALITY_CLONE_PHASE: Cell<Option<LegalityClonePhase>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegalityClonePhase {
    Generation,
    StrictFastPath,
    RawValidation,
    GroupedManaReadiness,
    PostApplyCore,
}

pub(crate) struct LegalityClonePhaseGuard {
    previous: Option<LegalityClonePhase>,
}

impl LegalityClonePhaseGuard {
    pub(crate) fn enter(phase: LegalityClonePhase) -> Self {
        let previous = LEGALITY_CLONE_PHASE.with(|current| current.replace(Some(phase)));
        Self { previous }
    }
}

impl Drop for LegalityClonePhaseGuard {
    fn drop(&mut self) {
        LEGALITY_CLONE_PHASE.with(|current| current.set(self.previous));
    }
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

#[cfg(feature = "test-support")]
pub fn record_homogeneous_target_walk_cache_initialization() {
    HOMOGENEOUS_TARGET_WALK_CACHE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.initializations += 1;
        cell.set(counters);
    });
}

#[cfg(feature = "test-support")]
pub fn record_homogeneous_target_walk_cache_advance() {
    HOMOGENEOUS_TARGET_WALK_CACHE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.advances += 1;
        cell.set(counters);
    });
}

/// CR 601.2c + CR 603.3d: recorded once per candidate of the prior object slot
/// unioned at static slot-build time (`union_over_prior_object_candidates`).
#[cfg(feature = "test-support")]
pub fn record_prior_target_binding_static_union_enumeration() {
    PRIOR_TARGET_BINDING_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.static_union_enumerations += 1;
        cell.set(counters);
    });
}

/// CR 601.2c + CR 603.3d: recorded once per selection-time bind
/// (`bind_prior_object_targets` returning `Some`) inside
/// `legal_targets_for_selected_slot`.
#[cfg(feature = "test-support")]
pub fn record_prior_target_binding_selection() {
    PRIOR_TARGET_BINDING_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.selection_bindings += 1;
        cell.set(counters);
    });
}

fn record_phase_owned_state_clone_for(
    snapshot: &mut PerfCounterSnapshot,
    phase: Option<LegalityClonePhase>,
) {
    match phase {
        Some(LegalityClonePhase::Generation) => snapshot.generation_state_clones += 1,
        Some(LegalityClonePhase::StrictFastPath) => snapshot.strict_fast_path_state_clones += 1,
        Some(LegalityClonePhase::RawValidation) => snapshot.raw_validation_state_clones += 1,
        Some(LegalityClonePhase::GroupedManaReadiness) => {
            snapshot.grouped_mana_readiness_state_clones += 1;
        }
        Some(LegalityClonePhase::PostApplyCore) => {
            snapshot.post_apply_auto_payment_core_state_clones += 1;
        }
        None => {}
    }
}

pub(crate) fn record_phase_owned_state_clone() {
    let phase = LEGALITY_CLONE_PHASE.with(Cell::get);
    with_mut(|snapshot| record_phase_owned_state_clone_for(snapshot, phase));
}

pub(crate) fn record_mana_readiness_state_clone() {
    let phase = LEGALITY_CLONE_PHASE.with(Cell::get);
    with_mut(|snapshot| {
        record_phase_owned_state_clone_for(snapshot, phase);
        if phase == Some(LegalityClonePhase::StrictFastPath) {
            snapshot.strict_fast_path_mana_readiness_state_clones += 1;
        }
    });
}

pub(crate) fn record_auto_payment_borrowed_wrapper() {
    let phase = LEGALITY_CLONE_PHASE.with(Cell::get);
    with_mut(|s| {
        s.auto_payment_borrowed_wrapper_calls += 1;
        s.auto_payment_owned_state_clones += 1;
        match phase {
            Some(LegalityClonePhase::Generation) => {
                s.generation_auto_payment_wrapper_calls += 1;
            }
            Some(LegalityClonePhase::StrictFastPath) => {
                s.strict_fast_path_auto_payment_wrapper_calls += 1;
            }
            Some(
                LegalityClonePhase::RawValidation
                | LegalityClonePhase::GroupedManaReadiness
                | LegalityClonePhase::PostApplyCore,
            )
            | None => {}
        }
    });
    record_phase_owned_state_clone();
}

pub(crate) fn record_priority_cast_probe_state_clone() {
    with_mut(|s| s.priority_cast_probe_state_clones += 1);
}

pub(crate) fn record_post_apply_auto_payment_core_call() {
    if LEGALITY_CLONE_PHASE.with(Cell::get) == Some(LegalityClonePhase::PostApplyCore) {
        with_mut(|s| s.post_apply_auto_payment_core_calls += 1);
    }
}

pub(crate) fn record_post_apply_uncached_source_collection() {
    if LEGALITY_CLONE_PHASE.with(Cell::get) == Some(LegalityClonePhase::PostApplyCore) {
        with_mut(|s| s.post_apply_uncached_source_collections += 1);
    }
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
///
/// Also incremented by `combat::compute_combat_tax` once per admitted call — i.e.
/// once per real `battlefield ∪ command_zone` tax sweep, AFTER its O(1)
/// `static_kind_present(CantAttack / CantBlock / CantAttackOrBlock)` gate. Attack
/// candidate enumeration asks for a tax verdict once per proposed (attacker,
/// target) pairing, so on a board with no combat-tax static this counter stays at
/// 0 across the whole enumeration instead of reaching 2N.
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

pub fn record_activation_verdict_pass() {
    with_mut(|s| s.activation_verdict_passes += 1);
}

pub fn record_activation_block_display_abilities_examined(examined: usize) {
    with_mut(|s| s.activation_block_display_abilities_examined += examined as u64);
}

pub fn record_activation_verdict_flush_clone() {
    with_mut(|s| s.activation_verdict_flush_clones += 1);
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

#[cfg(feature = "test-support")]
pub fn homogeneous_target_walk_cache_snapshot() -> HomogeneousTargetWalkCacheCounters {
    HOMOGENEOUS_TARGET_WALK_CACHE_COUNTERS.with(Cell::get)
}

#[cfg(feature = "test-support")]
pub fn prior_target_binding_snapshot() -> PriorTargetBindingCounters {
    PRIOR_TARGET_BINDING_COUNTERS.with(Cell::get)
}

/// CR 508.1d: one solver target-table build.
#[cfg(feature = "test-support")]
pub fn record_attack_solver_target_table_build() {
    ATTACK_DECLARATION_SOLVER_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.target_table_builds += 1;
        cell.set(counters);
    });
}

/// CR 508.1c: one whole-battlefield sweep taken to derive an attacker cap.
#[cfg(feature = "test-support")]
pub fn record_attack_cap_static_sweep() {
    ATTACK_DECLARATION_SOLVER_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.cap_static_sweeps += 1;
        cell.set(counters);
    });
}

/// CR 508.1b: one per-pairing `attacker_can_attack_target` evaluation.
#[cfg(feature = "test-support")]
pub fn record_attack_pairability_evaluation() {
    ATTACK_DECLARATION_SOLVER_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.pairability_evaluations += 1;
        cell.set(counters);
    });
}

/// CR 702.3b: one full-body `combat::creature_can_attack_despite_defender`
/// execution — a defender-permission lookup that got PAST the `!Defender` guard.
#[cfg(feature = "test-support")]
pub fn record_defender_permission_lookup() {
    ATTACK_DECLARATION_SOLVER_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.defender_permission_lookups += 1;
        cell.set(counters);
    });
}

#[cfg(feature = "test-support")]
pub fn attack_declaration_solver_snapshot() -> AttackDeclarationSolverCounters {
    ATTACK_DECLARATION_SOLVER_COUNTERS.with(Cell::get)
}

/// CR 601.2f + CR 602.2b: one settlement write-back choosing the mana carrier.
#[cfg(feature = "test-support")]
pub fn record_activation_settlement_writeback() {
    ACTIVATION_COST_ROUTE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.settlement_writebacks += 1;
        cell.set(counters);
    });
}

/// CR 601.2h: one mana-leg finalization that found nothing left to pay.
#[cfg(feature = "test-support")]
pub fn record_activation_zero_mana_leg_skip() {
    ACTIVATION_COST_ROUTE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.zero_mana_leg_skips += 1;
        cell.set(counters);
    });
}

/// CR 601.2c + CR 602.2b: one run of the target-settlement cost lock.
#[cfg(feature = "test-support")]
pub fn record_activation_settlement(
    snapshot: Option<&crate::types::casting_costs::ActivationCostSnapshot>,
) {
    ACTIVATION_COST_ROUTE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.settlements += 1;
        match snapshot.map(|snapshot| &snapshot.lock) {
            Some(crate::types::casting_costs::ActivationCostLock::Open { .. }) => {
                counters.open_settlements += 1;
            }
            Some(crate::types::casting_costs::ActivationCostLock::Locked { .. }) => {}
            None => counters.carrierless_settlements += 1,
        }
        cell.set(counters);
    });
}

/// CR 601.2f: one reach of an unlocked-cost guard.
#[cfg(feature = "test-support")]
pub fn record_activation_cost_guard(site: crate::game::casting::ActivationCostGuardSite) {
    ACTIVATION_COST_ROUTE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.guard_reaches[site as usize] += 1;
        cell.set(counters);
    });
}

/// CR 118.3 + CR 601.2c: one pre-activation feasibility walk over target
/// assignments.
#[cfg(feature = "test-support")]
pub fn record_activation_cost_window_search() {
    ACTIVATION_COST_ROUTE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.window_searches += 1;
        cell.set(counters);
    });
}

#[cfg(feature = "test-support")]
pub fn activation_cost_route_snapshot() -> ActivationCostRouteCounters {
    ACTIVATION_COST_ROUTE_COUNTERS.with(Cell::get)
}

/// One unit of target-completion walk work, performed after its charge succeeded.
#[cfg(feature = "test-support")]
pub fn record_completion_walk_work(op: crate::game::ability_utils::WalkOp) {
    COMPLETION_WALK_WORK.with(|cell| {
        let mut work = cell.get();
        work.per_op[op as usize] += 1;
        cell.set(work);
    });
}

#[cfg(feature = "test-support")]
pub fn completion_walk_work_snapshot() -> CompletionWalkWork {
    COMPLETION_WALK_WORK.with(Cell::get)
}

/// CR 610.3b: one search of the latch pass's event buffer for a holder's trigger event.
#[cfg(feature = "test-support")]
pub fn record_exile_return_trigger_origin_lookup() {
    EXILE_RETURN_LATCH_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        counters.trigger_origin_lookups += 1;
        cell.set(counters);
    });
}

#[cfg(feature = "test-support")]
pub fn exile_return_latch_snapshot() -> ExileReturnLatchCounters {
    EXILE_RETURN_LATCH_COUNTERS.with(Cell::get)
}

#[cfg(feature = "test-support")]
pub fn reset_prior_target_binding_counters() {
    PRIOR_TARGET_BINDING_COUNTERS
        .with(|counters| counters.set(PriorTargetBindingCounters::default()));
}

#[cfg(feature = "test-support")]
fn with_take_cost(f: impl FnOnce(&mut TakeCostCounters)) {
    TAKE_COST_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        f(&mut counters);
        cell.set(counters);
    });
}

#[cfg(feature = "test-support")]
pub fn record_history_entry_copied() {
    with_take_cost(|c| c.history_entries_copied += 1);
}

#[cfg(feature = "test-support")]
pub fn record_history_map_entries_unshared(entries: u64) {
    with_take_cost(|c| c.history_map_entries_unshared += entries);
}

#[cfg(feature = "test-support")]
pub fn record_state_copy() {
    with_take_cost(|c| c.state_copies += 1);
}

#[cfg(feature = "test-support")]
pub fn record_journal_keyed_read() {
    with_take_cost(|c| c.journal_keyed_reads += 1);
}

#[cfg(feature = "test-support")]
pub fn record_journal_record_examined() {
    with_take_cost(|c| c.journal_records_examined += 1);
}

#[cfg(feature = "test-support")]
pub fn record_drive_snapshot() {
    with_take_cost(|c| c.drive_snapshots += 1);
}

/// Runs a debug-build check without charging its work to the take-cost counters.
#[cfg(debug_assertions)]
pub(crate) fn outside_take_cost(check: impl FnOnce()) {
    #[cfg(feature = "test-support")]
    let counted = TAKE_COST_COUNTERS.with(Cell::get);
    check();
    #[cfg(feature = "test-support")]
    TAKE_COST_COUNTERS.with(|cell| cell.set(counted));
}

#[cfg(feature = "test-support")]
pub fn record_pool_entries_walked(entries: u64) {
    with_take_cost(|c| c.pool_entries_walked += entries);
}

#[cfg(feature = "test-support")]
pub fn take_cost_snapshot() -> TakeCostCounters {
    TAKE_COST_COUNTERS.with(Cell::get)
}

#[cfg(feature = "test-support")]
pub(crate) fn record_play_trace(f: impl FnOnce(&mut PlayTraceCounters)) {
    PLAY_TRACE_COUNTERS.with(|cell| {
        let mut counters = cell.get();
        f(&mut counters);
        cell.set(counters);
    });
}

#[cfg(feature = "test-support")]
pub fn play_trace_counters() -> PlayTraceCounters {
    PLAY_TRACE_COUNTERS.with(Cell::get)
}

/// Every take recorded on this thread since the last [`reset`], in order.
#[cfg(feature = "test-support")]
pub fn take_cost_records() -> Vec<TakeCostRecord> {
    TAKE_COST_RECORDS.with(|records| records.borrow().clone())
}

#[cfg(feature = "test-support")]
pub fn reset_take_cost() {
    TAKE_COST_COUNTERS.with(|counters| counters.set(TakeCostCounters::default()));
    TAKE_COST_RECORDS.with(|records| records.borrow_mut().clear());
}

/// Builds one take's [`TakeCostRecord`] as the replay drive performs it.
#[cfg(feature = "test-support")]
pub(crate) struct TakeCostRecorder {
    count: u32,
    take_start: TakeCostCounters,
    cycle_start: TakeCostCounters,
    cycles: Vec<TakeCostCounters>,
}

#[cfg(feature = "test-support")]
impl TakeCostRecorder {
    pub(crate) fn begin(count: u32) -> Self {
        let now = take_cost_snapshot();
        Self {
            count,
            take_start: now,
            cycle_start: now,
            cycles: Vec::new(),
        }
    }

    pub(crate) fn begin_cycle(&mut self) {
        self.cycle_start = take_cost_snapshot();
    }

    pub(crate) fn end_cycle(&mut self) {
        self.cycles
            .push(take_cost_snapshot().since(self.cycle_start));
    }

    pub(crate) fn finish(self, delivered: u32) {
        let record = TakeCostRecord {
            count: self.count,
            delivered,
            whole_take: take_cost_snapshot().since(self.take_start),
            cycles: self.cycles,
        };
        TAKE_COST_RECORDS.with(|records| records.borrow_mut().push(record));
    }
}

pub fn reset() {
    COUNTERS.with(|c| c.set(PerfCounterSnapshot::default()));
    #[cfg(feature = "test-support")]
    HOMOGENEOUS_TARGET_WALK_CACHE_COUNTERS
        .with(|counters| counters.set(HomogeneousTargetWalkCacheCounters::default()));
    #[cfg(feature = "test-support")]
    reset_prior_target_binding_counters();
    #[cfg(feature = "test-support")]
    ATTACK_DECLARATION_SOLVER_COUNTERS
        .with(|counters| counters.set(AttackDeclarationSolverCounters::default()));
    #[cfg(feature = "test-support")]
    ACTIVATION_COST_ROUTE_COUNTERS
        .with(|counters| counters.set(ActivationCostRouteCounters::default()));
    #[cfg(feature = "test-support")]
    COMPLETION_WALK_WORK.with(|counters| counters.set(CompletionWalkWork::default()));
    #[cfg(feature = "test-support")]
    reset_take_cost();
    #[cfg(feature = "test-support")]
    PLAY_TRACE_COUNTERS.with(|counters| counters.set(PlayTraceCounters::default()));
    #[cfg(feature = "test-support")]
    EXILE_RETURN_LATCH_COUNTERS.with(|counters| counters.set(ExileReturnLatchCounters::default()));
    #[cfg(feature = "test-support")]
    STACK_BULK_COUNTERS.with(|counters| counters.set(StackBulkCounters::default()));
}

/// One committed bulk token run resolved `n` stack entries.
#[cfg(feature = "test-support")]
pub fn record_stack_bulk_entries(n: u32) {
    STACK_BULK_COUNTERS.with(|cell| {
        let mut c = cell.get();
        c.bulk_entries += u64::from(n);
        cell.set(c);
    });
}

/// One post-action pipeline pass (one priority checkpoint).
#[cfg(feature = "test-support")]
pub fn record_post_action_pipeline_pass() {
    STACK_BULK_COUNTERS.with(|cell| {
        let mut c = cell.get();
        c.post_action_pipeline_passes += 1;
        cell.set(c);
    });
}

#[cfg(feature = "test-support")]
pub fn stack_bulk_snapshot() -> StackBulkCounters {
    STACK_BULK_COUNTERS.with(Cell::get)
}
