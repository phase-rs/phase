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
//! asserts the single batched firing (CR 603.2c: one trigger event fires once,
//! so exactly one Calendar trigger reaches the stack) plus the three-counter
//! total it resolves into. Three one-counter batches would also total three,
//! so the total alone cannot pin the firing shape.
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
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter,
    TriggerConstraint, TriggerDefinition, TriggerLimitSpend, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
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
    // CR 603.2c: the simultaneous untap is one trigger event, so it fires once.
    // Three one-counter batches would also total three — count the Calendar's
    // stack entries before resolving to pin the single batched firing.
    let calendar_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. }
                if *source_id == calendar
            )
        })
        .count();
    assert_eq!(
        calendar_triggers, 1,
        "CR 603.2c: one simultaneous untap event must put exactly one Calendar \
         trigger on the stack, got {calendar_triggers}"
    );
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

/// Matrix rows 2-6 (issue #10940): hostile windows that must not count, the
/// doubling pin, the sub-threshold rest, and the multiplayer win.
///
/// Row 2 — an effect-untap outside your untap step (verbatim-Oracle Twiddle
/// during your upkeep) puts no counters; paired positive is row 1.
/// Row 3 — untaps during an opponent's untap step put no counters (your
/// tapped permanents stay tapped); paired positive is row 1.
/// Row 3a — P0 permanents untapped during P1's untap step (Seedborn-class
/// second pass) put no counters; paired positive is row 1.
/// Row 4 — {2},{T} doubles only your own Calendar's time counters.
/// Row 5 — 999 counters through priority: no sacrifice, no life change;
/// paired positive is row 6.
/// Row 6 — 3-player win: each opponent loses 1,000 life.
/// Single-fire pin — an unwindowed, unbatched `Untaps` fixture fires exactly
/// once through a real untap step (sole guard of the unwindowed
/// exactly-once half).
fn generic_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn blue_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect()
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("the source must expose its activated ability")
}

/// Discriminating reach guard for the hostile-window rows: the Calendar's
/// first-line head must parse to the fieldless `TriggerMode::Untaps` unit
/// variant. Fails pre-fix (the head is Unknown), holds post-fix — every
/// negative below is vacuous without it.
fn has_untaps_trigger(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .trigger_definitions
        .iter_unchecked()
        .any(|t| t.definition.mode == TriggerMode::Untaps)
}

/// The win half parses to a `StateCondition` trigger (already supported).
fn has_state_condition_trigger(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .trigger_definitions
        .iter_unchecked()
        .any(|t| t.definition.mode == TriggerMode::StateCondition)
}

