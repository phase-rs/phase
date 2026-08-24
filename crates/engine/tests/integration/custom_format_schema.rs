//! Schema-level tests for the custom-format engine core (Phase 1a). No
//! evaluator exists yet — these tests cover construction, serde round-trip,
//! the `GameFormat::Custom` wire format, the two registration gates against
//! synthetic values, and the disclosed non-panicking fallbacks for methods
//! that cannot resolve a Custom format's real values from a bare
//! `GameFormat` alone. Never real deck-legality enforcement (that's Phase 1d).

use engine::types::custom_format::{
    passes_legacy_axis_gate, passes_reprint_fidelity_gate, validate_custom_rules_consistency,
    CombatDamageTiming, CommanderEligibilityRule, CustomFormatDef, CustomFormatId,
    CustomFormatRules, LegacyRuleSet, LegalityRules, ManaBurnPolicy, PrintingFidelity,
    ReprintPolicy, SetCode, StructuralRules, WishOutsideGameScope,
};
use engine::types::format::{DeckCopyLimit, FormatConfig, GameFormat, SideboardPolicy};

fn sample_structural() -> StructuralRules {
    StructuralRules {
        starting_life: 30,
        min_players: 2,
        max_players: 4,
        deck_size: 60,
        singleton: false,
        command_zone: false,
        commander_damage_threshold: None,
        range_of_influence: None,
        team_based: false,
        sideboard_policy: SideboardPolicy::Unlimited,
        commander_eligibility_rule: None,
    }
}

fn sample_rules(id: u16) -> CustomFormatRules {
    CustomFormatRules {
        id: CustomFormatId(id),
        structural: sample_structural(),
        legality: LegalityRules {
            legal_sets: None,
            banned: Vec::new(),
            restricted: Vec::new(),
            legacy: LegacyRuleSet {
                mana_burn: ManaBurnPolicy::default(),
                damage_timing: CombatDamageTiming::default(),
                wish_scope: WishOutsideGameScope::default(),
                legend_rule_scope: engine::types::custom_format::LegendRuleScope::default(),
            },
        },
    }
}

fn sample_def(id: u16) -> CustomFormatDef {
    CustomFormatDef {
        rules: sample_rules(id),
        label: "Sample Custom Format".to_string(),
        short_label: "SCF".to_string(),
        description: "A test-only custom format".to_string(),
        reprint_policy: None,
        printing_fidelity: PrintingFidelity::NotApplicable,
    }
}

#[test]
fn custom_format_rules_serde_roundtrip() {
    let rules = sample_rules(5);
    let json = serde_json::to_string(&rules).unwrap();
    let back: CustomFormatRules = serde_json::from_str(&json).unwrap();
    assert_eq!(rules, back);
}

#[test]
fn custom_format_def_serde_roundtrip() {
    let def = sample_def(5);
    let json = serde_json::to_string(&def).unwrap();
    let back: CustomFormatDef = serde_json::from_str(&json).unwrap();
    assert_eq!(def, back);
}

#[test]
fn legal_sets_none_and_some_are_distinguishable() {
    let unrestricted = LegalityRules {
        legal_sets: None,
        banned: Vec::new(),
        restricted: Vec::new(),
        legacy: sample_rules(0).legality.legacy,
    };
    let restricted = LegalityRules {
        legal_sets: Some(vec![SetCode("LEA".to_string())]),
        ..unrestricted.clone()
    };
    let unrestricted_json = serde_json::to_value(&unrestricted).unwrap();
    let restricted_json = serde_json::to_value(&restricted).unwrap();
    assert_ne!(unrestricted_json, restricted_json);
    assert_eq!(unrestricted_json["legal_sets"], serde_json::Value::Null);
    assert_eq!(restricted_json["legal_sets"][0], "LEA");
}

#[test]
fn validate_custom_rules_consistency_accepts_matching_id() {
    let rules = sample_rules(5);
    let config = FormatConfig {
        custom_rules: Some(rules.clone()),
        ..FormatConfig {
            format: GameFormat::Custom(rules.id),
            ..FormatConfig::standard()
        }
    };
    assert!(validate_custom_rules_consistency(&config).is_ok());
}

