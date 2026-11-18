//! Issue #774: proliferate with Kilo, Apogee Mind and Dreamtide Whale. CR 701.34a lets
//! proliferate choose any permanent "that has a counter", and a time counter is a counter
//! (CR 122.1), so the Whale's Vanishing counters (CR 702.63a) make it a proliferate choice.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DREAMTIDE_WHALE: &str = "Vanishing 2 (This creature enters with two time counters on it. At the beginning of your upkeep, remove a time counter from it. When the last is removed, sacrifice it.)\nWhenever a player casts their second spell each turn, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)";

const KILO: &str = "Haste\nWhenever Kilo becomes tapped, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)";

fn time_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Time)
        .copied()
        .unwrap_or(0)
}

/// Kilo attacks (CR 508.1f taps it), its "becomes tapped" trigger proliferates, and the
/// Whale, which entered with two time counters, is offered and gets a third.
#[test]
fn kilo_tapping_proliferates_dreamtide_whales_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kilo = scenario
        .add_creature_from_oracle(P0, "Kilo, Apogee Mind", 3, 3, KILO)
        .id();
    // MTGJSON lists the Whale's keyword as "Vanishing"; the hint lets the parser read
    // the reminder-texted keyword line the way the card-data build does.
    let whale = scenario
        .add_creature_to_hand(P0, "Dreamtide Whale", 7, 5)
        .from_oracle_text_with_keywords(&["Vanishing"], DREAMTIDE_WHALE)
        .id();
    let mut runner = scenario.build();

    runner.cast(whale).resolve();
    assert_eq!(runner.state().objects[&whale].zone, Zone::Battlefield);
    assert_eq!(
        time_counters(&runner, whale),
        2,
        "CR 702.63a: Vanishing 2 enters with two time counters"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kilo, AttackTarget::Player(P1))])
        .expect("Kilo has haste and can attack");
    runner.resolve_top();

    let WaitingFor::ProliferateChoice { player, eligible } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Kilo's tap trigger must ask for the proliferate choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        eligible.contains(&TargetRef::Object(whale)),
        "CR 701.34a + CR 122.1: the Whale has time counters, so it is a proliferate choice; \
         eligible was {eligible:?}"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(whale)],
        })
        .expect("choosing the Whale is legal");
    assert_eq!(
        time_counters(&runner, whale),
        3,
        "CR 701.34a: proliferate gives the Whale another time counter"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the proliferate choice is done and P0 has priority again, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "Kilo's trigger has finished resolving"
    );
}

/// The Whale's own trigger: the second spell a player casts in a turn makes it proliferate,
/// and the Whale itself, with its two time counters, is offered and gets a third.
#[test]
fn second_spell_trigger_proliferates_dreamtide_whales_own_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let whale = scenario
        .add_creature_to_hand(P0, "Dreamtide Whale", 7, 5)
        .from_oracle_text_with_keywords(&["Vanishing"], DREAMTIDE_WHALE)
        .id();
    let second = scenario
        .add_creature_to_hand_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(whale).resolve();
    assert_eq!(time_counters(&runner, whale), 2);

    runner.cast(second).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the Whale's trigger goes on the stack above the second spell"
    );
    runner.resolve_top();

    let WaitingFor::ProliferateChoice { player, eligible } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Whale's trigger must ask for the proliferate choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        eligible.contains(&TargetRef::Object(whale)),
        "CR 701.34a + CR 122.1: the Whale's own time counters make it a proliferate choice; \
         eligible was {eligible:?}"
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(whale)],
        })
        .expect("choosing the Whale is legal");
    assert_eq!(time_counters(&runner, whale), 3);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the proliferate choice is done and P0 has priority again, got {:?}",
        runner.state().waiting_for
    );
    let stack: Vec<ObjectId> = runner.state().stack.iter().map(|entry| entry.id).collect();
    assert_eq!(
        stack,
        vec![second],
        "the Whale's trigger has finished resolving; only the second spell remains"
    );
}
