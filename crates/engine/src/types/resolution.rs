//! Typed, serializable suspension frames for ability resolution.
//!
//! This module deliberately models only suspended resolution work. Migrated
//! families use [`ResolutionStack`] as their runtime authority; unmigrated
//! families remain in their legacy `GameState` slots until their Phase-3 turn.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::types::ability::{AbilityDefinition, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    DrainStatus, DrawSequenceStack, GameState, PendingBatchDeliveries, PendingChangeZoneIteration,
    PendingChooseOneOf, PendingConniveReentry, PendingContinuation, PendingCopyTokenResolution,
    PendingCounterAdditionQueue, PendingCounterMoveQueue, PendingCounterRemovalQueue,
    PendingEachPlayerCopyChosen, PendingLifeTotalAssignment, PendingMultiDraw,
    PendingPerCategoryZoneChoice, PendingPerPlayerZoneChoice, PendingRepeatIteration,
    PendingRepeatUntil, PendingSpellResolution, PendingVoteBallotIteration, PostReplacementDrain,
    PostReplacementDrainStack, ResidentDrainPolicy, ResolvingTriggerContext, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

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
    /// The exact conniver snapshot remains inside its active draw authority so
    /// an adjacent paused PostReplacement parent stays the draw's immediate
    /// predecessor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connive_reentry: Option<PendingConniveReentry>,
}

/// CR 603.12a + CR 608.2c: A repeated optional-cost process parked at one
/// payment decision. The count belongs to this one process and remains in its
/// frame through the reflexive modal prompt after the last payment driver has
/// completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRepeatedOptionalPayment {
    pub payment_unit: Box<ResolvedAbility>,
    pub reflexive: Box<ResolvedAbility>,
    pub remaining: u32,
}

/// The complete parked repeated optional-payment authority.
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

/// CR 705.1 + CR 614.1a: Discriminates which multi-flip resolver paused for a
/// Krark's Thumb keep-one choice, carrying the loop position needed to re-enter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingCoinFlipKind {
    /// `Effect::FlipCoin` — a single logical flip.
    Single,
    /// `Effect::FlipCoins { count }` — `remaining` flips still to perform after
    /// the one currently paused for a keep choice.
    FlipN { remaining: u32 },
    /// `Effect::FlipCoinUntilLose` — `wins_so_far` flips won before the one
    /// currently paused for a keep choice.
    UntilLose { wins_so_far: u32 },
}

/// CR 705.1 + CR 614.1a: Full resolution context and loop position for a
/// multi-flip resolver paused at a Krark's Thumb keep-one choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCoinFlip {
    pub source_id: ObjectId,
    pub controller: PlayerId,
    /// CR 705.2: The player who flips (and therefore wins or loses) the coin.
    /// Defaults to the controller for in-flight states serialized before this
    /// field existed.
    #[serde(default)]
    pub flipper: PlayerId,
    pub targets: Vec<TargetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub win_effect: Option<Box<AbilityDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lose_effect: Option<Box<AbilityDefinition>>,
    pub kind: PendingCoinFlipKind,
}

/// CR 701.34a + CR 614.1a: Remaining proliferate actions after a count-modifying
/// replacement effect. Each completed `ProliferateChoice` drains one action;
/// when `remaining` reaches zero the originating effect resolves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProliferateActions {
    pub actor: PlayerId,
    pub source_id: ObjectId,
    pub remaining: u32,
}

