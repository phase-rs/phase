//! Regression for issue #2864: Kaldra Compleat's equipped creature must gain
//! haste along with the other granted keywords from the equipment static.
//!
//! https://github.com/phase-rs/phase/issues/2864

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn issue_2864_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_2864_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_2864_cards.json fixture must load")
    })
}

#[test]
fn issue_2864_kaldra_compleat_equipped_creature_gains_haste() {
    let db = issue_2864_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kaldra = scenario.add_real_card(P0, "Kaldra Compleat", Zone::Battlefield, db);
    let bearer = scenario.add_creature(P0, "Bearer", 1, 1).id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    attach_to(runner.state_mut(), kaldra, bearer);
    evaluate_layers(runner.state_mut());

    let bearer_obj = runner.state().objects.get(&bearer).unwrap();
    assert_eq!(bearer_obj.power, Some(6));
    assert_eq!(bearer_obj.toughness, Some(6));
    for kw in [
        Keyword::FirstStrike,
        Keyword::Trample,
        Keyword::Indestructible,
        Keyword::Haste,
    ] {
        assert!(
            bearer_obj.has_keyword(&kw),
            "equipped bearer missing keyword {:?}; keywords={:?}",
            kw,
            bearer_obj.keywords
        );
    }
}
