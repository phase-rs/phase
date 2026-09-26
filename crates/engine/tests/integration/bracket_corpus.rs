use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use engine::game::{
    assert_floor_matches_population, assert_ratchet_history, estimate_bracket,
    expectation_row_from_reading, held_out_split, load_corpus_dir, load_expectations,
    parse_corpus_fixture, reading_from_estimate, rows_digest, score_corpus, unresolved_names,
    BracketAxis, CommanderBracketTier, CorpusFixture, CorpusFraction, CorpusRuleStatus,
    CorpusSplit, ExpectationRow, GateRegime, LabelBasis, ARMED_RULES, HELD_OUT_MODULUS,
    HELD_OUT_RESIDUE, HELD_OUT_RULE_SENTENCE, MIN_AXIS_POPULATION_N, MIN_BAND_POPULATION_N,
};
use serde_json::Value;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("engine crate should be nested under crates/")
        .to_path_buf()
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("bracket_corpus")
}

fn fixtures() -> Vec<CorpusFixture> {
    load_corpus_dir(&corpus_dir()).expect("committed bracket corpus should load")
}

fn expectations_path() -> PathBuf {
    corpus_dir().join("expectations.json")
}

fn synthetic_fixture(id: &str, basis: LabelBasis) -> CorpusFixture {
    let basis = match basis {
        LabelBasis::RulesDerivation => "rules_derivation",
        LabelBasis::OwnerDeclared => "owner_declared",
        LabelBasis::ThirdPartyPublished => "third_party_published",
    };
    let source_terms = if basis == "third_party_published" {
        r#", "url":"https://invalid.example", "terms_checked_on":"2026-09-26""#
    } else {
        ""
    };
    parse_corpus_fixture(&format!(
        r#"{{
            "id":"{id}",
            "name":"synthetic",
            "label":{{
                "tier":"core",
                "basis":"{basis}",
                "labeller":"test",
                "labelled_on":"2026-09-26",
                "rules_copy_read":"test",
                "axis_counts":{{}},
                "cards_named":{{}}
            }},
            "source":{{
                "captured_on":"2026-09-26",
                "capture_note":"synthetic"{source_terms}
            }},
            "decklist":{{"commander":[""],"main_deck":[]}}
        }}"#
    ))
    .expect("synthetic fixture should parse")
}

#[test]
fn every_corpus_fixture_loads_and_validates() {
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), 8);
    for fixture in fixtures {
        if fixture.label.basis == LabelBasis::ThirdPartyPublished {
            assert!(fixture.source.url.is_some(), "{} lacks a URL", fixture.id);
            assert!(
                fixture.source.terms_checked_on.is_some(),
                "{} lacks a terms-check date",
                fixture.id
            );
        }
    }
}

#[test]
fn corpus_fixture_rejects_unknown_field() {
    let json = r#"{
        "id":"bad-provenance",
        "name":"bad",
        "label":{
            "tier":"core",
            "basis":"owner_declared",
            "labeller":"test",
            "labelled_on":"2026-09-26",
            "rules_copy_read":"test",
            "axis_counts":{},
            "cards_named":{}
        },
        "source":{
            "captured_onn":"2026-09-26",
            "capture_note":"misspelled field"
        },
        "decklist":{"commander":[""],"main_deck":[]}
    }"#;
    assert!(parse_corpus_fixture(json).is_err());
}

#[test]
fn every_corpus_card_resolves_in_the_shared_db() {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };
    for fixture in fixtures() {
        assert_eq!(
            unresolved_names(&fixture, db),
            Vec::<String>::new(),
            "{} contains unresolved card names",
            fixture.id
        );
    }
}

#[test]
fn designed_fixtures_are_excluded_from_every_population_rate() {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };
    let score = score_corpus(&fixtures(), db);
    assert_eq!(
        score.excluded_from_population,
        [
            "designed-001-winter-orb-prison",
            "designed-002-aggravated-assault-combat",
            "designed-003-chained-extra-turns",
            "designed-004-zero-signal",
            "designed-005-four-game-changers",
        ]
    );
    assert_eq!(score.band_agreement.denominator, 3);
    assert!(score
        .per_axis
        .values()
        .all(|axis| axis.agreement.denominator == 3));
    assert!(score.per_axis.values().all(|axis| {
        axis.regime
            == GateRegime::ReportOnly {
                n: 3,
                min_n: MIN_AXIS_POPULATION_N,
            }
    }));
    assert_eq!(
        score.band_regime,
        GateRegime::ReportOnly {
            n: 3,
            min_n: MIN_BAND_POPULATION_N,
        }
    );
}

