//! Regression for #7063: a card repositioned within a library is not a zone
//! change, while a different card put into that library still is.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DUTIFUL_KNOWLEDGE_SEEKER: &str =
    "Whenever one or more cards are put into a player's library from anywhere, put a +1/+1 counter on Dutiful Knowledge Seeker.";
const TIME_EBB: &str = "Put target creature on top of its owner's library.";

#[test]
fn time_ebb_triggers_dutiful_knowledge_seeker_for_a_distinct_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let dutiful = scenario
        .add_creature_from_oracle(
            P1,
            "Dutiful Knowledge Seeker",
            2,
            2,
            DUTIFUL_KNOWLEDGE_SEEKER,
        )
        .id();
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let time_ebb = scenario
        .add_spell_to_hand_from_oracle(P0, "Time Ebb", false, TIME_EBB)
        .with_mana_cost(ManaCost::zero())
        .id();

    let outcome = scenario
        .build()
        .cast(time_ebb)
        .target_object(victim)
        .resolve();
    let state = outcome.state();

    assert_eq!(state.objects[&victim].zone, Zone::Library);
    assert_eq!(state.players[1].library.front(), Some(&victim));
    assert_eq!(state.objects[&dutiful].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&dutiful]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "the distinct creature's move into the library must trigger Dutiful Knowledge Seeker"
    );
}