/// Row 2: an effect-untap outside your untap step puts no time counters on
/// The Millennium Calendar (CR 603.2: the "during your untap step" window is
/// part of the trigger event, and an upkeep untap is outside it. CR 701.26:
/// the untap action).
///
/// Twiddle's Oracle text is verbatim: "You may tap or untap target artifact,
/// creature, or land."
///
/// Paired positive: row 1 — the same untap inside your untap step counts 3.
#[test]
fn millennium_calendar_ignores_effect_untap_outside_untap_step() {
    const TWIDDLE: &str = "You may tap or untap target artifact, creature, or land.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let calendar = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let twiddle = scenario
        .add_spell_to_hand_from_oracle(P0, "Twiddle", true, TWIDDLE)
        .id();
    scenario.with_mana_pool(P0, blue_mana(1));
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;

    assert!(
        has_untaps_trigger(&runner, calendar),
        "reach guard: the Calendar head must parse to TriggerMode::Untaps"
    );

    // Cast Twiddle on the tapped land, accept the may, choose untap.
    runner
        .cast(twiddle)
        .target_object(land)
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "Twiddle's may-untap must pause at the optional-effect choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Twiddle's may must be legal");
    let WaitingFor::ChooseOneOfBranch {
        player,
        source_id,
        branches,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Twiddle must offer tap or untap, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, twiddle);
    let index = branches
        .iter()
        .position(|branch| branch.description.as_deref() == Some("untap"))
        .expect("Twiddle must offer an untap branch");
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("choosing untap must be legal");
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&land].tapped,
        "reach guard: Twiddle must have untapped the land during the upkeep"
    );
    assert_eq!(
        runner.state().objects[&twiddle].zone,
        Zone::Graveyard,
        "reach guard: Twiddle must have resolved"
    );
    assert_eq!(
        time_counters(&runner, calendar),
        0,
        "an untap outside your untap step must put no time counters on The Millennium Calendar"
    );
    assert_eq!(
        runner.state().objects[&calendar].zone,
        Zone::Battlefield,
        "no counters means no win trigger"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack must be empty after Twiddle resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: the game must rest at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Row 3: untaps during an OPPONENT's untap step put no time counters on The
/// Millennium Calendar (CR 502.3: the turn-based action untaps only the
/// active player's permanents, so yours stay tapped. CR 603.2: the window
/// names YOUR untap step).
///
/// Paired positive: row 1 — your own untap step counts 3.
#[test]
fn millennium_calendar_ignores_opponent_untap_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Island");
    }
    let calendar = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    let own_lands = [
        scenario.add_basic_land(P0, ManaColor::White),
        scenario.add_basic_land(P0, ManaColor::Blue),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let foe_lands = [
        scenario.add_basic_land(P1, ManaColor::Red),
        scenario.add_basic_land(P1, ManaColor::Green),
    ];
    let mut runner = scenario.build();
    for land in own_lands.iter().chain(foe_lands.iter()) {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }

    assert!(
        has_untaps_trigger(&runner, calendar),
        "reach guard: the Calendar head must parse to TriggerMode::Untaps"
    );

    // Drive into P1's upkeep; P1's untap step runs on the way.
    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, |state| {
        state.turn_number == turn + 1 && state.phase == Phase::Upkeep
    });
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the drive must land in P1's upkeep"
    );
    assert!(
        steps.contains(&Phase::Untap),
        "reach guard: an untap step must have run: {steps:?}"
    );
    for land in foe_lands {
        assert!(
            !runner.state().objects[&land].tapped,
            "reach guard: CR 502.3 must have untapped the active player's lands"
        );
    }
    for land in own_lands {
        assert!(
            runner.state().objects[&land].tapped,
            "reach guard: the inactive player's lands must stay tapped"
        );
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        time_counters(&runner, calendar),
        0,
        "untaps during an opponent's untap step must put no time counters on The Millennium Calendar"
    );
    assert_eq!(
        runner.state().objects[&calendar].zone,
        Zone::Battlefield,
        "no counters means no win trigger"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack must be empty after the drive"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: the game must rest at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Row 4: dedicated doubling pin — {2},{T} doubles the time counters on The
/// Millennium Calendar itself (3 → exactly 6), leaves a hostile zero at zero
/// and opponent counters untouched (CR 608.2c: "Double the number of time
/// counters on The Millennium Calendar" names the source object).
#[test]
fn millennium_calendar_activation_doubles_only_own_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana(2));
    let own = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    scenario.with_counter(own, CounterType::Time, 3);
    let hostile_zero = scenario
        .add_artifact_from_oracle(P1, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    let hostile_two = scenario
        .add_artifact_from_oracle(P1, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    scenario.with_counter(hostile_two, CounterType::Time, 2);
    let mut runner = scenario.build();

    // Production activation: fund {2} from the pool, pay {T} by tapping.
    let idx = activated_index(&runner, own);
    runner.activate(own, idx).resolve();

    assert_eq!(
        time_counters(&runner, own),
        6,
        "doubling 3 time counters must give exactly 6"
    );
    assert!(
        runner.state().objects[&own].tapped,
        "the {{T}} cost must leave the Calendar tapped"
    );
    assert_eq!(
        time_counters(&runner, hostile_zero),
        0,
        "a hostile zero must stay zero"
    );
    assert_eq!(
        time_counters(&runner, hostile_two),
        2,
        "opponent counters must be untouched"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack must be empty after the activation resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: the game must rest at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Row 5: 999 time counters through priority — no sacrifice, no life change
/// (CR 603.8: the StateCondition win trigger matches only at 1,000 or more).
///
/// Paired positive: row 6 — 1,000 fires.
#[test]
fn millennium_calendar_below_threshold_does_not_win() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 2000);
    let calendar = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    scenario.with_counter(calendar, CounterType::Time, 999);
    let mut runner = scenario.build();

    assert!(
        has_state_condition_trigger(&runner, calendar),
        "reach guard: the win line must parse to a StateCondition trigger"
    );

    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&calendar].zone,
        Zone::Battlefield,
        "999 counters are below the 1,000 threshold, so the Calendar must stay"
    );
    assert_eq!(runner.life(P1), 2000, "no life change below the threshold");
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack must be empty"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: the game must rest at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Row 6: 3-player win — at 1,000 time counters The Millennium Calendar is
/// sacrificed and EACH opponent loses 1,000 life (CR 603.8 state trigger;
/// "each opponent" spans both opponents).
#[test]
fn millennium_calendar_win_hits_each_opponent_in_three_player() {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 2000);
    scenario.with_life(P2, 2000);
    let calendar = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    scenario.with_counter(calendar, CounterType::Time, 1000);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    assert!(
        has_state_condition_trigger(&runner, calendar),
        "reach guard: the win line must parse to a StateCondition trigger"
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
        "the first opponent must lose 1,000 life: 2000 - 1000 = 1000"
    );
    assert_eq!(
        runner.life(P2),
        1000,
        "the second opponent must lose 1,000 life: 2000 - 1000 = 1000"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "the controller must lose no life"
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

/// Row 3a: P0 permanents untapping during P1's untap step (Seedborn Muse's
/// second pass) put no time counters on The Millennium Calendar (CR 603.2:
/// the window names YOUR untap step — `DuringPlayersTurn{Controller}` inside
/// the `EventTime` window fails while P1 is active. CR 502.3: the Seedborn
/// second pass untaps P0's permanents during the opponent's untap step).
///
/// The Seedborn source is the printed card: Seedborn Muse's verbatim Oracle
/// ("Untap all permanents you control during each other player's untap
/// step.") parses through the production static pipeline to
/// `StaticMode::UntapsDuringEachOtherPlayersUntapStep`, and a reach guard
/// below pins that shape before the drive.
///
/// Paired positive: row 1 — the same untap inside your own untap step counts.
/// Discrimination: reverting `DuringPlayersTurn{Controller}` flips this row
/// 0 → N (the phase is still `Untap`, so the bare `CurrentPhaseIs` window
/// would match).
#[test]
fn millennium_calendar_ignores_own_permanents_untapped_on_opponent_untap_step() {
    // Seedborn Muse Oracle, verbatim (canonical card-data; 2/4 Spirit).
    const SEEDBORN_MUSE: &str =
        "Untap all permanents you control during each other player's untap step.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Island");
    }
    let calendar = scenario
        .add_artifact_from_oracle(P0, "The Millennium Calendar", MILLENNIUM_CALENDAR)
        .id();
    let muse = scenario
        .add_creature_from_oracle(P0, "Seedborn Muse", 2, 4, SEEDBORN_MUSE)
        .id();
    let own_lands = [
        scenario.add_basic_land(P0, ManaColor::White),
        scenario.add_basic_land(P0, ManaColor::Blue),
    ];
    let foe_lands = [
        scenario.add_basic_land(P1, ManaColor::Red),
        scenario.add_basic_land(P1, ManaColor::Green),
    ];
    let mut runner = scenario.build();
    for land in own_lands.iter().chain(foe_lands.iter()) {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }

    assert!(
        has_untaps_trigger(&runner, calendar),
        "reach guard: the Calendar head must parse to TriggerMode::Untaps"
    );
    assert!(
        runner.state().objects[&muse]
            .static_definitions
            .iter_all()
            .any(
                |s| s.mode == StaticMode::UntapsDuringEachOtherPlayersUntapStep
                    && matches!(
                        &s.affected,
                        Some(TargetFilter::Typed(tf))
                            if tf.controller == Some(ControllerRef::You)
                    )
            ),
        "reach guard: Seedborn Muse must parse to the Seedborn-class untap \
         static affecting permanents you control"
    );

    // Drive into P1's upkeep; P1's untap step runs on the way.
    let turn = runner.state().turn_number;
    let steps = drive_until(&mut runner, |state| {
        state.turn_number == turn + 1 && state.phase == Phase::Upkeep
    });
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the drive must land in P1's upkeep"
    );
    assert!(
        steps.contains(&Phase::Untap),
        "reach guard: an untap step must have run: {steps:?}"
    );
    for land in foe_lands {
        assert!(
            !runner.state().objects[&land].tapped,
            "reach guard: CR 502.3 must have untapped the active player's lands"
        );
    }
    for land in own_lands {
        assert!(
            !runner.state().objects[&land].tapped,
            "reach guard: the Seedborn second pass must have untapped P0's lands \
             during P1's untap step"
        );
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        time_counters(&runner, calendar),
        0,
        "P0 permanents untapping during P1's untap step must put no time \
         counters on The Millennium Calendar"
    );
    assert_eq!(
        runner.state().objects[&calendar].zone,
        Zone::Battlefield,
        "no counters means no win trigger"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack must be empty after the drive"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: the game must rest at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Single-fire pin: an UNWINDOWED, unbatched `Untaps` trigger fires exactly
/// once when one permanent untaps in an untap step. This is the sole guard
/// of the unwindowed exactly-once half: the turn-based-action hold inside
/// `untap_permanents` collects it once (phase is still Untap) and claims the
/// event, so the upkeep scan cannot re-collect it (2 = double-fire,
/// 0 = lost).
///
/// Printed cards already reach unbatched, unwindowed `Untaps`, including
/// Ghostly Pilferer. This synthetic fixture isolates a controller-wide
/// permanent filter and a time-counter payload. It drives the real pipeline
/// (scenario → untap step → upkeep priority drain → resolution), asserting
/// exactly one firing.
/// Needs-assessment: needed — yes; sole guard of the unwindowed
/// exactly-once half.
///
/// Reach guard: the installed definition is `Untaps` with `batched == false`
/// and no condition.
#[test]
fn unwindowed_singular_untap_trigger_fires_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Island");
    }
    let source = scenario.add_creature(P0, "Untap Witness", 2, 2).id();
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("fixture source")
        .push_printed_trigger(
            TriggerDefinition::new(TriggerMode::Untaps)
                .valid_card(TargetFilter::Typed(
                    TypedFilter::permanent().controller(ControllerRef::You),
                ))
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::PutCounter {
                        target: TargetFilter::SelfRef,
                        counter_type: CounterType::Time,
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                )),
        );

    assert!(
        runner.state().objects[&source]
            .trigger_definitions
            .iter_unchecked()
            .any(|t| t.definition.mode == TriggerMode::Untaps
                && !t.definition.batched
                && t.definition.condition.is_none()),
        "reach guard: the fixture must be singular unbatched Untaps with no window"
    );

    // Drive through P1's turn into P0's next upkeep; P0's untap step runs on
    // the way and untaps the one tapped land.
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
    assert!(
        !runner.state().objects[&land].tapped,
        "reach guard: CR 502.3 must have untapped the tapped land"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        time_counters(&runner, source),
        1,
        "one permanent untapped in an untap step must fire an unwindowed \
         singular Untaps trigger exactly once"
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

/// Ledger-atomicity pin (issue #10940 finding): a ledger-backed-constrained
/// (`OncePerTurn { Triggering }`), UNWINDOWED, singular `Untaps` trigger
/// fires EXACTLY ONCE when two permanents untap during the untap step.
///
/// History: the interim pre-advance seam admitted+recorded the dropped
/// contexts, so the post-advance upkeep scan refused to re-admit them and
/// the trigger fired zero times. At head that seam is gone — upstream's hold
/// inside `untap_permanents` is the single collector, so no second admission
/// exists to spend ledgers twice. This pin guards the hold path: the first
/// untap is admitted (spending the limit) while the second finds it spent,
/// exactly one firing for two untaps (0 = the finding, 2 = double-fire).
///
/// Fixture honesty: no printed card exhibits constrained+unwindowed+Untaps.
/// Measured over the 7407-card `integration_cards/` fixture, exactly
/// five cards parse to `TriggerMode::Untaps` — Arbiter of the Ideal, Daring
/// Thief, Felhide Spiritbinder, Fishing Pole, Ghostly Pilferer — all singular
/// "becomes untapped" subjects, all `constraint: null`, all `condition:
/// null`. The nearest printed card is Ghostly Pilferer ("Whenever Ghostly
/// Pilferer becomes untapped, you may pay {2}. If you do, draw a card."): the
/// same singular unwindowed Untaps shape, but unconstrained and optional. The
/// `OncePerTurn` limit below is therefore installed synthetically — the
/// narrowest permitted construction (`push_printed_trigger`, as the sibling
/// row does) — while the test drives the REAL pipeline (scenario → untap
/// step → upkeep priority drain → resolution) and asserts exactly one firing.
/// Needs-assessment: needed — yes; sole guard of the ledger-atomicity finding
/// (the finding's Closes-when).
///
/// Reach guard: the installed definition is singular unbatched `Untaps` with
/// no condition and an `OncePerTurn { Triggering }` limit.
#[test]
fn unwindowed_once_per_turn_untap_trigger_fires_exactly_once_for_two_untaps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Island");
    }
    let source = scenario.add_creature(P0, "Ledger Witness", 2, 2).id();
    let lands = [
        scenario.add_basic_land(P0, ManaColor::White),
        scenario.add_basic_land(P0, ManaColor::Blue),
    ];
    let mut runner = scenario.build();
    for land in lands {
        runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
    }
    let mut definition = TriggerDefinition::new(TriggerMode::Untaps)
        .valid_card(TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::You),
        ))
        .execute(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                counter_type: CounterType::Time,
                count: QuantityExpr::Fixed { value: 1 },
            },
        ));
    definition.constraint = Some(TriggerConstraint::OncePerTurn {
        spent_by: TriggerLimitSpend::Triggering,
    });
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("fixture source")
        .push_printed_trigger(definition);

    assert!(
        runner.state().objects[&source]
            .trigger_definitions
            .iter_unchecked()
            .any(|t| t.definition.mode == TriggerMode::Untaps
                && !t.definition.batched
                && t.definition.condition.is_none()
                && matches!(
                    t.definition.constraint,
                    Some(TriggerConstraint::OncePerTurn {
                        spent_by: TriggerLimitSpend::Triggering
                    })
                )),
        "reach guard: the fixture must be singular unbatched Untaps with no window and a Triggering-spent OncePerTurn limit"
    );

    // Drive through P1's turn into P0's next upkeep; P0's untap step runs on
    // the way and untaps both tapped lands.
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
            "reach guard: CR 502.3 must have untapped both tapped lands"
        );
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        time_counters(&runner, source),
        1,
        "two permanents untapped in an untap step must fire a OncePerTurn unwindowed Untaps trigger exactly once"
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
