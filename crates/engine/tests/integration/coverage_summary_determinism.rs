//! Identical card data yields byte-identical coverage and audit output.

use std::collections::BTreeSet;

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
    ]))
    .to_string();
    let load = || CardDatabase::from_json_str(&export).expect("test export deserializes");
    let summary = analyze_coverage(&load());

    let mut walk_leaders = BTreeSet::new();
    for _ in 0..32 {
        let db = load();
        walk_leaders.extend(db.face_iter().next().map(|(key, _)| key.to_owned()));
        let drill = |limit| {
            build_warning_drilldown(
                &db,
                &summary,
                None,
                None,
                Some("Condition_If"),
                limit,
                false,
            )
        };

        let all = drill(usize::MAX);
        assert_eq!(all.matched_cards, 2);
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
        let rank = |index: usize| {
            let card = &all.cards[index];
            (card.supported, card.gap_count, card.name.clone())
        };
        assert_eq!(rank(0), rank(1), "every sort key above the face key ties");

        let top = drill(1);
        assert_eq!(
            top.cards
                .iter()
                .map(|card| card.key.as_str())
                .collect::<Vec<_>>(),
            [CHANNEL]
        );
    }
    assert_eq!(
        walk_leaders.len(),
        2,
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
