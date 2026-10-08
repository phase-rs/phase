//! Hundred-Battle Veteran — CR 122.1 + CR 611.3a: "As long as there are three
//! or more different kinds of counters among creatures you control, ~ gets
//! +2/+4."
//!
//! Before the fix, the condition "there are three or more different kinds of
//! counters among creatures you control" failed to parse and fell back to
//! `StaticCondition::Unrecognized`, which `game/layers.rs` evaluates as
//! unconditionally TRUE — so Hundred-Battle Veteran was silently ALWAYS 6/6,
//! regardless of the actual counter census. These tests drive the REAL
//! parse -> synthesis -> layer pipeline via `add_real_card` (the deployed
//! card-data export) + `rehydrate` and read the EFFECTIVE post-layer P/T.
//! They FAIL on the pre-fix export (always 6/6) and PASS once
//! `parse_distinct_counter_kinds_among_tail` is reachable from the
//! bare-suffix `alt()` in `parse_quantity_ref`.

use super::support::shared_card_db;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn recompute(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.expect("creature power")
}

fn toughness(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id]
        .toughness
        .expect("creature toughness")
}

/// Harness sanity check (reach-guard), run BEFORE trusting any zero/negative
/// result below: an unconditional real anthem (Glorious Anthem: "Creatures
/// you control get +1/+1.", no condition) must visibly move Hundred-Battle
/// Veteran's power/toughness after `recompute`. This proves the round-trip
/// actually re-evaluates layers rather than the test harness silently no-op'ing
/// (the failure mode the reviewer's own probe hit on its first attempt against
/// `GameScenario::build()` alone).
#[test]
fn harness_recompute_applies_unconditional_anthem_control() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hbv = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Battlefield, db);
    // No counters anywhere -> Hundred-Battle Veteran's own conditional gate is
    // OFF (0 distinct kinds). The anthem is unconditional, so its +1/+1 must
    // still land.
    scenario.add_real_card(P0, "Glorious Anthem", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    recompute(&mut runner);

    assert_eq!(
        (power(&runner, hbv), toughness(&runner, hbv)),
        (5, 3),
        "control anthem must apply base 4/2 + anthem 1/1 = 5/3; if this fails, \
         the recompute round-trip itself is broken and every gated result below \
         is untrustworthy"
    );
}

/// THE bug this PR fixes: zero distinct counter kinds among controller's
/// creatures. Before the fix this was (wrongly) 6/6; correctly, the gate is
/// OFF and Hundred-Battle Veteran stays at its printed 4/2.
#[test]
fn hundred_battle_veteran_stays_base_with_zero_counter_kinds() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hbv = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Battlefield, db);
    // A second creature with no counters at all.
    scenario.add_creature(P0, "Bystander", 2, 2);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    recompute(&mut runner);

    assert_eq!(
        (power(&runner, hbv), toughness(&runner, hbv)),
        (4, 2),
        "0 distinct counter kinds -> gate OFF -> base 4/2 (was silently 6/6 pre-fix)"
    );
}

/// Three distinct counter kinds spread across controller's creatures -> gate
/// ON -> 6/6.
#[test]
fn hundred_battle_veteran_boosts_with_three_distinct_counter_kinds() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hbv = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Battlefield, db);
    let a = scenario.add_creature(P0, "Counter Bearer A", 2, 2).id();
    let b = scenario.add_creature(P0, "Counter Bearer B", 2, 2).id();
    let c = scenario.add_creature(P0, "Counter Bearer C", 2, 2).id();
    scenario.with_counter(a, CounterType::Plus1Plus1, 1);
    scenario.with_counter(b, CounterType::Stun, 1);
    scenario.with_counter(c, CounterType::Shield, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    recompute(&mut runner);

    assert_eq!(
        (power(&runner, hbv), toughness(&runner, hbv)),
        (6, 6),
        "3 distinct counter kinds among creatures you control -> gate ON -> 6/6"
    );
}

