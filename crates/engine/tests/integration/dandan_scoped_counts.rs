//! Dandân shared pile: scoped zone counts read the pile through the storage
//! authority and count it once, not once per seat.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::dandan_filter_owner_axis::{dandan, scenario, stage, start};
use crate::support::shared_card_db;

const STANDARD: fn() -> FormatConfig = FormatConfig::standard;

/// Four distinct card types, in the order a delirium row grows through them.
const FOUR_TYPES: [&str; 4] = ["Grizzly Bears", "Lightning Bolt", "Divination", "Island"];

fn many(owner: PlayerId, name: &'static str, count: usize) -> Vec<(PlayerId, &'static str)> {
    vec![(owner, name); count]
}

fn owned(owner: PlayerId, names: &[&'static str]) -> Vec<(PlayerId, &'static str)> {
    names.iter().map(|&name| (owner, name)).collect()
}

/// A scenario with `permanent` (owned by `controller`) on the battlefield and `zone` filled from
/// `cards`; returns the runner (layers evaluated) and the permanent.
fn board(
    format: FormatConfig,
    controller: PlayerId,
    permanent: &str,
    zone: Zone,
    cards: &[(PlayerId, &str)],
) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    let id = sc.add_real_card(controller, permanent, Zone::Battlefield, db);
    stage(&mut sc, db, zone, cards);
    let mut runner = start(sc, controller);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    (runner, id)
}

fn pt(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = &runner.state().objects[&id];
    (obj.power.expect("power"), obj.toughness.expect("toughness"))
}

fn has_trample(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Trample)
}

// ---------------------------------------------------------------------------
// V5: CardTypeSetSource, You scope (Backwoods Survivalists)
// ---------------------------------------------------------------------------

#[test]
fn v5_delirium_reads_the_shared_graveyard_from_either_seat() {
    if shared_card_db().is_none() {
        return;
    }
    for seat in [P1, P0] {
        let (runner, id) = board(
            dandan(),
            seat,
            "Backwoods Survivalists",
            Zone::Graveyard,
            &owned(P0, &FOUR_TYPES),
        );
        assert_eq!(pt(&runner, id), (5, 4), "{seat:?}: four types in the pile");
        assert!(has_trample(&runner, id));

        let (runner, id) = board(
            dandan(),
            seat,
            "Backwoods Survivalists",
            Zone::Graveyard,
            &owned(P0, &FOUR_TYPES[..3]),
        );
        assert_eq!(pt(&runner, id), (4, 3), "{seat:?}: three types");
        assert!(!has_trample(&runner, id));
    }
}

#[test]
fn v5_delirium_in_standard_reads_only_the_controllers_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, id) = board(
        STANDARD(),
        P1,
        "Backwoods Survivalists",
        Zone::Graveyard,
        &owned(P1, &FOUR_TYPES),
    );
    assert_eq!(pt(&runner, id), (5, 4), "own graveyard");
    let (runner, id) = board(
        STANDARD(),
        P1,
        "Backwoods Survivalists",
        Zone::Graveyard,
        &owned(P0, &FOUR_TYPES),
    );
    assert_eq!(pt(&runner, id), (4, 3), "the other seat's graveyard");
}

// ---------------------------------------------------------------------------
// V6: CardTypeSetSource, Opponents and All scope
// ---------------------------------------------------------------------------

#[test]
fn v6_opponents_graveyard_types_read_the_shared_pile_from_either_seat() {
    if shared_card_db().is_none() {
        return;
    }
    for seat in [P0, P1] {
        let (runner, id) = board(
            dandan(),
            seat,
            "Nighthawk Scavenger",
            Zone::Graveyard,
            &owned(P0, &["Grizzly Bears", "Lightning Bolt"]),
        );
        assert_eq!(pt(&runner, id).0, 3, "{seat:?}: 1 + two card types");
    }
}

#[test]
fn v6_opponents_graveyard_types_in_standard_read_the_other_seat() {
    if shared_card_db().is_none() {
        return;
    }
    let mut cards = owned(P0, &["Grizzly Bears", "Lightning Bolt", "Divination"]);
    cards.extend(owned(P1, &["Island"]));
    let (runner, id) = board(
        STANDARD(),
        P0,
        "Nighthawk Scavenger",
        Zone::Graveyard,
        &cards,
    );
    assert_eq!(pt(&runner, id).0, 2, "only P1's land counts: 1 + 1");
}

#[test]
fn v6_all_graveyards_types_count_the_pile_once() {
    if shared_card_db().is_none() {
        return;
    }
    for seat in [P0, P1] {
        let (runner, id) = board(
            dandan(),
            seat,
            "Tarmogoyf",
            Zone::Graveyard,
            &owned(P0, &["Grizzly Bears", "Lightning Bolt"]),
        );
        assert_eq!(pt(&runner, id), (2, 3), "{seat:?}");
    }
}

// ---------------------------------------------------------------------------
// V7 / V8: GraveyardSize
// ---------------------------------------------------------------------------

#[test]
fn v7_threshold_reads_the_shared_graveyard_from_either_seat() {
    if shared_card_db().is_none() {
        return;
    }
    for seat in [P1, P0] {
        let (runner, id) = board(
            dandan(),
            seat,
            "Nimble Mongoose",
            Zone::Graveyard,
            &many(P0, "Island", 7),
        );
        assert_eq!(pt(&runner, id), (3, 3), "{seat:?}: seven in the pile");
        let (runner, id) = board(
            dandan(),
            seat,
            "Nimble Mongoose",
            Zone::Graveyard,
            &many(P0, "Island", 6),
        );
        assert_eq!(pt(&runner, id), (1, 1), "{seat:?}: six in the pile");
    }
}

