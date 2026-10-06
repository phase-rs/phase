//! CR 701.20a + CR 608.2c: Fathom Trawl — "Reveal cards from the top of your
//! library until you reveal three nonland cards. Put the nonland cards revealed
//! this way into your hand, then put the rest of the revealed cards on the bottom
//! of your library in any order."
//!
//! The revealed population includes the lands met on the way; only the nonland
//! cards go to hand, the lands go to the bottom, and cards below the third
//! nonland are never touched.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FATHOM_TRAWL: &str = "Reveal cards from the top of your library until you reveal three nonland cards. Put the nonland cards revealed this way into your hand, then put the rest of the revealed cards on the bottom of your library in any order.";

struct Staged {
    runner: GameRunner,
    spell: ObjectId,
    nonlands: Vec<ObjectId>,
    lands: Vec<ObjectId>,
    deep: ObjectId,
}

/// Top to bottom: land, nonland, land, nonland, land, nonland, deep card.
fn stage() -> Staged {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fathom Trawl", false, FATHOM_TRAWL)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let mut nonlands = Vec::new();
    let mut lands = Vec::new();
    for i in (0..3).rev() {
        nonlands.insert(
            0,
            scenario
                .add_spell_to_library_top(P0, &format!("Nonland {i}"), false)
                .id(),
        );
        lands.insert(
            0,
            scenario
                .add_spell_to_library_top(P0, &format!("Land {i}"), false)
                .as_land()
                .id(),
        );
    }
    Staged {
        runner: scenario.build(),
        spell,
        nonlands,
        lands,
        deep,
    }
}

#[test]
fn only_the_nonland_cards_go_to_hand_and_the_lands_go_to_the_bottom() {
    let Staged {
        mut runner,
        spell,
        nonlands,
        lands,
        deep,
    } = stage();

    // Drive by hand: the cast driver would answer the ordering prompt itself.
    let mut committed = runner.cast(spell).commit();
    committed.act(GameAction::PassPriority).unwrap();
    committed.act(GameAction::PassPriority).unwrap();

    let WaitingFor::RevealUntilBottomOrder { cards, .. } = committed.state().waiting_for.clone()
    else {
        panic!(
            "expected the any-order bottom prompt, got {:?}",
            committed.state().waiting_for
        );
    };
    assert_eq!(
        cards
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        lands.iter().copied().collect(),
        "only the missed lands are ordered onto the bottom"
    );
    let order: Vec<ObjectId> = lands.iter().rev().copied().collect();
    committed
        .act(GameAction::SelectCards {
            cards: order.clone(),
        })
        .expect("bottom order is accepted");

    let state = committed.state();
    let mut hand: Vec<ObjectId> = state.players[P0.0 as usize].hand.iter().copied().collect();
    hand.sort();
    let mut expected_hand = nonlands.clone();
    expected_hand.sort();
    assert_eq!(
        hand, expected_hand,
        "exactly the three nonland cards are in hand"
    );
    let library: Vec<ObjectId> = state.players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(library.first(), Some(&deep), "the deeper card is untouched");
    assert_eq!(
        library[1..],
        order[..],
        "the missed lands are on the bottom in the chosen order"
    );
    for land in &lands {
        assert_eq!(state.objects[land].zone, Zone::Library);
    }
}
