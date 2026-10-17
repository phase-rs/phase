//! Server-side policy for the online sustained-clicking contests (Eye to Eye,
//! Mouth to Mouth). The contest rules themselves live in the engine
//! (`engine::game::contest`); this module owns only what is a *host* concern: the
//! monotonic clock that stamps inputs, the per-seat click-rate guard, and the
//! deployment-tunable timing policy.

use std::collections::HashMap;
use std::time::Instant;

use engine::game::contest::{
    advance_contest, cancel_contest, contest_click, set_contest_connection, start_contest,
    ContestStep,
};
use engine::types::contest::{ClickDisposition, ContestCancelReason, ContestConfig};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::player::PlayerId;
use phase_ai::contest::AiContestants;

use crate::session::{AiDriverFault, AiTransition, DecisionSnapshot, GameSession};

/// Deployment-tunable contest settings. Defaults are chosen so ordinary fast
/// clicking is never throttled while scripted floods are bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContestPolicy {
    /// Contest timing handed to the engine state machine when a contest starts.
    pub config: ContestConfig,
    /// Sustained clicks per second a seat may submit. Fast human clicking tops out
    /// around 10-15/s; the default leaves generous headroom so a legitimate player
    /// is never throttled.
    pub max_clicks_per_second: u32,
    /// Instant burst a seat may submit (e.g. a touch device delivering a tap burst
    /// after a stall) before the sustained rate applies.
    pub click_burst: u32,
}

impl Default for ContestPolicy {
    fn default() -> Self {
        Self {
            config: ContestConfig::default(),
            max_clicks_per_second: 30,
            click_burst: 40,
        }
    }
}

/// Monotonic millisecond clock for one session — the single timebase every contest
/// input is stamped with. Never derived from a client-supplied value.
#[derive(Debug, Clone, Copy)]
pub struct ContestClock {
    epoch: Instant,
}

impl ContestClock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }

    /// Milliseconds since this clock was created.
    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn at(&self, observed_at: Instant) -> u64 {
        observed_at
            .saturating_duration_since(self.epoch)
            .as_millis() as u64
    }
}

impl Default for ContestClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Token-bucket click limiter. Dropping an over-limit click is harmless to the
/// sender: a click only ever signals activity, and a player submitting at the
/// limit is by definition active.
#[derive(Debug, Clone, PartialEq)]
pub struct ClickLimiter {
    capacity: f64,
    refill_per_ms: f64,
    tokens: f64,
    last_ms: u64,
}

impl ClickLimiter {
    pub fn new(policy: &ContestPolicy) -> Self {
        let capacity = f64::from(policy.click_burst.max(1));
        Self {
            capacity,
            refill_per_ms: f64::from(policy.max_clicks_per_second.max(1)) / 1_000.0,
            tokens: capacity,
            last_ms: 0,
        }
    }

    /// Whether a click arriving at `now_ms` is within the limit (consuming a token).
    pub fn allow(&mut self, now_ms: u64) -> bool {
        let elapsed = now_ms.saturating_sub(self.last_ms) as f64;
        self.last_ms = self.last_ms.max(now_ms);
        self.tokens = (self.tokens + elapsed * self.refill_per_ms).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Session integration
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub(crate) enum ContestPresence {
    Connected,
    Disconnected,
}

#[derive(Clone, Copy)]
struct ConnectionEdge {
    contest: u64,
    player: PlayerId,
    presence: ContestPresence,
    at_ms: u64,
}

/// Per-session contest runtime state. Not persisted: a restart cancels any
/// contest in flight (see `GameSession::cancel_contest_after_restore`) because the
/// monotonic clock does not survive the process.
#[derive(Default)]
pub struct ContestRuntime {
    pub policy: ContestPolicy,
    clock: ContestClock,
    limiters: HashMap<PlayerId, ClickLimiter>,
    ai: AiContestants,
    /// Transitions produced since the last drain, in order. Every path that
    /// mutates contest state (a click that settles it, a tick, a disconnect)
    /// pushes here; the driver task is the single publisher.
    outbox: Vec<AiTransition>,
    driver_running: bool,
    last_progress: Option<[(PlayerId, u32); 2]>,
    last_progress_ms: u64,
    connection_edges: Vec<ConnectionEdge>,
}

/// Lightweight live view of a running contest: each contestant's counted clicks.
/// Sent between full state transitions so both clients see the opponent's activity
/// without re-serializing the whole game 20 times a second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContestProgress {
    pub contest: u64,
    pub clicks: [(PlayerId, u32); 2],
}

/// What happened to a submitted click, for the transport's accounting only — a
/// click is never an error to the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContestClickReceipt {
    /// Counted toward the sender's activity.
    Counted,
    /// Dropped by the engine (not started, ended, duplicate, lapsed, ...).
    Ignored,
    /// Dropped by the per-seat rate guard.
    Throttled,
    /// The token does not name a seated player.
    UnknownSeat,
}

