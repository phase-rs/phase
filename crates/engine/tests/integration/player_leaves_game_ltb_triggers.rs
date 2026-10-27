//! CR 800.4a + CR 603.6c: when a player leaves the game, every object they own
//! leaves the game. A phased-in permanent leaving this way triggers
//! leaves-the-battlefield abilities, but it is not exiled, so an ability that
//! names where an object went never sees it. An object a leaving player
//! controls but doesn't own is exiled, and both kinds of observer see that.

use engine::game::game_object::{PhaseOutCause, PhaseStatus};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::zones::apply_resolved_zone_change;
use engine::types::ability::{
    DelayedTriggerCondition, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{DelayedTrigger, WaitingFor, ZoneDeparture};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{ResolvedRulesCommand, ResolvedZoneChangeCommand};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const EXTRACTOR_DEMON_ORACLE: &str = "Flying\n\
     Whenever another creature leaves the battlefield, you may have target player mill two \
     cards.";

const SOULHERDER_ORACLE: &str =
    "Whenever a creature is exiled from the battlefield, put a +1/+1 counter on this creature.";

const EXILE_WATCHER_ORACLE: &str =
    "Whenever a creature is put into exile from the battlefield, you gain 1 life.";

/// The board every test shares: P0 has Extractor Demon, Soulherder and an
/// exile watcher (a zone-change trigger naming exile as the destination); P2
/// owns a bear, and controls a bear P1 owns. Two P0 enchantments are the sources
/// of delayed abilities a test may install on the owned bear.
struct Board {
    runner: GameRunner,
    demon: ObjectId,
    soulherder: ObjectId,
    exile_watcher: ObjectId,
    owned_bear: ObjectId,
    borrowed_bear: ObjectId,
    leaves_marker: ObjectId,
    dies_or_exiled_marker: ObjectId,
}

fn board(configure: impl FnOnce(&mut GameScenario)) -> Board {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let demon = scenario
        .add_creature_from_oracle(P0, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE)
        .id();
    let soulherder = scenario
        .add_creature_from_oracle(P0, "Soulherder", 1, 1, SOULHERDER_ORACLE)
        .id();
    let exile_watcher = scenario
        .add_creature_from_oracle(P0, "Exile Watcher", 1, 1, EXILE_WATCHER_ORACLE)
        .id();
    let owned_bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let borrowed_bear = scenario
        .add_creature(P1, "P1 Bear", 2, 2)
        .controlled_by(P2)
        .id();
    let leaves_marker = scenario
        .add_creature(P0, "Leaves Marker", 0, 0)
        .as_enchantment()
        .id();
    let dies_or_exiled_marker = scenario
        .add_creature(P0, "Dies Or Exiled Marker", 0, 0)
        .as_enchantment()
        .id();
    configure(&mut scenario);
    Board {
        runner: scenario.build(),
        demon,
        soulherder,
        exile_watcher,
        owned_bear,
        borrowed_bear,
        leaves_marker,
        dies_or_exiled_marker,
    }
}

/// CR 603.7b: install a one-shot delayed ability P0 controls, sourced from
/// `marker`, that gains P0 one life when `condition` is met.
fn install_delayed_gain_life(
    runner: &mut GameRunner,
    marker: ObjectId,
    condition: DelayedTriggerCondition,
) {
    install_delayed_gain_life_for(runner, P0, marker, condition);
}

/// [`install_delayed_gain_life`] for a delayed ability `controller` controls.
fn install_delayed_gain_life_for(
    runner: &mut GameRunner,
    controller: PlayerId,
    marker: ObjectId,
    condition: DelayedTriggerCondition,
) {
    runner
        .state_mut()
        .delayed_triggers
        .push(DelayedTrigger::new(
            condition,
            Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                marker,
                controller,
            )),
            controller,
            marker,
            true,
        ));
}

