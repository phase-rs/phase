//! Regression for issue #3265: Knowledge Seeker's "second card each turn" trigger
//! must fire when the second draw comes from Kwain's per-player optional draw
//! during ability resolution — even while the next player's "may draw" prompt
//! is still open.
//!
//! https://github.com/phase-rs/phase/issues/3265

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

const KNOWLEDGE_SEEKER_ORACLE: &str = "Vigilance\n\
Whenever you draw your second card each turn, put a +1/+1 counter on this creature.\n\
When this creature dies, create a Clue token.";

const KWAIN_ORACLE: &str =
    "{T}: Each player may draw a card, then each player who drew a card this way gains 1 life.";

fn plus_one_counters(runner: &GameRunner, id: engine::types::identifiers::ObjectId) -> Option<u32> {
    runner
        .state()
        .objects
        .get(&id)?
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
}

#[test]
fn issue_3265_knowledge_seeker_triggers_on_kwain_second_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for _ in 0..4 {
        scenario.add_card_to_library_top(P0, "Island");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let knowledge_seeker = scenario
        .add_creature(P0, "Knowledge Seeker", 2, 1)
        .from_oracle_text(KNOWLEDGE_SEEKER_ORACLE)
        .id();

    let kwain = scenario
        .add_creature(P0, "Kwain, Itinerant Meddler", 1, 3)
        .from_oracle_text(KWAIN_ORACLE)
        .id();

    let mut runner = scenario.build();

    // CR 504.1: simulate the draw step's mandatory first draw already having happened.
    runner.state_mut().players[0].cards_drawn_this_turn = 1;

    runner.activate(kwain, 0).accept_optional().resolve();

    assert_eq!(
        plus_one_counters(&runner, knowledge_seeker),
        Some(1),
        "Knowledge Seeker must receive a +1/+1 counter when P0 draws their \
         second card of the turn via Kwain (CR 603.2 / CR 121.2)"
    );

    // Sanity: P1 was also offered Kwain's draw; with accept_optional both players draw.
    assert!(
        !runner.state().players[1].hand.is_empty(),
        "P1 should have been offered and accepted Kwain's optional draw"
    );
}
