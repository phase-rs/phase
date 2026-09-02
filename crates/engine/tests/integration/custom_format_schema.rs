//! Schema-level tests for the custom-format engine core (Phase 1a). No
//! evaluator exists yet — these tests cover construction, serde round-trip,
//! the `GameFormat::Custom` wire format, the two registration gates against
//! synthetic values, and the disclosed non-panicking fallbacks for methods
//! that cannot resolve a Custom format's real values from a bare
//! `GameFormat` alone. Never real deck-legality enforcement (that's Phase 1d).

use engine::types::custom_format::{
    assert_no_lobby_save_sentinel_collision, passes_legacy_axis_gate, passes_reprint_fidelity_gate,
    validate_custom_rules_consistency, CombatDamageTiming, CommandZoneMode,
    CommanderEligibilityRule, CustomFormatDef, CustomFormatId, CustomFormatRules, LegacyRuleSet,
    LegalityRules, ManaBurnPolicy, PrintingFidelity, ReprintPolicy, SetCode, StructuralRules,
    WishOutsideGameScope, LOBBY_SAVE_CUSTOM_FORMAT_ID,
};
use engine::types::format::{
    DeckCopyLimit, DeckSizeRule, FormatConfig, GameFormat, SideboardPolicy,
};

fn sample_structural() -> StructuralRules {
    StructuralRules {
        starting_life: 30,
        min_players: 2,
        max_players: 4,
        deck_size: DeckSizeRule::Minimum(60),
        singleton: false,
        command_zone_mode: CommandZoneMode::Disabled,
        range_of_influence: None,
        team_based: false,
        sideboard_policy: SideboardPolicy::Unlimited,
        default_deck_copy_limit: DeckCopyLimit::UpTo(4),
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
        custom_rules: Some(Box::new(rules.clone())),
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
        custom_rules: Some(Box::new(sample_rules(7))),
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
        custom_rules: Some(Box::new(sample_rules(5))),
        ..FormatConfig::standard()
    };
    assert!(validate_custom_rules_consistency(&config).is_err());
}

#[test]
fn validate_custom_rules_consistency_accepts_every_builtin_default() {
    for meta in GameFormat::registry() {
        let config = FormatConfig::for_format(meta.format).unwrap();
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
    def.rules.legality.legacy.mana_burn = ManaBurnPolicy::Obsolete;
    assert!(!passes_legacy_axis_gate(&def.rules.legality.legacy));
}

#[test]
fn legacy_axis_gate_accepts_all_default_axes() {
    let def = sample_def(2);
    assert!(passes_legacy_axis_gate(&def.rules.legality.legacy));
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
fn wish_outside_game_scope_default_is_the_deck_construction_policy_not_a_cr_mandate() {
    // Pins the intended policy this axis encodes: PostM10SideboardOnly is
    // the default (modern deck-construction/tournament restriction, CR
    // 100.4), distinct from PreM10ReachesExile (the historical templating
    // difference). Neither CR 400.11 nor CR 400.11a themselves restrict
    // "outside the game" to only the sideboard — see the type's doc
    // comment — so this test exists to catch a future change accidentally
    // flipping which variant is the default, since nothing else enforces
    // it yet (Phase 2cd wires the real behavior).
    use engine::types::custom_format::WishOutsideGameScope;
    assert_eq!(
        WishOutsideGameScope::default(),
        WishOutsideGameScope::PostM10SideboardOnly
    );
    assert_ne!(
        WishOutsideGameScope::default(),
        WishOutsideGameScope::PreM10ReachesExile
    );
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
            Ok(expected),
            "{format:?}"
        );
    }
}

#[test]
fn commander_eligibility_rule_from_source_format_rejects_custom_without_panicking() {
    // The maintainer's review found this public function still panicked on
    // GameFormat::Custom, a value any external caller can hold. Confirms it
    // now returns a typed error instead of terminating.
    assert!(
        CommanderEligibilityRule::from_source_format(GameFormat::Custom(CustomFormatId(1)))
            .is_err()
    );
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
        let config = FormatConfig::for_format(meta.format).unwrap();
        assert!(matches!(
            config.format.default_deck_copy_limit(),
            DeckCopyLimit::Unlimited | DeckCopyLimit::UpTo(_)
        ));
    }
}

#[test]
fn format_config_for_format_rejects_custom() {
    // The public-factory panic CodeRabbit/the maintainer flagged: a bare
    // GameFormat::Custom parsed from external input must not terminate the
    // process here — for_format has no CustomFormatRules to build from.
    assert!(FormatConfig::for_format(GameFormat::Custom(CustomFormatId(1))).is_err());
}

#[test]
fn custom_format_sideboard_policy_returns_disclosed_fallback_not_panic() {
    assert_eq!(
        GameFormat::Custom(CustomFormatId(1)).sideboard_policy(),
        SideboardPolicy::Forbidden
    );
}

