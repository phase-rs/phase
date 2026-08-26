//! Combustible Gearhulk — the chosen opponent controls the optional draw and,
//! on decline, receives damage equal to the exact cards milled this way.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GEARHULK: &str = "First strike\nWhen this creature enters, target opponent may have you draw three cards. If the player doesn't, you mill three cards, then this creature deals damage to that player equal to the total mana value of those cards.";

fn setup() -> (GameRunner, ObjectId, i32, i32, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gearhulk = scenario
        .add_creature_to_hand_from_oracle(P0, "Combustible Gearhulk", 6, 6, GEARHULK)
        .with_mana_cost(ManaCost::zero())
        .id();
    // `add_spell_to_library_top` inserts at index zero, so reverse fixture
    // order preserves the intended top-three sequence 2, 5, 1.
    for (index, mana_value) in [2, 5, 1, 9].into_iter().rev().enumerate() {
        scenario
            .add_spell_to_library_top(P0, &format!("Gearhulk Library {index}"), false)
            .with_mana_cost(ManaCost::generic(mana_value));
    }
    let runner = scenario.build();
    let p0_life = runner.life(P0);
    let p1_life = runner.life(P1);
    let p0_hand = runner.state().players[P0.0 as usize].hand.len();
    (runner, gearhulk, p0_life, p1_life, p0_hand)
}

fn graveyard_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.owner == P0 && object.zone == Zone::Graveyard)
        .count()
}

#[test]
fn opponent_decline_mills_three_and_takes_exact_mana_value_damage() {
    let (mut runner, gearhulk, p0_life, p1_life, p0_hand) = setup();

    runner
        .cast(gearhulk)
        .target_player(P1)
        .decline_optional()
        .resolve();

    assert_eq!(graveyard_count(&runner), 3);
    assert_eq!(runner.life(P1), p1_life - 8);
    assert_eq!(runner.life(P0), p0_life);
    // Casting Gearhulk consumes the one card that was initially in P0's hand.
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand - 1
    );
}

#[test]
fn opponent_accept_draws_three_and_skips_mill_and_damage() {
    let (mut runner, gearhulk, p0_life, p1_life, p0_hand) = setup();

    runner
        .cast(gearhulk)
        .target_player(P1)
        .accept_optional()
        .resolve();

    assert_eq!(graveyard_count(&runner), 0);
    assert_eq!(runner.life(P1), p1_life);
    assert_eq!(runner.life(P0), p0_life);
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        // Gearhulk leaves hand, then the accepted option draws three cards.
        p0_hand + 2
    );
}
