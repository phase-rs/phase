use std::sync::atomic::{AtomicU64, Ordering};

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

static STATE_CLONE_FOR_LEGALITY: AtomicU64 = AtomicU64::new(0);
static LAYERS_FULL_EVAL: AtomicU64 = AtomicU64::new(0);
static LAYERS_INCREMENTAL: AtomicU64 = AtomicU64::new(0);
static LAYERS_ESCALATED: AtomicU64 = AtomicU64::new(0);
static MANA_DISPLAY_SWEEPS: AtomicU64 = AtomicU64::new(0);
static MANA_DISPLAY_SWEPT_OBJECTS: AtomicU64 = AtomicU64::new(0);
static STACK_BATCH_CANDIDATES: AtomicU64 = AtomicU64::new(0);
static STACK_BATCH_PLANS: AtomicU64 = AtomicU64::new(0);
static STACK_BATCH_OBSERVER_REFUSALS: AtomicU64 = AtomicU64::new(0);
static STACK_BATCHED_ENTRIES: AtomicU64 = AtomicU64::new(0);
static STACK_INERT_NOOP_BATCHES: AtomicU64 = AtomicU64::new(0);
static STACK_INERT_NOOP_ENTRIES: AtomicU64 = AtomicU64::new(0);

pub fn record_state_clone_for_legality() {
    STATE_CLONE_FOR_LEGALITY.fetch_add(1, Ordering::Relaxed);
}

pub fn record_layers_full_eval() {
    LAYERS_FULL_EVAL.fetch_add(1, Ordering::Relaxed);
}

pub fn record_layers_incremental() {
    LAYERS_INCREMENTAL.fetch_add(1, Ordering::Relaxed);
}

pub fn record_layers_escalated() {
    LAYERS_ESCALATED.fetch_add(1, Ordering::Relaxed);
}

pub fn record_mana_display_sweep(swept_objects: usize) {
    MANA_DISPLAY_SWEEPS.fetch_add(1, Ordering::Relaxed);
    MANA_DISPLAY_SWEPT_OBJECTS.fetch_add(swept_objects as u64, Ordering::Relaxed);
}

pub fn record_stack_batch_candidate() {
    STACK_BATCH_CANDIDATES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_stack_batch_plan() {
    STACK_BATCH_PLANS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_stack_batch_observer_refusal() {
    STACK_BATCH_OBSERVER_REFUSALS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_stack_batched_entries(entries: u32) {
    STACK_BATCHED_ENTRIES.fetch_add(u64::from(entries), Ordering::Relaxed);
}

pub fn record_stack_inert_noop_batch(entries: u32) {
    STACK_INERT_NOOP_BATCHES.fetch_add(1, Ordering::Relaxed);
    STACK_INERT_NOOP_ENTRIES.fetch_add(u64::from(entries), Ordering::Relaxed);
}

pub fn snapshot() -> PerfCounterSnapshot {
    PerfCounterSnapshot {
        state_clone_for_legality: STATE_CLONE_FOR_LEGALITY.load(Ordering::Relaxed),
        layers_full_eval: LAYERS_FULL_EVAL.load(Ordering::Relaxed),
        layers_incremental: LAYERS_INCREMENTAL.load(Ordering::Relaxed),
        layers_escalated: LAYERS_ESCALATED.load(Ordering::Relaxed),
        mana_display_sweeps: MANA_DISPLAY_SWEEPS.load(Ordering::Relaxed),
        mana_display_swept_objects: MANA_DISPLAY_SWEPT_OBJECTS.load(Ordering::Relaxed),
        stack_batch_candidates: STACK_BATCH_CANDIDATES.load(Ordering::Relaxed),
        stack_batch_plans: STACK_BATCH_PLANS.load(Ordering::Relaxed),
        stack_batch_observer_refusals: STACK_BATCH_OBSERVER_REFUSALS.load(Ordering::Relaxed),
        stack_batched_entries: STACK_BATCHED_ENTRIES.load(Ordering::Relaxed),
        stack_inert_noop_batches: STACK_INERT_NOOP_BATCHES.load(Ordering::Relaxed),
        stack_inert_noop_entries: STACK_INERT_NOOP_ENTRIES.load(Ordering::Relaxed),
    }
}

pub fn reset() {
    STATE_CLONE_FOR_LEGALITY.store(0, Ordering::Relaxed);
    LAYERS_FULL_EVAL.store(0, Ordering::Relaxed);
    LAYERS_INCREMENTAL.store(0, Ordering::Relaxed);
    LAYERS_ESCALATED.store(0, Ordering::Relaxed);
    MANA_DISPLAY_SWEEPS.store(0, Ordering::Relaxed);
    MANA_DISPLAY_SWEPT_OBJECTS.store(0, Ordering::Relaxed);
    STACK_BATCH_CANDIDATES.store(0, Ordering::Relaxed);
    STACK_BATCH_PLANS.store(0, Ordering::Relaxed);
    STACK_BATCH_OBSERVER_REFUSALS.store(0, Ordering::Relaxed);
    STACK_BATCHED_ENTRIES.store(0, Ordering::Relaxed);
    STACK_INERT_NOOP_BATCHES.store(0, Ordering::Relaxed);
    STACK_INERT_NOOP_ENTRIES.store(0, Ordering::Relaxed);
}