#[test]
fn held_out_split_is_deterministic_and_order_insensitive() {
    let mut rows: Vec<CorpusFixture> = (0..9)
        .map(|index| {
            synthetic_fixture(&format!("population-{index:03}"), LabelBasis::OwnerDeclared)
        })
        .collect();
    rows.push(synthetic_fixture("designed-a", LabelBasis::RulesDerivation));
    rows.push(synthetic_fixture("designed-b", LabelBasis::RulesDerivation));

    let forward = held_out_split(&rows);
    rows.reverse();
    let reverse = held_out_split(&rows);
    rows.rotate_left(4);
    let rotated = held_out_split(&rows);
    assert_eq!(format!("{forward:?}"), format!("{reverse:?}"));
    assert_eq!(format!("{forward:?}"), format!("{rotated:?}"));
    assert_eq!(forward["designed-a"], CorpusSplit::Designed);
    assert_eq!(forward["designed-b"], CorpusSplit::Designed);

    for index in 0..9 {
        let id = format!("population-{index:03}");
        let expected = if index % HELD_OUT_MODULUS == HELD_OUT_RESIDUE {
            CorpusSplit::HeldOut
        } else {
            CorpusSplit::Train
        };
        assert_eq!(forward[&id], expected);
    }

    let before = forward;
    rows.push(synthetic_fixture(
        "population-zzz",
        LabelBasis::OwnerDeclared,
    ));
    let after = held_out_split(&rows);
    for (id, split) in before {
        assert_eq!(after[&id], split, "adding a trailing id moved {id}");
    }
}

#[test]
fn readme_states_the_held_out_rule_verbatim() {
    let readme = fs::read_to_string(corpus_dir().join("README.md"))
        .expect("bracket corpus README should exist");
    assert!(readme.contains(HELD_OUT_RULE_SENTENCE));
}

#[test]
fn armed_rules_are_exactly_game_changers() {
    assert_eq!(ARMED_RULES, &[BracketAxis::GameChangers]);
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };
    let score = score_corpus(&fixtures(), db);
    // Only the authoritative, dated Game Changers list may gate; the three
    // curated/evidence axes remain report-only while this corpus measures them.
    for axis in [
        BracketAxis::GameChangers,
        BracketAxis::MassLandDenial,
        BracketAxis::ExtraTurns,
        BracketAxis::EfficientTutors,
    ] {
        let expected = if axis == BracketAxis::GameChangers {
            CorpusRuleStatus::Armed
        } else {
            CorpusRuleStatus::Reported
        };
        assert_eq!(score.per_axis[&axis].status, expected);
    }
}

#[test]
fn golden_expectations_match_the_engine() {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };
    let rows: BTreeMap<String, ExpectationRow> = fixtures()
        .into_iter()
        .map(|fixture| {
            let estimate = estimate_bracket(&fixture.decklist, db)
                .unwrap_or_else(|| panic!("{} did not produce an estimate", fixture.id));
            let row = expectation_row_from_reading(&reading_from_estimate(&estimate));
            (fixture.id, row)
        })
        .collect();

    if std::env::var_os("BRACKET_CORPUS_PRINT_EXPECTATIONS").is_some() {
        println!(
            "ROWS_JSON={}\nROWS_DIGEST={}",
            serde_json::to_string_pretty(&rows).expect("expectation rows should serialize"),
            rows_digest(&rows)
        );
        return;
    }

    let expectations = load_expectations(&expectations_path()).expect("expectations should load");
    assert_eq!(rows, expectations.rows);
}

#[test]
fn ratchet_history_is_well_formed() {
    let expectations = load_expectations(&expectations_path()).expect("expectations should load");
    assert_ratchet_history(&expectations, env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|errors| panic!("ratchet history errors: {errors:#?}"));
}

#[test]
fn ratchet_digest_moves_when_a_row_moves() {
    let expectations = load_expectations(&expectations_path()).expect("expectations should load");
    let recorded = rows_digest(&expectations.rows);
    let mut moved = expectations.rows.clone();
    let row = moved.values_mut().next().expect("at least one golden row");
    row.tier = match row.tier {
        CommanderBracketTier::Core => CommanderBracketTier::Upgraded,
        _ => CommanderBracketTier::Core,
    };
    assert_ne!(rows_digest(&moved), recorded);
}

#[test]
fn no_rate_floor_is_recorded_below_its_minimum_n() {
    let fixtures = fixtures();
    let population_n = fixtures
        .iter()
        .filter(|fixture| !fixture.label.basis.is_designed() && fixture.label.disputed.is_none())
        .count();
    assert_eq!(population_n, 3);
    let expectations = load_expectations(&expectations_path()).expect("expectations should load");
    assert!(expectations.band_agreement_floor.is_none());
    assert_floor_matches_population(expectations.band_agreement_floor, population_n)
        .expect("the recorded floor should be legal for this population");
}

