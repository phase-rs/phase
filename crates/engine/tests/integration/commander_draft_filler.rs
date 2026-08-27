//! CR 903.13e grantable commander filler (U7) and CR 903.13f(3) partner grant (U8).
//!
//! Two rules, one per-set table, asserted at the seams the engine actually
//! owns: `draft_set_concessions` for the table, and `can_pair_commanders` for
//! the grant.
//!
//! No card-name literal appears in the U7 assertions. CR 903.13e *is* a list of
//! card names, so those names live in the CR-quoting table itself; a test that
//! retyped them would assert only that two literals match. What these tests
//! assert instead is the table's STRUCTURE — which sets grant, whether two sets
//! grant the same card, and which single set additionally carries the CR
//! 903.13f(3) colour bound — and that discriminates every way the mapping can
//! be mis-keyed.

use std::collections::BTreeMap;

use engine::database::synthesis::{commander_qualification, CommanderQualification};
use engine::database::CardDatabase;
use engine::game::deck_validation::{can_pair_commanders, draft_set_concessions, PartnerGrant};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::keywords::{Keyword, PartnerType};
use engine::types::mana::ManaColor;

// ---------------------------------------------------------------------------
// U7 — the per-set concession table.
// ---------------------------------------------------------------------------

/// U7 row 1 — the table's structure, asserted without naming a card.
///
/// CR 903.13e names Commander Legends and Commander Masters for ONE card and
/// Commander Legends: Battle for Baldur's Gate for a DIFFERENT one, so the
/// CMR/CMM fillers must be equal to each other and the CLB filler must not
/// equal either. That single relation catches a table keyed to the wrong set,
/// a row copy-pasted onto the wrong card, and a collapsed two-set row.
#[test]
fn the_filler_table_matches_the_sets_cr_903_13e_names() {
    let cmr = draft_set_concessions("CMR")
        .filler
        .expect("CR 903.13e names Commander Legends");
    let cmm = draft_set_concessions("CMM")
        .filler
        .expect("CR 903.13e names Commander Masters");
    let clb = draft_set_concessions("CLB")
        .filler
        .expect("CR 903.13e names Commander Legends: Battle for Baldur's Gate");

    assert_eq!(cmr, cmm, "CR 903.13e names ONE card for both CMR and CMM");
    assert_ne!(clb, cmr, "CR 903.13e names a DIFFERENT card for CLB");

    // CR 903.13e: "each player may add up to two".
    for filler in [&cmr, &cmm, &clb] {
        assert_eq!(filler.max_copies, 2, "CR 903.13e caps the grant at two");
    }
}

/// U7 row 1, orthogonal axis — CR 903.13f(3) names Commander Masters and
/// nothing else, so exactly one of the three granting sets carries a partner
/// grant. This is what an implementation that collapsed the two rules into one
/// table gets wrong.
#[test]
fn only_commander_masters_carries_the_partner_grant() {
    assert_eq!(
        draft_set_concessions("CMM").partner_grant,
        // CR 903.13f(3): "whose color identity includes one or fewer colors".
        Some(PartnerGrant { max_colors: 1 }),
    );
    assert_eq!(draft_set_concessions("CMR").partner_grant, None);
    assert_eq!(draft_set_concessions("CLB").partner_grant, None);
}

/// U7 row 2 — the negative, paired to its reach guard so "returns nothing"
/// cannot pass by the table being empty, plus the case-insensitivity the
/// caller-supplied `DraftConfig.set_code` requires.
#[test]
fn an_unnamed_set_concedes_nothing_and_lookup_is_case_insensitive() {
    let neo = draft_set_concessions("NEO");
    assert_eq!(neo.filler, None);
    assert_eq!(neo.partner_grant, None);

    // Reach guard: the same instrument returns something for a named set.
    assert!(draft_set_concessions("CMM").filler.is_some());

    assert_eq!(draft_set_concessions("cmm"), draft_set_concessions("CMM"));
}

// ---------------------------------------------------------------------------
// U8 — CR 903.13f(3), asserted at the `can_pair_commanders` seam.
// ---------------------------------------------------------------------------

/// CR 903.13f(3)'s own bound: "one or fewer colors".
const CMM_GRANT: Option<PartnerGrant> = Some(PartnerGrant { max_colors: 1 });

