//! Real-time "physical contest" model (Un-set cards: Eye to Eye's staring
//! contest, Mouth to Mouth's breath-holding contest).
//!
//! The Comprehensive Rules do not cover these contests — they are
//! Unhinged-era silly cards with no CR section — so nothing here carries a CR
//! annotation for the contest itself. The surrounding rules it plugs into
//! (CR 608.2c "if you win" riders, CR 109.4 controller lookup, CR 115 targets)
//! are annotated where they are implemented in `game::effects::contest`.
//!
//! An online game cannot ask players to physically stare or hold their breath,
//! so the contest is adapted into a **sustained clicking contest**: both
//! players keep activating a control and the first one to stop for longer than
//! the inactivity timeout loses. The state machine lives in
//! `game::contest`; this module is the serializable data model.
//!
//! # Time
//!
//! The engine never reads a clock. Every input is stamped by the *host*
//! (phase-server, WASM adapter) with a monotonic millisecond counter, so the
//! machine is deterministic and unit-testable without sleeping. All durations
//! and instants are `u64` milliseconds in that host-defined timebase.

use serde::{Deserialize, Serialize};

use super::player::PlayerId;

/// Which printed contest is being adapted. Selects presentation only — the
/// timing, input, winner and lifecycle logic is identical for every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContestKind {
    /// Eye to Eye: "You and target creature's controller have a staring contest."
    Staring,
    /// Mouth to Mouth: "You and that player have a breath-holding contest."
    BreathHolding,
}

/// Lifecycle of a contest. See `ContestPhase::can_transition_to` for the legal
/// edges; `Completed` and `Cancelled` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContestPhase {
    /// Created by the resolving effect; the host has not started the clock.
    Pending,
    /// Both players are notified and the start countdown is running. Clicks are
    /// not counted.
    Preparing,
    /// Clicks are accepted and inactivity is monitored.
    Active,
    /// The outcome has been determined; further input is ignored. Transient —
    /// the engine settles it into `Completed` in the same host call.
    Resolving,
    /// The outcome was applied to the game.
    Completed,
    /// The contest was aborted without an outcome (a technical cancellation,
    /// distinct from a tie).
    Cancelled,
}

impl ContestPhase {
    /// Whether `self -> next` is a legal lifecycle edge.
    pub fn can_transition_to(self, next: ContestPhase) -> bool {
        use ContestPhase::*;
        matches!(
            (self, next),
            (Pending, Preparing)
                | (Preparing, Active)
                | (Active, Resolving)
                | (Resolving, Completed)
                // A forfeit before GO (disconnect past grace) decides the contest
                // without an Active stretch.
                | (Pending | Preparing, Resolving)
                | (Pending | Preparing | Active | Resolving, Cancelled)
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, ContestPhase::Completed | ContestPhase::Cancelled)
    }
}

/// Why a decided contest ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContestLossReason {
    /// The loser stopped clicking for longer than the inactivity timeout while
    /// the winner was still active.
    Inactivity,
    /// The loser disconnected and did not return within the reconnection grace
    /// period. Forfeits the contest only, never the game.
    Forfeit,
}

/// The decided result of a contest that ran to completion.
///
/// A tie is a first-class outcome: it is neither a win, a loss, nor a
/// cancellation, and triggers no win/loss consequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContestOutcome {
    Decided {
        winner: PlayerId,
        loser: PlayerId,
        reason: ContestLossReason,
    },
    /// Both players stopped within the same settle window.
    Tie,
}

/// Why a contest ended without an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContestCancelReason {
    /// Both players were gone past the grace period.
    BothDisconnected,
    /// The contest hit its maximum duration (a safety net against endless
    /// activity, e.g. scripted input from both seats).
    TimeLimit,
    /// A participant left the game (concession / elimination) mid-contest.
    ParticipantLeftGame,
    /// The host aborted the contest (shutdown, unrecoverable error).
    HostAborted,
}

/// Terminal result of a contest: an outcome, or a technical cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContestResult {
    Outcome(ContestOutcome),
    Cancelled(ContestCancelReason),
}

/// Tunable contest timing. Defaults suit human click rates over typical
/// network conditions; hosts may override per deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContestConfig {
    /// Countdown between "contest about to begin" and "GO".
    pub countdown_ms: u64,
    /// A player who has not clicked for longer than this is lapsed.
    pub inactivity_timeout_ms: u64,
    /// How long a disconnected player may take to return before forfeiting.
    /// The contest clock is paused for everyone while any seat is away.
    pub reconnect_grace_ms: u64,
    /// Jitter allowance and simultaneity window. A lapsed player may still
    /// recover by clicking until `deadline + settle_window_ms`; two players
    /// whose deadlines fall within this window of each other are *simultaneous*
    /// and the contest is tied.
    pub settle_window_ms: u64,
    /// Hard cap on the Active stretch.
    pub max_active_ms: u64,
}

impl Default for ContestConfig {
    fn default() -> Self {
        Self {
            countdown_ms: 3_000,
            inactivity_timeout_ms: 2_000,
            reconnect_grace_ms: 10_000,
            settle_window_ms: 150,
            max_active_ms: 300_000,
        }
    }
}

/// Whether a participant's seat is currently attached to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContestantConnection {
    Connected,
    /// Away since `since_ms` (host timebase).
    Disconnected {
        since_ms: u64,
    },
}

/// One side of the contest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contestant {
    pub player: PlayerId,
    /// Valid clicks counted while Active (display only — never decides the
    /// winner).
    pub clicks: u32,
    /// Host instant of the last activity (initially the GO instant).
    pub last_activity_ms: u64,
    /// Highest click sequence number accepted, for duplicate/reorder
    /// protection.
    pub last_seq: Option<u32>,
    pub connection: ContestantConnection,
}

/// Why a submitted click was not counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClickRejection {
    /// Submitter is not one of the two contestants.
    NotParticipant,
    /// The contest is not Active (not started, or already decided).
    NotActive,
    /// Duplicate or out-of-order delivery (sequence not newer than the last).
    StaleSequence,
    /// The submitter's recovery window already closed.
    Lapsed,
    /// The click was addressed to a different contest than the one running
    /// (a frame delayed from an earlier contest). Ignored without advancing time.
    WrongContest,
}

/// Result of submitting a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClickDisposition {
    Counted,
    Rejected(ClickRejection),
}

/// Complete contest state: carried inside `WaitingFor::PhysicalContest`, so it
/// is part of `GameState` and survives reconnects/persistence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContestState {
    /// Engine-authored identity (derived from the state revision at resolution and
    /// the interaction session, so a contest replayed after a takeback restore gets
    /// a new one) and immutable for the contest's lifetime. Every host input (click,
    /// progress frame, AI schedule) is bound to it so a frame delayed from an earlier
    /// contest can never be mistaken for activity in a later one.
    #[serde(default)]
    pub id: u64,
    pub kind: ContestKind,
    pub phase: ContestPhase,
    pub config: ContestConfig,
    /// Exactly two contestants: index 0 is the effect's controller.
    pub contestants: [Contestant; 2],
    /// Highest host instant observed. Time never runs backwards even if the
    /// host delivers stamps out of order.
    pub clock_ms: u64,
    /// GO instant, set when the countdown starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go_at_ms: Option<u64>,
    /// Instant the clock was paused because a seat is away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_since_ms: Option<u64>,
    /// Set when the phase reaches `Resolving`, `Completed` or `Cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ContestResult>,
}
