//! Pins the legacy bracket-signal wire shape through database loading and estimation.

use engine::database::{BracketLists, CardDatabase};
use engine::game::bracket_estimate::{estimate_bracket, BracketAxis, CommanderBracketTier};
use engine::game::deck_loading::PlayerDeckList;

const LEGACY_CARD_DATA: &str = r#"{
    "smothering tithe": {
        "name": "Smothering Tithe",
        "mana_cost": { "type": "NoCost" },
        "card_type": { "supertypes": [], "core_types": ["Enchantment"], "subtypes": [] },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": null,
        "abilities": [],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "keywords": [],
        "bracket_signals": {
            "game_changer": true,
            "mass_land_denial": false,
            "extra_turn": false,
            "efficient_tutor": false
        }
    },
    "ancient tomb": {
        "name": "Ancient Tomb",
        "mana_cost": { "type": "NoCost" },
        "card_type": { "supertypes": [], "core_types": ["Land"], "subtypes": [] },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": null,
        "abilities": [],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "keywords": [],
        "bracket_signals": { "game_changer": true }
    },
    "forest": {
        "name": "Forest",
        "mana_cost": { "type": "NoCost" },
        "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Forest"] },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": null,
        "abilities": [],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "keywords": []
    }
}"#;

fn legacy_db() -> CardDatabase {
    CardDatabase::from_json_str(LEGACY_CARD_DATA)
        .unwrap()
        .with_bracket_lists(BracketLists::from_json_str(r#"{"version":"test-1"}"#).unwrap())
}

#[test]
fn legacy_card_data_export_still_carries_bracket_signals() {
    let db = legacy_db();

    assert!(db
        .bracket_signals_for("Smothering Tithe")
        .axes()
        .contains(&BracketAxis::GameChangers));
    assert!(db
        .bracket_signals_for("Ancient Tomb")
        .axes()
        .contains(&BracketAxis::GameChangers));
    assert!(db.bracket_signals_for("Forest").axes().is_empty());
}

#[test]
fn estimate_over_a_legacy_export_reports_an_axis_keyed_reading() {
    let db = legacy_db();
    let deck = PlayerDeckList {
        commander: vec!["Atraxa, Praetors' Voice".to_string()],
        main_deck: vec!["Smothering Tithe".to_string(), "Forest".to_string()],
        ..Default::default()
    };

    let estimate = estimate_bracket(&deck, &db).unwrap();
    assert_eq!(estimate.tier, CommanderBracketTier::Upgraded);
    assert_eq!(estimate.axes[&BracketAxis::GameChangers].count, 1);
    assert_eq!(
        estimate.axes[&BracketAxis::GameChangers].contributing,
        ["Smothering Tithe"]
    );
    assert_eq!(
        estimate.axes[&BracketAxis::GameChangers].cap_at_tier,
        Some(3)
    );
    assert_eq!(estimate.axes[&BracketAxis::MassLandDenial].count, 0);
    assert_eq!(
        estimate.axes[&BracketAxis::MassLandDenial].cap_at_tier,
        Some(0)
    );
}
