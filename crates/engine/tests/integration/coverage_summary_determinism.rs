//! Identical card data yields byte-identical coverage and audit output.

use std::collections::{BTreeMap, BTreeSet};

use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, audit_resolver_features, audit_semantic, build_warning_drilldown,
    format_semantic_audit_markdown, CoverageSupport, WarningDrilldownCard,
};
use flate2::read::GzDecoder;
use serde::Serialize;

fn load() -> CardDatabase {
    CardDatabase::from_export_reader(GzDecoder::new(
        &include_bytes!("../fixtures/integration_cards.json.gz")[..],
    ))
    .expect("integration fixture loads")
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("summary serializes")
}

#[test]
fn summaries_do_not_depend_on_hash_order() {
    let (first, second) = (load(), load());
    let keys = |db: &CardDatabase| {
        db.face_iter()
            .map(|(k, _)| k.to_owned())
            .collect::<Vec<_>>()
    };
    assert!(
        keys(&first) != keys(&second),
        "the two loads must walk their hash maps in different orders"
    );

    // `assert!` rather than `assert_eq!`: a failure would print megabytes.
    assert!(
        json(&analyze_coverage(&first)) == json(&analyze_coverage(&second)),
        "analyze_coverage"
    );
    assert!(
        json(&audit_resolver_features(&first)) == json(&audit_resolver_features(&second)),
        "audit_resolver_features"
    );
    let (semantic_first, semantic_second) = (audit_semantic(&first), audit_semantic(&second));
    assert!(
        json(&semantic_first) == json(&semantic_second),
        "audit_semantic"
    );
    assert!(
        format_semantic_audit_markdown(&semantic_first)
            == format_semantic_audit_markdown(&semantic_second),
        "format_semantic_audit_markdown"
    );
}

const CHANNEL_TEXT: &str =
    "Until end of turn, any time you could activate a mana ability, you may pay 1 life. If you do, add {C}.";
const CHANNEL: &str = "channel";
const CHANNEL_PREPARE: &str = "channel [d9068051-b51c-4700-b492-30b148e3a72c]";
const CHANNEL_PRINTINGS: [&str; 16] = [
    "2ED", "30A", "3ED", "4BB", "4ED", "CED", "CEI", "FBB", "IMA", "LEA", "LEB", "ME4", "STA",
    "SUM", "V09", "VMA",
];

/// No printed same-name pair carries a parse warning in the export, so Channel's two faces (the
/// SOC one minus its `prepare` layout) get a synthetic `Condition_If` warning.
fn channel_face(oracle_id: &str, printings: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "name": "Channel",
        "mana_cost": { "type": "Cost", "shards": ["Green", "Green"], "generic": 0 },
        "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": CHANNEL_TEXT,
        "non_ability_text": null,
        "flavor_name": null,
        "keywords": [],
        "abilities": [],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "color_override": null,
        "scryfall_oracle_id": oracle_id,
        "printings": printings,
        "parse_warnings": [{
            "type": "SwallowedClause",
            "detector": "Condition_If",
            "description": "if you do",
            "line_index": 0
        }]
    })
}

const ANCESTRAL_RECALL: &str = "ancestral recall";

/// Ancestral Recall's exported face with a synthetic `IgnoredRemainder`, a warning that adds no gap.
fn ancestral_recall_face() -> serde_json::Value {
    serde_json::json!({
        "name": "Ancestral Recall",
        "mana_cost": { "type": "Cost", "shards": ["Blue"], "generic": 0 },
        "card_type": { "supertypes": [], "core_types": ["Instant"], "subtypes": [] },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": "Target player draws three cards.",
        "non_ability_text": null,
        "flavor_name": null,
        "keywords": [],
        "abilities": [{
            "condition": null,
            "cost": null,
            "declares_chosen_group": 2147483648_u64,
            "description": "Target player draws three cards.",
            "duration": null,
            "effect": {
                "type": "Draw",
                "count": { "type": "Fixed", "value": 3 },
                "target": { "type": "Player" }
            },
            "forward_result": false,
            "kind": "Spell",
            "optional": false,
            "optional_targeting": false,
            "sub_ability": null,
            "target_prompt": null
        }],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "color_override": null,
        "color_identity": ["Blue"],
        "legalities": {
            "commander": "banned",
            "duel": "banned",
            "legacy": "banned",
            "oathbreaker": "banned",
            "vintage": "restricted"
        },
        "metadata": {
            "source_printing_ids": [
                "71283e4c-efd9-4a96-991c-6bbb169b99e7",
                "d3c44b29-a973-4174-80a6-76edbd4f2a22"
            ]
        },
        "rarities": ["rare", "mythic"],
        "scryfall_oracle_id": "550c74d4-1fcb-406a-b02a-639a760a4380",
        "printings": ["2ED", "30A", "CED", "CEI", "LEA", "LEB", "OVNT", "PRM", "VMA", "YDMU"],
        "parse_warnings": [{
            "type": "IgnoredRemainder", "parser": "draw", "text": "three cards", "line_index": 0
        }]
    })
}

