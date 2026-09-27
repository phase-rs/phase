//! Integration coverage for the two table-backed Commander combo floors.

use std::collections::BTreeSet;

use engine::database::{
    BracketLists, CardDatabase, ComboCardinality, ComboCoverage, ComboEntry, ComboFilterCounts,
    ComboOmission, ComboOutcome, ComboPiece, ComboPieceZone, ComboProvenance, ComboRelevance,
    ComboResource, ComboSetup, ComboTable, ComboTableDoc, EarlyComboReading,
};
use engine::game::bracket_estimate::reconcile;
use engine::game::{
    estimate_bracket, Barometer, BarometerAuthority, BracketAxis, BracketCheckOutcome,
    BracketEstimate, ComboFloorTrigger, CommanderBracketTier, DeclarationVerdict, PlayerDeckList,
    EARLY_ASSEMBLE_CEILING,
};

const DRUID: &str = "Devoted Druid";
const VIZIER: &str = "Vizier of Remedies";

fn db() -> CardDatabase {
    CardDatabase::default().with_bracket_lists(BracketLists::from_pairs("combo-floor-test", &[]))
}

fn provenance() -> ComboProvenance {
    ComboProvenance {
        snapshot_date: "2026-09-27".to_owned(),
        table_version: "test-v1".to_owned(),
        attribution: "Commander Spellbook test fixture".to_owned(),
        card_pool_version: "test-pool".to_owned(),
        filtered: ComboFilterCounts::default(),
        omitted: vec![
            ComboOmission::PrerequisiteText,
            ComboOmission::ResultText,
            ComboOmission::UnmodeledResultClasses,
        ],
    }
}

fn piece(name: &str) -> ComboPiece {
    ComboPiece {
        key: name.to_lowercase(),
        display: name.to_owned(),
        zone: ComboPieceZone::Anywhere,
    }
}

fn entry(
    relevance: ComboRelevance,
    setup: ComboSetup,
    assemble_cost: u16,
    axes: &[BracketAxis],
) -> ComboEntry {
    ComboEntry {
        pieces: [piece(DRUID), piece(VIZIER)],
        relevance,
        setup,
        mana_value_needed: 0,
        assemble_cost,
        popularity: 1,
        outcomes: BTreeSet::from([ComboOutcome::Unbounded(ComboResource::Mana)]),
        axes: axes.iter().copied().collect(),
    }
}

fn table_with(entry: ComboEntry) -> ComboTable {
    ComboTable::from_doc(ComboTableDoc {
        provenance: provenance(),
        entries: vec![entry],
    })
}

fn deck_with(main_deck: &[&str]) -> PlayerDeckList {
    PlayerDeckList {
        commander: vec!["Test Commander".to_owned()],
        main_deck: main_deck.iter().map(|name| (*name).to_owned()).collect(),
        ..Default::default()
    }
}

fn combo_check(
    estimate: &BracketEstimate,
    trigger: ComboFloorTrigger,
) -> &engine::game::ComboCheck {
    estimate
        .combo_checks
        .iter()
        .find(|check| check.trigger == trigger)
        .expect("combo floor row should be present")
}

#[test]
fn standalone_two_card_line_floors_to_upgraded() {
    // Defect #2: Devoted Druid + Vizier of Remedies was invisible to the
    // card-name axes, leaving a Core estimate despite a standalone line.
    let table = table_with(entry(
        ComboRelevance::Standalone,
        ComboSetup::AsPrinted,
        7,
        &[],
    ));
    let estimate = estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &table).unwrap();

    assert_eq!(estimate.tier, CommanderBracketTier::Upgraded);
    assert_eq!(estimate.combos.len(), 1);
    assert_eq!(
        combo_check(&estimate, ComboFloorTrigger::StandaloneTwoCard).outcome,
        BracketCheckOutcome::Fired
    );
}

