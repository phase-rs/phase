//! Dandân shared graveyard: "your graveyard" is the whole shared pile, so a
//! player-scoped mass move takes every card in it, whichever seat owns the card
//! (CR 400.1 as modified by the format). With separate graveyards the scoped
//! player moves only the cards they own.

use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::dandan_filter_owner_axis::{dandan, scenario, stage, start};
use crate::support::shared_card_db;

/// `actor` activates Feldon's Cane over a graveyard holding one card per seat;
/// returns the runner and the staged `(owner, card)` pairs.
fn cane_run(format: FormatConfig, actor: PlayerId) -> (GameRunner, Vec<(PlayerId, ObjectId)>) {
    let db = shared_card_db().expect("card db");
    let mut sc = scenario(format);
    let cane = sc.add_real_card(actor, "Feldon's Cane", Zone::Battlefield, db);
    let owners = [P0, P1, P0, P1];
    let cards = stage(
        &mut sc,
        db,
        Zone::Graveyard,
        &owners.map(|owner| (owner, "Grizzly Bears")),
    );
    let mut runner = start(sc, actor);
    runner.state_mut().objects.get_mut(&cane).unwrap().tapped = false;
    runner.activate(cane, 0).resolve();
    (runner, owners.into_iter().zip(cards).collect())
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

#[test]
fn the_whole_shared_graveyard_moves_for_either_seat() {
    if shared_card_db().is_none() {
        return;
    }
    for actor in [P0, P1] {
        let (runner, cards) = cane_run(dandan(), actor);
        for (owner, id) in &cards {
            assert_eq!(
                zone_of(&runner, *id),
                Zone::Library,
                "{actor:?} shuffles the whole pile, including {owner:?}'s card"
            );
        }
        assert_eq!(
            runner.state().library_of(actor).len(),
            cards.len(),
            "reach: the pile holds the moved cards"
        );
    }
}

#[test]
fn separate_graveyards_move_only_the_activators_cards() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, cards) = cane_run(FormatConfig::standard(), P1);
    for (owner, id) in &cards {
        let expected = if *owner == P1 {
            Zone::Library
        } else {
            Zone::Graveyard
        };
        assert_eq!(zone_of(&runner, *id), expected, "{owner:?}'s card");
    }
}
