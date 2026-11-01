//! Deterministic state machine for the online "physical contest" adaptation
//! (see `types::contest` for the model and rationale).
//!
//! # Ordering policy
//!
//! The host feeds inputs one at a time, each stamped with its own monotonic
//! clock. **Before any input is applied, time is first advanced to that input's
//! stamp**, so deadlines that expired earlier resolve first. Concretely:
//!
//! * a player's deadline is `last_activity + inactivity_timeout`;
//! * a click stamped *at* the deadline counts (a timeout needs `now > deadline`);
//! * a lapsed player may still recover by clicking until
//!   `deadline + settle_window`; after that they are *out* and their clicks are
//!   rejected;
//! * the contest is decided once at least one player has lapsed and every
//!   lapsed player is out. If both lapsed and their deadlines are within
//!   `settle_window` of each other it is a [`ContestOutcome::Tie`]; otherwise
//!   the earlier deadline loses; a single lapsed player loses.
//!
//! The outcome depends only on stamps and deadlines, never on how late a tick
//! happened to run.

use crate::game::public_state::{
    bump_state_revision, finalize_public_state, mark_public_state_all_dirty,
};
use crate::types::ability::EffectKind;
use crate::types::contest::{
    ClickDisposition, ClickRejection, ContestCancelReason, ContestConfig, ContestKind,
    ContestLossReason, ContestOutcome, ContestPhase, ContestResult, ContestState, Contestant,
    ContestantConnection,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;

impl ContestState {
    /// A new contest in `Pending`. `controller` is contestant 0.
    pub fn new(
        id: u64,
        kind: ContestKind,
        controller: PlayerId,
        opponent: PlayerId,
        config: ContestConfig,
    ) -> Self {
        let contestant = |player| Contestant {
            player,
            clicks: 0,
            last_activity_ms: 0,
            last_seq: None,
            connection: ContestantConnection::Connected,
        };
        Self {
            id,
            kind,
            phase: ContestPhase::Pending,
            config,
            contestants: [contestant(controller), contestant(opponent)],
            clock_ms: 0,
            go_at_ms: None,
            paused_since_ms: None,
            result: None,
        }
    }

    pub fn is_participant(&self, player: PlayerId) -> bool {
        self.index_of(player).is_some()
    }

    pub fn players(&self) -> [PlayerId; 2] {
        [self.contestants[0].player, self.contestants[1].player]
    }

    fn index_of(&self, player: PlayerId) -> Option<usize> {
        self.contestants.iter().position(|c| c.player == player)
    }

    fn observe(&mut self, now_ms: u64) -> u64 {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.clock_ms
    }

    /// Apply a lifecycle edge; returns whether it was legal. Illegal edges are
    /// no-ops so every public entry point is idempotent.
    fn transition(&mut self, next: ContestPhase) -> bool {
        if self.phase.can_transition_to(next) {
            self.phase = next;
            true
        } else {
            false
        }
    }

    /// Inactivity deadline for contestant `idx`.
    pub fn deadline_ms(&self, idx: usize) -> u64 {
        self.contestants[idx].last_activity_ms + self.config.inactivity_timeout_ms
    }

    /// `Pending -> Preparing`: start the countdown. Idempotent — a duplicate
    /// start (retry, duplicate event) changes nothing and returns `false`.
    pub fn start(&mut self, now_ms: u64) -> bool {
        // A duplicate start must not observe `now_ms`: a retried start stamped after
        // buffered clicks would otherwise advance the clock past their stamps.
        if self.phase != ContestPhase::Pending {
            return false;
        }
        let now = self.observe(now_ms);
        if !self.transition(ContestPhase::Preparing) {
            return false;
        }
        self.go_at_ms = Some(now + self.config.countdown_ms);
        // A seat that was already away before the clock started keeps the
        // contest paused; restart the pause from the countdown's own start so
        // the countdown is not over-extended on return.
        if self.paused_since_ms.is_some() {
            self.paused_since_ms = Some(now);
        }
        true
    }

    /// Evaluate time-driven transitions up to `now_ms`: countdown completion,
    /// reconnect-grace expiry, inactivity lapses and the duration cap.
    pub fn advance(&mut self, now_ms: u64) {
        let now = self.observe(now_ms);
        if self.phase.is_terminal() || self.phase == ContestPhase::Resolving {
            return;
        }
        if self.settle_forfeits(now) {
            return;
        }
        // The clock is paused for everyone while any seat is away.
        if self.paused_since_ms.is_some() {
            return;
        }
        if self.phase == ContestPhase::Preparing {
            if let Some(go) = self.go_at_ms.filter(|&go| now >= go) {
                self.activate(go);
            }
        }
        if self.phase == ContestPhase::Active {
            // Inactivity is decided over the history up to the cap first, so a late
            // tick that crosses the cap cannot turn a contest an on-time tick would
            // have decided into a cancellation. Only an undecided contest is capped.
            self.settle_inactivity(now.min(self.time_limit_ms()));
            if self.phase == ContestPhase::Active {
                self.settle_time_limit(now);
            }
        }
    }

    /// The instant the Active phase hits its hard duration cap.
    fn time_limit_ms(&self) -> u64 {
        self.go_at_ms.unwrap_or(0) + self.config.max_active_ms
    }

    /// Unpaused Active-phase time elapsed at `now_ms`: the shared timebase for
    /// anything that schedules itself against the contest (AI cadence, stamina).
    /// Frozen while the clock is paused, because resuming shifts `go_at_ms` by the
    /// outage — the same adjustment the inactivity deadlines receive.
    pub fn active_elapsed_ms(&self, now_ms: u64) -> u64 {
        let until = self
            .paused_since_ms
            .map_or(now_ms, |paused| paused.min(now_ms));
        until.saturating_sub(self.go_at_ms.unwrap_or(until))
    }

    fn activate(&mut self, go_at: u64) {
        if self.transition(ContestPhase::Active) {
            for contestant in &mut self.contestants {
                contestant.last_activity_ms = go_at;
            }
        }
    }

    fn settle_time_limit(&mut self, now: u64) {
        if now > self.time_limit_ms() {
            self.cancel(ContestCancelReason::TimeLimit, now);
        }
    }

    fn settle_inactivity(&mut self, now: u64) {
        let window = self.config.settle_window_ms;
        let deadlines = [self.deadline_ms(0), self.deadline_ms(1)];
        let lapsed = deadlines.map(|d| now > d);
        let out = deadlines.map(|d| now > d + window);
        if !lapsed.iter().any(|&l| l) {
            return;
        }
        // Wait while any lapsed player can still recover.
        if (0..2).any(|i| lapsed[i] && !out[i]) {
            return;
        }
        let outcome = match lapsed {
            [true, true] if deadlines[0].abs_diff(deadlines[1]) <= window => ContestOutcome::Tie,
            [true, true] if deadlines[0] < deadlines[1] => {
                self.loses(0, ContestLossReason::Inactivity)
            }
            [true, true] => self.loses(1, ContestLossReason::Inactivity),
            [true, false] => self.loses(0, ContestLossReason::Inactivity),
            [false, true] => self.loses(1, ContestLossReason::Inactivity),
            [false, false] => return,
        };
        self.resolve(outcome);
    }

    /// Reconnect-grace expiry. Returns whether the contest was decided or
    /// cancelled. If both seats are away when any grace expires the contest is
    /// cancelled (nobody is left to win); otherwise the lone absentee forfeits.
    fn settle_forfeits(&mut self, now: u64) -> bool {
        let grace = self.config.reconnect_grace_ms;
        let expired: [bool; 2] = [0, 1].map(|i| {
            matches!(
                self.contestants[i].connection,
                ContestantConnection::Disconnected { since_ms } if now > since_ms + grace
            )
        });
        if !expired.iter().any(|&e| e) {
            return false;
        }
        let away = [0, 1].map(|i| {
            matches!(
                self.contestants[i].connection,
                ContestantConnection::Disconnected { .. }
            )
        });
        if away[0] && away[1] {
            self.cancel(ContestCancelReason::BothDisconnected, now);
        } else {
            let loser = if expired[0] { 0 } else { 1 };
            let outcome = self.loses(loser, ContestLossReason::Forfeit);
            self.resolve(outcome);
        }
        true
    }

    fn loses(&self, loser_idx: usize, reason: ContestLossReason) -> ContestOutcome {
        ContestOutcome::Decided {
            winner: self.contestants[1 - loser_idx].player,
            loser: self.contestants[loser_idx].player,
            reason,
        }
    }

    /// `* -> Resolving`: record the outcome and stop accepting input.
    fn resolve(&mut self, outcome: ContestOutcome) {
        if self.transition(ContestPhase::Resolving) {
            self.result = Some(ContestResult::Outcome(outcome));
        }
    }

    /// Abort without an outcome. Idempotent once terminal.
    pub fn cancel(&mut self, reason: ContestCancelReason, now_ms: u64) {
        self.observe(now_ms);
        if self.transition(ContestPhase::Cancelled) {
            self.result = Some(ContestResult::Cancelled(reason));
        }
    }

    /// `Resolving -> Completed`. Returns the result exactly once; later calls
    /// return `None`, so an outcome can never be applied twice.
    pub fn complete(&mut self) -> Option<ContestResult> {
        if self.transition(ContestPhase::Completed) {
            self.result
        } else {
            None
        }
    }

    /// Submit a click for `player`. `seq` must increase per player; duplicates
    /// and reordered deliveries are rejected.
    pub fn click(&mut self, player: PlayerId, seq: u32, now_ms: u64) -> ClickDisposition {
        // Timeouts that expired before this click's stamp resolve first.
        self.advance(now_ms);
        let now = self.clock_ms;
        let Some(idx) = self.index_of(player) else {
            return ClickDisposition::Rejected(ClickRejection::NotParticipant);
        };
        if self.phase != ContestPhase::Active {
            return ClickDisposition::Rejected(ClickRejection::NotActive);
        }
        if self.contestants[idx]
            .last_seq
            .is_some_and(|last| seq <= last)
        {
            return ClickDisposition::Rejected(ClickRejection::StaleSequence);
        }
        let paused = self.paused_since_ms.is_some();
        if !paused && now > self.deadline_ms(idx) + self.config.settle_window_ms {
            return ClickDisposition::Rejected(ClickRejection::Lapsed);
        }
        let contestant = &mut self.contestants[idx];
        contestant.last_seq = Some(seq);
        contestant.clicks = contestant.clicks.saturating_add(1);
        // While paused the clock is frozen, so activity is not restamped; the
        // click still counts for display.
        if !paused {
            contestant.last_activity_ms = now;
        }
        ClickDisposition::Counted
    }

    /// Record a seat attaching/detaching. Returns whether the state changed.
    /// Idempotent: repeating the current state is a no-op.
    pub fn set_connected(&mut self, player: PlayerId, connected: bool, now_ms: u64) -> bool {
        let Some(idx) = self.index_of(player) else {
            return false;
        };
        // Reporting the state a seat is already in is clock-neutral: hosts repeat the
        // report every tick, and a repeat must not advance the contest clock past
        // input the host has stamped earlier but not yet delivered.
        let is_edge = matches!(
            (self.contestants[idx].connection, connected),
            (ContestantConnection::Connected, false)
                | (ContestantConnection::Disconnected { .. }, true)
        );
        if !is_edge {
            return false;
        }
        // Expired deadlines / grace resolve before the connection change.
        self.advance(now_ms);
        let now = self.clock_ms;
        if self.phase.is_terminal() || self.phase == ContestPhase::Resolving {
            return false;
        }
        match (self.contestants[idx].connection, connected) {
            (ContestantConnection::Connected, false) => {
                self.contestants[idx].connection =
                    ContestantConnection::Disconnected { since_ms: now };
                self.paused_since_ms.get_or_insert(now);
                true
            }
            (ContestantConnection::Disconnected { .. }, true) => {
                self.contestants[idx].connection = ContestantConnection::Connected;
                if self
                    .contestants
                    .iter()
                    .all(|c| c.connection == ContestantConnection::Connected)
                {
                    self.resume(now);
                }
                true
            }
            _ => false,
        }
    }

    /// Unpause: shift every running clock by the time spent paused, so a
    /// reconnecting player does not lose (or regain) inactivity budget.
    fn resume(&mut self, now: u64) {
        let Some(paused_since) = self.paused_since_ms.take() else {
            return;
        };
        let elapsed = now.saturating_sub(paused_since);
        match self.phase {
            ContestPhase::Preparing => {
                self.go_at_ms = self.go_at_ms.map(|go| go + elapsed);
            }
            ContestPhase::Active => {
                // The hard cap measures unpaused play, so its epoch shifts with the
                // inactivity clocks.
                self.go_at_ms = self.go_at_ms.map(|go| go + elapsed);
                for contestant in &mut self.contestants {
                    contestant.last_activity_ms += elapsed;
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Host driver: the entry points a transport calls while the game is parked on
// `WaitingFor::PhysicalContest`. Nobody submits a `GameAction` there, so these
// are what advance the state (`NoActor::HostContestDriver`).
// ---------------------------------------------------------------------------

/// What a driver call did to the game, so the transport knows what to publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContestStep {
    /// The game is not waiting on a contest (it already settled, or never began).
    NotInContest,
    /// The input changed nothing visible (e.g. a rejected click, a tick inside a phase).
    Unchanged,
    /// The contest is still running but its visible state changed (countdown
    /// started, GO, a seat attached/detached).
    Updated,
    /// The contest reached a terminal phase; its result was applied to the game
    /// and the parked continuation resumed. The transport must publish the new
    /// game state and the emitted events.
    Settled,
}

/// The parts of a contest a client renders, compared before/after a call.
fn visible_signature(
    contest: &ContestState,
) -> (
    ContestPhase,
    [ContestantConnection; 2],
    Option<u64>,
    Option<u64>,
) {
    (
        contest.phase,
        [
            contest.contestants[0].connection,
            contest.contestants[1].connection,
        ],
        contest.go_at_ms,
        contest.paused_since_ms,
    )
}

/// Run `input` against the live contest, then settle if it reached a terminal
/// phase. The single funnel for every driver entry point.
fn drive<R>(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    input: impl FnOnce(&mut ContestState) -> R,
) -> (Option<R>, ContestStep) {
    let WaitingFor::PhysicalContest { contest, .. } = &mut state.waiting_for else {
        return (None, ContestStep::NotInContest);
    };
    let before = visible_signature(contest);
    let output = input(contest);
    let terminal = matches!(
        contest.phase,
        ContestPhase::Resolving | ContestPhase::Completed | ContestPhase::Cancelled
    );
    let changed = visible_signature(contest) != before;
    if terminal {
        settle(state, events);
        publish_state(state);
        (Some(output), ContestStep::Settled)
    } else if changed {
        publish_state(state);
        (Some(output), ContestStep::Updated)
    } else {
        (Some(output), ContestStep::Unchanged)
    }
}

/// A driver step mutated the game outside `apply()`, so do the bookkeeping
/// `apply()` would: advance the state revision and refresh the public projection
/// that clients render.
fn publish_state(state: &mut GameState) {
    // The wait just changed outside `apply()` (a contest settling into a priority
    // window, say), so the interaction slots `apply()` would have rebound must be
    // rebound here: without them the new prompt has no valid interaction id and
    // the player's answer to it is rejected.
    super::interaction::ensure_interaction_authority(state);
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    finalize_public_state(state);
}

/// Begin the countdown (`Pending -> Preparing`) with the host's timing
/// `config`. Idempotent: a duplicate start leaves the running contest alone.
pub fn start_contest(
    state: &mut GameState,
    config: ContestConfig,
    now_ms: u64,
    events: &mut Vec<GameEvent>,
) -> ContestStep {
    drive(state, events, |contest| {
        if contest.phase == ContestPhase::Pending {
            contest.config = config;
        }
        contest.start(now_ms)
    })
    .1
}

/// Advance contest time to `now_ms`. The host calls this on a short interval.
pub fn advance_contest(
    state: &mut GameState,
    now_ms: u64,
    events: &mut Vec<GameEvent>,
) -> ContestStep {
    // A participant who left the game (concession / elimination) can no longer
    // contest; abort rather than wait out a grace period for a player who is gone.
    if let WaitingFor::PhysicalContest { contest, .. } = &state.waiting_for {
        let departed = contest
            .players()
            .into_iter()
            .any(|player| !crate::game::players::is_alive(state, player));
        if departed {
            return cancel_contest(
                state,
                ContestCancelReason::ParticipantLeftGame,
                now_ms,
                events,
            );
        }
    }
    drive(state, events, |contest| contest.advance(now_ms)).1
}

/// Submit a click addressed to contest `contest_id`. `seq` is the submitter's
/// per-contest click counter. A click for any other contest — a frame delayed from
/// an earlier one — is rejected before it can observe time or touch the sequence
/// counters of the running contest.
pub fn contest_click(
    state: &mut GameState,
    contest_id: u64,
    player: PlayerId,
    seq: u32,
    now_ms: u64,
    events: &mut Vec<GameEvent>,
) -> (ClickDisposition, ContestStep) {
    let (disposition, step) = drive(state, events, |contest| {
        if contest.id != contest_id {
            return ClickDisposition::Rejected(ClickRejection::WrongContest);
        }
        contest.click(player, seq, now_ms)
    });
    (
        disposition.unwrap_or(ClickDisposition::Rejected(ClickRejection::NotActive)),
        step,
    )
}

/// Record a seat attaching or detaching.
pub fn set_contest_connection(
    state: &mut GameState,
    player: PlayerId,
    connected: bool,
    now_ms: u64,
    events: &mut Vec<GameEvent>,
) -> ContestStep {
    drive(state, events, |contest| {
        contest.set_connected(player, connected, now_ms)
    })
    .1
}

/// Abort the contest with no outcome (host shutdown, unrecoverable error).
pub fn cancel_contest(
    state: &mut GameState,
    reason: ContestCancelReason,
    now_ms: u64,
    events: &mut Vec<GameEvent>,
) -> ContestStep {
    drive(state, events, |contest| contest.cancel(reason, now_ms)).1
}

/// Settle a contest that a restore found in flight.
///
/// A contest's deadlines, grace timestamps and hard cap are stamped on the host
/// clock of the process that ran it, and that clock does not survive the process:
/// a state exported from one page or worker and imported into another carries
/// timestamps from an epoch the new host cannot compare with its own. Continuing
/// would freeze the contest until the new clock caught up with the old one
/// (`observe` never moves backwards) and would keep grace deadlines that no longer
/// mean anything. So every restore authority cancels it, through the ordinary
/// cancellation and settlement: the controller does not win, the parked
/// continuation resumes exactly once, and the game carries on. The events are the
/// ordinary settlement's; a restore has no audience for them.
///
/// Stamped with the contest's own clock, so cancelling depends on no new epoch.
/// A no-op outside a contest.
pub fn cancel_contest_after_restore(state: &mut GameState) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let WaitingFor::PhysicalContest { contest, .. } = &state.waiting_for else {
        return events;
    };
    let now_ms = contest.clock_ms;
    cancel_contest(state, ContestCancelReason::HostAborted, now_ms, &mut events);
    events
}

/// Apply a finished contest to the game exactly once and resume the parked
/// continuation.
///
/// The controller's win is recorded as `optional_effect_performed` on the parked
/// continuation — the same channel a settled clash uses — so the printed "if you
/// win" rider (`AbilityCondition::EventOutcomeWon`) fires only for a decided win
/// by the controller. A loss, a tie and a cancellation all leave it false, so
/// none triggers the rider's consequence.
fn settle(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let WaitingFor::PhysicalContest { contest, source_id } = &mut state.waiting_for else {
        return;
    };
    let source_id = *source_id;
    // `complete()` yields the result once; a cancelled contest already holds it.
    let result = contest.complete().or(contest.result);
    let (kind, participants) = (contest.kind, contest.players());
    let controller = participants[0];
    let Some(result) = result else {
        // Unreachable by construction (a terminal phase always carries a result);
        // fail toward "nothing happens" rather than wedging the game.
        state.waiting_for = WaitingFor::Priority { player: controller };
        state.priority_player = controller;
        return;
    };
    let controller_won = matches!(
        result,
        ContestResult::Outcome(ContestOutcome::Decided { winner, .. }) if winner == controller
    );

    events.push(GameEvent::PhysicalContestResolved {
        kind,
        participants,
        result,
    });
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PhysicalContest,
        source_id,
        subject: None,
    });

    // CR 608.2c: "If you win" is read from the performed instruction; stamp it
    // on the continuation that carries the rider.
    if let Some(frame) = state.active_ability_continuation_frame_mut() {
        frame.pending.chain.context.optional_effect_performed = controller_won;
    }

    // CR 117.3b: the active player receives priority after a spell or ability
    // resolves — not necessarily the contest's controller (Eye to Eye is an instant).
    let active_player = state.active_player;
    state.waiting_for = WaitingFor::Priority {
        player: active_player,
    };
    state.priority_player = active_player;
    let event_start = events.len();
    super::engine::resume_pending_continuation_if_priority(state, events)
        .expect("a settled contest must resume its continuation");

    // CR 117.5 + CR 704.3 + CR 603.3: the host settled this outside `apply()`, so run
    // the pass `apply()` would — state-based actions (a destroyed token ceasing to
    // exist, the legend rule after control changes), then triggers put on the stack —
    // before anyone receives priority. A continuation that paused on a decision of its
    // own is left parked; the action that answers it runs the pipeline.
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        let default_wf = state.waiting_for.clone();
        match super::engine_priority::run_post_action_pipeline_from(
            state,
            events,
            event_start,
            &default_wf,
            false,
            false,
        ) {
            Ok(settled) => super::public_state::sync_waiting_for(state, &settled),
            // The effect already resolved; fail toward the priority window already
            // installed rather than wedging a host-driven wait nobody can answer.
            Err(error) => debug_assert!(false, "contest settlement pipeline failed: {error:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PlayerId = PlayerId(0);
    const B: PlayerId = PlayerId(1);

    fn cfg() -> ContestConfig {
        ContestConfig {
            countdown_ms: 1_000,
            inactivity_timeout_ms: 1_000,
            reconnect_grace_ms: 5_000,
            settle_window_ms: 0,
            max_active_ms: 60_000,
        }
    }

    /// Active contest whose GO instant is t=1000.
    fn active(config: ContestConfig) -> ContestState {
        let mut c = ContestState::new(1, ContestKind::Staring, A, B, config);
        assert!(c.start(0));
        c.advance(config.countdown_ms);
        assert_eq!(c.phase, ContestPhase::Active);
        c
    }

    fn decided(c: &ContestState) -> Option<ContestOutcome> {
        match c.result {
            Some(ContestResult::Outcome(o)) => Some(o),
            _ => None,
        }
    }

    fn a_wins() -> ContestOutcome {
        ContestOutcome::Decided {
            winner: A,
            loser: B,
            reason: ContestLossReason::Inactivity,
        }
    }

    #[test]
    fn lifecycle_edges_are_restricted() {
        use ContestPhase::*;
        assert!(Pending.can_transition_to(Preparing));
        assert!(!Pending.can_transition_to(Active));
        assert!(!Active.can_transition_to(Preparing));
        assert!(!Completed.can_transition_to(Cancelled));
        assert!(!Cancelled.can_transition_to(Completed));
        assert!(Completed.is_terminal() && Cancelled.is_terminal());
    }

    #[test]
    fn start_is_idempotent() {
        let mut c = ContestState::new(1, ContestKind::Staring, A, B, cfg());
        assert!(c.start(100));
        let go = c.go_at_ms;
        assert!(
            !c.start(900),
            "duplicate start must not reset the countdown"
        );
        assert_eq!(c.go_at_ms, go);
        assert_eq!(c.phase, ContestPhase::Preparing);
    }

    #[test]
    fn clicks_before_go_are_not_counted() {
        let mut c = ContestState::new(1, ContestKind::Staring, A, B, cfg());
        assert_eq!(
            c.click(A, 1, 0),
            ClickDisposition::Rejected(ClickRejection::NotActive)
        );
        c.start(0);
        assert_eq!(
            c.click(A, 1, 999),
            ClickDisposition::Rejected(ClickRejection::NotActive)
        );
        assert_eq!(c.contestants[0].clicks, 0);
        assert_eq!(c.click(A, 1, 1_000), ClickDisposition::Counted);
        assert_eq!(c.phase, ContestPhase::Active);
    }

    #[test]
    fn one_player_stopping_loses() {
        let mut c = active(cfg());
        // B stops immediately; A keeps clicking.
        for (seq, t) in (1u32..).zip((1_500..=3_500).step_by(500)) {
            c.click(A, seq, t);
        }
        // B's deadline was 2000 and A is still active: B lost at the first
        // advance past it, so A's later clicks are no longer scored.
        assert_eq!(decided(&c), Some(a_wins()));
        assert_eq!(
            c.contestants[0].clicks, 2,
            "only clicks before the loss count"
        );
        assert_eq!(c.phase, ContestPhase::Resolving);
    }

    #[test]
    fn click_rate_does_not_decide_the_winner() {
        let mut c = active(cfg());
        // A clicks 50x as fast, but stops first; B trickles one click per ~900ms.
        let mut seq_a = 0;
        for t in (1_000..2_000).step_by(20) {
            seq_a += 1;
            c.click(A, seq_a, t);
        }
        c.click(B, 1, 1_900);
        c.click(B, 2, 2_800);
        // A's last click was ~1980; deadline ~2980. B clicked 2800, deadline 3800.
        c.advance(3_100);
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: B,
                loser: A,
                reason: ContestLossReason::Inactivity
            }),
            "the slower but longer-lasting player wins"
        );
        assert!(c.contestants[0].clicks > c.contestants[1].clicks);
    }

    #[test]
    fn click_exactly_at_deadline_counts_and_after_does_not() {
        let mut c = active(cfg());
        c.click(B, 1, 1_500);
        // A's deadline is 2000 (GO 1000 + 1000).
        assert_eq!(c.click(A, 1, 2_000), ClickDisposition::Counted);

        let mut c = active(cfg());
        c.click(B, 1, 1_500);
        assert_eq!(
            c.click(A, 1, 2_001),
            ClickDisposition::Rejected(ClickRejection::NotActive),
            "window is 0, so one millisecond late has already lost the contest"
        );
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: B,
                loser: A,
                reason: ContestLossReason::Inactivity
            })
        );
    }

    #[test]
    fn click_just_before_deadline_extends_it() {
        let mut c = active(cfg());
        assert_eq!(c.click(A, 1, 1_999), ClickDisposition::Counted);
        c.click(B, 1, 1_999);
        c.advance(2_500);
        assert_eq!(c.phase, ContestPhase::Active);
        assert_eq!(c.deadline_ms(0), 2_999);
    }

    #[test]
    fn both_stopping_simultaneously_is_a_tie() {
        let mut c = active(cfg());
        c.click(A, 1, 1_400);
        c.click(B, 1, 1_400);
        c.advance(2_500);
        assert_eq!(decided(&c), Some(ContestOutcome::Tie));
    }

    #[test]
    fn never_clicking_is_a_tie_not_a_cancellation() {
        let mut c = active(cfg());
        c.advance(5_000);
        assert_eq!(decided(&c), Some(ContestOutcome::Tie));
        assert!(!matches!(c.result, Some(ContestResult::Cancelled(_))));
    }

    #[test]
    fn near_simultaneous_stops_within_window_tie_and_outside_do_not() {
        let config = ContestConfig {
            settle_window_ms: 100,
            ..cfg()
        };
        let mut c = active(config);
        c.click(A, 1, 1_400);
        c.click(B, 1, 1_500); // 100ms apart == window -> tie
        c.advance(10_000);
        assert_eq!(decided(&c), Some(ContestOutcome::Tie));

        let mut c = active(config);
        c.click(A, 1, 1_400);
        c.click(B, 1, 1_501); // 101ms apart -> A stopped first
        c.advance(10_000);
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: B,
                loser: A,
                reason: ContestLossReason::Inactivity
            })
        );
    }

    #[test]
    fn a_lapsed_player_can_recover_inside_the_window_before_finalization() {
        let config = ContestConfig {
            settle_window_ms: 100,
            ..cfg()
        };
        let mut c = active(config);
        c.click(B, 1, 1_900);
        // A's deadline is 2000; at 2050 A has lapsed but is not out.
        c.advance(2_050);
        assert_eq!(c.phase, ContestPhase::Active);
        assert_eq!(c.click(A, 1, 2_080), ClickDisposition::Counted);
        c.advance(2_500);
        assert_eq!(c.phase, ContestPhase::Active, "A recovered; no one lapsed");
    }

    #[test]
    fn a_player_past_their_window_cannot_click_while_the_other_is_still_recovering() {
        let config = ContestConfig {
            settle_window_ms: 100,
            ..cfg()
        };
        let mut c = active(config);
        c.click(A, 1, 1_000); // A deadline 2000
        c.click(B, 1, 1_050); // B deadline 2050
                              // At 2120 A is out (past 2100) but B (lapsed since 2050) is not (2150).
        assert_eq!(
            c.click(A, 2, 2_120),
            ClickDisposition::Rejected(ClickRejection::Lapsed)
        );
        assert_eq!(c.phase, ContestPhase::Active, "B may still recover");
        assert_eq!(c.click(B, 2, 2_130), ClickDisposition::Counted);
        c.advance(2_500);
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: B,
                loser: A,
                reason: ContestLossReason::Inactivity
            })
        );
    }

    #[test]
    fn tick_lateness_does_not_change_the_outcome() {
        let mut early = active(cfg());
        early.click(A, 1, 1_300);
        early.click(B, 1, 1_900);
        early.advance(2_400);
        let mut late = active(cfg());
        late.click(A, 1, 1_300);
        late.click(B, 1, 1_900);
        late.advance(60_000);
        assert_eq!(decided(&early), decided(&late));
    }

    #[test]
    fn outcome_is_final_and_late_clicks_are_ignored() {
        let mut c = active(cfg());
        c.click(B, 1, 1_900);
        c.advance(2_500); // A lapsed
        let outcome = decided(&c);
        assert_eq!(
            c.click(A, 1, 2_600),
            ClickDisposition::Rejected(ClickRejection::NotActive)
        );
        assert_eq!(
            c.click(B, 2, 2_600),
            ClickDisposition::Rejected(ClickRejection::NotActive)
        );
        assert_eq!(decided(&c), outcome);
        assert_eq!(c.complete(), Some(ContestResult::Outcome(outcome.unwrap())));
        assert_eq!(c.complete(), None, "the result is applied exactly once");
        assert_eq!(c.phase, ContestPhase::Completed);
    }

    #[test]
    fn duplicate_and_reordered_clicks_are_rejected() {
        let mut c = active(cfg());
        assert_eq!(c.click(A, 5, 1_100), ClickDisposition::Counted);
        assert_eq!(
            c.click(A, 5, 1_110),
            ClickDisposition::Rejected(ClickRejection::StaleSequence)
        );
        assert_eq!(
            c.click(A, 4, 1_120),
            ClickDisposition::Rejected(ClickRejection::StaleSequence)
        );
        assert_eq!(c.contestants[0].clicks, 1);
        assert_eq!(c.click(A, 6, 1_130), ClickDisposition::Counted);
    }

    #[test]
    fn non_participants_are_rejected() {
        let mut c = active(cfg());
        assert_eq!(
            c.click(PlayerId(2), 1, 1_100),
            ClickDisposition::Rejected(ClickRejection::NotParticipant)
        );
    }

    #[test]
    fn rapid_clicks_are_all_counted() {
        let mut c = active(cfg());
        for seq in 1..=200 {
            assert_eq!(
                c.click(A, seq, 1_100 + u64::from(seq) / 20),
                ClickDisposition::Counted
            );
        }
        assert_eq!(c.contestants[0].clicks, 200);
    }

    #[test]
    fn stale_host_clock_never_runs_backwards() {
        let mut c = active(cfg());
        c.click(A, 1, 1_500);
        c.click(B, 1, 1_200); // earlier stamp delivered late
        assert_eq!(c.clock_ms, 1_500);
        assert_eq!(c.contestants[1].last_activity_ms, 1_500);
    }

    #[test]
    fn disconnect_during_preparing_pauses_then_forfeits_past_grace() {
        let mut c = ContestState::new(1, ContestKind::BreathHolding, A, B, cfg());
        c.start(0);
        assert!(c.set_connected(B, false, 200));
        c.advance(1_500); // past original GO; paused so still Preparing
        assert_eq!(c.phase, ContestPhase::Preparing);
        c.advance(5_201); // 200 + grace(5000) exceeded
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: A,
                loser: B,
                reason: ContestLossReason::Forfeit
            })
        );
    }

    #[test]
    fn reconnect_within_grace_preserves_remaining_inactivity_budget() {
        let mut c = active(cfg());
        c.click(A, 1, 1_600);
        c.click(B, 1, 1_600);
        // Deadlines are 2600. B drops at 1_800 and returns at 4_800 (3s away).
        assert!(c.set_connected(B, false, 1_800));
        c.advance(4_000);
        assert_eq!(
            c.phase,
            ContestPhase::Active,
            "clock paused while B is away"
        );
        assert!(c.set_connected(B, true, 4_800));
        assert_eq!(c.deadline_ms(0), 5_600);
        assert_eq!(c.deadline_ms(1), 5_600);
        // Not reset to a full fresh timeout either: still 800ms of budget left.
        c.click(A, 2, 5_000);
        c.advance(5_700);
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: A,
                loser: B,
                reason: ContestLossReason::Inactivity
            })
        );
    }

    #[test]
    fn disconnect_during_active_past_grace_forfeits() {
        let mut c = active(cfg());
        c.set_connected(A, false, 1_500);
        c.advance(6_501);
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: B,
                loser: A,
                reason: ContestLossReason::Forfeit
            })
        );
    }

    #[test]
    fn both_disconnected_past_grace_cancels() {
        let mut c = active(cfg());
        c.set_connected(A, false, 1_500);
        c.set_connected(B, false, 1_600);
        c.advance(6_600);
        assert_eq!(
            c.result,
            Some(ContestResult::Cancelled(
                ContestCancelReason::BothDisconnected
            ))
        );
        assert_eq!(c.phase, ContestPhase::Cancelled);
    }

    #[test]
    fn connection_changes_are_idempotent() {
        let mut c = active(cfg());
        assert!(c.set_connected(A, false, 1_500));
        assert!(!c.set_connected(A, false, 1_600));
        assert_eq!(
            c.contestants[0].connection,
            ContestantConnection::Disconnected { since_ms: 1_500 }
        );
        assert!(c.set_connected(A, true, 1_700));
        assert!(!c.set_connected(A, true, 1_800));
    }

    #[test]
    fn a_player_cannot_dodge_a_loss_by_disconnecting_after_their_deadline() {
        let mut c = active(cfg());
        c.click(B, 1, 1_900);
        // A's deadline (2000) already passed when A's disconnect is processed.
        assert!(!c.set_connected(A, false, 2_500));
        assert_eq!(
            decided(&c),
            Some(ContestOutcome::Decided {
                winner: B,
                loser: A,
                reason: ContestLossReason::Inactivity
            })
        );
    }

    #[test]
    fn time_limit_cancels_endless_activity() {
        let config = ContestConfig {
            max_active_ms: 5_000,
            ..cfg()
        };
        let mut c = active(config);
        let mut seq = 0;
        for t in (1_500..=6_500).step_by(500) {
            seq += 1;
            c.click(A, seq, t);
            c.click(B, seq, t);
        }
        assert_eq!(
            c.result,
            Some(ContestResult::Cancelled(ContestCancelReason::TimeLimit))
        );
    }

    #[test]
    fn cancel_is_terminal_and_idempotent() {
        let mut c = active(cfg());
        c.cancel(ContestCancelReason::ParticipantLeftGame, 1_100);
        c.cancel(ContestCancelReason::HostAborted, 1_200);
        assert_eq!(
            c.result,
            Some(ContestResult::Cancelled(
                ContestCancelReason::ParticipantLeftGame
            ))
        );
        assert_eq!(c.complete(), None);
        assert_eq!(
            c.click(A, 1, 1_300),
            ClickDisposition::Rejected(ClickRejection::NotActive)
        );
    }

    /// The hard cap measures UNPAUSED play: an outage while a seat is away shifts
    /// the cap epoch exactly as it shifts the inactivity clocks, so the cap cannot
    /// fire early just because the contest sat paused.
    #[test]
    fn an_outage_does_not_count_toward_the_hard_cap() {
        let config = ContestConfig {
            max_active_ms: 3_000,
            reconnect_grace_ms: 10_000,
            ..cfg()
        };
        let mut c = active(config); // GO at 1000, so the cap is at 4000
        c.click(A, 1, 1_900);
        c.click(B, 1, 1_900);
        assert!(c.set_connected(B, false, 2_000));
        assert!(c.set_connected(B, true, 4_000)); // 2s outage: the cap moves to 6000

        // Still Active well past the unshifted cap, with both players clicking.
        c.click(A, 2, 4_500);
        c.click(B, 2, 4_500);
        c.click(A, 3, 5_300);
        c.click(B, 3, 5_300);
        c.click(A, 4, 5_900);
        c.click(B, 4, 5_900);
        assert_eq!(c.phase, ContestPhase::Active);

        c.advance(6_001);
        assert_eq!(
            c.result,
            Some(ContestResult::Cancelled(ContestCancelReason::TimeLimit))
        );
    }

    /// Hosts repeat each seat's connectivity every tick. A repeat of the state the seat
    /// is already in must not move the clock: a click stamped before a delayed tick
    /// still has to be judged against the deadlines in force when it was made.
    #[test]
    fn repeating_a_seats_connectivity_does_not_advance_the_clock() {
        let mut c = active(cfg()); // GO at 1000, deadlines at 2000
        let before = c.clock_ms;
        assert!(!c.set_connected(A, true, 5_151));
        assert!(!c.set_connected(B, true, 5_151));
        assert_eq!(c.clock_ms, before);
        assert_eq!(c.phase, ContestPhase::Active);

        // A click stamped inside A's window still counts and extends A's deadline.
        assert_eq!(c.click(A, 1, 1_999), ClickDisposition::Counted);
        assert_eq!(c.deadline_ms(0), 1_999 + cfg().inactivity_timeout_ms);
    }

    /// A real edge still resolves what expired before it, exactly as before.
    #[test]
    fn a_real_disconnect_edge_still_settles_earlier_expiry_first() {
        let mut c = active(cfg());
        c.click(A, 1, 2_900);
        // The contest was already decided by the time the edge applies, so it is moot.
        assert!(!c.set_connected(A, false, 3_500));
        // B lapsed at 2000 while A was still clicking: B lost before the edge applied.
        assert_eq!(c.phase, ContestPhase::Resolving);
    }

    #[test]
    fn active_elapsed_time_freezes_while_paused_and_ignores_the_outage() {
        let mut c = active(ContestConfig {
            reconnect_grace_ms: 60_000,
            ..cfg()
        }); // GO at 1000
        assert_eq!(c.active_elapsed_ms(1_500), 500);
        c.set_connected(B, false, 1_500);
        assert_eq!(c.active_elapsed_ms(9_000), 500, "frozen at the pause");
        c.set_connected(B, true, 9_000);
        assert_eq!(
            c.active_elapsed_ms(9_200),
            700,
            "the 7.5s outage is not play time"
        );
    }
}