/// Boundary: exactly two distinct counter kinds is NOT "three or more" ->
/// gate stays OFF -> 4/2.
#[test]
fn hundred_battle_veteran_stays_base_at_two_kinds_boundary() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hbv = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Battlefield, db);
    let a = scenario.add_creature(P0, "Counter Bearer A", 2, 2).id();
    let b = scenario.add_creature(P0, "Counter Bearer B", 2, 2).id();
    scenario.with_counter(a, CounterType::Plus1Plus1, 1);
    scenario.with_counter(b, CounterType::Stun, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    recompute(&mut runner);

    assert_eq!(
        (power(&runner, hbv), toughness(&runner, hbv)),
        (4, 2),
        "exactly 2 distinct counter kinds -> below the GE-3 threshold -> stays base 4/2"
    );
}

/// CR 122.1: counters with the same name/kind are interchangeable, so the
/// same kind repeated across multiple creatures still counts once, not once
/// per instance. Three creatures all carrying +1/+1 counters is only ONE
/// distinct kind -> gate stays OFF.
#[test]
fn hundred_battle_veteran_dedups_same_kind_across_multiple_creatures() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hbv = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Battlefield, db);
    let a = scenario.add_creature(P0, "Counter Bearer A", 2, 2).id();
    let b = scenario.add_creature(P0, "Counter Bearer B", 2, 2).id();
    let c = scenario.add_creature(P0, "Counter Bearer C", 2, 2).id();
    scenario.with_counter(a, CounterType::Plus1Plus1, 1);
    scenario.with_counter(b, CounterType::Plus1Plus1, 3);
    scenario.with_counter(c, CounterType::Plus1Plus1, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    recompute(&mut runner);

    assert_eq!(
        (power(&runner, hbv), toughness(&runner, hbv)),
        (4, 2),
        "CR 122.1: the same counter kind duplicated across creatures still \
         counts once, not per-instance -> only 1 distinct kind -> stays base 4/2"
    );
}

/// Controller-scope check: three or more distinct counter kinds exist on the
/// battlefield, but ALL of them are on an opponent's creatures. "Creatures
/// you control" must not read across the table -> gate stays OFF.
#[test]
fn hundred_battle_veteran_ignores_opponent_creatures_kinds() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hbv = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Battlefield, db);
    let oa = scenario.add_creature(P1, "Opposing Bearer A", 2, 2).id();
    let ob = scenario.add_creature(P1, "Opposing Bearer B", 2, 2).id();
    let oc = scenario.add_creature(P1, "Opposing Bearer C", 2, 2).id();
    scenario.with_counter(oa, CounterType::Plus1Plus1, 1);
    scenario.with_counter(ob, CounterType::Stun, 1);
    scenario.with_counter(oc, CounterType::Shield, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    recompute(&mut runner);

    assert_eq!(
        (power(&runner, hbv), toughness(&runner, hbv)),
        (4, 2),
        "3 distinct kinds exist only on the opponent's creatures -> \
         controller-scoped population is empty -> stays base 4/2"
    );
}

/// Hundred-Battle Veteran itself is one of "creatures you control" and its
/// own counter contributes to the census. Two allies carry the other two
/// kinds, and Hundred-Battle Veteran carries a finality counter (its own
/// graveyard-cast rider, CR 122.1h) as the third distinct kind -> gate ON.
#[test]
fn hundred_battle_veteran_counts_its_own_counter_as_one_of_three() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hbv = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Battlefield, db);
    scenario.with_counter(hbv, CounterType::Finality, 1);
    let a = scenario.add_creature(P0, "Counter Bearer A", 2, 2).id();
    let b = scenario.add_creature(P0, "Counter Bearer B", 2, 2).id();
    scenario.with_counter(a, CounterType::Plus1Plus1, 1);
    scenario.with_counter(b, CounterType::Stun, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    recompute(&mut runner);

    assert_eq!(
        (power(&runner, hbv), toughness(&runner, hbv)),
        (6, 6),
        "~ is itself one of the 'creatures you control' whose counters count \
         toward the census; its own finality counter is the 3rd distinct kind \
         -> gate ON -> 6/6"
    );
}