#[test]
fn fraction_comparison_uses_no_floats() {
    let cases = [
        ((1, 3), (33_333, 100_000), true),
        ((1, 3), (33_334, 100_000), false),
        ((2, 6), (1, 3), true),
        ((999_999, 1_000_000), (1, 1), false),
        ((1, 0), (0, 1), false),
        ((1, 1), (0, 0), false),
    ];
    for ((left_n, left_d), (right_n, right_d), expected) in cases {
        let left = CorpusFraction {
            numerator: left_n,
            denominator: left_d,
        };
        let right = CorpusFraction {
            numerator: right_n,
            denominator: right_d,
        };
        assert_eq!(left.at_least(right), expected, "{left:?} vs {right:?}");
    }
}

#[test]
fn known_misses_are_reported_with_their_citations() {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: full card-data export not generated");
        return;
    };
    let fixtures = fixtures();
    let score = score_corpus(&fixtures, db);
    assert!(
        score.misses.is_empty(),
        "unexpected axis misses: {:#?}",
        score.misses
    );

    let actual: Vec<_> = score
        .band_misses
        .iter()
        .map(|miss| (miss.id.as_str(), miss.labelled, miss.observed))
        .collect();
    let expected = vec![
        (
            "designed-003-chained-extra-turns",
            CommanderBracketTier::Optimized,
            CommanderBracketTier::Core,
        ),
        (
            "population-001-bundled-heliod",
            CommanderBracketTier::Cedh,
            CommanderBracketTier::Upgraded,
        ),
        (
            "population-002-bundled-inalla",
            CommanderBracketTier::Cedh,
            CommanderBracketTier::Optimized,
        ),
        (
            "population-003-bundled-winota",
            CommanderBracketTier::Cedh,
            CommanderBracketTier::Optimized,
        ),
    ];
    assert_eq!(actual, expected);

    let readme = fs::read_to_string(corpus_dir().join("README.md"))
        .expect("bracket corpus README should exist");
    for miss in &score.band_misses {
        let fixture = fixtures
            .iter()
            .find(|fixture| fixture.id == miss.id)
            .expect("miss should name a fixture");
        assert!(!fixture.source.capture_note.trim().is_empty());
        assert!(readme.contains(&miss.id), "README omits {}", miss.id);
    }
}

#[test]
fn precon_distribution_report_is_presence_gated() {
    let root = repository_root();
    let decks_path = root.join("client/public/decks.json");
    if !decks_path.exists() {
        eprintln!("skipping: client/public/decks.json not generated");
        return;
    }
    let cards_path = root.join("client/public/card-data.json");
    if !cards_path.exists() {
        eprintln!("skipping: client/public/card-data.json not generated");
        return;
    }

    let decks: BTreeMap<String, Value> = serde_json::from_str(
        &fs::read_to_string(decks_path).expect("generated decks should be readable"),
    )
    .expect("generated decks should be valid JSON");
    let cards: BTreeMap<String, Value> = serde_json::from_str(
        &fs::read_to_string(cards_path).expect("generated cards should be readable"),
    )
    .expect("generated cards should be valid JSON");
    let mut histograms: BTreeMap<&str, BTreeMap<u64, u64>> = [
        "game_changer",
        "mass_land_denial",
        "extra_turn",
        "efficient_tutor",
    ]
    .into_iter()
    .map(|axis| (axis, BTreeMap::new()))
    .collect();
    let mut coverage = Vec::new();
    let mut commander_decks = 0_u64;

    for deck in decks
        .values()
        .filter(|deck| deck["type"] == "Commander Deck")
    {
        commander_decks += 1;
        if let Some(value) = deck["coveragePct"].as_u64() {
            coverage.push(value);
        }
        let mut counts: BTreeMap<&str, u64> =
            histograms.keys().copied().map(|axis| (axis, 0)).collect();
        for section in ["commander", "mainBoard"] {
            for card in deck[section].as_array().into_iter().flatten() {
                let Some(name) = card["name"].as_str() else {
                    continue;
                };
                let copies = card["count"].as_u64().unwrap_or(1);
                let Some(signals) = cards
                    .get(&name.to_lowercase())
                    .and_then(|entry| entry.get("bracket_signals"))
                else {
                    continue;
                };
                for (axis, count) in &mut counts {
                    if signals.get(*axis).and_then(Value::as_bool) == Some(true) {
                        *count += copies;
                    }
                }
            }
        }
        for (axis, count) in counts {
            *histograms
                .entry(axis)
                .or_default()
                .entry(count)
                .or_default() += 1;
        }
    }

    let coverage_sum: u64 = coverage.iter().sum();
    println!(
        "Commander precons={commander_decks}; axis_histograms={histograms:?}; coveragePct=min:{:?} max:{:?} sum/count:{coverage_sum}/{}",
        coverage.iter().min(),
        coverage.iter().max(),
        coverage.len()
    );
}
