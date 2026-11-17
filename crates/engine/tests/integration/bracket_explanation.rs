//! The bracket explanation: the reasons at the estimated tier ("Why") and the
//! ladder of steps down ("Bring it down"), each reason carrying typed cuts.

use std::collections::BTreeSet;

use engine::database::{
    BracketCardClass, BracketLists, CardDatabase, ComboEntry, ComboFilterCounts, ComboOmission,
    ComboOutcome, ComboPiece, ComboPieceZone, ComboProvenance, ComboRelevance, ComboSetup,
    ComboTable, ComboTableDoc, EarlyComboReading,
};
use engine::game::{
    estimate_bracket, Barometer, BarometerAuthority, BracketAxis, BracketCard, BracketCardRole,
    BracketCut, BracketEstimate, BracketExplanation, BracketLadderStep, BracketReasonSource,
    ComboDeclaration, ComboFloorTrigger, ComboWindow, CommanderBracketTier, ExplainedCombo,
    PlayerDeckList, EARLY_ASSEMBLE_CEILING,
};

fn db(game_changers: &[&str], mass_land_denial: &[&str], extra_turns: &[&str]) -> CardDatabase {
    CardDatabase::default().with_bracket_lists(BracketLists::from_pairs(
        "explain-test",
        &[
            (BracketCardClass::GameChangers, game_changers),
            (BracketCardClass::MassLandSweepers, mass_land_denial),
            (BracketCardClass::ExtraTurns, extra_turns),
        ],
    ))
}

fn deck(commander: &[&str], main: &[&str]) -> PlayerDeckList {
    PlayerDeckList {
        commander: commander.iter().map(|name| (*name).to_owned()).collect(),
        main_deck: main.iter().map(|name| (*name).to_owned()).collect(),
        ..Default::default()
    }
}

fn card(name: &str) -> BracketCard {
    BracketCard {
        name: name.to_owned(),
        role: BracketCardRole::Deck,
    }
}

fn commander(name: &str) -> BracketCard {
    BracketCard {
        name: name.to_owned(),
        role: BracketCardRole::Commander,
    }
}

fn estimate(deck: &PlayerDeckList, db: &CardDatabase) -> BracketEstimate {
    estimate_with(deck, db, &ComboTable::default())
}

fn estimate_with(deck: &PlayerDeckList, db: &CardDatabase, table: &ComboTable) -> BracketEstimate {
    estimate_bracket(deck, db, table).unwrap()
}

/// A standalone, as-printed two-card combo that wins the game.
fn combo(first: &str, second: &str, assemble_cost: u16, axes: &[BracketAxis]) -> ComboEntry {
    let piece = |name: &str| ComboPiece {
        key: name.to_lowercase(),
        display: name.to_owned(),
        zone: ComboPieceZone::Anywhere,
    };
    ComboEntry {
        pieces: [piece(first), piece(second)],
        relevance: ComboRelevance::Standalone,
        setup: ComboSetup::AsPrinted,
        mana_value_needed: 0,
        assemble_cost,
        popularity: 1,
        outcomes: BTreeSet::from([ComboOutcome::Wins]),
        axes: axes.iter().copied().collect(),
    }
}

fn table(entries: Vec<ComboEntry>) -> ComboTable {
    ComboTable::from_doc(ComboTableDoc {
        provenance: ComboProvenance {
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
        },
        entries,
    })
}

fn pair(a: BracketCard, b: BracketCard, cost: u16) -> ExplainedCombo {
    ExplainedCombo {
        pieces: [a, b],
        outcomes: BTreeSet::from([ComboOutcome::Wins]),
        assemble_cost: cost,
    }
}

const EARLY_TWO_CARD: ComboFloorTrigger = ComboFloorTrigger::EarlyTwoCard {
    reading: EarlyComboReading::IncludingArguable,
    assemble_ceiling: EARLY_ASSEMBLE_CEILING,
};

const TERGRID: &str = "Tergrid, God of Fright";

fn tergrid_estimate() -> BracketEstimate {
    estimate(
        &deck(&[TERGRID], &["Rhystic Study", "Cyclonic Rift"]),
        &db(&[TERGRID, "Rhystic Study", "Cyclonic Rift"], &[], &[]),
    )
}

