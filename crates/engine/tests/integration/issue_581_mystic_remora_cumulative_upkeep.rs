//! Regression (issue #581): `add_real_card` must register Mystic Remora's
//! synthesized cumulative-upkeep trigger in the derived `TriggerIndex` without
//! waiting for a later `flush_layers` rebuild.

use engine::game::scenario::GameScenario;
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::trigger_index::candidates_for_event;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const P0: PlayerId = PlayerId(0);

fn assert_has_cumulative_upkeep_trigger(state: &GameState, remora: ObjectId) {
    assert!(
        state
            .objects
            .get(&remora)
            .unwrap()
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|t| matches!(t.definition.mode, TriggerMode::PayCumulativeUpkeep)),
        "Mystic Remora must carry a synthesized cumulative-upkeep trigger"
    );
}

fn upkeep_phase_candidates(state: &GameState) -> Vec<ObjectId> {
    candidates_for_event(
        state,
        &GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        },
    )
    .into_iter()
    .collect()
}

#[test]
fn add_real_card_registers_upkeep_trigger_before_layer_flush() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let remora = scenario.add_real_card(P0, "Mystic Remora", Zone::Battlefield, db);
    let runner = scenario.build();

    assert_has_cumulative_upkeep_trigger(runner.state(), remora);
    assert!(
        upkeep_phase_candidates(runner.state()).contains(&remora),
        "add_real_card must re-index synthesized upkeep triggers immediately; \
         consult must not depend on a later flush_layers rebuild (issue #581)"
    );
}