/// The two delayed abilities on the owned bear: "when it leaves the
/// battlefield" and "when it dies or is exiled".
fn install_delayed_watchers_on_owned_bear(board: &mut Board) {
    let bear = board.owned_bear;
    install_delayed_gain_life(
        &mut board.runner,
        board.leaves_marker,
        DelayedTriggerCondition::WhenLeavesPlay { object_id: bear },
    );
    install_delayed_gain_life(
        &mut board.runner,
        board.dies_or_exiled_marker,
        DelayedTriggerCondition::WhenDiesOrExiled {
            filter: TargetFilter::SpecificObject { id: bear },
        },
    );
}

/// Put pending triggers on the stack: order them as listed and give each the
/// first legal target, until a player has priority.
fn drive_trigger_prompts(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("a trigger takes its first legal target");
            }
            other => panic!("unexpected prompt while putting triggers on the stack: {other:?}"),
        }
    }
    panic!("triggers were not put on the stack within the prompt budget");
}

fn triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Assert what P0's observers put on the stack for P2's departure: the bear P2
/// owned left the game, and the bear P2 controlled but didn't own was exiled.
fn assert_departure_triggers(board: &Board) {
    let runner = &board.runner;
    assert_eq!(
        triggers_from(runner, board.demon),
        2,
        "CR 603.6c: Extractor Demon sees the bear P2 owned leave the game and the bear \
         P2 controlled get exiled"
    );
    assert_eq!(
        triggers_from(runner, board.soulherder),
        1,
        "CR 800.4a: only the bear P2 controlled but didn't own was exiled"
    );
    assert_eq!(
        triggers_from(runner, board.exile_watcher),
        1,
        "CR 800.4a: a trigger naming exile as the destination sees only the exiled bear"
    );
}

/// Assert what the two delayed abilities on the owned bear put on the stack.
fn assert_delayed_departure_triggers(
    runner: &GameRunner,
    leaves: ObjectId,
    dies_or_exiled: ObjectId,
) {
    assert_eq!(
        triggers_from(runner, leaves),
        1,
        "CR 603.6c: the delayed leaves-the-battlefield ability sees the bear leave the game"
    );
    assert_eq!(
        triggers_from(runner, dies_or_exiled),
        0,
        "CR 800.4a: the bear that left the game neither died nor was exiled"
    );
}

/// CR 104.3a + CR 800.4a + CR 603.6c: P2 concedes. Their bear leaves the game
/// and the bear they controlled is exiled (it is still controlled by P2 once the
/// owned objects are gone). Leaves-the-battlefield observers, ordinary and
/// delayed, see both leave; exile observers see only the exiled one.
///
/// REVERT-FAILING: without the concession collecting its triggers (ordinary and
/// delayed), the Demon and the delayed leaves ability trigger zero times; without
/// the leave-the-game departure, Soulherder and the exile watcher trigger for the
/// bear that left the game too.
#[test]
fn concession_triggers_leaves_the_battlefield_but_not_exile_for_owned_permanents() {
    let mut board = board(|_| {});
    install_delayed_watchers_on_owned_bear(&mut board);

    board
        .runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert_eq!(zone(&board.runner, board.owned_bear), Zone::Exile);
    assert_eq!(zone(&board.runner, board.borrowed_bear), Zone::Exile);

    drive_trigger_prompts(&mut board.runner);
    assert_departure_triggers(&board);
    assert_delayed_departure_triggers(
        &board.runner,
        board.leaves_marker,
        board.dies_or_exiled_marker,
    );
}

/// CR 704.5a + CR 800.4a + CR 603.6c: the same departure by losing the game to a
/// state-based action. The post-action pipeline already scans these events, so
/// this row pins the departure reading: the owned bear left the game and was not
/// exiled.
///
/// REVERT-FAILING: without the leave-the-game departure, Soulherder and the exile
/// watcher trigger twice.
#[test]
fn losing_to_zero_life_triggers_leaves_the_battlefield_but_not_exile_for_owned_permanents() {
    let mut board = board(|scenario| {
        scenario.with_life(P2, 0);
    });

    board
        .runner
        .act(GameAction::PassPriority)
        .expect("state-based actions run");
    assert!(board.runner.state().players[2].is_eliminated);

    drive_trigger_prompts(&mut board.runner);
    assert_departure_triggers(&board);
}

