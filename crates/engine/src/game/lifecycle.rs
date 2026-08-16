//! Invocation-local observations emitted after delayed-trigger mutations commit.
//!
//! These facts deliberately stay outside game state, public events, and the
//! resolved-rules journal. They are a prospective-simulation sidecar only.

use std::cell::RefCell;

use crate::types::identifiers::{DelayedTriggerOrigin, ObjectId, TriggerFiring};
use crate::types::player::PlayerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImmutableBinding {
    pub(super) source_id: ObjectId,
    pub(super) controller: PlayerId,
}

/// Why a delayed firing left its active carrier without resolving normally.
///
/// The lifecycle sidecar observes this already-settled mutation; it does not
/// implement any rules behavior itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Terminal owners are migrated incrementally through the carrier census.
pub(super) enum DelayedTerminalDisposition {
    Resolved,
    Countered,
    Removed,
    NoLegalChoice,
    InterveningIfFalse,
    AllTargetsIllegal,
    ReflexiveUnmatched,
    CleanupExpired,
    EndTurn,
    Eliminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReducerLifecycleFact {
    Installed {
        origin: DelayedTriggerOrigin,
        binding: ImmutableBinding,
    },
    Due {
        origin: DelayedTriggerOrigin,
        binding: ImmutableBinding,
    },
    Terminal {
        firing: TriggerFiring,
        disposition: DelayedTerminalDisposition,
    },
}

/// Opaque, immutable observations from one successful outer prospective action.
///
/// Only the action boundary can construct this value. Consumers may inspect
/// receipt-relevant facts but cannot create, reorder, or merge observations.
#[derive(Debug, Default)]
pub(super) struct ProspectiveLifecycleFacts {
    facts: Vec<ReducerLifecycleFact>,
}

impl ProspectiveLifecycleFacts {
    pub(super) fn delayed_installations(
        &self,
    ) -> impl Iterator<Item = (DelayedTriggerOrigin, ObjectId, PlayerId)> + '_ {
        self.facts.iter().filter_map(|fact| match fact {
            ReducerLifecycleFact::Installed { origin, binding } => {
                Some((*origin, binding.source_id, binding.controller))
            }
            ReducerLifecycleFact::Due { .. } | ReducerLifecycleFact::Terminal { .. } => None,
        })
    }

    pub(super) fn receipt_finished_normally(&self, origin: DelayedTriggerOrigin) -> bool {
        self.facts.iter().any(|fact| {
            matches!(
                fact,
                ReducerLifecycleFact::Terminal {
                    firing: TriggerFiring::ReceiptEligible(observed),
                    disposition: DelayedTerminalDisposition::Resolved,
                } if *observed == origin
            )
        })
    }

    pub(super) fn receipt_terminalized(&self, origin: DelayedTriggerOrigin) -> bool {
        self.facts.iter().any(|fact| {
            matches!(
                fact,
                ReducerLifecycleFact::Terminal {
                    firing: TriggerFiring::ReceiptEligible(observed),
                    ..
                } if *observed == origin
            )
        })
    }
}

#[derive(Default)]
struct LifecycleFrame {
    facts: Vec<ReducerLifecycleFact>,
}

thread_local! {
    static FRAMES: RefCell<Vec<LifecycleFrame>> = const { RefCell::new(Vec::new()) };
}

/// Opaque ownership of exactly one action-boundary frame.
#[must_use]
pub(super) struct ActionLifecycleGuard {
    depth: usize,
    open: bool,
}

pub(super) fn enter_action_frame() -> ActionLifecycleGuard {
    let depth = FRAMES.with(|frames| {
        let mut frames = frames.borrow_mut();
        let depth = frames.len();
        frames.push(LifecycleFrame::default());
        depth
    });
    ActionLifecycleGuard { depth, open: true }
}

impl ActionLifecycleGuard {
    /// Merges a nested action's facts into its parent. Outermost facts are
    /// intentionally discarded until a prospective consumer explicitly takes
    /// ownership of a completed action's sidecar.
    pub(super) fn commit_into_parent(self) {
        let _ = self.take_outer_facts();
    }

    /// Commits into an enclosing boundary or returns the facts from the one
    /// successful outermost prospective boundary. A nested prospective call
    /// can never drain an ancestor frame.
    pub(super) fn take_outer_facts(mut self) -> Option<ProspectiveLifecycleFacts> {
        let frame = self.take_frame();
        FRAMES.with(|frames| {
            let mut frames = frames.borrow_mut();
            if let Some(parent) = frames.last_mut() {
                parent.facts.extend(frame.facts);
                None
            } else {
                Some(ProspectiveLifecycleFacts { facts: frame.facts })
            }
        })
    }

    pub(super) fn discard(mut self) {
        let _ = self.take_frame();
    }