#[test]
fn five_game_changers_step_down_two_then_all() {
    let five = ["A", "B", "C", "D", "E"];
    let estimate = estimate(&deck(&["Cmdr"], &five), &db(&five, &[], &[]));
    let e = &estimate.explanation;
    let all_five: Vec<BracketCard> = five.iter().map(|name| card(name)).collect();

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(e.why.len(), 1, "only the >= 4 rule holds the deck at B4");
    assert!(matches!(
        &e.why[0].source,
        BracketReasonSource::CardRule {
            axis: BracketAxis::GameChangers,
            threshold: 4,
            observed: 5,
            allowed_below: 3,
            tier_below: CommanderBracketTier::Upgraded,
            ..
        }
    ));
    assert_eq!(
        e.steps.iter().map(|step| step.lands_on).collect::<Vec<_>>(),
        [CommanderBracketTier::Upgraded, CommanderBracketTier::Core]
    );
    assert_eq!(
        e.steps[0].reasons[0].cuts,
        [BracketCut::RemoveCards {
            count: 2,
            from: all_five.clone(),
        }]
    );
    assert_eq!(
        e.steps[1].reasons[0].cuts,
        [BracketCut::RemoveCards {
            count: 5,
            from: all_five,
        }]
    );
    assert_eq!(e.steps[0].reasons, e.why);
}

#[test]
fn a_clean_deck_has_no_reasons_and_no_steps() {
    let estimate = estimate(&deck(&["Cmdr"], &["Forest"]), &db(&[], &[], &[]));

    assert_eq!(estimate.tier, CommanderBracketTier::Core);
    assert_eq!(estimate.explanation, BracketExplanation::default());
}

#[test]
fn an_extra_turn_card_is_a_why_reason_at_the_base_floor_but_never_a_step() {
    let estimate = estimate(
        &deck(&["Cmdr"], &["Time Warp"]),
        &db(&[], &[], &["Time Warp"]),
    );
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Core);
    assert_eq!(e.why.len(), 1);
    assert_eq!(e.why[0].floor, CommanderBracketTier::Core);
    let BracketReasonSource::CardRule {
        axis,
        tier_below,
        allowed_below,
        cards,
        combos,
        ..
    } = &e.why[0].source
    else {
        panic!("expected a card rule, got {:?}", e.why[0].source);
    };
    assert_eq!(*axis, BracketAxis::ExtraTurns);
    assert_eq!(*tier_below, CommanderBracketTier::Exhibition);
    assert_eq!(*allowed_below, 0);
    assert_eq!(*cards, [card("Time Warp")]);
    assert!(combos.is_empty());
    assert!(e.steps.is_empty(), "the ladder ends at the base floor");
}

#[test]
fn the_owners_early_combo_answer_is_its_own_reason_with_no_table() {
    let declaration = ComboDeclaration::Intended {
        window: Some(ComboWindow::EarlyGame),
    };
    let combo_deck = PlayerDeckList {
        combo_declaration: declaration,
        ..deck(&["Cmdr"], &["Forest"])
    };
    let estimate = estimate(&combo_deck, &db(&[], &[], &[]));
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(e.why.len(), 1);
    assert_eq!(e.why[0].floor, CommanderBracketTier::Optimized);
    assert_eq!(
        e.why[0].source,
        BracketReasonSource::OwnerComboAnswer { declaration }
    );
    assert_eq!(e.why[0].cuts, [BracketCut::ChangeComboAnswer]);
    assert!(e.why[0]
        .official_line
        .starts_with("No intentional early-game two-card infinite combos."));
    assert_eq!(
        e.steps,
        [BracketLadderStep {
            lands_on: CommanderBracketTier::Core,
            reasons: e.why.clone(),
        }]
    );
}

