//! Thread-local "relative-player scope" for the target/controller parser.
//!
//! When a trigger condition introduces an attacked/affected player (e.g.
//! "whenever you attack a player"), follow-on possessive phrases inside the
//! same effect text — "that player controls", "that player owns" — refer to
//! that player, not to the trigger controller. The runtime already has full
//! infrastructure for this case via `ControllerRef::TargetPlayer` (see
//! `game/ability_utils.rs::effect_references_target_player`), but the parser
//! needs a way to know "we're inside a context where 'that player' refers to
//! a target". This module provides a typed RAII guard for that.
//!
//! No bool flags: callers push a typed `ControllerRef` (currently always
//! `ControllerRef::TargetPlayer`) onto the scope and the controller-suffix
//! parser reads it back. The thread-local pattern mirrors `oracle_warnings`.

use std::cell::RefCell;

use crate::types::ability::ControllerRef;

thread_local! {
    /// The active relative-player scope, if any. `Some(ControllerRef::TargetPlayer)`
    /// means "phrases like 'that player controls' should resolve to the
    /// triggering/attacked player rather than the trigger controller."
    static SCOPE: RefCell<Option<ControllerRef>> = const { RefCell::new(None) };
}

/// Read the current relative-player scope.
pub(crate) fn current() -> Option<ControllerRef> {
    SCOPE.with(|s| s.borrow().clone())
}

/// RAII guard that sets the relative-player scope for the lifetime of the
/// guard, restoring the prior value on drop. Nested guards stack correctly.
pub(crate) struct ScopeGuard {
    previous: Option<ControllerRef>,
}

impl ScopeGuard {
    pub(crate) fn new(scope: ControllerRef) -> Self {
        let previous = SCOPE.with(|s| s.replace(Some(scope)));
        Self { previous }
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        SCOPE.with(|s| *s.borrow_mut() = self.previous.take());
    }
}
