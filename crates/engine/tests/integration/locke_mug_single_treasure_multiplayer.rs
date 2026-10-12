//! Locke, Treasure Hunter — "If a land card was milled this way" is ONE
//! look-back over the whole multi-player mill, not a per-player gate (#9225).
//!
//! Oracle (Mug): "Whenever Locke attacks, each player mills a card. If a land
//! card was milled this way, create a Treasure token. Until end of turn, you may
//! cast a spell from among those cards."
//!
//! Ruling (2025-06-06): "As long as one or more lands were milled this way,
//! you'll create a Treasure token. Additional land cards milled beyond the first
//! won't cause you to create additional Treasures."
//!
//! CR anchors:
//!   - CR 608.2f: "each player mills a card" is one action taken on multiple
//!     players; the engine processes it per player, but it stays ONE action.
//!   - CR 608.2c: the following sentence is the next instruction in the order
//!     written, so its "this way" condition reads that whole action's result
//!     once, and its "you" is Locke's controller (CR 109.5).
//!
//! The defect only shows with three or more players: the player-scope splitter
//! kept every `ZoneChangedThisWay`-gated sub inside the per-player iteration,
//! so the Treasure clause re-ran once per player whose own mill was a land, with
//! its implicit controller rebound to that player. In a two-player game the
//! symptom can coincide with the correct answer, so these fixtures use four.
//!
//! The per-player class (Kroxa's "each opponent who didn't discard a nonland
//! card this way loses 3 life") must keep its per-iteration gate — that is
//! pinned by `kroxa_titan_nonland_discard_life_loss`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall `cards/named?exact=Locke, Treasure Hunter`).
const LOCKE: &str = "Locke can't be blocked by creatures with greater power.\n\
     Mug — Whenever Locke attacks, each player mills a card. If a land card was \
     milled this way, create a Treasure token. Until end of turn, you may cast a \
     spell from among those cards.";

const PLAYER_COUNT: u8 = 4;
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Four players, Locke (P0) attacking P1, with one staged card on top of every
/// player's library: a land for each player in `land_milled_by`, a spell for the
/// rest. Returns the runner after the Mug trigger has resolved, plus the staged
/// cards in seat order.
fn locke_attacks_in_four_player_game(land_milled_by: &[PlayerId]) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9225);
    // CR 504.1: start past the draw step so the staged top cards stay on top.
    scenario.at_phase(Phase::PreCombatMain);
    let locke = scenario
        .add_creature_from_oracle(P0, "Locke, Treasure Hunter", 3, 3, LOCKE)
        .id();
    let staged: Vec<ObjectId> = (0..PLAYER_COUNT)
        .map(|seat| {
            let player = PlayerId(seat);
            if land_milled_by.contains(&player) {
                scenario.add_land_to_library_top(player, "Milled Land").id()
            } else {
                scenario
                    .add_spell_to_library_top(player, "Milled Spell", false)
                    .id()
            }
        })
        .collect();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(locke, AttackTarget::Player(P1))])
        .expect("Locke must be able to attack");
    runner.advance_until_stack_empty();
    (runner, staged)
}

/// Controllers of every Treasure on the battlefield.
fn treasure_controllers(runner: &GameRunner) -> Vec<PlayerId> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == "Treasure")
        .map(|object| object.controller)
        .collect()
}

fn assert_every_staged_card_was_milled(runner: &GameRunner, staged: &[ObjectId]) {
    for (seat, id) in staged.iter().enumerate() {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Graveyard,
            "reach guard: player {seat} must have milled their staged top card"
        );
    }
}

/// CR 608.2c + CR 608.2f: every one of four players mills a land, and Locke's
/// controller creates exactly ONE Treasure. Reverting the splitter fix keeps the
/// gated Token inside the per-player iteration, creating a Treasure per player
/// (each under the iterating player's control).
#[test]
fn every_player_milling_a_land_creates_exactly_one_treasure_for_lockes_controller() {
    let (runner, staged) = locke_attacks_in_four_player_game(&[P0, P1, P2, P3]);
    assert_every_staged_card_was_milled(&runner, &staged);

    assert_eq!(
        treasure_controllers(&runner),
        vec![P0],
        "one or more lands milled this way creates exactly one Treasure, under \
         Locke's controller — not one per player who milled a land"
    );
}

/// CR 109.5 + CR 608.2c: only an opponent mills a land. The single Treasure is
/// still Locke's controller's — "you" never rebinds to the player whose mill
/// satisfied the condition. Reverting the fix hands the Treasure to P2.
#[test]
fn an_opponents_milled_land_creates_the_treasure_for_lockes_controller() {
    let (runner, staged) = locke_attacks_in_four_player_game(&[P2]);
    assert_every_staged_card_was_milled(&runner, &staged);

    assert_eq!(
        treasure_controllers(&runner),
        vec![P0],
        "the Treasure is created by Locke's controller even though only P2 milled a land"
    );
}

/// CR 608.2c: with no land among the milled cards the condition is false and no
/// Treasure is created. Pairs with the test above so the gate is shown to
/// discriminate, not merely to fire once.
#[test]
fn no_land_milled_creates_no_treasure() {
    let (runner, staged) = locke_attacks_in_four_player_game(&[]);
    assert_every_staged_card_was_milled(&runner, &staged);

    assert!(
        treasure_controllers(&runner).is_empty(),
        "no land card was milled this way, so no Treasure is created"
    );
}