/// CR 104.3a + CR 104.1: a concession that ends the game puts nothing on the
/// stack, though the conceding player's creature left the battlefield. Nothing
/// is collected at all: a one-shot delayed "when it leaves the battlefield"
/// ability on that creature is still installed afterwards, since collecting it
/// would have consumed it.
///
/// REVERT-FAILING: without the game-over guard on the concession's collection,
/// the delayed ability is consumed.
#[test]
fn concession_that_ends_the_game_puts_no_trigger_on_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE);
    let bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let marker = scenario
        .add_creature(P0, "Leaves Marker", 0, 0)
        .as_enchantment()
        .id();
    let mut runner = scenario.build();
    install_delayed_gain_life(
        &mut runner,
        marker,
        DelayedTriggerCondition::WhenLeavesPlay { object_id: bear },
    );

    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
    // Reach guard: the bear left the battlefield with its owner.
    assert_eq!(zone(&runner, bear), Zone::Exile);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().deferred_triggers.is_empty());
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "CR 104.1: the finished game collects nothing, so the one-shot delayed ability is not consumed"
    );
}

/// CR 702.26k: a phased-out permanent owned by a leaving player also leaves the
/// game, but doesn't cause zone-change abilities to trigger. The reach guard is
/// the phased-in bear P2 controlled, which the Demon still sees exiled.
///
/// REVERT-FAILING: without the phased-out departure, the Demon also triggers for
/// the phased-out bear.
#[test]
fn concession_with_a_phased_out_permanent_triggers_nothing_for_it() {
    let Board {
        mut runner,
        demon,
        soulherder,
        owned_bear,
        leaves_marker,
        dies_or_exiled_marker: filtered_leaves_marker,
        ..
    } = board(|_| {});
    install_delayed_gain_life(
        &mut runner,
        leaves_marker,
        DelayedTriggerCondition::WhenLeavesPlay {
            object_id: owned_bear,
        },
    );
    install_delayed_gain_life(
        &mut runner,
        filtered_leaves_marker,
        DelayedTriggerCondition::WhenLeavesPlayFiltered {
            filter: TargetFilter::SpecificObject { id: owned_bear },
        },
    );
    runner
        .state_mut()
        .objects
        .get_mut(&owned_bear)
        .expect("the bear is on the battlefield")
        .phase_status = PhaseStatus::PhasedOut {
        cause: PhaseOutCause::Directly,
    };

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert_eq!(zone(&runner, owned_bear), Zone::Exile);

    drive_trigger_prompts(&mut runner);
    assert_eq!(
        triggers_from(&runner, demon),
        1,
        "CR 702.26k: only the exiled bear, not the phased-out one, is seen leaving"
    );
    assert_eq!(triggers_from(&runner, soulherder), 1);
    for marker in [leaves_marker, filtered_leaves_marker] {
        assert_eq!(
            triggers_from(&runner, marker),
            0,
            "CR 702.26k: a delayed leaves-the-battlefield ability doesn't see it either"
        );
    }
}

/// How P2 leaves the game in a test.
#[derive(Clone, Copy, Debug)]
enum Departure {
    /// CR 104.3a: P2 concedes.
    Concession,
    /// CR 704.5a: P2 is at 0 life when P0 passes priority.
    ZeroLife,
}

/// CR 800.4a + CR 603.10a: everything P2 owns leaves the game at once. An
/// Extractor Demon P2 owns but P0 controls leaves with P2's other creatures and,
/// looking back, sees them leave in the same event; P0 controls those triggers.
/// The Demon P2 controlled sees them too, but a trigger P2 would control isn't
/// put on the stack (CR 800.4d), and the game doesn't wait on P2.
fn assert_departing_observer_sees_the_others(departure: Departure) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let borrowed_demon = scenario
        .add_creature_from_oracle(P2, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE)
        .controlled_by(P0)
        .id();
    let p2_demon = scenario
        .add_creature_from_oracle(P2, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE)
        .id();
    scenario.add_creature(P2, "P2 Bear", 2, 2);
    if matches!(departure, Departure::ZeroLife) {
        scenario.with_life(P2, 0);
    }
    let mut runner = scenario.build();

    let action = match departure {
        Departure::Concession => GameAction::Concede { player_id: P2 },
        Departure::ZeroLife => GameAction::PassPriority,
    };
    runner.act(action).expect("P2 leaves the game");
    assert!(runner.state().players[2].is_eliminated);
    assert_eq!(zone(&runner, borrowed_demon), Zone::Exile);
    assert_ne!(
        runner.state().waiting_for.acting_player(),
        Some(P2),
        "{departure:?}: CR 800.4a: the game doesn't wait on a player who left"
    );

    drive_trigger_prompts(&mut runner);
    assert_eq!(
        triggers_from(&runner, borrowed_demon),
        2,
        "{departure:?}: CR 603.10a: the departing Demon P0 controlled sees the other two creatures leave with it"
    );
    assert_eq!(
        triggers_from(&runner, p2_demon),
        0,
        "{departure:?}: CR 800.4d: a trigger the departed player would control isn't put on the stack"
    );
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.controller == P0));
}