#[test]
fn a_game_changer_commander_is_tagged_and_never_cut() {
    let estimate = tergrid_estimate();
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Upgraded);
    let BracketReasonSource::CardRule { cards, .. } = &e.why[0].source else {
        panic!("expected a card rule, got {:?}", e.why[0].source);
    };
    assert_eq!(
        *cards,
        [
            commander(TERGRID),
            card("Rhystic Study"),
            card("Cyclonic Rift"),
        ]
    );
    assert_eq!(
        e.why[0].cuts,
        [
            BracketCut::RemoveCards {
                count: 2,
                from: vec![card("Rhystic Study"), card("Cyclonic Rift")],
            },
            BracketCut::ReplaceCommander {
                count: 1,
                of: vec![commander(TERGRID)],
            },
        ]
    );
    // The spec's case is about the step to B2: it carries this same reason, so its cuts
    // also leave Tergrid out of the removals and ask for a different commander.
    assert_eq!(
        e.steps,
        [BracketLadderStep {
            lands_on: CommanderBracketTier::Core,
            reasons: e.why.clone(),
        }]
    );
}

#[test]
fn under_the_four_rule_a_commander_game_changer_is_left_out_of_the_cut() {
    let four = ["A", "B", "C", "D"];
    let estimate = estimate(
        &deck(&[TERGRID], &four),
        &db(&[TERGRID, "A", "B", "C", "D"], &[], &[]),
    );
    let e = &estimate.explanation;
    let all_four: Vec<BracketCard> = four.iter().map(|name| card(name)).collect();

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(
        e.steps[0].reasons[0].cuts,
        [BracketCut::RemoveCards {
            count: 2,
            from: all_four.clone(),
        }]
    );
    assert_eq!(
        e.steps[1].reasons[0].cuts,
        [
            BracketCut::RemoveCards {
                count: 4,
                from: all_four,
            },
            BracketCut::ReplaceCommander {
                count: 1,
                of: vec![commander(TERGRID)],
            },
        ]
    );
}

#[test]
fn partner_game_changer_commanders_need_both_replaced_under_the_one_rule() {
    let estimate = estimate(&deck(&["P1", "P2"], &[]), &db(&["P1", "P2"], &[], &[]));
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Upgraded);
    assert_eq!(
        e.why[0].cuts,
        [BracketCut::ReplaceCommander {
            count: 2,
            of: vec![commander("P1"), commander("P2")],
        }]
    );
}

#[test]
fn explanation_wire_shape_is_snake_case_and_kind_tagged() {
    let value = serde_json::to_value(tergrid_estimate()).unwrap();
    let why = &value["explanation"]["why"][0];

    assert_eq!(why["source"]["kind"], "card_rule");
    assert_eq!(why["source"]["tier_below"], "core");
    assert_eq!(
        why["cuts"][0],
        serde_json::json!({
            "kind": "remove_cards",
            "count": 2,
            "from": [
                { "name": "Rhystic Study", "role": "deck" },
                { "name": "Cyclonic Rift", "role": "deck" },
            ],
        })
    );
    assert_eq!(why["cuts"][1]["kind"], "replace_commander");
    assert_eq!(why["cuts"][1]["of"][0]["role"], "commander");
}

#[test]
fn an_estimate_without_an_explanation_still_deserializes() {
    let estimate = tergrid_estimate();
    let mut value = serde_json::to_value(&estimate).unwrap();

    assert_eq!(
        serde_json::from_value::<BracketEstimate>(value.clone()).unwrap(),
        estimate,
        "the explanation round-trips"
    );
    value.as_object_mut().unwrap().remove("explanation");
    assert_eq!(
        serde_json::from_value::<BracketEstimate>(value)
            .unwrap()
            .explanation,
        BracketExplanation::default()
    );
}

const ORACLE: &str = "Thassa's Oracle";
const CONSULTATION: &str = "Demonic Consultation";
const EATER: &str = "Land Eater";
const FEEDER: &str = "Land Feeder";

