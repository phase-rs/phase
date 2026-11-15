//! CR 117.3b + CR 117.3d + CR 117.4 + CR 732.2b: one all-pass boundary of a
//! stack-resolution session may consume several fenced stack entries only when
//! every priority participant authorized the windows that boundary skips.
//! Consuming K entries skips each participant's window after each of the first
//! K − 1 resolutions, so a verified AI cohort collapses its run while a
//! participant with neither a verified nor a standing pass, or with Full
//! Control, keeps one entry per boundary. A Recheck participant also authorizes
//! a window by its standing pass over that window's top
//! (`priority::standing_priority_pass`: its own object, or a trigger it yielded
//! to), unless it holds Full Control.
//!
//! Every row drives the real priority pipeline: AI seats through
//! `engine::apply_verified_ai_priority_pass`, human seats through `apply`.

use std::collections::{BTreeMap, BTreeSet};

use engine::ai_support::AiDecisionContract;
use engine::game::engine::{apply, apply_verified_ai_priority_pass};
use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{GameAction, PriorityYieldOp};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassMode, AutoPassRequest, GameState, PriorityPassingMode, StackResolutionAutoPassOverlay,
    StackResolutionBudget, StackResolutionEntryFence, StackResolutionPolicy,
    StackResolutionSession, WaitingFor, YieldScope,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const SCUTE_SWARM_ORACLE: &str = "Landfall — Whenever a land you control enters, create a 1/1 green Insect creature token. If you control six or more lands, create a token that's a copy of this creature instead.";

const SOUL_WARDEN_ORACLE: &str = "Whenever another creature enters, you gain 1 life.";

const BRISTLY_BILL_ORACLE: &str = "Landfall — Whenever a land you control enters, put a +1/+1 counter on target creature.\n{3}{G}{G}: Double the number of +1/+1 counters on each creature you control.";

const AUTHORITY_OF_THE_CONSULS_ORACLE: &str = "Creatures your opponents control enter tapped.\nWhenever a creature an opponent controls enters, you gain 1 life.";

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
/// same fenced run. P1 is the ordinary seat: P0 owns the run, so P0's standing
/// pass would authorize every window over it without a verified pass.
#[test]
fn verified_ai_cohort_collapses_on_an_automatic_close() {
    let mut board = four_seat_scute_board(P0, None, &[]);
    let state = board.runner.state_mut();

    // P1 passes as an ordinary seat and has no standing pass over P0's run, so
    // P3's explicit close resolves exactly one entry.
    verified_ai_pass(state, P0);
    apply(state, P1, GameAction::PassPriority).expect("P1 may pass");
    for player in [P2, P3] {
        verified_ai_pass(state, player);
    }
    // Reach guards: the cohort survived its first close, holds every seat but
    // P1 as verified, and P1 holds the next window.
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
        BTreeSet::from([P0, P2, P3])
    );
    assert_eq!((cohort.cursor, cohort.entries.len()), (1, SCUTE_COUNT));
    assert_eq!(priority_holder(state), P1);
    perf_counters::reset();

    // P1's verified pass opens the round; P2 and P3 already verified, so the
    // session passes each of them automatically and P3's automatic pass closes
    // the round.
    verified_ai_pass(state, P1);

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

/// Drives priority until the stack empties or a non-priority prompt is up:
/// `human` passes through `apply`, every other seat through a verified AI pass.
/// Returns `(human_dispatches, ai_dispatches)`.
fn drive_with_human(state: &mut GameState, human: PlayerId, cap: usize) -> (usize, usize) {
    let mut human_dispatches = 0;
    let mut ai_dispatches = 0;
    while !state.stack.is_empty() {
        assert!(
            human_dispatches + ai_dispatches < cap,
            "the drive terminates"
        );
        let WaitingFor::Priority { player } = state.waiting_for else {
            break;
        };
        if player == human {
            apply(state, human, GameAction::PassPriority).expect("the human seat may pass");
            human_dispatches += 1;
        } else {
            verified_ai_pass(state, player);
            ai_dispatches += 1;
        }
    }
    (human_dispatches, ai_dispatches)
}

