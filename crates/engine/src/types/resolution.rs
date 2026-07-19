//! Typed, serializable suspension frames for ability resolution.
//!
//! This module deliberately models only suspended resolution work. The legacy
//! `GameState` slots remain the runtime authority until their individual Phase-3
//! migrations; Phase 2 uses these payloads at the wire boundary without
//! introducing a second mutable runtime owner.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::types::ability::ResolvedAbility;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    DrawSequenceStack, PendingBatchDeliveries, PendingChangeZoneIteration, PendingChooseOneOf,
    PendingCoinFlip, PendingConniveReentry, PendingContinuation, PendingCopyTokenResolution,
    PendingCounterAdditionQueue, PendingCounterMoveQueue, PendingCounterRemovalQueue,
    PendingEachPlayerCopyChosen, PendingLifeTotalAssignment, PendingMutateMerge,
    PendingPerCategoryZoneChoice, PendingPerPlayerZoneChoice, PendingProliferateActions,
    PendingRepeatIteration, PendingRepeatUntil, PendingRepeatedOptionalPayment,
    PendingSpellResolution, PendingVoteBallotIteration, PostReplacementDrainStack,
    ResolvingTriggerContext, WaitingFor,
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
/// The count is a separate legacy runtime register, but it is part of the
/// same resolution lifetime and therefore travels with this frame on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatedOptionalPaymentFrame {
    pub pending: Box<PendingRepeatedOptionalPayment>,
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
    pub pending: PendingChangeZoneIteration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devour_eligible_snapshot: Option<HashSet<ObjectId>>,
}

/// The per-category zone-choice owner and its captured trigger context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerCategoryZoneChoiceFrame {
    pub pending: PendingPerCategoryZoneChoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_context: Option<ResolvingTriggerContext>,
}

/// The one place that states every serializable family of suspended
/// resolution work. The variants intentionally mirror the exhaustive Phase-2
/// census; a new pause family must be added here before it can cross the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ResolutionFrame {
    AbilityContinuation(PendingContinuation),
    RepeatFor(PendingRepeatIteration),
    RepeatUntil(PendingRepeatUntil),
    RepeatedOptionalPayment(RepeatedOptionalPaymentFrame),
    ChangeZone(ChangeZoneFrame),
    BatchDelivery(PendingBatchDeliveries),
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
            Self::RepeatedOptionalPayment(_) | Self::OptionalEffect(_) => {
                FrameGate::DirectChoice(DirectChoiceGate::OptionalEffect)
            }
            Self::CoinFlip(_) => FrameGate::DirectChoice(DirectChoiceGate::CoinFlipKeep),
            Self::Proliferate(_) => FrameGate::DirectChoice(DirectChoiceGate::Proliferate),
            Self::MutateMerge(_) => FrameGate::DirectChoice(DirectChoiceGate::MutateMerge),
            Self::AbilityContinuation(_)
            | Self::RepeatFor(_)
            | Self::RepeatUntil(_)
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
            ) | (Self::CoinFlipKeep, WaitingFor::CoinFlipKeepChoice { .. })
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
        for frame in &self.frames {
            if let ResolutionFrame::MultiDraw(draw) = frame {
                draw.draw_sequences.validate().map_err(|message| {
                    ResolutionStackError::InvalidPayload {
                        frame: FrameKind::MultiDraw,
                        message,
                    }
                })?;
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
    use crate::types::ability::{
        Effect, EffectKind, PostReplacementContinuation, QuantityExpr, TargetFilter,
    };
    use crate::types::game_state::{
        DrainStatus, GameState, PendingCoinFlipKind, PostReplacementDrain, ResidentDrainPolicy,
    };
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use crate::types::zones::{EtbTapState, Zone};

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

    fn continuation_frame(source_id: u64) -> ResolutionFrame {
        let state = GameState::new_two_player(source_id);
        ResolutionFrame::AbilityContinuation(PendingContinuation::new(
            Box::new(resolved_draw(source_id)),
            &state,
        ))
    }

    fn change_zone_frame(group_seed: u64) -> ResolutionFrame {
        let mut state = GameState::new_two_player(group_seed);
        let mut logical_zone_change_group = state.allocate_logical_zone_change_group(&[]);
        logical_zone_change_group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("empty logical group still needs its pre-delivery latch");
        ResolutionFrame::ChangeZone(ChangeZoneFrame {
            pending: PendingChangeZoneIteration {
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
            },
            devour_eligible_snapshot: None,
        })
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

    #[test]
    fn structural_operations_are_top_only_and_full_drain_is_explicit() {
        let mut stack = ResolutionStack::default();
        assert!(stack.is_empty());
        assert_eq!(
            stack.insert_parent_of_active(continuation_frame(1)),
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
}
