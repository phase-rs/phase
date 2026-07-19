//! Typed, serializable suspension frames for ability resolution.
//!
//! This module deliberately models only suspended resolution work. The legacy
//! `GameState` slots remain the runtime authority until their individual Phase-3
//! migrations; Phase 2 uses these payloads at the wire boundary without
//! introducing a second mutable runtime owner.

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
/// The count is a separate legacy runtime register, but it is part of the
/// same resolution lifetime and therefore travels with this frame on the wire.
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
/// resolution work. The variants intentionally mirror the exhaustive Phase-2
/// census; a new pause family must be added here before it can cross the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ResolutionFrame {
    AbilityContinuation(AbilityContinuationFrame),
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
/// `GameState` intentionally retains the legacy runtime slots until their
/// Phase-3 migrations. This adapter is the only persistence seam that turns
/// those slots into the typed stack: v1 reads legacy-only state, v2 reads
/// frame-only state, and v2 writes no legacy resolution field.
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
                let mut legacy: GameState =
                    serde_json::from_value(value).map_err(|error| error.to_string())?;
                legacy.migrate_post_replacement_continuation();
                legacy.migrate_pending_multi_draw();
                let frames = canonicalize_legacy_resolution_state(&legacy)?;
                frames
                    .validate(&legacy.waiting_for)
                    .map_err(|error| error.to_string())?;
                Ok(Self {
                    state: project_frames_into_legacy_state(&legacy, &frames)?,
                })
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

enum DrawAndPostConversion {
    Paired {
        parent: ResolutionFrame,
        child: ResolutionFrame,
    },
    UnpairedDraw(ResolutionFrame),
    UnpairedPost(ResolutionFrame),
}

