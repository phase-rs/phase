use std::fmt;

use serde::Serialize;

/// Every way an LLM opponent turn can fail to produce a decision.
///
/// Each variant is a *recoverable* outcome: the caller falls back to the
/// heuristic engine AI. LLM opponents are strictly opt-in and never mandatory,
/// so no variant here may ever stall a game or a draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LlmError {
    /// The stored endpoint configuration cannot produce a request (no model,
    /// no key where one is required, unusable base URL).
    Configuration { detail: String },
    /// The response body was not valid JSON for this provider's envelope.
    MalformedResponse { detail: String },
    /// The provider returned a structured error envelope.
    Provider { detail: String },
    /// The provider returned a well-formed envelope carrying no assistant text
    /// (an empty completion, or a refusal with no content block).
    EmptyCompletion,
    /// The assistant text carried no choice this engine could bind to a legal
    /// option.
    UndecodableChoice { detail: String },
    /// The model named an option outside the engine-issued domain.
    ChoiceOutOfRange { choice: i64, option_count: usize },
    /// The decision the request was built for is no longer the decision the
    /// engine is waiting on. An ordinary race, not a failure of the model.
    StaleDecision,
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::Configuration { detail } => write!(f, "LLM configuration error: {detail}"),
            LlmError::MalformedResponse { detail } => {
                write!(f, "malformed LLM response: {detail}")
            }
            LlmError::Provider { detail } => write!(f, "LLM provider error: {detail}"),
            LlmError::EmptyCompletion => write!(f, "LLM returned no completion text"),
            LlmError::UndecodableChoice { detail } => {
                write!(f, "could not decode a choice from the LLM reply: {detail}")
            }
            LlmError::ChoiceOutOfRange {
                choice,
                option_count,
            } => write!(
                f,
                "LLM chose option {choice}, outside the {option_count} options offered"
            ),
            LlmError::StaleDecision => {
                write!(f, "the decision this LLM request was built for has changed")
            }
        }
    }
}

impl std::error::Error for LlmError {}

pub type LlmResult<T> = Result<T, LlmError>;