    fn take_frame(&mut self) -> LifecycleFrame {
        debug_assert!(self.open);
        let frame = FRAMES.with(|frames| {
            let mut frames = frames.borrow_mut();
            debug_assert_eq!(frames.len(), self.depth + 1);
            frames
                .pop()
                .expect("an action lifecycle guard owns its frame")
        });
        self.open = false;
        frame
    }
}

impl Drop for ActionLifecycleGuard {
    fn drop(&mut self) {
        if self.open {
            let _ = self.take_frame();
        }
    }
}

pub(super) fn record_delayed_installed(origin: DelayedTriggerOrigin, binding: ImmutableBinding) {
    append(ReducerLifecycleFact::Installed { origin, binding });
}

pub(super) fn record_delayed_due(origin: DelayedTriggerOrigin, binding: ImmutableBinding) {
    append(ReducerLifecycleFact::Due { origin, binding });
}

pub(super) fn record_delayed_terminal(
    firing: TriggerFiring,
    disposition: DelayedTerminalDisposition,
) {
    if matches!(firing, TriggerFiring::ReceiptEligible(_)) {
        append(ReducerLifecycleFact::Terminal {
            firing,
            disposition,
        });
    }
}

fn append(fact: ReducerLifecycleFact) {
    FRAMES.with(|frames| {
        let mut frames = frames.borrow_mut();
        if let Some(frame) = frames.last_mut() {
            frame.facts.push(fact);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::{DelayedTriggerInstanceId, DelayedTriggerToken};

    fn origin() -> DelayedTriggerOrigin {
        DelayedTriggerOrigin {
            token: DelayedTriggerToken(1),
            instance: DelayedTriggerInstanceId(2),
            source_id: ObjectId(3),
        }
    }

    #[test]
    fn nested_success_merges_into_the_outer_prospective_result() {
        let origin = origin();
        let outer = enter_action_frame();
        record_delayed_installed(
            origin,
            ImmutableBinding {
                source_id: ObjectId(3),
                controller: PlayerId(4),
            },
        );

        let inner = enter_action_frame();
        record_delayed_due(
            origin,
            ImmutableBinding {
                source_id: ObjectId(3),
                controller: PlayerId(4),
            },
        );
        assert!(inner.take_outer_facts().is_none());

        let facts = outer
            .take_outer_facts()
            .expect("the outer frame owns the prospective facts");
        assert_eq!(
            facts.delayed_installations().collect::<Vec<_>>(),
            vec![(origin, ObjectId(3), PlayerId(4))]
        );
        assert!(facts.facts.iter().any(|fact| {
            matches!(
                fact,
                ReducerLifecycleFact::Due {
                    origin: observed,
                    ..
                } if *observed == origin
            )
        }));
    }

    #[test]
    fn discarded_nested_frame_cannot_leak_facts_to_its_parent() {
        let origin = origin();
        let outer = enter_action_frame();
        record_delayed_installed(
            origin,
            ImmutableBinding {
                source_id: ObjectId(3),
                controller: PlayerId(4),
            },
        );

        let inner = enter_action_frame();
        record_delayed_terminal(
            TriggerFiring::ReceiptEligible(origin),
            DelayedTerminalDisposition::Removed,
        );
        inner.discard();

        let facts = outer
            .take_outer_facts()
            .expect("the outer frame owns the prospective facts");
        assert!(!facts.receipt_terminalized(origin));
        assert_eq!(
            facts.delayed_installations().collect::<Vec<_>>(),
            vec![(origin, ObjectId(3), PlayerId(4))]
        );
    }

    #[test]
    fn only_receipt_eligible_terminal_facts_are_observed() {
        let origin = origin();
        let outer = enter_action_frame();
        record_delayed_terminal(
            TriggerFiring::Ordinary,
            DelayedTerminalDisposition::Resolved,
        );
        record_delayed_terminal(
            TriggerFiring::ReceiptEligible(origin),
            DelayedTerminalDisposition::Resolved,
        );

        let facts = outer
            .take_outer_facts()
            .expect("the outer frame owns the prospective facts");
        assert!(facts.receipt_finished_normally(origin));
        assert!(facts.receipt_terminalized(origin));
    }

    #[test]
    fn recording_without_an_action_frame_is_lifecycle_silent() {
        let origin = origin();
        record_delayed_installed(
            origin,
            ImmutableBinding {
                source_id: ObjectId(3),
                controller: PlayerId(4),
            },
        );
        let facts = enter_action_frame()
            .take_outer_facts()
            .expect("the outer frame always returns its own fact collection");
        assert!(
            facts.delayed_installations().next().is_none(),
            "an observation made outside an action boundary cannot leak into a later one"
        );
    }
}
