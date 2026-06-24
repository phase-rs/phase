//! Regression for issue #3654: Nyxbloom Ancient and Mana Reflection must
//! stack multiplicatively (2× × 3× = 6×) when both are on the battlefield.
//!
//! https://github.com/phase-rs/phase/issues/3654

use crate::support::shared_card_db as load_db;
use engine::game::mana_payment::produce_mana;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::mana::ManaType;
use engine::types::zones::Zone;

/// CR 106.12b + CR 616.1: Tapping a land with both Mana Reflection (×2) and
/// Nyxbloom Ancient (×3) on the battlefield produces six mana of the tapped type.
#[test]
fn nyxbloom_and_mana_reflection_stack_to_six_mana() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let _reflection = scenario.add_real_card(P0, "Mana Reflection", Zone::Battlefield, db);
    let _nyxbloom = scenario.add_real_card(P0, "Nyxbloom Ancient", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let reflection_obj = state
        .objects
        .values()
        .find(|o| o.name == "Mana Reflection")
        .expect("Mana Reflection on battlefield");
    assert!(
        !reflection_obj.replacement_definitions.is_empty(),
        "Mana Reflection must carry ProduceMana replacements"
    );

    let mut events = Vec::new();
    produce_mana(state, forest, ManaType::Green, P0, true, &mut events);

    assert_eq!(
        state.players[0].mana_pool.count_color(ManaType::Green),
        6,
        "Mana Reflection (×2) and Nyxbloom Ancient (×3) should produce 6 green mana"
    );
}