#[test]
fn warning_drilldown_joins_by_face_key_and_breaks_name_ties_by_key() {
    let export = serde_json::Value::Object(serde_json::Map::from_iter([
        (
            CHANNEL.to_string(),
            channel_face("d1b815d1-2848-40d4-a555-66822d1becbc", &CHANNEL_PRINTINGS),
        ),
        (
            CHANNEL_PREPARE.to_string(),
            channel_face("d9068051-b51c-4700-b492-30b148e3a72c", &["SOC"]),
        ),
        (ANCESTRAL_RECALL.to_string(), ancestral_recall_face()),
    ]))
    .to_string();
    let load = || CardDatabase::from_json_str(&export).expect("test export deserializes");
    let summary = analyze_coverage(&load());

    let mut walk_leaders = BTreeSet::new();
    for _ in 0..64 {
        let db = load();
        walk_leaders.extend(db.face_iter().next().map(|(key, _)| key.to_owned()));
        let drill = |detector, limit| {
            build_warning_drilldown(&db, &summary, None, None, detector, limit, false)
        };

        let all = drill(None, usize::MAX);
        assert_eq!(all.matched_cards, 3);
        let swallowed = serde_json::to_value(drill(Some("Condition_If"), usize::MAX))
            .expect("drilldown serializes");
        assert_eq!(swallowed["matched_cards"], 2);
        let rows = swallowed["cards"]
            .as_array()
            .expect("cards serialize as an array");
        assert_eq!(
            rows.iter()
                .map(|card| (card["key"].clone(), card["supported"].clone()))
                .collect::<Vec<_>>(),
            [
                (serde_json::json!(CHANNEL), serde_json::json!(false)),
                (serde_json::json!(CHANNEL_PREPARE), serde_json::json!(false)),
            ]
        );
        let printings = |key: &str| {
            all.cards
                .iter()
                .find(|card| card.key == key)
                .map(|card| card.printings.clone())
        };
        assert_eq!(
            printings(CHANNEL),
            Some(CHANNEL_PRINTINGS.map(String::from).to_vec())
        );
        assert_eq!(printings(CHANNEL_PREPARE), Some(vec!["SOC".to_string()]));
        assert_eq!(
            all.cards
                .iter()
                .map(|card| card.key.as_str())
                .collect::<Vec<_>>(),
            [ANCESTRAL_RECALL, CHANNEL, CHANNEL_PREPARE]
        );
        let rank = |index: usize| {
            let card = &all.cards[index];
            (card.supported, card.gap_count, card.name.clone())
        };
        assert_eq!(rank(1), rank(2), "every sort key above the face key ties");

        let top = serde_json::to_value(drill(None, 1)).expect("drilldown serializes");
        assert_eq!(top["supported_cards"], 1);
        let cards = top["cards"]
            .as_array()
            .expect("cards serialize as an array");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["key"], ANCESTRAL_RECALL);
        assert_eq!(cards[0]["supported"], true);
    }
    assert!(
        walk_leaders.contains(CHANNEL) && walk_leaders.contains(CHANNEL_PREPARE),
        "the face walk must lead with each Channel in some load"
    );
}

#[test]
fn warning_drilldown_support_keeps_its_boolean_wire_shape() {
    for (support, wire) in [
        (Some(CoverageSupport::Supported), serde_json::json!(true)),
        (Some(CoverageSupport::Unsupported), serde_json::json!(false)),
        (None, serde_json::Value::Null),
    ] {
        let card = WarningDrilldownCard {
            name: "Channel".into(),
            key: CHANNEL.into(),
            supported: support,
            gap_count: None,
            printings: vec![],
            oracle_text: None,
            warnings: vec![],
            parsed_labels: vec![],
            gap_details: vec![],
            parse_details: None,
            card: None,
        };
        let json = serde_json::to_value(&card).expect("card serializes");
        assert_eq!(json.get("supported"), Some(&wire), "{support:?}");
        assert_eq!(
            serde_json::from_value::<Option<CoverageSupport>>(wire).expect("wire deserializes"),
            support
        );
    }
}

const JUMP: &str = "jump";
const JUMP_PREPARE: &str = "jump [d6accaed-ff35-4324-b31b-35e6837bc079]";

