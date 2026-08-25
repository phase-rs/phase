//! Ephemeral, viewer-filterable action rejection metadata.

use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;

/// Stable machine-readable reason for an action the engine did not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRejectionCode {
    InvalidAction,
    WrongPlayer,
    NotYourPriority,
    ActionNotAllowed,
    InteractionUnavailable,
    InteractionNotAuthorized,
    StaleInteraction,
    InvalidInteractionResponse,
    InteractionPayloadTooLarge,
    InteractionConstraintUnsatisfied,
    InteractionCancelOnly,
    InteractionReducerRejected,
    UnsupportedInteractionResponse,
    ResolveAllNotReady,
}

/// Broad client-recovery category for an [`ActionRejectionCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRejectionDisposition {
    Invalid,
    Unauthorized,
    Unavailable,
    Stale,
    Unsupported,
}

impl ActionRejectionCode {
    /// The closed recovery category for this code.
    pub const fn disposition(self) -> ActionRejectionDisposition {
        match self {
            Self::InvalidAction
            | Self::InvalidInteractionResponse
            | Self::InteractionPayloadTooLarge
            | Self::InteractionConstraintUnsatisfied
            | Self::InteractionReducerRejected => ActionRejectionDisposition::Invalid,
            Self::WrongPlayer | Self::InteractionNotAuthorized => {
                ActionRejectionDisposition::Unauthorized
            }
            Self::NotYourPriority
            | Self::ActionNotAllowed
            | Self::InteractionUnavailable
            | Self::InteractionCancelOnly
            | Self::ResolveAllNotReady => ActionRejectionDisposition::Unavailable,
            Self::StaleInteraction => ActionRejectionDisposition::Stale,
            Self::UnsupportedInteractionResponse => ActionRejectionDisposition::Unsupported,
        }
    }

    /// Safe static text for this code. Dynamic engine errors never cross this
    /// boundary because every mapping terminates in this closed enum.
    pub const fn message(self) -> &'static str {
        match self {
            Self::InvalidAction => "That action is not valid in the current game state.",
            Self::WrongPlayer => "That action belongs to a different player.",
            Self::NotYourPriority => "You do not currently have priority.",
            Self::ActionNotAllowed => "That action is not allowed right now.",
            Self::InteractionUnavailable => "That interaction is no longer available.",
            Self::InteractionNotAuthorized => "You are not authorized to answer that interaction.",
            Self::StaleInteraction => "That interaction has already changed.",
            Self::InvalidInteractionResponse => "That response is not valid for this interaction.",
            Self::InteractionPayloadTooLarge => "That interaction response is too large.",
            Self::InteractionConstraintUnsatisfied => {
                "That response does not satisfy the interaction constraints."
            }
            Self::InteractionCancelOnly => "This interaction can only be cancelled.",
            Self::InteractionReducerRejected => "That interaction can no longer be applied.",
            Self::UnsupportedInteractionResponse => "That interaction response is not supported.",
            Self::ResolveAllNotReady => "Resolve All is not ready to run.",
        }
    }
}

/// A stable, safe explanation for an action the engine did not apply.
///
/// This is deliberately a boundary result rather than `GameState` data: a
/// rejection describes one attempted action and must never be persisted into a
/// game or exposed to another viewer by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRejection {
    /// Stable machine-readable reason code.
    pub code: ActionRejectionCode,
    /// Stable broad category suitable for client recovery policy.
    pub disposition: ActionRejectionDisposition,
    /// Safe, non-diagnostic display text.
    pub message: String,
    /// Object identities referred to by the rejected action, after projection.
    pub related_object_ids: Vec<ObjectId>,
}

impl ActionRejection {
    pub(crate) fn from_code(code: ActionRejectionCode, related_object_ids: Vec<ObjectId>) -> Self {
        Self {
            disposition: code.disposition(),
            message: code.message().to_string(),
            code,
            related_object_ids,
        }
    }
}