/// P0 yields (CR 117.3d) to each source's triggers through the production
/// `SetPriorityYield` action.
fn yield_to(state: &mut GameState, sources: &[ObjectId]) {
    for &source_id in sources {
        apply(
            state,
            P0,
            GameAction::SetPriorityYield {
                op: PriorityYieldOp::Add {
                    source_id,
                    scope: YieldScope::ThisObject,
                },
            },
        )
        .expect("P0 may yield to a trigger on the stack");
    }
}

fn stack_resolved_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
        .count()
}

/// A1: the human run owner's standing pass authorizes every window over its
/// own run, so the cohort collapses the run and P0 dispatches only its opening
/// pass.
#[test]
fn standing_pass_collapses_the_human_owners_run() {
    let mut board = four_seat_scute_board(P0, None, &[]);
    let state = board.runner.state_mut();
    perf_counters::reset();

    apply(state, P0, GameAction::PassPriority).expect("P0 may pass");
    verified_ai_pass(state, P1);
    // Reach guard: the installer verified only itself, so the run is intact.
    assert_eq!(state.stack.len(), SCUTE_COUNT);
    assert_eq!(
        session(state).verified_pass_representatives,
        BTreeSet::from([P1])
    );
    let (later_human_dispatches, later_ai_dispatches) = drive_with_human(state, P0, 40);

    assert_eq!(
        1 + later_human_dispatches,
        1,
        "P0's opening pass is its only dispatch"
    );
    assert_eq!(1 + later_ai_dispatches, 3);
    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    let counters = perf_counters::snapshot();
    assert_eq!(counters.stack_batch_plans, 1);
    assert_eq!(counters.stack_batched_entries, SCUTE_COUNT as u64);
}

/// A1-S: the snapshot's topology. P3 is active and already passed, P1–P3 are
/// verified, and P0, the unverified run owner, holds priority. One P0 pass
/// consumes the whole run; P2's automatic pass closes the round.
#[test]
fn snapshot_topology_one_human_pass_consumes_the_owned_run() {
    let mut board = four_seat_scute_board(P0, None, &[]);
    let state = board.runner.state_mut();
    let entries: Vec<_> = state
        .stack
        .iter()
        .rev()
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
        verified_pass_representatives: BTreeSet::from([P1, P2, P3]),
        budget: StackResolutionBudget::Unlimited,
        policy: StackResolutionPolicy::RecheckNoMeaningfulPriorityAction,
        auto_pass_overlay: StackResolutionAutoPassOverlay {
            baseline: BTreeMap::new(),
        },
    });
    state.active_player = P3;
    state.priority_passes = BTreeSet::from([P3]);
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    // Reach guards: the snapshot's session fields.
    assert_eq!(
        session(state).verified_pass_representatives,
        BTreeSet::from([P1, P2, P3])
    );
    assert_eq!(priority_holder(state), P0);
    perf_counters::reset();

    let result = apply(state, P0, GameAction::PassPriority).expect("P0 may pass");

    assert_eq!(stack_resolved_count(&result.events), SCUTE_COUNT);
    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        SCUTE_COUNT as u64
    );
}

/// A2 (preservation, positive control A1): a Full Control owner is never
/// standing-passed, so each of its windows is offered.
#[test]
fn full_control_owner_is_never_standing_passed() {
    let mut board = four_seat_scute_board(P0, None, &[P0]);
    let state = board.runner.state_mut();
    perf_counters::reset();

    let (human_dispatches, _) = drive_with_human(state, P0, 80);

    assert_eq!(human_dispatches, SCUTE_COUNT);
    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    assert_eq!(perf_counters::snapshot().stack_batched_entries, 0);
}