#[test]
fn yuriko_breaks_the_combo_then_drops_the_game_changers() {
    let game_changers = [ORACLE, "Rhystic Study", "Cyclonic Rift"];
    let estimate = estimate_with(
        &deck(
            &["Yuriko, the Tiger's Shadow"],
            &[
                ORACLE,
                CONSULTATION,
                "Rhystic Study",
                "Cyclonic Rift",
                "Time Warp",
                "Sol Ring",
            ],
        ),
        &db(&game_changers, &[], &["Time Warp"]),
        &table(vec![combo(ORACLE, CONSULTATION, 3, &[])]),
    );
    let e = &estimate.explanation;
    let oracle_combo = pair(card(ORACLE), card(CONSULTATION), 3);

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(e.why.len(), 1);
    assert_eq!(e.why[0].floor, CommanderBracketTier::Optimized);
    assert_eq!(
        e.why[0].source,
        BracketReasonSource::ComboFloor {
            trigger: EARLY_TWO_CARD,
            combos: vec![oracle_combo.clone()],
        }
    );
    assert!(e.why[0]
        .official_line
        .starts_with("No intentional early-game two-card infinite combos."));
    assert_eq!(
        e.why[0].cuts,
        [BracketCut::BreakCombo {
            combo: oracle_combo,
            remove_one_of: vec![card(ORACLE), card(CONSULTATION)],
        }]
    );

    assert_eq!(
        e.steps.iter().map(|step| step.lands_on).collect::<Vec<_>>(),
        [CommanderBracketTier::Upgraded, CommanderBracketTier::Core]
    );
    assert_eq!(e.steps[0].reasons, e.why);
    // The standalone floor matched the same combo; it is shown once, at B4, so B3
    // holds only the Game Changers. Thassa's Oracle is in both cuts on purpose:
    // steps describe the deck as it is and do not simulate the cuts above them.
    assert_eq!(e.steps[1].reasons.len(), 1);
    let BracketReasonSource::CardRule { axis, combos, .. } = &e.steps[1].reasons[0].source else {
        panic!(
            "expected a card rule, got {:?}",
            e.steps[1].reasons[0].source
        );
    };
    assert_eq!(*axis, BracketAxis::GameChangers);
    assert!(combos.is_empty());
    assert_eq!(
        e.steps[1].reasons[0].cuts,
        [BracketCut::RemoveCards {
            count: 3,
            from: game_changers.iter().map(|name| card(name)).collect(),
        }]
    );
}

#[test]
fn a_combo_that_matches_both_floors_is_one_step_to_the_base() {
    let estimate = estimate_with(
        &deck(&["Cmdr"], &["A", "B"]),
        &db(&[], &[], &[]),
        &table(vec![combo("A", "B", 2, &[])]),
    );
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(e.why.len(), 1);
    assert_eq!(
        e.why[0].source,
        BracketReasonSource::ComboFloor {
            trigger: EARLY_TWO_CARD,
            combos: vec![pair(card("A"), card("B"), 2)],
        }
    );
    assert_eq!(
        e.steps,
        [BracketLadderStep {
            lands_on: CommanderBracketTier::Core,
            reasons: e.why.clone(),
        }]
    );
}

#[test]
fn an_early_and_a_late_combo_each_get_their_own_step() {
    let estimate = estimate_with(
        &deck(&["Cmdr"], &["A", "B", "C", "D"]),
        &db(&[], &[], &[]),
        &table(vec![combo("A", "B", 3, &[]), combo("C", "D", 9, &[])]),
    );
    let e = &estimate.explanation;
    let steps: Vec<(CommanderBracketTier, Vec<&BracketReasonSource>)> = e
        .steps
        .iter()
        .map(|step| {
            (
                step.lands_on,
                step.reasons.iter().map(|reason| &reason.source).collect(),
            )
        })
        .collect();

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(
        steps,
        [
            (
                CommanderBracketTier::Upgraded,
                vec![&BracketReasonSource::ComboFloor {
                    trigger: EARLY_TWO_CARD,
                    combos: vec![pair(card("A"), card("B"), 3)],
                }],
            ),
            (
                CommanderBracketTier::Core,
                vec![&BracketReasonSource::ComboFloor {
                    trigger: ComboFloorTrigger::StandaloneTwoCard,
                    combos: vec![pair(card("C"), card("D"), 9)],
                }],
            ),
        ]
    );
}

