use std::cell::Cell;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PerfCounterSnapshot {
    pub state_clone_for_legality: u64,
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

pub fn snapshot() -> PerfCounterSnapshot {
    COUNTERS.with(|c| c.get())
}

pub fn reset() {
    COUNTERS.with(|c| c.set(PerfCounterSnapshot::default()));
}