#[test]
fn no_combo_table_changes_nothing() {
    let estimate =
        estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &ComboTable::default()).unwrap();

    assert_eq!(estimate.tier, CommanderBracketTier::Core);
    assert!(estimate.axes.values().all(|reading| reading.count == 0));
    assert_eq!(estimate.checks.len(), 4);
    assert_eq!(estimate.coverage.counted, 3);
    assert_eq!(estimate.combo_coverage, ComboCoverage::Unmeasured);
    assert!(estimate.combos.is_empty());
    assert!(estimate.combo_checks.is_empty());
    assert_eq!(estimate.combo_provenance, None);
}

#[test]
fn helper_only_match_floors_nothing() {
    let table = table_with(entry(ComboRelevance::Helper, ComboSetup::AsPrinted, 1, &[]));
    let estimate = estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &table).unwrap();

    assert_eq!(estimate.tier, CommanderBracketTier::Core);
    assert_eq!(estimate.combos.len(), 1);
    assert!(estimate
        .combo_checks
        .iter()
        .all(|check| check.outcome != BracketCheckOutcome::Fired));
}

#[test]
fn contextual_match_floors_nothing() {
    let table = table_with(entry(
        ComboRelevance::Contextual,
        ComboSetup::AsPrinted,
        1,
        &[],
    ));
    let estimate = estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &table).unwrap();

    assert_eq!(estimate.tier, CommanderBracketTier::Core);
    assert_eq!(estimate.combos.len(), 1);
    assert!(estimate
        .combo_checks
        .iter()
        .all(|check| check.outcome != BracketCheckOutcome::Fired));
}

#[test]
fn cheap_standalone_line_floors_to_optimized() {
    let table = table_with(entry(
        ComboRelevance::Standalone,
        ComboSetup::AsPrinted,
        EARLY_ASSEMBLE_CEILING,
        &[],
    ));
    let estimate = estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &table).unwrap();

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(
        combo_check(
            &estimate,
            ComboFloorTrigger::EarlyTwoCard {
                reading: EarlyComboReading::IncludingArguable,
                assemble_ceiling: EARLY_ASSEMBLE_CEILING,
            },
        )
        .outcome,
        BracketCheckOutcome::Fired
    );
}

#[test]
fn expensive_standalone_line_stops_at_upgraded() {
    // This pins the six-mana derivation: changing the ceiling requires
    // re-arguing the official "first six turns" sentence.
    let table = table_with(entry(
        ComboRelevance::Standalone,
        ComboSetup::AsPrinted,
        EARLY_ASSEMBLE_CEILING + 1,
        &[],
    ));
    let estimate = estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &table).unwrap();

    assert_eq!(estimate.tier, CommanderBracketTier::Upgraded);
}

#[test]
fn floor_a_and_floor_b_do_not_share_a_predicate() {
    let table = table_with(entry(
        ComboRelevance::Standalone,
        ComboSetup::NotablePrerequisites,
        EARLY_ASSEMBLE_CEILING,
        &[],
    ));
    let estimate = estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &table).unwrap();
    assert_eq!(
        estimate.combos[0].cardinality,
        ComboCardinality::ArguablyTwoCard
    );
    assert_eq!(
        combo_check(&estimate, ComboFloorTrigger::StandaloneTwoCard).outcome,
        BracketCheckOutcome::Fired
    );
    assert_eq!(
        combo_check(
            &estimate,
            ComboFloorTrigger::EarlyTwoCard {
                reading: EarlyComboReading::IncludingArguable,
                assemble_ceiling: EARLY_ASSEMBLE_CEILING,
            },
        )
        .outcome,
        BracketCheckOutcome::Fired
    );
    assert!(!EarlyComboReading::DefinitelyOnly.accepts(estimate.combos[0].cardinality));
    assert!(EarlyComboReading::IncludingArguable.accepts(estimate.combos[0].cardinality));
}