#[test]
fn mass_land_denial_from_a_pair_is_cleared_by_breaking_the_pair() {
    let estimate = estimate_with(
        &deck(&["Cmdr"], &[EATER, FEEDER]),
        &db(&[], &[], &[]),
        &table(vec![combo(
            EATER,
            FEEDER,
            10,
            &[BracketAxis::MassLandDenial],
        )]),
    );
    let e = &estimate.explanation;
    let land_combo = pair(card(EATER), card(FEEDER), 10);

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(e.why.len(), 1);
    let BracketReasonSource::CardRule {
        axis,
        observed,
        cards,
        combos,
        ..
    } = &e.why[0].source
    else {
        panic!("expected a card rule, got {:?}", e.why[0].source);
    };
    assert_eq!(*axis, BracketAxis::MassLandDenial);
    assert_eq!(*observed, 1);
    assert!(cards.is_empty());
    assert_eq!(*combos, std::slice::from_ref(&land_combo));
    assert_eq!(
        e.why[0].cuts,
        [BracketCut::BreakCombo {
            combo: land_combo,
            remove_one_of: vec![card(EATER), card(FEEDER)],
        }]
    );
    assert_eq!(
        e.steps,
        [BracketLadderStep {
            lands_on: CommanderBracketTier::Core,
            reasons: e.why.clone(),
        }],
        "the standalone floor's copy of the pair was shown at B4"
    );
}

#[test]
fn a_pair_a_card_rule_and_a_combo_floor_both_count_is_in_both_whys_but_one_step() {
    let estimate = estimate_with(
        &deck(&["Cmdr"], &[EATER, FEEDER]),
        &db(&[], &[], &[]),
        &table(vec![combo(
            EATER,
            FEEDER,
            2,
            &[BracketAxis::MassLandDenial],
        )]),
    );
    let e = &estimate.explanation;
    let land_combo = pair(card(EATER), card(FEEDER), 2);

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(
        e.why.len(),
        2,
        "why lists every reason at B4 as it fired: {:?}",
        e.why
    );
    let BracketReasonSource::CardRule { axis, combos, .. } = &e.why[0].source else {
        panic!("expected a card rule, got {:?}", e.why[0].source);
    };
    assert_eq!(*axis, BracketAxis::MassLandDenial);
    assert_eq!(*combos, std::slice::from_ref(&land_combo));
    assert_eq!(
        e.why[1].source,
        BracketReasonSource::ComboFloor {
            trigger: EARLY_TWO_CARD,
            combos: vec![land_combo],
        }
    );
    assert_eq!(
        e.steps,
        [BracketLadderStep {
            lands_on: CommanderBracketTier::Core,
            reasons: vec![e.why[0].clone()],
        }],
        "the step shows the pair once, under the card rule"
    );
}

#[test]
fn a_card_and_a_pair_on_mass_land_denial_both_appear_in_the_cut() {
    let estimate = estimate_with(
        &deck(&["Cmdr"], &["Armageddon", EATER, FEEDER]),
        &db(&[], &["Armageddon"], &[]),
        &table(vec![combo(
            EATER,
            FEEDER,
            10,
            &[BracketAxis::MassLandDenial],
        )]),
    );
    let e = &estimate.explanation;

    let BracketReasonSource::CardRule {
        observed, cards, ..
    } = &e.why[0].source
    else {
        panic!("expected a card rule, got {:?}", e.why[0].source);
    };
    assert_eq!(*observed, 2);
    assert_eq!(*cards, [card("Armageddon")]);
    assert_eq!(
        e.why[0].cuts,
        [
            BracketCut::RemoveCards {
                count: 1,
                from: vec![card("Armageddon")],
            },
            BracketCut::BreakCombo {
                combo: pair(card(EATER), card(FEEDER), 10),
                remove_one_of: vec![card(EATER), card(FEEDER)],
            },
        ]
    );
}

#[test]
fn a_combo_with_the_commander_cuts_only_the_other_piece() {
    let estimate = estimate_with(
        &deck(&[EATER], &[FEEDER]),
        &db(&[], &[], &[]),
        &table(vec![combo(
            EATER,
            FEEDER,
            10,
            &[BracketAxis::MassLandDenial],
        )]),
    );
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(
        e.why[0].cuts,
        [BracketCut::BreakCombo {
            combo: pair(commander(EATER), card(FEEDER), 10),
            remove_one_of: vec![card(FEEDER)],
        }]
    );
}