fn canonicalize_legacy_resolution_state(state: &GameState) -> Result<ResolutionStack, String> {
    if state.pending_continuation.is_none() && state.pending_choose_zone_trigger_context.is_some() {
        return Err(
            "pending choose-from-zone trigger context has no continuation owner".to_string(),
        );
    }
    if state.pending_optional_effect.is_none()
        && (state.pending_optional_trigger_event.is_some()
            || state.pending_optional_trigger_match_count.is_some())
    {
        return Err(
            "pending optional-effect trigger context has no optional-effect owner".to_string(),
        );
    }
    let mut frames = ResolutionStack::default();

    push_legacy_after_child_frames(state, &mut frames);
    if let Some(conversion) = classify_draw_and_post_replacement(state)? {
        // The paired branch is deliberately first. Once a paused resident drain
        // proves the adjacency relationship, neither unpaired converter may run.
        match conversion {
            DrawAndPostConversion::Paired { parent, child } => frames
                .install_adjacent_post_replacement_draw(parent, child)
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
    if let Some(pending) = state.pending_continuation.clone() {
        frames.push_inner(ResolutionFrame::AbilityContinuation(
            AbilityContinuationFrame {
                pending,
                choose_zone_trigger_context: state.pending_choose_zone_trigger_context.clone(),
            },
        ));
    }
    if let Some(pending) = state.pending_repeat_iteration.clone() {
        frames.push_inner(ResolutionFrame::RepeatFor(pending));
    }
    if let Some(pending) = state.pending_repeat_until.clone() {
        frames.push_inner(ResolutionFrame::RepeatUntil(pending));
    }
    if state.pending_change_zone_iteration.is_some() || state.devour_eligible_snapshot.is_some() {
        frames.push_inner(ResolutionFrame::ChangeZone(ChangeZoneFrame {
            pending: state.pending_change_zone_iteration.clone(),
            devour_eligible_snapshot: state.devour_eligible_snapshot.clone(),
        }));
    }
    if let Some(pending) = state.pending_batch_deliveries.clone() {
        frames.push_inner(ResolutionFrame::BatchDelivery(pending));
    }
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
    if let Some(pending) = state.pending_choose_one_of.clone() {
        frames.push_inner(ResolutionFrame::ChooseOneOf(pending));
    }
    if let Some(pending) = state.pending_vote_ballot_iteration.clone() {
        frames.push_inner(ResolutionFrame::VoteBallot(pending));
    }
    if let Some(pending) = state.pending_per_player_zone_choice.clone() {
        frames.push_inner(ResolutionFrame::PerPlayerZoneChoice(pending));
    }
    if let Some(pending) = state.pending_per_category_zone_choice.clone() {
        frames.push_inner(ResolutionFrame::PerCategoryZoneChoice(
            PerCategoryZoneChoiceFrame { pending },
        ));
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
            Some(DrawAndPostConversion::Paired { parent, child })
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
                set_once(
                    &mut projected.pending_continuation,
                    frame.pending.clone(),
                    "AbilityContinuation",
                )?;
                if projected.pending_choose_zone_trigger_context.is_some() {
                    return Err("duplicate AbilityContinuation trigger context".to_string());
                }
                projected.pending_choose_zone_trigger_context =
                    frame.choose_zone_trigger_context.clone();
            }
            ResolutionFrame::RepeatFor(pending) => set_once(
                &mut projected.pending_repeat_iteration,
                pending.clone(),
                "RepeatFor",
            )?,
            ResolutionFrame::RepeatUntil(pending) => set_once(
                &mut projected.pending_repeat_until,
                pending.clone(),
                "RepeatUntil",
            )?,
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
                if let Some(pending) = frame.pending.clone() {
                    set_once(
                        &mut projected.pending_change_zone_iteration,
                        pending,
                        "ChangeZone",
                    )?;
                }
                if projected.devour_eligible_snapshot.is_some() {
                    return Err("duplicate ChangeZone devour snapshot".to_string());
                }
                projected.devour_eligible_snapshot = frame.devour_eligible_snapshot.clone();
            }
            ResolutionFrame::BatchDelivery(pending) => set_once(
                &mut projected.pending_batch_deliveries,
                pending.clone(),
                "BatchDelivery",
            )?,
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
            ResolutionFrame::ChooseOneOf(pending) => set_once(
                &mut projected.pending_choose_one_of,
                pending.clone(),
                "ChooseOneOf",
            )?,
            ResolutionFrame::VoteBallot(pending) => set_once(
                &mut projected.pending_vote_ballot_iteration,
                pending.clone(),
                "VoteBallot",
            )?,
            ResolutionFrame::PerPlayerZoneChoice(pending) => set_once(
                &mut projected.pending_per_player_zone_choice,
                pending.clone(),
                "PerPlayerZoneChoice",
            )?,
            ResolutionFrame::PerCategoryZoneChoice(frame) => {
                set_once(
                    &mut projected.pending_per_category_zone_choice,
                    frame.pending.clone(),
                    "PerCategoryZoneChoice",
                )?;
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
    state.pending_continuation = None;
    state.pending_repeat_iteration = None;
    state.pending_repeat_until = None;
    state.pending_repeated_optional_payment = None;
    state.optional_cost_payments_this_resolution = 0;
    state.pending_change_zone_iteration = None;
    state.devour_eligible_snapshot = None;
    state.pending_batch_deliveries = None;
    state.pending_counter_moves = None;
    state.pending_counter_removals = None;
    state.pending_counter_additions = None;
    state.pending_copy_token_resolution = None;
    state.pending_each_player_copy_chosen = None;
    state.pending_choose_one_of = None;
    state.pending_vote_ballot_iteration = None;
    state.pending_per_player_zone_choice = None;
    state.pending_per_category_zone_choice = None;
    state.pending_choose_zone_trigger_context = None;
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
        ResolutionFrame::ChangeZone(ChangeZoneFrame {
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
        let mut state = GameState::new_two_player(43);
        state.pending_continuation = Some(PendingContinuation::new(
            Box::new(resolved_draw(43)),
            &state,
        ));
        let context = ResolvingTriggerContext {
            event: None,
            events: Vec::new(),
            match_count: Some(2),
            die_result: None,
        };
        state.pending_choose_zone_trigger_context = Some(context.clone());

        let mut v1 = serde_json::to_value(&state).expect("legacy state serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 continuation sidecar converts through frames");
        let v2 = serde_json::to_value(&wire).expect("v2 wire serializes");
        assert!(v2.get("pending_choose_zone_trigger_context").is_none());

        let restored: ResolutionStateWire =
            serde_json::from_value(v2).expect("v2 continuation sidecar projects for runtime");
        assert_eq!(
            restored
                .into_game_state()
                .pending_choose_zone_trigger_context,
            Some(context)
        );
    }

    #[test]
    fn resolution_state_wire_keeps_devour_snapshot_without_a_change_zone_iteration() {
        let mut state = GameState::new_two_player(44);
        let snapshot = HashSet::from([ObjectId(7), ObjectId(8)]);
        state.devour_eligible_snapshot = Some(snapshot.clone());

        let mut v1 = serde_json::to_value(&state).expect("legacy state serializes");
        v1["resolution_state_version"] = Value::from(LEGACY_RESOLUTION_STATE_WIRE_VERSION);
        let wire: ResolutionStateWire =
            serde_json::from_value(v1).expect("v1 devour sidecar converts through frames");
        let v2 = serde_json::to_value(&wire).expect("v2 wire serializes");
        assert!(v2.get("devour_eligible_snapshot").is_none());

        let restored: ResolutionStateWire =
            serde_json::from_value(v2).expect("v2 devour sidecar projects for runtime");
        assert_eq!(
            restored.into_game_state().devour_eligible_snapshot,
            Some(snapshot)
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
}
