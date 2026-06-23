//! Regression for issue #3991: Carnage Interpreter must immediately become 5/5
//! with menace after its ETB discard empties the controller's hand.
//!
//! https://github.com/phase-rs/phase/issues/3991

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn hybrid_mana() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ]
}

#[test]
fn carnage_interpreter_gets_plus_two_plus_two_after_etb_discard() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Rest A", "Rest B", "Rest C"] {
        scenario.add_card_to_hand(P0, name);
    }
    let carnage = scenario.add_real_card(P0, "Carnage Interpreter", Zone::Hand, db);
    scenario.with_mana_pool(P0, hybrid_mana());

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.cast(carnage).resolve();

    let carnage_obj = &runner.state().objects[&carnage];
    assert_eq!(
        carnage_obj.zone,
        Zone::Battlefield,
        "Carnage Interpreter must resolve onto the battlefield"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "ETB discard must empty the controller's hand"
    );
    assert_eq!(
        carnage_obj.power,
        Some(5),
        "Carnage Interpreter must be 5/5 with an empty hand (+2/+2 over 3/3)"
    );
    assert_eq!(carnage_obj.toughness, Some(5));
    assert!(
        carnage_obj.keywords.contains(&Keyword::Menace),
        "Carnage Interpreter must have menace with one or fewer cards in hand"
    );
}
