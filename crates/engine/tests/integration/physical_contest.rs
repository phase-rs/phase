//! Eye to Eye / Mouth to Mouth — the online sustained-clicking adaptation of the
//! staring and breath-holding contests.
//!
//! The Comprehensive Rules define no physical contests, so nothing here cites a
//! CR number for the contest itself. The surrounding rules exercised are CR 608.2c
//! (an "if you win" rider is read from the performed instruction) and CR 109.4
//! (controller of the targeted creature).
//!
//! Every test casts the real card through the cast pipeline, then plays the contest
//! through the host driver (`game::contest::*`) with explicit millisecond stamps —
//! no sleeping, no clock.

use engine::game::contest::{
    advance_contest, cancel_contest, contest_click, set_contest_connection, start_contest,
    ContestStep,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::contest::{
    ClickDisposition, ClickRejection, ContestCancelReason, ContestConfig, ContestKind,
    ContestOutcome, ContestPhase, ContestResult,
};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (MTGJSON/Scryfall).
const EYE_TO_EYE: &str =
    "You and target creature's controller have a staring contest. If you win, destroy that creature.";
const MOUTH_TO_MOUTH: &str = "Choose target opponent and target creature they control. You and that player have a breath-holding contest. If you win, you gain control of that creature.";

/// Short, exact timings so each test reads as a timeline. GO is at t=1000.
fn config() -> ContestConfig {
    ContestConfig {
        countdown_ms: 1_000,
        inactivity_timeout_ms: 1_000,
        reconnect_grace_ms: 5_000,
        settle_window_ms: 0,
        max_active_ms: 600_000,
    }
}

struct Table {
    runner: GameRunner,
    victim: ObjectId,
    events: Vec<GameEvent>,
}

fn eye_to_eye_table() -> Table {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_vanilla(P1, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[victim]).resolve();
    Table {
        runner,
        victim,
        events: Vec::new(),
    }
}

fn mouth_to_mouth_table() -> Table {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_vanilla(P1, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mouth to Mouth", false, MOUTH_TO_MOUTH)
        .id();
    let mut runner = scenario.build();
    runner
        .cast(spell)
        .target_player(P1)
        .target_objects(&[victim])
        .resolve();
    Table {
        runner,
        victim,
        events: Vec::new(),
    }
}

impl Table {
    fn state(&self) -> &engine::types::game_state::GameState {
        self.runner.state()
    }

    fn contest(&self) -> &engine::types::contest::ContestState {
        match &self.state().waiting_for {
            WaitingFor::PhysicalContest { contest, .. } => contest,
            other => panic!("expected PhysicalContest, got {other:?}"),
        }
    }

    fn start(&mut self) -> ContestStep {
        start_contest(self.runner.state_mut(), config(), 0, &mut self.events)
    }

    fn tick(&mut self, now: u64) -> ContestStep {
        advance_contest(self.runner.state_mut(), now, &mut self.events)
    }

    fn click(&mut self, player: PlayerId, seq: u32, now: u64) -> (ClickDisposition, ContestStep) {
        // After settlement there is no contest to read an id from; any id is then
        // equally "not in a contest".
        let id = match &self.state().waiting_for {
            WaitingFor::PhysicalContest { contest, .. } => contest.id,
            _ => 0,
        };
        contest_click(
            self.runner.state_mut(),
            id,
            player,
            seq,
            now,
            &mut self.events,
        )
    }

    /// Both players click every 400ms starting at GO until `until`; `quit` stops
    /// one player's clicking after the given instant.
    fn rally(&mut self, until: u64, quit: Option<(PlayerId, u64)>) {
        let (mut seq0, mut seq1) = (0, 0);
        let mut t = 1_000;
        while t <= until && matches!(self.state().waiting_for, WaitingFor::PhysicalContest { .. }) {
            for (player, seq) in [(P0, &mut seq0), (P1, &mut seq1)] {
                if quit.is_some_and(|(p, at)| p == player && t > at) {
                    continue;
                }
                *seq += 1;
                self.click(player, *seq, t);
            }
            t += 400;
        }
    }

    fn resolved_events(&self) -> Vec<&GameEvent> {
        self.events
            .iter()
            .filter(|e| matches!(e, GameEvent::PhysicalContestResolved { .. }))
            .collect()
    }
}

#[test]
fn eye_to_eye_parses_to_a_contest_not_unimplemented() {
    let table = eye_to_eye_table();
    let contest = table.contest();
    assert_eq!(contest.kind, ContestKind::Staring);
    assert_eq!(
        contest.players(),
        [P0, P1],
        "controller vs target's controller"
    );
    assert_eq!(
        contest.phase,
        ContestPhase::Pending,
        "pending until the host starts it"
    );
}

#[test]
fn contest_waits_for_the_host_and_has_no_actor() {
    let table = eye_to_eye_table();
    assert!(table.state().waiting_for.acting_players().is_empty());
}

#[test]
fn eye_to_eye_winner_destroys_the_creature() {
    let mut table = eye_to_eye_table();
    assert_eq!(table.start(), ContestStep::Updated);
    table.rally(10_000, Some((P1, 1_500)));
    assert!(
        !matches!(
            table.state().waiting_for,
            WaitingFor::PhysicalContest { .. }
        ),
        "the contest settled"
    );
    assert_eq!(
        table.state().objects[&table.victim].zone,
        Zone::Graveyard,
        "P0 outlasted P1, so the creature is destroyed"
    );
}

#[test]
fn eye_to_eye_loser_keeps_the_creature() {
    let mut table = eye_to_eye_table();
    table.start();
    table.rally(10_000, Some((P0, 1_500)));
    assert!(!matches!(
        table.state().waiting_for,
        WaitingFor::PhysicalContest { .. }
    ));
    assert_eq!(table.state().objects[&table.victim].zone, Zone::Battlefield);
}

#[test]
fn a_tie_does_nothing_for_eye_to_eye() {
    let mut table = eye_to_eye_table();
    table.start();
    // Both players give up together.
    table.rally(1_800, None);
    table.tick(20_000);
    assert!(matches!(
        table.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(table.state().objects[&table.victim].zone, Zone::Battlefield);
    let resolved = table.resolved_events();
    assert_eq!(resolved.len(), 1);
    assert!(matches!(
        resolved[0],
        GameEvent::PhysicalContestResolved {
            result: ContestResult::Outcome(ContestOutcome::Tie),
            ..
        }
    ));
}

#[test]
fn a_cancelled_contest_does_nothing_and_is_not_a_tie() {
    let mut table = eye_to_eye_table();
    table.start();
    table.tick(1_000);
    assert_eq!(
        cancel_contest(
            table.runner.state_mut(),
            ContestCancelReason::HostAborted,
            1_100,
            &mut table.events
        ),
        ContestStep::Settled
    );
    assert!(matches!(
        table.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(table.state().objects[&table.victim].zone, Zone::Battlefield);
    assert!(matches!(
        table.resolved_events()[0],
        GameEvent::PhysicalContestResolved {
            result: ContestResult::Cancelled(ContestCancelReason::HostAborted),
            ..
        }
    ));
}

#[test]
fn the_result_is_applied_exactly_once() {
    let mut table = eye_to_eye_table();
    table.start();
    table.rally(10_000, Some((P1, 1_500)));
    assert_eq!(table.resolved_events().len(), 1);
    // Duplicate/late deliveries after settlement are inert.
    assert_eq!(table.tick(30_000), ContestStep::NotInContest);
    let (disposition, step) = table.click(P1, 99, 30_100);
    assert_eq!(step, ContestStep::NotInContest);
    assert!(matches!(disposition, ClickDisposition::Rejected(_)));
    assert_eq!(
        cancel_contest(
            table.runner.state_mut(),
            ContestCancelReason::HostAborted,
            30_200,
            &mut table.events
        ),
        ContestStep::NotInContest
    );
    assert_eq!(table.resolved_events().len(), 1);
}

#[test]
fn a_duplicate_start_does_not_restart_the_countdown() {
    let mut table = eye_to_eye_table();
    assert_eq!(table.start(), ContestStep::Updated);
    let go = table.contest().go_at_ms;
    let again = start_contest(table.runner.state_mut(), config(), 700, &mut table.events);
    assert_eq!(again, ContestStep::Unchanged);
    assert_eq!(table.contest().go_at_ms, go);
}

#[test]
fn clicks_before_go_are_not_counted() {
    let mut table = eye_to_eye_table();
    table.start();
    let (disposition, step) = table.click(P0, 1, 500);
    assert!(matches!(disposition, ClickDisposition::Rejected(_)));
    assert_eq!(step, ContestStep::Unchanged);
    assert_eq!(table.contest().contestants[0].clicks, 0);
}

#[test]
fn a_disconnect_past_grace_forfeits_only_the_contest() {
    let mut table = eye_to_eye_table();
    table.start();
    table.tick(1_000);
    assert_eq!(
        set_contest_connection(
            table.runner.state_mut(),
            P1,
            false,
            1_200,
            &mut table.events
        ),
        ContestStep::Updated
    );
    assert_eq!(table.tick(6_300), ContestStep::Settled);
    assert_eq!(
        table.state().objects[&table.victim].zone,
        Zone::Graveyard,
        "P1 forfeited the contest, so P0 won it"
    );
    assert!(
        !matches!(table.state().waiting_for, WaitingFor::GameOver { .. }),
        "a forfeited contest is not a forfeited game"
    );
}

#[test]
fn a_participant_conceding_cancels_the_contest() {
    let mut table = eye_to_eye_table();
    table.start();
    table.tick(1_000);
    engine::game::engine::apply(
        table.runner.state_mut(),
        P1,
        GameAction::Concede { player_id: P1 },
    )
    .expect("concede is always legal");
    // Two-player game: the concession ends the game and replaces the contest state.
    if matches!(
        table.state().waiting_for,
        WaitingFor::PhysicalContest { .. }
    ) {
        assert_eq!(table.tick(1_100), ContestStep::Settled);
    }
    assert!(!matches!(
        table.state().waiting_for,
        WaitingFor::PhysicalContest { .. }
    ));
}

#[test]
fn mouth_to_mouth_winner_gains_control_of_the_creature() {
    let mut table = mouth_to_mouth_table();
    assert_eq!(table.contest().kind, ContestKind::BreathHolding);
    assert_eq!(table.contest().players(), [P0, P1]);
    table.start();
    table.rally(10_000, Some((P1, 1_500)));
    assert!(!matches!(
        table.state().waiting_for,
        WaitingFor::PhysicalContest { .. }
    ));
    assert_eq!(table.state().objects[&table.victim].controller, P0);
}

#[test]
fn mouth_to_mouth_loser_and_tie_leave_control_unchanged() {
    let mut table = mouth_to_mouth_table();
    table.start();
    table.rally(10_000, Some((P0, 1_500)));
    assert_eq!(table.state().objects[&table.victim].controller, P1);

    let mut table = mouth_to_mouth_table();
    table.start();
    table.rally(1_800, None);
    table.tick(20_000);
    assert!(matches!(
        table.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(table.state().objects[&table.victim].controller, P1);
}

// ---------------------------------------------------------------------------
// Settlement runs the ordinary post-resolution pipeline (CR 117.3b, 117.5, 704.3)
// ---------------------------------------------------------------------------

const DYING_WITNESS: &str = "When Dying Witness dies, you gain 3 life.";

/// Eye to Eye on a creature with a death trigger, cast while it is `P1`'s turn.
fn eye_to_eye_vs_witness(token: bool, off_turn: bool) -> Table {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature_from_oracle(P1, "Dying Witness", 2, 2, DYING_WITNESS)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
        .id();
    let mut runner = scenario.build();
    if token {
        runner
            .state_mut()
            .objects
            .get_mut(&victim)
            .unwrap()
            .is_token = true;
    }
    if off_turn {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.cast(spell).target_objects(&[victim]).resolve();
    Table {
        runner,
        victim,
        events: Vec::new(),
    }
}

/// CR 603.3 + CR 117.5: a destroyed creature's death trigger is put on the stack
/// before anyone receives priority — the host settled the contest outside `apply()`,
/// so settlement itself must run the trigger pass.
#[test]
fn a_destroyed_creatures_death_trigger_goes_on_the_stack_at_settlement() {
    let mut table = eye_to_eye_vs_witness(false, false);
    table.start();
    table.rally(10_000, Some((P1, 1_500)));

    assert_eq!(table.state().objects[&table.victim].zone, Zone::Graveyard);
    assert_eq!(
        table.state().stack.len(),
        1,
        "the death trigger is waiting on the stack, not left unscanned"
    );
}

/// CR 704.5d + CR 704.3: a destroyed token is put into the graveyard and then
/// ceases to exist as the state-based actions run before priority.
#[test]
fn a_destroyed_token_ceases_to_exist_at_settlement() {
    let mut table = eye_to_eye_vs_witness(true, false);
    assert!(table.state().objects[&table.victim].is_token, "reach-guard");
    table.start();
    table.rally(10_000, Some((P1, 1_500)));

    assert!(
        table
            .state()
            .objects
            .get(&table.victim)
            .is_none_or(|object| object.zone != Zone::Graveyard),
        "a token never lingers in the graveyard"
    );
}

/// CR 117.3b: the ACTIVE player receives priority after a spell resolves — not the
/// off-turn player who cast it.
#[test]
fn priority_after_an_off_turn_contest_goes_to_the_active_player() {
    let mut table = eye_to_eye_vs_witness(false, true);
    assert_eq!(
        table.state().active_player,
        P1,
        "reach-guard: P0 cast off-turn"
    );
    table.start();
    table.rally(10_000, Some((P1, 1_500)));

    // The trigger (P1's Dying Witness) is on the stack, so P1 — the active player —
    // is the one who receives priority first.
    assert!(
        matches!(
            table.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ),
        "got {:?}",
        table.state().waiting_for
    );
}

/// The contest's own participants' departure in a pod: with three players the game
/// continues, and the departed contestant's contest is cancelled (not stuck).
#[test]
fn a_contestant_leaving_a_three_player_game_cancels_the_contest() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_vanilla(P1, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[victim]).resolve();
    let mut table = Table {
        runner,
        victim,
        events: Vec::new(),
    };
    table.start();
    table.tick(1_000);

    engine::game::engine::apply(
        table.runner.state_mut(),
        P1,
        GameAction::Concede { player_id: P1 },
    )
    .expect("a contestant may concede mid-contest");
    table.tick(1_100);

    assert!(
        !matches!(
            table.state().waiting_for,
            WaitingFor::PhysicalContest { .. }
        ),
        "the contest no longer waits on a departed contestant"
    );
    assert!(
        !matches!(table.state().waiting_for, WaitingFor::GameOver { .. }),
        "the pod plays on"
    );
    assert!(table.resolved_events().iter().any(|e| matches!(
        e,
        GameEvent::PhysicalContestResolved {
            result: ContestResult::Cancelled(ContestCancelReason::ParticipantLeftGame),
            ..
        }
    )));
}

// ---------------------------------------------------------------------------
// Host ordering and identity
// ---------------------------------------------------------------------------

/// A host that re-sends its start on every tick (the local WASM driver does) must
/// not move the contest clock: buffered clicks stamped earlier still land at their
/// own stamps, and the outcome is the one the stamps decide.
#[test]
fn a_repeated_start_after_a_delayed_tick_does_not_clamp_earlier_clicks() {
    let mut table = eye_to_eye_table();
    table.start();
    table.tick(1_000);

    // The tick that carries these clicks is late (t=9s), and re-sends its start.
    let again = start_contest(table.runner.state_mut(), config(), 9_000, &mut table.events);
    assert_eq!(again, ContestStep::Unchanged);
    assert_eq!(
        table.contest().clock_ms,
        1_000,
        "a duplicate start observes no time"
    );

    // Delivered in stamp order, as the host applies them: P1 stops after 1.4s.
    assert_eq!(table.click(P0, 1, 1_400).0, ClickDisposition::Counted);
    assert_eq!(table.click(P1, 1, 1_400).0, ClickDisposition::Counted);
    assert_eq!(table.click(P0, 2, 1_900).0, ClickDisposition::Counted);
    assert_eq!(table.click(P0, 3, 2_400).0, ClickDisposition::Counted);
    table.tick(9_000);

    let [resolved] = table.resolved_events()[..] else {
        panic!("exactly one result");
    };
    assert!(
        matches!(
            resolved,
            GameEvent::PhysicalContestResolved {
                result: ContestResult::Outcome(ContestOutcome::Decided { winner, .. }),
                ..
            } if *winner == P0
        ),
        "P1 stopped at 1.4s, P0 kept going: {resolved:?}"
    );
}

fn capped_config() -> ContestConfig {
    ContestConfig {
        max_active_ms: 3_000,
        ..config()
    }
}

fn capped_table() -> Table {
    let mut table = eye_to_eye_table();
    start_contest(
        table.runner.state_mut(),
        capped_config(),
        0,
        &mut table.events,
    );
    table.tick(1_000);
    table
}

/// The same stamped history must decide the same way whether the tick that
/// evaluates it is on time or arrives after the hard cap: inactivity that was
/// already decidable by the cap is not turned into a cancellation by a late tick.
#[test]
fn a_late_tick_past_the_cap_decides_what_an_on_time_tick_would() {
    let outcome_after = |tick_at: u64| {
        let mut table = capped_table();
        table.click(P1, 1, 1_500);
        table.click(P0, 1, 2_000);
        table.tick(tick_at);
        let [resolved] = table.resolved_events()[..] else {
            panic!("settled exactly once at tick {tick_at}");
        };
        match resolved {
            GameEvent::PhysicalContestResolved { result, .. } => *result,
            _ => unreachable!(),
        }
    };

    let on_time = outcome_after(4_000);
    let late = outcome_after(10_000);
    assert_eq!(on_time, late);
    assert!(
        matches!(
            late,
            ContestResult::Outcome(ContestOutcome::Decided { winner, .. }) if winner == P0
        ),
        "P1 lapsed first, so P0 wins either way: {late:?}"
    );
}

/// A frame delayed from an earlier contest names that contest's identity and is
/// dropped before it can observe time or consume the running contest's sequence.
#[test]
fn a_click_for_another_contest_is_rejected_without_touching_the_clock() {
    let mut table = eye_to_eye_table();
    table.start();
    table.tick(1_000);
    let id = table.contest().id;
    assert_ne!(id, 0, "contests carry an engine-authored identity");

    let (disposition, step) = contest_click(
        table.runner.state_mut(),
        id + 1,
        P0,
        1,
        40_000,
        &mut table.events,
    );

    assert_eq!(
        disposition,
        ClickDisposition::Rejected(ClickRejection::WrongContest)
    );
    assert_eq!(step, ContestStep::Unchanged);
    assert_eq!(
        table.contest().clock_ms,
        1_000,
        "the stray stamp advanced nothing"
    );
    assert_eq!(table.click(P0, 1, 1_100).0, ClickDisposition::Counted);
}

/// Each contest gets its own identity: a second contest in the same game is never
/// confusable with the first, so the first's delayed frames cannot count in it.
#[test]
fn consecutive_contests_get_distinct_identities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_victim = scenario.add_vanilla(P1, 2, 2);
    let second_victim = scenario.add_vanilla(P1, 3, 3);
    let first_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
        .id();
    let second_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
        .id();
    let mut runner = scenario.build();

    runner
        .cast(first_spell)
        .target_objects(&[first_victim])
        .resolve();
    let mut table = Table {
        runner,
        victim: first_victim,
        events: Vec::new(),
    };
    let first_id = table.contest().id;
    table.start();
    table.rally(10_000, Some((P1, 1_500)));
    assert!(!matches!(
        table.state().waiting_for,
        WaitingFor::PhysicalContest { .. }
    ));

    table
        .runner
        .cast(second_spell)
        .target_objects(&[second_victim])
        .resolve();
    let second_id = table.contest().id;

    assert_ne!(first_id, second_id);
    assert_ne!(first_id, 0, "0 is never a valid identity");
    assert!(
        first_id < 1 << 53 && second_id < 1 << 53,
        "an identity must survive a round trip through a JavaScript number"
    );
}

/// The host settles outside `apply()`, so it must rebind the interaction slots that
/// `apply()` would have: the priority window a contest settles into needs a live
/// interaction id, or the player's first answer to it is rejected.
#[test]
fn settlement_rebinds_the_interaction_slots_for_the_new_priority_window() {
    use engine::game::interaction::bind_interaction_authority;
    use engine::types::interaction::InteractionSessionId;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_vanilla(P1, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
        .id();
    let mut runner = scenario.build();
    bind_interaction_authority(
        runner.state_mut(),
        InteractionSessionId("contest-test".to_string()),
    )
    .expect("a valid session binds");
    runner.cast(spell).target_objects(&[victim]).resolve();
    let mut table = Table {
        runner,
        victim,
        events: Vec::new(),
    };
    assert!(
        table.state().active_interaction_slots.is_empty(),
        "reach-guard: a host-driven contest has no human decision slot"
    );

    table.start();
    table.rally(10_000, Some((P1, 1_500)));

    let WaitingFor::Priority { player } = table.state().waiting_for else {
        panic!(
            "expected a priority window, got {:?}",
            table.state().waiting_for
        );
    };
    let slots = &table.state().active_interaction_slots;
    assert_eq!(slots.len(), 1, "the priority window has its slot");
    assert_eq!(slots[0].semantic_owner, player.0);
}
