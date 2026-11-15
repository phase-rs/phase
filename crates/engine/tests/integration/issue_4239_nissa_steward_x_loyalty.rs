//! Issue #4239 — Nissa, Steward of Elements enters with the announced X loyalty.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::card::PrintedLoyalty;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const NISSA: &str = "Nissa, Steward of Elements";

#[test]
fn nissa_steward_enters_with_announced_x_loyalty() {
    let Some(db) = load_db() else {
        return;
    };

    let face = db
        .get_face_by_name(NISSA)
        .expect("Nissa must be in fixture");
    assert_eq!(face.loyalty.as_deref(), Some("X"));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nissa = scenario.add_real_card(P0, NISSA, Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, Vec::new()),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, Vec::new()),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, Vec::new()),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let outcome = runner.cast(nissa).x(3).resolve();

    assert_eq!(outcome.zone_of(nissa), Zone::Battlefield);
    let nissa = &outcome.state().objects[&nissa];
    assert_eq!(nissa.printed_loyalty, Some(PrintedLoyalty::X));
    assert_eq!(
        nissa.counters.get(&CounterType::Loyalty).copied(),
        Some(3),
        "CR 306.5b + CR 107.3m: Nissa must enter with loyalty counters equal to her announced X"
    );
}
