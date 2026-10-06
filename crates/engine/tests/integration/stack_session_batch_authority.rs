//! CR 117.3b + CR 117.3d + CR 117.4 + CR 732.2b: one all-pass boundary of a
//! stack-resolution session may consume several fenced stack entries only when
//! every priority participant authorized the windows that boundary skips.
//! Consuming K entries skips each participant's window after each of the first
//! K − 1 resolutions, so a verified AI cohort collapses its run while an
//! unverified or Full Control participant keeps one entry per boundary.
//!
//! Every row drives the real priority pipeline: AI seats through
//! `engine::apply_verified_ai_priority_pass`, human seats through `apply`.

use std::collections::{BTreeMap, BTreeSet};

use engine::ai_support::AiDecisionContract;
use engine::game::engine::{apply, apply_verified_ai_priority_pass};
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassMode, AutoPassRequest, GameState, PriorityPassingMode, StackResolutionAutoPassOverlay,
    StackResolutionBudget, StackResolutionEntryFence, StackResolutionPolicy,
    StackResolutionSession, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const SCUTE_SWARM_ORACLE: &str = "Landfall — Whenever a land you control enters, create a 1/1 green Insect creature token. If you control six or more lands, create a token that's a copy of this creature instead.";

const SOUL_WARDEN_ORACLE: &str = "Whenever another creature enters, you gain 1 life.";

const SCUTE_COUNT: usize = 6;

struct ScuteBoard {
    runner: GameRunner,
    soul_warden: Option<ObjectId>,
}

/// Four seats; `controller` is active, controls six Scute Swarm (and P1
/// optionally controls Soul Warden), and plays a Forest with no other land, so
/// each Scute trigger takes the Insect branch: six identical untargeted
/// triggers with Priority back to `controller`. `full_control` seats set the
/// Full Control passing mode before the land is played.
fn four_seat_scute_board(
    controller: PlayerId,
    soul_warden: Option<PlayerId>,
    full_control: &[PlayerId],
) -> ScuteBoard {
    let mut scenario = GameScenario::new_n_player(4, 0x5C07E);
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..SCUTE_COUNT {
        scenario
            .add_creature_from_oracle(controller, "Scute Swarm", 1, 1, SCUTE_SWARM_ORACLE)
            .with_subtypes(vec!["Insect"]);
    }
    let soul_warden = soul_warden.map(|player| {
        scenario
            .add_creature_from_oracle(player, "Soul Warden", 1, 1, SOUL_WARDEN_ORACLE)
            .with_subtypes(vec!["Human", "Cleric"])
            .id()
    });
    let forest = scenario.add_land_to_hand(controller, "Forest").id();
    let mut runner = scenario.build();
    {
        // `at_phase` makes P0 active; the board's controller takes the turn.
        let state = runner.state_mut();
        state.active_player = controller;
        state.priority_player = controller;
        state.waiting_for = WaitingFor::Priority { player: controller };
        for &player in full_control {
            state
                .priority_passing_modes
                .insert(player, PriorityPassingMode::FullControl);
        }
    }
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");
    // CR 603.3b: answer any engine-issued order prompt with the identity order.
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the identity order is legal");
    }
    assert_eq!(
        runner.state().stack.len(),
        SCUTE_COUNT,
        "reach guard: six Scute landfall triggers are on the stack"
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: controller }
    );
    ScuteBoard {
        runner,
        soul_warden,
    }
}

fn verified_ai_pass(state: &mut GameState, player: PlayerId) {
    let contract = AiDecisionContract::issue(state, player);
    apply_verified_ai_priority_pass(state, player, &contract, GameAction::PassPriority)
        .expect("a verified AI priority pass is legal");
}

fn insects(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token
                && object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Insect")
        })
        .count()
}

fn session(state: &GameState) -> &StackResolutionSession {
    state
        .stack_resolution_session
        .as_ref()
        .expect("a stack-resolution session is installed")
}