#[test]
fn a_combo_between_partner_commanders_needs_a_different_commander() {
    let estimate = estimate_with(
        &deck(&["Partner A", "Partner B"], &[]),
        &db(&[], &[], &[]),
        &table(vec![combo("Partner A", "Partner B", 2, &[])]),
    );
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(
        e.why[0].cuts,
        [BracketCut::ReplaceCommander {
            count: 1,
            of: vec![commander("Partner A"), commander("Partner B")],
        }]
    );
}

#[test]
fn unmeasured_combos_give_no_combo_reasons_and_an_unanswered_barometer() {
    let estimate = estimate(
        &deck(&["Cmdr"], &[ORACLE, CONSULTATION]),
        &db(&[ORACLE], &[], &[]),
    );
    let e = &estimate.explanation;
    let reasons: Vec<_> = e
        .why
        .iter()
        .chain(e.steps.iter().flat_map(|step| &step.reasons))
        .collect();

    assert_eq!(
        estimate.combo_barometer.declaration,
        ComboDeclaration::Undeclared
    );
    assert_eq!(
        estimate.barometers[&Barometer::TwoCardCombos],
        BarometerAuthority::Unanswered
    );
    assert_eq!(estimate.tier, CommanderBracketTier::Upgraded);
    assert!(!reasons.is_empty(), "the Game Changer still holds the deck");
    for reason in reasons {
        match &reason.source {
            BracketReasonSource::CardRule { combos, .. } => assert!(combos.is_empty()),
            BracketReasonSource::ComboFloor { .. } => {
                panic!("an unmeasured table fired a combo floor: {reason:?}")
            }
            BracketReasonSource::OwnerComboAnswer { .. } => {
                panic!("an undeclared deck has no combo answer: {reason:?}")
            }
        }
    }
}

#[test]
fn a_combo_floor_with_two_combos_breaks_each_pair() {
    let estimate = estimate_with(
        &deck(&["Cmdr"], &["A", "B", "C", "D"]),
        &db(&[], &[], &[]),
        &table(vec![combo("A", "B", 3, &[]), combo("C", "D", 4, &[])]),
    );
    let e = &estimate.explanation;
    let first = pair(card("A"), card("B"), 3);
    let second = pair(card("C"), card("D"), 4);

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(e.why.len(), 1);
    assert_eq!(
        e.why[0].source,
        BracketReasonSource::ComboFloor {
            trigger: EARLY_TWO_CARD,
            combos: vec![first.clone(), second.clone()],
        }
    );
    assert_eq!(
        e.why[0].cuts,
        [
            BracketCut::BreakCombo {
                combo: first,
                remove_one_of: vec![card("A"), card("B")],
            },
            BracketCut::BreakCombo {
                combo: second,
                remove_one_of: vec![card("C"), card("D")],
            },
        ]
    );
}

#[test]
fn a_land_denial_pair_of_partner_commanders_needs_a_different_commander() {
    let estimate = estimate_with(
        &deck(&["Partner A", "Partner B"], &[]),
        &db(&[], &[], &[]),
        &table(vec![combo(
            "Partner A",
            "Partner B",
            10,
            &[BracketAxis::MassLandDenial],
        )]),
    );
    let e = &estimate.explanation;

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(e.why.len(), 1);
    let BracketReasonSource::CardRule {
        axis,
        cards,
        combos,
        ..
    } = &e.why[0].source
    else {
        panic!("expected a card rule, got {:?}", e.why[0].source);
    };
    assert_eq!(*axis, BracketAxis::MassLandDenial);
    assert!(cards.is_empty());
    assert_eq!(
        *combos,
        [pair(commander("Partner A"), commander("Partner B"), 10)]
    );
    assert_eq!(
        e.why[0].cuts,
        [BracketCut::ReplaceCommander {
            count: 1,
            of: vec![commander("Partner A"), commander("Partner B")],
        }]
    );
}
