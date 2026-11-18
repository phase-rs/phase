//! Identical card data yields byte-identical coverage and audit output.

use std::collections::{BTreeMap, BTreeSet};

use engine::database::legality::LegalityFormat;
use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, audit_resolver_features, audit_semantic, build_warning_drilldown,
    format_semantic_audit_markdown, CoverageSupport, ParseWarningPattern, WarningDrilldownCard,
};
use engine::game::gap_analysis::{gap_report_from_args, GapReport};
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

/// Both Channel faces with their printed legalities: the standalone card is banned in Commander,
/// the SOC prepared face is legal there.
fn channel_faces_with_legalities() -> [(String, serde_json::Value); 2] {
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
    [
        (CHANNEL.to_string(), channel),
        (CHANNEL_PREPARE.to_string(), channel_prepare),
    ]
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

    let recall = patterns_starting_with(&summary.parse_warning_patterns, "draw:");
    assert_eq!(recall.len(), 1);
    assert_eq!(recall[0].single_gap_cards, 0);
    let recall_drill = build_warning_drilldown(
        &load(),
        &summary,
        None,
        Some(&recall[0].pattern),
        None,
        usize::MAX,
        false,
    );
    assert_eq!(recall_drill.supported_cards, 1);
    assert_eq!(recall_drill.single_gap_cards, 0);

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

/// Both Jump faces, each with its own synthetic warning as its only gap.
fn jump_faces() -> [(String, serde_json::Value); 2] {
    [
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
    ]
}

const JERMANE: &str = "jermane, pride of the circus";

/// Jermane's exported face; its real `IgnoredRemainder` sits on the ability without the gap.
fn jermane_face() -> serde_json::Value {
    serde_json::json!({
        "name": "Jermane, Pride of the Circus",
        "mana_cost": {
            "generic": 0,
            "shards": [
                "Green",
                "Green"
            ],
            "type": "Cost"
        },
        "card_type": {
            "core_types": [
                "Creature"
            ],
            "subtypes": [
                "Cat",
                "Performer"
            ],
            "supertypes": [
                "Legendary"
            ]
        },
        "power": {
            "type": "Fixed",
            "value": 2
        },
        "toughness": {
            "type": "Fixed",
            "value": 3
        },
        "loyalty": null,
        "defense": null,
        "oracle_text": "{G}, {T}: Put two +1/+1 counters on target creature with four or more legs.\n{1}{G}{G}, {T}: Until end of turn, all creatures able to block target creature with four or more legs do so.",
        "non_ability_text": null,
        "flavor_name": null,
        "keywords": [],
        "abilities": [
            {
                "condition": null,
                "cost": {
                    "costs": [
                        {
                            "cost": {
                                "generic": 0,
                                "shards": [
                                    "Green"
                                ],
                                "type": "Cost"
                            },
                            "type": "Mana"
                        },
                        {
                            "type": "Tap"
                        }
                    ],
                    "type": "Composite"
                },
                "description": "{G}, {T}: Put two +1/+1 counters on target creature with four or more legs.",
                "duration": null,
                "effect": {
                    "description": "Put two +1/+1 counters on target creature with four or more legs",
                    "name": "put_counter_tail",
                    "type": "Unimplemented"
                },
                "forward_result": false,
                "kind": "Activated",
                "optional": false,
                "optional_targeting": false,
                "sub_ability": null,
                "target_prompt": null
            },
            {
                "condition": null,
                "cost": {
                    "costs": [
                        {
                            "cost": {
                                "generic": 1,
                                "shards": [
                                    "Green",
                                    "Green"
                                ],
                                "type": "Cost"
                            },
                            "type": "Mana"
                        },
                        {
                            "type": "Tap"
                        }
                    ],
                    "type": "Composite"
                },
                "description": "{1}{G}{G}, {T}: Until end of turn, all creatures able to block target creature with four or more legs do so.",
                "duration": "UntilEndOfTurn",
                "effect": {
                    "duration": "UntilEndOfTurn",
                    "static_abilities": [
                        {
                            "active_zones": [],
                            "affected": {
                                "controller": null,
                                "properties": [],
                                "type": "Typed",
                                "type_filters": [
                                    "Creature"
                                ]
                            },
                            "affected_zone": null,
                            "characteristic_defining": false,
                            "condition": null,
                            "description": null,
                            "effect_zone": null,
                            "mode": {
                                "MustBeBlockedByAll": {}
                            },
                            "modifications": [
                                {
                                    "mode": {
                                        "MustBeBlockedByAll": {}
                                    },
                                    "type": "AddStaticMode"
                                }
                            ]
                        }
                    ],
                    "target": {
                        "controller": null,
                        "properties": [],
                        "type": "Typed",
                        "type_filters": [
                            "Creature"
                        ]
                    },
                    "type": "GenericEffect"
                },
                "forward_result": false,
                "kind": "Activated",
                "optional": false,
                "optional_targeting": false,
                "sub_ability": null,
                "target_prompt": null
            }
        ],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "color_override": null,
        "scryfall_oracle_id": "2b7855b8-33ac-4e49-a043-8cc392fc6397",
        "legalities": {},
        "printings": [
            "UNF"
        ],
        "parse_warnings": [
            {
                "line_index": 1,
                "parser": "must-block",
                "text": "with four or more legs",
                "type": "IgnoredRemainder"
            }
        ]
    })
}

const RAM_THROUGH: &str = "ram through";

/// Ram Through's exported face; its two real warnings are two different gaps.
fn ram_through_face() -> serde_json::Value {
    serde_json::json!({
        "name": "Ram Through",
        "mana_cost": {
            "generic": 1,
            "shards": [
                "Green"
            ],
            "type": "Cost"
        },
        "card_type": {
            "core_types": [
                "Instant"
            ],
            "subtypes": [],
            "supertypes": []
        },
        "power": null,
        "toughness": null,
        "loyalty": null,
        "defense": null,
        "oracle_text": "Target creature you control deals damage equal to its power to target creature you don't control. If the creature you control has trample, excess damage is dealt to that creature's controller instead.",
        "non_ability_text": null,
        "flavor_name": null,
        "keywords": [],
        "abilities": [
            {
                "condition": null,
                "cost": null,
                "declares_chosen_group": 0,
                "description": "Target creature you control deals damage equal to its power to target creature you don't control. If the creature you control has trample, excess damage is dealt to that creature's controller instead.",
                "duration": null,
                "effect": {
                    "target": {
                        "controller": "You",
                        "properties": [],
                        "type": "Typed",
                        "type_filters": [
                            "Creature"
                        ]
                    },
                    "type": "TargetOnly"
                },
                "forward_result": false,
                "kind": "Spell",
                "optional": false,
                "optional_targeting": false,
                "sub_ability": {
                    "condition": null,
                    "cost": null,
                    "description": null,
                    "duration": null,
                    "effect": {
                        "amount": {
                            "qty": {
                                "scope": {
                                    "type": "Target"
                                },
                                "type": "Power"
                            },
                            "type": "Ref"
                        },
                        "damage_source": "Target",
                        "excess": {
                            "source_keyword": "Trample",
                            "type": "TargetController"
                        },
                        "target": {
                            "controller": "Opponent",
                            "properties": [],
                            "type": "Typed",
                            "type_filters": [
                                "Creature"
                            ]
                        },
                        "type": "DealDamage"
                    },
                    "forward_result": false,
                    "kind": "Spell",
                    "optional": false,
                    "optional_targeting": false,
                    "sub_ability": null,
                    "target_prompt": null
                },
                "target_prompt": null
            }
        ],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "color_override": null,
        "scryfall_oracle_id": "4b1befff-48fa-47a3-832e-bdaf43495c02",
        "legalities": {
            "brawl": "legal",
            "commander": "legal",
            "duel": "legal",
            "historic": "legal",
            "legacy": "legal",
            "modern": "legal",
            "oathbreaker": "legal",
            "pauper": "legal",
            "paupercommander": "legal",
            "pioneer": "legal",
            "timeless": "legal",
            "vintage": "legal"
        },
        "printings": [
            "CMM",
            "FDC",
            "GN3",
            "IKO",
            "J22",
            "PLST"
        ],
        "parse_warnings": [
            {
                "description": "Target creature you control deals damage equal to its power to target creature you don't control. If the creature you control has trample, e",
                "detector": "Replacement_Instead",
                "items": [
                    0
                ],
                "line_index": 0,
                "type": "SwallowedClause",
                "unit_span": {
                    "end_byte": 200,
                    "first_line": 0,
                    "last_line": 0,
                    "ordinal_within_span": 0,
                    "precision": "Exact",
                    "start_byte": 0
                }
            },
            {
                "description": "Target creature you control deals damage equal to its power to target creature you don't control. If the creature you control has trample, e",
                "detector": "Condition_If",
                "gap": {
                    "guard": "the creature you control has trample",
                    "kind": "unparsed_condition"
                },
                "items": [
                    0
                ],
                "line_index": 0,
                "type": "SwallowedClause",
                "unit_span": {
                    "end_byte": 200,
                    "first_line": 0,
                    "last_line": 0,
                    "ordinal_within_span": 0,
                    "precision": "Exact",
                    "start_byte": 0
                }
            }
        ]
    })
}

#[test]
fn single_gap_formats_credit_every_same_name_face() {
    let export = serde_json::Value::Object(serde_json::Map::from_iter(
        jump_faces()
            .into_iter()
            .chain(channel_faces_with_legalities())
            .chain([(JERMANE.to_string(), jermane_face())]),
    ))
    .to_string();
    let db = CardDatabase::from_json_str(&export).expect("test export deserializes");
    let summary = analyze_coverage(&db);
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

    let jermane = summary
        .cards
        .iter()
        .find(|card| card.card_face_key.as_deref() == Some(JERMANE))
        .expect("Jermane is scanned");
    assert_eq!(jermane.gap_count, 1);
    assert_eq!(jermane.gap_details[0].handler, "Effect:put_counter_tail");
    let must_block = summary
        .parse_warning_patterns
        .iter()
        .find(|pattern| pattern.pattern.starts_with("must-block:"))
        .expect("Jermane's warning forms a pattern");
    assert_eq!((must_block.warning_count, must_block.card_count), (1, 1));
    assert_eq!(must_block.single_gap_cards, 0);

    let drill = |pattern: &str| {
        build_warning_drilldown(&db, &summary, None, Some(pattern), None, usize::MAX, false)
    };
    let jermane_drill = drill(&must_block.pattern);
    assert_eq!(jermane_drill.matched_cards, 1);
    assert_eq!(jermane_drill.cards[0].gap_count, Some(1));
    assert_eq!(jermane_drill.single_gap_cards, 0);
    assert_eq!(drill(&pattern.pattern).single_gap_cards, 2);
}

fn patterns_starting_with<'a>(
    patterns: &'a [ParseWarningPattern],
    prefix: &str,
) -> Vec<&'a ParseWarningPattern> {
    patterns
        .iter()
        .filter(|pattern| pattern.pattern.starts_with(prefix))
        .collect()
}

