use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PerfCounterSnapshot {
    pub state_clone_for_legality: u64,
    pub layers_full_eval: u64,
    pub layers_incremental: u64,
    pub layers_escalated: u64,
    pub mana_display_sweeps: u64,
    pub mana_display_swept_objects: u64,
}

static STATE_CLONE_FOR_LEGALITY: AtomicU64 = AtomicU64::new(0);
static LAYERS_FULL_EVAL: AtomicU64 = AtomicU64::new(0);
static LAYERS_INCREMENTAL: AtomicU64 = AtomicU64::new(0);
static LAYERS_ESCALATED: AtomicU64 = AtomicU64::new(0);
static MANA_DISPLAY_SWEEPS: AtomicU64 = AtomicU64::new(0);
static MANA_DISPLAY_SWEPT_OBJECTS: AtomicU64 = AtomicU64::new(0);

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

pub fn snapshot() -> PerfCounterSnapshot {
    PerfCounterSnapshot {
        state_clone_for_legality: STATE_CLONE_FOR_LEGALITY.load(Ordering::Relaxed),
        layers_full_eval: LAYERS_FULL_EVAL.load(Ordering::Relaxed),
        layers_incremental: LAYERS_INCREMENTAL.load(Ordering::Relaxed),
        layers_escalated: LAYERS_ESCALATED.load(Ordering::Relaxed),
        mana_display_sweeps: MANA_DISPLAY_SWEEPS.load(Ordering::Relaxed),
        mana_display_swept_objects: MANA_DISPLAY_SWEPT_OBJECTS.load(Ordering::Relaxed),
    }
}

pub fn reset() {
    STATE_CLONE_FOR_LEGALITY.store(0, Ordering::Relaxed);
    LAYERS_FULL_EVAL.store(0, Ordering::Relaxed);
    LAYERS_INCREMENTAL.store(0, Ordering::Relaxed);
    LAYERS_ESCALATED.store(0, Ordering::Relaxed);
    MANA_DISPLAY_SWEEPS.store(0, Ordering::Relaxed);
    MANA_DISPLAY_SWEPT_OBJECTS.store(0, Ordering::Relaxed);
}
