//! Integration coverage for issue #885 — Urza's Tower / Mine / Power-Plant.
//!
//! Oracle (activated mana ability on each):
//!   `{T}: Add {C}. If you control an Urza's <other> and an Urza's <other>,`
//!   `add {C}{C}{C} instead.`
//!
//! A unit test in `mana_abilities.rs` already proves the resolver produces
//! three colorless mana from a handcrafted `Effect::Mana` + `sub_ability` AST,
//! but nothing exercises the full pipeline (parser emit → real card load →
//! runtime `ActivateAbility` → mana production). This test closes that gap
//! using the parsed `client/public/card-data.json` so any drift in the
//! parser shape that the resolver depends on shows up here as a runtime
//! divergence, not just a unit-test failure.
//!
//! CR 605.3b: An activated mana ability doesn't go on the stack — it
//! resolves immediately after it is activated, so the assertion looks at
//! the active player's mana pool directly after the `ActivateAbility` call.
//! CR 614.1a: "Add {C}. If you control … add {C}{C}{C} instead." — the
//! word "instead" makes the sub-ability a replacement effect; its condition
//! is evaluated as the ability resolves (CR 608), and with all three Urza
//! lands controlled the `And` condition is satisfied and the delta
//! (+2 colorless) replaces the base production net (1 + 2 = 3 C).
//! CR 205.3i: "Mine," "Power-Plant," and "Tower" are distinct land subtypes
//! from the enumerated land type list; the cross-naming of the parsed
//! `ControllerControlsMatching` filters is what makes the three lands
//! reference each other rather than themselves.

use engine::game::max_x_value;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// With all three Urza lands on the battlefield, tapping Urza's Tower for mana
/// must produce three colorless (the `Add {C}` base plus the +2 delta granted
/// by the satisfied `If you control an Urza's Mine and an Urza's Power-Plant`
/// sub-ability). This is the load-bearing end-to-end check for issue #885.
#[test]
fn urzas_tower_with_mine_and_power_plant_produces_three_colorless() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tower_id = scenario.add_real_card(P0, "Urza's Tower", Zone::Battlefield, db);
    let _mine_id = scenario.add_real_card(P0, "Urza's Mine", Zone::Battlefield, db);
    let _plant_id = scenario.add_real_card(P0, "Urza's Power Plant", Zone::Battlefield, db);

    let mut runner = scenario.build();

    // CR 605.3b: a mana ability resolves immediately on activation (no stack),
    // so the outcome reads the resulting pool directly.
    let outcome = runner.activate(tower_id, 0).resolve();

    assert_eq!(
        outcome.mana_pool_color(P0, ManaType::Colorless),
        3,
        "Urza's Tower with Urza's Mine + Urza's Power Plant must produce 3 colorless \
         (1 base + 2 delta from satisfied sub-ability)",
    );
    assert_eq!(
        outcome.mana_pool_total(P0),
        3,
        "no other mana types must be produced",
    );
}

/// CR 107.1b + CR 601.2f + CR 605.3b: the X cap must see the same Tron output
/// that tapping produces. With Tower (3) + Mine (2) + Power Plant (2) the caster
/// has 7 mana, so `{X}{X}` (Walking Ballista) can be announced up to X = 3.
/// Before the fix the capacity preview read only each land's base `Add {C}`
/// (1 + 1 + 1 = 3) and capped X at 1 — the engine then refused the larger
/// announcement the caster could actually pay (field report 2026-09-15).
#[test]
fn urza_lands_x_cap_counts_full_tron_output() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Urza's Tower", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Urza's Mine", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Urza's Power Plant", Zone::Battlefield, db);
    let runner = scenario.build();

    let x_x = ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::X],
        generic: 0,
    };
    assert_eq!(
        max_x_value(runner.state(), P0, &x_x, None),
        3,
        "{{X}}{{X}} with a full Tron (3 + 2 + 2 = 7 mana) must allow X = 3",
    );
}

/// CR 605.3b + CR 614.1a: auto-pay must plan with the same Tron output. A {4}
/// spell is paid by two Urza lands (Tower + one other, or Mine + Power Plant);
/// before the fix the planner credited each land with its base `Add {C}` and
/// tapped all three, stranding the surplus in the pool (field report
/// 2026-09-15).
#[test]
fn auto_pay_with_full_tron_taps_only_what_generic_four_needs() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands = [
        scenario.add_real_card(P0, "Urza's Tower", Zone::Battlefield, db),
        scenario.add_real_card(P0, "Urza's Mine", Zone::Battlefield, db),
        scenario.add_real_card(P0, "Urza's Power Plant", Zone::Battlefield, db),
    ];
    let spell = scenario
        .add_spell_to_hand(P0, "Generic Four Sorcery", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast a {4} sorcery with auto-pay");

    let tapped = lands
        .iter()
        .filter(|id| runner.state().objects[id].tapped)
        .count();
    assert_eq!(
        tapped, 2,
        "{{4}} with a full Tron must tap exactly two Urza lands, not all three",
    );
    assert!(
        runner.state().players[0].mana_pool.total() <= 1,
        "at most one colorless may float after paying {{4}} with Tron",
    );
}