/// Minimum spacing between `ContestProgress` frames.
const PROGRESS_INTERVAL_MS: u64 = 100;

impl GameSession {
    /// Whether the game is parked on a running contest.
    pub fn contest_active(&self) -> bool {
        matches!(self.state.waiting_for, WaitingFor::PhysicalContest { .. })
    }

    /// Claim the right to run this session's contest driver task. Returns `true`
    /// exactly once per contest — the caller must then spawn the driver and call
    /// [`Self::release_contest_driver`] when it ends. Makes spawning idempotent no
    /// matter how many code paths notice the contest.
    pub fn claim_contest_driver(&mut self) -> bool {
        if self.contest_active() && !self.contest.driver_running {
            self.contest.driver_running = true;
            true
        } else {
            false
        }
    }

    pub fn release_contest_driver(&mut self) {
        self.contest.driver_running = false;
        self.contest.ai.clear();
        self.contest.limiters.clear();
        self.contest.last_progress = None;
    }

    /// Submit a click from the seat that owns `player_token`. The sender is
    /// resolved from the authenticated token and the instant from the server
    /// clock — nothing about timing or identity is taken from the payload.
    pub fn handle_contest_click(
        &mut self,
        player_token: &str,
        contest: u64,
        seq: u32,
    ) -> ContestClickReceipt {
        self.handle_contest_click_at(player_token, contest, seq, Instant::now())
    }

    /// Observe time when the authenticated host accepts the input, not after a
    /// CPU-admission or blocking-worker wait.
    pub fn handle_contest_click_at(
        &mut self,
        player_token: &str,
        contest: u64,
        seq: u32,
        observed_at: Instant,
    ) -> ContestClickReceipt {
        self.flush_contest_connections();
        let Some(player) = self.player_for_token(player_token) else {
            return ContestClickReceipt::UnknownSeat;
        };
        if !self.contest_active() {
            return ContestClickReceipt::Ignored;
        }
        let now = self.contest.clock.at(observed_at);
        let policy = self.contest.policy;
        let limiter = self
            .contest
            .limiters
            .entry(player)
            .or_insert_with(|| ClickLimiter::new(&policy));
        if !limiter.allow(now) {
            return ContestClickReceipt::Throttled;
        }
        self.submit_contest_click(contest, player, seq, now)
    }

    fn submit_contest_click(
        &mut self,
        contest: u64,
        player: PlayerId,
        seq: u32,
        now: u64,
    ) -> ContestClickReceipt {
        let mut events = Vec::new();
        let (disposition, step) =
            contest_click(&mut self.state, contest, player, seq, now, &mut events);
        self.publish_contest_step(step, events);
        match disposition {
            ClickDisposition::Counted => ContestClickReceipt::Counted,
            ClickDisposition::Rejected(_) => ContestClickReceipt::Ignored,
        }
    }

    /// Advance the contest to the server clock: start a pending countdown, let AI
    /// contestants click, and evaluate timeouts. Called by the driver task on a short
    /// interval.
    pub fn tick_contest(&mut self) {
        self.tick_contest_at(Instant::now());
    }

    /// A host tick's observation precedes its compute admission.
    pub fn tick_contest_at(&mut self, observed_at: Instant) {
        self.flush_contest_connections();
        if !self.contest_active() {
            self.contest.ai.clear();
            return;
        }
        let now = self.contest.clock.at(observed_at);
        self.sync_contest_connections(now);
        let mut events = Vec::new();
        let step = start_contest(
            &mut self.state,
            self.contest.policy.config,
            now,
            &mut events,
        );
        self.publish_contest_step(step, std::mem::take(&mut events));
        let step = advance_contest(&mut self.state, now, &mut events);
        self.publish_contest_step(step, std::mem::take(&mut events));
        self.drive_ai_contestants(now);
    }

    /// Seed the contest with the seats' live connection state. A contest starts with
    /// every contestant `Connected`, so a seat that was already away when the effect
    /// resolved (or whose disconnect landed before the driver's first tick) must be
    /// reported here, or its grace would never start and its absence would not pause
    /// the clock. Idempotent: repeating a seat's current state is a no-op.
    fn sync_contest_connections(&mut self, now: u64) {
        let WaitingFor::PhysicalContest { contest, .. } = &self.state.waiting_for else {
            return;
        };
        let away: Vec<PlayerId> = contest
            .players()
            .into_iter()
            .filter(|player| {
                !self.ai_seats.contains(player)
                    && !self
                        .connected
                        .get(player.0 as usize)
                        .copied()
                        .unwrap_or(true)
            })
            .collect();
        for player in away {
            let mut events = Vec::new();
            let step = set_contest_connection(&mut self.state, player, false, now, &mut events);
            self.publish_contest_step(step, events);
        }
    }

