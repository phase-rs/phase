//! Issue #1202 — Prepared copies must pay the prepare spell's mana cost and obey
//! sorcery timing (not instant speed on the opponent's turn).

use engine::game::effects::prepare::can_cast_prepared_copy_now;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn issue_1202_prepared_sorcery_requires_mana_and_sorcery_timing() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emeritus = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let back = runner
        .state()
        .objects
        .get(&emeritus)
        .and_then(|o| o.back_face.clone())
        .expect("Emeritus must hydrate a prepare spell back face");
    assert!(
        !matches!(back.mana_cost, engine::types::mana::ManaCost::NoCost),
        "prepare spell back face must carry a payable mana cost, got {:?}",
        back.mana_cost
    );

    runner
        .act(GameAction::Debug(
            engine::types::actions::DebugAction::SetPrepared {
                object_id: emeritus,
                prepared: true,
            },
        ))
        .expect("mark Emeritus prepared");

    assert!(
        !can_cast_prepared_copy_now(runner.state(), P0, emeritus),
        "prepared sorcery must not be castable without mana in pool"
    );

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P0;
    assert!(
        !can_cast_prepared_copy_now(runner.state(), P0, emeritus),
        "prepared sorcery must not be castable during the opponent's turn"
    );
}