#[test]
fn otherwise_supported_counts_a_face_gapped_only_by_its_warnings() {
    let export = serde_json::Value::Object(serde_json::Map::from_iter(
        jump_faces()
            .into_iter()
            .chain([(RAM_THROUGH.to_string(), ram_through_face())]),
    ))
    .to_string();
    let db = CardDatabase::from_json_str(&export).expect("test export deserializes");
    let summary = analyze_coverage(&db);

    let ram_through = summary
        .cards
        .iter()
        .find(|card| card.card_face_key.as_deref() == Some(RAM_THROUGH))
        .expect("Ram Through is scanned");
    assert_eq!(ram_through.gap_count, 2);
    for prefix in ["Condition_If:", "Replacement_Instead:"] {
        let patterns = patterns_starting_with(&summary.parse_warning_patterns, prefix);
        assert_eq!(patterns.len(), 1, "{prefix}");
        assert_eq!(patterns[0].card_count, 1, "{prefix}");
        assert_eq!(patterns[0].otherwise_supported_cards, 1, "{prefix}");
        assert_eq!(patterns[0].single_gap_cards, 0, "{prefix}");
        let drill = build_warning_drilldown(
            &db,
            &summary,
            None,
            Some(&patterns[0].pattern),
            None,
            usize::MAX,
            false,
        );
        assert_eq!(drill.matched_cards, 1, "{prefix}");
        assert_eq!(drill.single_gap_cards, 0, "{prefix}");
    }
    let jump = patterns_starting_with(&summary.parse_warning_patterns, "Duration_UntilEndOfTurn:");
    assert_eq!(jump.len(), 1);
    assert_eq!(jump[0].otherwise_supported_cards, 1);
}

