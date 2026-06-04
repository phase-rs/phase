//! Regression (issue #736): Cultivate-class search split-destination.
//!
//! Cultivate: "Search your library for up to two basic land cards, reveal those
//! cards, put one onto the battlefield tapped and the other into your hand, then
//! shuffle."
//!
//! Before the fix, the "...and the other into your hand" half was silently
//! dropped: the parser collapsed the destination to a single `ChangeZone ->
//! Battlefield` and the runtime moved BOTH found cards onto the battlefield.
//!
//! These are full `apply()`-driven pipeline tests: they cast Cultivate, walk the
//! SearchChoice -> SearchPartitionChoice -> SelectCards handoff, and assert the
//! partition (CR 701.23a + CR 608.2c), the tapped battlefield entry via the ETB
//! pipeline (CR 614.1 / CR 110.5b), and the CR 609.3 single-basic fast-path.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

/// {2}{G} for Cultivate.
fn add_cultivate_mana(runner: &mut engine::game::scenario::GameRunner) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    pool.add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    pool.add(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(0),
        false,
        vec![],
    ));
    pool.add(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(0),
        false,
        vec![],
    ));
}

#[test]
fn discriminator_cultivate_splits_battlefield_and_hand() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    // A third library card so the post-search shuffle is observable.
    scenario.add_real_card(P0, "Island", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_cultivate_mana(&mut runner);

    // The cast driver resolves Cultivate and stops at the SearchChoice boundary
    // (default SearchPolicy::Stop), leaving the live runner parked there.
    let outcome = runner.cast(cultivate).resolve();

    // Step 1: SearchChoice — pick BOTH basics as the found set.
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(cards.contains(&forest) && cards.contains(&mountain));
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest, mountain],
        })
        .expect("selecting both basics should park the partition choice");

    // Step 2: SearchPartitionChoice — found (2) > primary_count (1), so the
    // searcher chooses which one goes to the battlefield.
    match &runner.state().waiting_for {
        WaitingFor::SearchPartitionChoice {
            cards,
            primary_count,
            primary_destination,
            primary_enter_tapped,
            rest_destination,
            ..
        } => {
            assert_eq!(*primary_count, 1);
            assert_eq!(*primary_destination, Zone::Battlefield);
            assert!(*primary_enter_tapped);
            assert_eq!(*rest_destination, Zone::Hand);
            assert!(cards.contains(&forest) && cards.contains(&mountain));
        }
        other => panic!("expected SearchPartitionChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("partition pick should resolve");

    // Forest -> battlefield, TAPPED (proves it routed through change_zone::resolve
    // ETB pipeline, not a bare move_to_zone which would leave it untapped).
    let forest_obj = &runner.state().objects[&forest];
    assert_eq!(forest_obj.zone, Zone::Battlefield, "primary -> battlefield");
    assert!(
        forest_obj.tapped,
        "primary basic must enter TAPPED (CR 614.1)"
    );

    // Mountain -> hand (the previously-dropped half).
    assert_eq!(
        runner.state().objects[&mountain].zone,
        Zone::Hand,
        "rest -> hand"
    );
}

#[test]
fn cultivate_single_basic_auto_routes_no_partition() {
    // CR 609.3 fast-path: only one basic in the library, so found (1) <=
    // primary_count (1) — no SearchPartitionChoice is parked; the single basic
    // enters the battlefield tapped and the hand is unchanged.
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    // A nonbasic so only one basic is findable.
    scenario.add_real_card(P0, "Mishra's Factory", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_cultivate_mana(&mut runner);

    let hand_before = runner.state().players[0]
        .hand
        .iter()
        .filter(|&&id| id != cultivate)
        .count();

    // The cast driver resolves Cultivate and stops at the SearchChoice boundary
    // (default SearchPolicy::Stop), leaving the live runner parked there.
    let outcome = runner.cast(cultivate).resolve();

    // Only one legal basic -> pick it.
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(cards.contains(&forest));
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("selecting the single basic should resolve via the fast-path");

    // No partition prompt was parked (fast-path took over).
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::SearchPartitionChoice { .. }
        ),
        "single-basic search must NOT park a SearchPartitionChoice"
    );
    let forest_obj = &runner.state().objects[&forest];
    assert_eq!(forest_obj.zone, Zone::Battlefield);
    assert!(forest_obj.tapped, "fast-path basic must enter TAPPED");
    let hand_after = runner.state().players[0]
        .hand
        .iter()
        .filter(|&&id| id != cultivate)
        .count();
    assert_eq!(hand_after, hand_before, "hand unchanged by the fast-path");
}

#[test]
fn search_partition_selectcards_is_dispatched() {
    // Item 13a gate-reachability guard: drive the split to the
    // SearchPartitionChoice park, submit SelectCards, and assert it is HANDLED
    // (cards actually move) — not InvalidAction. Without the
    // `engine_resolution_choices::handles` registration the SelectCards would
    // fall through to InvalidAction and the cards would not move.
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cultivate = scenario.add_real_card(P0, "Cultivate", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_cultivate_mana(&mut runner);

    // The cast driver resolves Cultivate and stops at the SearchChoice boundary
    // (default SearchPolicy::Stop), leaving the live runner parked there.
    let _ = runner.cast(cultivate).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest, mountain],
        })
        .expect("found-set selection should park the partition choice");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::SearchPartitionChoice { .. }
        ),
        "partition choice must be parked",
    );

    // The dispatching assertion: SelectCards must be accepted (Ok), proving the
    // variant is registered in `handles` and routed to the new handler.
    let result = runner.act(GameAction::SelectCards {
        cards: vec![mountain],
    });
    assert!(
        result.is_ok(),
        "SearchPartitionChoice + SelectCards must be HANDLED, not InvalidAction: {result:?}"
    );
    // And the cards actually moved.
    assert_eq!(runner.state().objects[&mountain].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&forest].zone, Zone::Hand);
}