fn priority_holder(state: &GameState) -> PlayerId {
    match state.waiting_for {
        WaitingFor::Priority { player } => player,
        ref other => panic!("expected a priority window, got {other:?}"),
    }
}

fn all_seats() -> BTreeSet<PlayerId> {
    BTreeSet::from([P0, P1, P2, P3])
}

/// A1: four verified AI seats collapse P0's Scute run in the closing pass.
#[test]
fn verified_ai_cohort_resolves_the_fenced_run_in_one_boundary() {
    let mut board = four_seat_scute_board(P0, None, &[]);
    let state = board.runner.state_mut();
    perf_counters::reset();

    for player in [P0, P1, P2] {
        verified_ai_pass(state, player);
        assert_eq!(
            state.stack.len(),
            SCUTE_COUNT,
            "no entry resolves before the last seat passes"
        );
    }
    // Reach guard: the AI install verified only the installer, and each later
    // pass extended the cohort by exactly its own seat.
    let cohort = session(state);
    assert_eq!(
        cohort.policy,
        StackResolutionPolicy::RecheckNoMeaningfulPriorityAction
    );
    assert_eq!(cohort.representatives, all_seats());
    assert_eq!(
        cohort.verified_pass_representatives,
        BTreeSet::from([P0, P1, P2])
    );
    assert_eq!((cohort.cursor, cohort.entries.len()), (0, SCUTE_COUNT));
    assert_eq!(priority_holder(state), P3);

    verified_ai_pass(state, P3);

    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    let counters = perf_counters::snapshot();
    assert_eq!(counters.stack_batch_plans, 1);
    assert_eq!(counters.stack_batched_entries, SCUTE_COUNT as u64);
}

/// A1-A: the cohort also collapses when the round closes on an AUTOMATIC pass
/// by a seat whose verified pass was recorded in an earlier window of the
/// same fenced run.
#[test]
fn verified_ai_cohort_collapses_on_an_automatic_close() {
    let mut board = four_seat_scute_board(P0, None, &[]);
    let state = board.runner.state_mut();

    // P0 passes as an ordinary seat; the cohort installs on P1's verified pass
    // without P0, so P3's explicit close resolves exactly one entry.
    apply(state, P0, GameAction::PassPriority).expect("P0 may pass");
    for player in [P1, P2, P3] {
        verified_ai_pass(state, player);
    }
    // Reach guards: the cohort survived its first close, holds every seat but
    // P0 as verified, and P0 holds the next window.
    assert_eq!(state.stack.len(), SCUTE_COUNT - 1);
    assert_eq!(insects(state), 1);
    let cohort = session(state);
    assert_eq!(
        cohort.policy,
        StackResolutionPolicy::RecheckNoMeaningfulPriorityAction
    );
    assert_eq!(cohort.representatives, all_seats());
    assert_eq!(
        cohort.verified_pass_representatives,
        BTreeSet::from([P1, P2, P3])
    );
    assert_eq!((cohort.cursor, cohort.entries.len()), (1, SCUTE_COUNT));
    assert_eq!(priority_holder(state), P0);
    perf_counters::reset();

    // P0's verified pass opens the round; P1, P2 and P3 already verified, so
    // the session passes each of them automatically and P3's automatic pass
    // closes the round.
    verified_ai_pass(state, P0);

    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    let counters = perf_counters::snapshot();
    assert_eq!(counters.stack_batch_plans, 1);
    assert_eq!(counters.stack_batched_entries, (SCUTE_COUNT - 1) as u64);
}