#[test]
fn validate_custom_rules_consistency_rejects_mismatched_id() {
    let config = FormatConfig {
        format: GameFormat::Custom(CustomFormatId(5)),
        custom_rules: Some(sample_rules(7)),
        ..FormatConfig::standard()
    };
    assert!(validate_custom_rules_consistency(&config).is_err());
}

#[test]
fn validate_custom_rules_consistency_rejects_custom_without_rules() {
    let config = FormatConfig {
        format: GameFormat::Custom(CustomFormatId(5)),
        custom_rules: None,
        ..FormatConfig::standard()
    };
    assert!(validate_custom_rules_consistency(&config).is_err());
}

#[test]
fn validate_custom_rules_consistency_rejects_builtin_with_custom_rules() {
    let config = FormatConfig {
        format: GameFormat::Standard,
        custom_rules: Some(sample_rules(5)),
        ..FormatConfig::standard()
    };
    assert!(validate_custom_rules_consistency(&config).is_err());
}

#[test]
fn validate_custom_rules_consistency_accepts_every_builtin_default() {
    for meta in GameFormat::registry() {
        let config = FormatConfig::for_format(meta.format);
        assert!(
            validate_custom_rules_consistency(&config).is_ok(),
            "{:?}: built-in default config must be accepted",
            meta.format
        );
    }
}

#[test]
fn legacy_axis_gate_rejects_undeclared_axis() {
    let mut def = sample_def(1);
    def.rules.legality.legacy.mana_burn = ManaBurnPolicy::Legacy;
    assert!(!passes_legacy_axis_gate(&def));
}

#[test]
fn legacy_axis_gate_accepts_all_default_axes() {
    let def = sample_def(2);
    assert!(passes_legacy_axis_gate(&def));
}

#[test]
fn reprint_fidelity_gate_rejects_mismatch() {
    let mut def = sample_def(3);
    def.reprint_policy = Some(ReprintPolicy::OriginalPrintingsOnly);
    def.printing_fidelity = PrintingFidelity::NotApplicable;
    assert!(!passes_reprint_fidelity_gate(&def));

    let mut def2 = sample_def(4);
    def2.reprint_policy = None;
    def2.printing_fidelity = PrintingFidelity::SetCodeApproximation;
    assert!(!passes_reprint_fidelity_gate(&def2));
}

#[test]
fn reprint_fidelity_gate_accepts_agreement() {
    let mut def = sample_def(5);
    def.reprint_policy = Some(ReprintPolicy::AllowAnyPrinting);
    def.printing_fidelity = PrintingFidelity::SetCodeApproximation;
    assert!(passes_reprint_fidelity_gate(&def));

    let def2 = sample_def(6);
    assert!(passes_reprint_fidelity_gate(&def2));
}

#[test]
fn custom_format_registry_is_empty_in_phase_1a() {
    assert!(engine::types::custom_format::custom_format_registry().is_empty());
}

#[test]
fn game_format_from_str_display_roundtrip_builtins() {
    let all = [
        GameFormat::Standard,
        GameFormat::Limited,
        GameFormat::Commander,
        GameFormat::Pioneer,
        GameFormat::Modern,
        GameFormat::Premodern,
        GameFormat::Legacy,
        GameFormat::Vintage,
        GameFormat::Historic,
        GameFormat::Timeless,
        GameFormat::Pauper,
        GameFormat::PauperCommander,
        GameFormat::DuelCommander,
        GameFormat::TinyLeaders,
        GameFormat::Oathbreaker,
        GameFormat::Brawl,
        GameFormat::HistoricBrawl,
        GameFormat::FreeForAll,
        GameFormat::TwoHeadedGiant,
        GameFormat::Archenemy,
        GameFormat::Planechase,
        GameFormat::Momir,
    ];
    assert_eq!(all.len(), 22);
    for format in all {
        let s = format.to_string();
        let back: GameFormat = s.parse().unwrap();
        assert_eq!(format, back);
    }
}

