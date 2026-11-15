//! The Millennium Calendar — untap-count trigger + 1,000-counter win.
//!
//! Issue #10940 / PR #10013. Oracle (verbatim):
//!   "Whenever you untap one or more permanents during your untap step, put
//!    that many time counters on The Millennium Calendar.
//!    {2}, {T}: Double the number of time counters on The Millennium Calendar.
//!    When there are 1,000 or more time counters on The Millennium Calendar,
//!    sacrifice it and each opponent loses 1,000 life."
//!
//! Leg 1 (RED): untapping three permanents during your untap step must put
//! three time counters on the Calendar. Pre-fix the trigger condition is
//! unparsed (preview coverage marks the node supported:false), so no counters
//! are ever added and the doubling ability spins 0 into 0. The test drives the
//! real turn structure — P0's next untap step untaps three tapped lands — and
//! asserts the counter total, which is robust to the batching shape (one
//! trigger counting 3, or three triggers of 1 each).
//!
//! Leg 2 (win gate): at 1,000 time counters the StateCondition trigger fires —
//! the Calendar is sacrificed and each opponent loses 1,000 life. This pins the
//! already-supported win half of the card while leg 1 drives the fix.
//!
//! CR references (verified against docs/MagicCompRules.txt, edition effective
//! September 25, 2026):
//!   - CR 502.3: the untap step's turn-based action untaps the active player's
//!     permanents simultaneously.
//!   - CR 502.4: no player receives priority during the untap step; abilities
//!     that trigger there are held until the next priority.
//!   - CR 603.3: a triggered ability goes on the stack the next time a player
//!     would receive priority.
//!   - CR 603.8: state triggers fire as soon as the game state matches.
//!   - CR 122.1: a counter is a marker placed on an object.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const MILLENNIUM_CALENDAR: &str = "Whenever you untap one or more permanents during your untap step, put that many time counters on The Millennium Calendar.\n{2}, {T}: Double the number of time counters on The Millennium Calendar.\nWhen there are 1,000 or more time counters on The Millennium Calendar, sacrifice it and each opponent loses 1,000 life.";

fn time_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Time)
        .copied()
        .unwrap_or(0)
}

/// Drive the game until `stop` holds: pass every priority window, declare no
/// attackers for P0 (P1 attacks with whatever it validly may — nothing here),
/// no blockers. Returns every step entered. Mirrors the driver in
/// `added_untap_step_is_not_a_turn_start.rs`.
fn drive_until(runner: &mut GameRunner, stop: impl Fn(&GameState) -> bool) -> Vec<Phase> {
    let mut steps = Vec::new();
    for _ in 0..200 {
        if stop(runner.state()) {
            return steps;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                ..
            } => GameAction::DeclareAttackers {
                attacks: if *player == P1 {
                    valid_attacker_ids
                        .iter()
                        .map(|id| (*id, AttackTarget::Player(P0)))
                        .collect()
                } else {
                    Vec::new()
                },
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
            other => panic!("unexpected prompt while driving: {other:?}"),
        };
        let result = runner.act(action).expect("driver action accepted");
        steps.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
    }
    panic!("stop condition never held; steps entered: {steps:?}");
}

/// Leg 1: untapping three permanents during your untap step puts three time
/// counters on The Millennium Calendar (CR 502.3 + CR 502.4 + CR 603.3).
///
/// RED pre-fix (issue #10940): the trigger condition is unparsed, so the
/// Calendar gains zero counters.
#[test]
fn millennium_calendar_puts_one_time_counter_per_permanent_untapped_in_untap_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Island");
    }
    let calendar = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    let lands = [
        scenario.add_basic_land(P0, ManaColor::White),
        scenario.add_basic_land(P0, ManaColor::Blue),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let mut runner = scenario.build();
    for land in lands {
        runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
    }

    // Drive through P1's turn into P0's next upkeep; P0's untap step runs on
    // the way (CR 502.3) and any trigger it raises is held for upkeep
    // priority (CR 502.4 + CR 603.3).
    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, |state| {
        state.turn_number == turn + 2 && state.phase == Phase::Upkeep
    });
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach guard: the drive must land in P0's next upkeep"
    );
    assert!(
        steps.contains(&Phase::Untap),
        "reach guard: an untap step must have run: {steps:?}"
    );
    for land in lands {
        assert!(
            !runner.state().objects[&land].tapped,
            "reach guard: CR 502.3 must have untapped the tapped lands"
        );
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        time_counters(&runner, calendar),
        3,
        "untapping three permanents during your untap step must put three time \
         counters on The Millennium Calendar (issue #10940)"
    );
    assert_eq!(
        runner.state().objects[&calendar].zone,
        Zone::Battlefield,
        "3 counters are below the 1,000 threshold, so the win trigger must not \
         have fired"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack must be empty after the trigger resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: the game must rest at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Leg 2: at 1,000 time counters the StateCondition win trigger fires — the
/// Calendar is sacrificed and each opponent loses 1,000 life (CR 603.8).
#[test]
fn millennium_calendar_win_fires_at_one_thousand_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 2000);
    let calendar = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    scenario.with_counter(calendar, CounterType::Time, 1000);
    let mut runner = scenario.build();

    assert!(
        runner.state().objects[&calendar]
            .trigger_definitions
            .iter_unchecked()
            .any(|t| t.definition.mode == TriggerMode::StateCondition),
        "reach guard: the win line must parse to a StateCondition trigger; got {:?}",
        runner.state().objects[&calendar]
            .trigger_definitions
            .iter_unchecked()
            .map(|t| t.definition.mode.clone())
            .collect::<Vec<_>>()
    );

    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&calendar].zone,
        Zone::Graveyard,
        "the win trigger must sacrifice The Millennium Calendar"
    );
    assert_eq!(
        runner.life(P1),
        1000,
        "each opponent must lose 1,000 life: 2000 - 1000 = 1000"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack must be empty after the win trigger resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: the game must rest at priority, got {:?}",
        runner.state().waiting_for
    );
}
