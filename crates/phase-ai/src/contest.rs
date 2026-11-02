//! AI participation in the online sustained-clicking contests (Eye to Eye,
//! Mouth to Mouth — see `engine::types::contest`).
//!
//! An AI seat never receives a `GameAction` while the game is parked on
//! `WaitingFor::PhysicalContest`; the host drives the contest. This module is the
//! pure policy a host consults to decide *when the AI clicks*. It keeps a steady,
//! human-ish cadence and runs out of stamina after a random stretch, so the AI
//! can both win and lose. It reads no clock — the host passes the same
//! millisecond timebase it stamps human clicks with.

use std::collections::{HashMap, HashSet};

use engine::game::contest::{
    advance_contest, cancel_contest, contest_click, start_contest, ContestStep,
};
use engine::types::contest::{ContestCancelReason, ContestPhase};
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use rand::Rng;

/// Fastest and slowest AI click cadence, in milliseconds between clicks.
const MIN_INTERVAL_MS: u64 = 220;
const MAX_INTERVAL_MS: u64 = 380;
/// How long past GO the AI keeps clicking before it gives up.
const MIN_STAMINA_MS: u64 = 2_500;
const MAX_STAMINA_MS: u64 = 9_000;

/// One AI contestant's click schedule for a single contest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContestClicker {
    interval_ms: u64,
    quit_at_ms: u64,
    next_click_ms: u64,
    seq: u32,
}

impl ContestClicker {
    /// Plan a schedule for a contest whose GO instant is `go_at_ms`. The cadence is
    /// capped at half the inactivity timeout so a configured short timeout can never
    /// make the AI lose by merely clicking at its natural rate.
    pub fn new(go_at_ms: u64, inactivity_timeout_ms: u64, rng: &mut impl Rng) -> Self {
        let max_interval = MAX_INTERVAL_MS.min((inactivity_timeout_ms / 2).max(1));
        let min_interval = MIN_INTERVAL_MS.min(max_interval);
        Self {
            interval_ms: rng.random_range(min_interval..=max_interval),
            quit_at_ms: go_at_ms + rng.random_range(MIN_STAMINA_MS..=MAX_STAMINA_MS),
            next_click_ms: go_at_ms,
            seq: 0,
        }
    }

    /// The next click's sequence number if one is due at `now_ms`. At most one
    /// click per poll, so a late tick never produces a burst.
    pub fn poll(&mut self, now_ms: u64) -> Option<u32> {
        if now_ms >= self.quit_at_ms || now_ms < self.next_click_ms {
            return None;
        }
        self.next_click_ms = now_ms + self.interval_ms;
        self.seq += 1;
        Some(self.seq)
    }

    /// The instant the AI stops clicking.
    pub fn quit_at_ms(&self) -> u64 {
        self.quit_at_ms
    }
}

/// The AI seats' clickers for the contest currently running in one game. Shared by
/// every host that can seat an AI (phase-server sessions, the WASM engine) so they
/// all click on the same schedule.
#[derive(Debug, Default)]
pub struct AiContestants {
    /// The contest the schedules below were planned for.
    contest_id: Option<u64>,
    clickers: HashMap<PlayerId, ContestClicker>,
}

impl AiContestants {
    /// Forget every schedule — call when no contest is running, so the next contest
    /// plans fresh ones.
    pub fn clear(&mut self) {
        self.contest_id = None;
        self.clickers.clear();
    }