#[test]
fn game_format_from_str_display_roundtrip_custom() {
    for id in [0u16, 5, u16::MAX] {
        let format = GameFormat::Custom(CustomFormatId(id));
        let s = format.to_string();
        assert_eq!(s, format!("Custom:{id}"));
        let back: GameFormat = s.parse().unwrap();
        assert_eq!(format, back);
    }
}

#[test]
fn game_format_serde_roundtrip_builtin_and_custom() {
    let json = serde_json::to_string(&GameFormat::Commander).unwrap();
    assert_eq!(json, "\"Commander\"");
    let back: GameFormat = serde_json::from_str(&json).unwrap();
    assert_eq!(back, GameFormat::Commander);

    let custom_json = serde_json::to_string(&GameFormat::Custom(CustomFormatId(5))).unwrap();
    assert_eq!(custom_json, "\"Custom:5\"");
    let back: GameFormat = serde_json::from_str(&custom_json).unwrap();
    assert_eq!(back, GameFormat::Custom(CustomFormatId(5)));
}

#[test]
fn game_format_deserialize_rejects_malformed_custom_strings() {
    for bad in [
        "\"Custom:\"",
        "\"Custom:abc\"",
        "\"Custom:-1\"",
        "\"Custom:70000\"",
        "\"custom:5\"",
        "\"CustomFormat:5\"",
        "\"NotARealFormat\"",
        "{}",
        "42",
    ] {
        assert!(
            serde_json::from_str::<GameFormat>(bad).is_err(),
            "expected {bad} to fail to deserialize as GameFormat"
        );
    }
}

#[test]
fn game_format_deserialize_accepts_valid_custom_string() {
    let back: GameFormat = serde_json::from_str("\"Custom:5\"").unwrap();
    assert_eq!(back, GameFormat::Custom(CustomFormatId(5)));
}

#[test]
fn commander_eligibility_rule_from_source_format_covers_every_builtin() {
    use CommanderEligibilityRule::*;
    let cases = [
        (GameFormat::Standard, None),
        (GameFormat::Limited, None),
        (GameFormat::Commander, Some(Standard)),
        (GameFormat::Pioneer, None),
        (GameFormat::Modern, None),
        (GameFormat::Premodern, None),
        (GameFormat::Legacy, None),
        (GameFormat::Vintage, None),
        (GameFormat::Historic, None),
        (GameFormat::Timeless, None),
        (GameFormat::Pauper, None),
        (GameFormat::PauperCommander, Some(Standard)),
        (GameFormat::DuelCommander, Some(Standard)),
        (GameFormat::TinyLeaders, Some(TinyLeaders)),
        (GameFormat::Oathbreaker, Some(OathbreakerSignatureSpell)),
        (GameFormat::Brawl, Some(BrawlColorIdentity)),
        (GameFormat::HistoricBrawl, Some(BrawlColorIdentity)),
        (GameFormat::FreeForAll, None),
        (GameFormat::TwoHeadedGiant, None),
        (GameFormat::Archenemy, None),
        (GameFormat::Planechase, None),
        (GameFormat::Momir, None),
    ];
    for (format, expected) in cases {
        assert_eq!(
            CommanderEligibilityRule::from_source_format(format),
            expected,
            "{format:?}"
        );
    }
}

#[test]
fn game_format_serialization_is_byte_identical_to_old_derive_for_builtins() {
    let expectations: &[(GameFormat, &str)] = &[
        (GameFormat::Standard, "Standard"),
        (GameFormat::Limited, "Limited"),
        (GameFormat::Commander, "Commander"),
        (GameFormat::Pioneer, "Pioneer"),
        (GameFormat::Modern, "Modern"),
        (GameFormat::Premodern, "Premodern"),
        (GameFormat::Legacy, "Legacy"),
        (GameFormat::Vintage, "Vintage"),
        (GameFormat::Historic, "Historic"),
        (GameFormat::Timeless, "Timeless"),
        (GameFormat::Pauper, "Pauper"),
        (GameFormat::PauperCommander, "PauperCommander"),
        (GameFormat::DuelCommander, "DuelCommander"),
        (GameFormat::TinyLeaders, "TinyLeaders"),
        (GameFormat::Oathbreaker, "Oathbreaker"),
        (GameFormat::Brawl, "Brawl"),
        (GameFormat::HistoricBrawl, "HistoricBrawl"),
        (GameFormat::FreeForAll, "FreeForAll"),
        (GameFormat::TwoHeadedGiant, "TwoHeadedGiant"),
        (GameFormat::Archenemy, "Archenemy"),
        (GameFormat::Planechase, "Planechase"),
        (GameFormat::Momir, "Momir"),
    ];
    assert_eq!(expectations.len(), 22);
    for (format, expected) in expectations {
        let value = serde_json::to_value(format).unwrap();
        assert_eq!(value, serde_json::Value::String(expected.to_string()));
    }
}