#[test]
fn v7_threshold_in_standard_reads_only_the_controllers_graveyard() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, id) = board(
        STANDARD(),
        P1,
        "Nimble Mongoose",
        Zone::Graveyard,
        &many(P1, "Island", 7),
    );
    assert_eq!(pt(&runner, id), (3, 3), "own seven");
    let (runner, id) = board(
        STANDARD(),
        P1,
        "Nimble Mongoose",
        Zone::Graveyard,
        &many(P0, "Island", 7),
    );
    assert_eq!(pt(&runner, id), (1, 1), "the other seat's seven");
}

/// P1 casts Visions of Beyond over `graveyard` cards; returns how many cards the pile library lost.
fn visions_library_loss(graveyard: usize) -> usize {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(dandan());
    let spell = sc.add_real_card(P1, "Visions of Beyond", Zone::Hand, db);
    stage(&mut sc, db, Zone::Graveyard, &many(P0, "Island", graveyard));
    stage(&mut sc, db, Zone::Library, &many(P0, "Island", 6));
    let mut runner = start(sc, P1);
    runner.cast(spell).resolve();
    6 - runner.state().library_of(P1).len()
}

#[test]
fn v8_a_graveyard_with_twenty_cards_is_the_pile_for_either_threshold_side() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(visions_library_loss(19), 1, "reach: below twenty draws one");
    assert_eq!(
        visions_library_loss(20),
        3,
        "twenty in the pile draws three"
    );
}

// ---------------------------------------------------------------------------
// V9: player count over a graveyard scalar (Master's Councillors)
// ---------------------------------------------------------------------------

#[test]
fn v9_each_graveyard_with_seven_cards_counts_the_shared_pile_once() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, id) = board(
        dandan(),
        P1,
        "Master's Councillors",
        Zone::Graveyard,
        &many(P0, "Island", 8),
    );
    assert_eq!(pt(&runner, id).0, 3, "1 + 2 for the one pile");
    let (runner, id) = board(
        dandan(),
        P1,
        "Master's Councillors",
        Zone::Graveyard,
        &many(P0, "Island", 6),
    );
    assert_eq!(pt(&runner, id).0, 1, "six cards: no graveyard qualifies");
}

#[test]
fn v9_each_graveyard_with_seven_cards_in_standard_counts_each_seat() {
    if shared_card_db().is_none() {
        return;
    }
    let mut both = many(P0, "Island", 8);
    both.extend(many(P1, "Island", 8));
    let (runner, id) = board(
        STANDARD(),
        P1,
        "Master's Councillors",
        Zone::Graveyard,
        &both,
    );
    assert_eq!(pt(&runner, id).0, 5, "two graveyards qualify");
    let (runner, id) = board(
        STANDARD(),
        P1,
        "Master's Councillors",
        Zone::Graveyard,
        &many(P0, "Island", 8),
    );
    assert_eq!(pt(&runner, id).0, 3, "one graveyard qualifies");
}

// ---------------------------------------------------------------------------
// V10: ZoneCardCount, Library, You
// ---------------------------------------------------------------------------

fn fractal_counters(runner: &GameRunner) -> u32 {
    runner
        .state()
        .objects
        .values()
        .find(|obj| obj.name == "Fractal" && obj.zone == Zone::Battlefield)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .expect("a Fractal token with +1/+1 counters")
}

fn body_of_research_counters(format: FormatConfig, library_owner: PlayerId) -> u32 {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    let spell = sc.add_real_card(P1, "Body of Research", Zone::Hand, db);
    stage(
        &mut sc,
        db,
        Zone::Library,
        &many(library_owner, "Island", 12),
    );
    let mut runner = start(sc, P1);
    runner.cast(spell).resolve();
    fractal_counters(&runner)
}

#[test]
fn v10_cards_in_your_library_count_the_shared_pile() {
    if shared_card_db().is_none() {
        return;
    }
    assert_eq!(body_of_research_counters(dandan(), P0), 12);
    assert_eq!(body_of_research_counters(STANDARD(), P1), 12, "own library");
}

#[test]
fn v10_an_empty_library_condition_sees_the_shared_pile() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, id) = board(
        dandan(),
        P1,
        "Living Conundrum",
        Zone::Library,
        &many(P0, "Island", 3),
    );
    assert_eq!(
        pt(&runner, id),
        (2, 5),
        "a non-empty pile is not 'no cards'"
    );
    let (runner, id) = board(dandan(), P1, "Living Conundrum", Zone::Library, &[]);
    assert_eq!(pt(&runner, id), (10, 10), "reach: an empty pile is");
}

// ---------------------------------------------------------------------------
// V11: ZoneCardCount, All scope
// ---------------------------------------------------------------------------

#[test]
fn v11_instants_in_all_graveyards_count_the_pile_once() {
    if shared_card_db().is_none() {
        return;
    }
    for seat in [P0, P1] {
        let mut cards = many(P0, "Lightning Bolt", 2);
        cards.extend(many(P1, "Lightning Bolt", 1));
        let (runner, id) = board(dandan(), seat, "Cognivore", Zone::Graveyard, &cards);
        assert_eq!(
            pt(&runner, id),
            (3, 3),
            "{seat:?}: three instants, once each"
        );
    }
}

#[test]
fn v11_instants_in_all_graveyards_in_standard_sum_every_seat() {
    if shared_card_db().is_none() {
        return;
    }
    let mut cards = many(P0, "Lightning Bolt", 2);
    cards.extend(many(P1, "Lightning Bolt", 1));
    let (runner, id) = board(STANDARD(), P0, "Cognivore", Zone::Graveyard, &cards);
    assert_eq!(pt(&runner, id), (3, 3), "2 + 1 across distinct graveyards");
}
