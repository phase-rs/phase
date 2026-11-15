//! Issue #5925 — Consuming Vapors / Tribute to Hunger class:
//! "Target player sacrifices a creature of their choice. You gain life equal
//! to that creature's toughness."
//!
//! Discriminating cast-pipeline coverage for the Demonstrative toughness
//! referent after an interactive (or auto) sacrifice. A stamp regression on
//! the EffectZoneChoice → continuation hand-off leaves
//! `Toughness { Demonstrative }` at 0 and the controller's life unchanged.
//!
//! Rebound is intentionally omitted: the life-gain defect is independent of
//! CR 702.88, and the existing `consuming_vapors_rebound` suite already
//! covers that keyword.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Target player sacrifices a creature of their choice. You gain life equal to that creature's toughness.";

/// CR 608.2c + CR 701.21a + CR 119.3 + CR 400.7j: two eligible creatures force
/// `EffectZoneChoice`; choosing the printed-toughness-5 creature with three
/// +1/+1 counters must gain the controller exactly 8 life (not 0, not its
/// printed toughness, and not the other creature's 2).
#[test]
fn consuming_vapors_gains_life_equal_to_chosen_sacrificed_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consuming Vapors", false, ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    // The selected creature is a printed 1/5, but its effective battlefield
    // toughness is 8. This makes the cast pipeline discriminate battlefield
    // LKI from printed/base card data after the sacrifice.
    let tough5 = scenario
        .add_creature(P1, "Five Toughness", 1, 5)
        .with_plus_counters(3)
        .id();
    let tough2 = scenario.add_creature(P1, "Two Toughness", 1, 2).id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .effect_zone(&[tough5])
        .resolve();

    outcome.assert_life_delta(P0, 8);
    outcome.assert_life_delta(P1, 0);
    outcome.assert_zone(&[tough5], Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&tough2].zone,
        Zone::Battlefield,
        "unchosen creature must remain on the battlefield"
    );
}

/// CR 608.2c + CR 701.21a: a single eligible creature auto-sacrifices without
/// `EffectZoneChoice`; the SYNC parent→child hand-off must still bind
/// Demonstrative toughness for the GainLife sibling.
#[test]
fn consuming_vapors_auto_sacrifice_gains_life_equal_to_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consuming Vapors", false, ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let creature = scenario.add_creature(P1, "Lone Creature", 2, 4).id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();

    outcome.assert_life_delta(P0, 4);
    outcome.assert_life_delta(P1, 0);
    outcome.assert_zone(&[creature], Zone::Graveyard);
}
