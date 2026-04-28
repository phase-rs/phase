//! Thread-local parse warning accumulator.
//!
//! Collects diagnostic warnings during Oracle text parsing — silent fallbacks,
//! ignored remainders, bare filters — without changing any parse results.
//! Warnings are harvested after each card's parse and stored on `CardFace`.

use std::cell::RefCell;

thread_local! {
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Push a diagnostic warning for the card currently being parsed.
pub fn push_warning(msg: impl Into<String>) {
    WARNINGS.with(|w| w.borrow_mut().push(msg.into()));
}

/// Drain all accumulated warnings (returns them and clears the buffer).
pub fn take_warnings() -> Vec<String> {
    WARNINGS.with(|w| w.borrow_mut().drain(..).collect())
}

/// Discard any accumulated warnings (called at the start of each card parse).
pub fn clear_warnings() {
    WARNINGS.with(|w| w.borrow_mut().clear());
}

/// Snapshot the current warnings buffer length. Pair with `truncate_warnings`
/// to roll back any warnings emitted during a trial parse that ends up being
/// rejected — e.g., `try_parse_choose_one_of_inline` runs `parse_effect_clause`
/// on a candidate left/right half before deciding whether the split is
/// real, and side-effects from those trial parses must not leak into the
/// committed warnings buffer when the split is rejected.
pub fn snapshot_warnings() -> usize {
    WARNINGS.with(|w| w.borrow().len())
}

/// Truncate the warnings buffer back to the given snapshot length, discarding
/// any warnings pushed since the snapshot. Used for trial-parse rollback.
pub fn truncate_warnings(snapshot: usize) {
    WARNINGS.with(|w| {
        let mut buf = w.borrow_mut();
        if snapshot < buf.len() {
            buf.truncate(snapshot);
        }
    });
}
