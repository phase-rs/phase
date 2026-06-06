use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_white_mana(runner: &mut engine::game::scenario::GameRunner) {
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::White,
        ObjectId(0),
        false,
        vec![],
    ));
}

#[test]
fn enlightened_tutor_search_shuffle_puts_found_card_on_top() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tutor = scenario.add_real_card(P0, "Enlightened Tutor", Zone::Hand, db);
    let sol_ring = scenario.add_real_card(P0, "Sol Ring", Zone::Library, db);
    scenario.add_real_card(P0, "Forest", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_white_mana(&mut runner);

    // The cast driver resolves the tutor and stops at the SearchChoice boundary
    // (default SearchPolicy::Stop), exposing it via `final_waiting_for()`.
    let outcome = runner.cast(tutor).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(
                cards.contains(&sol_ring),
                "Sol Ring should be a legal Enlightened Tutor search choice"
            );
        }
        other => panic!("expected SearchChoice after Enlightened Tutor resolves, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![sol_ring],
        })
        .expect("selecting Sol Ring should resolve the tutor continuation");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "Enlightened Tutor must not ask for a separate hand/library card to put back"
    );
    assert_eq!(runner.state().objects[&sol_ring].zone, Zone::Library);
    assert_eq!(
        runner.state().players[0].library[0],
        sol_ring,
        "selected search card should be on top after shuffle"
    );
}