/// A1-H1: K never exceeds the fenced prefix. A cohort fencing only the top
/// three entries consumes exactly those three and leaves the rest for a new
/// cohort.
#[test]
fn verified_ai_cohort_stays_inside_its_fenced_prefix() {
    let mut board = four_seat_scute_board(P0, None, &[]);
    let state = board.runner.state_mut();
    let fenced = 3;
    let entries: Vec<_> = state
        .stack
        .iter()
        .rev()
        .take(fenced)
        .map(StackResolutionEntryFence::capture)
        .collect();
    for player in all_seats() {
        state.auto_pass.insert(
            player,
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: state.stack.len(),
                policy: StackResolutionPolicy::RecheckNoMeaningfulPriorityAction,
            },
        );
    }
    state.stack_resolution_session = Some(StackResolutionSession {
        entries,
        cursor: 0,
        representatives: all_seats(),
        verified_pass_representatives: BTreeSet::from([P0, P1, P2]),
        budget: StackResolutionBudget::Unlimited,
        policy: StackResolutionPolicy::RecheckNoMeaningfulPriorityAction,
        auto_pass_overlay: StackResolutionAutoPassOverlay {
            baseline: BTreeMap::new(),
        },
    });
    state.priority_passes = BTreeSet::from([P0, P1, P2]);
    state.priority_player = P3;
    state.waiting_for = WaitingFor::Priority { player: P3 };
    perf_counters::reset();

    verified_ai_pass(state, P3);

    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        fenced as u64
    );
    assert_eq!(state.stack.len(), SCUTE_COUNT - fenced);
    assert_eq!(insects(state), fenced);
    assert!(
        state.stack_resolution_session.is_none(),
        "the cohort ends at its fence"
    );
    assert_eq!(priority_holder(state), P0);
}

/// A1-H2: when a member's checkpoint adds a stack entry (Soul Warden), the
/// batch proof refuses, one entry resolves, and the changed topology tears the
/// cohort down; the run still finishes with the sequential outcome.
#[test]
fn verified_ai_cohort_refuses_a_run_that_grows_the_stack() {
    let mut board = four_seat_scute_board(P0, Some(P1), &[]);
    let soul_warden = board.soul_warden.expect("Soul Warden is on the board");
    let state = board.runner.state_mut();
    perf_counters::reset();

    for player in [P0, P1, P2, P3] {
        verified_ai_pass(state, player);
    }

    assert_eq!(insects(state), 1);
    assert_eq!(
        state.stack.back().map(|entry| entry.source_id),
        Some(soul_warden),
        "Soul Warden's trigger lands above the remaining Scute entries"
    );
    assert!(state.stack_resolution_session.is_none());
    assert_eq!(priority_holder(state), P0);
    let counters = perf_counters::snapshot();
    assert!(
        counters.stack_batch_candidates >= 1,
        "reach guard: the cohort authorized K > 1 and the proof was attempted"
    );
    assert_eq!(counters.stack_batch_plans, 0);
    assert_eq!(counters.stack_batched_entries, 0);

    let mut dispatches = 0;
    while !state.stack.is_empty() {
        assert!(dispatches < 80, "the growth run terminates");
        let holder = priority_holder(state);
        verified_ai_pass(state, holder);
        dispatches += 1;
    }
    assert_eq!(insects(state), SCUTE_COUNT);
    // Soul Warden gained P1 one life per Insect that entered.
    assert_eq!(state.players[1].life, 20 + SCUTE_COUNT as i32);
}

/// A2 (preservation): an unverified human participant never authorized its
/// later windows, so each of its passes resolves exactly one entry.
#[test]
fn unverified_human_participant_keeps_one_entry_per_pass() {
    let mut board = four_seat_scute_board(P1, None, &[]);
    let state = board.runner.state_mut();
    perf_counters::reset();

    for player in [P1, P2, P3] {
        verified_ai_pass(state, player);
    }
    // Reach guard: the cohort exists and only the human seat is missing.
    assert_eq!(priority_holder(state), P0);
    let cohort = session(state);
    assert_eq!(
        cohort.policy,
        StackResolutionPolicy::RecheckNoMeaningfulPriorityAction
    );
    assert_eq!(
        cohort.verified_pass_representatives,
        BTreeSet::from([P1, P2, P3])
    );

    let mut human_passes = 0;
    while !state.stack.is_empty() {
        assert_eq!(priority_holder(state), P0);
        let stack_before = state.stack.len();
        let cursor_before = session(state).cursor;
        let result = apply(state, P0, GameAction::PassPriority).expect("P0 may pass");
        human_passes += 1;
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                .count(),
            1
        );
        assert_eq!(state.stack.len(), stack_before - 1);
        if let Some(cohort) = state.stack_resolution_session.as_ref() {
            assert_eq!(cohort.cursor, cursor_before + 1);
        }
    }
    assert_eq!(human_passes, SCUTE_COUNT);
    assert_eq!(perf_counters::snapshot().stack_batched_entries, 0);
    assert_eq!(insects(state), SCUTE_COUNT);
}