/// CR 702.140c + CR 730.2a: Context stored when a mutating creature spell
/// resolves with a legal target. Resolution pauses until the spell's controller
/// chooses top or bottom via `GameAction::ChooseMutateMergeSide`; the typed
/// frame remains the sole authority until `merge::handle_mutate_merge_choice`
/// performs the merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMutateMerge {
    /// The resolving mutate spell object (the card/token being merged onto the
    /// target). Retains its original owner so CR 730.3 can route it correctly.
    pub merging_id: ObjectId,
    /// The surviving battlefield creature. The merged permanent keeps THIS
    /// object's `ObjectId` (CR 730.2c continuity).
    pub target_id: ObjectId,
    /// The mutate spell's controller — the player who chooses top/bottom
    /// (CR 702.140c).
    pub controller: PlayerId,
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

    /// Whether this frame resides in `GameState::resolution_stack` at runtime.
    ///
    /// The v2 wire also carries the remaining legacy resolution authorities as
    /// frames. Those are projected back into their dedicated state fields on
    /// decode, so they may follow an active stack-resident child in wire order.
    const fn is_runtime_stack_resident(&self) -> bool {
        match self {
            Self::AbilityContinuation(_)
            | Self::RepeatFor(_)
            | Self::RepeatUntil(_)
            | Self::RepeatedOptionalPayment(_)
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
            | Self::OptionalEffect(_)
            | Self::CoinFlip(_)
            | Self::Proliferate(_)
            | Self::MutateMerge(_)
            | Self::MultiDraw(_)
            | Self::ConniveReentry(_)
            | Self::LifeTotalAssignment(_)
            | Self::SpellResolution(_)
            | Self::PostReplacement(_) => true,
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
    #[error(
        "child-stack boundary {child_stack_start} has {actual:?} immediately below it, expected {expected:?}"
    )]
    UnexpectedChildBoundaryParent {
        child_stack_start: usize,
        expected: FrameKind,
        actual: FrameKind,
    },
    #[error("child-stack boundary {child_stack_start} does not retain the ChangeZone owner being re-parked")]
    MismatchedChangeZoneBoundaryOwner { child_stack_start: usize },
    #[error("top frame {frame:?} does not match waiting prompt {waiting_for}")]
    PromptMismatch {
        frame: FrameKind,
        waiting_for: &'static str,
    },
    #[error("resolution stack contains multiple direct-choice owners")]
    MultipleDirectChoiceOwners,
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
    /// The monotonic draw-frame allocator survives an abandoned MultiDraw
    /// frame, so a stale captured ID cannot alias a later instruction.
    #[serde(default)]
    next_draw_sequence_frame_id: u64,
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

    pub(crate) fn next_draw_sequence_frame_id(&self) -> u64 {
        self.next_draw_sequence_frame_id
    }

    pub(crate) fn restore_next_draw_sequence_frame_id(&mut self, next_frame_id: u64) {
        self.next_draw_sequence_frame_id = next_frame_id;
    }

    pub(crate) fn observe_draw_sequence_frame_id(&mut self, next_frame_id: u64) {
        self.next_draw_sequence_frame_id = self.next_draw_sequence_frame_id.max(next_frame_id);
    }

    /// Restores a v2 payload written before the outer allocator was serialized.
    /// The active frame's allocator is the lower bound, never a reset value.
    fn recover_draw_sequence_allocator(&mut self) {
        let next_frame_id = self
            .frames
            .iter()
            .filter_map(|frame| match frame {
                ResolutionFrame::MultiDraw(draw) => Some(draw.draw_sequences.next_frame_id()),
                _ => None,
            })
            .max();
        if let Some(next_frame_id) = next_frame_id {
            self.observe_draw_sequence_frame_id(next_frame_id);
        }
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
                (
                    Some(ResolutionFrame::MultiDraw(left)),
                    Some(ResolutionFrame::MultiDraw(right)),
                ) if left.draw_sequences.loop_equal(&right.draw_sequences)
                    && left.connive_reentry == right.connive_reentry => {}
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

    /// Insert a newly paused ChangeZone owner below the child stack raised by
    /// the current zone move. If the exact boundary or its immediate predecessor
    /// is the Devour-only owner for this move, upgrade that exact frame in place
    /// so its eligibility snapshot remains coupled to the complete logical
    /// iteration owner.
    pub fn insert_change_zone_parent_at_child_boundary(
        &mut self,
        pending: PendingChangeZoneIteration,
        child_stack_start: usize,
    ) -> Result<(), ResolutionStackError> {
        if child_stack_start >= self.frames.len() {
            return Err(ResolutionStackError::InvalidChildBoundary {
                child_stack_start,
                stack_len: self.frames.len(),
            });
        }

        if let Some(ResolutionFrame::ChangeZone(frame)) = self.frames.get(child_stack_start) {
            if frame.pending.is_none() && frame.devour_eligible_snapshot.is_some() {
                let devour_eligible_snapshot = frame.devour_eligible_snapshot.clone();
                self.frames[child_stack_start] =
                    ResolutionFrame::ChangeZone(Box::new(ChangeZoneFrame {
                        pending: Some(pending),
                        devour_eligible_snapshot,
                    }));
                return Ok(());
            }
        }

        if let Some(parent_index) = child_stack_start.checked_sub(1) {
            if let Some(ResolutionFrame::ChangeZone(frame)) = self.frames.get(parent_index) {
                if frame.pending.is_none() && frame.devour_eligible_snapshot.is_some() {
                    let devour_eligible_snapshot = frame.devour_eligible_snapshot.clone();
                    self.frames[parent_index] =
                        ResolutionFrame::ChangeZone(Box::new(ChangeZoneFrame {
                            pending: Some(pending),
                            devour_eligible_snapshot,
                        }));
                    return Ok(());
                }
            }
        }

        self.insert_parent_at_child_boundary(
            ResolutionFrame::ChangeZone(Box::new(ChangeZoneFrame {
                pending: Some(pending),
                devour_eligible_snapshot: None,
            })),
            child_stack_start,
        )
    }

    /// Replace the exact ChangeZone owner at the captured child boundary or
    /// immediately below it. The retained frame must own the same logical
    /// group as `pending`, so a nested ChangeZone child is never overwritten.
    /// This is the re-pause counterpart to `replace_active_change_zone`: the
    /// owner remains in place while an ETB-counter child owns the stack top.
    pub fn replace_change_zone_parent_at_child_boundary(
        &mut self,
        pending: PendingChangeZoneIteration,
        child_stack_start: usize,
    ) -> Result<(), ResolutionStackError> {
        let logical_group_id = pending.logical_zone_change_group.logical_group_id;
        if let Some(ResolutionFrame::ChangeZone(frame)) = self.frames.get(child_stack_start) {
            if frame.pending.as_ref().is_some_and(|current| {
                current.logical_zone_change_group.logical_group_id == logical_group_id
            }) {
                let devour_eligible_snapshot = frame.devour_eligible_snapshot.clone();
                self.frames[child_stack_start] =
                    ResolutionFrame::ChangeZone(Box::new(ChangeZoneFrame {
                        pending: Some(pending),
                        devour_eligible_snapshot,
                    }));
                return Ok(());
            }
        }

        let parent_index =
            child_stack_start
                .checked_sub(1)
                .ok_or(ResolutionStackError::InvalidChildBoundary {
                    child_stack_start,
                    stack_len: self.frames.len(),
                })?;
        let Some(parent) = self.frames.get(parent_index) else {
            return Err(ResolutionStackError::InvalidChildBoundary {
                child_stack_start,
                stack_len: self.frames.len(),
            });
        };
        let ResolutionFrame::ChangeZone(frame) = parent else {
            return Err(ResolutionStackError::UnexpectedChildBoundaryParent {
                child_stack_start,
                expected: FrameKind::ChangeZone,
                actual: parent.kind(),
            });
        };
        if !frame.pending.as_ref().is_some_and(|current| {
            current.logical_zone_change_group.logical_group_id == logical_group_id
        }) {
            return Err(ResolutionStackError::MismatchedChangeZoneBoundaryOwner {
                child_stack_start,
            });
        }
        let devour_eligible_snapshot = frame.devour_eligible_snapshot.clone();
        self.frames[parent_index] = ResolutionFrame::ChangeZone(Box::new(ChangeZoneFrame {
            pending: Some(pending),
            devour_eligible_snapshot,
        }));
        Ok(())
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

    /// Mutably accesses a BatchDelivery owner while its exact active
    /// PostReplacement child finishes an as-enters copy choice. This fixed
    /// parent/child relation lets the copy-choice path record the resumed zone
    /// delivery before it retires the child; it is deliberately not a search
    /// for a buried batch frame.
    pub fn active_batch_delivery_or_post_replacement_child_mut(
        &mut self,
    ) -> Option<&mut PendingBatchDeliveries> {
        let parent_index = self.frames.len().checked_sub(2);
        match self.frames.last() {
            Some(ResolutionFrame::BatchDelivery(_)) => match self.frames.last_mut() {
                Some(ResolutionFrame::BatchDelivery(frame)) => Some(frame),
                Some(_) | None => unreachable!("checked active batch frame must match"),
            },
            Some(ResolutionFrame::PostReplacement(_)) => match parent_index {
                Some(index) => match self.frames.get_mut(index) {
                    Some(ResolutionFrame::BatchDelivery(frame)) => Some(frame),
                    Some(_) | None => None,
                },
                None => None,
            },
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

    /// Returns the CounterMoves queue only when its typed frame owns the stack
    /// top. A buried counter queue is a parent dependency, never a fallback.
    pub fn active_counter_moves(&self) -> Option<&PendingCounterMoveQueue> {
        match self.last() {
            Some(ResolutionFrame::CounterMoves(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable top-only access to the active CounterMoves queue.
    pub fn active_counter_moves_mut(&mut self) -> Option<&mut PendingCounterMoveQueue> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::CounterMoves(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active CounterMoves queue after its final entry
    /// settles.
    pub fn take_active_counter_moves(
        &mut self,
    ) -> Result<Option<PendingCounterMoveQueue>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::CounterMoves(_)) => {
                let ResolutionFrame::CounterMoves(frame) =
                    self.pop_expected(FrameKind::CounterMoves)?
                else {
                    unreachable!("checked counter-moves frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CounterMoves,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a new CounterMoves queue as the active inner frame.
    pub fn push_counter_moves(&mut self, pending: PendingCounterMoveQueue) {
        self.push_inner(ResolutionFrame::CounterMoves(pending));
    }

    /// Re-park the active CounterMoves queue after it advances or pauses again.
    pub fn replace_active_counter_moves(
        &mut self,
        pending: PendingCounterMoveQueue,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::CounterMoves(_)) => {
                self.replace_active(ResolutionFrame::CounterMoves(pending))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CounterMoves,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the CounterRemovals queue only when its typed frame owns the
    /// stack top. A buried counter queue is a parent dependency, never a
    /// fallback.
    pub fn active_counter_removals(&self) -> Option<&PendingCounterRemovalQueue> {
        match self.last() {
            Some(ResolutionFrame::CounterRemovals(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable top-only access to the active CounterRemovals queue.
    pub fn active_counter_removals_mut(&mut self) -> Option<&mut PendingCounterRemovalQueue> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::CounterRemovals(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active CounterRemovals queue after its final entry
    /// settles.
    pub fn take_active_counter_removals(
        &mut self,
    ) -> Result<Option<PendingCounterRemovalQueue>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::CounterRemovals(_)) => {
                let ResolutionFrame::CounterRemovals(frame) =
                    self.pop_expected(FrameKind::CounterRemovals)?
                else {
                    unreachable!("checked counter-removals frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CounterRemovals,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a new CounterRemovals queue as the active inner frame.
    pub fn push_counter_removals(&mut self, pending: PendingCounterRemovalQueue) {
        self.push_inner(ResolutionFrame::CounterRemovals(pending));
    }

    /// Re-park the active CounterRemovals queue after it advances or pauses
    /// again.
    pub fn replace_active_counter_removals(
        &mut self,
        pending: PendingCounterRemovalQueue,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::CounterRemovals(_)) => {
                self.replace_active(ResolutionFrame::CounterRemovals(pending))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CounterRemovals,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the CounterAdditions queue only when its typed frame owns the
    /// stack top. A buried counter queue is a parent dependency, never a
    /// fallback.
    pub fn active_counter_additions(&self) -> Option<&PendingCounterAdditionQueue> {
        match self.last() {
            Some(ResolutionFrame::CounterAdditions(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable top-only access to the active CounterAdditions queue.
    pub fn active_counter_additions_mut(&mut self) -> Option<&mut PendingCounterAdditionQueue> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::CounterAdditions(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active CounterAdditions queue after its final entry
    /// and completion settle.
    pub fn take_active_counter_additions(
        &mut self,
    ) -> Result<Option<PendingCounterAdditionQueue>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::CounterAdditions(_)) => {
                let ResolutionFrame::CounterAdditions(frame) =
                    self.pop_expected(FrameKind::CounterAdditions)?
                else {
                    unreachable!("checked counter-additions frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CounterAdditions,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a new CounterAdditions queue as the active inner frame.
    pub fn push_counter_additions(&mut self, pending: PendingCounterAdditionQueue) {
        self.push_inner(ResolutionFrame::CounterAdditions(pending));
    }

    /// Re-park the active CounterAdditions queue after it advances or pauses
    /// again.
    pub fn replace_active_counter_additions(
        &mut self,
        pending: PendingCounterAdditionQueue,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::CounterAdditions(_)) => {
                self.replace_active(ResolutionFrame::CounterAdditions(pending))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CounterAdditions,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the CopyToken owner only when its typed frame owns the stack top.
    pub fn active_copy_token(&self) -> Option<&PendingCopyTokenResolution> {
        match self.last() {
            Some(ResolutionFrame::CopyToken(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable top-only access to the active CopyToken owner.
    pub fn active_copy_token_mut(&mut self) -> Option<&mut PendingCopyTokenResolution> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::CopyToken(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active CopyToken owner after its remaining batches
    /// settle.
    pub fn take_active_copy_token(
        &mut self,
    ) -> Result<Option<PendingCopyTokenResolution>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::CopyToken(_)) => {
                let ResolutionFrame::CopyToken(frame) = self.pop_expected(FrameKind::CopyToken)?
                else {
                    unreachable!("checked copy-token frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CopyToken,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a new CopyToken owner as the active inner frame.
    pub fn push_copy_token(&mut self, pending: PendingCopyTokenResolution) {
        self.push_inner(ResolutionFrame::CopyToken(pending));
    }

    /// Re-park the active CopyToken owner after it advances or pauses again.
    pub fn replace_active_copy_token(
        &mut self,
        pending: PendingCopyTokenResolution,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::CopyToken(_)) => {
                self.replace_active(ResolutionFrame::CopyToken(pending))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CopyToken,
                actual: frame.kind(),
            }),
        }
    }

    /// Insert a CopyToken parent below the complete child stack its batch
    /// created after the producer recorded that stack boundary.
    pub fn insert_copy_token_parent_at_child_boundary(
        &mut self,
        pending: PendingCopyTokenResolution,
        child_stack_start: usize,
    ) -> Result<(), ResolutionStackError> {
        self.insert_parent_at_child_boundary(ResolutionFrame::CopyToken(pending), child_stack_start)
    }

    /// Returns the EachPlayerCopyChosen owner only when its typed frame owns
    /// the stack top.
    pub fn active_each_player_copy_chosen(&self) -> Option<&PendingEachPlayerCopyChosen> {
        match self.last() {
            Some(ResolutionFrame::EachPlayerCopyChosen(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutable top-only access to the active EachPlayerCopyChosen owner.
    pub fn active_each_player_copy_chosen_mut(
        &mut self,
    ) -> Option<&mut PendingEachPlayerCopyChosen> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::EachPlayerCopyChosen(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consume exactly the active EachPlayerCopyChosen owner after its child
    /// copy or counter work settles.
    pub fn take_active_each_player_copy_chosen(
        &mut self,
    ) -> Result<Option<PendingEachPlayerCopyChosen>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::EachPlayerCopyChosen(_)) => {
                let ResolutionFrame::EachPlayerCopyChosen(frame) =
                    self.pop_expected(FrameKind::EachPlayerCopyChosen)?
                else {
                    unreachable!("checked each-player-copy-chosen frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::EachPlayerCopyChosen,
                actual: frame.kind(),
            }),
        }
    }

    /// Park a new EachPlayerCopyChosen owner as the active inner frame.
    pub fn push_each_player_copy_chosen(&mut self, pending: PendingEachPlayerCopyChosen) {
        self.push_inner(ResolutionFrame::EachPlayerCopyChosen(pending));
    }

    /// Re-park the active EachPlayerCopyChosen owner after it advances or
    /// pauses again.
    pub fn replace_active_each_player_copy_chosen(
        &mut self,
        pending: PendingEachPlayerCopyChosen,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::EachPlayerCopyChosen(_)) => {
                self.replace_active(ResolutionFrame::EachPlayerCopyChosen(pending))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::EachPlayerCopyChosen,
                actual: frame.kind(),
            }),
        }
    }

    /// Insert an EachPlayerCopyChosen parent below the complete child stack
    /// its current copy or counter step created after the producer recorded
    /// that stack boundary.
    pub fn insert_each_player_copy_chosen_parent_at_child_boundary(
        &mut self,
        pending: PendingEachPlayerCopyChosen,
        child_stack_start: usize,
    ) -> Result<(), ResolutionStackError> {
        self.insert_parent_at_child_boundary(
            ResolutionFrame::EachPlayerCopyChosen(pending),
            child_stack_start,
        )
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

    /// Returns the repeated optional-payment owner only when it owns the
    /// stack top.
    pub fn active_repeated_optional_payment(&self) -> Option<&RepeatedOptionalPaymentFrame> {
        match self.last() {
            Some(ResolutionFrame::RepeatedOptionalPayment(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active repeated optional-payment owner.
    pub fn active_repeated_optional_payment_mut(
        &mut self,
    ) -> Option<&mut RepeatedOptionalPaymentFrame> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::RepeatedOptionalPayment(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active repeated optional-payment owner.
    pub fn take_active_repeated_optional_payment(
        &mut self,
    ) -> Result<Option<RepeatedOptionalPaymentFrame>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::RepeatedOptionalPayment(_)) => {
                let ResolutionFrame::RepeatedOptionalPayment(frame) =
                    self.pop_expected(FrameKind::RepeatedOptionalPayment)?
                else {
                    unreachable!("checked repeated optional-payment frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::RepeatedOptionalPayment,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks a newly active repeated optional-payment owner.
    pub fn push_repeated_optional_payment(&mut self, frame: RepeatedOptionalPaymentFrame) {
        self.push_inner(ResolutionFrame::RepeatedOptionalPayment(frame));
    }

    /// Re-parks the active repeated optional-payment owner after it advances
    /// to the next payment prompt.
    pub fn replace_active_repeated_optional_payment(
        &mut self,
        frame: RepeatedOptionalPaymentFrame,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::RepeatedOptionalPayment(_)) => {
                self.replace_active(ResolutionFrame::RepeatedOptionalPayment(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::RepeatedOptionalPayment,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the optional-effect owner only when it owns the stack top.
    pub fn active_optional_effect(&self) -> Option<&OptionalEffectFrame> {
        match self.last() {
            Some(ResolutionFrame::OptionalEffect(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active optional-effect owner.
    pub fn active_optional_effect_mut(&mut self) -> Option<&mut OptionalEffectFrame> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::OptionalEffect(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active optional-effect frame.
    pub fn take_active_optional_effect(
        &mut self,
    ) -> Result<Option<OptionalEffectFrame>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::OptionalEffect(_)) => {
                let ResolutionFrame::OptionalEffect(frame) =
                    self.pop_expected(FrameKind::OptionalEffect)?
                else {
                    unreachable!("checked optional-effect frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::OptionalEffect,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks a newly active optional-effect owner.
    pub fn push_optional_effect(&mut self, frame: OptionalEffectFrame) {
        self.push_inner(ResolutionFrame::OptionalEffect(frame));
    }

    /// Re-parks the active optional-effect owner without an empty-stack interval.
    pub fn replace_active_optional_effect(
        &mut self,
        frame: OptionalEffectFrame,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::OptionalEffect(_)) => {
                self.replace_active(ResolutionFrame::OptionalEffect(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::OptionalEffect,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the coin-flip owner only when it owns the stack top.
    pub fn active_coin_flip(&self) -> Option<&PendingCoinFlip> {
        match self.last() {
            Some(ResolutionFrame::CoinFlip(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active coin-flip owner.
    pub fn active_coin_flip_mut(&mut self) -> Option<&mut PendingCoinFlip> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::CoinFlip(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active coin-flip owner after a kept result is
    /// selected.
    pub fn take_active_coin_flip(
        &mut self,
    ) -> Result<Option<PendingCoinFlip>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::CoinFlip(_)) => {
                let ResolutionFrame::CoinFlip(frame) = self.pop_expected(FrameKind::CoinFlip)?
                else {
                    unreachable!("checked coin-flip frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CoinFlip,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks one Krark's Thumb keep-choice resolution.
    pub fn push_coin_flip(&mut self, frame: PendingCoinFlip) {
        self.push_inner(ResolutionFrame::CoinFlip(frame));
    }

    /// Re-parks the active coin-flip owner without exposing an empty-stack
    /// interval between consecutive keep choices.
    pub fn replace_active_coin_flip(
        &mut self,
        frame: PendingCoinFlip,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::CoinFlip(_)) => {
                self.replace_active(ResolutionFrame::CoinFlip(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::CoinFlip,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the proliferate owner only when it owns the stack top.
    pub fn active_proliferate(&self) -> Option<&PendingProliferateActions> {
        match self.last() {
            Some(ResolutionFrame::Proliferate(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active proliferate owner.
    pub fn active_proliferate_mut(&mut self) -> Option<&mut PendingProliferateActions> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::Proliferate(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active proliferate owner after its target choice.
    pub fn take_active_proliferate(
        &mut self,
    ) -> Result<Option<PendingProliferateActions>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::Proliferate(_)) => {
                let ResolutionFrame::Proliferate(frame) =
                    self.pop_expected(FrameKind::Proliferate)?
                else {
                    unreachable!("checked proliferate frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::Proliferate,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks one proliferate target-choice resolution.
    pub fn push_proliferate(&mut self, frame: PendingProliferateActions) {
        self.push_inner(ResolutionFrame::Proliferate(frame));
    }

    /// Re-parks the active proliferate owner without exposing an empty-stack
    /// interval between replacement-produced proliferate choices.
    pub fn replace_active_proliferate(
        &mut self,
        frame: PendingProliferateActions,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::Proliferate(_)) => {
                self.replace_active(ResolutionFrame::Proliferate(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::Proliferate,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns the mutate-merge owner only when it owns the stack top.
    pub fn active_mutate_merge(&self) -> Option<&PendingMutateMerge> {
        match self.last() {
            Some(ResolutionFrame::MutateMerge(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active mutate-merge owner.
    pub fn active_mutate_merge_mut(&mut self) -> Option<&mut PendingMutateMerge> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::MutateMerge(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active mutate-merge owner after its controller
    /// chooses which component is on top.
    pub fn take_active_mutate_merge(
        &mut self,
    ) -> Result<Option<PendingMutateMerge>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::MutateMerge(_)) => {
                let ResolutionFrame::MutateMerge(frame) =
                    self.pop_expected(FrameKind::MutateMerge)?
                else {
                    unreachable!("checked mutate-merge frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::MutateMerge,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks one mutate-merge top/bottom choice resolution.
    pub fn push_mutate_merge(&mut self, frame: PendingMutateMerge) {
        self.push_inner(ResolutionFrame::MutateMerge(frame));
    }

    /// Re-parks the active mutate-merge owner without exposing an empty-stack
    /// interval.
    pub fn replace_active_mutate_merge(
        &mut self,
        frame: PendingMutateMerge,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::MutateMerge(_)) => {
                self.replace_active(ResolutionFrame::MutateMerge(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::MutateMerge,
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

    /// Returns the complete draw authority only when its MultiDraw frame owns
    /// the active stack top.
    pub fn active_multi_draw(&self) -> Option<&MultiDrawFrame> {
        match self.last() {
            Some(ResolutionFrame::MultiDraw(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active MultiDraw frame.
    pub fn active_multi_draw_mut(&mut self) -> Option<&mut MultiDrawFrame> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::MultiDraw(frame)) => Some(frame),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active MultiDraw frame after all of its draw work
    /// and any embedded connive link have settled.
    pub fn take_active_multi_draw(
        &mut self,
    ) -> Result<Option<MultiDrawFrame>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::MultiDraw(_)) => {
                let ResolutionFrame::MultiDraw(frame) = self.pop_expected(FrameKind::MultiDraw)?
                else {
                    unreachable!("checked multi-draw frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::MultiDraw,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks a complete in-flight draw authority as the active inner frame.
    pub fn push_multi_draw(&mut self, frame: MultiDrawFrame) {
        self.observe_draw_sequence_frame_id(frame.draw_sequences.next_frame_id());
        self.push_inner(ResolutionFrame::MultiDraw(frame));
    }

    /// Re-parks the active MultiDraw authority without an empty-stack interval.
    pub fn replace_active_multi_draw(
        &mut self,
        frame: MultiDrawFrame,
    ) -> Result<(), ResolutionStackError> {
        self.observe_draw_sequence_frame_id(frame.draw_sequences.next_frame_id());
        match self.last() {
            Some(ResolutionFrame::MultiDraw(_)) => {
                self.replace_active(ResolutionFrame::MultiDraw(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::MultiDraw,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns a standalone Connive re-entry only when it owns the active
    /// stack top. A re-entry coupled to an active draw belongs in that draw's
    /// `MultiDrawFrame` instead, preserving the draw's parent adjacency.
    pub fn active_connive_reentry(&self) -> Option<&PendingConniveReentry> {
        match self.last() {
            Some(ResolutionFrame::ConniveReentry(pending)) => Some(pending),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only a standalone active Connive re-entry.
    pub fn active_connive_reentry_mut(&mut self) -> Option<&mut PendingConniveReentry> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::ConniveReentry(pending)) => Some(pending),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active standalone Connive re-entry frame.
    pub fn take_active_connive_reentry(
        &mut self,
    ) -> Result<Option<PendingConniveReentry>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::ConniveReentry(_)) => {
                let ResolutionFrame::ConniveReentry(pending) =
                    self.pop_expected(FrameKind::ConniveReentry)?
                else {
                    unreachable!("checked connive re-entry frame kind must match")
                };
                Ok(Some(pending))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::ConniveReentry,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks a standalone Connive re-entry when no active draw owns it.
    pub fn push_connive_reentry(&mut self, pending: PendingConniveReentry) {
        self.push_inner(ResolutionFrame::ConniveReentry(pending));
    }

    /// Returns a life-total assignment tail only when it owns the active stack
    /// top.
    pub fn active_life_total_assignment(&self) -> Option<&PendingLifeTotalAssignment> {
        match self.last() {
            Some(ResolutionFrame::LifeTotalAssignment(pending)) => Some(pending),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active life-total assignment tail.
    pub fn active_life_total_assignment_mut(&mut self) -> Option<&mut PendingLifeTotalAssignment> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::LifeTotalAssignment(pending)) => Some(pending),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active life-total assignment frame.
    pub fn take_active_life_total_assignment(
        &mut self,
    ) -> Result<Option<PendingLifeTotalAssignment>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::LifeTotalAssignment(_)) => {
                let ResolutionFrame::LifeTotalAssignment(pending) =
                    self.pop_expected(FrameKind::LifeTotalAssignment)?
                else {
                    unreachable!("checked life-total assignment frame kind must match")
                };
                Ok(Some(pending))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::LifeTotalAssignment,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks a life-total assignment tail above the replacement choice that
    /// suspended it.
    pub fn push_life_total_assignment(&mut self, pending: PendingLifeTotalAssignment) {
        self.push_inner(ResolutionFrame::LifeTotalAssignment(pending));
    }

    /// Returns permanent-spell completion context only when its frame owns the
    /// active stack top.
    pub fn active_spell_resolution(&self) -> Option<&PendingSpellResolution> {
        match self.last() {
            Some(ResolutionFrame::SpellResolution(pending)) => Some(pending),
            Some(_) | None => None,
        }
    }

    /// Mutably accesses only the active permanent-spell completion context.
    pub fn active_spell_resolution_mut(&mut self) -> Option<&mut PendingSpellResolution> {
        match self.frames.last_mut() {
            Some(ResolutionFrame::SpellResolution(pending)) => Some(pending),
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active permanent-spell completion frame.
    pub fn take_active_spell_resolution(
        &mut self,
    ) -> Result<Option<PendingSpellResolution>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::SpellResolution(_)) => {
                let ResolutionFrame::SpellResolution(pending) =
                    self.pop_expected(FrameKind::SpellResolution)?
                else {
                    unreachable!("checked spell-resolution frame kind must match")
                };
                Ok(Some(pending))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::SpellResolution,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks permanent-spell completion context above the replacement choice
    /// that suspended its entry.
    pub fn push_spell_resolution(&mut self, pending: PendingSpellResolution) {
        self.push_inner(ResolutionFrame::SpellResolution(pending));
    }

    /// Returns the active general replacement drain. It may be the exact
    /// immediate parent of the active child raised while its continuation
    /// dispatches; there is intentionally no general frame search.
    pub fn active_post_replacement_or_paired_parent(&self) -> Option<&PostReplacementDrainStack> {
        let index = self.active_post_replacement_parent_index()?;
        match self.frames.get(index) {
            Some(ResolutionFrame::PostReplacement(drains)) => Some(drains),
            Some(_) | None => unreachable!("checked post-replacement parent must match"),
        }
    }

    /// Mutably accesses the active general drain or its exact immediate parent.
    pub fn active_post_replacement_or_paired_parent_mut(
        &mut self,
    ) -> Option<&mut PostReplacementDrainStack> {
        let index = self.active_post_replacement_parent_index()?;
        match self.frames.get_mut(index) {
            Some(ResolutionFrame::PostReplacement(drains)) => Some(drains),
            Some(_) | None => unreachable!("checked post-replacement parent must match"),
        }
    }

    /// Removes only the active child immediately above a post-replacement
    /// frame. This is the abandonment boundary for work raised by that
    /// dispatch; it never reaches through a child or searches for a buried
    /// parent.
    pub fn take_active_post_replacement_child(&mut self) -> Option<ResolutionFrame> {
        let parent_index = self.active_post_replacement_parent_index()?;
        let child_index = self.frames.len().checked_sub(1)?;
        if parent_index.checked_add(1) != Some(child_index) {
            return None;
        }
        self.frames.pop()
    }

    /// Finds the active post-replacement authority or its one direct child.
    /// The two legal shapes are `[... PostReplacement]` and
    /// `[... PostReplacement, child]`; any deeper relationship is deliberately
    /// invisible here so callers cannot turn this into a generic frame search.
    fn active_post_replacement_parent_index(&self) -> Option<usize> {
        let parent_index = self.frames.len().checked_sub(1)?;
        if matches!(
            self.frames.get(parent_index),
            Some(ResolutionFrame::PostReplacement(_))
        ) {
            return Some(parent_index);
        }

        let parent_index = parent_index.checked_sub(1)?;
        matches!(
            self.frames.get(parent_index),
            Some(ResolutionFrame::PostReplacement(_))
        )
        .then_some(parent_index)
    }

    /// Returns the active ChangeZone frame, or its exact immediate parent
    /// while a post-replacement child raised by that zone change is active.
    /// This is the one Devour snapshot relationship that survives a paused
    /// as-enters replacement; it deliberately does not search deeper frames.
    pub fn active_change_zone_or_post_replacement_child(&self) -> Option<&ChangeZoneFrame> {
        match self.last() {
            Some(ResolutionFrame::ChangeZone(frame)) => Some(frame),
            Some(ResolutionFrame::PostReplacement(_)) => match self.active_predecessor() {
                Some(ResolutionFrame::ChangeZone(frame)) => Some(frame),
                Some(_) | None => None,
            },
            Some(_) | None => None,
        }
    }

    /// Consumes exactly the active general post-replacement frame.
    pub fn take_active_post_replacement(
        &mut self,
    ) -> Result<Option<PostReplacementDrainStack>, ResolutionStackError> {
        match self.last() {
            None => Ok(None),
            Some(ResolutionFrame::PostReplacement(_)) => {
                let ResolutionFrame::PostReplacement(frame) =
                    self.pop_expected(FrameKind::PostReplacement)?
                else {
                    unreachable!("checked post-replacement frame kind must match")
                };
                Ok(Some(frame))
            }
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::PostReplacement,
                actual: frame.kind(),
            }),
        }
    }

    /// Parks a complete general post-replacement drain as the active inner frame.
    pub fn push_post_replacement(&mut self, frame: PostReplacementDrainStack) {
        self.push_inner(ResolutionFrame::PostReplacement(frame));
    }

    /// Re-parks the active general post-replacement frame without clearing its
    /// resident event context.
    pub fn replace_active_post_replacement(
        &mut self,
        frame: PostReplacementDrainStack,
    ) -> Result<(), ResolutionStackError> {
        match self.last() {
            Some(ResolutionFrame::PostReplacement(_)) => {
                self.replace_active(ResolutionFrame::PostReplacement(frame))
            }
            None => Err(ResolutionStackError::Empty),
            Some(frame) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::PostReplacement,
                actual: frame.kind(),
            }),
        }
    }

    /// Returns only the immediate predecessor of the active frame.
    ///
    /// This is intentionally narrower than a frame search: it serves only the
    /// paused-drain/draw adjacency.
    pub fn active_predecessor(&self) -> Option<&ResolutionFrame> {
        self.frames.get(self.frames.len().checked_sub(2)?)
    }

    /// True only for the live general-drain/draw pair at the active stack
    /// boundary. This inspects the top and its exact immediate predecessor; it
    /// is not a search for a buried PostReplacement frame.
    pub fn has_active_post_replacement_draw_pair(&self) -> bool {
        matches!(
            (self.active_predecessor(), self.last()),
            (
                Some(ResolutionFrame::PostReplacement(drains)),
                Some(ResolutionFrame::MultiDraw(_)),
            ) if matches!(
                drains.resident().map(|drain| &drain.status),
                Some(DrainStatus::Paused | DrainStatus::Dispatching)
            )
        )
    }

    /// Returns the continuation immediately outside the active paused
    /// post-replacement/draw pair. This fixed three-frame relationship exists
    /// while a replacement-produced draw is paused for a choice: the
    /// continuation stays outside the pair so `PostReplacement` remains the
    /// draw's exact immediate parent. This is positional access, not a frame
    /// search, and it never authorizes taking the outer continuation early.
    pub fn outer_ability_continuation_of_active_post_replacement_draw_pair(
        &self,
    ) -> Option<&AbilityContinuationFrame> {
        let post_replacement_index = self.frames.len().checked_sub(2)?;
        let continuation_index = post_replacement_index.checked_sub(1)?;
        match (
            self.frames.get(continuation_index),
            self.frames.get(post_replacement_index),
            self.last(),
        ) {
            (
                Some(ResolutionFrame::AbilityContinuation(continuation)),
                Some(ResolutionFrame::PostReplacement(drains)),
                Some(ResolutionFrame::MultiDraw(_)),
            ) if matches!(
                drains.resident().map(|drain| &drain.status),
                Some(DrainStatus::Paused)
            ) =>
            {
                Some(continuation)
            }
            _ => None,
        }
    }

    /// Mutably accesses only the continuation immediately outside the active
    /// paused post-replacement/draw pair. See the immutable companion for the
    /// structural invariant this preserves.
    pub fn outer_ability_continuation_of_active_post_replacement_draw_pair_mut(
        &mut self,
    ) -> Option<&mut AbilityContinuationFrame> {
        let continuation_index = self.frames.len().checked_sub(3)?;
        self.outer_ability_continuation_of_active_post_replacement_draw_pair()?;
        match self.frames.get_mut(continuation_index) {
            Some(ResolutionFrame::AbilityContinuation(continuation)) => Some(continuation),
            Some(_) | None => {
                unreachable!("checked paired continuation must retain its frame kind")
            }
        }
    }

    /// Inserts a continuation outside the active general-drain/draw pair.
    ///
    /// The continuation is the draw's later instruction, so it must resume
    /// after the draw while the resident drain still exposes its event context.
    /// Keeping it below the pair preserves the required immediate
    /// `PostReplacement` → `MultiDraw` relationship for the duration of the
    /// paused draw.
    pub fn insert_ability_continuation_outside_active_post_replacement_draw(
        &mut self,
        frame: AbilityContinuationFrame,
    ) -> Result<(), ResolutionStackError> {
        if !self.has_active_post_replacement_draw_pair() {
            return match self.last() {
                None => Err(ResolutionStackError::NoActiveChild),
                Some(actual) => Err(ResolutionStackError::UnexpectedTop {
                    expected: FrameKind::MultiDraw,
                    actual: actual.kind(),
                }),
            };
        }
        let post_replacement_index = self
            .frames
            .len()
            .checked_sub(2)
            .expect("the checked adjacent pair has a parent");
        self.frames.insert(
            post_replacement_index,
            ResolutionFrame::AbilityContinuation(frame),
        );
        Ok(())
    }

    /// After the active child draw completes, makes its outer continuation the
    /// new active child of the resident post-replacement drain. The operation
    /// is a fixed two-frame swap after the MultiDraw child has been popped; it
    /// never searches the stack or removes the drain.
    pub fn promote_ability_continuation_after_post_replacement_draw(
        &mut self,
    ) -> Result<bool, ResolutionStackError> {
        let post_replacement_index = self
            .frames
            .len()
            .checked_sub(1)
            .ok_or(ResolutionStackError::Empty)?;
        let Some(continuation_index) = post_replacement_index.checked_sub(1) else {
            return Ok(false);
        };
        match (
            self.frames.get(continuation_index),
            self.frames.get(post_replacement_index),
        ) {
            (
                Some(ResolutionFrame::AbilityContinuation(_)),
                Some(ResolutionFrame::PostReplacement(drains)),
            ) if matches!(
                drains.resident().map(|drain| &drain.status),
                Some(DrainStatus::Paused)
            ) =>
            {
                self.frames.swap(continuation_index, post_replacement_index);
                Ok(true)
            }
            (Some(_), Some(ResolutionFrame::PostReplacement(_))) => Ok(false),
            (_, Some(actual)) => Err(ResolutionStackError::UnexpectedTop {
                expected: FrameKind::PostReplacement,
                actual: actual.kind(),
            }),
            (_, None) => unreachable!("checked active post-replacement index exists"),
        }
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ResolutionFrame> {
        self.frames.iter()
    }

    /// Park work that is inside the current active operation.
    pub fn push_inner(&mut self, frame: ResolutionFrame) {
        if let ResolutionFrame::MultiDraw(draw) = &frame {
            self.observe_draw_sequence_frame_id(draw.draw_sequences.next_frame_id());
        }
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
        if let ResolutionFrame::MultiDraw(draw) = &frame {
            self.observe_draw_sequence_frame_id(draw.draw_sequences.next_frame_id());
        }
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
        let ResolutionFrame::MultiDraw(draw) = &child else {
            unreachable!("validated adjacent child must be multi-draw")
        };
        self.observe_draw_sequence_frame_id(draw.draw_sequences.next_frame_id());
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
        let multi_draw_count = self
            .frames
            .iter()
            .filter(|frame| matches!(frame, ResolutionFrame::MultiDraw(_)))
            .count();
        if multi_draw_count > 1 {
            return Err(ResolutionStackError::InvalidPayload {
                frame: FrameKind::MultiDraw,
                message: "multiple multi-draw frames split one draw authority".to_string(),
            });
        }
        let has_multi_draw = multi_draw_count == 1;
        let mut direct_choice_count = 0;
        let mut buried_direct_choice = None;
        for (index, frame) in self.frames.iter().enumerate() {
            if matches!(frame.gate(), FrameGate::DirectChoice(_)) {
                direct_choice_count += 1;
                if index + 1 != self.frames.len() {
                    buried_direct_choice = Some(frame.kind());
                }
            }
            if let ResolutionFrame::MultiDraw(draw) = frame {
                draw.draw_sequences.validate().map_err(|message| {
                    ResolutionStackError::InvalidPayload {
                        frame: FrameKind::MultiDraw,
                        message,
                    }
                })?;
                if draw.draw_sequences.next_frame_id() > self.next_draw_sequence_frame_id {
                    return Err(ResolutionStackError::InvalidPayload {
                        frame: FrameKind::MultiDraw,
                        message: "the resolution-stack draw allocator is behind its active frame"
                            .to_string(),
                    });
                }
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

        if direct_choice_count > 1 {
            return Err(ResolutionStackError::MultipleDirectChoiceOwners);
        }
        if let Some(frame) = buried_direct_choice {
            return Err(ResolutionStackError::InvalidPayload {
                frame,
                message: "a direct-choice owner is buried below another resolution frame"
                    .to_string(),
            });
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

/// The full-state resolution wire version for typed [`ResolutionStack`] frames.
///
/// Version 2 is the compatibility boundary for protocol-19 clients. Phase 4
/// hardening changes neither this serialized shape nor the version.
pub const RESOLUTION_STATE_WIRE_VERSION: u64 = 2;

/// Historical full-state resolution wire version accepted only for migration.
///
/// The reader accepts v1 saves and converts them to v2 frames; the writer
/// never emits v1 resolution fields.
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

    /// Decodes persisted full-game state at the resolution compatibility boundary.
    ///
    /// Version 1 is read only through the legacy migration path below. Version
    /// 2 is the only shape this adapter writes for protocol-19 clients.
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
            // V1 reader compatibility path: historical keys are consumed here
            // and projected into typed frames before runtime state is restored.
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
                let legacy_counter_moves = LegacyCounterMovesWire::from_value(&value)?;
                let legacy_counter_removals = LegacyCounterRemovalsWire::from_value(&value)?;
                let legacy_counter_additions = LegacyCounterAdditionsWire::from_value(&value)?;
                let legacy_copy_token = LegacyCopyTokenWire::from_value(&value)?;
                let legacy_each_player_copy_chosen =
                    LegacyEachPlayerCopyChosenWire::from_value(&value)?;
                let legacy_choose_one_of = LegacyChooseOneOfWire::from_value(&value)?;
                let legacy_vote_ballot = LegacyVoteBallotWire::from_value(&value)?;
                let legacy_per_player_zone_choice =
                    LegacyPerPlayerZoneChoiceWire::from_value(&value)?;
                let legacy_per_category_zone_choice =
                    LegacyPerCategoryZoneChoiceWire::from_value(&value)?;
                let legacy_repeated_optional_payment =
                    LegacyRepeatedOptionalPaymentWire::from_value(&value)?;
                let legacy_optional_effect = LegacyOptionalEffectWire::from_value(&value)?;
                let legacy_coin_flip = LegacyCoinFlipWire::from_value(&value)?;
                let legacy_proliferate = LegacyProliferateWire::from_value(&value)?;
                let legacy_mutate_merge = LegacyMutateMergeWire::from_value(&value)?;
                let legacy_replacement_tails = LegacyReplacementTailsWire::from_value(&value)?;
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
                legacy_object.remove("pending_counter_moves");
                legacy_object.remove("pending_counter_removals");
                legacy_object.remove("pending_counter_additions");
                legacy_object.remove("pending_copy_token_resolution");
                legacy_object.remove("pending_each_player_copy_chosen");
                legacy_object.remove("pending_choose_one_of");
                legacy_object.remove("pending_vote_ballot_iteration");
                legacy_object.remove("pending_per_player_zone_choice");
                legacy_object.remove("pending_per_category_zone_choice");
                legacy_object.remove("pending_repeated_optional_payment");
                legacy_object.remove("optional_cost_payments_this_resolution");
                legacy_object.remove("pending_optional_effect");
                legacy_object.remove("pending_optional_trigger_event");
                legacy_object.remove("pending_optional_trigger_match_count");
                legacy_object.remove("pending_coin_flip");
                legacy_object.remove("pending_proliferate_actions");
                legacy_object.remove("pending_mutate_merge");
                legacy_object.remove("draw_sequences");
                legacy_object.remove("pending_multi_draw");
                legacy_object.remove("pending_connive_reentry");
                legacy_object.remove("pending_life_total_assignment");
                legacy_object.remove("pending_spell_resolution");
                legacy_object.remove("post_replacement_drains");
                legacy_object.remove("post_replacement_effect");
                legacy_object.remove("post_replacement_resolved_effect");
                legacy_object.remove("post_replacement_continuation");
                legacy_object.remove("post_replacement_source");
                legacy_object.remove("post_replacement_applied");
                legacy_object.remove("post_replacement_event_source");
                legacy_object.remove("post_replacement_event_target");
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
                if let Some(frame) = legacy_counter_moves.into_frame() {
                    legacy.push_counter_moves(frame);
                }
                if let Some(frame) = legacy_counter_removals.into_frame() {
                    legacy.push_counter_removals(frame);
                }
                // A per-player copy walk parks beneath its inner token-copy
                // work, and CopyToken in turn parks beneath an ETB-counter
                // child. Rebuild that exact outer-to-inner order from the v1
                // independent slots before top-only resume dispatch sees it.
                if let Some(frame) = legacy_each_player_copy_chosen.into_frame() {
                    legacy.push_each_player_copy_chosen(frame);
                }
                if let Some(frame) = legacy_copy_token.into_frame() {
                    legacy.push_copy_token(frame);
                }
                if let Some(frame) = legacy_counter_additions.into_frame() {
                    legacy.push_counter_additions(frame);
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
                if let Some(frame) = legacy_repeated_optional_payment.into_frame() {
                    legacy.push_repeated_optional_payment_frame(frame);
                }
                if let Some(frame) = legacy_optional_effect.into_frame()? {
                    legacy.push_optional_effect_frame(frame);
                }
                if let Some(frame) = legacy_coin_flip.into_frame() {
                    legacy.push_coin_flip_frame(frame);
                }
                if let Some(frame) = legacy_proliferate.into_frame() {
                    legacy.push_proliferate_frame(frame);
                }
                if let Some(frame) = legacy_mutate_merge.into_frame() {
                    legacy.push_mutate_merge_frame(frame);
                }
                let (tail_frames, next_draw_sequence_frame_id) =
                    legacy_replacement_tails.into_frames()?;
                legacy
                    .resolution_stack
                    .observe_draw_sequence_frame_id(next_draw_sequence_frame_id);
                if let [parent @ ResolutionFrame::PostReplacement(_), child @ ResolutionFrame::MultiDraw(_)] =
                    tail_frames.as_slice()
                {
                    legacy
                        .resolution_stack
                        .install_adjacent_post_replacement_draw(parent.clone(), child.clone())
                        .map_err(|error| error.to_string())?;
                } else {
                    for frame in tail_frames {
                        legacy.resolution_stack.push_inner(frame);
                    }
                }
                let frames = canonicalize_legacy_resolution_state(&legacy)?;
                frames
                    .validate(&legacy.waiting_for)
                    .map_err(|error| error.to_string())?;
                #[cfg(debug_assertions)]
                debug_assert_runtime_resolution_invariants(&legacy);
                Ok(Self { state: legacy })
            }
            RESOLUTION_STATE_WIRE_VERSION => {
                if legacy_resolution_wire_field(object).is_some() {
                    return Err("v2 resolution state must not contain a legacy resolution field".to_string());
                }
                let frames_value = object
                    .get("resolution_frames")
                    .ok_or_else(|| "v2 resolution state is missing resolution_frames".to_string())?;
                let mut frames: ResolutionStack = serde_json::from_value(frames_value.clone())
                    .map_err(|error| error.to_string())?;
                frames.recover_draw_sequence_allocator();

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

/// v1-only CounterMoves field. Runtime state carries the queue only in its
/// typed frame.
#[derive(Deserialize)]
struct LegacyCounterMovesWire {
    #[serde(default)]
    pending_counter_moves: Option<PendingCounterMoveQueue>,
}

impl LegacyCounterMovesWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingCounterMoveQueue> {
        self.pending_counter_moves
    }
}

/// v1-only CounterRemovals field. Runtime state carries the queue only in its
/// typed frame.
#[derive(Deserialize)]
struct LegacyCounterRemovalsWire {
    #[serde(default)]
    pending_counter_removals: Option<PendingCounterRemovalQueue>,
}

impl LegacyCounterRemovalsWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingCounterRemovalQueue> {
        self.pending_counter_removals
    }
}

/// v1-only CounterAdditions field. Runtime state carries the queue only in its
/// typed frame.
#[derive(Deserialize)]
struct LegacyCounterAdditionsWire {
    #[serde(default)]
    pending_counter_additions: Option<PendingCounterAdditionQueue>,
}

impl LegacyCounterAdditionsWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingCounterAdditionQueue> {
        self.pending_counter_additions
    }
}

/// v1-only CopyToken field. Runtime state carries the owner only in its typed
/// frame.
#[derive(Deserialize)]
struct LegacyCopyTokenWire {
    #[serde(default)]
    pending_copy_token_resolution: Option<PendingCopyTokenResolution>,
}

impl LegacyCopyTokenWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingCopyTokenResolution> {
        self.pending_copy_token_resolution
    }
}

/// v1-only EachPlayerCopyChosen field. Runtime state carries the owner only
/// in its typed frame.
#[derive(Deserialize)]
struct LegacyEachPlayerCopyChosenWire {
    #[serde(default)]
    pending_each_player_copy_chosen: Option<PendingEachPlayerCopyChosen>,
}

impl LegacyEachPlayerCopyChosenWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingEachPlayerCopyChosen> {
        self.pending_each_player_copy_chosen
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

/// v1-only repeated optional-payment authority. Runtime state keeps both the
/// current driver and its resolution-local count in one frame.
#[derive(Deserialize)]
struct LegacyRepeatedOptionalPaymentWire {
    #[serde(default)]
    pending_repeated_optional_payment: Option<Box<PendingRepeatedOptionalPayment>>,
    #[serde(default)]
    optional_cost_payments_this_resolution: u32,
}

/// v1-only coin-flip authority. Runtime state keeps the keep-choice resolver
/// in a typed `CoinFlip` frame.
#[derive(Deserialize)]
struct LegacyCoinFlipWire {
    #[serde(default)]
    pending_coin_flip: Option<PendingCoinFlip>,
}

impl LegacyCoinFlipWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingCoinFlip> {
        self.pending_coin_flip
    }
}

/// v1-only proliferate authority. Runtime state keeps the target-choice
/// continuation in a typed `Proliferate` frame.
#[derive(Deserialize)]
struct LegacyProliferateWire {
    #[serde(default)]
    pending_proliferate_actions: Option<PendingProliferateActions>,
}

impl LegacyProliferateWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingProliferateActions> {
        self.pending_proliferate_actions
    }
}

/// v1-only mutate-merge authority. Runtime state keeps the top/bottom choice
/// continuation in a typed `MutateMerge` frame.
#[derive(Deserialize)]
struct LegacyMutateMergeWire {
    #[serde(default)]
    pending_mutate_merge: Option<PendingMutateMerge>,
}

impl LegacyMutateMergeWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<PendingMutateMerge> {
        self.pending_mutate_merge
    }
}

impl LegacyRepeatedOptionalPaymentWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Option<RepeatedOptionalPaymentFrame> {
        (self.pending_repeated_optional_payment.is_some()
            || self.optional_cost_payments_this_resolution != 0)
            .then_some(RepeatedOptionalPaymentFrame {
                pending: self.pending_repeated_optional_payment,
                optional_cost_payments_this_resolution: self.optional_cost_payments_this_resolution,
            })
    }
}

/// v1-only optional-effect authority. Runtime state keeps the ability and its
/// trigger event/count context together in an `OptionalEffect` frame.
#[derive(Deserialize)]
struct LegacyOptionalEffectWire {
    #[serde(default)]
    pending_optional_effect: Option<Box<ResolvedAbility>>,
    #[serde(default)]
    pending_optional_trigger_event: Option<GameEvent>,
    #[serde(default)]
    pending_optional_trigger_match_count: Option<u32>,
}

impl LegacyOptionalEffectWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frame(self) -> Result<Option<OptionalEffectFrame>, String> {
        let Some(ability) = self.pending_optional_effect else {
            if self.pending_optional_trigger_event.is_some()
                || self.pending_optional_trigger_match_count.is_some()
            {
                return Err(
                    "legacy optional-effect trigger context has no optional-effect owner"
                        .to_string(),
                );
            }
            return Ok(None);
        };
        Ok(Some(OptionalEffectFrame {
            ability,
            trigger_event: self.pending_optional_trigger_event,
            trigger_match_count: self.pending_optional_trigger_match_count,
        }))
    }
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

/// v1-only replacement-tail fields. The shipped model stored the general drain
/// and draw cursor independently, so this reader is the sole place that
/// reconstructs their proven paused-parent adjacency.
#[derive(Deserialize)]
struct LegacyReplacementTailsWire {
    #[serde(default)]
    draw_sequences: DrawSequenceStack,
    #[serde(default)]
    pending_multi_draw: Option<PendingMultiDraw>,
    #[serde(default)]
    pending_connive_reentry: Option<PendingConniveReentry>,
    #[serde(default)]
    pending_life_total_assignment: Option<PendingLifeTotalAssignment>,
    #[serde(default)]
    pending_spell_resolution: Option<PendingSpellResolution>,
    #[serde(default)]
    post_replacement_drains: PostReplacementDrainStack,
    #[serde(default)]
    post_replacement_effect: Option<Box<AbilityDefinition>>,
    #[serde(default)]
    post_replacement_resolved_effect: Option<Box<ResolvedAbility>>,
    #[serde(default)]
    post_replacement_continuation: Option<crate::types::ability::PostReplacementContinuation>,
    #[serde(default)]
    post_replacement_source: Option<ObjectId>,
    #[serde(default)]
    post_replacement_applied: HashSet<crate::types::proposed_event::AppliedReplacementKey>,
    #[serde(default)]
    post_replacement_event_source: Option<ObjectId>,
    #[serde(default)]
    post_replacement_event_target: Option<TargetRef>,
}

impl LegacyReplacementTailsWire {
    fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }

    fn into_frames(mut self) -> Result<(Vec<ResolutionFrame>, u64), String> {
        if self.draw_sequences.is_empty() {
            if let Some(pending) = self.pending_multi_draw.take() {
                let frame_id = self.draw_sequences.push(pending.player, pending.remaining);
                self.draw_sequences
                    .active_if(frame_id)
                    .expect("newly restored v1 draw frame must be active")
                    .accumulated = pending.accumulated;
            }
        }

        let continuation = self
            .post_replacement_continuation
            .take()
            .or_else(|| {
                self.post_replacement_resolved_effect
                    .take()
                    .map(crate::types::ability::PostReplacementContinuation::Resolved)
            })
            .or_else(|| {
                self.post_replacement_effect
                    .take()
                    .map(crate::types::ability::PostReplacementContinuation::Template)
            });
        if self.post_replacement_drains.is_empty() {
            if let Some(continuation) = continuation {
                self.post_replacement_drains.install(
                    PostReplacementDrain {
                        status: DrainStatus::Ready(continuation),
                        source: self.post_replacement_source,
                        applied: self.post_replacement_applied,
                        event_source: self.post_replacement_event_source,
                        event_target: self.post_replacement_event_target,
                    },
                    ResidentDrainPolicy::Replace,
                );
            }
        }

        let next_draw_sequence_frame_id = self.draw_sequences.next_frame_id();
        let pending_connive_reentry = self.pending_connive_reentry.take();
        let pending_life_total_assignment = self.pending_life_total_assignment.take();
        let pending_spell_resolution = self.pending_spell_resolution.take();
        if !self.draw_sequences.is_empty() {
            if pending_life_total_assignment.is_some() || pending_spell_resolution.is_some() {
                return Err(
                    "legacy multi-draw state cannot have an independent life-total assignment or spell-resolution tail"
                        .to_string(),
                );
            }
            let child = ResolutionFrame::MultiDraw(MultiDrawFrame {
                draw_sequences: self.draw_sequences,
                connive_reentry: pending_connive_reentry,
            });
            if self.post_replacement_drains.is_empty() {
                return Ok((vec![child], next_draw_sequence_frame_id));
            }

            let parent = ResolutionFrame::PostReplacement(self.post_replacement_drains);
            let ResolutionFrame::PostReplacement(drains) = &parent else {
                unreachable!("post-replacement conversion built the wrong frame kind")
            };
            if !matches!(
                drains.resident().map(|drain| &drain.status),
                Some(DrainStatus::Paused)
            ) {
                return Err(
                    "legacy post-replacement and multi-draw state is ambiguous without a paused resident drain"
                        .to_string(),
                );
            }
            return Ok((vec![parent, child], next_draw_sequence_frame_id));
        }

        let mut frames = Vec::new();
        if !self.post_replacement_drains.is_empty() {
            frames.push(ResolutionFrame::PostReplacement(
                self.post_replacement_drains,
            ));
        }
        if let Some(pending) = pending_connive_reentry {
            frames.push(ResolutionFrame::ConniveReentry(pending));
        }
        if let Some(pending) = pending_life_total_assignment {
            frames.push(ResolutionFrame::LifeTotalAssignment(pending));
        }
        if let Some(pending) = pending_spell_resolution {
            frames.push(ResolutionFrame::SpellResolution(pending));
        }
        Ok((frames, next_draw_sequence_frame_id))
    }
}

pub(crate) fn canonicalize_legacy_resolution_state(
    state: &GameState,
) -> Result<ResolutionStack, String> {
    let mut frames = ResolutionStack::default();
    frames
        .restore_next_draw_sequence_frame_id(state.resolution_stack.next_draw_sequence_frame_id());

    for frame in state.resolution_stack.iter() {
        if !frame.is_runtime_stack_resident() {
            return Err(format!(
                "runtime resolution stack contains unmigrated {:?} frame",
                frame.kind()
            ));
        }
        frames.push_inner(frame.clone());
    }
    Ok(frames)
}

fn project_frames_into_legacy_state(
    state: &GameState,
    frames: &ResolutionStack,
) -> Result<GameState, String> {
    let mut projected = state.clone();
    clear_legacy_resolution_slots(&mut projected);
    projected
        .resolution_stack
        .restore_next_draw_sequence_frame_id(frames.next_draw_sequence_frame_id());
    for frame in frames.iter() {
        match frame {
            ResolutionFrame::AbilityContinuation(frame) => {
                projected.push_ability_continuation(frame.clone());
            }
            ResolutionFrame::RepeatFor(pending) => projected.push_repeat_for(pending.clone()),
            ResolutionFrame::RepeatUntil(pending) => projected.push_repeat_until(pending.clone()),
            ResolutionFrame::RepeatedOptionalPayment(frame) => {
                projected.push_repeated_optional_payment_frame(frame.clone());
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
            ResolutionFrame::CounterMoves(pending) => {
                projected
                    .resolution_stack
                    .push_counter_moves(pending.clone());
            }
            ResolutionFrame::CounterRemovals(pending) => {
                projected
                    .resolution_stack
                    .push_counter_removals(pending.clone());
            }
            ResolutionFrame::CounterAdditions(pending) => {
                projected
                    .resolution_stack
                    .push_counter_additions(pending.clone());
            }
            ResolutionFrame::CopyToken(pending) => {
                projected.resolution_stack.push_copy_token(pending.clone());
            }
            ResolutionFrame::EachPlayerCopyChosen(pending) => {
                projected
                    .resolution_stack
                    .push_each_player_copy_chosen(pending.clone());
            }
            ResolutionFrame::ChooseOneOf(pending) => projected.push_choose_one_of(pending.clone()),
            ResolutionFrame::VoteBallot(pending) => projected.push_vote_ballot(pending.clone()),
            ResolutionFrame::PerPlayerZoneChoice(pending) => {
                projected.push_per_player_zone_choice(pending.clone())
            }
            ResolutionFrame::PerCategoryZoneChoice(frame) => {
                projected.push_per_category_zone_choice(frame.pending.clone())
            }
            ResolutionFrame::OptionalEffect(frame) => {
                projected.push_optional_effect_frame(frame.clone());
            }
            ResolutionFrame::CoinFlip(pending) => projected.push_coin_flip_frame(pending.clone()),
            ResolutionFrame::Proliferate(pending) => {
                projected.push_proliferate_frame(pending.clone())
            }
            ResolutionFrame::MutateMerge(pending) => {
                projected.push_mutate_merge_frame(pending.clone())
            }
            ResolutionFrame::MultiDraw(frame) => {
                projected.resolution_stack.push_multi_draw(frame.clone())
            }
            ResolutionFrame::ConniveReentry(pending) => projected
                .resolution_stack
                .push_connive_reentry(pending.clone()),
            ResolutionFrame::LifeTotalAssignment(pending) => projected
                .resolution_stack
                .push_life_total_assignment(pending.clone()),
            ResolutionFrame::SpellResolution(pending) => projected
                .resolution_stack
                .push_spell_resolution(pending.clone()),
            ResolutionFrame::PostReplacement(drains) => {
                projected
                    .resolution_stack
                    .push_post_replacement(drains.clone());
            }
        }
    }
    Ok(projected)
}

fn clear_legacy_resolution_slots(state: &mut GameState) {
    state.resolution_stack = ResolutionStack::default();
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
        CastingVariant, CopyChosenStage, DrainStatus, DrawSequenceOrigin, GameState,
        PendingBatchDeliveries, PendingChooseOneOf, PendingCopyTokenResolution,
        PendingCounterAdditionQueue, PendingCounterMoveQueue, PendingCounterRemovalQueue,
        PendingEachPlayerCopyChosen, PendingLifeTotalAssignment, PendingPerCategoryZoneChoice,
        PendingPerPlayerZoneChoice, PendingRepeatIteration, PendingRepeatUntil,
        PendingSpellResolution, PendingVoteBallotIteration, PostReplacementDrain,
        ResidentDrainPolicy, ZoneDeliveryExileTracking,
    };
    use crate::types::identifiers::{CardId, LogicalZoneChangeGroupId, ObjectId};
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
            connive_reentry: None,
        })
    }

    fn restore_v1_optional_effect_fixture(
        state: GameState,
        frame: OptionalEffectFrame,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_optional_effect"] =
            serde_json::to_value(frame.ability).expect("legacy optional effect serializes");
        v1["pending_optional_trigger_event"] =
            serde_json::to_value(frame.trigger_event).expect("legacy optional event serializes");
        v1["pending_optional_trigger_match_count"] =
            serde_json::to_value(frame.trigger_match_count)
                .expect("legacy optional count serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire = serde_json::from_value(v1)
            .expect("v1 optional-effect fixture converts through the wire");
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
            .expect("v2 optional-effect fixture restores for the runtime action path")
            .into_game_state()
    }

    fn restore_v1_repeated_optional_payment_fixture(
        state: GameState,
        frame: RepeatedOptionalPaymentFrame,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_repeated_optional_payment"] =
            serde_json::to_value(frame.pending).expect("legacy repeated-payment serializes");
        v1["optional_cost_payments_this_resolution"] =
            Value::from(frame.optional_cost_payments_this_resolution);
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire = serde_json::from_value(v1)
            .expect("v1 repeated-payment fixture converts through the wire");
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
            .expect("v2 repeated-payment fixture restores for the runtime action path")
            .into_game_state()
    }

    fn restore_v1_coin_flip_fixture(state: GameState, pending: PendingCoinFlip) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_coin_flip"] =
            serde_json::to_value(pending).expect("legacy coin-flip serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 coin-flip fixture converts through the wire");
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
            .expect("v2 coin-flip fixture restores for the runtime action path")
            .into_game_state()
    }

    fn restore_v1_proliferate_fixture(
        state: GameState,
        pending: PendingProliferateActions,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_proliferate_actions"] =
            serde_json::to_value(pending).expect("legacy proliferate serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 proliferate fixture converts through the wire");
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
            .expect("v2 proliferate fixture restores for the runtime action path")
            .into_game_state()
    }

    fn restore_v1_mutate_merge_fixture(state: GameState, pending: PendingMutateMerge) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_mutate_merge"] =
            serde_json::to_value(pending).expect("legacy mutate-merge serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 mutate-merge fixture converts through the wire");
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
            .expect("v2 mutate-merge fixture restores for the runtime action path")
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

    fn restore_v1_counter_moves_fixture(
        state: GameState,
        pending: PendingCounterMoveQueue,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_counter_moves"] =
            serde_json::to_value(pending).expect("legacy counter-moves serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 counter-moves fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_counter_removals_fixture(
        state: GameState,
        pending: PendingCounterRemovalQueue,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_counter_removals"] =
            serde_json::to_value(pending).expect("legacy counter-removals serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 counter-removals fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_counter_additions_fixture(
        state: GameState,
        pending: PendingCounterAdditionQueue,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_counter_additions"] =
            serde_json::to_value(pending).expect("legacy counter-additions serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 counter-additions fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_copy_token_fixture(
        state: GameState,
        pending: PendingCopyTokenResolution,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_copy_token_resolution"] =
            serde_json::to_value(pending).expect("legacy copy-token serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 copy-token fixture converts through the wire")
            .into_game_state()
    }

    fn restore_v1_each_player_copy_chosen_fixture(
        state: GameState,
        pending: PendingEachPlayerCopyChosen,
    ) -> GameState {
        assert!(state.resolution_stack.is_empty());
        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_each_player_copy_chosen"] =
            serde_json::to_value(pending).expect("legacy each-player-copy-chosen serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("v1 each-player-copy-chosen fixture converts through the wire")
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
        state.park_ability_continuation(PendingContinuation::new(
            Box::new(resolved_draw(157)),
            &state,
        ));
        state.resolution_stack.push_multi_draw(draw);

        crate::game::effects::resume_resolution_frames(&mut state, &mut Vec::new());

        assert!(state.active_draw_sequence().is_none());
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
        state
            .resolution_stack
            .install_adjacent_post_replacement_draw(
                ResolutionFrame::PostReplacement(drains),
                ResolutionFrame::MultiDraw(draw),
            )
            .expect("fixture installs the shipped adjacent pair");
        crate::game::effects::resume_resolution_frames(&mut state, &mut Vec::new());

        assert!(state.active_draw_sequence().is_none());
        assert!(
            state.resolution_stack.is_empty(),
            "the draw authority retires only its completed child and paused parent"
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
        optional_effect.push_inner(continuation_frame(7));
        assert_eq!(
            optional_effect.validate(&WaitingFor::OpponentMayChoice {
                player: PlayerId(1),
                source_id: ObjectId(6),
                description: None,
                remaining: Vec::new(),
            }),
            Err(ResolutionStackError::InvalidPayload {
                frame: FrameKind::OptionalEffect,
                message: "a direct-choice owner is buried below another resolution frame"
                    .to_string(),
            })
        );
        optional_effect
            .pop_expected(FrameKind::AbilityContinuation)
            .expect("test continuation restores the direct-choice top");
        optional_effect.push_inner(ResolutionFrame::CoinFlip(PendingCoinFlip {
            source_id: ObjectId(6),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        }));
        assert_eq!(
            optional_effect.validate(&WaitingFor::CoinFlipKeepChoice {
                player: PlayerId(0),
                results: vec![true, false],
                keep_count: 1,
            }),
            Err(ResolutionStackError::MultipleDirectChoiceOwners)
        );
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
        state.waiting_for = WaitingFor::CoinFlipKeepChoice {
            player: PlayerId(0),
            results: vec![true, false],
            keep_count: 1,
        };
        let pending = PendingCoinFlip {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        };

        let mut v1 = serde_json::to_value(&state).expect("legacy state serializes");
        v1["pending_coin_flip"] =
            serde_json::to_value(&pending).expect("legacy coin flip serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 legacy state converts through frames");
        assert_eq!(wire.game_state().active_coin_flip_frame(), Some(&pending));

        let v2 = serde_json::to_value(&wire).expect("v2 wire serializes");
        assert_eq!(
            v2["resolution_state_version"],
            Value::from(RESOLUTION_STATE_WIRE_VERSION)
        );
        assert!(v2.get("resolution_frames").is_some());
        assert!(v2.get("pending_coin_flip").is_none());

        let restored: ResolutionStateWire =
            serde_json::from_value(v2).expect("v2 frame state restores for the runtime action");
        assert_eq!(
            restored.into_game_state().active_coin_flip_frame(),
            Some(&pending)
        );
    }

    #[test]
    fn resolution_wire_contract_pins_v2_and_the_v1_reader() {
        assert_eq!(RESOLUTION_STATE_WIRE_VERSION, 2);
        assert_eq!(LEGACY_RESOLUTION_STATE_WIRE_VERSION, 1);

        let mut v1 =
            serde_json::to_value(GameState::new_two_player(57)).expect("legacy state serializes");
        v1["resolution_state_version"] = Value::from(1_u64);
        serde_json::from_value::<ResolutionStateWire>(v1)
            .expect("the v1 reader remains available for historical saves");
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
    fn change_zone_repark_keeps_a_distinct_nested_change_zone_child() {
        let ResolutionFrame::ChangeZone(outer) = change_zone_frame(160) else {
            unreachable!("helper constructs a ChangeZone frame")
        };
        let mut replacement = outer
            .pending
            .clone()
            .expect("helper constructs a parked ChangeZone iteration");
        replacement.source_id = ObjectId(162);

        let ResolutionFrame::ChangeZone(mut child) = change_zone_frame(161) else {
            unreachable!("helper constructs a ChangeZone frame")
        };
        child
            .pending
            .as_mut()
            .expect("helper constructs a parked ChangeZone iteration")
            .logical_zone_change_group
            .logical_group_id = LogicalZoneChangeGroupId(161);

        let mut stack = ResolutionStack::default();
        stack.push_inner(ResolutionFrame::ChangeZone(outer));
        stack.push_inner(ResolutionFrame::ChangeZone(child));

        stack
            .replace_change_zone_parent_at_child_boundary(replacement, 1)
            .expect("the outer ChangeZone owner remains immediately below its child");

        let frames = stack.iter().collect::<Vec<_>>();
        assert!(matches!(
            frames.as_slice(),
            [ResolutionFrame::ChangeZone(outer), ResolutionFrame::ChangeZone(child)]
                if outer.pending.as_ref().is_some_and(|pending| pending.source_id == ObjectId(162))
                    && child.pending.as_ref().is_some_and(|pending| pending.source_id == ObjectId(161))
        ));
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
        let state = GameState::new_two_player(45);

        let mut v1 = serde_json::to_value(&state).expect("legacy state serializes");
        v1["optional_cost_payments_this_resolution"] = Value::from(2);
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
                .active_repeated_optional_payment_frame()
                .map(|frame| frame.optional_cost_payments_this_resolution),
            Some(2)
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
        let mut v1 = serde_json::to_value(GameState::new_two_player(42))
            .expect("legacy paired state serializes");
        v1["post_replacement_drains"] =
            serde_json::to_value(drains).expect("paused drain serializes");
        v1["draw_sequences"] =
            serde_json::to_value(draw.draw_sequences).expect("active draw serializes");
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
        repeated.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };
        let mut repeated = restore_v1_repeated_optional_payment_fixture(
            repeated,
            RepeatedOptionalPaymentFrame {
                pending: Some(Box::new(PendingRepeatedOptionalPayment {
                    payment_unit: Box::new(resolved_draw(100)),
                    reflexive: Box::new(resolved_draw(101)),
                    remaining: 0,
                })),
                optional_cost_payments_this_resolution: 0,
            },
        );
        apply_as_current(
            &mut repeated,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("repeated-payment fixture resumes through the real optional-choice action");
        assert!(repeated.active_repeated_optional_payment_frame().is_none());
        assert_reserializes_v2_only(repeated);

        let mut optional = GameState::new_two_player(102);
        optional.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(102),
            description: None,
            may_trigger_key: None,
        };
        let mut optional = restore_v1_optional_effect_fixture(
            optional,
            OptionalEffectFrame {
                ability: Box::new(resolved_draw(102)),
                trigger_event: None,
                trigger_match_count: None,
            },
        );
        apply_as_current(
            &mut optional,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("optional-effect fixture resumes through the real optional-choice action");
        assert!(optional.active_optional_effect_frame().is_none());
        assert_reserializes_v2_only(optional);

        let mut coin = GameState::new_two_player(103);
        coin.waiting_for = WaitingFor::CoinFlipKeepChoice {
            player: PlayerId(0),
            results: vec![true, false],
            keep_count: 1,
        };
        let mut coin = restore_v1_coin_flip_fixture(
            coin,
            PendingCoinFlip {
                source_id: ObjectId(103),
                controller: PlayerId(0),
                flipper: PlayerId(0),
                targets: Vec::new(),
                win_effect: None,
                lose_effect: None,
                kind: PendingCoinFlipKind::Single,
            },
        );
        apply_as_current(
            &mut coin,
            GameAction::SelectCoinFlips {
                keep_indices: vec![0],
            },
        )
        .expect("coin-flip fixture resumes through the real keep-choice action");
        assert!(coin.active_coin_flip_frame().is_none());
        assert_reserializes_v2_only(coin);

        let mut proliferate = GameState::new_two_player(104);
        proliferate.waiting_for = WaitingFor::ProliferateChoice {
            player: PlayerId(0),
            eligible: Vec::new(),
        };
        let mut proliferate = restore_v1_proliferate_fixture(
            proliferate,
            PendingProliferateActions {
                actor: PlayerId(0),
                source_id: ObjectId(104),
                remaining: 0,
            },
        );
        apply_as_current(
            &mut proliferate,
            GameAction::SelectTargets {
                targets: Vec::new(),
            },
        )
        .expect("proliferate fixture resumes through the real target-choice action");
        assert!(proliferate.active_proliferate_frame().is_none());
        assert_reserializes_v2_only(proliferate);

        let mut scenario = GameScenario::new();
        let merging_id = scenario.add_creature(PlayerId(0), "Rider", 4, 4).id();
        let target_id = scenario.add_creature(PlayerId(0), "Host", 2, 2).id();
        let mut mutate = scenario.state;
        mutate.waiting_for = WaitingFor::MutateMergeChoice {
            player: PlayerId(0),
            merging_id,
            target_id,
        };
        let mut mutate = restore_v1_mutate_merge_fixture(
            mutate,
            PendingMutateMerge {
                merging_id,
                target_id,
                controller: PlayerId(0),
            },
        );
        apply_as_current(
            &mut mutate,
            GameAction::ChooseMutateMergeSide {
                side: MergeSide::Top,
            },
        )
        .expect("mutate fixture resumes through the real merge-choice action");
        assert!(mutate.active_mutate_merge_frame().is_none());
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

        let counter_moves = GameState::new_two_player(114);
        let counter_moves = resume_priority_fixture(restore_v1_counter_moves_fixture(
            counter_moves,
            PendingCounterMoveQueue {
                remaining: Vec::new(),
                effect_kind: EffectKind::MoveCounters,
                source_id: ObjectId(114),
            },
        ));
        assert!(counter_moves.active_counter_moves().is_none());
        assert_reserializes_v2_only(counter_moves);

        let counter_removals = GameState::new_two_player(115);
        let counter_removals = resume_priority_fixture(restore_v1_counter_removals_fixture(
            counter_removals,
            PendingCounterRemovalQueue {
                remaining: Vec::new(),
                source_id: ObjectId(115),
                effect_kind: EffectKind::RemoveCounter,
                source_ability_id: ObjectId(115),
                total: 0,
            },
        ));
        assert!(counter_removals.active_counter_removals().is_none());
        assert_reserializes_v2_only(counter_removals);

        let counter_additions = GameState::new_two_player(116);
        let counter_additions = resume_priority_fixture(restore_v1_counter_additions_fixture(
            counter_additions,
            PendingCounterAdditionQueue {
                remaining: Vec::new(),
                completion: None,
            },
        ));
        assert!(counter_additions.active_counter_additions().is_none());
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

        let mut copy_token = restore_v1_copy_token_fixture(
            GameState::new_two_player(121),
            PendingCopyTokenResolution {
                created_ids: Vec::new(),
                remaining: VecDeque::new(),
                effect_kind: EffectKind::CopyTokenOf,
                source_id: ObjectId(121),
            },
        );
        crate::game::effects::token_copy::drain_pending_copy_token_resolution(
            &mut copy_token,
            &mut Vec::new(),
        );
        assert!(copy_token.active_copy_token().is_none());
        assert_reserializes_v2_only(copy_token);
    }

    #[test]
    fn v1_nested_each_player_copy_and_counter_fixture_preserves_child_order() {
        let state = GameState::new_two_player(122);
        let each_player = PendingEachPlayerCopyChosen {
            stage: CopyChosenStage::AwaitingCopy,
            player: PlayerId(0),
            chosen: Vec::new(),
            remaining_choices: Vec::new(),
            choose_filter: TargetFilter::Controller,
            min: 0,
            max: 0,
            copy_modifications: Vec::new(),
            scale: None,
            choose_scope: CopyChooseScope::Chooser,
            source_id: ObjectId(122),
            source_controller: PlayerId(0),
            scoped_players: Vec::new(),
            trigger_event: None,
        };
        let copy_token = PendingCopyTokenResolution {
            created_ids: Vec::new(),
            remaining: VecDeque::new(),
            effect_kind: EffectKind::CopyTokenOf,
            source_id: ObjectId(122),
        };
        let counter_additions = PendingCounterAdditionQueue {
            remaining: Vec::new(),
            completion: None,
        };

        let mut v1 = serde_json::to_value(state).expect("legacy fixture serializes");
        v1["pending_each_player_copy_chosen"] =
            serde_json::to_value(each_player).expect("legacy each-player owner serializes");
        v1["pending_copy_token_resolution"] =
            serde_json::to_value(copy_token).expect("legacy copy-token child serializes");
        v1["pending_counter_additions"] =
            serde_json::to_value(counter_additions).expect("legacy counter child serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);

        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 nested pause converts through the wire");
        let v2 = serde_json::to_value(&wire).expect("nested pause serializes as v2 frames");
        for field in legacy_resolution_wire_fields() {
            assert!(
                v2.get(*field).is_none(),
                "nested v2 fixture must not write legacy field {field}"
            );
        }

        let mut state = wire.into_game_state();
        assert_eq!(
            state
                .resolution_stack
                .iter()
                .map(ResolutionFrame::kind)
                .collect::<Vec<_>>(),
            vec![
                FrameKind::EachPlayerCopyChosen,
                FrameKind::CopyToken,
                FrameKind::CounterAdditions,
            ],
            "v1 nested parent/child slots must restore outer-to-inner"
        );

        let mut events = Vec::new();
        for _ in 0..3 {
            crate::game::effects::resume_resolution_frames(&mut state, &mut events);
        }
        assert!(state.active_each_player_copy_chosen().is_none());
        assert!(state.active_copy_token().is_none());
        assert!(state.active_counter_additions().is_none());
        assert_reserializes_v2_only(state);
    }

    #[test]
    fn v1_choice_iteration_fixtures_resume_via_their_production_drains() {
        let mut each_player_copy = restore_v1_each_player_copy_chosen_fixture(
            GameState::new_two_player(130),
            PendingEachPlayerCopyChosen {
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
            },
        );
        crate::game::effects::each_player_copy_chosen::drain_pending(
            &mut each_player_copy,
            &mut Vec::new(),
        );
        assert!(each_player_copy.active_each_player_copy_chosen().is_none());
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
    fn v2_reader_honors_outer_draw_allocator_and_recovers_legacy_allocator_forms() {
        let mut state = GameState::new_two_player(139);
        let captured = state.push_draw_sequence_with_origin(
            PlayerId(0),
            1,
            HashSet::new(),
            DrawSequenceOrigin::Plain,
        );
        let v2 = serde_json::to_value(ResolutionStateWire::from_game_state(state))
            .expect("v2 active draw fixture serializes");
        let mut with_outer_allocator = v2.clone();
        with_outer_allocator["resolution_frames"]["next_draw_sequence_frame_id"] = Value::from(99);
        let restored_with_outer =
            serde_json::from_value::<ResolutionStateWire>(with_outer_allocator)
                .expect("v2 payload with an outer allocator restores")
                .into_game_state();
        assert_eq!(
            restored_with_outer
                .resolution_stack
                .next_draw_sequence_frame_id(),
            99,
            "the shipped reader must honor an explicit outer allocator"
        );
        assert!(
            restored_with_outer
                .resolution_stack
                .next_draw_sequence_frame_id()
                >= restored_with_outer
                    .active_multi_draw_frame()
                    .expect("restored multi-draw frame remains active")
                    .draw_sequences
                    .next_frame_id(),
            "every successful load leaves the stack allocator at or above the active frame allocator"
        );

        let mut stale_outer_allocator = v2.clone();
        stale_outer_allocator["resolution_frames"]["next_draw_sequence_frame_id"] = Value::from(0);
        let restored_stale_outer =
            serde_json::from_value::<ResolutionStateWire>(stale_outer_allocator)
                .expect("stale outer allocator is repaired from the active frame")
                .into_game_state();
        assert_eq!(
            restored_stale_outer
                .resolution_stack
                .next_draw_sequence_frame_id(),
            restored_stale_outer
                .active_multi_draw_frame()
                .expect("restored multi-draw frame remains active")
                .draw_sequences
                .next_frame_id(),
            "the shipped reader clamps an explicit stale allocator to the active frame allocator"
        );
        assert!(
            restored_stale_outer
                .resolution_stack
                .next_draw_sequence_frame_id()
                >= restored_stale_outer
                    .active_multi_draw_frame()
                    .expect("restored multi-draw frame remains active")
                    .draw_sequences
                    .next_frame_id(),
            "the shipped reader clamps an explicit stale allocator to the active frame allocator"
        );

        let mut without_outer_allocator = v2;
        without_outer_allocator["resolution_frames"]
            .as_object_mut()
            .expect("resolution frames serialize as an object")
            .remove("next_draw_sequence_frame_id");

        let mut restored = serde_json::from_value::<ResolutionStateWire>(without_outer_allocator)
            .expect("older v2 active-draw payload restores")
            .into_game_state();
        assert_eq!(
            restored.resolution_stack.next_draw_sequence_frame_id(),
            restored
                .active_multi_draw_frame()
                .expect("restored multi-draw frame remains active")
                .draw_sequences
                .next_frame_id(),
            "a missing outer allocator recovers exactly from the active frame"
        );
        assert!(
            restored.resolution_stack.next_draw_sequence_frame_id()
                >= restored
                    .active_multi_draw_frame()
                    .expect("restored multi-draw frame remains active")
                    .draw_sequences
                    .next_frame_id(),
            "missing outer allocator recovers from the active frame"
        );
        restored.abandon_active_replacement_tails();
        let later = restored.push_draw_sequence_with_origin(
            PlayerId(0),
            1,
            HashSet::new(),
            DrawSequenceOrigin::Plain,
        );

        assert!(
            later > captured,
            "the recovered allocator must not reuse an ID captured by the abandoned draw frame"
        );
    }

    #[test]
    fn v1_remaining_resolution_frames_resume_via_shipped_authorities() {
        let mut draw_sequences = DrawSequenceStack::default();
        let outer = draw_sequences.push(PlayerId(0), 0);
        let inner = draw_sequences.push(PlayerId(0), 0);
        let mut multi_draw = serde_json::to_value(GameState::new_two_player(140))
            .expect("legacy multi-draw fixture serializes");
        multi_draw["draw_sequences"] =
            serde_json::to_value(draw_sequences).expect("legacy draw sequences serialize");
        multi_draw["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let mut multi_draw = serde_json::from_value::<ResolutionStateWire>(multi_draw)
            .expect("v1 nested multi-draw fixture restores")
            .into_game_state();
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
        assert!(multi_draw.active_draw_sequence().is_none());
        assert_reserializes_v2_only(multi_draw);

        let mut connive = GameScenario::new();
        let conniver = connive.add_creature(PlayerId(0), "Conniver", 1, 1).id();
        let connive = connive.state;
        let pending_connive_reentry = PendingConniveReentry {
            conniver: connive
                .capture_connive_subject(conniver)
                .expect("fixture conniver exists"),
            count: 0,
            applied: HashSet::new(),
        };
        let mut connive = serde_json::to_value(connive).expect("legacy connive fixture serializes");
        connive["pending_connive_reentry"] =
            serde_json::to_value(pending_connive_reentry).expect("legacy connive tail serializes");
        connive["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let mut connive = serde_json::from_value::<ResolutionStateWire>(connive)
            .expect("v1 connive fixture restores")
            .into_game_state();
        let pending = connive
            .take_active_connive_reentry()
            .expect("v1 fixture restores the exact connive subject");
        crate::game::effects::connive::propose_connive(
            &mut connive,
            pending.conniver,
            pending.count,
            pending.applied,
            &mut Vec::new(),
        )
        .expect("connive fixture re-enters through the production proposer");
        assert!(connive.active_connive_reentry().is_none());
        assert_reserializes_v2_only(connive);

        let life = GameState::new_two_player(141);
        let pending_life_total_assignment = PendingLifeTotalAssignment {
            completion_player: PlayerId(0),
            remaining: Vec::new(),
            completion: None,
        };
        let mut life = serde_json::to_value(life).expect("legacy life fixture serializes");
        life["pending_life_total_assignment"] = serde_json::to_value(pending_life_total_assignment)
            .expect("legacy life tail serializes");
        life["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let mut life = serde_json::from_value::<ResolutionStateWire>(life)
            .expect("v1 life fixture restores")
            .into_game_state();
        crate::game::effects::life::drain_pending_life_total_assignment(&mut life, &mut Vec::new());
        assert!(life.active_life_total_assignment().is_none());
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
        let pending_spell_resolution = PendingSpellResolution {
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
        };
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
        let mut spell = serde_json::to_value(spell).expect("legacy spell fixture serializes");
        spell["pending_spell_resolution"] =
            serde_json::to_value(pending_spell_resolution).expect("legacy spell tail serializes");
        spell["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let mut spell = serde_json::from_value::<ResolutionStateWire>(spell)
            .expect("v1 spell fixture restores")
            .into_game_state();
        apply_as_current(&mut spell, GameAction::ChooseReplacement { index: 0 })
            .expect("spell fixture resumes through the real replacement action");
        assert!(spell.active_spell_resolution().is_none());
        assert_reserializes_v2_only(spell);

        let mut drains = PostReplacementDrainStack::default();
        assert!(drains.install(
            PostReplacementDrain::ready(PostReplacementContinuation::Resolved(Box::new(
                resolved_draw(144),
            ))),
            ResidentDrainPolicy::KeepResident,
        ));
        let mut post_replacement = serde_json::to_value(GameState::new_two_player(144))
            .expect("legacy post-replacement fixture serializes");
        post_replacement["post_replacement_drains"] =
            serde_json::to_value(drains).expect("legacy post-replacement drains serialize");
        post_replacement["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let mut post_replacement = serde_json::from_value::<ResolutionStateWire>(post_replacement)
            .expect("v1 post-replacement fixture restores")
            .into_game_state();
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
        assert!(post_replacement.active_post_replacement_drains().is_none());
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
        let mut paired = serde_json::to_value(GameState::new_two_player(145))
            .expect("legacy paired fixture serializes");
        paired["post_replacement_drains"] =
            serde_json::to_value(drains).expect("paused drain serializes");
        paired["draw_sequences"] =
            serde_json::to_value(draw.draw_sequences).expect("active draw serializes");
        paired["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let mut paired = serde_json::from_value::<ResolutionStateWire>(paired)
            .expect("v1 paired fixture restores")
            .into_game_state();
        let _ = crate::game::effects::draw::resume_draw_sequence(
            &mut paired,
            frame_id,
            &mut Vec::new(),
        );
        assert!(paired.active_draw_sequence().is_none());
        assert!(paired.active_post_replacement_drains().is_none());
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

        let multiple_direct = GameState::new_two_player(151);
        let pending_coin_flip = PendingCoinFlip {
            source_id: ObjectId(151),
            controller: PlayerId(0),
            flipper: PlayerId(0),
            targets: Vec::new(),
            win_effect: None,
            lose_effect: None,
            kind: PendingCoinFlipKind::Single,
        };
        let mut multiple_direct = serde_json::to_value(multiple_direct).expect("v1 serializes");
        multiple_direct["pending_coin_flip"] =
            serde_json::to_value(pending_coin_flip).expect("legacy coin flip serializes");
        multiple_direct["pending_proliferate_actions"] =
            serde_json::to_value(PendingProliferateActions {
                actor: PlayerId(0),
                source_id: ObjectId(151),
                remaining: 0,
            })
            .expect("legacy proliferate serializes");
        multiple_direct["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        assert!(serde_json::from_value::<ResolutionStateWire>(multiple_direct).is_err());

        let mut buried_optional = GameState::new_two_player(151);
        buried_optional.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(151),
            description: None,
            may_trigger_key: None,
        };
        let mut buried_optional_frames = ResolutionStack::default();
        buried_optional_frames.push_inner(ResolutionFrame::OptionalEffect(OptionalEffectFrame {
            ability: Box::new(resolved_draw(151)),
            trigger_event: None,
            trigger_match_count: None,
        }));
        buried_optional_frames.push_inner(continuation_frame(151));
        assert!(
            serde_json::from_value::<ResolutionStateWire>(v2_fixture_with_frames(
                buried_optional,
                buried_optional_frames,
            ))
            .is_err()
        );

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

        let mut orphan_optional_context =
            serde_json::to_value(GameState::new_two_player(153)).expect("v1 serializes");
        orphan_optional_context["pending_optional_trigger_match_count"] = Value::from(1);
        orphan_optional_context["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        assert!(serde_json::from_value::<ResolutionStateWire>(orphan_optional_context).is_err());

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
        let mut ambiguous_legacy_pair =
            serde_json::to_value(GameState::new_two_player(154)).expect("v1 serializes");
        ambiguous_legacy_pair["post_replacement_drains"] =
            serde_json::to_value(ready_drains).expect("ready drain serializes");
        ambiguous_legacy_pair["draw_sequences"] =
            serde_json::to_value(draw.draw_sequences).expect("draw serializes");
        ambiguous_legacy_pair["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let error = serde_json::from_value::<ResolutionStateWire>(ambiguous_legacy_pair)
            .expect_err("a ready resident drain is ambiguous beside a legacy multi-draw");
        assert!(
            error
                .to_string()
                .contains("ambiguous without a paused resident drain"),
            "the translated paused-drain ambiguity must reject at the legacy converter"
        );

        let ResolutionFrame::MultiDraw(draw) = active_multi_draw_frame() else {
            unreachable!("fixture constructs a multi-draw frame")
        };
        let draw_sequences =
            serde_json::to_value(draw.draw_sequences).expect("draw sequences serialize");

        let mut draw_with_life_tail =
            serde_json::to_value(GameState::new_two_player(155)).expect("v1 serializes");
        draw_with_life_tail["draw_sequences"] = draw_sequences.clone();
        draw_with_life_tail["pending_life_total_assignment"] =
            serde_json::to_value(PendingLifeTotalAssignment {
                completion_player: PlayerId(0),
                remaining: Vec::new(),
                completion: None,
            })
            .expect("legacy life-total tail serializes");
        draw_with_life_tail["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let error = serde_json::from_value::<ResolutionStateWire>(draw_with_life_tail)
            .expect_err("legacy draw state cannot be paired with an independent life-total tail");
        assert!(
            error.to_string().contains(
                "legacy multi-draw state cannot have an independent life-total assignment or spell-resolution tail"
            ),
            "the legacy converter must reject the translated life-total ambiguity"
        );

        let mut draw_with_spell_tail =
            serde_json::to_value(GameState::new_two_player(156)).expect("v1 serializes");
        draw_with_spell_tail["draw_sequences"] = draw_sequences;
        draw_with_spell_tail["pending_spell_resolution"] =
            serde_json::to_value(PendingSpellResolution {
                object_id: ObjectId(156),
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
            })
            .expect("legacy spell-resolution tail serializes");
        draw_with_spell_tail["resolution_state_version"] =
            Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let error = serde_json::from_value::<ResolutionStateWire>(draw_with_spell_tail).expect_err(
            "legacy draw state cannot be paired with an independent spell-resolution tail",
        );
        assert!(
            error.to_string().contains(
                "legacy multi-draw state cannot have an independent life-total assignment or spell-resolution tail"
            ),
            "the legacy converter must reject the translated spell-resolution ambiguity"
        );

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
