//! Typed, serializable suspension frames for ability resolution.
//!
//! This module deliberately models only suspended resolution work. Migrated
//! families use [`ResolutionStack`] as their runtime authority; unmigrated
//! families remain in their legacy `GameState` slots until their Phase-3 turn.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::types::ability::ResolvedAbility;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    DrawSequenceStack, GameState, PendingBatchDeliveries, PendingChangeZoneIteration,
    PendingChooseOneOf, PendingCoinFlip, PendingConniveReentry, PendingContinuation,
    PendingCopyTokenResolution, PendingCounterAdditionQueue, PendingCounterMoveQueue,
    PendingCounterRemovalQueue, PendingEachPlayerCopyChosen, PendingLifeTotalAssignment,
    PendingMutateMerge, PendingPerCategoryZoneChoice, PendingPerPlayerZoneChoice,
    PendingProliferateActions, PendingRepeatIteration, PendingRepeatUntil,
    PendingRepeatedOptionalPayment, PendingSpellResolution, PendingVoteBallotIteration,
    PostReplacementDrainStack, ResolvingTriggerContext, WaitingFor,
};
use crate::types::identifiers::ObjectId;

/// The complete shipped draw authority carried by one `MultiDraw` frame.
///
/// The plan's designed `DrawResolutionState` was never shipped. The actual
/// model is a draw-sequence stack plus the dedicated exact-subject connive
/// re-entry link. General replacement drains stay in their own adjacent
/// `PostReplacement` frame, where a `DrainStatus::Paused` entry proves the
/// parent/child relationship while the draw is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiDrawFrame {
    pub draw_sequences: DrawSequenceStack,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_connive_reentry: Option<PendingConniveReentry>,
}

/// The persisted payload for a parked repeated optional-payment decision.
///
/// The count remains in a legacy runtime register until that family migrates,
/// but it is part of the same resolution lifetime and therefore travels with
/// this frame on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatedOptionalPaymentFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Box<PendingRepeatedOptionalPayment>>,
    pub optional_cost_payments_this_resolution: u32,
}

/// The complete parked optional-effect authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionalEffectFrame {
    pub ability: Box<ResolvedAbility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_event: Option<GameEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_match_count: Option<u32>,
}

/// The ChangeZone owner plus the only sidecar that is not already embedded in
/// `PendingChangeZoneIteration`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeZoneFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingChangeZoneIteration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devour_eligible_snapshot: Option<HashSet<ObjectId>>,
}

/// The complete parked continuation authority.
///
/// `ChooseFromZone` stores its narrow trigger-context sidecar beside the
/// continuation it will drain, not beside the independent per-category
/// iterator. Keeping it here prevents a v1→v2 conversion from dropping that
/// sidecar at a save boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbilityContinuationFrame {
    pub pending: PendingContinuation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choose_zone_trigger_context: Option<ResolvingTriggerContext>,
}

/// The per-category zone-choice owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerCategoryZoneChoiceFrame {
    pub pending: PendingPerCategoryZoneChoice,
}

/// The one place that states every serializable family of suspended
/// resolution work. The variants intentionally mirror the exhaustive census;
/// a new pause family must be added here before it can cross the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ResolutionFrame {
    AbilityContinuation(AbilityContinuationFrame),
    RepeatFor(PendingRepeatIteration),
    RepeatUntil(PendingRepeatUntil),
    RepeatedOptionalPayment(RepeatedOptionalPaymentFrame),
    ChangeZone(Box<ChangeZoneFrame>),
    BatchDelivery(Box<PendingBatchDeliveries>),
    CounterMoves(PendingCounterMoveQueue),
    CounterRemovals(PendingCounterRemovalQueue),
    CounterAdditions(PendingCounterAdditionQueue),
    CopyToken(PendingCopyTokenResolution),
    EachPlayerCopyChosen(PendingEachPlayerCopyChosen),
    ChooseOneOf(PendingChooseOneOf),
    VoteBallot(PendingVoteBallotIteration),
    PerPlayerZoneChoice(PendingPerPlayerZoneChoice),
    PerCategoryZoneChoice(PerCategoryZoneChoiceFrame),
    OptionalEffect(OptionalEffectFrame),
    CoinFlip(PendingCoinFlip),
    Proliferate(PendingProliferateActions),
    MultiDraw(MultiDrawFrame),
    ConniveReentry(PendingConniveReentry),
    LifeTotalAssignment(PendingLifeTotalAssignment),
    SpellResolution(PendingSpellResolution),
    MutateMerge(PendingMutateMerge),
    PostReplacement(PostReplacementDrainStack),
}

/// The discriminant of a [`ResolutionFrame`], used by checked stack
/// transitions without exposing the backing vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameKind {
    AbilityContinuation,
    RepeatFor,
    RepeatUntil,
    RepeatedOptionalPayment,
    ChangeZone,
    BatchDelivery,
    CounterMoves,
    CounterRemovals,
    CounterAdditions,
    CopyToken,
    EachPlayerCopyChosen,
    ChooseOneOf,
    VoteBallot,
    PerPlayerZoneChoice,
    PerCategoryZoneChoice,
    OptionalEffect,
    CoinFlip,
    Proliferate,
    MultiDraw,
    ConniveReentry,
    LifeTotalAssignment,
    SpellResolution,
    MutateMerge,
    PostReplacement,
}

impl ResolutionFrame {
    pub const fn kind(&self) -> FrameKind {
        match self {
            Self::AbilityContinuation(_) => FrameKind::AbilityContinuation,
            Self::RepeatFor(_) => FrameKind::RepeatFor,
            Self::RepeatUntil(_) => FrameKind::RepeatUntil,
            Self::RepeatedOptionalPayment(_) => FrameKind::RepeatedOptionalPayment,
            Self::ChangeZone(_) => FrameKind::ChangeZone,
            Self::BatchDelivery(_) => FrameKind::BatchDelivery,
            Self::CounterMoves(_) => FrameKind::CounterMoves,
            Self::CounterRemovals(_) => FrameKind::CounterRemovals,
            Self::CounterAdditions(_) => FrameKind::CounterAdditions,
            Self::CopyToken(_) => FrameKind::CopyToken,
            Self::EachPlayerCopyChosen(_) => FrameKind::EachPlayerCopyChosen,
            Self::ChooseOneOf(_) => FrameKind::ChooseOneOf,
            Self::VoteBallot(_) => FrameKind::VoteBallot,
            Self::PerPlayerZoneChoice(_) => FrameKind::PerPlayerZoneChoice,
            Self::PerCategoryZoneChoice(_) => FrameKind::PerCategoryZoneChoice,
            Self::OptionalEffect(_) => FrameKind::OptionalEffect,
            Self::CoinFlip(_) => FrameKind::CoinFlip,
            Self::Proliferate(_) => FrameKind::Proliferate,
            Self::MultiDraw(_) => FrameKind::MultiDraw,
            Self::ConniveReentry(_) => FrameKind::ConniveReentry,
            Self::LifeTotalAssignment(_) => FrameKind::LifeTotalAssignment,
            Self::SpellResolution(_) => FrameKind::SpellResolution,
            Self::MutateMerge(_) => FrameKind::MutateMerge,
            Self::PostReplacement(_) => FrameKind::PostReplacement,
        }
    }

    /// Parent continuations wake only after their child has completed. Direct
    /// choice frames are the prompt-owning family and will be checked against
    /// the concrete `WaitingFor` variant by the structural API.
    pub const fn gate(&self) -> FrameGate {
        match self {
            Self::RepeatedOptionalPayment(RepeatedOptionalPaymentFrame {
                pending: Some(_),
                ..
            })
            | Self::OptionalEffect(_) => FrameGate::DirectChoice(DirectChoiceGate::OptionalEffect),
            Self::CoinFlip(_) => FrameGate::DirectChoice(DirectChoiceGate::CoinFlipKeep),
            Self::Proliferate(_) => FrameGate::DirectChoice(DirectChoiceGate::Proliferate),
            Self::MutateMerge(_) => FrameGate::DirectChoice(DirectChoiceGate::MutateMerge),
            Self::AbilityContinuation(_)
            | Self::RepeatFor(_)
            | Self::RepeatUntil(_)
            | Self::RepeatedOptionalPayment(RepeatedOptionalPaymentFrame {
                pending: None, ..
            })
            | Self::ChangeZone(_)
            | Self::BatchDelivery(_)
            | Self::CounterMoves(_)
            | Self::CounterRemovals(_)
            | Self::CounterAdditions(_)
            | Self::CopyToken(_)
            | Self::EachPlayerCopyChosen(_)
            | Self::ChooseOneOf(_)
            | Self::VoteBallot(_)
            | Self::PerPlayerZoneChoice(_)
            | Self::PerCategoryZoneChoice(_)
            | Self::MultiDraw(_)
            | Self::ConniveReentry(_)
            | Self::LifeTotalAssignment(_)
            | Self::SpellResolution(_)
            | Self::PostReplacement(_) => FrameGate::AfterChild,
        }
    }
}

/// Whether the active frame owns the current direct prompt or waits until its
/// inner child returns to a resumable boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameGate {
    DirectChoice(DirectChoiceGate),
    AfterChild,
}

/// A concrete prompt that a direct-choice frame is permitted to consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectChoiceGate {
    OptionalEffect,
    CoinFlipKeep,
    Proliferate,
    MutateMerge,
}

impl DirectChoiceGate {
    const fn matches(self, waiting_for: &WaitingFor) -> bool {
        matches!(
            (self, waiting_for),
            (
                Self::OptionalEffect,
                WaitingFor::OptionalEffectChoice { .. }
            ) | (Self::OptionalEffect, WaitingFor::OpponentMayChoice { .. })
                | (Self::CoinFlipKeep, WaitingFor::CoinFlipKeepChoice { .. })
                | (Self::Proliferate, WaitingFor::ProliferateChoice { .. })
                | (Self::MutateMerge, WaitingFor::MutateMergeChoice { .. })
        )
    }
}

/// A checked structural-stack failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolutionStackError {
    #[error("resolution stack is empty")]
    Empty,
    #[error("resolution stack top is {actual:?}, expected {expected:?}")]
    UnexpectedTop {
        expected: FrameKind,
        actual: FrameKind,
    },
    #[error("a parent frame requires an active child")]
    NoActiveChild,
    #[error(
        "child-stack boundary {child_stack_start} is not below the active child stack of length {stack_len}"
    )]
    InvalidChildBoundary {
        child_stack_start: usize,
        stack_len: usize,
    },
    #[error("top frame {frame:?} does not match waiting prompt {waiting_for}")]
    PromptMismatch {
        frame: FrameKind,
        waiting_for: &'static str,
    },
    #[error("invalid adjacent post-replacement and multi-draw pair: {0}")]
    InvalidAdjacentPair(&'static str),
    #[error("invalid embedded {frame:?} frame: {message}")]
    InvalidPayload { frame: FrameKind, message: String },
}

/// An ordered, LIFO stack of suspended resolution work.
///
/// Its backing storage is intentionally private: all future mutations must
/// pass through the checked structural APIs rather than searching for or
/// removing a non-top parent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolutionStack {
    frames: Vec<ResolutionFrame>,
}

impl ResolutionStack {
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn last(&self) -> Option<&ResolutionFrame> {
        self.frames.last()
    }

    /// Compares runtime frames with the `GameState` equality contract.
    ///
    /// A Devour-only ChangeZone frame preserves a live CR 614.12a/614.13a
    /// eligibility constraint, but that transient snapshot was deliberately
    /// excluded from `GameState` equality before its migration. Keep that
    /// contract: omit a frame with no iteration owner, and compare an owner
    /// without its Devour sidecar. The derived stack equality remains strict for
    /// wire round-trip validation.
    pub(crate) fn game_state_eq(&self, other: &Self) -> bool {
        let mut left = self.frames.iter().filter(|frame| {
            !matches!(
                frame,
                ResolutionFrame::ChangeZone(change_zone) if change_zone.pending.is_none()
            )
        });
        let mut right = other.frames.iter().filter(|frame| {
            !matches!(
                frame,
                ResolutionFrame::ChangeZone(change_zone) if change_zone.pending.is_none()
            )
        });

        loop {
            match (left.next(), right.next()) {
                (None, None) => return true,
                (
                    Some(ResolutionFrame::ChangeZone(left)),
                    Some(ResolutionFrame::ChangeZone(right)),
                ) => {
                    if left.pending != right.pending {
                        return false;
                    }
                }
                (Some(left), Some(right)) if left == right => {}
                (Some(_), Some(_)) | (Some(_), None) | (None, Some(_)) => return false,
            }
        }
    }

