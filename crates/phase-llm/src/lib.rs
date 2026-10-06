//! LLM-driven opponents for phase.rs.
//!
//! # What this crate is
//!
//! An engine-owned adapter that lets a large language model occupy an AI seat —
//! in a game, or in a draft pod — **only when a player has configured one**. It
//! is strictly opt-in: with no configured endpoint, nothing here runs and every
//! seat is driven by the heuristic AI in `phase-ai` exactly as before.
//!
//! # The shape of the integration
//!
//! A model never authors a game action or names a card. It is handed the
//! engine-issued option domain — an `AiDecisionContract`'s candidates in a game,
//! the pack in front of a seat in a draft — and returns an INDEX into it. The
//! engine then re-validates that selection through the same authority path the
//! heuristic AI uses. There is deliberately no route by which a model's text
//! becomes an action without passing an engine legality check.
//!
//! # Where the network lives
//!
//! Not here. [`wire::build_chat_request`] produces an
//! [`HttpRequestSpec`](provider::HttpRequestSpec) that a transport executes
//! verbatim, and [`wire::extract_completion_text`] parses the raw body back. The
//! transport chooses nothing — not the URL, not the auth scheme, not the prompt,
//! not the fallback. That keeps every LLM opponent behaviour testable in Rust
//! and keeps the display layer a display layer.
//!
//! # Failure is always recoverable
//!
//! Every error in [`error::LlmError`] means "fall back to the heuristic AI".
//! A missing key, a rate limit, a timeout, a model that answers in prose, a
//! decision that moved on while the request was in flight — none of them may
//! stall a game or a draft.

pub mod catalog;
pub mod error;
pub mod fingerprint;
pub mod game_decision;
pub mod probe;
pub mod prompt;
pub mod provider;
pub mod render;
pub mod wire;

#[cfg(feature = "draft")]
pub mod draft_decision;

pub use error::{LlmError, LlmResult};
pub use game_decision::{
    build_game_decision_prompt, decision_fingerprint, select_action, GameDecisionRequest,
    LlmActionSelection,
};
pub use probe::{connection_probe_prompt, validate_probe_response};
pub use prompt::{difficulty_brief, LlmChoice, LlmPrompt};
pub use provider::{
    HttpHeader, HttpRequestSpec, LlmEndpointConfig, LlmProvider, WireProtocol,
    ACCEPTED_PROVIDER_LABELS,
};
pub use wire::{build_chat_request, completion_from_response, extract_completion_text};

#[cfg(feature = "draft")]
pub use draft_decision::{
    build_draft_pick_prompt, pick_fingerprint, select_picks, DraftPickRequest, LlmPickSelection,
};
