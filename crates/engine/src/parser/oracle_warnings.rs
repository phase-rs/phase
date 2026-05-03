//! Thread-local diagnostic accumulator for Oracle text parsing.
//!
//! Typed diagnostics accumulate during parsing and are collected
//! onto OracleDocIr.diagnostics at the end of parse_oracle_ir.

use std::cell::RefCell;

use super::oracle_ir::diagnostic::OracleDiagnostic;

thread_local! {
    static DIAGNOSTICS: RefCell<Vec<OracleDiagnostic>> = const { RefCell::new(Vec::new()) };
}

/// Push a typed diagnostic for the card currently being parsed.
pub fn push_diagnostic(d: OracleDiagnostic) {
    DIAGNOSTICS.with(|v| v.borrow_mut().push(d));
}

/// Drain all accumulated diagnostics (returns them and clears the buffer).
pub fn take_diagnostics() -> Vec<OracleDiagnostic> {
    DIAGNOSTICS.with(|v| v.borrow_mut().drain(..).collect())
}

/// Discard any accumulated diagnostics (called at the start of each card parse).
pub fn clear_diagnostics() {
    DIAGNOSTICS.with(|v| v.borrow_mut().clear());
}

/// Snapshot the current diagnostics buffer length. Pair with `truncate_diagnostics`
/// to roll back any diagnostics emitted during a trial parse that ends up being
/// rejected.
pub fn snapshot_diagnostics() -> usize {
    DIAGNOSTICS.with(|v| v.borrow().len())
}

/// Truncate the diagnostics buffer back to the given snapshot length, discarding
/// any diagnostics pushed since the snapshot. Used for trial-parse rollback.
pub fn truncate_diagnostics(snapshot: usize) {
    DIAGNOSTICS.with(|v| {
        let mut buf = v.borrow_mut();
        if snapshot < buf.len() {
            buf.truncate(snapshot);
        }
    });
}
