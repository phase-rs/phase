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
    /// The configured provider cannot be asked this particular decision, as
    /// opposed to the endpoint being misconfigured: a typed Choice needs 2 to
    /// 255 options, so a forced move (one option) or an enormous domain has no
    /// question to pose. The caller takes the heuristic for this decision.
    UnsupportedDecision { detail: String },
    /// The decision the request was built for is no longer the decision the
    /// engine is waiting on. An ordinary race, not a failure of the model.
    StaleDecision,
    /// The provider refused a message-thread request: it would not continue
    /// the seat's thread, or cannot use threads at all. Threads are best effort
    /// (one expires after five idle minutes), so this is not the model failing:
    /// the seat's thread has been dropped, and a game decision is asked once
    /// more as a fresh request.
    ThreadUnavailable { code: ThreadErrorCode },
}

/// Why the Anthropic Messages API refused a message-thread request.
///
/// Read from the error body's `error.details.error_code`, never from the HTTP
/// status alone: a 404 also answers an unknown model, which is not a thread
/// error and must not be retried as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadErrorCode {
    /// The thread expired, was never stored, or belongs to another account.
    NotFound,
    /// The previous reply's id has already been continued once.
    AlreadyContinued,
    /// Another continue from the same id is still running.
    ContinueInFlight,
    /// A field that must stay fixed for the whole thread changed.
    FingerprintMismatch,
    /// The request, or the organization, cannot use threads at all.
    UnsupportedRequest,
}

impl ThreadErrorCode {
    /// The code for an `error.details.error_code` value, or `None` for any
    /// value that is not a thread error.
    pub fn from_api_code(code: &str) -> Option<ThreadErrorCode> {
        match code {
            "thread_not_found" => Some(ThreadErrorCode::NotFound),
            "thread_already_continued" => Some(ThreadErrorCode::AlreadyContinued),
            "thread_continue_in_flight" => Some(ThreadErrorCode::ContinueInFlight),
            "thread_fingerprint_mismatch" => Some(ThreadErrorCode::FingerprintMismatch),
            "thread_unsupported_request" => Some(ThreadErrorCode::UnsupportedRequest),
            _ => None,
        }
    }

    /// The API's own spelling of this code.
    pub const fn api_code(self) -> &'static str {
        match self {
            ThreadErrorCode::NotFound => "thread_not_found",
            ThreadErrorCode::AlreadyContinued => "thread_already_continued",
            ThreadErrorCode::ContinueInFlight => "thread_continue_in_flight",
            ThreadErrorCode::FingerprintMismatch => "thread_fingerprint_mismatch",
            ThreadErrorCode::UnsupportedRequest => "thread_unsupported_request",
        }
    }
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
            LlmError::UnsupportedDecision { detail } => {
                write!(f, "this decision cannot be put to the provider: {detail}")
            }
            LlmError::StaleDecision => {
                write!(f, "the decision this LLM request was built for has changed")
            }
            LlmError::ThreadUnavailable { code } => write!(
                f,
                "the provider refused the message thread ({})",
                code.api_code()
            ),
        }
    }
}

impl std::error::Error for LlmError {}

pub type LlmResult<T> = Result<T, LlmError>;