#[test]
fn format_config_for_format_still_works_for_every_builtin() {
    for meta in GameFormat::registry() {
        let config = FormatConfig::for_format(meta.format);
        assert!(matches!(
            config.format.default_deck_copy_limit(),
            DeckCopyLimit::Unlimited | DeckCopyLimit::UpTo(_)
        ));
    }
}

#[test]
fn custom_format_sideboard_policy_returns_disclosed_fallback_not_panic() {
    assert_eq!(
        GameFormat::Custom(CustomFormatId(1)).sideboard_policy(),
        SideboardPolicy::Forbidden
    );
}

#[test]
fn custom_format_default_deck_copy_limit_returns_disclosed_fallback_not_panic() {
    assert_eq!(
        GameFormat::Custom(CustomFormatId(1)).default_deck_copy_limit(),
        DeckCopyLimit::UpTo(1)
    );
}

#[test]
fn custom_format_label_falls_back_when_id_is_not_registered() {
    assert_eq!(
        GameFormat::Custom(CustomFormatId(1)).label(),
        "Custom Format"
    );
}

#[test]
fn custom_format_deck_compatibility_summary_reports_not_yet_supported() {
    use engine::database::CardDatabase;
    use engine::game::deck_validation::{evaluate_deck_compatibility, DeckCompatibilityRequest};

    let db = CardDatabase::from_json_str("{}").expect("empty card database");
    let request = DeckCompatibilityRequest {
        selected_format: Some(GameFormat::Custom(CustomFormatId(1))),
        summary_only: true,
        ..Default::default()
    };
    let result = evaluate_deck_compatibility(&db, &request);
    assert_eq!(result.selected_format_compatible, Some(false));
    assert!(result
        .selected_format_reasons
        .iter()
        .any(|r| r.contains("not yet supported")));
}

#[test]
fn custom_format_deck_compatibility_reports_not_yet_supported() {
    use engine::database::CardDatabase;
    use engine::game::deck_validation::{evaluate_deck_compatibility, DeckCompatibilityRequest};

    let db = CardDatabase::from_json_str("{}").expect("empty card database");
    let request = DeckCompatibilityRequest {
        selected_format: Some(GameFormat::Custom(CustomFormatId(1))),
        summary_only: false,
        ..Default::default()
    };
    let result = evaluate_deck_compatibility(&db, &request);
    assert_eq!(result.selected_format_compatible, Some(false));
    assert!(result
        .selected_format_reasons
        .iter()
        .any(|r| r.contains("not yet supported")));
}

#[test]
fn validate_name_deck_for_format_full_rejects_custom_format_honestly() {
    use engine::database::CardDatabase;
    use engine::game::deck_validation::validate_name_deck_for_format_full;

    let db = CardDatabase::from_json_str("{}").expect("empty card database");
    // The real signature takes every deck slot explicitly, plus a match type
    // and a player count — not the three-argument shape an earlier planning
    // pass assumed.
    let result = validate_name_deck_for_format_full(
        &db,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        GameFormat::Custom(CustomFormatId(1)),
        None,
        2,
    );
    match result {
        Err(reasons) => assert!(reasons.iter().any(|r| r.contains("not yet supported"))),
        Ok(()) => panic!("expected Custom format validation to be rejected as not yet supported"),
    }
}