/// Pins the co-departure reading. On the concession path the look-back latch
/// already keeps the departing Demon on the battlefield; on the state-based
/// action path it is the simultaneity stamp (CR 603.10a) that lets it see the
/// others.
#[test]
fn concession_departing_observer_sees_the_other_permanents_that_leave_with_it() {
    assert_departing_observer_sees_the_others(Departure::Concession);
}

/// REVERT-FAILING: without dropping triggers a departed player would control at
/// collection, P2's own Demon goes on the stack and the game waits on P2 to
/// choose its target.
#[test]
fn losing_to_zero_life_departing_observer_sees_the_other_permanents_that_leave_with_it() {
    assert_departing_observer_sees_the_others(Departure::ZeroLife);
}

/// CR 704.5a + CR 800.4a + CR 603.6c + CR 603.7: delayed abilities on a
/// permanent whose owner loses to a state-based action. "When it leaves the
/// battlefield" sees it leave the game (the reach guard); "when it dies or is
/// exiled" doesn't, since it did neither. No ordinary trigger opens a prompt on
/// this board. A delayed leaves-the-battlefield ability P2 controls triggers
/// too, but isn't put on the stack (CR 800.4d).
///
/// REVERT-FAILING: without the departure gate on the delayed matchers, the
/// "dies or is exiled" ability triggers; without retiring departed-controller
/// delayed contexts, P2's delayed ability goes on the stack.
#[test]
fn losing_to_zero_life_delayed_abilities_read_the_departure_as_leaving_the_game() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let leaves = scenario
        .add_creature(P0, "Leaves Marker", 0, 0)
        .as_enchantment()
        .id();
    let dies_or_exiled = scenario
        .add_creature(P0, "Dies Or Exiled Marker", 0, 0)
        .as_enchantment()
        .id();
    let p2_delayed_source = scenario
        .add_creature(P1, "P2 Delayed Source", 0, 0)
        .as_enchantment()
        .id();
    scenario.with_life(P2, 0);
    let mut runner = scenario.build();
    install_delayed_gain_life_for(
        &mut runner,
        P2,
        p2_delayed_source,
        DelayedTriggerCondition::WhenLeavesPlay { object_id: bear },
    );
    install_delayed_gain_life(
        &mut runner,
        leaves,
        DelayedTriggerCondition::WhenLeavesPlay { object_id: bear },
    );
    install_delayed_gain_life(
        &mut runner,
        dies_or_exiled,
        DelayedTriggerCondition::WhenDiesOrExiled {
            filter: TargetFilter::SpecificObject { id: bear },
        },
    );

    runner
        .act(GameAction::PassPriority)
        .expect("state-based actions run");
    assert!(runner.state().players[2].is_eliminated);
    drive_trigger_prompts(&mut runner);
    assert_delayed_departure_triggers(&runner, leaves, dies_or_exiled);
    assert_eq!(
        triggers_from(&runner, p2_delayed_source),
        0,
        "CR 800.4d: a delayed ability the departed player would control isn't put on the stack"
    );
}