    /// AI seats click on the schedule `phase_ai::contest` plans for them.
    fn drive_ai_contestants(&mut self, now: u64) {
        let ai_players: Vec<PlayerId> = self.ai_seats.iter().copied().collect();
        let mut steps = Vec::new();
        self.contest.ai.click_due(
            &mut self.state,
            &ai_players,
            now,
            &mut rand::rng(),
            |step, events| steps.push((step, events)),
        );
        for (step, events) in steps {
            self.publish_contest_step(step, events);
        }
    }

    /// Presence bookkeeping may run outside compute workers. Retain each edge
    /// and its contest identity until the next admitted contest operation.
    pub(crate) fn queue_contest_connection(&mut self, player: PlayerId, presence: ContestPresence) {
        let WaitingFor::PhysicalContest { contest, .. } = &self.state.waiting_for else {
            return;
        };
        self.contest.connection_edges.push(ConnectionEdge {
            contest: contest.id,
            player,
            presence,
            at_ms: self.contest.clock.now_ms(),
        });
    }

    /// Apply recorded edges without re-stamping them after CPU queueing. The
    /// autonomous tick calls this too, so no further client traffic is needed.
    pub fn flush_contest_connections(&mut self) {
        let mut edges = std::mem::take(&mut self.contest.connection_edges);
        for edge in edges.drain(..) {
            if !matches!(&self.state.waiting_for,
                WaitingFor::PhysicalContest { contest, .. } if contest.id == edge.contest)
            {
                continue;
            }
            let mut events = Vec::new();
            let step = set_contest_connection(
                &mut self.state,
                edge.player,
                matches!(edge.presence, ContestPresence::Connected),
                edge.at_ms,
                &mut events,
            );
            self.publish_contest_step(step, events);
        }
        self.contest.connection_edges = edges;
    }

    /// Record a human seat attaching or detaching. The engine pauses the contest
    /// clock while any seat is away and forfeits the absentee past the grace period.
    pub fn note_contest_connection(&mut self, player: PlayerId, connected: bool) {
        self.flush_contest_connections();
        if !self.contest_active() {
            return;
        }
        let now = self.contest.clock.now_ms();
        let mut events = Vec::new();
        let step = set_contest_connection(&mut self.state, player, connected, now, &mut events);
        self.publish_contest_step(step, events);
    }

    /// A restored session cannot continue a contest: its clock epoch is gone.
    /// Cancel it so the game resumes instead of waiting on a timer that no longer
    /// means anything.
    pub fn cancel_contest_after_restore(&mut self) {
        if !self.contest_active() {
            return;
        }
        let now = self.contest.clock.now_ms();
        let mut events = Vec::new();
        self.contest.connection_edges.clear();
        let step = cancel_contest(
            &mut self.state,
            ContestCancelReason::HostAborted,
            now,
            &mut events,
        );
        self.publish_contest_step(step, events);
        // Nobody is attached to receive this transition; a reconnecting client is
        // sent the full state instead.
        self.contest.outbox.clear();
    }

    /// Turn a contest step into a broadcastable transition. `Unchanged` steps and
    /// steps outside a contest publish nothing.
    fn publish_contest_step(&mut self, step: ContestStep, events: Vec<GameEvent>) {
        if !matches!(step, ContestStep::Updated | ContestStep::Settled) {
            return;
        }
        let log_entries = engine::game::log::resolve_log_entries(&events, &self.state, &self.state);
        let revision = self.advance_state_revision();
        self.observe_transition(&events, &self.state.clone());
        self.game_log
            .write_game_log_entries(&self.game_code, &log_entries);
        self.contest.outbox.push(AiTransition {
            state_revision: revision,
            state: self.state.clone(),
            events,
            log_entries,
            decision: None,
        });
    }

    /// Take every transition produced since the last drain. After a contest
    /// settles, AI seats owed a decision act before the transitions are returned,
    /// so the broadcast order matches the order the states occurred in.
    pub fn drain_contest_outbox(&mut self) -> ContestDrain {
        match self.begin_contest_drain() {
            std::ops::ControlFlow::Break(drain) => drain,
            std::ops::ControlFlow::Continue((transitions, mut cursor)) => loop {
                match self.advance_ai_run(cursor) {
                    std::ops::ControlFlow::Continue(next) => cursor = next,
                    std::ops::ControlFlow::Break(outcome) => {
                        return self.finish_contest_drain(transitions, outcome);
                    }
                }
            },
        }
    }

