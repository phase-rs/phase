//! Issue #1526 — Harvest Season must allow selecting up to X basic lands where
//! X equals the number of tapped creatures you control.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const HARVEST_SEASON: &str = "Search your library for up to X basic land cards, where X is \
the number of tapped creatures you control, put those cards onto the battlefield tapped, then shuffle.";

fn add_harvest_mana(runner: &mut engine::game::scenario::GameRunner) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    pool.add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    for _ in 0..2 {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

#[test]
fn issue_1526_harvest_season_allows_up_to_tapped_creature_count() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let harvest = scenario
        .add_spell_to_hand_from_oracle(P0, "Harvest Season", false, HARVEST_SEASON)
        .id();

    let mut tapped_creature_ids = Vec::new();
    for _ in 0..6 {
        tapped_creature_ids.push(scenario.add_creature(P0, "Grizzly Bears", 2, 2).id());
    }

    let mut land_ids = Vec::new();
    for _ in 0..6 {
        land_ids.push(scenario.add_real_card(P0, "Forest", Zone::Library, db));
    }
    scenario.add_real_card(P0, "Island", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    for id in tapped_creature_ids {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    add_harvest_mana(&mut runner);

    let outcome = runner.cast(harvest).resolve();

    let WaitingFor::SearchChoice {
        cards,
        count,
        up_to,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "expected SearchChoice after casting Harvest Season, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(up_to, "Harvest Season search must be up-to");
    assert_eq!(
        *count, 6,
        "with six tapped creatures, Harvest Season must allow selecting up to six lands"
    );
    for land in &land_ids {
        assert!(cards.contains(land), "library basics must be searchable");
    }

    runner
        .act(GameAction::SelectCards {
            cards: land_ids.clone(),
        })
        .expect("select all six basics");

    runner.advance_until_stack_empty();

    for land in &land_ids {
        let obj = runner.state().objects.get(land).expect("land must exist");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "selected lands must enter play"
        );
        assert!(
            obj.tapped,
            "Harvest Season puts lands onto the battlefield tapped"
        );
    }
}