/// Jump's exported face, shared by the standalone card and Encouraging Aviator's prepared face,
/// carrying `warnings` copies of a synthetic `Duration_UntilEndOfTurn` swallowed clause.
fn jump_face(oracle_id: &str, legalities: serde_json::Value, warnings: usize) -> serde_json::Value {
    let warning = serde_json::json!({
        "type": "SwallowedClause",
        "detector": "Duration_UntilEndOfTurn",
        "description": "until end of turn",
        "line_index": 0
    });
    serde_json::json!({
        "name": "Jump",
        "mana_cost": { "type": "Cost", "shards": ["Blue"], "generic": 0 },
        "card_type": { "supertypes": [], "core_types": ["Instant"], "subtypes": [] },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": "Target creature gains flying until end of turn.",
        "non_ability_text": null,
        "flavor_name": null,
        "keywords": [],
        "abilities": [{
            "kind": "Spell",
            "effect": {
                "type": "GenericEffect",
                "static_abilities": [{
                    "mode": "Continuous",
                    "affected": { "type": "ParentTarget" },
                    "modifications": [{ "type": "AddKeyword", "keyword": "Flying" }],
                    "condition": null,
                    "affected_zone": null,
                    "effect_zone": null,
                    "active_zones": [],
                    "characteristic_defining": false,
                    "description": "gain flying"
                }],
                "duration": "UntilEndOfTurn",
                "target": {
                    "type": "Typed",
                    "type_filters": ["Creature"],
                    "controller": null,
                    "properties": []
                }
            },
            "cost": null,
            "sub_ability": null,
            "duration": "UntilEndOfTurn",
            "description": "Target creature gains flying until end of turn.",
            "target_prompt": null,
            "condition": null,
            "optional_targeting": false,
            "optional": false,
            "forward_result": false
        }],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "color_override": null,
        "scryfall_oracle_id": oracle_id,
        "legalities": legalities,
        "parse_warnings": vec![warning; warnings]
    })
}

#[test]
fn single_gap_formats_credit_every_same_name_face() {
    let mut channel = channel_face("d1b815d1-2848-40d4-a555-66822d1becbc", &CHANNEL_PRINTINGS);
    channel["legalities"] = serde_json::json!({
        "brawl": "banned",
        "commander": "banned",
        "duel": "banned",
        "historic": "banned",
        "legacy": "banned",
        "oathbreaker": "banned",
        "premodern": "banned",
        "timeless": "restricted",
        "vintage": "restricted"
    });
    let mut channel_prepare = channel_face("d9068051-b51c-4700-b492-30b148e3a72c", &["SOC"]);
    channel_prepare["legalities"] = serde_json::json!({
        "commander": "legal",
        "duel": "legal",
        "legacy": "legal",
        "oathbreaker": "legal",
        "vintage": "legal"
    });
    let export = serde_json::Value::Object(serde_json::Map::from_iter([
        (
            JUMP.to_string(),
            jump_face(
                "f7518456-45ed-41d4-bd3c-5aacea28eb35",
                serde_json::json!({
                    "commander": "legal",
                    "duel": "legal",
                    "legacy": "legal",
                    "modern": "legal",
                    "oathbreaker": "legal",
                    "pauper": "legal",
                    "paupercommander": "legal",
                    "premodern": "legal",
                    "vintage": "legal"
                }),
                2,
            ),
        ),
        (
            JUMP_PREPARE.to_string(),
            jump_face(
                "d6accaed-ff35-4324-b31b-35e6837bc079",
                serde_json::json!({
                    "brawl": "legal",
                    "commander": "legal",
                    "duel": "legal",
                    "historic": "legal",
                    "legacy": "legal",
                    "modern": "legal",
                    "oathbreaker": "legal",
                    "pioneer": "legal",
                    "standard": "legal",
                    "standardbrawl": "legal",
                    "timeless": "legal",
                    "vintage": "legal"
                }),
                1,
            ),
        ),
        (CHANNEL.to_string(), channel),
        (CHANNEL_PREPARE.to_string(), channel_prepare),
    ]))
    .to_string();
    let summary =
        analyze_coverage(&CardDatabase::from_json_str(&export).expect("test export deserializes"));
    let count = |map: &BTreeMap<String, usize>, format: &str| map.get(format).copied().unwrap_or(0);

    let pattern = summary
        .parse_warning_patterns
        .iter()
        .find(|pattern| pattern.pattern.starts_with("Duration_UntilEndOfTurn:"))
        .expect("the shared warning forms a pattern");
    assert_eq!(pattern.warning_count, 3);
    assert_eq!(pattern.single_gap_cards, 1);
    assert_eq!(count(&pattern.single_gap_by_format, "pauper"), 1);
    assert_eq!(count(&pattern.single_gap_by_format, "standard"), 1);
    assert!(
        pattern.single_gap_by_format.values().all(|&n| n == 1),
        "{:?}",
        pattern.single_gap_by_format
    );

    let top_gap = summary
        .top_gaps
        .iter()
        .find(|gap| gap.handler == "Swallow:Duration_UntilEndOfTurn")
        .expect("the shared warning is a top gap");
    assert_eq!(top_gap.single_gap_cards, 2);
    assert_eq!(count(&top_gap.single_gap_by_format, "pauper"), 1);
    assert_eq!(count(&top_gap.single_gap_by_format, "standard"), 1);
    assert_eq!(count(&top_gap.single_gap_by_format, "commander"), 2);

    let bundle = summary
        .gap_bundles
        .iter()
        .find(|bundle| bundle.handlers.iter().any(|h| h == "Swallow:Condition_If"))
        .expect("Channel's gaps form a bundle");
    assert_eq!(bundle.unlocked_cards, 2);
    assert_eq!(
        bundle.unlocked_by_format,
        ["commander", "duel", "legacy", "oathbreaker", "vintage"]
            .map(|format| (format.to_string(), 1))
            .into()
    );
}
