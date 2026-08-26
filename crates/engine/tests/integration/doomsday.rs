//! Runtime regression coverage for Doomsday's multi-zone search, remainder
//! exile, and five-card library ordering.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::SearchOrderingHint;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DOOMSDAY_ORACLE: &str = "Search your library and graveyard for five cards and exile the rest. Put the chosen cards on top of your library in any order. You lose half your life, rounded up.";

fn named_id(runner: &GameRunner, name: &str) -> engine::types::identifiers::ObjectId {
    runner
        .state()
        .objects
        .iter()
        .find_map(|(id, object)| (object.name == name).then_some(*id))
        .unwrap_or_else(|| panic!("missing scenario card {name:?}"))
}

/// CR 401.4 + CR 701.23a + CR 608.2c: drive the real cast/apply pipeline,
/// choose five cards from the library/graveyard in a deliberate order, and
/// verify that only the unchosen searched-zone cards are exiled. The selected
/// order is the order submitted through the production SearchChoice action;
/// `PutAtLibraryPosition` preserves it when it resolves. Reverting either
/// parser continuation leaves the selected cards in their original zones or
/// sends them through the wrong destination.
#[test]
fn doomsday_exiles_search_remainder_and_orders_five_chosen_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Library A",
            "Library B",
            "Library C",
            "Library D",
            "Library E",
            "Library F",
            "Library G",
        ],
    );
    scenario.with_graveyard(P0, &["Graveyard A", "Graveyard B", "Graveyard C"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Doomsday", false, DOOMSDAY_ORACLE)
        .id();
    let mut runner = scenario.build();

    let library_a = named_id(&runner, "Library A");
    let library_c = named_id(&runner, "Library C");
    let library_e = named_id(&runner, "Library E");
    let graveyard_a = named_id(&runner, "Graveyard A");
    let graveyard_b = named_id(&runner, "Graveyard B");
    let graveyard_c = named_id(&runner, "Graveyard C");
    let unchosen = [
        named_id(&runner, "Library B"),
        named_id(&runner, "Library D"),
        named_id(&runner, "Library F"),
        named_id(&runner, "Library G"),
        graveyard_c,
    ];
    let chosen = vec![library_c, graveyard_a, library_e, graveyard_b, library_a];
    let expected_top_order = chosen.clone();

    let mut cast = runner.cast(spell).commit();
    for _ in 0..2 {
        assert!(matches!(
            cast.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        cast.act(GameAction::PassPriority)
            .expect("passing priority should resolve Doomsday");
        if !matches!(cast.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
    }

    let search_cards = match &cast.state().waiting_for {
        WaitingFor::SearchChoice {
            cards,
            count,
            ordering_hint,
            ..
        } => {
            assert_eq!(*count, 5, "Doomsday must require five selected cards");
            assert_eq!(*ordering_hint, SearchOrderingHint::OrderedToLibraryTop);
            assert!(chosen.iter().all(|id| cards.contains(id)));
            cards.clone()
        }
        other => panic!("expected SearchChoice for Doomsday, got {other:?}"),
    };
    assert_eq!(
        search_cards.len(),
        10,
        "library + graveyard search candidates"
    );

    let resolution = cast
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("selecting Doomsday's five cards should be legal");

    assert!(
        !resolution.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Exile,
                ..
            } if chosen.contains(object_id)
        )),
        "chosen cards must never undergo the remainder's exile move; events={:?}",
        resolution.events
    );

    assert!(
        matches!(cast.state().waiting_for, WaitingFor::Priority { .. }),
        "Doomsday should finish its resolution after the ordered search choice, got {:?}",
        cast.state().waiting_for
    );
    assert_eq!(
        cast.state().players[P0.0 as usize].life,
        10,
        "the verbatim Doomsday text must also resolve its half-life loss"
    );

    let actual_top_order: Vec<_> = cast.state().players[P0.0 as usize]
        .library
        .iter()
        .take(5)
        .copied()
        .collect();
    assert_eq!(
        actual_top_order, expected_top_order,
        "the selected cards must be placed on top in the submitted order"
    );
    for id in unchosen {
        assert_eq!(
            cast.state().objects[&id].zone,
            Zone::Exile,
            "unchosen library cards must be exiled"
        );
    }
    assert_eq!(
        cast.state().objects[&graveyard_a].zone,
        Zone::Library,
        "chosen graveyard cards must be ordered into the library"
    );
    assert_eq!(
        cast.state().objects[&graveyard_b].zone,
        Zone::Library,
        "chosen graveyard cards must be ordered into the library"
    );
}