    /// Submit every AI click due at `now_ms` for the running contest. Each
    /// resulting engine step is reported through `on_step` together with the events
    /// it emitted, so the host can publish it exactly as it publishes a human click.
    /// A no-op outside an Active contest.
    pub fn click_due(
        &mut self,
        state: &mut GameState,
        ai_players: &[PlayerId],
        now_ms: u64,
        rng: &mut impl Rng,
        mut on_step: impl FnMut(ContestStep, Vec<GameEvent>),
    ) {
        let WaitingFor::PhysicalContest { contest, .. } = &state.waiting_for else {
            self.clear();
            return;
        };
        // A schedule planned for an earlier contest must never carry over.
        if self.contest_id != Some(contest.id) {
            self.clear();
            self.contest_id = Some(contest.id);
        }
        // No AI click while the clock is paused for an absent seat: the engine
        // freezes everyone's budget, and the AI's own stamina must not run down
        // (or its cadence advance) during an outage either.
        if contest.phase != ContestPhase::Active || contest.paused_since_ms.is_some() {
            return;
        }
        let contest_id = contest.id;
        // The schedule runs on the contest's own unpaused timebase, the same one the
        // engine shifts its deadlines and hard cap by, starting at GO.
        let elapsed = contest.active_elapsed_ms(now_ms);
        let timeout = contest.config.inactivity_timeout_ms;
        let participating: Vec<PlayerId> = contest
            .players()
            .into_iter()
            .filter(|player| ai_players.contains(player))
            .collect();
        for player in participating {
            let clicker = self
                .clickers
                .entry(player)
                .or_insert_with(|| ContestClicker::new(0, timeout, rng));
            if let Some(seq) = clicker.poll(elapsed) {
                let mut events = Vec::new();
                let (_, step) = contest_click(state, contest_id, player, seq, now_ms, &mut events);
                on_step(step, events);
            }
        }
    }
}

/// Virtual time between engine ticks when a headless host plays a contest out.
const HEADLESS_TICK_MS: u64 = 50;

