//! Offline game-state analysis used by the infinite-combo detector.
//!
//! This module is **purely additive** and changes no game behavior. It provides
//! the measurement substrate the net-progress loop detector is built on:
//!
//! - [`ResourceVector`] — a snapshot/delta of the *monotone* resources a loop
//!   can pump (mana, life, damage, library size, tokens, draws, triggers,
//!   counters, …). See [`resource`].
//! - [`loop_states_equal_modulo_resources`] — the **complement** of the existing
//!   strict CR 104.4b loop equality (`types::game_state::loop_states_equal`):
//!   board/zones/tap-state must be identical, but the monotone resources are
//!   allowed to differ. This is what distinguishes a *net-progress* (CR 732.2)
//!   loop from a *mandatory-draw* (CR 104.4b) loop.
//!
//! The strict comparison treats differing life/damage/counters as different
//! states (correct for a mandatory loop → draw). The detector needs the inverse:
//! "same board, resources may differ" → a beneficial loop that should be
//! shortcut (CR 732.2a) rather than drawn (CR 104.4b / CR 732.4).
//!
//! - [`sim`] — the offline simulation harness ([`LoopProbe`] / [`accumulate_events`])
//!   that drives `GameRunner::act` and *feeds* the event-fed `ResourceVector`
//!   axes (damage, tokens, draws, casts, triggers) from the runner's event
//!   stream, which a single `GameState` snapshot cannot supply.
//! - [`loop_check`] — Engine A: [`detect_loop`] turns a same-board-plus-net-progress
//!   measurement into a [`LoopCertificate`] (the unbounded axes + a [`WinKind`]),
//!   the offline classification the corpus harness asserts against. Still
//!   **zero gameplay change** — never called from the reducer.
//! - [`ability_graph`] — Engine B: [`candidate_cycles`] is the static, offline
//!   candidate generator. From a list of `CardFace` ASTs it builds an
//!   ability/resource graph, finds SCCs, and emits over-approximate
//!   [`CandidateCycle`]s for Engine A to confirm. Like the rest of this module it
//!   is **purely additive** — it never drives the reducer and never touches a
//!   `GameState`.

pub mod ability_graph;
pub mod decision_template;
pub mod loop_check;
pub mod resource;
pub mod sim;

// The combo corpus + bespoke driver toolkit, shared by the `#[cfg(test)]`
// acceptance suite and the `combo-verify` CLI. Gated so it is excluded from the
// shipped lib / WASM surface (no game behavior change).
#[cfg(any(test, feature = "combo-verify"))]
pub mod corpus;

#[cfg(test)]
mod corpus_tests;

pub use ability_graph::{candidate_cycles, AbilityGraph, CandidateCycle};
#[cfg(any(test, feature = "combo-verify"))]
pub use corpus::{
    corpus_len, drive_row, row, ComboRow, DeferralBucket, ResourceFamily, RowReport, RowStatus,
};
pub use decision_template::{
    predictability_gate, resolve, ConcreteDecision, ConcreteTarget, DecisionSlot, DecisionSource,
    DecisionTemplate, IterationCount, IterationIndex, PinnedDecision, PredictabilityViolation,
    ReplayFailure, ReplayMode, TargetPin, TargetSchedule,
};
pub use loop_check::{detect_loop, LoopCertificate, WinKind};
pub use resource::{
    board_delta, loop_states_equal_modulo_resources, BoardDelta, CounterClass, ObjectClass,
    ResidualPermanent, ResourceAxis, ResourceVector, TriggerKind,
};
pub use sim::{accumulate_events, LoopProbe};
