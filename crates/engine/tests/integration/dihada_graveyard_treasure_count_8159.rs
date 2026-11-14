//! Regression test for GitHub issue #8159 — Dihada, Binder of Wills's -3
//! doesn't create Treasure tokens for cards put into the graveyard.
//!
//! Oracle (Dihada, Binder of Wills -3):
//! > Reveal the top four cards of your library. Put any number of legendary
//! > cards from among them into your hand and the rest into your graveyard.
//! > Create a Treasure token for each card put into your graveyard this way.
//!
//! The bug: the Dig continuation always published the KEPT (hand-bound)
//! partition as the tracked set a downstream count reads, even when the
//! Oracle text names the GRAVEYARD (rest) partition instead. Activating -3,
//! revealing four cards, and putting a chosen legendary subset into hand
//! correctly moved the rest to the graveyard, but the Treasure count read
//! the (possibly empty) kept-hand partition instead — so choosing to keep
//! zero cards (this issue's exact repro) created zero Treasures despite all
//! four cards reaching the graveyard.
//!
//! This drives the real pipeline end-to-end: activate the printed -3 loyalty
//! ability from Dihada's verbatim Oracle text, answer the resulting
//! `WaitingFor::DigChoice` with a chosen kept subset, and assert the
//! Treasure count tracks the GRAVEYARD partition's size dynamically:
//!   * zero legendaries kept -> all four cards hit the graveyard -> 4 Treasures
//!   * one of two legendaries kept -> three cards hit the graveyard -> 3 Treasures
//!
//! Covering both counts (not just "it fires once") proves the count is bound
//! to the actual runtime size of the rest pile, not a fixed sentinel.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DIHADA_ORACLE: &str = concat!(
    "+2: Up to one target legendary creature gains vigilance, lifelink, and ",
    "indestructible until your next turn.\n",
    "\u{2212}3: Reveal the top four cards of your library. Put any number of ",
    "legendary cards from among them into your hand and the rest into your ",
    "graveyard. Create a Treasure token for each card put into your ",
    "graveyard this way.\n",
    "\u{2212}11: Gain control of all nonland permanents until end of turn. ",
    "Untap them. They gain haste.",
);

fn treasure_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.controller == player)
        .filter(|obj| obj.name.eq_ignore_ascii_case("Treasure"))
        .count()
}

/// P0 controls Dihada plus a four-card library top. The first
/// `legendary_count` cards from the top are stamped Legendary; the rest are
/// plain vanilla cards. Returns the runner (parked in P0's main phase),
/// Dihada's id, and the four library card ids in top-to-bottom order.
fn setup(legendary_count: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dihada = scenario
        .add_planeswalker_from_oracle(P0, "Dihada, Binder of Wills", "Dihada", 5, DIHADA_ORACLE)
        .id();

    // `add_card_to_library_top` always re-seats its card at index 0, so
    // inserting bottom-first (index 3 down to 0) leaves the library
    // top-to-bottom as [card0, card1, card2, card3].
    let mut by_index = vec![ObjectId(0); 4];
    for i in (0..4).rev() {
        let id = scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
        by_index[i] = id;
    }

    let mut runner = scenario.build();
    for (i, &id) in by_index.iter().enumerate() {
        if i < legendary_count {
            let obj = runner
                .state_mut()
                .objects
                .get_mut(&id)
                .expect("library card exists");
            obj.card_types.supertypes = vec![Supertype::Legendary];
        }
    }
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::PreCombatMain;
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    (runner, dihada, by_index)
}

/// Activate Dihada's -3 (loyalty ability index 1) and answer the resulting
/// dig choice by keeping exactly `keep`.
fn activate_minus_three_and_keep(runner: &mut GameRunner, dihada: ObjectId, keep: &[ObjectId]) {
    runner.activate(dihada, 1).resolve();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "expected WaitingFor::DigChoice after activating -3, got {}",
        runner.waiting_for_kind()
    );
    runner
        .act(GameAction::SelectCards {
            cards: keep.to_vec(),
        })
        .expect("submit the -3 dig keep selection");
}

/// Discriminating case #1 (the issue's exact repro): 1 of 4 revealed cards is
/// legendary; the controller keeps NONE of it. All four cards must land in
/// the graveyard and Dihada must create exactly 4 Treasures — not zero.
#[test]
fn dihada_minus_three_keeping_no_legendaries_creates_treasure_per_graveyard_card() {
    let (mut runner, dihada, cards) = setup(1);

    activate_minus_three_and_keep(&mut runner, dihada, &[]);

    let state = runner.state();
    for &id in &cards {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Graveyard,
            "every revealed card must reach the graveyard when none are kept"
        );
    }
    assert_eq!(
        treasure_count(&runner, P0),
        4,
        "Dihada must create one Treasure for each of the 4 cards put into the \
         graveyard this way, even when zero legendaries were kept"
    );
}

/// Discriminating case #2: 2 of 4 revealed cards are legendary; the
/// controller keeps exactly ONE. Only 3 cards land in the graveyard, so
/// Dihada must create exactly 3 Treasures — proving the count tracks the
/// REST partition's actual size rather than firing a fixed amount.
#[test]
fn dihada_minus_three_keeping_one_legendary_creates_fewer_treasures() {
    let (mut runner, dihada, cards) = setup(2);
    let kept_card = cards[0];

    activate_minus_three_and_keep(&mut runner, dihada, &[kept_card]);

    let state = runner.state();
    assert_eq!(
        state.objects[&kept_card].zone,
        Zone::Hand,
        "the chosen legendary card must be put into hand"
    );
    for &id in &cards[1..] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Graveyard,
            "every non-kept revealed card must reach the graveyard"
        );
    }
    assert_eq!(
        treasure_count(&runner, P0),
        3,
        "Dihada must create one Treasure for each of the 3 cards put into the \
         graveyard this way — fewer than the all-declined case, proving the \
         count is dynamic rather than a fixed \"always fires once\" stub"
    );
}