/// P1 owns the Scute run; P0 yields to the `yielded` top-most entries'
/// sources, then P1 and P2 pass as verified AI seats.
fn opponent_run_after_two_ai_passes(yielded: usize) -> ScuteBoard {
    let mut board = four_seat_scute_board(P1, None, &[]);
    let state = board.runner.state_mut();
    let sources: Vec<ObjectId> = state
        .stack
        .iter()
        .rev()
        .take(yielded)
        .map(|entry| entry.source_id)
        .collect();
    yield_to(state, &sources);
    assert_eq!(state.priority_yields.len(), yielded);
    for player in [P1, P2] {
        verified_ai_pass(state, player);
    }
    // Reach guard: no entry resolved before P3's pass.
    assert_eq!(state.stack.len(), SCUTE_COUNT);
    assert_eq!(
        session(state).verified_pass_representatives,
        BTreeSet::from([P1, P2])
    );
    perf_counters::reset();
    board
}

/// A3: P0's yields to every source of P1's run are standing passes, so P0's
/// automatic pass closes the round and the run collapses without a P0
/// dispatch.
#[test]
fn yield_lets_the_session_pass_an_opponents_run() {
    let mut board = opponent_run_after_two_ai_passes(SCUTE_COUNT);
    let state = board.runner.state_mut();

    verified_ai_pass(state, P3);

    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        SCUTE_COUNT as u64
    );
}

/// A3-ctl (preservation): without a yield P0 has no standing pass over P1's
/// run, so the session pauses at P0's window.
#[test]
fn unyielded_opponent_run_pauses_the_human() {
    let mut board = opponent_run_after_two_ai_passes(0);
    let state = board.runner.state_mut();

    verified_ai_pass(state, P3);

    assert_eq!(priority_holder(state), P0);
    assert_eq!(state.stack.len(), SCUTE_COUNT);
    let cohort = session(state);
    assert_eq!(
        cohort.policy,
        StackResolutionPolicy::RecheckNoMeaningfulPriorityAction
    );
    assert_eq!(
        cohort.verified_pass_representatives,
        BTreeSet::from([P1, P2, P3])
    );
    assert_eq!(cohort.cursor, 0);
}

/// A4-Y: each skipped window is judged against its own top. P0 yielded to the
/// top three sources only, so the boundary stops before the first window over
/// an unyielded source.
#[test]
fn partial_yield_stops_at_the_first_unyielded_window() {
    const YIELDED: usize = 3;
    let mut board = opponent_run_after_two_ai_passes(YIELDED);
    let state = board.runner.state_mut();
    let unyielded_source = state.stack[SCUTE_COUNT - YIELDED - 1].source_id;
    // Reach guard: the next top after the yielded prefix is not yielded.
    assert!(state
        .stack
        .iter()
        .rev()
        .take(YIELDED)
        .all(|entry| entry.source_id != unyielded_source));

    verified_ai_pass(state, P3);

    assert_eq!(state.stack.len(), SCUTE_COUNT - YIELDED);
    assert_eq!(insects(state), YIELDED);
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        YIELDED as u64
    );
    assert_eq!(priority_holder(state), P0);
    let cohort = session(state);
    assert_eq!(
        (cohort.cursor, cohort.entries.len()),
        (YIELDED, SCUTE_COUNT)
    );
}