/// Play a contest between AI seats to its conclusion on a deterministic virtual clock,
/// for hosts with no clock or input of their own (self-play, benchmarks, gates).
///
/// A live host drives the contest in real time (`phase-server`, `engine-wasm`); a
/// headless one would otherwise stop at the wait, because no seat acts on it. Returns
/// whether it ran: `false` when the game is not in a contest or a contestant is not an
/// AI seat (a human's contest is not this host's to play). The contest is bounded by
/// its own hard cap, with a final cancellation as a backstop, so it always leaves the
/// wait.
pub fn settle_headless_contest(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    rng: &mut impl Rng,
) -> bool {
    let WaitingFor::PhysicalContest { contest, .. } = &state.waiting_for else {
        return false;
    };
    if !contest.players().iter().all(|p| ai_players.contains(p)) {
        return false;
    }
    let config = contest.config;
    let seats: Vec<PlayerId> = contest.players().to_vec();
    let budget_ms = config.countdown_ms
        + config.max_active_ms
        + config.settle_window_ms
        + config.inactivity_timeout_ms;
    let mut clickers = AiContestants::default();
    let mut events = Vec::new();
    let mut now = 0;
    start_contest(state, config, now, &mut events);
    while matches!(state.waiting_for, WaitingFor::PhysicalContest { .. }) {
        if now > budget_ms {
            cancel_contest(state, ContestCancelReason::HostAborted, now, &mut events);
            break;
        }
        now += HEADLESS_TICK_MS;
        advance_contest(state, now, &mut events);
        clickers.click_due(state, &seats, now, rng, |_, _| {});
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn clicker(timeout: u64) -> ContestClicker {
        ContestClicker::new(1_000, timeout, &mut StdRng::seed_from_u64(7))
    }

    #[test]
    fn clicks_steadily_from_go_and_never_leaves_the_inactivity_window() {
        let mut c = clicker(2_000);
        let mut last = None;
        let mut clicks = 0;
        for now in (1_000..c.quit_at_ms()).step_by(10) {
            if c.poll(now).is_some() {
                if let Some(prev) = last {
                    assert!(now - prev < 2_000, "gap {} exceeds the timeout", now - prev);
                }
                last = Some(now);
                clicks += 1;
            }
        }
        assert!(clicks >= 5, "an AI that clicks too rarely is not a contest");
    }

    #[test]
    fn stops_after_its_stamina_runs_out() {
        let mut c = clicker(2_000);
        let quit = c.quit_at_ms();
        assert!((1_000 + MIN_STAMINA_MS..=1_000 + MAX_STAMINA_MS).contains(&quit));
        assert_eq!(c.poll(quit), None);
        assert_eq!(c.poll(quit + 5_000), None);
    }

    #[test]
    fn does_not_click_before_go_and_never_bursts() {
        let mut c = clicker(2_000);
        assert_eq!(c.poll(999), None);
        assert_eq!(c.poll(1_000), Some(1));
        assert_eq!(c.poll(1_001), None, "one click per interval");
    }

    #[test]
    fn a_short_timeout_caps_the_cadence() {
        let c = clicker(300);
        assert!(c.interval_ms <= 150);
    }

    #[test]
    fn sequence_numbers_strictly_increase() {
        let mut c = clicker(2_000);
        let mut prev = 0;
        for now in (1_000..c.quit_at_ms()).step_by(5) {
            if let Some(seq) = c.poll(now) {
                assert!(seq > prev);
                prev = seq;
            }
        }
    }

    // ---- Host integration (`AiContestants::click_due`) ------------------------

    use engine::types::contest::{ContestConfig, ContestKind, ContestState};
    use engine::types::identifiers::ObjectId;

    const AI: PlayerId = PlayerId(1);

    fn contest_state(id: u64) -> GameState {
        let config = ContestConfig {
            reconnect_grace_ms: 600_000,
            ..ContestConfig::default()
        };
        let mut contest = ContestState::new(id, ContestKind::Staring, PlayerId(0), AI, config);
        assert!(contest.start(0));
        contest.advance(config.countdown_ms); // Active; GO at 3000
        let mut state = GameState::new_two_player(1);
        state.waiting_for = WaitingFor::PhysicalContest {
            contest: Box::new(contest),
            source_id: ObjectId(1),
        };
        state
    }

    fn contest_mut(state: &mut GameState) -> &mut ContestState {
        let WaitingFor::PhysicalContest { contest, .. } = &mut state.waiting_for else {
            panic!("not in a contest");
        };
        contest
    }

    fn clicks_at(ai: &mut AiContestants, state: &mut GameState, now: u64) -> usize {
        let mut count = 0;
        ai.click_due(state, &[AI], now, &mut StdRng::seed_from_u64(11), |_, _| {
            count += 1
        });
        count
    }

    /// While a seat is away the engine freezes the contest clock; the AI must not
    /// click through the outage, and the outage must not spend its stamina — it runs
    /// on the contest's unpaused timebase.
    #[test]
    fn the_ai_neither_clicks_nor_tires_while_the_contest_is_paused() {
        let mut state = contest_state(1);
        let mut ai = AiContestants::default();
        assert_eq!(clicks_at(&mut ai, &mut state, 3_000), 1, "clicks at GO");

        contest_mut(&mut state).set_connected(PlayerId(0), false, 3_500);
        assert_eq!(
            clicks_at(&mut ai, &mut state, 20_000),
            0,
            "silent while paused"
        );

        // A minute-long outage later, the AI is 0.5s into the contest, not a minute in.
        contest_mut(&mut state).set_connected(PlayerId(0), true, 63_500);
        assert_eq!(
            clicks_at(&mut ai, &mut state, 63_500),
            1,
            "an outage is not stamina: the AI is still in its first seconds"
        );
    }

    /// A schedule planned for one contest never carries into the next, even when the
    /// host never observed a gap between them.
    #[test]
    fn a_new_contest_discards_the_previous_ones_schedule() {
        let mut ai = AiContestants::default();
        let mut state = contest_state(1);
        assert_eq!(clicks_at(&mut ai, &mut state, 3_000), 1);
        assert_eq!(
            clicks_at(&mut ai, &mut state, 3_001),
            0,
            "inside its interval"
        );

        let mut next = contest_state(2);
        assert_eq!(
            clicks_at(&mut ai, &mut next, 3_001),
            1,
            "a fresh contest plans a fresh schedule and clicks at its own GO"
        );
    }

    // ---- Headless hosting (`settle_headless_contest`) -------------------------

    use std::sync::Arc;

    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::actions::GameAction;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    use crate::auto_play::{
        run_ai_actions, run_ai_actions_bounded_headless, run_driver_loop, AiActionsStop, DriverExit,
    };
    use crate::config::{create_config, AiDifficulty, Platform};
    use crate::session::AiSession;

    const EYE_TO_EYE: &str = "You and target creature's controller have a staring contest. If you win, destroy that creature.";

    /// A game parked on a contest that a REAL Eye to Eye cast produced.
    fn cast_into_contest() -> (GameState, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario.add_vanilla(P1, 2, 2);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
            .id();
        let mut runner = scenario.build();
        runner.cast(spell).target_objects(&[victim]).resolve();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::PhysicalContest { .. }
            ),
            "reach guard: the cast must have parked the game on the contest"
        );
        (runner.state().clone(), victim)
    }

    /// A game where Eye to Eye is on the stack and P1 has priority to pass and resolve it.
    fn cast_on_stack_awaiting_resolution() -> (GameState, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario.add_vanilla(P1, 2, 2);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
            .id();
        let mut runner = scenario.build();
        let _commit = runner.cast(spell).target_objects(&[victim]).commit();
        runner.act(GameAction::PassPriority).unwrap();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player } if player == P1
            ),
            "reach guard: P1 must have priority with the spell on the stack"
        );
        assert_eq!(runner.state().stack.len(), 1, "spell must be on the stack");
        (runner.state().clone(), victim)
    }

    fn configs() -> HashMap<PlayerId, crate::config::AiConfig> {
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        HashMap::from([(P0, config.clone()), (P1, config)])
    }

    fn seats(players: &[PlayerId]) -> HashSet<PlayerId> {
        players.iter().copied().collect()
    }

    /// The batch every live host runs stops at the contest: no seat acts there.
    #[test]
    fn a_live_host_batch_stops_at_the_contest() {
        let (mut state, _) = cast_into_contest();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let run = run_ai_actions(
            &mut state,
            &seats(&[P0, P1]),
            &configs(),
            &mut StdRng::seed_from_u64(3),
            &session,
        );

        assert!(run.is_empty());
        assert!(matches!(run.stop, AiActionsStop::NoEligibleAiActor));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::PhysicalContest { .. }
        ));
    }

    /// The headless batch plays the contest out and the AI carries on in the same batch.
    #[test]
    fn a_headless_batch_plays_a_real_contest_out_and_resumes_play() {
        let (mut state, victim) = cast_into_contest();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let run = run_ai_actions_bounded_headless(
            &mut state,
            &seats(&[P0, P1]),
            &configs(),
            &mut StdRng::seed_from_u64(3),
            &session,
            4,
        );

        assert!(
            !matches!(state.waiting_for, WaitingFor::PhysicalContest { .. }),
            "the contest must have settled"
        );
        assert!(!run.is_empty(), "the AI must have acted after the contest");
        // The result was applied exactly once: the victim is either destroyed or kept.
        let zone = state.objects[&victim].zone;
        assert!(zone == Zone::Graveyard || zone == Zone::Battlefield);
    }

    /// Host settlement is not an AI action: a batch budgeted to one action settles the
    /// contest AND applies the resumed action, instead of spending its only unit on the
    /// settlement and reporting `ActionBudgetReached` with nothing applied (which a
    /// duel driver reads as a stall).
    #[test]
    fn a_budget_of_one_settles_the_contest_and_still_applies_an_action() {
        let (mut state, _) = cast_into_contest();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let run = run_ai_actions_bounded_headless(
            &mut state,
            &seats(&[P0, P1]),
            &configs(),
            &mut StdRng::seed_from_u64(3),
            &session,
            1,
        );

        assert!(
            !matches!(state.waiting_for, WaitingFor::PhysicalContest { .. }),
            "the contest must have settled"
        );
        assert_eq!(run.len(), 1, "the one budgeted action must be applied");
        assert!(matches!(
            run.stop,
            AiActionsStop::ActionBudgetReached { limit: 1 }
        ));
    }

    /// The driver sees the progress: at cap 1 it counts the resumed action and reaches
    /// its cap, rather than breaking out on a zero-action batch.
    #[test]
    fn a_driver_capped_at_one_action_counts_the_resumed_action_after_a_contest() {
        let (mut state, _) = cast_into_contest();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let outcome = run_driver_loop(
            &mut state,
            &seats(&[P0, P1]),
            &configs(),
            &mut StdRng::seed_from_u64(3),
            &session,
            1,
            &mut |_, _, _| {},
        );

        assert_eq!(outcome.total_actions, 1);
        assert!(matches!(outcome.exit, DriverExit::CapReached));
    }

    /// Deterministic: the same seed settles the same way.
    #[test]
    fn a_headless_contest_is_deterministic_per_seed() {
        let outcome = |seed: u64| {
            let (mut state, victim) = cast_into_contest();
            let session: Arc<AiSession> = AiSession::arc_from_game(&state);
            run_ai_actions_bounded_headless(
                &mut state,
                &seats(&[P0, P1]),
                &configs(),
                &mut StdRng::seed_from_u64(seed),
                &session,
                1,
            );
            state.objects[&victim].zone
        };
        assert_eq!(outcome(5), outcome(5));
        // Reach guard: across seeds both results occur, so equality above is not vacuous.
        let zones: HashSet<Zone> = (0..40).map(outcome).collect();
        assert!(zones.len() > 1, "the contest must be able to go either way");
    }

    /// A contest with a human contestant is that human's to play, not the headless host's.
    #[test]
    fn a_headless_batch_leaves_a_humans_contest_alone() {
        let (mut state, _) = cast_into_contest();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let run = run_ai_actions_bounded_headless(
            &mut state,
            &seats(&[P1]),
            &configs(),
            &mut StdRng::seed_from_u64(3),
            &session,
            4,
        );

        assert!(run.is_empty());
        assert!(matches!(run.stop, AiActionsStop::NoEligibleAiActor));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::PhysicalContest { .. }
        ));
    }

    /// When the final budgeted AI action creates a contest, host settlement runs and
    /// the batch reports `ActionBudgetReached` rather than terminal `NoEligibleAiActor`.
    #[test]
    fn a_contest_created_by_the_final_budgeted_action_settles_and_continues_batch() {
        let (mut state, victim) = cast_on_stack_awaiting_resolution();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let run = run_ai_actions_bounded_headless(
            &mut state,
            &seats(&[P0, P1]),
            &configs(),
            &mut StdRng::seed_from_u64(3),
            &session,
            1,
        );

        assert_eq!(run.len(), 1, "P1's resolving pass must be recorded");
        assert!(
            matches!(run.stop, AiActionsStop::ActionBudgetReached { limit: 1 }),
            "expected ActionBudgetReached, got {:?}",
            run.stop
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::PhysicalContest { .. }),
            "the contest must have settled"
        );
        let zone = state.objects[&victim].zone;
        assert!(zone == Zone::Graveyard || zone == Zone::Battlefield);
    }

    /// A duel capped at two actions takes the resolving pass, settles the resulting contest,
    /// and proceeds to take its second action rather than breaking out on `NoEligibleAiActor`.
    #[test]
    fn a_contest_created_by_the_final_budgeted_action_continues_the_duel() {
        let (mut state, _) = cast_on_stack_awaiting_resolution();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let outcome = run_driver_loop(
            &mut state,
            &seats(&[P0, P1]),
            &configs(),
            &mut StdRng::seed_from_u64(3),
            &session,
            2,
            &mut |_, _, _| {},
        );

        assert_eq!(outcome.total_actions, 2);
        assert!(matches!(outcome.exit, DriverExit::CapReached));
    }

    /// When the final budgeted action creates a contest involving a human player,
    /// host settlement does not run and the batch hands off to the human via `NoEligibleAiActor`.
    #[test]
    fn a_contest_created_by_the_final_budgeted_action_hands_off_to_a_human() {
        let (mut state, _) = cast_on_stack_awaiting_resolution();
        let session: Arc<AiSession> = AiSession::arc_from_game(&state);

        let mut p1_configs = configs();
        p1_configs.remove(&P0);
        let run = run_ai_actions_bounded_headless(
            &mut state,
            &seats(&[P1]),
            &p1_configs,
            &mut StdRng::seed_from_u64(3),
            &session,
            1,
        );

        assert_eq!(run.len(), 1, "P1's resolving pass was applied");
        assert!(matches!(run.stop, AiActionsStop::NoEligibleAiActor));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::PhysicalContest { .. }
        ));
    }
}
