//! Regression for Mauhur, Uruk-hai Captain with Swarming of Moria.
//!
//! CR 701.47a: amass Orcs puts counters on the chosen Army. Mauhur does not
//! move those counters to itself; it increases the number put on that Army.

use engine::game::effects::counters::add_counter_with_replacement;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const MAUHUR: &str = "Menace\n\
If one or more +1/+1 counters would be put on an Army, Goblin, or Orc you control, \
that many plus one +1/+1 counters are put on it instead.";

const SWARMING_OF_MORIA: &str = "Create a Treasure token.\nAmass Orcs 2.";

fn plus_one_counters(runner: &GameRunner, object_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object_id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn controlled_army(runner: &GameRunner) -> Option<ObjectId> {
    runner.state().battlefield.iter().copied().find(|id| {
        runner.state().objects.get(id).is_some_and(|obj| {
            obj.controller == P0 && obj.card_types.subtypes.iter().any(|s| s == "Army")
        })
    })
}

#[test]
fn swarming_of_moria_puts_mauhurs_extra_counter_on_the_army() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mauhur = scenario
        .add_creature_from_oracle(P0, "Mauhur, Uruk-hai Captain", 2, 2, MAUHUR)
        .with_subtypes(vec!["Orc", "Soldier"])
        .id();
    let swarming = scenario
        .add_spell_to_hand_from_oracle(P0, "Swarming of Moria", false, SWARMING_OF_MORIA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(swarming).resolve();

    let army = controlled_army(&runner).expect("Swarming of Moria should create an Army");
    assert_eq!(
        plus_one_counters(&runner, army),
        3,
        "amass Orcs 2 should put 3 counters on the Army while Mauhur is controlled"
    );
    assert_eq!(
        plus_one_counters(&runner, mauhur),
        0,
        "Mauhur modifies the Army's counter event; it does not receive those counters"
    );
}

#[test]
fn mauhur_does_not_add_counters_to_unlisted_creature_types() {
    let mut scenario = GameScenario::new();
    let _mauhur = scenario
        .add_creature_from_oracle(P0, "Mauhur, Uruk-hai Captain", 2, 2, MAUHUR)
        .with_subtypes(vec!["Orc", "Soldier"])
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    assert!(add_counter_with_replacement(
        runner.state_mut(),
        P0,
        bear,
        CounterType::Plus1Plus1,
        1,
        &mut events,
    ));

    assert_eq!(
        plus_one_counters(&runner, bear),
        1,
        "Mauhur only modifies counters put on Armies, Goblins, and Orcs you control"
    );
}
