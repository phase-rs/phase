//! Regression for issue #3652: Together as One must draw, deal damage, and
//! gain life equal to the number of colors of mana spent to cast it.
//!
//! https://github.com/phase-rs/phase/issues/3652

use crate::support::shared_card_db as load_db;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

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

/// CR 207.2c + CR 601.2h: Casting Together as One with five colors of mana
/// must draw 5 cards for the chosen player, deal 5 damage, and gain 5 life.
#[test]
fn together_as_one_converge_five_colors_draws_deals_and_gains_life() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario.add_real_card(P0, "Together as One", Zone::Hand, db);
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Filler");
        scenario.add_card_to_library_top(P1, "Filler");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Pay {6} with WUBRG + colorless — five distinct colors spent.
    add_mana(
        &mut runner,
        &[
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
            ManaType::Colorless,
        ],
    );

    let life_before = runner.state().players[0].life;
    let hand_before = runner.state().players[0].hand.len();
    let p1_hand_before = runner.state().players[1].hand.len();
    let opp_life_before = runner.state().players[1].life;

    let commit = runner
        .cast(spell)
        .target_player(P1)
        .target_player(P1)
        .commit();

    let colors_on_stack = commit
        .state()
        .stack
        .iter()
        .find_map(|entry| commit.state().objects.get(&entry.id))
        .map(|obj| obj.colors_spent_to_cast.distinct_colors())
        .unwrap_or(0);
    assert_eq!(
        colors_on_stack, 5,
        "spell on stack should record five distinct colors spent to cast"
    );

    let outcome = commit.resolve();

    let hand_after = outcome.state().players[0].hand.len();
    let life_after = outcome.state().players[0].life;
    let p1_hand_after = outcome.state().players[1].hand.len();
    let opp_life_after = outcome.state().players[1].life;

    // Controller casts (-1) but does not draw — the chosen target player does.
    assert_eq!(
        hand_after as i32 - hand_before as i32,
        -1,
        "caster's hand should only lose the cast spell"
    );
    assert_eq!(
        p1_hand_after as i32 - p1_hand_before as i32,
        5,
        "target player should draw X=5 cards at converge 5"
    );
    assert_eq!(
        life_after - life_before,
        5,
        "Together as One should gain X=5 life"
    );
    assert_eq!(
        opp_life_before - opp_life_after,
        5,
        "Together as One should deal X=5 damage to the chosen target"
    );
}