    /// Capture host transitions without running an entire AI batch in one job.
    pub fn begin_contest_drain(
        &mut self,
    ) -> std::ops::ControlFlow<ContestDrain, (Vec<AiTransition>, crate::session::AiRunCursor)> {
        self.flush_contest_connections();
        let transitions = std::mem::take(&mut self.contest.outbox);
        if !self.contest_active() && !transitions.is_empty() {
            return match self.begin_ai_run() {
                std::ops::ControlFlow::Break(outcome) => {
                    std::ops::ControlFlow::Break(self.finish_contest_drain(transitions, outcome))
                }
                std::ops::ControlFlow::Continue(cursor) => {
                    std::ops::ControlFlow::Continue((transitions, cursor))
                }
            };
        }
        std::ops::ControlFlow::Break(self.finish_contest_drain(
            transitions,
            crate::session::AiRunOutcome {
                transitions: Vec::new(),
                failure: None,
                fault: None,
            },
        ))
    }

    /// Preserve the complete host/AI history and only its final decision set.
    pub fn finish_contest_drain(
        &self,
        mut transitions: Vec<AiTransition>,
        outcome: crate::session::AiRunOutcome,
    ) -> ContestDrain {
        transitions.extend(outcome.transitions);
        if let Some(last) = transitions.last_mut() {
            last.decision
                .get_or_insert_with(|| DecisionSnapshot::from_state(&last.state));
        }
        ContestDrain {
            transitions,
            ai_failure: outcome.fault,
        }
    }

    /// The live click counters, if they changed since the last call and the minimum
    /// spacing has elapsed.
    pub fn contest_progress(&mut self) -> Option<ContestProgress> {
        let WaitingFor::PhysicalContest { contest, .. } = &self.state.waiting_for else {
            return None;
        };
        let clicks = [
            (contest.contestants[0].player, contest.contestants[0].clicks),
            (contest.contestants[1].player, contest.contestants[1].clicks),
        ];
        let now = self.contest.clock.now_ms();
        if self.contest.last_progress == Some(clicks)
            || now.saturating_sub(self.contest.last_progress_ms) < PROGRESS_INTERVAL_MS
        {
            return None;
        }
        self.contest.last_progress = Some(clicks);
        self.contest.last_progress_ms = now;
        Some(ContestProgress {
            contest: contest.id,
            clicks,
        })
    }
}

/// Everything the driver task must publish after one step.
pub struct ContestDrain {
    pub transitions: Vec<AiTransition>,
    pub ai_failure: Option<AiDriverFault>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter() -> ClickLimiter {
        ClickLimiter::new(&ContestPolicy::default())
    }

    #[test]
    fn legitimate_fast_clicking_is_never_throttled() {
        // 15 clicks per second for a full minute: well above human maximum.
        let mut l = limiter();
        for i in 0..900u64 {
            assert!(l.allow(1_000 + i * 1_000 / 15), "click {i} throttled");
        }
    }

    #[test]
    fn an_instant_burst_up_to_capacity_passes_then_floods_are_cut() {
        let mut l = limiter();
        let admitted = (0..500).filter(|_| l.allow(1_000)).count();
        assert_eq!(admitted, 40, "burst capacity, nothing more at one instant");
    }

    #[test]
    fn a_sustained_flood_is_bounded_to_the_configured_rate() {
        let mut l = limiter();
        // 1000 clicks/s for 10s.
        let admitted = (0..10_000u64).filter(|i| l.allow(*i)).count();
        // 40 burst + 30/s * 10s = ~340.
        assert!((330..=350).contains(&admitted), "admitted {admitted}");
    }

    #[test]
    fn tokens_recover_after_a_pause() {
        let mut l = limiter();
        while l.allow(0) {}
        assert!(!l.allow(0));
        assert!(l.allow(1_000), "a second of quiet refills the bucket");
    }

    #[test]
    fn a_stale_timestamp_neither_panics_nor_refunds() {
        let mut l = limiter();
        while l.allow(5_000) {}
        assert!(!l.allow(1_000), "time never runs backwards");
    }

    // ---- Session integration -------------------------------------------------

