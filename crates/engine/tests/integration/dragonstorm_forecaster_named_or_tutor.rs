//! Runtime regression for the named-card bare-`or` tutor class. Dragonstorm
//! Forecaster must offer either printed name, not one impossible combined name.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "{2}, {T}: Search your library for a card named Dragonstorm Globe or Boulderborn Dragon, reveal it, put it into your hand, then shuffle.";

#[derive(Clone, Copy)]
enum SearchSelection {
    DragonstormGlobe,
    BoulderbornDragon,
    FailToFind,
}

fn payable_mana() -> Vec<ManaUnit> {
    (0..2)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn run_named_tutor(selection: SearchSelection) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forecaster = scenario
        .add_creature_from_oracle(P0, "Dragonstorm Forecaster", 2, 3, ORACLE)
        .id();
    let globe = scenario.add_card_to_library_top(P0, "Dragonstorm Globe");
    let dragon = scenario.add_card_to_library_top(P0, "Boulderborn Dragon");
    let unrelated = scenario.add_card_to_library_top(P0, "Unrelated Card");
    scenario.with_mana_pool(P0, payable_mana());

    let mut runner = scenario.build();

    // CR 602.2a-b: activate the printed ability and pay both its mana and tap
    // costs through the normal activation pipeline.
    let outcome = runner.activate(forecaster, 0).resolve();
    assert!(outcome.is_tapped(forecaster), "tap cost must be paid");
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the two generic mana must be spent"
    );

    // CR 701.23a: both stated names, and only those names, are legal finds.
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice {
            player,
            cards,
            count,
            reveal,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*count, 1);
            assert!(*reveal);
            assert_eq!(cards.len(), 2, "exactly the two printed names are legal");
            assert!(cards.contains(&globe), "Dragonstorm Globe must be offered");
            assert!(
                cards.contains(&dragon),
                "Boulderborn Dragon must be offered"
            );
            assert!(
                !cards.contains(&unrelated),
                "unrelated card must be excluded"
            );
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }

    let (chosen, other_legal) = match selection {
        SearchSelection::DragonstormGlobe => (Some(globe), Some(dragon)),
        SearchSelection::BoulderbornDragon => (Some(dragon), Some(globe)),
        SearchSelection::FailToFind => (None, None),
    };

    runner
        .act(GameAction::SelectCards {
            cards: chosen.into_iter().collect(),
        })
        .expect("the stated-quality search selection must be accepted");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "search and shuffle continuation must drain to priority"
    );
    assert_eq!(runner.state().objects[&unrelated].zone, Zone::Library);

    if let Some(chosen) = chosen {
        assert_eq!(runner.state().objects[&chosen].zone, Zone::Hand);
        assert!(runner.state().players[P0.0 as usize].hand.contains(&chosen));
        let other_legal = other_legal.expect("a positive selection has one other legal card");
        assert_eq!(runner.state().objects[&other_legal].zone, Zone::Library);
        assert!(runner.state().players[P0.0 as usize]
            .library
            .contains(&other_legal));
    } else {
        // CR 701.23b: a stated-quality search of a hidden zone may legally fail
        // to find even when matching cards are present.
        for id in [globe, dragon, unrelated] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
            assert!(runner.state().players[P0.0 as usize].library.contains(&id));
        }
    }
}

#[test]
fn dragonstorm_forecaster_can_find_dragonstorm_globe() {
    run_named_tutor(SearchSelection::DragonstormGlobe);
}

#[test]
fn dragonstorm_forecaster_can_find_boulderborn_dragon() {
    run_named_tutor(SearchSelection::BoulderbornDragon);
}

#[test]
fn dragonstorm_forecaster_may_fail_to_find() {
    run_named_tutor(SearchSelection::FailToFind);
}