/// P0 submits Resolve All (`SetAutoPass UntilStackEmpty`), installing a
/// Committed session with P0 as its only representative.
fn commit_resolve_all(runner: &mut GameRunner) {
    perf_counters::reset();
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("P0 may request Resolve All");
}

fn assert_committed_cursor(state: &GameState, cursor: usize) {
    let cohort = session(state);
    assert_eq!(cohort.policy, StackResolutionPolicy::Committed);
    assert_eq!(cohort.representatives, BTreeSet::from([P0]));
    assert_eq!((cohort.cursor, cohort.entries.len()), (cursor, SCUTE_COUNT));
}

/// A3: a Full Control non-representative's window after each resolution is
/// offered, even when an automatic pass closes the round.
#[test]
fn full_control_participant_is_offered_each_window_of_a_committed_run() {
    let mut board = four_seat_scute_board(P0, None, &[P1]);
    commit_resolve_all(&mut board.runner);
    // Reach guard: the live Full Control gate paused the session at P1.
    let state = board.runner.state_mut();
    assert_eq!(priority_holder(state), P1);
    assert_eq!(state.stack.len(), SCUTE_COUNT);
    assert_committed_cursor(state, 0);

    apply(state, P1, GameAction::PassPriority).expect("P1 may pass");

    assert_eq!(state.stack.len(), SCUTE_COUNT - 1);
    assert_eq!(insects(state), 1);
    assert_eq!(priority_holder(state), P1);
    assert_committed_cursor(state, 1);
    assert_eq!(perf_counters::snapshot().stack_batched_entries, 0);
}

/// A3b: as A3, but the Full Control seat's own explicit pass closes the round.
#[test]
fn full_control_participant_explicit_close_resolves_one_entry() {
    let mut board = four_seat_scute_board(P0, None, &[P3]);
    commit_resolve_all(&mut board.runner);
    let state = board.runner.state_mut();
    assert_eq!(priority_holder(state), P3);
    assert_committed_cursor(state, 0);

    apply(state, P3, GameAction::PassPriority).expect("P3 may pass");

    assert_eq!(state.stack.len(), SCUTE_COUNT - 1);
    assert_eq!(priority_holder(state), P3);
    assert_committed_cursor(state, 1);
    assert_eq!(perf_counters::snapshot().stack_batched_entries, 0);
}

/// A3-ctl (preservation): without Full Control the committed run collapses in
/// the Resolve All dispatch itself.
#[test]
fn committed_run_without_full_control_resolves_in_one_dispatch() {
    let mut board = four_seat_scute_board(P0, None, &[]);
    commit_resolve_all(&mut board.runner);
    let state = board.runner.state();
    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        SCUTE_COUNT as u64
    );
}

/// A3-R: a Full Control representative is never skipped either, so its own
/// Resolve All resolves one entry per explicit pass.
#[test]
fn full_control_representative_resolves_one_entry_per_pass() {
    let mut board = four_seat_scute_board(P0, None, &[P0]);
    commit_resolve_all(&mut board.runner);
    let state = board.runner.state_mut();
    assert_eq!(state.stack.len(), SCUTE_COUNT - 1);
    assert_eq!(insects(state), 1);
    assert_eq!(priority_holder(state), P0);
    assert_committed_cursor(state, 1);

    apply(state, P0, GameAction::PassPriority).expect("P0 may pass");

    assert_eq!(state.stack.len(), SCUTE_COUNT - 2);
    assert_eq!(perf_counters::snapshot().stack_batched_entries, 0);
}
