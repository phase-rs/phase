//! Integration coverage for issue #885 — Urza's Tower / Mine / Power-Plant.
//!
//! Oracle (activated mana ability on each):
//!   `{T}: Add {C}. If you control an Urza's <other> and an Urza's <other>,`
//!   `add {C}{C}{C} instead.`
//!
//! A unit test in `mana_abilities.rs` already proves the resolver produces
//! three colorless mana from a handcrafted `Effect::Mana` + `sub_ability` AST,
//! but nothing exercises the full pipeline (parser emit → real card load →
//! runtime `ActivateAbility` → mana production). This test closes that gap
//! using the parsed `client/public/card-data.json` so any drift in the
//! parser shape that the resolver depends on shows up here as a runtime
//! divergence, not just a unit-test failure.
//!
//! CR 605.3b: An activated mana ability doesn't go on the stack — it
//! resolves immediately after it is activated, so the assertion looks at
//! the active player's mana pool directly after the `ActivateAbility` call.
//! CR 614.1a: "Add {C}. If you control … add {C}{C}{C} instead." — the
//! word "instead" makes the sub-ability a replacement effect; its condition
//! is evaluated as the ability resolves (CR 608), and with all three Urza
//! lands controlled the `And` condition is satisfied and the delta
//! (+2 colorless) replaces the base production net (1 + 2 = 3 C).
//! CR 205.3i: "Mine," "Power-Plant," and "Tower" are distinct land subtypes
//! from the enumerated land type list; the cross-naming of the parsed
//! `ControllerControlsMatching` filters is what makes the three lands
//! reference each other rather than themselves.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const URZA_LAND_KEYS: [&str; 3] = ["urza's tower", "urza's mine", "urza's power plant"];

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| {
        let raw = std::fs::read_to_string(&path).expect("card-data.json should be readable");
        let export: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&raw).expect("card-data.json should be a JSON object");
        let mut selected = serde_json::Map::new();
        for key in URZA_LAND_KEYS {
            let value = export
                .get(key)
                .unwrap_or_else(|| panic!("{key} should be in card-data.json"));
            selected.insert(key.to_string(), value.clone());
        }
        CardDatabase::from_json_str(&serde_json::Value::Object(selected).to_string())
            .expect("Urza land export records should load")
    }))
}

/// With all three Urza lands on the battlefield, tapping Urza's Tower for mana
/// must produce three colorless (the `Add {C}` base plus the +2 delta granted
/// by the satisfied `If you control an Urza's Mine and an Urza's Power-Plant`
/// sub-ability). This is the load-bearing end-to-end check for issue #885.
#[test]
fn urzas_tower_with_mine_and_power_plant_produces_three_colorless() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tower_id = scenario.add_real_card(P0, "Urza's Tower", Zone::Battlefield, db);
    let _mine_id = scenario.add_real_card(P0, "Urza's Mine", Zone::Battlefield, db);
    let _plant_id = scenario.add_real_card(P0, "Urza's Power Plant", Zone::Battlefield, db);

    let mut runner = scenario.build();

    // CR 605.3b: a mana ability resolves immediately on activation (no stack),
    // so the outcome reads the resulting pool directly.
    let outcome = runner.activate(tower_id, 0).resolve();

    assert_eq!(
        outcome.mana_pool_color(P0, ManaType::Colorless),
        3,
        "Urza's Tower with Urza's Mine + Urza's Power Plant must produce 3 colorless \
         (1 base + 2 delta from satisfied sub-ability)",
    );
    assert_eq!(
        outcome.mana_pool_total(P0),
        3,
        "no other mana types must be produced",
    );
}