/// CR 800.4a + CR 603.6c + CR 702.26k: the resolved-rules journal records each
/// owned permanent's leave-the-game move with its departure, so a serialized
/// replay of the concession installs the same records as live play: `LeftGame`
/// for a phased-in permanent, `LeftGamePhasedOut` for a phased-out one, and
/// `Moved` for the permanent the leaver controlled but didn't own (it is exiled).
///
/// REVERT-FAILING: stamping the departure after the move journals it leaves the
/// recorded commands, and so the replayed records, at `Moved`.
#[test]
fn concession_journal_records_and_replays_the_departure() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let phased_in = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let phased_out = scenario.add_creature(P2, "P2 Phased Bear", 2, 2).id();
    let borrowed = scenario
        .add_creature(P1, "P1 Bear", 2, 2)
        .controlled_by(P2)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&phased_out)
        .expect("the bear is on the battlefield")
        .phase_status = PhaseStatus::PhasedOut {
        cause: PhaseOutCause::Directly,
    };
    let before = runner.state().clone();
    let journal_start = before.resolved_rules_journal.entries().len();

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");

    let commands: Vec<ResolvedZoneChangeCommand> = runner
        .state()
        .resolved_rules_journal
        .entries_since(journal_start)
        .into_iter()
        .filter_map(|entry| match entry.command {
            Some(ResolvedRulesCommand::ZoneChange(command)) => Some(*command),
            _ => None,
        })
        .collect();
    let mut replayed = before.clone();
    for command in &commands {
        let wire = serde_json::to_string(command).expect("the command serializes");
        let decoded: ResolvedZoneChangeCommand =
            serde_json::from_str(&wire).expect("the command deserializes");
        apply_resolved_zone_change(&mut replayed, &decoded).expect("the command replays");
    }

    for (id, expected) in [
        (phased_in, ZoneDeparture::LeftGame),
        (phased_out, ZoneDeparture::LeftGamePhasedOut),
        (borrowed, ZoneDeparture::Moved),
    ] {
        let live = runner
            .state()
            .zone_changes_this_turn
            .iter()
            .rev()
            .find(|record| record.object_id == id)
            .expect("the live move is recorded");
        assert_eq!(live.departure, expected, "{id:?}: live record");
        let replayed_record = replayed
            .zone_changes_this_turn
            .iter()
            .rev()
            .find(|record| record.object_id == id)
            .expect("the replayed move is recorded");
        assert_eq!(
            replayed_record.departure, expected,
            "{id:?}: the replayed record matches live play"
        );
    }
}

const ACT_OF_AGGRESSION_ORACLE: &str = "Gain control of target creature an opponent controls \
     until end of turn. Untap that creature. It gains haste until end of turn.";

/// CR 603.10a + CR 603.3a + CR 800.4a + CR 800.4d: P2 steals P0's Extractor
/// Demon with a real control-changing spell, then concedes while owning a bear.
/// The bear leaves the game in the same event that ends P2's control effects
/// (CR 800.4a), and the stolen Demon's leaves-the-battlefield ability looks back
/// to its source as it was immediately before that event, which P2 controlled:
/// P2 controls that trigger, and it isn't put on the stack. Control then
/// reverts and the Demon stays with P0, which puts no trigger of its own on the
/// stack for the bear. P0's other Demon is the reach guard: it sees the bear
/// leave and triggers under P0.
///
/// REVERT-FAILING: collecting the departure against the post-revert board gives
/// the stolen Demon's trigger to P0.
#[test]
fn concession_after_stealing_an_observer_retires_the_stolen_observers_trigger() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let stolen_demon = scenario
        .add_creature_from_oracle(P0, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE)
        .id();
    let p0_demon = scenario
        .add_creature_from_oracle(P0, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE)
        .id();
    let bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let aggression = scenario
        .add_spell_to_hand_from_oracle(P2, "Act of Aggression", true, ACT_OF_AGGRESSION_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    // P0 and P1 pass; P2 receives priority and steals the Demon.
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes to P2");
    }
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P2 }
    );
    runner
        .cast(aggression)
        .target_object(stolen_demon)
        .resolve();
    // Reach guard: the control effect is live.
    assert_eq!(runner.state().objects[&stolen_demon].controller, P2);
    assert!(runner.state().stack.is_empty());

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert_eq!(zone(&runner, bear), Zone::Exile);
    assert_eq!(
        runner.state().objects[&stolen_demon].controller,
        P0,
        "CR 800.4a: the control effect ended and the Demon reverted to P0"
    );
    assert_eq!(zone(&runner, stolen_demon), Zone::Battlefield);

    drive_trigger_prompts(&mut runner);
    assert_eq!(
        triggers_from(&runner, p0_demon),
        1,
        "CR 603.6c: P0's own Demon sees the bear leave the game"
    );
    assert_eq!(
        triggers_from(&runner, stolen_demon),
        0,
        "CR 603.3a + CR 800.4d: the stolen Demon's trigger was P2's, and P2 left"
    );
}