    use crate::session::SessionManager;
    use engine::game::deck_loading::PlayerDeckPayload;
    use engine::types::contest::{ContestKind, ContestPhase, ContestState};
    use engine::types::phase::Phase;
    use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};

    const HOST: &str = "host-token";
    const GUEST: &str = "guest-token";

    /// A two-seat session parked on a Pending contest between seats 0 and 1.
    fn session_with_contest(policy: ContestPolicy) -> SessionManager {
        let mut mgr = SessionManager::new();
        let (code, _) = mgr.create_game(PlayerDeckPayload::default(), None);
        let session = mgr.session_exclusive(&code).expect("session exists");
        session.player_tokens = vec![HOST.to_string(), GUEST.to_string()];
        // Both seats are attached: the driver seeds the contest from this table.
        session.connected = vec![true, true];
        session.contest.policy = policy;
        session.state.waiting_for = WaitingFor::PhysicalContest {
            contest: Box::new(ContestState::new(
                1,
                ContestKind::Staring,
                PlayerId(0),
                PlayerId(1),
                ContestConfig::default(),
            )),
            source_id: engine::types::identifiers::ObjectId(1),
        };
        mgr
    }

    fn only_session(mgr: &mut SessionManager) -> &mut GameSession {
        let code = mgr.game_codes().next().expect("one game").clone();
        mgr.session_exclusive(&code).expect("session exists")
    }

    fn fast_policy() -> ContestPolicy {
        ContestPolicy {
            config: ContestConfig {
                countdown_ms: 1,
                inactivity_timeout_ms: 400,
                reconnect_grace_ms: 5_000,
                settle_window_ms: 0,
                max_active_ms: 60_000,
            },
            ..ContestPolicy::default()
        }
    }

    fn phase(session: &GameSession) -> ContestPhase {
        match &session.state.waiting_for {
            WaitingFor::PhysicalContest { contest, .. } => contest.phase,
            other => panic!("not in a contest: {other:?}"),
        }
    }

    #[test]
    fn a_tick_starts_the_countdown_and_publishes_the_transition() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        assert_eq!(phase(session), ContestPhase::Pending);

        session.tick_contest();

        assert_eq!(phase(session), ContestPhase::Preparing);
        let drain = session.drain_contest_outbox();
        assert_eq!(drain.transitions.len(), 1, "one transition for the start");
        assert!(matches!(
            drain.transitions[0].state.waiting_for,
            WaitingFor::PhysicalContest { .. }
        ));
        assert!(
            session.drain_contest_outbox().transitions.is_empty(),
            "drained once"
        );
    }

    #[test]
    fn draining_preserves_every_contest_snapshot_and_only_offers_the_final_decision() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        let before_revision = session.state_revision;
        let initial_life = session.state.players[0].life;
        let first_events = vec![GameEvent::DebugActionUsed {
            player_id: PlayerId(0),
            description: "first contest transition".to_string(),
        }];
        session.publish_contest_step(ContestStep::Updated, first_events.clone());
        session.state.players[0].life -= 1;
        session.state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let final_events = vec![GameEvent::DebugActionUsed {
            player_id: PlayerId(0),
            description: "contest settlement".to_string(),
        }];
        session.publish_contest_step(ContestStep::Settled, final_events.clone());
        let expected_logs: Vec<_> = session
            .contest
            .outbox
            .iter()
            .map(|transition| transition.log_entries.clone())
            .collect();
        assert!(expected_logs.iter().all(|entries| !entries.is_empty()));
        assert!(session
            .contest
            .outbox
            .iter()
            .all(|entry| entry.decision.is_none()));

        let drain = session.drain_contest_outbox();

        assert!(drain.ai_failure.is_none());
        assert_eq!(drain.transitions.len(), 2);
        let first = &drain.transitions[0];
        let last = &drain.transitions[1];
        assert_eq!(first.state_revision, before_revision + 1);
        assert_eq!(last.state_revision, before_revision + 2);
        assert_eq!(first.state.players[0].life, initial_life);
        assert_eq!(last.state.players[0].life, initial_life - 1);
        assert!(matches!(
            first.state.waiting_for,
            WaitingFor::PhysicalContest { .. }
        ));
        assert_eq!(last.state.waiting_for, session.state.waiting_for);
        assert_eq!(first.events, first_events);
        assert_eq!(last.events, final_events);
        assert_eq!(first.log_entries, expected_logs[0]);
        assert_eq!(last.log_entries, expected_logs[1]);
        assert!(first.decision.is_none());
        let decision = last
            .decision
            .as_ref()
            .expect("final snapshot offers a decision");
        let expected = DecisionSnapshot::from_state(&last.state);
        assert_eq!(decision.legal_actions, expected.legal_actions);
        assert_eq!(
            decision.auto_pass_recommended,
            expected.auto_pass_recommended
        );
        assert_eq!(decision.spell_costs, expected.spell_costs);
        assert_eq!(
            decision.legal_actions_by_object,
            expected.legal_actions_by_object
        );
        assert!(session.drain_contest_outbox().transitions.is_empty());
    }

    #[test]
    fn contest_settlement_keeps_ai_followup_history_and_its_final_decision() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        let ai_player = PlayerId(0);
        session.ai_seats.insert(ai_player);
        session.ai_configs.insert(
            ai_player,
            create_config_for_players(AiDifficulty::Easy, Platform::Native, 2),
        );
        session.state.phase = Phase::PreCombatMain;
        let mut events = Vec::new();
        let step = cancel_contest(
            &mut session.state,
            ContestCancelReason::HostAborted,
            0,
            &mut events,
        );
        assert_eq!(step, ContestStep::Settled);
        assert!(
            matches!(session.state.waiting_for, WaitingFor::Priority { player } if player == ai_player)
        );
        session.publish_contest_step(step, events.clone());
        let settlement = session.contest.outbox[0].clone();
        assert!(!settlement.log_entries.is_empty());

        let drain = session.drain_contest_outbox();

        assert!(drain.ai_failure.is_none(), "{:?}", drain.ai_failure);
        assert!(drain.transitions.len() >= 2, "AI acted after settlement");
        let first = &drain.transitions[0];
        assert_eq!(first.state_revision, settlement.state_revision);
        assert_eq!(first.state.waiting_for, settlement.state.waiting_for);
        assert_eq!(first.events, events);
        assert_eq!(first.log_entries, settlement.log_entries);
        let (last, intermediate) = drain.transitions.split_last().expect("nonempty drain");
        assert!(intermediate.iter().all(|entry| entry.decision.is_none()));
        for (index, transition) in drain.transitions.iter().enumerate() {
            assert_eq!(
                transition.state_revision,
                settlement.state_revision + index as u64
            );
        }
        assert_eq!(last.state.waiting_for, session.state.waiting_for);
        let decision = last
            .decision
            .as_ref()
            .expect("AI's final decision survives");
        let expected = DecisionSnapshot::from_state(&last.state);
        assert_eq!(decision.legal_actions, expected.legal_actions);
        assert_eq!(
            decision.auto_pass_recommended,
            expected.auto_pass_recommended
        );
        assert_eq!(decision.spell_costs, expected.spell_costs);
        assert_eq!(
            decision.legal_actions_by_object,
            expected.legal_actions_by_object
        );
    }

    #[test]
    fn clicks_resolve_the_sender_from_the_token_and_never_before_go() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        assert_eq!(
            session.handle_contest_click("nobody", 1, 1),
            ContestClickReceipt::UnknownSeat
        );
        // Pending: not started, so not counted.
        assert_eq!(
            session.handle_contest_click(HOST, 1, 1),
            ContestClickReceipt::Ignored
        );

        session.tick_contest();
        std::thread::sleep(std::time::Duration::from_millis(10));
        session.tick_contest();
        assert_eq!(phase(session), ContestPhase::Active);

        assert_eq!(
            session.handle_contest_click(HOST, 1, 1),
            ContestClickReceipt::Counted
        );
        assert_eq!(
            session.handle_contest_click(HOST, 1, 1),
            ContestClickReceipt::Ignored,
            "a duplicate delivery is not counted twice"
        );
        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("contest ended early");
        };
        assert_eq!(contest.contestants[0].clicks, 1);
        assert_eq!(contest.contestants[1].clicks, 0);
    }

    #[test]
    fn a_flood_beyond_the_rate_limit_is_throttled_not_fatal() {
        let policy = ContestPolicy {
            max_clicks_per_second: 5,
            click_burst: 5,
            ..fast_policy()
        };
        let mut mgr = session_with_contest(policy);
        let session = only_session(&mut mgr);
        session.tick_contest();
        std::thread::sleep(std::time::Duration::from_millis(10));
        session.tick_contest();

        let receipts: Vec<_> = (1..=50)
            .map(|seq| session.handle_contest_click(HOST, 1, seq))
            .collect();
        assert!(receipts.contains(&ContestClickReceipt::Throttled));
        assert!(
            receipts
                .iter()
                .filter(|r| **r == ContestClickReceipt::Counted)
                .count()
                <= 8
        );
        assert!(session.contest_active(), "a flood never ends the contest");
    }

    #[test]
    fn a_seat_dropping_pauses_the_contest_and_returning_resumes_it() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        session.tick_contest();
        std::thread::sleep(std::time::Duration::from_millis(10));
        session.tick_contest();
        session.drain_contest_outbox();

        session.mark_disconnected(PlayerId(1));
        session.flush_contest_connections();
        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("contest ended");
        };
        assert!(
            contest.paused_since_ms.is_some(),
            "the clock stops for everyone"
        );
        assert_eq!(session.drain_contest_outbox().transitions.len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(450));
        session.tick_contest();
        assert!(
            session.contest_active(),
            "paused: no one lapses while a seat is away"
        );

        session.mark_connected(PlayerId(1));
        session.flush_contest_connections();
        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("contest ended");
        };
        assert!(contest.paused_since_ms.is_none());
    }

    #[test]
    fn a_recorded_disconnect_pauses_and_forfeits_without_more_client_input() {
        use engine::types::contest::{ContestLossReason, ContestOutcome, ContestResult};
        use std::time::Duration;

        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        let observed = Instant::now();
        session.contest.clock.epoch = observed - Duration::from_secs(1);
        session.tick_contest_at(observed - Duration::from_millis(100));
        session.tick_contest_at(observed - Duration::from_millis(90));
        assert_eq!(phase(session), ContestPhase::Active);
        session.drain_contest_outbox();

        session.mark_disconnected(PlayerId(1));
        // Fix the host input stamp; only the admitted worker's observation moves.
        let disconnected_at = 1_000;
        session.contest.connection_edges[0].at_ms = disconnected_at;
        // Advance the test host clock without changing the already-recorded edge.
        session.contest.clock.epoch -= Duration::from_secs(1);
        session.tick_contest_at(
            session.contest.clock.epoch + Duration::from_millis(disconnected_at + 1_000),
        );
        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("the five-second grace has not elapsed");
        };
        assert_eq!(contest.paused_since_ms, Some(disconnected_at));
        assert_eq!(session.drain_contest_outbox().transitions.len(), 1);

        let expired_at = session.contest.clock.epoch
            + Duration::from_millis(
                disconnected_at + session.contest.policy.config.reconnect_grace_ms + 1,
            );
        session.tick_contest_at(expired_at);
        let drain = session.drain_contest_outbox();
        assert!(matches!(
            session.state.waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(drain
            .transitions
            .iter()
            .flat_map(|transition| &transition.events)
            .any(|event| {
                matches!(
                    event,
                    GameEvent::PhysicalContestResolved {
                        result: ContestResult::Outcome(ContestOutcome::Decided {
                            winner: PlayerId(0),
                            loser: PlayerId(1),
                            reason: ContestLossReason::Forfeit,
                        }),
                        ..
                    }
                )
            }));
    }

    #[test]
    fn delayed_tick_and_click_workers_use_the_hosts_observation_time() {
        use std::time::Duration;

        for click in [None, Some(1)] {
            let mut mgr = session_with_contest(fast_policy());
            let session = only_session(&mut mgr);
            let worker_time = Instant::now();
            session.contest.clock.epoch = worker_time - Duration::from_secs(1);
            session.tick_contest_at(worker_time - Duration::from_millis(600));
            session.tick_contest_at(worker_time - Duration::from_millis(590));
            assert_eq!(phase(session), ContestPhase::Active);
            let observed = worker_time - Duration::from_millis(250);

            if let Some(seq) = click {
                assert_eq!(
                    session.handle_contest_click_at(HOST, 1, seq, observed),
                    ContestClickReceipt::Counted,
                    "the host received the click before either inactivity deadline",
                );
            } else {
                session.tick_contest_at(observed);
            }
            assert_eq!(
                phase(session),
                ContestPhase::Active,
                "worker delay must not adjudicate an on-time observation as a late one",
            );
        }
    }

    #[test]
    fn a_queued_presence_edge_cannot_pause_a_replacement_contest() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        session.mark_disconnected(PlayerId(1));
        let WaitingFor::PhysicalContest { contest, .. } = &mut session.state.waiting_for else {
            panic!("the fixture has a contest");
        };
        contest.id += 1;
        session.flush_contest_connections();
        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("a stale presence edge must not settle a replacement contest");
        };
        assert!(contest.paused_since_ms.is_none());
        assert!(session.drain_contest_outbox().transitions.is_empty());
    }

    /// A frame delayed from an earlier contest names that contest's identity; it is
    /// dropped before it can count as activity — or consume the running contest's
    /// sequence numbers — in the one that is Active now.
    #[test]
    fn a_click_addressed_to_another_contest_is_ignored() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        session.tick_contest();
        std::thread::sleep(std::time::Duration::from_millis(10));
        session.tick_contest();
        assert_eq!(phase(session), ContestPhase::Active);

        assert_eq!(
            session.handle_contest_click(HOST, 99, 1),
            ContestClickReceipt::Ignored,
            "a prior contest's frame is not activity in this one"
        );
        assert_eq!(
            session.handle_contest_click(HOST, 1, 1),
            ContestClickReceipt::Counted,
            "and it did not burn the sequence number the real click needs"
        );
    }

    /// A seat already away when the contest starts must pause it and start its
    /// reconnect grace — the engine begins every contestant `Connected`.
    #[test]
    fn a_seat_already_away_at_start_pauses_the_contest() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        session.connected[1] = false;

        session.tick_contest();

        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("contest ended");
        };
        assert!(
            contest.paused_since_ms.is_some(),
            "the absent seat pauses the clock from the first tick"
        );
        assert!(matches!(
            contest.contestants[1].connection,
            engine::types::contest::ContestantConnection::Disconnected { .. }
        ));
    }

    /// A seat that stays away past the reconnect grace forfeits the CONTEST — the
    /// game itself carries on — and the grace is measured from the moment the host
    /// first saw it away, even when that was before the contest started.
    #[test]
    fn a_seat_away_past_grace_forfeits_the_contest_but_not_the_game() {
        let policy = ContestPolicy {
            config: ContestConfig {
                reconnect_grace_ms: 30,
                ..fast_policy().config
            },
            ..fast_policy()
        };
        let mut mgr = session_with_contest(policy);
        let session = only_session(&mut mgr);
        session.connected[1] = false;

        session.tick_contest();
        assert!(
            session.contest_active(),
            "within grace the contest only waits"
        );
        std::thread::sleep(std::time::Duration::from_millis(60));
        session.tick_contest();

        assert!(
            !session.contest_active(),
            "the absentee forfeited the contest"
        );
        assert!(
            matches!(session.state.waiting_for, WaitingFor::Priority { .. }),
            "the game continues: {:?}",
            session.state.waiting_for
        );
    }

    /// Returning within the grace resumes the contest and nobody forfeits.
    #[test]
    fn a_seat_returning_within_grace_resumes_without_forfeiting() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        session.connected[1] = false;
        session.tick_contest();

        session.mark_connected(PlayerId(1));
        session.tick_contest();

        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("the contest ended although the seat returned in time");
        };
        assert!(contest.paused_since_ms.is_none(), "resumed");
        assert_eq!(
            contest.contestants[1].connection,
            engine::types::contest::ContestantConnection::Connected
        );
    }

    /// Ordinary actions are frozen while a takeback vote is open, so a vote opened
    /// mid-contest would let the real-time contest keep mutating under it.
    #[test]
    fn a_takeback_cannot_be_requested_during_a_contest() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);

        let result =
            session.request_takeback(PlayerId(0), crate::takeback::RewindTarget::LastAction);

        // The message pins the REASON: with no history the request would also fail
        // ("no previous action"), so a bare `is_err` would pass vacuously.
        let error = result.expect_err("the request is refused");
        assert!(
            error.contains("contest"),
            "refused for the contest: {error}"
        );
        assert!(
            session.pending_takeback.is_none(),
            "and no vote was installed"
        );
    }

    #[test]
    fn an_ai_seat_clicks_for_itself_and_the_driver_claim_is_handed_out_once() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        session.ai_seats.insert(PlayerId(1));

        assert!(session.claim_contest_driver());
        assert!(!session.claim_contest_driver(), "one driver per contest");

        session.tick_contest();
        std::thread::sleep(std::time::Duration::from_millis(10));
        session.tick_contest();

        let WaitingFor::PhysicalContest { contest, .. } = &session.state.waiting_for else {
            panic!("contest ended");
        };
        assert!(contest.contestants[1].clicks >= 1, "the AI clicked at GO");
        assert_eq!(
            contest.contestants[0].clicks, 0,
            "the human seat is untouched"
        );

        session.release_contest_driver();
        assert!(
            session.claim_contest_driver(),
            "released claims can be retaken"
        );
    }

    #[test]
    fn a_restored_session_cancels_the_contest_instead_of_waiting_on_a_dead_clock() {
        let mut mgr = session_with_contest(fast_policy());
        let session = only_session(&mut mgr);
        session.tick_contest();

        session.cancel_contest_after_restore();

        assert!(!session.contest_active());
        assert!(matches!(
            session.state.waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            session.drain_contest_outbox().transitions.is_empty(),
            "nobody is attached to receive it"
        );
    }

    #[test]
    fn the_clock_is_monotonic() {
        let c = ContestClock::new();
        let a = c.now_ms();
        let b = c.now_ms();
        assert!(b >= a);
    }
}
