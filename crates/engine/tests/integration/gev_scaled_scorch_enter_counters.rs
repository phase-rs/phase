//! Gev, Scaled Scorch — distributive "enter with … for each opponent who lost
//! life this turn" ETB-counter replacement.
//!
//! Oracle: "Ward—Pay 2 life.\nOther creatures you control enter with an
//! additional +1/+1 counter on them for each opponent who lost life this
//! turn.\nWhenever you cast a Lizard spell, Gev deals 1 damage to target
//! opponent."
//!
//! Before this change three parser seams made the replacement line fall to
//! `Effect::Unimplemented { name: "static_structure" }`:
//!   1. The bare-verb plural subject ("Other creatures you control ENTER with")
//!      was not recognized as an enters-with-counter replacement (the
//!      classifier + dispatch + replacement gate only matched singular
//!      "enters"/"escapes").
//!   2. The for-each suffix recognizer only matched "on IT for each …", not the
//!      distributive "on THEM for each …".
//!   3. The per-each clause only matched the plural "opponents who lost life
//!      this turn", not the singular "opponent who lost life this turn".
//!
//! These tests drive the real cast → stack → resolve → ETB → replacement
//! pipeline through `apply`. Only the real pipeline fires the `ChangeZone`
//! replacement, so the scaled count is exercised end to end.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1c: a replacement effect that modifies how a permanent enters.
//!   - CR 614.12: external (non-SelfRef) ETB-counter replacements route through
//!     ChangeZone so the count resolves against the replacement source's
//!     controller (Gev's controller).
//!   - CR 122.6 / 122.6a: an object that enters with counters on it.
//!   - CR 119.3: an effect that causes a player to lose life adjusts their life
//!     total; `life_lost_this_turn` is the per-turn tally read by
//!     `PlayerFilter::OpponentLostLife`.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Gev's printed Oracle text — byte-identical to the MTGJSON Oracle text.
const GEV: &str = "Ward\u{2014}Pay 2 life.\nOther creatures you control enter \
with an additional +1/+1 counter on them for each opponent who lost life this \
turn.\nWhenever you cast a Lizard spell, Gev deals 1 damage to target opponent.";

fn cast_creature_from_hand(
    runner: &mut engine::game::scenario::GameRunner,
    hand_card: ObjectId,
) -> CastOutcome {
    runner.cast(hand_card).resolve()
}

/// Mark `player` as having lost life this turn (the per-turn tally
/// `PlayerFilter::OpponentLostLife` reads). Documented escape hatch: the test
/// drives the *replacement* pipeline, not the life-loss pipeline, so seeding
/// the tally directly keeps the test focused on the scaled-count behavior.
fn set_life_lost(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, amount: u32) {
    let state = runner.state_mut();
    let p = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    p.life_lost_this_turn = amount;
}

/// Core discriminating case (two-player): exactly one opponent lost life this
/// turn, so another creature Gev's controller plays enters with exactly ONE
/// +1/+1 counter. Reverting any of the three parser seams drops the line to
/// `Unimplemented` and the entering creature gets ZERO counters — this assertion
/// flips.
#[test]
fn other_creature_enters_with_one_counter_when_one_opponent_lost_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Gev, Scaled Scorch", 2, 2, GEV)
        .with_subtypes(vec!["Lizard"]);
    let newcomer = scenario
        .add_creature_to_hand(P0, "Incoming Drake", 3, 3)
        .with_subtypes(vec!["Drake"])
        .id();

    let mut runner = scenario.build();
    // One opponent (P1) lost life this turn.
    set_life_lost(&mut runner, P1, 3);

    let outcome = cast_creature_from_hand(&mut runner, newcomer);

    outcome.assert_zone(&[newcomer], Zone::Battlefield);
    // CR 614.1c + CR 119.3: one qualifying opponent → one +1/+1 counter.
    outcome.assert_counters(newcomer, CounterType::Plus1Plus1, 1);
}

/// Negative case (two-player): NO opponent lost life this turn → the entering
/// creature gets ZERO counters. This is the same count the `Unimplemented`
/// regression would produce, so it is paired with the positive cases above and
/// below (count = 1 and count = 2) which DO flip on revert.
#[test]
fn other_creature_enters_with_no_counter_when_no_opponent_lost_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Gev, Scaled Scorch", 2, 2, GEV)
        .with_subtypes(vec!["Lizard"]);
    let newcomer = scenario
        .add_creature_to_hand(P0, "Incoming Drake", 3, 3)
        .with_subtypes(vec!["Drake"])
        .id();

    let mut runner = scenario.build();
    // No opponent lost life this turn (life_lost_this_turn defaults to 0).

    let outcome = cast_creature_from_hand(&mut runner, newcomer);

    outcome.assert_zone(&[newcomer], Zone::Battlefield);
    outcome.assert_counters(newcomer, CounterType::Plus1Plus1, 0);
}

/// Multiplayer discriminating case: TWO opponents lost life this turn → the
/// entering creature gets exactly TWO +1/+1 counters. A `Fixed { value: 1 }`
/// fallback (the pre-fix behavior of the for-each suffix when the per-each
/// clause failed) would produce 1, not 2 — so this count cannot coincide with
/// any degenerate fallback.
#[test]
fn other_creature_enters_with_two_counters_when_two_opponents_lost_life() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Gev, Scaled Scorch", 2, 2, GEV)
        .with_subtypes(vec!["Lizard"]);
    let newcomer = scenario
        .add_creature_to_hand(P0, "Incoming Drake", 3, 3)
        .with_subtypes(vec!["Drake"])
        .id();

    let mut runner = scenario.build();
    // Both opponents (P1 and P2) lost life this turn.
    set_life_lost(&mut runner, PlayerId(1), 2);
    set_life_lost(&mut runner, PlayerId(2), 5);

    let outcome = cast_creature_from_hand(&mut runner, newcomer);

    outcome.assert_zone(&[newcomer], Zone::Battlefield);
    // CR 614.1c: count = number of opponents who lost life (2), resolved against
    // Gev's controller.
    outcome.assert_counters(newcomer, CounterType::Plus1Plus1, 2);
}

/// Gev herself entering gets NO ETB counter — "Other creatures you control"
/// excludes the source (`FilterProp::Another`). Validates the distributive
/// subject parsed to a `Typed` filter carrying `Another`, not a `SelfRef`.
#[test]
fn gev_self_does_not_get_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gev = scenario
        .add_creature_to_hand_from_oracle(P0, "Gev, Scaled Scorch", 2, 2, GEV)
        .with_subtypes(vec!["Lizard"])
        .id();

    let mut runner = scenario.build();
    set_life_lost(&mut runner, P1, 3);

    let outcome = cast_creature_from_hand(&mut runner, gev);

    // "Other creatures you control" excludes Gev herself.
    outcome.assert_counters(gev, CounterType::Plus1Plus1, 0);
}

/// An opponent's creature entering does NOT receive counters — "creatures YOU
/// control" excludes opponent permanents.
#[test]
fn opponent_creature_unaffected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Gev, Scaled Scorch", 2, 2, GEV)
        .with_subtypes(vec!["Lizard"]);
    let opp_creature = scenario
        .add_creature_to_hand(P1, "Opposing Drake", 3, 3)
        .with_subtypes(vec!["Drake"])
        .id();

    let mut runner = scenario.build();
    set_life_lost(&mut runner, P1, 3);
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    }

    let outcome = cast_creature_from_hand(&mut runner, opp_creature);

    // "creatures you control" (controller: You) excludes opponent permanents.
    outcome.assert_counters(opp_creature, CounterType::Plus1Plus1, 0);
}