fn face(name: &str, core_types: Vec<CoreType>, subtypes: Vec<&str>) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: vec![Supertype::Legendary],
            core_types,
            subtypes: subtypes.into_iter().map(String::from).collect(),
        },
        ..CardFace::default()
    }
}

fn legend(name: &str, colors: Vec<ManaColor>) -> CardFace {
    CardFace {
        color_identity: colors,
        ..face(name, vec![CoreType::Creature], vec![])
    }
}

fn db_of(faces: Vec<CardFace>) -> CardDatabase {
    let mut entries = BTreeMap::new();
    for f in faces {
        let mut obj = serde_json::to_value(&f).unwrap();
        // `CardExportEntry` flattens `CardFace` and adds its own defaults.
        obj.as_object_mut()
            .unwrap()
            .insert("legalities".to_string(), serde_json::json!({}));
        entries.insert(f.name.to_lowercase(), obj);
    }
    CardDatabase::from_json_str(&serde_json::to_string(&entries).unwrap()).unwrap()
}

/// U8 row 1 — the multi-authority hostile fixture and its reach guard, as one
/// pair. The SAME two cards, with NO printed partner keyword, pair under a
/// Commander Masters draft and do not pair in constructed Commander.
///
/// One card, two authorities, opposite verdicts. A printing-derived
/// implementation cannot pass this, which is exactly why it is the fixture.
#[test]
fn cmm_mono_color_commander_gains_partner_for_deckbuilding() {
    let db = db_of(vec![
        legend("Mono White Legend", vec![ManaColor::White]),
        legend("Mono Black Legend", vec![ManaColor::Black]),
    ]);

    assert!(
        can_pair_commanders(&db, "Mono White Legend", "Mono Black Legend", CMM_GRANT),
        "CR 903.13f(3): under a Commander Masters draft both are considered to have partner"
    );
    assert!(
        !can_pair_commanders(&db, "Mono White Legend", "Mono Black Legend", None),
        "constructed Commander grants nothing — CR 903.13f(3) is scoped to the draft"
    );
}

/// U8 row 2 — the colour bound. CR 903.13f(3) says "one or fewer colors", so a
/// two-colour legend gains nothing. This is the assertion that silently passes
/// if colour identity is ignored.
#[test]
fn a_two_color_legend_gains_nothing_under_the_grant() {
    let db = db_of(vec![
        legend("Mono White Legend", vec![ManaColor::White]),
        legend("Two Color Legend", vec![ManaColor::White, ManaColor::Blue]),
    ]);

    assert!(!can_pair_commanders(
        &db,
        "Mono White Legend",
        "Two Color Legend",
        CMM_GRANT
    ));

    // Reach guard: a colourless legend IS within "one or fewer colors", so the
    // rejection above is the bound biting and not the grant being dead.
    let db = db_of(vec![
        legend("Mono White Legend", vec![ManaColor::White]),
        legend("Colorless Legend", vec![]),
    ]);
    assert!(can_pair_commanders(
        &db,
        "Mono White Legend",
        "Colorless Legend",
        CMM_GRANT
    ));
}

/// U8 row 3 — the "by itself" carve-out. CR 903.13f(3) grants only to a card
/// that "can be a player's commander BY ITSELF", and CR 702.124k says a
/// legendary Background enchantment "can't be your commander unless you have
/// also designated a commander with 'choose a Background'". So a Background is
/// commander-eligible but not by itself, and gains nothing.
///
/// Bounded reach, recorded rather than hidden: Commander Masters prints no
/// Backgrounds, so this carve-out is unreachable through a CMM pool. It is
/// reachable at the seam under test, which is why the assertion lives here.
#[test]
fn a_background_is_eligible_but_not_by_itself_and_gains_nothing() {
    let background = CardFace {
        color_identity: vec![ManaColor::Black],
        ..face(
            "A Background",
            vec![CoreType::Enchantment],
            vec!["Background"],
        )
    };
    assert_eq!(
        commander_qualification(&background),
        CommanderQualification::OnlyAlongsideChooseABackground,
    );
    // Reach guard on the same axis: a mono-colour legendary creature IS
    // by-itself, so the verdict above is the carve-out and not a dead predicate.
    assert_eq!(
        commander_qualification(&legend("Mono White Legend", vec![ManaColor::White])),
        CommanderQualification::ByItself,
    );

    let db = db_of(vec![
        legend("Mono White Legend", vec![ManaColor::White]),
        background,
    ]);
    assert!(!can_pair_commanders(
        &db,
        "Mono White Legend",
        "A Background",
        CMM_GRANT
    ));
}

