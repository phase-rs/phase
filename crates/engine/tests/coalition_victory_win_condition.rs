//! Coalition Victory — the win-condition gate (CR 104.2b + CR 608.2c).
//!
//! Oracle: "You win the game if you control a land of each basic land type and a
//! creature of each color." Before the parser fix, the trailing-`if` condition was
//! DROPPED (`ability.condition == None`), so `resolve_win` fired UNCONDITIONALLY —
//! the controller won the instant the spell resolved, regardless of board state.
//!
//! The fix attaches a flat `And{[5 basic-land members + 5 color members]}` to the
//! ability. The resolution gate at `game/effects/mod.rs` (`evaluate_condition`)
//! then only invokes `resolve_win` when every member is satisfied.
//!
//! Discriminating structure (each asserts through the REAL cast+resolve pipeline,
//! `apply()` via `GameRunner::cast(..).resolve()`):
//!
//! - `met`  → controller wins (`GameOver { winner }`, opponent eliminated).
//! - `missing_one_land_type` → NO win (opponent alive, game not over). Revert guard.
//! - `missing_one_color`     → NO win (opponent alive, game not over). Revert guard.
//!
//! On pre-fix code the two "missing" cases WIN (wrong), so they fail — proving the
//! gate is live and driven by the parsed condition, not a shape assertion.

use engine::game::scenario::GameScenario;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const COALITION_VICTORY: &str =
    "You win the game if you control a land of each basic land type and a creature of each color.";

/// Distinct single-color creatures for each of the five colors, keyed by color so
/// `HasColor` (obj.color.contains) matches one member per color.
const WUBRG: [ManaColor; 5] = [
    ManaColor::White,
    ManaColor::Blue,
    ManaColor::Black,
    ManaColor::Red,
    ManaColor::Green,
];

/// A single-color mana cost (`{W}`, `{U}`, …) so the creature's derived color is
/// exactly that color.
fn one_color_cost(color: ManaColor) -> ManaCost {
    let shard = match color {
        ManaColor::White => ManaCostShard::White,
        ManaColor::Blue => ManaCostShard::Blue,
        ManaColor::Black => ManaCostShard::Black,
        ManaColor::Red => ManaCostShard::Red,
        ManaColor::Green => ManaCostShard::Green,
    };
    ManaCost::Cost {
        generic: 0,
        shards: vec![shard],
    }
}

/// Build a 2-player scenario in P0's precombat main with a zero-cost Coalition
/// Victory in hand. `land_colors` seeds one basic land per listed color (its
/// basic land type follows), `creature_colors` seeds one single-color creature per
/// listed color. Returns (scenario, spell id).
fn setup(
    land_colors: &[ManaColor],
    creature_colors: &[ManaColor],
) -> (GameScenario, engine::types::ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    for &color in land_colors {
        scenario.add_basic_land(P0, color);
    }
    for (i, &color) in creature_colors.iter().enumerate() {
        scenario
            .add_creature(P0, &format!("Color Bearer {i}"), 1, 1)
            .with_mana_cost(one_color_cost(color));
    }

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coalition Victory", false, COALITION_VICTORY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    (scenario, spell)
}

/// CR 104.2b + CR 608.2c: with all five basic land types AND all five colors
/// controlled, the win condition is MET — P0 wins and the sole opponent is
/// eliminated.
#[test]
fn coalition_victory_condition_met_controller_wins() {
    let (scenario, spell) = setup(&WUBRG, &WUBRG);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    assert_eq!(
        *outcome.final_waiting_for(),
        WaitingFor::GameOver { winner: Some(P0) },
        "condition met: P0 must win, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .map(|p| p.is_eliminated)
            .unwrap(),
        "condition met: the sole opponent must be eliminated"
    );
}

/// REVERT GUARD (the bug): missing ONE basic land type (Forest) — the condition is
/// NOT met, so `resolve_win` must NOT fire. On pre-fix code the condition was
/// dropped and P0 wrongly won here.
#[test]
fn coalition_victory_missing_one_land_type_no_win() {
    // Four of five basic land types (no Forest); all five colors present.
    let land_colors = [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
    ];
    let (scenario, spell) = setup(&land_colors, &WUBRG);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    assert_ne!(
        *outcome.final_waiting_for(),
        WaitingFor::GameOver { winner: Some(P0) },
        "missing a land type: P0 must NOT win"
    );
    assert!(
        !outcome
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .map(|p| p.is_eliminated)
            .unwrap(),
        "missing a land type: the opponent must still be alive"
    );
}

/// REVERT GUARD (the bug): all five basic land types but missing ONE color (no
/// green creature) — the condition is NOT met, so no win. Pre-fix, P0 wrongly won.
#[test]
fn coalition_victory_missing_one_color_no_win() {
    // All five basic land types; four of five colors (no green creature).
    let creature_colors = [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
    ];
    let (scenario, spell) = setup(&WUBRG, &creature_colors);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    assert_ne!(
        *outcome.final_waiting_for(),
        WaitingFor::GameOver { winner: Some(P0) },
        "missing a color: P0 must NOT win"
    );
    assert!(
        !outcome
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .map(|p| p.is_eliminated)
            .unwrap(),
        "missing a color: the opponent must still be alive"
    );
}
