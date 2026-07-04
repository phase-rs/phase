//! Regression for issue #3316: Stinging Study must correctly resolve X
//! as the mana value of a commander you own on the battlefield or in the command zone.
//!
//! https://github.com/phase-rs/phase/issues/3316
//!
//! Casts real Stinging Study card data with a known-mana-value commander in the
//! command zone and asserts that the controller draws X cards and loses X life.
//! This exercises the full parser → cast → resolve pipeline so a regression in
//! any of those layers will cause this test to fail.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    static DB: std::sync::OnceLock<Option<CardDatabase>> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_3316_cards.json");
        CardDatabase::from_export(&path)
            .expect("issue_3316_cards.json fixture must load")
            .into()
    })
    .as_ref()
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Stinging Study draws X cards and loses X life where X = commander's mana value.
///
/// This test verifies the full cast→resolve pipeline: the real Stinging Study
/// ability (loaded from a committed fixture with `CommanderManaValue` quantities)
/// is cast with a 4-mana commander in the command zone and the outcomes are
/// asserted.
#[test]
fn stinging_study_cast_draws_cards_and_loses_life_equal_to_commander_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // 4-mana commander in the command zone — Stinging Study resolves X = 4.
    let commander_id = scenario
        .add_creature(P0, "Test Commander", 3, 3)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    scenario.with_commander(commander_id);

    // Real Stinging Study ({4}{B}) from the fixture — uses CommanderManaValue.
    let stinging_study = scenario.add_real_card(P0, "Stinging Study", Zone::Hand, db);

    // Stock P0's library so there are cards to draw (X=4).
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Filler Card");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Provide {4}{B} to pay the casting cost.
    add_mana(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
        ],
    );

    let life_before = runner.state().players[0].life;
    let hand_before = runner.state().players[0].hand.len() as i32;

    let outcome = runner.cast(stinging_study).resolve();

    // After casting: Stinging Study leaves hand (-1) and X=4 cards drawn (+4).
    // Net change = +3. X = commander's mana value = 4.
    let hand_after = outcome.state().players[0].hand.len() as i32;
    assert_eq!(
        hand_after - hand_before,
        3, // net: -1 (spell cast) + 4 (drawn) = +3
        "Stinging Study should net draw X-1=3 cards (spell leaves hand + draw X=4)"
    );

    let life_after = outcome.state().players[0].life;
    assert_eq!(
        life_before - life_after,
        4,
        "Stinging Study should lose X=4 life (commander's mana value)"
    );
}