#[test]
fn custom_format_uses_commander_rejects_without_panicking() {
    // Unlike sideboard_policy/default_deck_copy_limit, uses_commander has no
    // safe disclosed-fallback value: a Custom format can legitimately
    // resolve to a commander-using configuration, so `false` would be a
    // silently wrong answer rather than a safe default. This is a public
    // query callable with any GameFormat, including one parsed straight
    // from untrusted input (GameFormat::from_str accepts any
    // "Custom:<u16>" string) — it must return a typed error, not panic.
    assert!(GameFormat::Custom(CustomFormatId(1))
        .uses_commander()
        .is_err());
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

// `evaluate_deck_compatibility` is the UI-HINT entry point (it feeds the
// lobby's live deck-legality chip via `classifyCompatResult`, where `None`
// already means "idle"/no opinion). The engine cannot evaluate Custom-format
// legality yet — no per-card `CustomFormatRules` resolver exists — so a hard
// "illegal" verdict would assert a rules claim nothing computed. Both
// dispatches (summary and full) therefore answer "no opinion". The ENFORCING
// paths are covered separately and still fail closed:
// `validate_name_deck_for_format_full` below, plus
// `validate_deck_for_format` / `evaluate_deck_format_gate` in
// `deck_validation.rs`'s own test module.

#[test]
fn custom_format_deck_compatibility_summary_reports_no_opinion() {
    use engine::database::CardDatabase;
    use engine::game::deck_validation::{evaluate_deck_compatibility, DeckCompatibilityRequest};

    let db = CardDatabase::from_json_str("{}").expect("empty card database");
    let request = DeckCompatibilityRequest {
        selected_format: Some(GameFormat::Custom(CustomFormatId(1))),
        summary_only: true,
        ..Default::default()
    };
    let result = evaluate_deck_compatibility(&db, &request);
    assert_eq!(result.selected_format_compatible, None);
    assert!(result.selected_format_reasons.is_empty());
}

#[test]
fn custom_format_deck_compatibility_reports_no_opinion() {
    use engine::database::CardDatabase;
    use engine::game::deck_validation::{evaluate_deck_compatibility, DeckCompatibilityRequest};

    let db = CardDatabase::from_json_str("{}").expect("empty card database");
    let request = DeckCompatibilityRequest {
        selected_format: Some(GameFormat::Custom(CustomFormatId(1))),
        summary_only: false,
        ..Default::default()
    };
    let result = evaluate_deck_compatibility(&db, &request);
    assert_eq!(result.selected_format_compatible, None);
    assert!(result.selected_format_reasons.is_empty());
}

#[test]
fn validate_name_deck_for_format_full_rejects_custom_format_honestly() {
    use engine::database::CardDatabase;
    use engine::game::deck_validation::validate_name_deck_for_format_full;

    let db = CardDatabase::from_json_str("{}").expect("empty card database");
    // The real signature takes every deck slot explicitly, plus the
    // CR 903.13f(3) draft set codes, a resolved `FormatConfig`, a match type,
    // and a player count. Passing the config (not a bare `GameFormat`) is the
    // point: a Custom format's declared rules only exist on the config, so
    // this is the shape a future resolver would read.
    let custom_config = FormatConfig {
        format: GameFormat::Custom(CustomFormatId(1)),
        custom_rules: Some(Box::new(sample_rules(1))),
        ..FormatConfig::standard()
    };
    let result = validate_name_deck_for_format_full(
        &db,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &custom_config,
        None,
        2,
    );
    match result {
        Err(reasons) => assert!(reasons.iter().any(|r| r.contains("not yet supported"))),
        Ok(()) => panic!("expected Custom format validation to be rejected as not yet supported"),
    }
}

#[test]
fn companion_candidates_returns_empty_for_custom_format_without_panicking() {
    use engine::database::CardDatabase;
    use engine::game::deck_validation::{companion_candidates, DeckCompatibilityRequest};

    let db = CardDatabase::from_json_str("{}").expect("empty card database");
    let request = DeckCompatibilityRequest {
        selected_format: Some(GameFormat::Custom(CustomFormatId(1))),
        ..Default::default()
    };
    // Exercises the exact guard added to companion_candidates: without it,
    // `GameFormat::Custom(_).uses_commander()` inside the function's own
    // `Option::filter` would panic. This is the direct production entry
    // point `companion_candidates_js` (engine-wasm) calls with untrusted
    // input — the other Custom tests above only cover it indirectly via
    // `evaluate_deck_compatibility`.
    assert_eq!(companion_candidates(&db, &request), Vec::<String>::new());
}

// The authoritative FormatConfig ingress. Phase 1a rejected EVERY
// externally-deserialized Custom FormatConfig outright, because no resolver
// existed to derive this struct's own runtime fields (command_zone,
// commander_damage_threshold, uses_commander, singleton, ...) FROM
// custom_rules.structural — they were two independently-writable
// representations of the same state with nothing cross-checking them. Phase
// 1c builds that resolver (FormatConfig::for_custom_rules), so the boundary
// now accepts a Custom payload exactly when it equals what the resolver
// derives from the payload's own custom_rules (allow_debug_actions excepted,
// being a session capability rather than a format rule), and rejects
// anything else. Each value below is constructed directly in Rust (bypassing
// Deserialize, which has no reason to reject it going the other way) and
// round-tripped through `serde_json` — the only way to exercise
// FormatConfig's real Deserialize impl without hand-guessing its full field
// set.

/// A fully self-consistent active Custom config: exactly what the resolver
/// derives from `sample_rules(id)`, i.e. the shape a legitimate Axis-A save
/// resolves to when a player selects it.
fn sample_custom_config(id: u16) -> FormatConfig {
    FormatConfig::for_custom_rules(&sample_rules(id))
}

/// Asserts the rejection came from the resolver re-derivation check, not
/// from some unrelated deserialization failure (a malformed field, a type
/// mismatch) that would also make `.is_err()` pass vacuously.
fn assert_rejected_as_structural_mismatch<T: std::fmt::Debug>(
    result: Result<T, serde_json::Error>,
) {
    let error = result.expect_err("expected deserialization to be rejected");
    assert!(
        error
            .to_string()
            .contains("contradicts its own custom_rules.structural"),
        "expected the resolver-mismatch rejection message, got: {error}"
    );
}

#[test]
fn format_config_deserialization_rejects_custom_without_matching_rules() {
    let invalid = FormatConfig {
        format: GameFormat::Custom(CustomFormatId(1)),
        custom_rules: None,
        ..FormatConfig::standard()
    };
    let json = serde_json::to_value(&invalid).unwrap();
    let error = serde_json::from_value::<FormatConfig>(json)
        .expect_err("a Custom format with no custom_rules must be rejected");
    assert!(
        error.to_string().contains("custom_rules is None"),
        "expected the id-consistency rejection message, got: {error}"
    );
}

#[test]
fn format_config_deserialization_rejects_custom_with_mismatched_rules_id() {
    let invalid = FormatConfig {
        format: GameFormat::Custom(CustomFormatId(5)),
        custom_rules: Some(Box::new(sample_rules(7))),
        ..FormatConfig::standard()
    };
    let json = serde_json::to_value(&invalid).unwrap();
    let error = serde_json::from_value::<FormatConfig>(json)
        .expect_err("a Custom format whose custom_rules.id disagrees must be rejected");
    assert!(
        error.to_string().contains("custom_rules.id is"),
        "expected the id-consistency rejection message, got: {error}"
    );
}

#[test]
fn format_config_deserialization_accepts_a_fully_consistent_custom_config() {
    // The Phase 1c behavior change: custom_rules.id matches AND every
    // runtime field is exactly what FormatConfig::for_custom_rules derives
    // from custom_rules.structural, so there is nothing left for the
    // boundary to distrust. Phase 1a rejected this same payload outright.
    let config = sample_custom_config(5);
    let json = serde_json::to_value(&config).unwrap();
    let back = serde_json::from_value::<FormatConfig>(json)
        .expect("a resolver-consistent Custom config must be accepted");
    assert_eq!(back, config);
}

#[test]
fn format_config_deserialization_rejects_matching_id_but_structurally_contradictory_payload() {
    // The specific hostile case the maintainer's review named: a
    // matching-id Custom payload whose CommandZoneMode declares Disabled in
    // custom_rules.structural while FormatConfig's own independent
    // command_zone/uses_commander/commander_damage_threshold fields claim
    // the format DOES use a command zone. An id-only consistency check would
    // accept this; the resolver re-derivation does not. Built by mutating an
    // otherwise-valid resolved config, so the ONLY thing wrong with it is
    // the contradiction under test.
    let mut contradictory = sample_custom_config(5);
    assert_eq!(
        contradictory
            .custom_rules
            .as_ref()
            .unwrap()
            .structural
            .command_zone_mode,
        CommandZoneMode::Disabled,
        "fixture precondition: the declared rules must have no command zone"
    );
    contradictory.command_zone = true;
    contradictory.uses_commander = true;
    contradictory.commander_damage_threshold = Some(21);
    let json = serde_json::to_value(&contradictory).unwrap();
    assert_rejected_as_structural_mismatch(serde_json::from_value::<FormatConfig>(json));
}

#[test]
fn format_config_deserialization_rejects_a_custom_payload_forging_a_looser_copy_limit() {
    // The Custom-format sibling of the built-in forged-copy-limit attack
    // below: custom_rules.structural declares UpTo(4), but the runtime field
    // the whole engine actually reads (max_deck_copies and every evaluate_*/
    // quick_* dispatch) claims Unlimited. The built-in branch's
    // permits_no_more_than check never runs for Custom — the resolver
    // equality check is what closes this, and this test is what proves it
    // does, since a resolver that simply copied the payload's own runtime
    // field through would pass everything else.
    let mut forged = sample_custom_config(5);
    forged.default_deck_copy_limit = DeckCopyLimit::Unlimited;
    let json = serde_json::to_value(&forged).unwrap();
    assert_rejected_as_structural_mismatch(serde_json::from_value::<FormatConfig>(json));
}

#[test]
fn format_config_deserialization_rejects_a_custom_payload_declaring_an_unimplemented_legacy_axis() {
    // Hostile fixture: structurally self-consistent (the resolver check
    // would pass), but custom_rules.legality.legacy declares
    // ManaBurnPolicy::Obsolete — a LegacyAxis not in IMPLEMENTED_LEGACY_AXES.
    // Accepting it would promise mana-burn behavior no engine code enforces.
    // The registry gate alone does not cover this: a deserialized Custom
    // config never passes through custom_format_registry().
    let mut rules = sample_rules(5);
    rules.legality.legacy.mana_burn = ManaBurnPolicy::Obsolete;
    let config = FormatConfig::for_custom_rules(&rules);
    let json = serde_json::to_value(&config).unwrap();
    let error = serde_json::from_value::<FormatConfig>(json)
        .expect_err("a Custom payload declaring an unimplemented legacy axis must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("LegacyRuleSet axis the engine does not implement"),
        "expected the legacy-axis rejection message, got: {error}"
    );
    // Distinct from the structural-mismatch rejection: this payload IS
    // structurally consistent, and conflating the two messages would hide
    // which gate fired.
    assert!(
        !message.contains("contradicts its own custom_rules.structural"),
        "the legacy-axis rejection must be distinguishable from the structural one, got: {error}"
    );
}

#[test]
fn format_config_deserialization_accepts_a_custom_config_with_default_legacy_rules() {
    // Positive control paired with the hostile legacy-axis fixture above: an
    // all-default LegacyRuleSet (every axis at its modern value, which is
    // what every Axis-A lobby save declares) must pass the same gate, so the
    // rejection above cannot be passing because Custom is refused wholesale.
    let rules = sample_rules(5);
    assert_eq!(
        rules.legality.legacy,
        LegacyRuleSet::default(),
        "fixture precondition: the sample must declare no non-default axis"
    );
    let json = serde_json::to_value(FormatConfig::for_custom_rules(&rules)).unwrap();
    assert!(serde_json::from_value::<FormatConfig>(json).is_ok());
}

#[test]
fn format_config_deserialization_ignores_allow_debug_actions_in_the_custom_equality_check() {
    // allow_debug_actions is a per-session capability (sandbox debug
    // actions), orthogonal to format and not derivable from
    // custom_rules — the resolver always emits false, so a strict
    // whole-struct equality check would reject every sandboxed Custom game.
    // Both values must round-trip; the paired assertions are what prove the
    // field is genuinely excluded rather than coincidentally matching.
    for allow_debug_actions in [true, false] {
        let mut config = sample_custom_config(5);
        config.allow_debug_actions = allow_debug_actions;
        let json = serde_json::to_value(&config).unwrap();
        let back = serde_json::from_value::<FormatConfig>(json)
            .unwrap_or_else(|error| panic!("allow_debug_actions={allow_debug_actions}: {error}"));
        assert_eq!(back, config);
        assert_eq!(back.allow_debug_actions, allow_debug_actions);
    }
}

#[test]
fn format_config_deserialization_rejects_a_built_in_with_a_looser_forged_copy_limit() {
    // The exact hostile payload named in the maintainer's Finding 1 review:
    // {"format":"Standard","default_deck_copy_limit":{"type":"Unlimited"},...}.
    // Built as a real FormatConfig value and round-tripped through
    // serde_json — a hand-typed bare-string literal like
    // "default_deck_copy_limit":"Unlimited" would fail with a type
    // mismatch before ever reaching the new check (DeckCopyLimit's
    // `#[serde(tag = "type", content = "data")]` shape only accepts
    // {"type":"Unlimited"}), which would make this test pass for the wrong
    // reason. Standard's true CR 100.2a ceiling is UpTo(4); accepting
    // Unlimited would let max_deck_copies (and, pre-fix, admission)
    // disclose/enforce a 60-copy Lightning Bolt deck as legal.
    let mut forged = FormatConfig::standard();
    forged.default_deck_copy_limit = DeckCopyLimit::Unlimited;
    let json = serde_json::to_value(&forged).unwrap();
    let error = serde_json::from_value::<FormatConfig>(json)
        .expect_err("a Standard payload forging Unlimited copies must be rejected");
    assert!(
        error.to_string().contains("more permissive"),
        "expected the copy-limit rejection message (proving this specific check fired, \
         not some other deserialize failure), got: {error}"
    );
}

#[test]
fn format_config_deserialization_rejects_commander_with_a_looser_forged_copy_limit() {
    // Same attack against a command-zone singleton format: forging UpTo(4)
    // in place of Commander's true CR 903.5b ceiling (UpTo(1)) would let a
    // 4-of Sol Ring through.
    let mut forged = FormatConfig::commander();
    forged.default_deck_copy_limit = DeckCopyLimit::UpTo(4);
    let json = serde_json::to_value(&forged).unwrap();
    let error = serde_json::from_value::<FormatConfig>(json)
        .expect_err("a Commander payload forging UpTo(4) copies must be rejected");
    assert!(
        error.to_string().contains("more permissive"),
        "expected the copy-limit rejection message, got: {error}"
    );
}

#[test]
fn format_config_deserialization_accepts_every_builtin_registry_default_copy_limit() {
    // Paired positive control: every registry built-in's own correct,
    // untouched default_deck_copy_limit must still round-trip successfully
    // — the new check must not reject legitimate payloads. Iterates
    // GameFormat::registry() dynamically rather than a hardcoded count, so
    // this stays correct as formats are added.
    for meta in GameFormat::registry() {
        let config = FormatConfig::for_format(meta.format).unwrap();
        let json = serde_json::to_value(&config).unwrap();
        assert!(
            serde_json::from_value::<FormatConfig>(json).is_ok(),
            "{:?}: a config using the format's own real default_deck_copy_limit must be accepted",
            meta.format
        );
    }
}

#[test]
fn format_config_deserialization_accepts_a_stricter_than_truth_copy_limit() {
    // A declared value STRICTER than truth (including the pre-existing
    // default_deck_copy_limit_fallback() == UpTo(1) that a legacy payload
    // predating this field resolves to) can only under-permit, never admit
    // an illegal deck, and must not be rejected — this is the backward-
    // compatibility case the strict-equality alternative would have broken.
    let mut stricter = FormatConfig::standard();
    stricter.default_deck_copy_limit = DeckCopyLimit::UpTo(1);
    let json = serde_json::to_value(&stricter).unwrap();
    assert!(serde_json::from_value::<FormatConfig>(json).is_ok());
}

#[test]
fn persisted_game_state_restore_accepts_a_normal_built_in_game() {
    // Paired positive control for the rejection test below: a normal
    // built-in game's persisted state must still round-trip successfully,
    // so the rejection test can't be passing because *nothing* deserializes.
    use engine::types::game_state::{GameState, PersistedGameState};

    let state = GameState::new(FormatConfig::standard(), 2, 42);
    let persisted = PersistedGameState::capture(state);
    let json = serde_json::to_value(&persisted).unwrap();
    assert!(
        serde_json::from_value::<PersistedGameState>(json).is_ok(),
        "restoring a persisted built-in-format GameState must succeed"
    );
}

#[test]
fn persisted_game_state_restore_rejects_a_structurally_contradictory_custom_format_config() {
    // Empirically proves the rejection reaches the real restore/resume
    // chokepoint engine-wasm's decode_restored_game_state calls
    // (serde_json::from_value::<PersistedGameState>), not just a
    // FormatConfig-in-isolation unit test. Builds a normal, valid two-player
    // GameState, swaps in a Custom format_config the same way an
    // attacker-controlled restore payload would, then round-trips the whole
    // persisted envelope.
    //
    // Phase 1c fixture redesign: this test used to swap in a Custom config
    // built from `..FormatConfig::standard()`, which was rejected by Phase
    // 1a's categorical "no Custom at this boundary" rule. That rule is gone,
    // so the fixture now carries a DELIBERATE, explicit contradiction — a
    // singleton runtime field the declared StructuralRules does not entail —
    // rather than passing for a reason that no longer exists.
    use engine::types::game_state::{GameState, PersistedGameState};

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let mut config = sample_custom_config(5);
    assert!(
        !config.singleton,
        "fixture precondition: the declared rules must be non-singleton"
    );
    config.singleton = true;
    state.format_config = config;
    let persisted = PersistedGameState::capture(state);
    let json = serde_json::to_value(&persisted).unwrap();
    let error = serde_json::from_value::<PersistedGameState>(json).expect_err(
        "restoring a persisted GameState whose Custom format_config contradicts its own \
         custom_rules must be rejected, mirroring engine-wasm's decode_restored_game_state \
         chokepoint",
    );
    assert!(
        error
            .to_string()
            .contains("contradicts its own custom_rules.structural"),
        "expected the resolver-mismatch rejection to propagate through the persisted envelope, \
         got: {error}"
    );
}

#[test]
fn persisted_game_state_restore_accepts_a_consistent_custom_format_config() {
    // Paired positive control for the rejection above, and the Phase 1c
    // behavior change at the real restore chokepoint: a Custom format_config
    // that IS exactly what the resolver derives from its own custom_rules
    // must now restore successfully. Without this, the rejection test could
    // pass for the old categorical reason.
    use engine::types::game_state::{GameState, PersistedGameState};

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    state.format_config = sample_custom_config(5);
    let persisted = PersistedGameState::capture(state);
    let json = serde_json::to_value(&persisted).unwrap();
    assert!(
        serde_json::from_value::<PersistedGameState>(json).is_ok(),
        "restoring a persisted GameState with a resolver-consistent Custom format_config must \
         succeed"
    );
}

#[test]
fn companion_reveal_check_does_not_panic_for_an_in_memory_custom_format_config() {
    // The maintainer's review found that rejecting Custom at the external
    // deserialization boundary doesn't make GameFormat::Custom safe
    // in-memory: game/companion.rs's check_companion_reveal reads
    // state.format_config on ANY live GameState (built-in or Custom,
    // constructed directly in Rust, never through Deserialize), and used to
    // call the bare GameFormat's uses_commander() internally — which
    // panics for Custom. This proves the fix (reading the resolved
    // FormatConfig.uses_commander field instead) reaches the real
    // production entry point rather than just the unit-level helpers.
    use engine::types::game_state::{GameState, PlayerDeckPool};
    use engine::types::PlayerId;

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let rules = sample_rules(5);
    state.format_config = FormatConfig {
        format: GameFormat::Custom(rules.id),
        custom_rules: Some(Box::new(rules)),
        uses_commander: true,
        ..FormatConfig::standard()
    };
    state.deck_pools = vec![
        PlayerDeckPool {
            player: PlayerId(0),
            ..Default::default()
        },
        PlayerDeckPool {
            player: PlayerId(1),
            ..Default::default()
        },
    ];

    // No companion is registered in either empty pool, so the honest answer
    // is "no reveal offer" — the discriminating claim is that this returns
    // a defined result at all, rather than panicking on GameFormat::Custom's
    // uses_commander() inside companion_offers/companion_starting_deck.
    let result = engine::game::companion::check_all_companion_reveals(&state);
    assert!(result.is_none());
}

#[test]
fn custom_format_unlimited_sideboard_survives_deck_loading() {
    // The maintainer's review found GameFormat::sideboard_policy()'s
    // disclosed Forbidden fallback for Custom silently discarded a real,
    // already-known declared policy sitting in
    // custom_rules.structural.sideboard_policy — deck_loading.rs trusted the
    // bare-GameFormat fallback and emptied the sideboard even for a Custom
    // format whose real policy is Unlimited. FormatConfig now stores its own
    // sideboard_policy field (mirroring uses_commander/supplies_fixed_deck),
    // and deck_loading.rs reads that instead. This proves the fix through
    // the real production entry point, load_deck_into_state: builds an
    // in-memory Custom FormatConfig with sideboard_policy: Unlimited, loads
    // a deck payload with a nonempty sideboard, and confirms the sideboard
    // actually survives into the resulting deck pool rather than being
    // silently dropped to empty.
    use engine::game::deck_loading::{
        load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload,
    };
    use engine::types::card::CardFace;
    use engine::types::game_state::GameState;

    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let rules = sample_rules(5);
    state.format_config = FormatConfig {
        format: GameFormat::Custom(rules.id),
        custom_rules: Some(Box::new(rules)),
        sideboard_policy: SideboardPolicy::Unlimited,
        ..FormatConfig::standard()
    };

    let sideboard_card = DeckEntry {
        card: CardFace {
            name: "Test Sideboard Card".to_string(),
            ..Default::default()
        },
        count: 1,
    };
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            sideboard: vec![sideboard_card.clone()],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            sideboard: vec![sideboard_card],
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    let p0 = state
        .deck_pools
        .iter()
        .find(|pool| pool.player == engine::types::PlayerId(0))
        .expect("player 0 deck pool must exist after loading");
    assert_eq!(
        p0.current_sideboard.len(),
        1,
        "a Custom format with sideboard_policy: Unlimited must not have its sideboard dropped"
    );
    assert_eq!(p0.current_sideboard[0].card.name, "Test Sideboard Card");
}

// Axis A (Phase 1c): CustomFormatDef::from_lobby_config captures a lobby's
// live built-in FormatConfig as a saved DEFINITION, and
// FormatConfig::for_custom_rules is the inverse — the shared resolver that
// turns a definition back into the active config a game runs on.

#[test]
fn from_lobby_config_rejects_archenemy_source() {
    // CR 408.1 + CR 408.3 + CR 904.3: Archenemy's command zone holds a
    // supplementary scheme deck, not a commander — one member of the general
    // "deck_loading.rs grants an auxiliary deck/component keyed on this
    // literal GameFormat" class (see
    // GameFormat::has_unrepresentable_auxiliary_deck_component), which also
    // covers Planechase and Momir below.
    let error = CustomFormatDef::from_lobby_config("Archy".to_string(), &FormatConfig::archenemy())
        .expect_err("Archenemy must not be saveable as a custom format");
    assert!(
        error.to_string().contains("auxiliary deck or component"),
        "expected the auxiliary-deck-component rejection, got: {error}"
    );
}

#[test]
fn from_lobby_config_rejects_momir_source() {
    // CR 109.4c + CR 114.1: Momir's command zone holds a game-start emblem,
    // granted by deck_loading.rs keyed off GameFormat::Momir itself rather
    // than off any StructuralRules field. Same defect class as Archenemy and
    // Planechase, for a different underlying reason.
    let error = CustomFormatDef::from_lobby_config("Momo".to_string(), &FormatConfig::momir())
        .expect_err("Momir must not be saveable as a custom format");
    assert!(
        error.to_string().contains("auxiliary deck or component"),
        "expected the auxiliary-deck-component rejection, got: {error}"
    );
}

#[test]
fn from_lobby_config_rejects_planechase_source() {
    // CR 901.15a: Planechase's shared communal planar deck is granted by
    // deck_loading.rs's `load_shared_planar_deck`, keyed on
    // GameFormat::Planechase itself. Unlike Archenemy/Momir, Planechase's
    // `command_zone` is false — FormatConfig::planechase() sets
    // command_zone: false — so this format would otherwise fall straight
    // through the command-zone/eligibility check below to
    // CommandZoneMode::Disabled and save "successfully," silently dropping
    // the planar deck. has_unrepresentable_auxiliary_deck_component is the
    // only guard that reaches it.
    let error =
        CustomFormatDef::from_lobby_config("Planar".to_string(), &FormatConfig::planechase())
            .expect_err("Planechase must not be saveable as a custom format");
    assert!(
        error.to_string().contains("auxiliary deck or component"),
        "expected the auxiliary-deck-component rejection, got: {error}"
    );
}

#[test]
fn from_lobby_config_accepts_a_commander_style_command_zone_source() {
    // Positive sibling for the two rejections above: a command-zone format
    // whose zone really does hold a commander (CR 903.13g routes Commander
    // Draft through CR 903.3's eligibility test) saves fine. Without this,
    // the rejections could be passing because command_zone: true is refused
    // outright.
    let def = CustomFormatDef::from_lobby_config(
        "Drafty Commander".to_string(),
        &FormatConfig::commander_draft(),
    )
    .expect("a commander-style source must be saveable");
    assert_eq!(
        def.rules.structural.command_zone_mode,
        CommandZoneMode::Enabled {
            commander_damage_threshold: Some(21),
            eligibility_rule: CommanderEligibilityRule::Standard,
        }
    );
}

#[test]
fn from_lobby_config_rejects_an_empty_or_whitespace_only_name() {
    // Rejected explicitly rather than saved with an empty label/short_label:
    // there would be nothing to label the saved format with, and the badge
    // code derived from it would be empty too.
    for name in ["", "   ", "\t\n "] {
        let result =
            CustomFormatDef::from_lobby_config(name.to_string(), &FormatConfig::standard());
        let error = match result {
            Ok(def) => panic!("name {name:?} must be rejected, got {def:?}"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("non-empty format name"),
            "{name:?}: expected the empty-name rejection, got: {error}"
        );
    }
}

#[test]
fn from_lobby_config_rejects_a_custom_source_whatever_its_command_zone_flag() {
    // Re-saving a save is out of scope: the source's own legality rules
    // (legal_sets/banned/restricted/legacy) have no home in this conversion
    // and would be silently dropped. Both flag values are exercised because
    // the Custom check must not depend on reaching the command-zone branch.
    let mut with_zone = sample_custom_config(5);
    with_zone.command_zone = true;
    for config in [sample_custom_config(5), with_zone] {
        let error = CustomFormatDef::from_lobby_config("Re-save".to_string(), &config)
            .expect_err("a Custom source must not be re-saveable");
        assert!(
            error.to_string().contains("cannot save Custom"),
            "expected the Custom-source rejection, got: {error}"
        );
    }
}

#[test]
fn from_lobby_config_uses_the_reserved_lobby_save_sentinel_id() {
    let def = CustomFormatDef::from_lobby_config("Sentinel".to_string(), &FormatConfig::standard())
        .expect("a built-in source must be saveable");
    assert_eq!(def.rules.id, LOBBY_SAVE_CUSTOM_FORMAT_ID);
}

#[test]
fn from_lobby_config_leaves_legality_and_reprint_metadata_at_lobby_save_defaults() {
    // A lobby save models no published paper ruleset, so it declares no
    // card pool, no banned/restricted list, no historical rules era, and no
    // reprint intent.
    let def = CustomFormatDef::from_lobby_config("Plain".to_string(), &FormatConfig::standard())
        .expect("a built-in source must be saveable");
    assert_eq!(def.rules.legality.legal_sets, None);
    assert!(def.rules.legality.banned.is_empty());
    assert!(def.rules.legality.restricted.is_empty());
    assert_eq!(def.rules.legality.legacy, LegacyRuleSet::default());
    assert_eq!(def.reprint_policy, None);
    assert_eq!(def.printing_fidelity, PrintingFidelity::NotApplicable);
}

#[test]
fn lobby_save_round_trips_every_structural_field_back_through_the_resolver() {
    // Full-fidelity round trip on a source whose fields are deliberately
    // NOT the common defaults: Tiny Leaders is the command-zone-without-
    // commander-damage shape (CommandZoneMode::Enabled with a None
    // threshold), with an Exactly deck-size rule, singleton on, a
    // Limited(10) sideboard and an UpTo(1) copy limit — every one of which a
    // partial capture would silently replace with a default.
    use engine::types::format::RangeOfInfluenceConfig;

    let mut source = FormatConfig::tiny_leaders();
    source.starting_life = 33;
    source.max_players = 5;
    source.team_based = true;
    source.range_of_influence = Some(Box::new(RangeOfInfluenceConfig {
        default_range: 1,
        player_overrides: Default::default(),
    }));

    let def = CustomFormatDef::from_lobby_config("Tiny Round Trip".to_string(), &source)
        .expect("a commander-style built-in source must be saveable");
    let resolved = FormatConfig::for_custom_rules(&def.rules);

    assert_eq!(resolved.starting_life, 33);
    assert_eq!(resolved.min_players, source.min_players);
    assert_eq!(resolved.max_players, 5);
    assert_eq!(resolved.deck_size, DeckSizeRule::Exactly(50));
    assert!(resolved.singleton);
    assert!(resolved.team_based);
    assert_eq!(resolved.range_of_influence, source.range_of_influence);
    assert_eq!(resolved.sideboard_policy, SideboardPolicy::Limited(10));
    assert_eq!(resolved.default_deck_copy_limit, DeckCopyLimit::UpTo(1));
    // CR 903.10a / CR 704.6c: a command zone with no commander-damage
    // threshold is a real format class — uses_commander must stay false
    // rather than being forced true by `Enabled` alone.
    assert!(resolved.command_zone);
    assert_eq!(resolved.commander_damage_threshold, None);
    assert!(!resolved.uses_commander);
    // Fixed by the resolver, never captured from the source.
    assert_eq!(
        resolved.format,
        GameFormat::Custom(LOBBY_SAVE_CUSTOM_FORMAT_ID)
    );
    assert_eq!(resolved.custom_rules.as_deref(), Some(&def.rules));
    assert!(!resolved.supplies_fixed_deck);
    assert_eq!(resolved.archenemy_player, None);
    assert!(!resolved.allow_debug_actions);
}

#[test]
fn resolver_derives_uses_commander_from_the_declared_damage_threshold() {
    // The paired half of the Tiny-Leaders case above: the same Enabled
    // variant WITH a threshold must resolve to uses_commander: true, so the
    // assertion above cannot be satisfied by hardcoding false.
    let mut rules = sample_rules(5);
    rules.structural.command_zone_mode = CommandZoneMode::Enabled {
        commander_damage_threshold: Some(21),
        eligibility_rule: CommanderEligibilityRule::Standard,
    };
    let resolved = FormatConfig::for_custom_rules(&rules);
    assert!(resolved.command_zone);
    assert_eq!(resolved.commander_damage_threshold, Some(21));
    assert!(resolved.uses_commander);

    rules.structural.command_zone_mode = CommandZoneMode::Disabled;
    let resolved = FormatConfig::for_custom_rules(&rules);
    assert!(!resolved.command_zone);
    assert_eq!(resolved.commander_damage_threshold, None);
    assert!(!resolved.uses_commander);
}

#[test]
fn a_lobby_save_resolves_to_a_config_the_deserialize_boundary_accepts() {
    // End-to-end production chain: save a live lobby config -> resolve the
    // saved definition -> ship it across the wire. Every Axis-A format a host
    // saves must survive the FormatConfig ingress, or the feature is
    // unusable no matter how well each half works alone.
    for source in [
        FormatConfig::standard(),
        FormatConfig::commander(),
        // CR 903.13f(1)/(2): the only saveable source combining a command
        // zone with DeckSizeRule::Minimum and an Unlimited copy limit — a
        // structural shape none of the other sources below exercise.
        FormatConfig::commander_draft(),
        FormatConfig::tiny_leaders(),
        FormatConfig::two_headed_giant(),
        FormatConfig::limited(),
    ] {
        let def = CustomFormatDef::from_lobby_config("Saved Format".to_string(), &source)
            .unwrap_or_else(|error| panic!("{:?}: {error}", source.format));
        let resolved = FormatConfig::for_custom_rules(&def.rules);

        // `back == resolved` below only proves the resolver and serde agree
        // with THEMSELVES — a `from_lobby_config`/`for_custom_rules` mapping
        // bug that swaps or drops a field the same way on both sides would
        // still pass it. Compare `resolved` against `source` directly for
        // every field the charter's documented mapping
        // (IMPLEMENTATION_PLAN.md's Phase 1c section) calls a direct copy or
        // a lossless CommandZoneMode round trip, so a real capture/resolve
        // regression is caught here instead.
        assert_eq!(
            resolved.starting_life, source.starting_life,
            "{:?}",
            source.format
        );
        assert_eq!(
            resolved.min_players, source.min_players,
            "{:?}",
            source.format
        );
        assert_eq!(
            resolved.max_players, source.max_players,
            "{:?}",
            source.format
        );
        assert_eq!(resolved.deck_size, source.deck_size, "{:?}", source.format);
        assert_eq!(resolved.singleton, source.singleton, "{:?}", source.format);
        assert_eq!(
            resolved.team_based, source.team_based,
            "{:?}",
            source.format
        );
        assert_eq!(
            resolved.range_of_influence, source.range_of_influence,
            "{:?}",
            source.format
        );
        assert_eq!(
            resolved.sideboard_policy, source.sideboard_policy,
            "{:?}",
            source.format
        );
        assert_eq!(
            resolved.default_deck_copy_limit, source.default_deck_copy_limit,
            "{:?}",
            source.format
        );
        // CommandZoneMode-derived, not direct-copy — but every built-in
        // source's command-zone shape round-trips losslessly through it.
        assert_eq!(
            resolved.command_zone, source.command_zone,
            "{:?}",
            source.format
        );
        assert_eq!(
            resolved.commander_damage_threshold, source.commander_damage_threshold,
            "{:?}",
            source.format
        );
        assert_eq!(
            resolved.uses_commander, source.uses_commander,
            "{:?}",
            source.format
        );
        // Deliberately NOT compared against `source` (per for_custom_rules's
        // own doc comment): `format`/`custom_rules` are fixed to the Custom
        // sentinel, and `archenemy_player`/`supplies_fixed_deck`/
        // `allow_debug_actions` are always reset, never captured.

        let json = serde_json::to_value(&resolved).unwrap();
        let back = serde_json::from_value::<FormatConfig>(json)
            .unwrap_or_else(|error| panic!("{:?}: {error}", source.format));
        assert_eq!(back, resolved);
    }
}

#[test]
fn short_label_is_derived_from_the_name_and_tolerates_short_names() {
    let cases = [
        ("Swedish Old School", "SWE"),
        ("  di-verse!  ", "DIV"),
        // Fewer alphanumerics than the 3-character convention: a shorter
        // code is the documented outcome, not a padded or invented one.
        ("Hi", "HI"),
        ("9", "9"),
    ];
    for (name, expected) in cases {
        let def = CustomFormatDef::from_lobby_config(name.to_string(), &FormatConfig::standard())
            .unwrap_or_else(|error| panic!("{name:?}: {error}"));
        assert_eq!(def.short_label, expected, "{name:?}");
        assert_eq!(def.label, name.trim(), "label is the name, trimmed");
    }
}

#[test]
fn description_is_derived_from_the_structural_rules_not_a_static_string() {
    let commander = CustomFormatDef::from_lobby_config(
        "Commander Save".to_string(),
        &FormatConfig::commander(),
    )
    .expect("commander source saves");
    let limited =
        CustomFormatDef::from_lobby_config("Limited Save".to_string(), &FormatConfig::limited())
            .expect("limited source saves");

    assert!(!commander.description.is_empty());
    assert!(!limited.description.is_empty());
    assert_ne!(
        commander.description, limited.description,
        "two different StructuralRules must describe themselves differently"
    );
    // Content-derived, per field: CR 903.5a's exact-100 singleton rule and
    // CR 100.5's 40-card floor must not read the same way.
    assert!(
        commander.description.contains("100-card singleton"),
        "got: {}",
        commander.description
    );
    assert!(
        limited.description.contains("40-card minimum"),
        "got: {}",
        limited.description
    );
    assert!(commander.description.contains("40 life"));
    assert!(limited.description.contains("20 life"));
}

#[test]
#[should_panic(expected = "reserved as LOBBY_SAVE_CUSTOM_FORMAT_ID")]
fn a_preset_claiming_the_lobby_save_sentinel_id_trips_the_registration_assert() {
    // custom_format_registry() runs this same assert over its own preset
    // list before filtering. Calling the extracted helper directly is what
    // makes the guard testable while the list is still empty — and the
    // assert is a real assert!, not debug_assert!, so it is active in every
    // build profile (neither `release` nor `server-release` in the workspace
    // Cargo.toml overrides debug-assertions).
    let colliding = sample_def(LOBBY_SAVE_CUSTOM_FORMAT_ID.0);
    assert_no_lobby_save_sentinel_collision(&[colliding]);
}

#[test]
fn presets_with_ordinary_ids_pass_the_sentinel_guard() {
    // Paired positive control: the guard must not reject every preset.
    assert_no_lobby_save_sentinel_collision(&[sample_def(1), sample_def(2)]);
    // And the real registry construction path still runs it without firing.
    assert!(engine::types::custom_format::custom_format_registry().is_empty());
}
