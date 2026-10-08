//! Regression for issue #5920 — Anya, Merciless Angel: "Anya gets +3/+3 for each
//! opponent whose life total is less than half their starting life total."
//!
//! Pre-fix, the "for each opponent whose life total …" dynamic count did not
//! parse (no life-total arm in `parse_for_each_opponent_player_attribute_clause`),
//! so the static line fell through to a FIXED +3/+3 and Anya was buffed
//! unconditionally — the reporter saw +3/+3 while every opponent was well above
//! half their starting life. The fix routes the clause to
//! `PlayerCount { PlayerFilter::PlayerAttribute { LifeTotal LT half-starting } }`,
//! whose runtime is already exercised by shipping cards (Bandit's Talent etc.).
//!
//! Drives the real Oracle parse → static synthesis → layer pipeline and asserts
//! Anya's derived power/toughness scales with the number of qualifying opponents.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::PlayerId;

const ANYA: &str = "Flying\nAnya gets +3/+3 for each opponent whose life total is less than half their starting life total.\nAs long as an opponent's life total is less than half their starting life total, Anya has indestructible.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let object = &runner.state().objects[&id];
    (
        object.power.expect("creature has power"),
        object.toughness.expect("creature has toughness"),
    )
}

fn effective_has_keyword(runner: &mut GameRunner, id: ObjectId, keyword: Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], &keyword)
}

fn set_life(runner: &mut GameRunner, player: PlayerId, life: i32) {
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists")
        .life = life;
}

#[test]
fn anya_gets_plus3_per_opponent_below_half_starting_life() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Base 4/4 Anya carrying her real Oracle text (parsed → static synthesis).
    let anya = scenario
        .add_creature_from_oracle(P0, "Anya", 4, 4, ANYA)
        .id();
    let mut runner = scenario.build();

    let p2 = PlayerId(2);
    // Initial life == starting life; half is rounded down (CR: DivideRounded Down).
    let start = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;
    let half = start / 2;

    // No opponent below half → count 0 → base 4/4 (the reporter's bug case).
    set_life(&mut runner, P1, start);
    set_life(&mut runner, p2, start);
    assert_eq!(
        effective_pt(&mut runner, anya),
        (4, 4),
        "Anya must NOT be buffed while every opponent is at/above half their starting life"
    );

    // One opponent below half → count 1 → +3/+3 → 7/7.
    set_life(&mut runner, P1, half - 1);
    assert_eq!(
        effective_pt(&mut runner, anya),
        (7, 7),
        "one qualifying opponent → +3/+3"
    );

    // Two opponents below half → count 2 → +6/+6 → 10/10.
    set_life(&mut runner, p2, half - 5);
    assert_eq!(
        effective_pt(&mut runner, anya),
        (10, 10),
        "two qualifying opponents → +6/+6"
    );

    // Boundary: an opponent at exactly half is NOT below it (strict LT, not LE).
    set_life(&mut runner, P1, half);
    set_life(&mut runner, p2, start);
    assert_eq!(
        effective_pt(&mut runner, anya),
        (4, 4),
        "life exactly at half their starting life must not count (less than, not <=)"
    );
}

#[test]
fn anya_uses_each_opponents_own_starting_life_for_both_clauses() {
    // P0 is a hero (20 starting life); P1 is the Archenemy (40). P1 at 15
    // qualifies against their own half-baseline (20), but not against P0's
    // half-baseline (10). P2 is P0's teammate and is not an opponent.
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P1);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let anya = scenario
        .add_creature_from_oracle(P0, "Anya", 4, 4, ANYA)
        .id();
    let mut runner = scenario.build();

    set_life(&mut runner, P1, 15);
    assert_eq!(effective_pt(&mut runner, anya), (7, 7));
    assert!(effective_has_keyword(
        &mut runner,
        anya,
        Keyword::Indestructible
    ));

    // Both clauses use strict less-than: the Archenemy at exactly half of 40
    // does not qualify, even though 20 is above half of the controller's 20.
    set_life(&mut runner, P1, 20);
    // A qualifying teammate must not satisfy either opponent predicate.
    set_life(&mut runner, PlayerId(2), 9);
    assert_eq!(effective_pt(&mut runner, anya), (4, 4));
    assert!(!effective_has_keyword(
        &mut runner,
        anya,
        Keyword::Indestructible
    ));
}