/// U8 row 4 — the grant COMPOSES with printed Partner rather than replacing it.
///
/// A granted mono-colour legend must pair with a card carrying PRINTED generic
/// Partner. This is the assertion a `granted(a) && granted(b)` conjunction
/// fails, and it is why the grant is expressed as synthesising a
/// `PartnerType::Generic` rather than as a special case inside the check.
#[test]
fn the_grant_composes_with_printed_partner() {
    let printed_partner = CardFace {
        keywords: vec![Keyword::Partner(PartnerType::Generic)],
        // A three-colour identity, so this card could never receive the grant
        // itself — the pairing can only come from its PRINTED keyword.
        color_identity: vec![ManaColor::White, ManaColor::Blue, ManaColor::Black],
        ..face("Printed Partner Legend", vec![CoreType::Creature], vec![])
    };
    let db = db_of(vec![
        legend("Mono White Legend", vec![ManaColor::White]),
        printed_partner,
    ]);

    assert!(
        can_pair_commanders(
            &db,
            "Mono White Legend",
            "Printed Partner Legend",
            CMM_GRANT
        ),
        "CR 702.124h: generic Partner pairs with generic Partner, however each side got it"
    );
    assert!(
        !can_pair_commanders(&db, "Mono White Legend", "Printed Partner Legend", None),
        "without the grant only one side has partner, so CR 702.124h is unsatisfied"
    );
}

/// U8 row 5 — CR 702.124f: "Different partner abilities are distinct from one
/// another and cannot be combined." Per CR 702.124n the grant is the partner
/// family only, so a granted legend does not pair with a card whose only
/// partner ability is "choose a Background".
#[test]
fn the_grant_does_not_cross_partner_families() {
    // TWO colours, so CR 903.13f(3) does NOT reach this card and its only
    // partner ability really is "choose a Background". A MONO-colour chooser
    // would be commander-by-itself and within the colour bound, so it would
    // receive the grant and pair through the generic Partner both cards then
    // have — legally, per CR 702.124g ("If a legendary card has more than one
    // partner ability, you may choose which one to use"). That is a different
    // rule from the one this row tests, and a mono-colour fixture here asserts
    // the opposite of what the CR says.
    let chooser = CardFace {
        keywords: vec![Keyword::Partner(PartnerType::ChooseABackground)],
        color_identity: vec![ManaColor::Green, ManaColor::Blue],
        ..face("Background Chooser", vec![CoreType::Creature], vec![])
    };
    let background = CardFace {
        color_identity: vec![ManaColor::Green],
        ..face(
            "A Background",
            vec![CoreType::Enchantment],
            vec!["Background"],
        )
    };
    let db = db_of(vec![
        legend("Mono White Legend", vec![ManaColor::White]),
        chooser,
        background,
    ]);

    // CR 702.124f: the granted generic Partner does not satisfy a
    // "choose a Background" pairing — different partner abilities cannot be
    // combined.
    assert!(!can_pair_commanders(
        &db,
        "Mono White Legend",
        "Background Chooser",
        CMM_GRANT
    ));

    // Reach guard on the same card: its "choose a Background" ability is live,
    // so the rejection above is the family boundary and not a dead keyword.
    assert!(can_pair_commanders(
        &db,
        "Background Chooser",
        "A Background",
        CMM_GRANT
    ));
}

/// A non-legendary card is not commander-eligible at all, so it cannot receive
/// the grant however few colours it has. Guards the `by_itself` conjunct from
/// collapsing into "any mono-colour card".
#[test]
fn a_nonlegendary_card_gains_nothing_under_the_grant() {
    let common = CardFace {
        name: "Ordinary Creature".to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        },
        color_identity: vec![ManaColor::Red],
        ..CardFace::default()
    };
    assert_eq!(commander_qualification(&common), CommanderQualification::No);

    let db = db_of(vec![
        legend("Mono White Legend", vec![ManaColor::White]),
        common,
    ]);
    assert!(!can_pair_commanders(
        &db,
        "Mono White Legend",
        "Ordinary Creature",
        CMM_GRANT
    ));
}