#[test]
fn combo_sourced_mass_land_denial_increments_the_existing_axis() {
    let table = table_with(entry(
        ComboRelevance::Helper,
        ComboSetup::AsPrinted,
        12,
        &[BracketAxis::MassLandDenial],
    ));
    let estimate = estimate_bracket(&deck_with(&[VIZIER, DRUID]), &db(), &table).unwrap();
    let reading = &estimate.axes[&BracketAxis::MassLandDenial];

    assert_eq!(reading.count, 2);
    assert_eq!(reading.contributing, [VIZIER, DRUID]);
    assert_eq!(reading.combo_pairs, [[DRUID.to_owned(), VIZIER.to_owned()]]);
    assert!(estimate.checks.iter().any(|check| {
        check.axis == BracketAxis::MassLandDenial && check.outcome == BracketCheckOutcome::Fired
    }));
}

#[test]
fn cards_until_fired_reads_one_when_one_piece_is_present_and_two_when_none() {
    let table = table_with(entry(
        ComboRelevance::Standalone,
        ComboSetup::AsPrinted,
        7,
        &[],
    ));
    let one = estimate_bracket(&deck_with(&[DRUID]), &db(), &table).unwrap();
    let none = estimate_bracket(&deck_with(&[]), &db(), &table).unwrap();

    assert_eq!(
        combo_check(&one, ComboFloorTrigger::StandaloneTwoCard).outcome,
        BracketCheckOutcome::Clear {
            cards_until_fired: Some(1)
        }
    );
    assert_eq!(
        combo_check(&none, ComboFloorTrigger::StandaloneTwoCard).outcome,
        BracketCheckOutcome::Clear {
            cards_until_fired: Some(2)
        }
    );
}

#[test]
fn measured_table_makes_the_combo_barometer_engine_authority() {
    let table = table_with(entry(
        ComboRelevance::Helper,
        ComboSetup::AsPrinted,
        12,
        &[],
    ));
    let estimate = estimate_bracket(&deck_with(&[]), &db(), &table).unwrap();

    assert_eq!(
        estimate.barometers[&Barometer::TwoCardCombos],
        BarometerAuthority::Engine
    );
}

#[test]
fn below_floor_names_the_combo_row_that_raised_it() {
    let table = table_with(entry(
        ComboRelevance::Standalone,
        ComboSetup::AsPrinted,
        EARLY_ASSEMBLE_CEILING,
        &[],
    ));
    let estimate = estimate_bracket(&deck_with(&[DRUID, VIZIER]), &db(), &table).unwrap();

    assert_eq!(
        reconcile(CommanderBracketTier::Core, &estimate),
        DeclarationVerdict::BelowFloor {
            floor: CommanderBracketTier::Optimized,
            raised_by: vec![],
            raised_by_combo_floor: Some(ComboFloorTrigger::EarlyTwoCard {
                reading: EarlyComboReading::IncludingArguable,
                assemble_ceiling: EARLY_ASSEMBLE_CEILING,
            }),
        }
    );
}

#[test]
fn estimate_is_pure_and_deterministic() {
    let table = table_with(entry(
        ComboRelevance::Standalone,
        ComboSetup::AsPrinted,
        EARLY_ASSEMBLE_CEILING,
        &[],
    ));
    let deck = deck_with(&[DRUID, VIZIER]);

    assert_eq!(
        estimate_bracket(&deck, &db(), &table),
        estimate_bracket(&deck, &db(), &table)
    );
}

#[test]
fn pre_p7_estimate_json_still_deserializes() {
    let json = r#"{
        "tier":"core",
        "axes":{
            "game_changers":{"count":0,"contributing":[]},
            "mass_land_denial":{"count":0,"contributing":[]},
            "extra_turns":{"count":0,"contributing":[]},
            "efficient_tutors":{"count":0,"contributing":[]}
        },
        "checks":[],
        "coverage":{"counted":1,"resolved":0,"unresolved":["Test Commander"],"confidence":"partial"},
        "data_version":"pre-p7"
    }"#;
    let estimate: BracketEstimate = serde_json::from_str(json).unwrap();

    assert_eq!(estimate.combo_coverage, ComboCoverage::Unmeasured);
    assert!(estimate.combos.is_empty());
    assert!(estimate.combo_checks.is_empty());
    assert_eq!(estimate.combo_provenance, None);
    assert!(estimate
        .axes
        .values()
        .all(|reading| reading.combo_pairs.is_empty()));
}