/// A4: P0's own run above P1's entry. P1 is active with Scute Swarm and Soul
/// Warden; P0 controls six Authority of the Consuls. The Insect entering under
/// P1 triggers Soul Warden (P1, active player, bottom) and the six Authority
/// triggers (P0, top) per CR 603.3b. The boundary consumes exactly P0's run and
/// stops with priority at P0's window over P1's entry.
#[test]
fn own_run_stops_at_an_opponent_entry() {
    let mut scenario = GameScenario::new_n_player(4, 0x5C07E);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P1, "Scute Swarm", 1, 1, SCUTE_SWARM_ORACLE)
        .with_subtypes(vec!["Insect"]);
    let soul_warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN_ORACLE)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    for _ in 0..SCUTE_COUNT {
        scenario.add_enchantment_from_oracle(
            P0,
            "Authority of the Consuls",
            AUTHORITY_OF_THE_CONSULS_ORACLE,
        );
    }
    let forest = scenario.add_land_to_hand(P1, "Forest").id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");
    for player in [P1, P2, P3] {
        verified_ai_pass(runner.state_mut(), player);
    }
    assert_eq!(priority_holder(runner.state()), P0);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 may pass the Scute trigger");
    // CR 603.3b: P0 orders its own simultaneous Authority triggers.
    if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the identity order is legal");
    }
    let state = runner.state_mut();
    // Reach guard: Soul Warden's trigger at the bottom, P0's six above it.
    let controllers: Vec<PlayerId> = state.stack.iter().map(|entry| entry.controller).collect();
    assert_eq!(
        controllers,
        [vec![P1], vec![P0; SCUTE_COUNT]].concat(),
        "stack controllers bottom to top"
    );
    assert_eq!(state.stack[0].source_id, soul_warden);
    perf_counters::reset();

    let mut ai_dispatches = 0;
    while priority_holder(state) != P0 {
        assert!(ai_dispatches < 40, "the AI seats reach P0's window");
        let holder = priority_holder(state);
        verified_ai_pass(state, holder);
        ai_dispatches += 1;
    }

    assert_eq!(priority_holder(state), P0);
    assert_eq!(state.stack.len(), 1);
    assert_eq!(state.stack.back().map(|entry| entry.controller), Some(P1));
    assert_eq!(state.players[0].life, 20 + SCUTE_COUNT as i32);
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        SCUTE_COUNT as u64
    );
    let cohort = session(state);
    assert_eq!(
        (cohort.cursor, cohort.entries.len()),
        (SCUTE_COUNT, SCUTE_COUNT + 1)
    );
}

/// A8: the realistic composition. A land enters under P0 with Bristly Bill and
/// six Scute Swarm (fewer than six lands, so each Scute trigger takes the
/// Insect branch). P0 orders Bristly Bill's trigger first (bottom, CR 603.3b)
/// through the engine-issued prompt and targets from its legal targets; then
/// one P0 pass collapses the Scute run.
#[test]
fn realistic_composition_targeted_landfall_then_scute_run() {
    let mut scenario = GameScenario::new_n_player(4, 0x5C07E);
    scenario.at_phase(Phase::PreCombatMain);
    let bristly = scenario
        .add_creature_from_oracle(P0, "Bristly Bill, Spine Sower", 2, 2, BRISTLY_BILL_ORACLE)
        .with_subtypes(vec!["Plant", "Druid"])
        .as_legendary()
        .id();
    for _ in 0..SCUTE_COUNT {
        scenario
            .add_creature_from_oracle(P0, "Scute Swarm", 1, 1, SCUTE_SWARM_ORACLE)
            .with_subtypes(vec!["Insect"]);
    }
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("playing the Forest is legal");
    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "seven landfall triggers must raise the CR 603.3b order prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    let bristly_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == bristly)
        .expect("Bristly Bill's trigger is in the order prompt");
    let order = std::iter::once(bristly_index)
        .chain((0..triggers.len()).filter(|&index| index != bristly_index))
        .collect();
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("the engine-issued order is legal");
    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "Bristly Bill's trigger must pause for its target, got {:?}",
            runner.state().waiting_for
        );
    };
    let target = target_slots[0].legal_targets[0].clone();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(target),
        })
        .expect("the engine-issued target is legal");
    let state = runner.state_mut();
    // Reach guards: Bristly Bill's entry at the bottom, six Scute triggers
    // above it, and the construction carrier released.
    assert_eq!(state.stack.len(), SCUTE_COUNT + 1);
    assert_eq!(state.stack[0].source_id, bristly);
    assert!(state.pending_trigger_event_batch.is_empty());
    perf_counters::reset();

    let (human_dispatches, _) = drive_with_human(state, P0, 80);

    assert_eq!(human_dispatches, 1);
    assert!(state.stack.is_empty());
    assert_eq!(insects(state), SCUTE_COUNT);
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        SCUTE_COUNT as u64
    );
}