#[test]
fn gap_report_format_filter_reads_each_face_legality() {
    let [mut channel, channel_prepare] = channel_faces_with_legalities();
    // Synthetic: a second detector, so the report shows which Channel face it kept.
    channel.1["parse_warnings"][0]["detector"] = "Duration_UntilEndOfTurn".into();
    channel.1["parse_warnings"][0]["description"] = "until end of turn".into();
    let export = serde_json::Value::Object(serde_json::Map::from_iter([channel, channel_prepare]))
        .to_string();
    let summary =
        analyze_coverage(&CardDatabase::from_json_str(&export).expect("test export deserializes"));
    let handlers = |key| {
        summary
            .cards
            .iter()
            .find(|card| card.card_face_key.as_deref() == Some(key))
            .map(|card| {
                card.gap_details
                    .iter()
                    .map(|gap| gap.handler.clone())
                    .collect::<Vec<_>>()
            })
    };
    let soc_handlers = handlers(CHANNEL_PREPARE).expect("SOC Channel is scanned");
    assert!(soc_handlers.iter().any(|h| h == "Swallow:Condition_If"));
    assert!(handlers(CHANNEL)
        .expect("standalone Channel is scanned")
        .iter()
        .any(|h| h == "Swallow:Duration_UntilEndOfTurn"));

    let root = tempfile::tempdir().expect("temp data root");
    std::fs::write(root.path().join("card-data.json"), export).expect("export written");
    let argv = [
        "parser-gap-analyzer".into(),
        root.path().display().to_string(),
        "--format".into(),
        "commander".into(),
    ];
    let Ok(GapReport { analysis, retained }) = gap_report_from_args(argv) else {
        panic!("the CLI argv yields a report");
    };
    let retained = retained.expect("the CLI forwards --format");
    assert_eq!(
        (retained.format, retained.kept, retained.before),
        (LegalityFormat::Commander, 1, 2)
    );
    assert_eq!(
        (analysis.total_unsupported, analysis.total_classified),
        (1, soc_handlers.len())
    );
    let families: BTreeSet<&str> = analysis
        .categories
        .values()
        .flat_map(|category| category.families.iter().map(|family| family.key.as_str()))
        .collect();
    assert!(families.contains("Swallow:Condition_If"), "{families:?}");
    assert!(
        !families.contains("Swallow:Duration_UntilEndOfTurn"),
        "{families:?}"
    );
}