const BECOME_MONARCH_ORACLE: &str = "Whenever you become the monarch, you gain 1 life.";

/// CR 603.10 + CR 725.4: only the departure is looked back on. When the monarch
/// concedes, the active player becomes the monarch as the conceding player
/// leaves, and a "whenever you become the monarch" ability is checked against
/// the game immediately after: a creature P2 owned already left with P2, so it
/// doesn't trigger, though P0 controlled it.
///
/// Pins that the look-back latch reads only the departure, not the rest of the
/// player-leaves-game events.
#[test]
fn concession_monarch_observer_that_left_with_its_owner_does_not_trigger() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario
        .add_creature_from_oracle(P2, "Monarch Watcher", 2, 2, BECOME_MONARCH_ORACLE)
        .controlled_by(P0)
        .id();
    let own_watcher = scenario
        .add_creature_from_oracle(P0, "P0 Monarch Watcher", 2, 2, BECOME_MONARCH_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().monarch = Some(P2);

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    // Reach guard: the monarchy passed to P0 and the watcher left the game.
    assert_eq!(runner.state().monarch, Some(P0));
    assert_eq!(zone(&runner, watcher), Zone::Exile);

    drive_trigger_prompts(&mut runner);
    assert_eq!(
        triggers_from(&runner, own_watcher),
        1,
        "reach guard: P0's own watcher sees P0 become the monarch"
    );
    assert_eq!(triggers_from(&runner, watcher), 0);
}

/// CR 603.10 + CR 725.4 + CR 800.4a: P2 steals P0's "whenever you become the
/// monarch" creature with a real control-changing spell, is the monarch, and
/// concedes. Immediately after, P0 controls the creature again and is the
/// monarch, so its ability triggers under P0.
///
/// Pins that the look-back latch reads only the departure: read against the
/// latched observers, the creature would still be P2's and be retired with P2.
#[test]
fn concession_monarch_observer_that_reverted_to_the_new_monarch_triggers() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario
        .add_creature_from_oracle(P0, "Monarch Watcher", 2, 2, BECOME_MONARCH_ORACLE)
        .id();
    let aggression = scenario
        .add_spell_to_hand_from_oracle(P2, "Act of Aggression", true, ACT_OF_AGGRESSION_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes to P2");
    }
    runner.cast(aggression).target_object(watcher).resolve();
    assert_eq!(runner.state().objects[&watcher].controller, P2);
    runner.state_mut().monarch = Some(P2);

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert_eq!(runner.state().monarch, Some(P0));
    assert_eq!(runner.state().objects[&watcher].controller, P0);

    drive_trigger_prompts(&mut runner);
    assert_eq!(
        triggers_from(&runner, watcher),
        1,
        "CR 725.4: P0 controls the watcher and became the monarch"
    );
}

const HARSH_MERCY_ORACLE: &str = "Each player chooses a creature type. Destroy all creatures that aren't of a type chosen this way. They can't be regenerated.";
const EXILE_FROM_ANYWHERE_ORACLE: &str =
    "Whenever a card is put into exile from anywhere, you gain 1 life.";