    /// Returns the active ability-continuation owner, if that family owns the
    /// stack top. This is deliberately a top-only view; callers must not use
    /// it to recover a buried parent continuation.
    pub fn active_ability_continuation(&self) -> Option<&AbilityContinuationFrame> {
        match self.last() {
            Some(ResolutionFrame::AbilityContinuation(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable counterpart to [`Self::active_ability_continuation`].
    ///
    /// It exposes only the active typed payload, never an arbitrary frame.
    pub fn active_ability_continuation_mut(&mut self) -> Option<&mut AbilityContinuationFrame> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::AbilityContinuation(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume the active ability-continuation frame.
    pub fn take_active_ability_continuation(
        &mut self,
    ) -> Result<Option<AbilityContinuationFrame>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::AbilityContinuation(_)) => {
                let ResolutionFrame::AbilityContinuation(frame) =
                    self.pop_expected(FrameKind::AbilityContinuation)?
                else {
                    unreachable!("checked ability-continuation frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::AbilityContinuation,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a newly active ability continuation.
    pub fn push_ability_continuation(&mut self, frame: AbilityContinuationFrame) {
        self.push_inner(ResolutionFrame::AbilityContinuation(frame));
    }

    /// Re-park the currently active ability continuation without an
    /// empty-stack interval.
    pub fn replace_active_ability_continuation(
        &mut self,
        frame: AbilityContinuationFrame,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::AbilityContinuation(_)) => {
                self.replace_active(ResolutionFrame::AbilityContinuation(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::AbilityContinuation,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the active repeat-for iteration owner when it owns the stack
    /// top. Repeat consumers must never search below a nested continuation.
    pub fn active_repeat_for(&self) -> Option<&PendingRepeatIteration> {
        match self.last() {
            Some(ResolutionFrame::RepeatFor(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active repeat-for iteration owner.
    pub fn active_repeat_for_mut(&mut self) -> Option<&mut PendingRepeatIteration> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::RepeatFor(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active repeat-for iteration frame.
    pub fn take_active_repeat_for(
        &mut self,
    ) -> Result<Option<PendingRepeatIteration>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::RepeatFor(_)) => {
                let ResolutionFrame::RepeatFor(frame) = self.pop_expected(FrameKind::RepeatFor)?
                else {
                    unreachable!("checked repeat-for frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::RepeatFor,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a newly active repeat-for iteration.
    pub fn push_repeat_for(&mut self, frame: PendingRepeatIteration) {
        self.push_inner(ResolutionFrame::RepeatFor(frame));
    }

    /// Re-park the active repeat-for iteration without an empty-stack interval.
    pub fn replace_active_repeat_for(
        &mut self,
        frame: PendingRepeatIteration,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::RepeatFor(_)) => {
                self.replace_active(ResolutionFrame::RepeatFor(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::RepeatFor,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the repeat-until owner only when it owns the stack top.
    pub fn active_repeat_until(&self) -> Option<&PendingRepeatUntil> {
        match self.last() {
            Some(ResolutionFrame::RepeatUntil(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Returns the complete ChangeZone owner only when it owns the stack top.
    ///
    /// The frame retains the complete logical zone-change group and any
    /// Devour snapshot together, so no caller can resume a partial carrier.
    pub fn active_change_zone(&self) -> Option<&ChangeZoneFrame> {
        match self.last() {
            Some(ResolutionFrame::ChangeZone(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable top-only access to the complete ChangeZone owner.
    pub fn active_change_zone_mut(&mut self) -> Option<&mut ChangeZoneFrame> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::ChangeZone(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active ChangeZone owner.
    pub fn take_active_change_zone(
        &mut self,
    ) -> Result<Option<ChangeZoneFrame>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::ChangeZone(_)) => {
                let ResolutionFrame::ChangeZone(frame) =
                    self.pop_expected(FrameKind::ChangeZone)?
                else {
                    unreachable!("checked ChangeZone frame kind must match")
                };
                Ok(Some(*frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::ChangeZone,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a complete ChangeZone owner as the active inner frame.
    pub fn push_change_zone(&mut self, frame: ChangeZoneFrame) {
        self.push_inner(ResolutionFrame::ChangeZone(Box::new(frame)));
    }

    /// Re-park the active ChangeZone owner after another replacement pause.
    pub fn replace_active_change_zone(
        &mut self,
        frame: ChangeZoneFrame,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::ChangeZone(_)) => {
                self.replace_active(ResolutionFrame::ChangeZone(Box::new(frame)))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::ChangeZone,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the complete BatchDelivery owner only when it owns the stack
    /// top. Its logical zone-change group, paused delivery, and undelivered
    /// tail remain one payload across every replacement boundary.
    pub fn active_batch_delivery(&self) -> Option<&PendingBatchDeliveries> {
        match self.last() {
            Some(ResolutionFrame::BatchDelivery(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable top-only access to the complete BatchDelivery owner.
    pub fn active_batch_delivery_mut(&mut self) -> Option<&mut PendingBatchDeliveries> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::BatchDelivery(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active BatchDelivery owner after its logical group
    /// settles once.
    pub fn take_active_batch_delivery(
        &mut self,
    ) -> Result<Option<PendingBatchDeliveries>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::BatchDelivery(_)) => {
                let ResolutionFrame::BatchDelivery(frame) =
                    self.pop_expected(FrameKind::BatchDelivery)?
                else {
                    unreachable!("checked batch-delivery frame kind must match")
                };
                Ok(Some(*frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::BatchDelivery,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a complete BatchDelivery owner as the active inner frame.
    pub fn push_batch_delivery(&mut self, pending: PendingBatchDeliveries) {
        self.push_inner(ResolutionFrame::BatchDelivery(Box::new(pending)));
    }

    /// Re-park the active BatchDelivery owner after another replacement pause.
    pub fn replace_active_batch_delivery(
        &mut self,
        pending: PendingBatchDeliveries,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::BatchDelivery(_)) => {
                self.replace_active(ResolutionFrame::BatchDelivery(Box::new(pending)))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::BatchDelivery,
                actual: frame.kind(),
            }),
        }
    }

    /// Mutably accesses only the active repeat-until owner.
    pub fn active_repeat_until_mut(&mut self) -> Option<&mut PendingRepeatUntil> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::RepeatUntil(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active repeat-until frame.
    pub fn take_active_repeat_until(
        &mut self,
    ) -> Result<Option<PendingRepeatUntil>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::RepeatUntil(_)) => {
                let ResolutionFrame::RepeatUntil(frame) =
                    self.pop_expected(FrameKind::RepeatUntil)?
                else {
                    unreachable!("checked repeat-until frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::RepeatUntil,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a newly active repeat-until owner.
    pub fn push_repeat_until(&mut self, frame: PendingRepeatUntil) {
        self.push_inner(ResolutionFrame::RepeatUntil(frame));
    }

    /// Replace the active repeat-until owner after it re-pauses.
    pub fn replace_active_repeat_until(
        &mut self,
        frame: PendingRepeatUntil,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::RepeatUntil(_)) => {
                self.replace_active(ResolutionFrame::RepeatUntil(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::RepeatUntil,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the multi-player choose-one owner only when it owns the stack
    /// top.
    pub fn active_choose_one_of(&self) -> Option<&PendingChooseOneOf> {
        match self.last() {
            Some(ResolutionFrame::ChooseOneOf(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active choose-one-of frame.
    pub fn take_active_choose_one_of(
        &mut self,
    ) -> Result<Option<PendingChooseOneOf>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::ChooseOneOf(_)) => {
                let ResolutionFrame::ChooseOneOf(frame) =
                    self.pop_expected(FrameKind::ChooseOneOf)?
                else {
                    unreachable!("checked choose-one-of frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::ChooseOneOf,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a multi-player choose-one-of frame below its selected branch.
    pub fn push_choose_one_of(&mut self, frame: PendingChooseOneOf) {
        self.push_inner(ResolutionFrame::ChooseOneOf(frame));
    }

    /// Returns the vote-ballot owner only when it owns the stack top.
    pub fn active_vote_ballot(&self) -> Option<&PendingVoteBallotIteration> {
        match self.last() {
            Some(ResolutionFrame::VoteBallot(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active vote-ballot frame.
    pub fn take_active_vote_ballot(
        &mut self,
    ) -> Result<Option<PendingVoteBallotIteration>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::VoteBallot(_)) => {
                let ResolutionFrame::VoteBallot(frame) =
                    self.pop_expected(FrameKind::VoteBallot)?
                else {
                    unreachable!("checked vote-ballot frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::VoteBallot,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a paused per-ballot vote iteration.
    pub fn push_vote_ballot(&mut self, frame: PendingVoteBallotIteration) {
        self.push_inner(ResolutionFrame::VoteBallot(frame));
    }

    /// Returns the per-player zone-choice owner only when it owns the stack top.
    pub fn active_per_player_zone_choice(&self) -> Option<&PendingPerPlayerZoneChoice> {
        match self.last() {
            Some(ResolutionFrame::PerPlayerZoneChoice(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active per-player zone-choice frame.
    pub fn take_active_per_player_zone_choice(
        &mut self,
    ) -> Result<Option<PendingPerPlayerZoneChoice>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::PerPlayerZoneChoice(_)) => {
                let ResolutionFrame::PerPlayerZoneChoice(frame) =
                    self.pop_expected(FrameKind::PerPlayerZoneChoice)?
                else {
                    unreachable!("checked per-player zone-choice frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::PerPlayerZoneChoice,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a per-player zone-choice iteration.
    pub fn push_per_player_zone_choice(&mut self, frame: PendingPerPlayerZoneChoice) {
        self.push_inner(ResolutionFrame::PerPlayerZoneChoice(frame));
    }

    /// Returns the per-category zone-choice owner only when it owns the stack top.
    pub fn active_per_category_zone_choice(&self) -> Option<&PendingPerCategoryZoneChoice> {
        match self.last() {
            Some(ResolutionFrame::PerCategoryZoneChoice(frame)) => Some(&frame.pending),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active per-category zone-choice frame.
    pub fn take_active_per_category_zone_choice(
        &mut self,
    ) -> Result<Option<PendingPerCategoryZoneChoice>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::PerCategoryZoneChoice(_)) => {
                let ResolutionFrame::PerCategoryZoneChoice(frame) =
                    self.pop_expected(FrameKind::PerCategoryZoneChoice)?
                else {
                    unreachable!("checked per-category zone-choice frame kind must match")
                };
                Ok(Some(frame.pending))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::PerCategoryZoneChoice,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a per-category zone-choice iteration.
    pub fn push_per_category_zone_choice(&mut self, pending: PendingPerCategoryZoneChoice) {
        self.push_inner(ResolutionFrame::PerCategoryZoneChoice(
            PerCategoryZoneChoiceFrame { pending },
        ));
    }

    /// Returns only the immediate predecessor of the active frame.
    ///
    /// This is intentionally narrower than a frame search: it serves only the
    /// paused-drain/draw adjacency.
    pub fn active_predecessor(&self) -> Option<&ResolutionFrame> {
        self.frames.get(self.frames.len().checked_sub(2)?)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ResolutionFrame> {
        self.frames.iter()
    }

    /// Park work that is inside the current active operation.
    pub fn push_inner(&mut self, frame: ResolutionFrame) {
        self.frames.push(frame);
    }

    /// Install an outer continuation immediately below the active child.
    ///
    /// There is deliberately no fallback insertion position: callers that do
    /// not have an active child must first trace the real nesting relationship.
    pub fn insert_parent_of_active(
        &mut self,
        frame: ResolutionFrame,
    ) -> Result<(), ResolutionStackError> {
        let active_index = self
            .frames
            .len()
            .checked_sub(1)
            .ok_or(ResolutionStackError::NoActiveChild)?;
        self.frames.insert(active_index, frame);
        Ok(())
    }

    /// Install an outer frame immediately below the child stack a producer
    /// created after recording its pre-resolution boundary.
    ///
    /// This is structural insertion, not a frame search: the caller supplies
    /// the exact stack depth observed before it invoked the child producer.
    /// The boundary must therefore precede at least one currently active child
    /// frame; a producer with no child parks its frame normally.
    pub fn insert_parent_at_child_boundary(
        &mut self,
        frame: ResolutionFrame,
        child_stack_start: usize,
    ) -> Result<(), ResolutionStackError> {
        let stack_len = self.frames.len();
        if stack_len == 0 {
            return Err(ResolutionStackError::NoActiveChild);
        }
        if child_stack_start >= stack_len {
            return Err(ResolutionStackError::InvalidChildBoundary {
                child_stack_start,
                stack_len,
            });
        }
        self.frames.insert(child_stack_start, frame);
        Ok(())
    }

    /// Consume exactly the active frame expected by one direct prompt handler.
    pub fn pop_expected(
        &mut self,
        expected: FrameKind,
    ) -> Result<ResolutionFrame, ResolutionStackError> {
        let actual = self
            .frames
            .last()
            .map(ResolutionFrame::kind)
            .ok_or(ResolutionStackError::Empty)?;
        if actual != expected {
            return Err(ResolutionStackError::UnexpectedTop { expected, actual });
        }
        Ok(self
            .frames
            .pop()
            .expect("checked resolution stack top must still be present"))
    }

    /// Re-park the current operation without exposing an empty-stack interval.
    pub fn replace_active(&mut self, frame: ResolutionFrame) -> Result<(), ResolutionStackError> {
        let active = self.frames.last_mut().ok_or(ResolutionStackError::Empty)?;
        *active = frame;
        Ok(())
    }

    /// Atomically install the shipped general-drain/draw pair.
    ///
    /// The semantic edge is positional: a paused resident drain must be the
    /// immediate predecessor of the active draw sequence. No designed drain or
    /// draw reference is reconstructed, and neither half is installed on a
    /// failed validation.
    pub fn install_adjacent_post_replacement_draw(
        &mut self,
        parent: ResolutionFrame,
        child: ResolutionFrame,
    ) -> Result<(), ResolutionStackError> {
        validate_shipped_post_replacement_draw_pair(&parent, &child)?;
        self.frames.push(parent);
        self.frames.push(child);
        Ok(())
    }

    /// Consume only the active child of an adjacent shipped drain/draw pair.
    ///
    /// The paused drain remains resident and is retired by the existing typed
    /// dispatch handle after the resumed continuation finishes. This method
    /// examines only the top and immediate predecessor; it never searches for a
    /// non-top parent.
    pub fn complete_adjacent_post_replacement_draw(
        &mut self,
    ) -> Result<ResolutionFrame, ResolutionStackError> {
        let child_index = self
            .frames
            .len()
            .checked_sub(1)
            .ok_or(ResolutionStackError::Empty)?;
        let parent_index =
            child_index
                .checked_sub(1)
                .ok_or(ResolutionStackError::InvalidAdjacentPair(
                    "a multi-draw child has no immediate post-replacement predecessor",
                ))?;
        validate_shipped_post_replacement_draw_pair(
            &self.frames[parent_index],
            &self.frames[child_index],
        )?;
        Ok(self
            .frames
            .pop()
            .expect("checked resolution child must be present"))
    }

    /// Validate stack-local structural and prompt coherence invariants.
    pub fn validate(&self, waiting_for: &WaitingFor) -> Result<(), ResolutionStackError> {
        let has_multi_draw = self
            .frames
            .iter()
            .any(|frame| matches!(frame, ResolutionFrame::MultiDraw(_)));
        for (index, frame) in self.frames.iter().enumerate() {
            if let ResolutionFrame::MultiDraw(draw) = frame {
                draw.draw_sequences.validate().map_err(|message| {
                    ResolutionStackError::InvalidPayload {
                        frame: FrameKind::MultiDraw,
                        message,
                    }
                })?;
            }
            if has_multi_draw
                && matches!(
                    frame,
                    ResolutionFrame::PostReplacement(drains)
                        if matches!(
                            drains.resident().map(|drain| &drain.status),
                            Some(crate::types::game_state::DrainStatus::Paused)
                        )
                )
            {
                let child =
                    self.frames
                        .get(index + 1)
                        .ok_or(ResolutionStackError::InvalidAdjacentPair(
                            "a paused post-replacement drain has no immediate multi-draw child",
                        ))?;
                validate_shipped_post_replacement_draw_pair(frame, child)?;
                if index + 2 != self.frames.len() {
                    return Err(ResolutionStackError::InvalidAdjacentPair(
                        "a paired multi-draw child is not the active stack top",
                    ));
                }
            }
        }

        let Some(top) = self.frames.last() else {
            return Ok(());
        };
        if let FrameGate::DirectChoice(gate) = top.gate() {
            if !gate.matches(waiting_for) {
                return Err(ResolutionStackError::PromptMismatch {
                    frame: top.kind(),
                    waiting_for: waiting_for.variant_name(),
                });
            }
        }
        Ok(())
    }
}

/// The full-state resolution wire protocol version that carries only typed
/// [`ResolutionStack`] frames for paused resolution work.
pub const RESOLUTION_STATE_WIRE_VERSION: u64 = 2;
const LEGACY_RESOLUTION_STATE_WIRE_VERSION: u64 = 1;

/// Versioned wire adapter for full game-state persistence and transport.
///
/// This adapter is the persistence seam between v1's legacy-only payloads and
/// v2's typed frames. v1 decoding converts migrated family payloads into the
/// runtime stack; v2 decoding restores those frames directly while retaining
/// legacy slots solely for unmigrated families.
#[derive(Debug, Clone)]
pub struct ResolutionStateWire {
    state: GameState,
}

impl ResolutionStateWire {
    pub fn from_game_state(state: GameState) -> Self {
        Self { state }
    }

    pub fn into_game_state(self) -> GameState {
        self.state
    }

    pub fn game_state(&self) -> &GameState {
        &self.state
    }

    fn to_value(&self) -> Result<Value, String> {
        let frames = canonicalize_legacy_resolution_state(&self.state)?;
        frames
            .validate(&self.state.waiting_for)
            .map_err(|error| error.to_string())?;

        let mut value = serde_json::to_value(&self.state).map_err(|error| error.to_string())?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| "GameState must serialize as a JSON object".to_string())?;
        object.remove("resolution_stack");
        remove_resolution_wire_fields(object);
        object.insert(
            "resolution_state_version".to_string(),
            Value::from(RESOLUTION_STATE_WIRE_VERSION),
        );
        object.insert(
            "resolution_frames".to_string(),
            serde_json::to_value(frames).map_err(|error| error.to_string())?,
        );
        Ok(value)
    }

    fn from_value(value: Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "resolution state wire must be a JSON object".to_string())?;
        let version = object
            .get("resolution_state_version")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                "resolution state wire is missing a numeric resolution_state_version".to_string()
            })?;

        match version {
            LEGACY_RESOLUTION_STATE_WIRE_VERSION => {
                if object.contains_key("resolution_frames") {
                    return Err("v1 resolution state must not contain resolution_frames".to_string());
                }
                if object.contains_key("resolution_stack") {
                    return Err("v1 resolution state must not contain resolution_stack".to_string());
                }
                let legacy_ability = LegacyAbilityContinuationWire::from_value(&value)?;
                let legacy_repeat_for = LegacyRepeatForWire::from_value(&value)?;
                let legacy_repeat_until = LegacyRepeatUntilWire::from_value(&value)?;
                let legacy_change_zone = LegacyChangeZoneWire::from_value(&value)?;
                let legacy_batch_delivery = LegacyBatchDeliveryWire::from_value(&value)?;
                let legacy_choose_one_of = LegacyChooseOneOfWire::from_value(&value)?;
                let legacy_vote_ballot = LegacyVoteBallotWire::from_value(&value)?;
                let legacy_per_player_zone_choice =
                    LegacyPerPlayerZoneChoiceWire::from_value(&value)?;
                let legacy_per_category_zone_choice =
                    LegacyPerCategoryZoneChoiceWire::from_value(&value)?;
                let mut legacy_value = value;
                let legacy_object = legacy_value
                    .as_object_mut()
                    .expect("checked JSON object");
                legacy_object.remove("pending_continuation");
                legacy_object.remove("search_continuation_attach_host");
                legacy_object.remove("pending_choose_zone_trigger_context");
                legacy_object.remove("pending_repeat_iteration");
                legacy_object.remove("pending_repeat_until");
                legacy_object.remove("pending_change_zone_iteration");
                legacy_object.remove("devour_eligible_snapshot");
                legacy_object.remove("pending_batch_deliveries");
                legacy_object.remove("pending_mill_deliveries");
                legacy_object.remove("pending_choose_one_of");
                legacy_object.remove("pending_vote_ballot_iteration");
                legacy_object.remove("pending_per_player_zone_choice");
                legacy_object.remove("pending_per_category_zone_choice");
                let mut legacy: GameState =
                    serde_json::from_value(legacy_value).map_err(|error| error.to_string())?;
                if let Some(frame) = legacy_ability.into_frame()? {
                    legacy.push_ability_continuation(frame);
                }
                if let Some(frame) = legacy_repeat_for.into_frame() {
                    legacy.push_repeat_for(frame);
                }
                if let Some(frame) = legacy_repeat_until.into_frame() {
                    legacy.push_repeat_until(frame);
                }
                if let Some(frame) = legacy_change_zone.into_frame() {
                    legacy.push_change_zone_frame(frame);
                }
                if let Some(frame) = legacy_batch_delivery.into_frame() {
                    legacy.push_batch_delivery(frame);
                }
                if let Some(frame) = legacy_choose_one_of.into_frame() {
                    legacy.push_choose_one_of(frame);
                }
                if let Some(frame) = legacy_vote_ballot.into_frame() {
                    legacy.push_vote_ballot(frame);
                }
                if let Some(frame) = legacy_per_player_zone_choice.into_frame() {
                    legacy.push_per_player_zone_choice(frame);
                }
                if let Some(frame) = legacy_per_category_zone_choice.into_frame() {
                    legacy.push_per_category_zone_choice(frame);
                }
                legacy.migrate_post_replacement_continuation();
                legacy.migrate_pending_multi_draw();
                let frames = canonicalize_legacy_resolution_state(&legacy)?;
                frames
                    .validate(&legacy.waiting_for)
                    .map_err(|error| error.to_string())?;
                let state = project_frames_into_legacy_state(&legacy, &frames)?;
                #[cfg(debug_assertions)]
                debug_assert_runtime_resolution_invariants(&state);
                Ok(Self { state })
            }
            RESOLUTION_STATE_WIRE_VERSION => {
                if legacy_resolution_wire_field(object).is_some() {
                    return Err("v2 resolution state must not contain a legacy resolution field".to_string());
                }
                let frames_value = object
                    .get("resolution_frames")
                    .ok_or_else(|| "v2 resolution state is missing resolution_frames".to_string())?;
                let frames: ResolutionStack = serde_json::from_value(frames_value.clone())
                    .map_err(|error| error.to_string())?;

                let mut state_value = value;
                let state_object = state_value.as_object_mut().expect("checked JSON object");
                state_object.remove("resolution_state_version");
                state_object.remove("resolution_frames");
                if state_object.remove("resolution_stack").is_some() {
                    return Err("v2 resolution state must not contain runtime resolution_stack"
                        .to_string());
                }
                let state: GameState =
                    serde_json::from_value(state_value).map_err(|error| error.to_string())?;
                frames
                    .validate(&state.waiting_for)
                    .map_err(|error| error.to_string())?;
                let projected = project_frames_into_legacy_state(&state, &frames)?;
                let canonical = canonicalize_legacy_resolution_state(&projected)?;
                if canonical != frames {
                    return Err("v2 resolution frames cannot be represented by the legacy runtime slots"
                        .to_string());
                }
                #[cfg(debug_assertions)]
                debug_assert_runtime_resolution_invariants(&projected);
                Ok(Self { state: projected })
            }
            other => Err(format!(
                "unsupported resolution_state_version {other}; expected 1 or {RESOLUTION_STATE_WIRE_VERSION}"
            )),
        }
    }
}

impl Serialize for ResolutionStateWire {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResolutionStateWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::from_value(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Checks the Phase-3 runtime invariants after a restore and after every
/// public action. Migrated families are authoritative in `ResolutionStack`;
/// canonicalization combines those with unmigrated legacy families for the
/// v2 wire boundary. Valid in-flight trigger occurrences may intentionally be
/// non-serializable and are checked only structurally at this boundary.
#[cfg(debug_assertions)]
pub(crate) fn debug_assert_runtime_resolution_invariants(state: &GameState) {
    let frames = canonicalize_legacy_resolution_state(state)
        .unwrap_or_else(|error| panic!("resolution state must canonicalize: {error}"));
    frames
        .validate(&state.waiting_for)
        .unwrap_or_else(|error| panic!("canonical resolution frames must validate: {error}"));
    assert!(
        state.pending_taps_for_mana_overrides.is_empty(),
        "inline-mana override entries must not survive a public boundary"
    );
    assert!(
        state.current_triggered_mana_override.is_none(),
        "the active inline-mana override must not survive a public boundary"
    );

    if let Ok(v2) = ResolutionStateWire::from_game_state(state.clone()).to_value() {
        let object = v2
            .as_object()
            .expect("resolution wire serialization is always an object");
        for field in legacy_resolution_wire_fields() {
            assert!(
                !object.contains_key(*field),
                "v2 resolution frames must not co-reside with legacy runtime field {field}"
            );
        }
    }
}

enum DrawAndPostConversion {
    Paired {
        parent: ResolutionFrame,
        child: Box<ResolutionFrame>,
    },
    UnpairedDraw(ResolutionFrame),
    UnpairedPost(ResolutionFrame),
}

/// v1-only continuation fields. Runtime state never carries these names after
/// the AbilityContinuation migration; this adapter is the sole reader for
/// historical saves.
#[derive(Deserialize)]
struct LegacyAbilityContinuationWire {
    #[serde(default)]
    pending_continuation: Option<PendingContinuation>,
    #[serde(default)]
    search_continuation_attach_host: Option<crate::game::game_object::AttachTarget>,
    #[serde(default)]
    pending_choose_zone_trigger_context: Option<ResolvingTriggerContext>,
}

impl LegacyAbilityContinuationWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Result<Option<AbilityContinuationFrame>, String> {
        let Some(mut pending) = self.pending_continuation else {
            if self.search_continuation_attach_host.is_some()
                || self.pending_choose_zone_trigger_context.is_some()
            {
                return Err(
                    "legacy ability-continuation sidecar has no continuation owner".to_string(),
                );
            }
            return Ok(None);
        };

        if let Some(host) = self.search_continuation_attach_host {
            match pending.search_attach_host {
                Some(existing) if existing != host => {
                    return Err("legacy ability-continuation attach hosts disagree".to_string());
                }
                Some(_) => {}
                None => pending.search_attach_host = Some(host),
            }
        }

        Ok(Some(AbilityContinuationFrame {
            pending,
            choose_zone_trigger_context: self.pending_choose_zone_trigger_context,
        }))
    }
}

/// v1-only repeat-for field. Runtime state carries it only as a typed frame.
#[derive(Deserialize)]
struct LegacyRepeatForWire {
    #[serde(default)]
    pending_repeat_iteration: Option<PendingRepeatIteration>,
}

/// v1-only repeat-until field. Runtime state carries it only as a typed frame.
#[derive(Deserialize)]
struct LegacyRepeatUntilWire {
    #[serde(default)]
    pending_repeat_until: Option<PendingRepeatUntil>,
}

/// v1-only ChangeZone fields. The complete logical-zone owner and its Devour
/// snapshot migrated together; runtime state never carries either field.
#[derive(Deserialize)]
struct LegacyChangeZoneWire {
    #[serde(default)]
    pending_change_zone_iteration: Option<PendingChangeZoneIteration>,
    #[serde(default)]
    devour_eligible_snapshot: Option<HashSet<ObjectId>>,
}

impl LegacyChangeZoneWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<ChangeZoneFrame> {
        (self.pending_change_zone_iteration.is_some() || self.devour_eligible_snapshot.is_some())
            .then_some(ChangeZoneFrame {
                pending: self.pending_change_zone_iteration,
                devour_eligible_snapshot: self.devour_eligible_snapshot,
            })
    }
}

/// v1-only BatchDelivery field. Runtime state carries the complete logical
/// owner only in its typed frame.
#[derive(Deserialize)]
struct LegacyBatchDeliveryWire {
    #[serde(default, alias = "pending_mill_deliveries")]
    pending_batch_deliveries: Option<PendingBatchDeliveries>,
}

impl LegacyBatchDeliveryWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingBatchDeliveries> {
        self.pending_batch_deliveries
    }
}

/// v1-only choose-one-of field. Runtime state carries it only as a typed frame.
#[derive(Deserialize)]
struct LegacyChooseOneOfWire {
    #[serde(default)]
    pending_choose_one_of: Option<PendingChooseOneOf>,
}

/// v1-only vote-ballot field. Runtime state carries it only as a typed frame.
#[derive(Deserialize)]
struct LegacyVoteBallotWire {
    #[serde(default)]
    pending_vote_ballot_iteration: Option<PendingVoteBallotIteration>,
}

/// v1-only per-player zone-choice field. Runtime state carries it only as a typed frame.
#[derive(Deserialize)]
struct LegacyPerPlayerZoneChoiceWire {
    #[serde(default)]
    pending_per_player_zone_choice: Option<PendingPerPlayerZoneChoice>,
}

/// v1-only per-category zone-choice field. Runtime state carries it only as a typed frame.
#[derive(Deserialize)]
struct LegacyPerCategoryZoneChoiceWire {
    #[serde(default)]
    pending_per_category_zone_choice: Option<PendingPerCategoryZoneChoice>,
}

impl LegacyPerCategoryZoneChoiceWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingPerCategoryZoneChoice> {
        self.pending_per_category_zone_choice
    }
}

impl LegacyPerPlayerZoneChoiceWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingPerPlayerZoneChoice> {
        self.pending_per_player_zone_choice
    }
}

impl LegacyVoteBallotWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingVoteBallotIteration> {
        self.pending_vote_ballot_iteration
    }
}

impl LegacyChooseOneOfWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingChooseOneOf> {
        self.pending_choose_one_of
    }
}

impl LegacyRepeatUntilWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingRepeatUntil> {
        self.pending_repeat_until
    }
}

impl LegacyRepeatForWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingRepeatIteration> {
        self.pending_repeat_iteration
    }
}

pub(crate) fn canonicalize_legacy_resolution_state(
    state: &GameState,
) -> Result<ResolutionStack, String> {
    if state.pending_optional_effect.is_none()
        && (state.pending_optional_trigger_event.is_some()
            || state.pending_optional_trigger_match_count.is_some())
    {
        return Err(
            "pending optional-effect trigger context has no optional-effect owner".to_string(),
        );
    }
    let mut frames = ResolutionStack::default();

    for frame in state.resolution_stack.iter() {
        if !matches!(
            frame,
            ResolutionFrame::AbilityContinuation(_)
                | ResolutionFrame::RepeatFor(_)
                | ResolutionFrame::RepeatUntil(_)
                | ResolutionFrame::ChangeZone(_)
                | ResolutionFrame::BatchDelivery(_)
                | ResolutionFrame::ChooseOneOf(_)
                | ResolutionFrame::VoteBallot(_)
                | ResolutionFrame::PerPlayerZoneChoice(_)
                | ResolutionFrame::PerCategoryZoneChoice(_)
        ) {
            return Err(format!(
                "runtime resolution stack contains unmigrated {:?} frame",
                frame.kind()
            ));
        }
        frames.push_inner(frame.clone());
    }
    push_legacy_after_child_frames(state, &mut frames);
    if let Some(conversion) = classify_draw_and_post_replacement(state)? {
        // The paired branch is deliberately first. Once a paused resident drain
        // proves the adjacency relationship, neither unpaired converter may run.
        match conversion {
            DrawAndPostConversion::Paired { parent, child } => frames
                .install_adjacent_post_replacement_draw(parent, *child)
                .map_err(|error| error.to_string())?,
            DrawAndPostConversion::UnpairedDraw(frame) => frames.push_inner(frame),
            DrawAndPostConversion::UnpairedPost(frame) => frames.push_inner(frame),
        }
    }
    if state.draw_sequences.is_empty() {
        if let Some(pending) = state.pending_connive_reentry.clone() {
            frames.push_inner(ResolutionFrame::ConniveReentry(pending));
        }
    }
    push_legacy_direct_choice_frames(state, &mut frames)?;
    Ok(frames)
}

fn push_legacy_after_child_frames(state: &GameState, frames: &mut ResolutionStack) {
    if let Some(pending) = state.pending_counter_moves.clone() {
        frames.push_inner(ResolutionFrame::CounterMoves(pending));
    }
    if let Some(pending) = state.pending_counter_removals.clone() {
        frames.push_inner(ResolutionFrame::CounterRemovals(pending));
    }
    if let Some(pending) = state.pending_counter_additions.clone() {
        frames.push_inner(ResolutionFrame::CounterAdditions(pending));
    }
    if let Some(pending) = state.pending_copy_token_resolution.clone() {
        frames.push_inner(ResolutionFrame::CopyToken(pending));
    }
    if let Some(pending) = state.pending_each_player_copy_chosen.clone() {
        frames.push_inner(ResolutionFrame::EachPlayerCopyChosen(pending));
    }
    if let Some(pending) = state.pending_life_total_assignment.clone() {
        frames.push_inner(ResolutionFrame::LifeTotalAssignment(pending));
    }
    if let Some(pending) = state.pending_spell_resolution.clone() {
        frames.push_inner(ResolutionFrame::SpellResolution(pending));
    }
}

fn push_legacy_direct_choice_frames(
    state: &GameState,
    frames: &mut ResolutionStack,
) -> Result<(), String> {
    let mut direct_choice_count = 0;
    if state.pending_repeated_optional_payment.is_some()
        || state.optional_cost_payments_this_resolution != 0
    {
        direct_choice_count += 1;
        frames.push_inner(ResolutionFrame::RepeatedOptionalPayment(
            RepeatedOptionalPaymentFrame {
                pending: state.pending_repeated_optional_payment.clone(),
                optional_cost_payments_this_resolution: state
                    .optional_cost_payments_this_resolution,
            },
        ));
    }
    if let Some(ability) = state.pending_optional_effect.clone() {
        direct_choice_count += 1;
        frames.push_inner(ResolutionFrame::OptionalEffect(OptionalEffectFrame {
            ability,
            trigger_event: state.pending_optional_trigger_event.clone(),
            trigger_match_count: state.pending_optional_trigger_match_count,
        }));
    }
    if let Some(pending) = state.pending_coin_flip.clone() {
        direct_choice_count += 1;
        frames.push_inner(ResolutionFrame::CoinFlip(pending));
    }
    if let Some(pending) = state.pending_proliferate_actions.clone() {
        direct_choice_count += 1;
        frames.push_inner(ResolutionFrame::Proliferate(pending));
    }
    if let Some(pending) = state.pending_mutate_merge.clone() {
        direct_choice_count += 1;
        frames.push_inner(ResolutionFrame::MutateMerge(pending));
    }
    if direct_choice_count > 1 {
        return Err("legacy resolution state has multiple direct-choice owners".to_string());
    }
    Ok(())
}

fn classify_draw_and_post_replacement(
    state: &GameState,
) -> Result<Option<DrawAndPostConversion>, String> {
    let draw = (!state.draw_sequences.is_empty()).then(|| {
        ResolutionFrame::MultiDraw(MultiDrawFrame {
            draw_sequences: state.draw_sequences.clone(),
            pending_connive_reentry: state.pending_connive_reentry.clone(),
        })
    });
    let post = (!state.post_replacement_drains.is_empty())
        .then(|| ResolutionFrame::PostReplacement(state.post_replacement_drains.clone()));

    let conversion = match (post, draw) {
        (Some(parent), Some(child)) => {
            let ResolutionFrame::PostReplacement(drains) = &parent else {
                unreachable!("post frame classifier constructed the wrong frame kind")
            };
            if !matches!(
                drains.resident().map(|drain| &drain.status),
                Some(crate::types::game_state::DrainStatus::Paused)
            ) {
                return Err(
                    "legacy post-replacement and multi-draw state is ambiguous without a paused resident drain"
                        .to_string(),
                );
            }
            Some(DrawAndPostConversion::Paired {
                parent,
                child: Box::new(child),
            })
        }
        (None, Some(draw)) => Some(DrawAndPostConversion::UnpairedDraw(draw)),
        (Some(post), None) => Some(DrawAndPostConversion::UnpairedPost(post)),
        (None, None) => None,
    };
    Ok(conversion)
}

fn project_frames_into_legacy_state(
    state: &GameState,
    frames: &ResolutionStack,
) -> Result<GameState, String> {
    let mut projected = state.clone();
    clear_legacy_resolution_slots(&mut projected);
    for frame in frames.iter() {
        match frame {
            ResolutionFrame::AbilityContinuation(frame) => {
                projected.push_ability_continuation(frame.clone());
            }
            ResolutionFrame::RepeatFor(pending) => projected.push_repeat_for(pending.clone()),
            ResolutionFrame::RepeatUntil(pending) => projected.push_repeat_until(pending.clone()),
            ResolutionFrame::RepeatedOptionalPayment(frame) => {
                if let Some(pending) = frame.pending.clone() {
                    set_once(
                        &mut projected.pending_repeated_optional_payment,
                        pending,
                        "RepeatedOptionalPayment",
                    )?;
                }
                projected.optional_cost_payments_this_resolution =
                    frame.optional_cost_payments_this_resolution;
            }
            ResolutionFrame::ChangeZone(frame) => {
                projected
                    .resolution_stack
                    .push_change_zone((**frame).clone());
            }
            ResolutionFrame::BatchDelivery(pending) => {
                projected
                    .resolution_stack
                    .push_batch_delivery((**pending).clone());
            }
            ResolutionFrame::CounterMoves(pending) => set_once(
                &mut projected.pending_counter_moves,
                pending.clone(),
                "CounterMoves",
            )?,
            ResolutionFrame::CounterRemovals(pending) => set_once(
                &mut projected.pending_counter_removals,
                pending.clone(),
                "CounterRemovals",
            )?,
            ResolutionFrame::CounterAdditions(pending) => set_once(
                &mut projected.pending_counter_additions,
                pending.clone(),
                "CounterAdditions",
            )?,
            ResolutionFrame::CopyToken(pending) => set_once(
                &mut projected.pending_copy_token_resolution,
                pending.clone(),
                "CopyToken",
            )?,
            ResolutionFrame::EachPlayerCopyChosen(pending) => set_once(
                &mut projected.pending_each_player_copy_chosen,
                pending.clone(),
                "EachPlayerCopyChosen",
            )?,
            ResolutionFrame::ChooseOneOf(pending) => projected.push_choose_one_of(pending.clone()),
            ResolutionFrame::VoteBallot(pending) => projected.push_vote_ballot(pending.clone()),
            ResolutionFrame::PerPlayerZoneChoice(pending) => {
                projected.push_per_player_zone_choice(pending.clone())
            }
            ResolutionFrame::PerCategoryZoneChoice(frame) => {
                projected.push_per_category_zone_choice(frame.pending.clone())
            }
            ResolutionFrame::OptionalEffect(frame) => {
                set_once(
                    &mut projected.pending_optional_effect,
                    frame.ability.clone(),
                    "OptionalEffect",
                )?;
                projected.pending_optional_trigger_event = frame.trigger_event.clone();
                projected.pending_optional_trigger_match_count = frame.trigger_match_count;
            }
            ResolutionFrame::CoinFlip(pending) => set_once(
                &mut projected.pending_coin_flip,
                pending.clone(),
                "CoinFlip",
            )?,
            ResolutionFrame::Proliferate(pending) => set_once(
                &mut projected.pending_proliferate_actions,
                pending.clone(),
                "Proliferate",
            )?,
            ResolutionFrame::MultiDraw(frame) => {
                if !projected.draw_sequences.is_empty()
                    || projected.pending_connive_reentry.is_some()
                {
                    return Err("duplicate MultiDraw frame".to_string());
                }
                projected.draw_sequences = frame.draw_sequences.clone();
                projected.pending_connive_reentry = frame.pending_connive_reentry.clone();
            }
            ResolutionFrame::ConniveReentry(pending) => set_once(
                &mut projected.pending_connive_reentry,
                pending.clone(),
                "ConniveReentry",
            )?,
            ResolutionFrame::LifeTotalAssignment(pending) => set_once(
                &mut projected.pending_life_total_assignment,
                pending.clone(),
                "LifeTotalAssignment",
            )?,
            ResolutionFrame::SpellResolution(pending) => set_once(
                &mut projected.pending_spell_resolution,
                pending.clone(),
                "SpellResolution",
            )?,
            ResolutionFrame::MutateMerge(pending) => set_once(
                &mut projected.pending_mutate_merge,
                pending.clone(),
                "MutateMerge",
            )?,
            ResolutionFrame::PostReplacement(drains) => {
                if !projected.post_replacement_drains.is_empty() {
                    return Err("duplicate PostReplacement frame".to_string());
                }
                projected.post_replacement_drains = drains.clone();
            }
        }
    }
    Ok(projected)
}

fn clear_legacy_resolution_slots(state: &mut GameState) {
    state.resolution_stack = ResolutionStack::default();
    state.pending_repeated_optional_payment = None;
    state.optional_cost_payments_this_resolution = 0;
    state.pending_counter_moves = None;
    state.pending_counter_removals = None;
    state.pending_counter_additions = None;
    state.pending_copy_token_resolution = None;
    state.pending_each_player_copy_chosen = None;
    state.pending_optional_effect = None;
    state.pending_optional_trigger_event = None;
    state.pending_optional_trigger_match_count = None;
    state.pending_coin_flip = None;
    state.pending_proliferate_actions = None;
    state.draw_sequences = DrawSequenceStack::default();
    state.legacy_pending_multi_draw = None;
    state.pending_connive_reentry = None;
    state.pending_life_total_assignment = None;
    state.pending_spell_resolution = None;
    state.pending_mutate_merge = None;
    state.post_replacement_drains = PostReplacementDrainStack::default();
    state.legacy_post_replacement_effect = None;
    state.legacy_post_replacement_resolved_effect = None;
    state.legacy_post_replacement_continuation = None;
    state.legacy_post_replacement_source = None;
    state.legacy_post_replacement_applied.clear();
    state.legacy_post_replacement_event_source = None;
    state.legacy_post_replacement_event_target = None;
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("duplicate {name} frame"));
    }
    Ok(())
}

fn legacy_resolution_wire_field(object: &Map<String, Value>) -> Option<&str> {
    legacy_resolution_wire_fields()
        .iter()
        .copied()
        .find(|field| object.contains_key(*field))
}

fn remove_resolution_wire_fields(object: &mut Map<String, Value>) {
    for field in legacy_resolution_wire_fields() {
        object.remove(*field);
    }
}

fn legacy_resolution_wire_fields() -> &'static [&'static str] {
    &[
        "pending_continuation",
        "pending_repeat_iteration",
        "pending_repeat_until",
        "pending_repeated_optional_payment",
        "optional_cost_payments_this_resolution",
        "pending_change_zone_iteration",
        "devour_eligible_snapshot",
        "pending_batch_deliveries",
        "pending_mill_deliveries",
        "pending_counter_moves",
        "pending_counter_removals",
        "pending_counter_additions",
        "pending_copy_token_resolution",
        "pending_each_player_copy_chosen",
        "pending_choose_one_of",
        "pending_vote_ballot_iteration",
        "pending_per_player_zone_choice",
        "pending_per_category_zone_choice",
        "pending_choose_zone_trigger_context",
        "pending_optional_effect",
        "pending_optional_trigger_event",
        "pending_optional_trigger_match_count",
        "pending_coin_flip",
        "pending_proliferate_actions",
        "draw_sequences",
        "pending_multi_draw",
        "pending_connive_reentry",
        "pending_life_total_assignment",
        "pending_spell_resolution",
        "pending_mutate_merge",
        "post_replacement_drains",
        "post_replacement_effect",
        "post_replacement_resolved_effect",
        "post_replacement_continuation",
        "post_replacement_source",
        "post_replacement_applied",
        "post_replacement_event_source",
        "post_replacement_event_target",
    ]
}

fn validate_shipped_post_replacement_draw_pair(
    parent: &ResolutionFrame,
    child: &ResolutionFrame,
) -> Result<(), ResolutionStackError> {
    let ResolutionFrame::PostReplacement(drains) = parent else {
        return Err(ResolutionStackError::InvalidAdjacentPair(
            "the immediate parent is not a post-replacement frame",
        ));
    };
    let ResolutionFrame::MultiDraw(draw) = child else {
        return Err(ResolutionStackError::InvalidAdjacentPair(
            "the immediate child is not a multi-draw frame",
        ));
    };
    if !matches!(
        drains.resident().map(|drain| &drain.status),
        Some(crate::types::game_state::DrainStatus::Paused)
    ) {
        return Err(ResolutionStackError::InvalidAdjacentPair(
            "the parent has no paused resident drain",
        ));
    }
    if draw.draw_sequences.active().is_none() {
        return Err(ResolutionStackError::InvalidAdjacentPair(
            "the child has no active draw sequence",
        ));
    }
    draw.draw_sequences
        .validate()
        .map_err(|message| ResolutionStackError::InvalidPayload {
            frame: FrameKind::MultiDraw,
            message,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::merge::MergeSide;
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CardSelectionMode, Chooser, CopyChooseScope, Effect,
        EffectKind, ForEachCategoryAction, IterationCategory, PostReplacementContinuation,
        QuantityExpr, RepeatContinuation, ReplacementDefinition, SpellContext, TargetFilter,
        ZoneOwner,
    };
    use crate::types::actions::GameAction;
    use crate::types::game_state::{
        CastingVariant, CopyChosenStage, DrainStatus, GameState, PendingBatchDeliveries,
        PendingChooseOneOf, PendingCoinFlip, PendingCoinFlipKind, PendingCopyTokenResolution,
        PendingCounterAdditionQueue, PendingCounterMoveQueue, PendingCounterRemovalQueue,
        PendingEachPlayerCopyChosen, PendingLifeTotalAssignment, PendingMutateMerge,
        PendingPerCategoryZoneChoice, PendingPerPlayerZoneChoice, PendingProliferateActions,
        PendingRepeatIteration, PendingRepeatUntil, PendingRepeatedOptionalPayment,
        PendingSpellResolution, PendingVoteBallotIteration, PostReplacementDrain,
        ResidentDrainPolicy, ZoneDeliveryExileTracking,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::{ProposedEvent, ReplacementId};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::{EtbTapState, Zone};
    use std::collections::VecDeque;

    fn resolved_draw(source_id: u64) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(source_id),
            PlayerId(0),
        )
    }

    fn resolved_effect(source_id: u64, effect: Effect) -> ResolvedAbility {
        ResolvedAbility::new(effect, Vec::new(), ObjectId(source_id), PlayerId(0))
    }

    fn continuation_frame(source_id: u64) -> ResolutionFrame {
        let state = GameState::new_two_player(source_id);
        ResolutionFrame::AbilityContinuation(AbilityContinuationFrame {
            pending: PendingContinuation::new(Box::new(resolved_draw(source_id)), &state),
            choose_zone_trigger_context: None,
        })
    }

    fn change_zone_frame(group_seed: u64) -> ResolutionFrame {
        let mut state = GameState::new_two_player(group_seed);
        let mut logical_zone_change_group = state.allocate_logical_zone_change_group(&[]);
        logical_zone_change_group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("empty logical group still needs its pre-delivery latch");
        ResolutionFrame::ChangeZone(Box::new(ChangeZoneFrame {
            pending: Some(PendingChangeZoneIteration {
                logical_zone_change_group,
                paused_current: None,
                remaining: Vec::new(),
                source_id: ObjectId(group_seed),
                controller: PlayerId(0),
                origin: None,
                destination: Zone::Battlefield,
                enter_transformed: false,
                enter_tapped: EtbTapState::Unspecified,
                enters_under_player: None,
                enters_attacking: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                duration: None,
                track_exiled_by_source: false,
                moved_count: None,
                face_down_profile: None,
                library_placement: None,
                enters_modified_if: None,
                enter_attached_to: None,
                effect_kind: EffectKind::ChangeZone,
            }),
            devour_eligible_snapshot: None,
        }))
    }

    fn paused_post_replacement_frame() -> ResolutionFrame {
        let mut drains = PostReplacementDrainStack::default();
        let installed = drains.install(
            PostReplacementDrain::ready(PostReplacementContinuation::Resolved(Box::new(
                resolved_draw(81),
            ))),
            ResidentDrainPolicy::KeepResident,
        );
        assert!(installed);
        let (_, dispatch) = drains
            .begin_dispatch()
            .expect("ready drain must begin dispatching");
        assert!(drains.pause_dispatch(dispatch));
        assert!(matches!(
            drains.resident().map(|drain| &drain.status),
            Some(DrainStatus::Paused)
        ));
        ResolutionFrame::PostReplacement(drains)
    }

    fn active_multi_draw_frame() -> ResolutionFrame {
        let mut draw_sequences = DrawSequenceStack::default();
        draw_sequences.push(PlayerId(0), 1);
        ResolutionFrame::MultiDraw(MultiDrawFrame {
            draw_sequences,
            pending_connive_reentry: None,
        })
    }

    fn restore_v1_fixture(state: GameState) -> GameState {
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 fixture converts through the wire");
        let v2 = serde_json::to_value(&wire).expect("converted fixture serializes as v2");
        assert_eq!(
            v2["resolution_state_version"],
            Value::from(RESOLUTION_STATE_WIRE_VERSION)
        );
        assert!(v2.get("resolution_frames").is_some());
        for field in legacy_resolution_wire_fields() {
            assert!(
                v2.get(*field).is_none(),
                "v2 fixture must not write legacy field {field}"
            );
        }

        serde_json::from_value::<ResolutionStateWire>(v2)
            .expect("v2 fixture restores for the legacy runtime action path")
            .into_game_state()
    }

    fn restore_v1_batch_delivery_fixture(
        state: GameState,
        pending: PendingBatchDeliveries,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_batch_deliveries"] =
            serde_json::to_value(pending).expect("legacy batch delivery serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 batch delivery fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_ability_fixture(state: GameState, frame: AbilityContinuationFrame) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_continuation"] =
            serde_json::to_value(frame.pending).expect("legacy continuation serializes");
        if let Some(context) = frame.choose_zone_trigger_context {
            v1["pending_choose_zone_trigger_context"] =
                serde_json::to_value(context).expect("legacy trigger context serializes");
        }
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 ability fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_repeat_for_fixture(
        state: GameState,
        pending: PendingRepeatIteration,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_repeat_iteration"] =
            serde_json::to_value(pending).expect("legacy repeat-for serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 repeat-for fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_repeat_until_fixture(state: GameState, pending: PendingRepeatUntil) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_repeat_until"] =
            serde_json::to_value(pending).expect("legacy repeat-until serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 repeat-until fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_change_zone_fixture(
        state: GameState,
        pending: Option<PendingChangeZoneIteration>,
        devour_eligible_snapshot: Option<HashSet<ObjectId>>,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        if let Some(pending) = pending {
            v1["pending_change_zone_iteration"] =
                serde_json::to_value(pending).expect("legacy ChangeZone owner serializes");
        }
        if let Some(snapshot) = devour_eligible_snapshot {
            v1["devour_eligible_snapshot"] =
                serde_json::to_value(snapshot).expect("legacy Devour snapshot serializes");
        }
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 ChangeZone fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_choose_one_of_fixture(
        state: GameState,
        pending: PendingChooseOneOf,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_choose_one_of"] =
            serde_json::to_value(pending).expect("legacy choose-one-of serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 choose-one-of fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_vote_ballot_fixture(
        state: GameState,
        pending: PendingVoteBallotIteration,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_vote_ballot_iteration"] =
            serde_json::to_value(pending).expect("legacy vote-ballot serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 vote-ballot fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_per_player_zone_choice_fixture(
        state: GameState,
        pending: PendingPerPlayerZoneChoice,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_per_player_zone_choice"] =
            serde_json::to_value(pending).expect("legacy per-player choice serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 per-player choice fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_per_category_zone_choice_fixture(
        state: GameState,
        pending: PendingPerCategoryZoneChoice,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_per_category_zone_choice"] =
            serde_json::to_value(pending).expect("legacy per-category choice serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 per-category choice fixture converts through the wire")
            .into_game_state()
    }

    fn assert_reserializes_v2_only(state: GameState) {
        let v2 = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("resumed fixture serializes as v2");
        assert_eq!(
            v2["resolution_state_version"],
            Value::from(RESOLUTION_STATE_WIRE_VERSION)
        );
        assert!(v2.get("resolution_frames").is_some());
        for field in legacy_resolution_wire_fields() {
            assert!(
                v2.get(*field).is_none(),
                "resumed v2 fixture must not write legacy field {field}"
            );
        }
    }

    fn v2_fixture_with_frames(state: GameState, frames: ResolutionStack) -> Value {
        let mut v2 = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("empty v2 fixture serializes");
        v2["resolution_frames"] = serde_json::to_value(frames).expect("fixture frames serialize");
        v2
    }

    #[test]
    fn dispatcher_resumes_only_the_active_frame() {
        let ResolutionFrame::MultiDraw(draw) = active_multi_draw_frame() else {
            unreachable!("helper constructs a multi-draw frame")
        };
        let mut state = GameState::new_two_player(157);
        state.draw_sequences = draw.draw_sequences.clone();
        state.park_ability_continuation(PendingContinuation::new(
            Box::new(resolved_draw(157)),
            &state,
        ));

        let mut frames = ResolutionStack::default();
        frames.push_inner(continuation_frame(157));
        frames.push_inner(ResolutionFrame::MultiDraw(draw));

        crate::game::effects::resume_resolution_frames(&mut state, &frames, &mut Vec::new());

        assert!(state.draw_sequences.is_empty());
        assert!(
            state.active_ability_continuation().is_some(),
            "the dispatcher must not search below the active multi-draw frame"
        );
    }

    #[test]
    fn dispatcher_resumes_the_shipped_paused_draw_pair_without_popping_generically() {
        let ResolutionFrame::PostReplacement(drains) = paused_post_replacement_frame() else {
            unreachable!("helper constructs a post-replacement frame")
        };
        let ResolutionFrame::MultiDraw(draw) = active_multi_draw_frame() else {
            unreachable!("helper constructs a multi-draw frame")
        };
        let mut state = GameState::new_two_player(158);
        state.post_replacement_drains = drains.clone();
        state.draw_sequences = draw.draw_sequences.clone();

        let mut frames = ResolutionStack::default();
        frames
            .install_adjacent_post_replacement_draw(
                ResolutionFrame::PostReplacement(drains),
                ResolutionFrame::MultiDraw(draw),
            )
            .expect("fixture installs the shipped adjacent pair");

        crate::game::effects::resume_resolution_frames(&mut state, &frames, &mut Vec::new());

        assert!(state.draw_sequences.is_empty());
        assert!(
            state.post_replacement_drains.is_empty(),
            "the draw authority, not a generic frame pop, retires the paused resident"
        );
        assert_eq!(
            frames.len(),
            2,
            "the transitional dispatcher does not own frames"
        );
    }

    fn resume_priority_fixture(mut state: GameState) -> GameState {
        apply_as_current(&mut state, GameAction::PassPriority)
            .expect("priority action resumes the legacy resolution drain");
        state
    }

    #[test]
    fn structural_operations_are_top_only_and_full_drain_is_explicit() {
        let mut stack = ResolutionStack::default();
        assert!(stack.is_empty());
        assert_eq!(
            stack.insert_parent_of_active(continuation_frame(1)),
            Err(ResolutionStackError::NoActiveChild)
        );
        assert_eq!(
            stack.insert_parent_at_child_boundary(continuation_frame(1), 0),
            Err(ResolutionStackError::NoActiveChild)
        );

        stack.push_inner(ResolutionFrame::PostReplacement(
            PostReplacementDrainStack::default(),
        ));
        stack.push_inner(continuation_frame(2));
        stack
            .insert_parent_of_active(ResolutionFrame::PostReplacement(
                PostReplacementDrainStack::default(),
            ))
            .expect("active child accepts an immediate parent");
        assert_eq!(
            stack.iter().map(ResolutionFrame::kind).collect::<Vec<_>>(),
            vec![
                FrameKind::PostReplacement,
                FrameKind::PostReplacement,
                FrameKind::AbilityContinuation,
            ]
        );
        assert_eq!(
            stack.insert_parent_at_child_boundary(continuation_frame(3), stack.len()),
            Err(ResolutionStackError::InvalidChildBoundary {
                child_stack_start: stack.len(),
                stack_len: stack.len(),
            })
        );

        assert_eq!(
            stack.pop_expected(FrameKind::CoinFlip),
            Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CoinFlip,
                actual: FrameKind::AbilityContinuation,
            })
        );
        stack
            .replace_active(ResolutionFrame::PostReplacement(
                PostReplacementDrainStack::default(),
            ))
            .expect("top frame can be re-parked atomically");
        while !stack.is_empty() {
            let kind = stack.last().expect("non-empty stack has top").kind();
            stack
                .pop_expected(kind)
                .expect("full drain consumes only the top frame");
        }
        assert_eq!(
            stack.pop_expected(FrameKind::CoinFlip),
            Err(ResolutionStackError::Empty)
        );
    }

    #[test]
    fn direct_choice_gate_must_match_the_waiting_prompt() {
        let frame = ResolutionFrame::CoinFlip(PendingCoinFlip {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        });
        let mut stack = ResolutionStack::default();
        stack.push_inner(frame);
        stack
            .validate(&WaitingFor::CoinFlipKeepChoice {
                player: PlayerId(0),
                results: vec![true, false],
                keep_count: 1,
            })
            .expect("coin-flip frame owns its coin-flip prompt");
        assert_eq!(
            stack.validate(&WaitingFor::Priority {
                player: PlayerId(0),
            }),
            Err(ResolutionStackError::PromptMismatch {
                frame: FrameKind::CoinFlip,
                waiting_for: "Priority",
            })
        );

        let mut optional_effect = ResolutionStack::default();
        optional_effect.push_inner(ResolutionFrame::OptionalEffect(OptionalEffectFrame {
            ability: Box::new(resolved_draw(6)),
            trigger_event: None,
            trigger_match_count: None,
        }));
        optional_effect
            .validate(&WaitingFor::OpponentMayChoice {
                player: PlayerId(1),
                source_id: ObjectId(6),
                description: None,
                remaining: Vec::new(),
            })
            .expect("optional-effect frame owns an opponent-may prompt");
    }

    #[test]
    fn serde_round_trip_preserves_adjacent_and_separated_same_kind_frames() {
        let mut stack = ResolutionStack::default();
        stack.push_inner(change_zone_frame(1));
        stack.push_inner(change_zone_frame(2));
        stack.push_inner(continuation_frame(3));
        stack.push_inner(ResolutionFrame::PostReplacement(
            PostReplacementDrainStack::default(),
        ));
        stack.push_inner(continuation_frame(4));

        let encoded = serde_json::to_value(&stack).expect("typed stack serializes");
        let decoded: ResolutionStack =
            serde_json::from_value(encoded).expect("typed stack deserializes");
        assert_eq!(
            decoded
                .iter()
                .map(ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![
                FrameKind::ChangeZone,
                FrameKind::ChangeZone,
                FrameKind::AbilityContinuation,
                FrameKind::PostReplacement,
                FrameKind::AbilityContinuation,
            ]
        );
        decoded
            .validate(&WaitingFor::Priority {
                player: PlayerId(0),
            })
            .expect("after-child frames are valid at their resumable boundary");
    }

    #[test]
    fn shipped_paused_drain_and_active_draw_install_and_complete_as_an_adjacent_pair() {
        let parent = paused_post_replacement_frame();
        let child = active_multi_draw_frame();
        let mut stack = ResolutionStack::default();
        stack
            .install_adjacent_post_replacement_draw(parent, child)
            .expect("paused drain and active draw form the shipped adjacent pair");
        assert_eq!(
            stack.iter().map(ResolutionFrame::kind).collect::<Vec<_>>(),
            vec![FrameKind::PostReplacement, FrameKind::MultiDraw]
        );
        let encoded = serde_json::to_value(&stack).expect("paired stack serializes");
        let decoded: ResolutionStack =
            serde_json::from_value(encoded).expect("paired stack deserializes");
        assert_eq!(decoded, stack);

        let completed = stack
            .complete_adjacent_post_replacement_draw()
            .expect("completion inspects only the active child and its predecessor");
        assert_eq!(completed.kind(), FrameKind::MultiDraw);
        assert_eq!(
            stack.last().map(ResolutionFrame::kind),
            Some(FrameKind::PostReplacement)
        );
    }

    #[test]
    fn adjacent_pair_operations_never_search_for_a_non_top_parent() {
        let mut stack = ResolutionStack::default();
        stack.push_inner(paused_post_replacement_frame());
        stack.push_inner(continuation_frame(9));
        stack.push_inner(active_multi_draw_frame());
        let before = stack.clone();
        let error = stack
            .complete_adjacent_post_replacement_draw()
            .expect_err("a non-adjacent parent must not be discovered by search");
        assert!(matches!(
            error,
            ResolutionStackError::InvalidAdjacentPair(_)
        ));
        assert_eq!(stack, before, "failed paired completion is atomic");

        let mut empty = ResolutionStack::default();
        let before = empty.clone();
        assert!(empty
            .install_adjacent_post_replacement_draw(
                ResolutionFrame::PostReplacement(PostReplacementDrainStack::default()),
                active_multi_draw_frame(),
            )
            .is_err());
        assert_eq!(empty, before, "failed paired installation is atomic");
    }

    #[test]
    fn resolution_state_wire_converts_v1_to_v2_without_legacy_projection() {
        let mut state = GameState::new_two_player(42);
        state.pending_coin_flip = Some(PendingCoinFlip {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        });
        state.waiting_for = WaitingFor::CoinFlipKeepChoice {
            player: PlayerId(0),
            results: vec![true, false],
            keep_count: 1,
        };

        let mut v1 = serde_json::to_value(&state).expect("legacy state serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 legacy state converts through frames");
        assert!(wire.game_state().pending_coin_flip.is_some());

        let v2 = serde_json::to_value(&wire).expect("v2 wire serializes");
        assert_eq!(
            v2["resolution_state_version"],
            Value::from(RESOLUTION_STATE_WIRE_VERSION)
        );
        assert!(v2.get("resolution_frames").is_some());
        assert!(v2.get("pending_coin_flip").is_none());

        let restored: ResolutionStateWire =
            serde_json::from_value(v2).expect("v2 frame state projects for legacy runtime");
        assert!(restored.into_game_state().pending_coin_flip.is_some());
    }

    #[test]
    fn resolution_state_wire_keeps_choose_from_zone_context_with_its_continuation() {
        let state = GameState::new_two_player(43);
        let context = ResolvingTriggerContext {
            event: None,
            events: Vec::new(),
            match_count: Some(2),
            die_result: None,
        };
        let restored = restore_v1_ability_fixture(
            state,
            AbilityContinuationFrame {
                pending: PendingContinuation::new(
                    Box::new(resolved_draw(43)),
                    &GameState::new_two_player(43),
                ),
                choose_zone_trigger_context: Some(context.clone()),
            },
        );
        let wire = ResolutionStateWire::from_game_state(restored);
        let v2 = serde_json::to_value(&wire).expect("v2 wire serializes");
        assert!(v2.get("pending_choose_zone_trigger_context").is_none());

        let restored: ResolutionStateWire =
            serde_json::from_value(v2).expect("v2 continuation sidecar projects for runtime");
        assert_eq!(
            restored
                .into_game_state()
                .active_ability_continuation_frame()
                .and_then(|frame| frame.choose_zone_trigger_context.clone()),
            Some(context)
        );
    }

    #[test]
    fn resolution_state_wire_keeps_devour_snapshot_without_a_change_zone_iteration() {
        let state = GameState::new_two_player(44);
        let snapshot = HashSet::from([ObjectId(7), ObjectId(8)]);
        let restored = restore_v1_change_zone_fixture(state, None, Some(snapshot.clone()));
        let wire = ResolutionStateWire::from_game_state(restored);
        let v2 = serde_json::to_value(&wire).expect("v2 wire serializes");
        assert!(v2.get("devour_eligible_snapshot").is_none());

        let restored: ResolutionStateWire =
            serde_json::from_value(v2).expect("v2 devour sidecar projects for runtime");
        assert_eq!(
            restored.into_game_state().active_devour_eligible_snapshot(),
            Some(&snapshot)
        );
    }

    #[test]
    fn resolution_state_wire_keeps_payment_count_after_its_driver_has_finished() {
        let mut state = GameState::new_two_player(45);
        state.optional_cost_payments_this_resolution = 2;

        let mut v1 = serde_json::to_value(&state).expect("legacy state serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 payment count converts through frames");
        let v2 = serde_json::to_value(&wire).expect("v2 wire serializes");
        assert!(v2.get("optional_cost_payments_this_resolution").is_none());

        let restored: ResolutionStateWire =
            serde_json::from_value(v2).expect("v2 payment count projects for runtime");
        assert_eq!(
            restored
                .into_game_state()
                .optional_cost_payments_this_resolution,
            2
        );
    }

    #[test]
    fn resolution_state_wire_converts_the_shipped_paused_drain_pair_atomically() {
        let ResolutionFrame::PostReplacement(drains) = paused_post_replacement_frame() else {
            unreachable!("test helper constructs a post-replacement frame")
        };
        let ResolutionFrame::MultiDraw(draw) = active_multi_draw_frame() else {
            unreachable!("test helper constructs a multi-draw frame")
        };
        let mut state = GameState::new_two_player(42);
        state.post_replacement_drains = drains;
        state.draw_sequences = draw.draw_sequences;
        let mut v1 = serde_json::to_value(&state).expect("legacy paired state serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("paused drain and draw become one adjacent pair");
        let v2 = serde_json::to_value(&wire).expect("converted pair serializes");
        let frames: ResolutionStack =
            serde_json::from_value(v2["resolution_frames"].clone()).expect("frame payload parses");
        assert_eq!(
            frames.iter().map(ResolutionFrame::kind).collect::<Vec<_>>(),
            vec![FrameKind::PostReplacement, FrameKind::MultiDraw]
        );
    }

    #[test]
    fn resolution_state_wire_rejects_missing_unknown_and_mixed_versions() {
        let state = GameState::new_two_player(42);
        let wire = ResolutionStateWire::from_game_state(state);
        let v2 = serde_json::to_value(wire).expect("v2 wire serializes");

        let mut missing = v2.clone();
        missing
            .as_object_mut()
            .expect("wire is an object")
            .remove("resolution_state_version");
        assert!(serde_json::from_value::<ResolutionStateWire>(missing).is_err());

        let mut unknown = v2.clone();
        unknown["resolution_state_version"] = Value::from(99);
        assert!(serde_json::from_value::<ResolutionStateWire>(unknown).is_err());

        let mut mixed = v2;
        mixed["pending_coin_flip"] = Value::Null;
        assert!(serde_json::from_value::<ResolutionStateWire>(mixed).is_err());
    }

    #[test]
    fn validation_rejects_a_paused_drain_pair_buried_below_another_frame() {
        let mut stack = ResolutionStack::default();
        stack
            .install_adjacent_post_replacement_draw(
                paused_post_replacement_frame(),
                active_multi_draw_frame(),
            )
            .expect("pair installs");
        stack.push_inner(continuation_frame(9));
        assert!(matches!(
            stack.validate(&WaitingFor::Priority {
                player: PlayerId(0)
            }),
            Err(ResolutionStackError::InvalidAdjacentPair(_))
        ));
    }

    #[test]
    fn validation_keeps_an_independent_paused_drain_without_a_draw_frame() {
        let mut stack = ResolutionStack::default();
        stack.push_inner(paused_post_replacement_frame());
        stack
            .validate(&WaitingFor::Priority {
                player: PlayerId(0),
            })
            .expect("a non-draw paused drain remains an independent post-replacement frame");
    }

    #[test]
    fn v1_direct_choice_fixtures_resume_on_the_real_action_path() {
        let mut repeated = GameState::new_two_player(100);
        repeated.pending_repeated_optional_payment =
            Some(Box::new(PendingRepeatedOptionalPayment {
                payment_unit: Box::new(resolved_draw(100)),
                reflexive: Box::new(resolved_draw(101)),
                remaining: 0,
            }));
        repeated.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };
        let mut repeated = restore_v1_fixture(repeated);
        apply_as_current(
            &mut repeated,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("repeated-payment fixture resumes through the real optional-choice action");
        assert!(repeated.pending_repeated_optional_payment.is_none());
        assert_reserializes_v2_only(repeated);

        let mut optional = GameState::new_two_player(102);
        optional.pending_optional_effect = Some(Box::new(resolved_draw(102)));
        optional.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(102),
            description: None,
            may_trigger_key: None,
        };
        let mut optional = restore_v1_fixture(optional);
        apply_as_current(
            &mut optional,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("optional-effect fixture resumes through the real optional-choice action");
        assert!(optional.pending_optional_effect.is_none());
        assert_reserializes_v2_only(optional);

        let mut coin = GameState::new_two_player(103);
        coin.pending_coin_flip = Some(PendingCoinFlip {
            source_id: ObjectId(103),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        });
        coin.waiting_for = WaitingFor::CoinFlipKeepChoice {
            player: PlayerId(0),
            results: vec![true, false],
            keep_count: 1,
        };
        let mut coin = restore_v1_fixture(coin);
        apply_as_current(
            &mut coin,
            GameAction::SelectCoinFlips {
                keep_indices: vec![0],
            },
        )
        .expect("coin-flip fixture resumes through the real keep-choice action");
        assert!(coin.pending_coin_flip.is_none());
        assert_reserializes_v2_only(coin);

        let mut proliferate = GameState::new_two_player(104);
        proliferate.pending_proliferate_actions = Some(PendingProliferateActions {
            actor: PlayerId(0),
            source_id: ObjectId(104),
            remaining: 0,
        });
        proliferate.waiting_for = WaitingFor::ProliferateChoice {
            player: PlayerId(0),
            eligible: Vec::new(),
        };
        let mut proliferate = restore_v1_fixture(proliferate);
        apply_as_current(
            &mut proliferate,
            GameAction::SelectTargets {
                targets: Vec::new(),
            },
        )
        .expect("proliferate fixture resumes through the real target-choice action");
        assert!(proliferate.pending_proliferate_actions.is_none());
        assert_reserializes_v2_only(proliferate);

        let mut scenario = GameScenario::new();
        let merging_id = scenario.add_creature(PlayerId(0), "Rider", 4, 4).id();
        let target_id = scenario.add_creature(PlayerId(0), "Host", 2, 2).id();
        let mut mutate = scenario.state;
        mutate.pending_mutate_merge = Some(PendingMutateMerge {
            merging_id,
            target_id,
            controller: PlayerId(0),
        });
        mutate.waiting_for = WaitingFor::MutateMergeChoice {
            player: PlayerId(0),
            merging_id,
            target_id,
        };
        let mut mutate = restore_v1_fixture(mutate);
        apply_as_current(
            &mut mutate,
            GameAction::ChooseMutateMergeSide {
                side: MergeSide::Top,
            },
        )
        .expect("mutate fixture resumes through the real merge-choice action");
        assert!(mutate.pending_mutate_merge.is_none());
        assert_eq!(
            mutate
                .objects
                .get(&target_id)
                .expect("merged target remains in the object map")
                .merged_components,
            vec![merging_id, target_id]
        );
        assert_reserializes_v2_only(mutate);
    }

    #[test]
    fn v1_after_child_fixtures_resume_on_the_real_priority_drain() {
        let continuation = GameState::new_two_player(110);
        let pending = PendingContinuation::new(Box::new(resolved_draw(110)), &continuation);
        let continuation = resume_priority_fixture(restore_v1_ability_fixture(
            continuation,
            AbilityContinuationFrame {
                pending,
                choose_zone_trigger_context: None,
            },
        ));
        assert!(continuation.active_ability_continuation().is_none());
        assert_reserializes_v2_only(continuation);

        let repeat_for = GameState::new_two_player(111);
        let repeat_for = resume_priority_fixture(restore_v1_repeat_for_fixture(
            repeat_for,
            PendingRepeatIteration {
                ability: Box::new(resolved_draw(111)),
                tracked_members: Vec::new(),
                iterated_counter_kinds: Vec::new(),
                next_iteration: 0,
                total_iterations: 0,
            },
        ));
        assert!(repeat_for.active_repeat_for().is_none());
        assert_reserializes_v2_only(repeat_for);

        let repeat_until = GameState::new_two_player(112);
        let mut repeat_ability = resolved_draw(112);
        repeat_ability.repeat_until = Some(RepeatContinuation::ControllerChoice);
        let mut repeat_until = resume_priority_fixture(restore_v1_repeat_until_fixture(
            repeat_until,
            PendingRepeatUntil {
                ability: Box::new(repeat_ability),
            },
        ));
        assert!(matches!(
            repeat_until.waiting_for,
            WaitingFor::RepeatDecision { .. }
        ));
        apply_as_current(
            &mut repeat_until,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("repeat-until fixture resumes through the real repeat decision");
        assert!(repeat_until.active_repeat_until().is_none());
        assert_reserializes_v2_only(repeat_until);

        let ResolutionFrame::ChangeZone(change_zone_frame) = change_zone_frame(113) else {
            unreachable!("helper constructs a change-zone frame")
        };
        let change_zone = GameState::new_two_player(113);
        let change_zone = resume_priority_fixture(restore_v1_change_zone_fixture(
            change_zone,
            change_zone_frame.pending,
            Some(HashSet::from([ObjectId(113)])),
        ));
        assert!(change_zone.active_change_zone_frame().is_none());
        assert_reserializes_v2_only(change_zone);

        let mut counter_moves = GameState::new_two_player(114);
        counter_moves.pending_counter_moves = Some(PendingCounterMoveQueue {
            remaining: Vec::new(),
            effect_kind: EffectKind::MoveCounters,
            source_id: ObjectId(114),
        });
        let counter_moves = resume_priority_fixture(restore_v1_fixture(counter_moves));
        assert!(counter_moves.pending_counter_moves.is_none());
        assert_reserializes_v2_only(counter_moves);

        let mut counter_removals = GameState::new_two_player(115);
        counter_removals.pending_counter_removals = Some(PendingCounterRemovalQueue {
            remaining: Vec::new(),
            source_id: ObjectId(115),
            effect_kind: EffectKind::RemoveCounter,
            source_ability_id: ObjectId(115),
            total: 0,
        });
        let counter_removals = resume_priority_fixture(restore_v1_fixture(counter_removals));
        assert!(counter_removals.pending_counter_removals.is_none());
        assert_reserializes_v2_only(counter_removals);

        let mut counter_additions = GameState::new_two_player(116);
        counter_additions.pending_counter_additions = Some(PendingCounterAdditionQueue {
            remaining: Vec::new(),
            completion: None,
        });
        let counter_additions = resume_priority_fixture(restore_v1_fixture(counter_additions));
        assert!(counter_additions.pending_counter_additions.is_none());
        assert_reserializes_v2_only(counter_additions);
    }

    #[test]
    fn v1_batch_and_copy_token_fixtures_resume_via_their_production_drains() {
        let mut batch = GameState::new_two_player(120);
        let mut logical_zone_change_group = batch.allocate_logical_zone_change_group(&[]);
        logical_zone_change_group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("empty batch group retains its pre-delivery latch");
        let pending = PendingBatchDeliveries {
            logical_zone_change_group,
            paused_current: None,
            remaining: Vec::new(),
            destination: Zone::Graveyard,
            source_id: None,
            enter_tapped: EtbTapState::Unspecified,
            exile_tracking: ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: Vec::new(),
            zone_change_record_start: batch.zone_changes_this_turn.len(),
            deferred_events: Vec::new(),
        };
        let mut batch = restore_v1_batch_delivery_fixture(batch, pending.clone());
        crate::game::zone_pipeline::drain_pending_batch_deliveries(&mut batch, &mut Vec::new());
        assert!(batch.active_batch_delivery().is_none());
        assert_reserializes_v2_only(batch);

        let alias_state = GameState::new_two_player(120);
        let mut v1 = serde_json::to_value(alias_state).expect("legacy alias fixture serializes");
        v1["pending_mill_deliveries"] =
            serde_json::to_value(pending).expect("legacy mill alias serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let alias = serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 pending_mill_deliveries alias restores")
            .into_game_state();
        assert!(alias.active_batch_delivery().is_some());
        assert_reserializes_v2_only(alias);

        let mut copy_token = GameState::new_two_player(121);
        copy_token.pending_copy_token_resolution = Some(PendingCopyTokenResolution {
            created_ids: Vec::new(),
            remaining: VecDeque::new(),
            effect_kind: EffectKind::CopyTokenOf,
            source_id: ObjectId(121),
        });
        let mut copy_token = restore_v1_fixture(copy_token);
        crate::game::effects::token_copy::drain_pending_copy_token_resolution(
            &mut copy_token,
            &mut Vec::new(),
        );
        assert!(copy_token.pending_copy_token_resolution.is_none());
        assert_reserializes_v2_only(copy_token);
    }

    #[test]
    fn v1_choice_iteration_fixtures_resume_via_their_production_drains() {
        let mut each_player_copy = GameState::new_two_player(130);
        each_player_copy.pending_each_player_copy_chosen = Some(PendingEachPlayerCopyChosen {
            stage: CopyChosenStage::AwaitingCounters,
            player: PlayerId(0),
            chosen: Vec::new(),
            remaining_choices: Vec::new(),
            choose_filter: TargetFilter::Controller,
            min: 0,
            max: 0,
            copy_modifications: Vec::new(),
            scale: None,
            choose_scope: CopyChooseScope::Chooser,
            source_id: ObjectId(130),
            source_controller: PlayerId(0),
            scoped_players: Vec::new(),
            trigger_event: None,
        });
        let mut each_player_copy = restore_v1_fixture(each_player_copy);
        crate::game::effects::each_player_copy_chosen::drain_pending(
            &mut each_player_copy,
            &mut Vec::new(),
        );
        assert!(each_player_copy.pending_each_player_copy_chosen.is_none());
        assert_reserializes_v2_only(each_player_copy);

        let choose_one_of = GameState::new_two_player(131);
        let mut choose_one_of = restore_v1_choose_one_of_fixture(
            choose_one_of,
            PendingChooseOneOf {
                controller: PlayerId(0),
                source_id: ObjectId(131),
                branches: Vec::new(),
                parent_targets: Vec::new(),
                context: SpellContext::default(),
                replacement_applied: HashSet::new(),
                remaining_players: Vec::new(),
            },
        );
        crate::game::effects::choose_one_of::resume_pending(&mut choose_one_of, &mut Vec::new());
        assert!(choose_one_of.active_choose_one_of().is_none());
        assert_reserializes_v2_only(choose_one_of);

        let vote = GameState::new_two_player(132);
        let mut vote = restore_v1_vote_ballot_fixture(
            vote,
            PendingVoteBallotIteration {
                ability_template: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::NoOp,
                )),
                remaining_voters: Vec::new(),
                source_id: ObjectId(132),
                controller: PlayerId(0),
            },
        );
        crate::game::effects::vote::drain_active_vote_ballot(&mut vote, &mut Vec::new());
        assert!(vote.active_vote_ballot().is_none());
        assert_reserializes_v2_only(vote);

        let choose_from_zone = resolved_effect(
            133,
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Graveyard,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::EachPlayer,
                filter: None,
                chooser: Chooser::Controller,
                up_to: true,
                selection: CardSelectionMode::Chosen,
                constraint: None,
            },
        );
        let per_player = GameState::new_two_player(133);
        let mut per_player = restore_v1_per_player_zone_choice_fixture(
            per_player,
            PendingPerPlayerZoneChoice {
                ability: Box::new(choose_from_zone),
                remaining_players: Vec::new(),
                accumulated: false,
            },
        );
        crate::game::effects::choose_from_zone::drain_active_per_player_zone_choice(
            &mut per_player,
            &[],
            &mut Vec::new(),
        );
        assert!(per_player.active_per_player_zone_choice().is_none());
        assert_reserializes_v2_only(per_player);

        let for_each_category = resolved_effect(
            134,
            Effect::ForEachCategory {
                category: IterationCategory::Color,
                chooser: Chooser::Controller,
                action: ForEachCategoryAction::ExileFromPool {
                    zone: Zone::Graveyard,
                    up_to: true,
                },
            },
        );
        let per_category = GameState::new_two_player(134);
        let mut per_category = restore_v1_per_category_zone_choice_fixture(
            per_category,
            PendingPerCategoryZoneChoice {
                ability: Box::new(for_each_category),
                pool: Vec::new(),
                remaining_member_filters: Vec::new(),
            },
        );
        let _ = crate::game::effects::choose_from_zone::drain_active_per_category_zone_choice(
            &mut per_category,
            &[],
            &mut Vec::new(),
        );
        assert!(per_category.active_per_category_zone_choice().is_none());
        assert_reserializes_v2_only(per_category);
    }

    #[test]
    fn v1_remaining_resolution_frames_resume_via_shipped_authorities() {
        let mut multi_draw = GameState::new_two_player(140);
        let outer = multi_draw.draw_sequences.push(PlayerId(0), 0);
        let inner = multi_draw.draw_sequences.push(PlayerId(0), 0);
        let mut multi_draw = restore_v1_fixture(multi_draw);
        let _ = crate::game::effects::draw::resume_draw_sequence(
            &mut multi_draw,
            inner,
            &mut Vec::new(),
        );
        let _ = crate::game::effects::draw::resume_draw_sequence(
            &mut multi_draw,
            outer,
            &mut Vec::new(),
        );
        assert!(multi_draw.draw_sequences.is_empty());
        assert_reserializes_v2_only(multi_draw);

        let mut connive = GameScenario::new();
        let conniver = connive.add_creature(PlayerId(0), "Conniver", 1, 1).id();
        let mut connive = connive.state;
        connive.pending_connive_reentry = Some(PendingConniveReentry {
            conniver: connive
                .capture_connive_subject(conniver)
                .expect("fixture conniver exists"),
            count: 0,
            applied: HashSet::new(),
        });
        let mut connive = restore_v1_fixture(connive);
        let pending = connive
            .pending_connive_reentry
            .take()
            .expect("v1 fixture restores the exact connive subject");
        crate::game::effects::connive::propose_connive(
            &mut connive,
            pending.conniver,
            pending.count,
            pending.applied,
            &mut Vec::new(),
        )
        .expect("connive fixture re-enters through the production proposer");
        assert!(connive.pending_connive_reentry.is_none());
        assert_reserializes_v2_only(connive);

        let mut life = GameState::new_two_player(141);
        life.pending_life_total_assignment = Some(PendingLifeTotalAssignment {
            completion_player: PlayerId(0),
            remaining: Vec::new(),
            completion: None,
        });
        let mut life = restore_v1_fixture(life);
        crate::game::effects::life::drain_pending_life_total_assignment(&mut life, &mut Vec::new());
        assert!(life.pending_life_total_assignment.is_none());
        assert_reserializes_v2_only(life);

        let mut spell = GameState::new_two_player(142);
        let spell_id = crate::game::zones::create_object(
            &mut spell,
            CardId(142),
            PlayerId(0),
            "Paused spell".to_string(),
            Zone::Stack,
        );
        let bear = crate::game::zones::create_object(
            &mut spell,
            CardId(143),
            PlayerId(0),
            "Regenerating bear".to_string(),
            Zone::Battlefield,
        );
        spell
            .objects
            .get_mut(&bear)
            .expect("fixture bear exists")
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Destroy)
            .regeneration_shield()
            .description("Regenerate".to_string())]
        .into();
        spell.pending_spell_resolution = Some(PendingSpellResolution {
            object_id: spell_id,
            controller: PlayerId(0),
            casting_variant: CastingVariant::Normal,
            cast_from_zone: None,
            cast_controller: None,
            cast_timing_permission: None,
            spell_targets: Vec::new(),
            actual_mana_spent: 0,
            kickers_paid: Vec::new(),
            additional_cost_payment_count: 0,
            additional_cost_payments: Vec::new(),
            convoked_creatures: Vec::new(),
        });
        spell.pending_replacement = Some(crate::types::game_state::PendingReplacement {
            proposed: ProposedEvent::Destroy {
                object_id: bear,
                source: None,
                cant_regenerate: false,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: vec![ReplacementId {
                source: bear,
                index: 0,
            }],
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        spell.waiting_for =
            crate::game::replacement::replacement_choice_waiting_for(PlayerId(0), &spell);
        let mut spell = restore_v1_fixture(spell);
        apply_as_current(&mut spell, GameAction::ChooseReplacement { index: 0 })
            .expect("spell fixture resumes through the real replacement action");
        assert!(spell.pending_spell_resolution.is_none());
        assert_reserializes_v2_only(spell);

        let mut post_replacement = GameState::new_two_player(144);
        assert!(post_replacement.post_replacement_drains.install(
            PostReplacementDrain::ready(PostReplacementContinuation::Resolved(Box::new(
                resolved_draw(144),
            ))),
            ResidentDrainPolicy::KeepResident,
        ));
        let mut post_replacement = restore_v1_fixture(post_replacement);
        assert!(
            crate::game::engine_replacement::apply_pending_post_replacement_effect(
                &mut post_replacement,
                None,
                None,
                None,
                &mut Vec::new(),
            )
            .is_none()
        );
        assert!(post_replacement.post_replacement_drains.is_empty());
        assert_reserializes_v2_only(post_replacement);
    }

    #[test]
    fn v1_paired_post_replacement_and_multi_draw_fixture_resumes_as_one_resident_pair() {
        let ResolutionFrame::PostReplacement(drains) = paused_post_replacement_frame() else {
            unreachable!("helper constructs a post-replacement frame")
        };
        let ResolutionFrame::MultiDraw(draw) = active_multi_draw_frame() else {
            unreachable!("helper constructs a multi-draw frame")
        };
        let frame_id = draw
            .draw_sequences
            .active()
            .expect("fixture draw frame is active")
            .frame_id;
        let mut paired = GameState::new_two_player(145);
        paired.post_replacement_drains = drains;
        paired.draw_sequences = draw.draw_sequences;
        let mut paired = restore_v1_fixture(paired);
        let _ = crate::game::effects::draw::resume_draw_sequence(
            &mut paired,
            frame_id,
            &mut Vec::new(),
        );
        assert!(paired.draw_sequences.is_empty());
        assert!(paired.post_replacement_drains.is_empty());
        assert_reserializes_v2_only(paired);
    }

    #[test]
    fn resolution_state_wire_rejects_translated_ambiguous_and_invalid_frame_shapes() {
        let base = GameState::new_two_player(150);
        let v2 = serde_json::to_value(ResolutionStateWire::from_game_state(base.clone()))
            .expect("base v2 fixture serializes");

        let mut v2_missing_frames = v2.clone();
        v2_missing_frames
            .as_object_mut()
            .expect("v2 fixture is an object")
            .remove("resolution_frames");
        assert!(serde_json::from_value::<ResolutionStateWire>(v2_missing_frames).is_err());

        let mut v2_with_legacy = v2.clone();
        v2_with_legacy["pending_coin_flip"] = Value::Null;
        assert!(serde_json::from_value::<ResolutionStateWire>(v2_with_legacy).is_err());

        let mut v1_with_frames = serde_json::to_value(base.clone()).expect("v1 serializes");
        v1_with_frames["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        v1_with_frames["resolution_frames"] = Value::Array(Vec::new());
        assert!(serde_json::from_value::<ResolutionStateWire>(v1_with_frames).is_err());

        let mut invalid_version_type = v2.clone();
        invalid_version_type["resolution_state_version"] = Value::from("two");
        assert!(serde_json::from_value::<ResolutionStateWire>(invalid_version_type).is_err());

        let mut multiple_direct = GameState::new_two_player(151);
        multiple_direct.pending_coin_flip = Some(PendingCoinFlip {
            source_id: ObjectId(151),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        });
        multiple_direct.pending_proliferate_actions = Some(PendingProliferateActions {
            actor: PlayerId(0),
            source_id: ObjectId(151),
            remaining: 0,
        });
        let mut multiple_direct = serde_json::to_value(multiple_direct).expect("v1 serializes");
        multiple_direct["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        assert!(serde_json::from_value::<ResolutionStateWire>(multiple_direct).is_err());

        let orphan_context = ResolvingTriggerContext {
            event: None,
            events: Vec::new(),
            match_count: None,
            die_result: None,
        };
        let mut orphan_choose_context =
            serde_json::to_value(GameState::new_two_player(152)).expect("v1 serializes");
        orphan_choose_context["pending_choose_zone_trigger_context"] =
            serde_json::to_value(orphan_context).expect("legacy trigger context serializes");
        orphan_choose_context["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        assert!(serde_json::from_value::<ResolutionStateWire>(orphan_choose_context).is_err());

        let mut orphan_optional_context = GameState::new_two_player(153);
        orphan_optional_context.pending_optional_trigger_match_count = Some(1);
        let mut orphan_optional_context =
            serde_json::to_value(orphan_optional_context).expect("v1 serializes");
        orphan_optional_context["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        assert!(serde_json::from_value::<ResolutionStateWire>(orphan_optional_context).is_err());

        let mut ambiguous_legacy_pair = GameState::new_two_player(154);
        let ResolutionFrame::PostReplacement(ready_drains) = ({
            let mut drains = PostReplacementDrainStack::default();
            assert!(drains.install(
                PostReplacementDrain::ready(PostReplacementContinuation::Resolved(Box::new(
                    resolved_draw(154),
                ))),
                ResidentDrainPolicy::KeepResident,
            ));
            ResolutionFrame::PostReplacement(drains)
        }) else {
            unreachable!("fixture constructs a post-replacement frame")
        };
        let ResolutionFrame::MultiDraw(draw) = active_multi_draw_frame() else {
            unreachable!("fixture constructs a multi-draw frame")
        };
        ambiguous_legacy_pair.post_replacement_drains = ready_drains;
        ambiguous_legacy_pair.draw_sequences = draw.draw_sequences;
        let mut ambiguous_legacy_pair =
            serde_json::to_value(ambiguous_legacy_pair).expect("v1 serializes");
        ambiguous_legacy_pair["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        assert!(serde_json::from_value::<ResolutionStateWire>(ambiguous_legacy_pair).is_err());

        let mut duplicate_draw = ResolutionStack::default();
        duplicate_draw.push_inner(active_multi_draw_frame());
        duplicate_draw.push_inner(active_multi_draw_frame());
        assert!(
            serde_json::from_value::<ResolutionStateWire>(v2_fixture_with_frames(
                base.clone(),
                duplicate_draw,
            ))
            .is_err()
        );

        let mut mismatched_gate = ResolutionStack::default();
        mismatched_gate.push_inner(ResolutionFrame::CoinFlip(PendingCoinFlip {
            source_id: ObjectId(155),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        }));
        assert!(
            serde_json::from_value::<ResolutionStateWire>(v2_fixture_with_frames(
                base.clone(),
                mismatched_gate,
            ))
            .is_err()
        );

        let mut nonadjacent_pair = ResolutionStack::default();
        nonadjacent_pair.push_inner(paused_post_replacement_frame());
        nonadjacent_pair.push_inner(continuation_frame(156));
        nonadjacent_pair.push_inner(active_multi_draw_frame());
        assert!(
            serde_json::from_value::<ResolutionStateWire>(v2_fixture_with_frames(
                base.clone(),
                nonadjacent_pair,
            ))
            .is_err()
        );

        let mut reordered_pair = ResolutionStack::default();
        reordered_pair.push_inner(active_multi_draw_frame());
        reordered_pair.push_inner(paused_post_replacement_frame());
        assert!(
            serde_json::from_value::<ResolutionStateWire>(v2_fixture_with_frames(
                base,
                reordered_pair,
            ))
            .is_err()
        );
    }
}