/// CR 800.4a + CR 608.2n: P0's Harsh Mercy is paused on P0's own creature-type
/// choice when P0 concedes. The spell leaves the game with its owner; it is not
/// exiled, so P1's "put into exile from anywhere" watcher doesn't see it. The
/// reach guard is the creature P1 owns and P0 controlled, which is exiled once
/// P0's control ends and which the watcher does see.
///
/// REVERT-FAILING: moving the paused spell without its leave-the-game departure
/// reports it as exiled and the watcher triggers for it too.
#[test]
fn concession_with_a_paused_spell_does_not_exile_it() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mercy = scenario
        .add_spell_to_hand_from_oracle(P0, "Harsh Mercy", false, HARSH_MERCY_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let watcher = scenario
        .add_creature_from_oracle(P1, "Exile Watcher", 1, 1, EXILE_FROM_ANYWHERE_ORACLE)
        .id();
    let borrowed = scenario
        .add_creature(P1, "P1 Bear", 2, 2)
        .controlled_by(P0)
        .id();
    let mut runner = scenario.build();

    runner.cast(mercy).commit();
    for _ in 0..12 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Harsh Mercy");
    }
    // Reach guard: the spell is resolving, paused on a player's choice.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Harsh Mercy pauses on a creature-type choice: {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    assert_eq!(zone(&runner, mercy), Zone::Exile);
    assert_eq!(zone(&runner, borrowed), Zone::Exile);

    drive_trigger_prompts(&mut runner);
    assert_eq!(
        triggers_from(&runner, watcher),
        1,
        "CR 800.4a: only the bear P0 controlled was exiled; Harsh Mercy left the game"
    );
}

const WORD_OF_SEIZING_ORACLE: &str =
    "Untap target permanent and gain control of it until end of turn.";
const GRAFTED_WARGEAR_ORACLE: &str = "Equipped creature gets +3/+2.\n\
     Whenever this Equipment becomes unattached from a permanent, sacrifice that permanent.\n\
     Equip {0}";

/// CR 603.10c + CR 603.3a + CR 800.4a + CR 800.4d: on P2's turn, P2 steals P0's
/// Grafted Wargear with Word of Seizing, equips it to P2's bear, and concedes.
/// The bear leaves the game and the Wargear becomes unattached in the same
/// event that ends P2's control effects. "Becomes unattached" looks back in
/// time, so the ability is P2's, the Wargear's controller immediately before,
/// and it isn't put on the stack; the Wargear then reverts to P0.
///
/// REVERT-FAILING: reading the departure's `Unattached` event against the
/// post-revert board gives the Wargear's trigger to P0.
#[test]
fn concession_after_stealing_equipment_retires_its_unattach_trigger() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Forest", "Forest", "Forest"]);
    }
    let wargear = scenario
        .add_artifact_from_oracle(P0, "Grafted Wargear", GRAFTED_WARGEAR_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let bear = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let seizing = scenario
        .add_spell_to_hand_from_oracle(P2, "Word of Seizing", true, WORD_OF_SEIZING_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    // Advance to P2's main phase (P0, then P1, then P2 in turn order).
    for _ in 0..2 {
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    assert_eq!(runner.state().active_player, P2, "reach guard: P2's turn");

    runner.cast(seizing).target_object(wargear).resolve();
    assert_eq!(runner.state().objects[&wargear].controller, P2);
    let equip = runner.state().objects[&wargear]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|description| description.contains("Equip"))
        })
        .expect("Grafted Wargear carries an Equip ability");
    runner
        .activate(wargear, equip)
        .target_object(bear)
        .resolve();
    // Reach guard: P2's Wargear is attached to P2's bear.
    assert_eq!(
        runner.state().objects[&wargear].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(bear))
    );
    assert!(runner.state().stack.is_empty());

    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert_eq!(zone(&runner, bear), Zone::Exile);
    assert_eq!(
        runner.state().objects[&wargear].attached_to,
        None,
        "the Wargear became unattached"
    );
    assert_eq!(
        runner.state().objects[&wargear].controller,
        P0,
        "CR 800.4a: the control effect ended and the Wargear reverted to P0"
    );

    drive_trigger_prompts(&mut runner);
    assert_eq!(
        triggers_from(&runner, wargear),
        0,
        "CR 603.10c + CR 800.4d: the unattach trigger was P2's, and P2 left"
    );
    assert!(runner.state().deferred_triggers.is_empty());
}
