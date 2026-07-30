//! Cast-pipeline regression for **Vincent's Limit Break** (FIN, {1}{B} instant).
//!
//! Oracle:
//!   Tiered (Choose one additional cost.)
//!   Until end of turn, target creature you control gains "When this creature
//!   dies, return it to the battlefield tapped under its owner's control" and has
//!   the chosen base power and toughness.
//!   • Galian Beast — {0} — 3/2.
//!   • Death Gigas — {1} — 5/2.
//!   • Hellmasker — {3} — 7/2.
//!
//! Before the fix the shared effect line sat between the `Tiered` keyword and the
//! parameter-only bullets, so `collect_mode_asts(lines, start+1)` broke on the
//! effect line, the modal never formed, and the three bullets fell through to
//! standalone `Effect::Unimplemented`. Separately the main effect parsed but
//! SILENTLY DROPPED "the chosen base power and toughness" (no `SetPower`/
//! `SetToughness`), because the anaphor carries no literal N/M.
//!
//! `parse_tiered_shared_effect_block` distributes the shared effect per mode with
//! each tier's literal base P/T substituted for the anaphor, so every mode lowers
//! to the full grant (dies-return trigger + set base P/T at layer 7b).
//!
//! CR 702.183a: Tiered — choose exactly one mode; its cost is an additional cost.
//! CR 613.4b: layer 7b sets the chosen base power/toughness.
//! CR 601.2f: the additional cost is part of the spell's total cost.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const VINCENTS_ORACLE: &str = "Tiered (Choose one additional cost.)\n\
    Until end of turn, target creature you control gains \"When this creature dies, return it to the battlefield tapped under its owner's control\" and has the chosen base power and toughness.\n\
    \u{2022} Galian Beast \u{2014} {0} \u{2014} 3/2.\n\
    \u{2022} Death Gigas \u{2014} {1} \u{2014} 5/2.\n\
    \u{2022} Hellmasker \u{2014} {3} \u{2014} 7/2.";

/// {1}{B} — Vincent's Limit Break's printed mana cost.
fn base_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 1,
    }
}

/// `n` units of black mana (black pays the {B} pip and every generic pip too),
/// so seeding exactly the total cost lets the post-resolve pool total read as the
/// change left over after payment.
fn black_pool(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| {
            ManaUnit::new(
                ManaType::Black,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            )
        })
        .collect()
}

/// B1 + B2: Galian ({0}) sets the target's base P/T to 3/2 at layer 7b — the
/// silent-drop fix. Revert-red: pre-fix no `SetPower` is emitted, so the vanilla
/// keeps its printed 2/2. The wrong-tier hostile guard rejects 5/x and 7/x.
#[test]
fn galian_sets_base_power_toughness_three_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Vincent's Limit Break", true, VINCENTS_ORACLE)
        .with_mana_cost(base_cost())
        .id();
    scenario.with_mana_pool(P0, black_pool(4));

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[0])
        .target_object(creature)
        .resolve();

    assert_eq!(
        outcome.power_toughness(creature),
        (3, 2),
        "Galian must set base power/toughness to 3/2 (CR 613.4b)"
    );
    let (p, _) = outcome.power_toughness(creature);
    assert!(p != 5 && p != 7, "must not pick up another tier's power");
}

/// B3: Hellmasker ({3}) sets 7/2 AND charges the additional {3}. The spell's
/// total cost is {1}{B} + {3} = {4}{B} = 5 mana. Seeding exactly that and
/// asserting the pool empties is the free-execution gate: if the additional cost
/// were skipped only {1}{B} (2 mana) would be spent, leaving 3 in the pool.
#[test]
fn hellmasker_sets_seven_two_and_charges_additional_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Vincent's Limit Break", true, VINCENTS_ORACLE)
        .with_mana_cost(base_cost())
        .id();
    // Exactly {4}{B} worth = base {1}{B} + Hellmasker additional {3}.
    scenario.with_mana_pool(P0, black_pool(5));

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[2])
        .target_object(creature)
        .resolve();

    assert_eq!(
        outcome.power_toughness(creature),
        (7, 2),
        "Hellmasker must set base power/toughness to 7/2"
    );
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the full {{4}}{{B}} (base {{1}}{{B}} + additional {{3}}) must be paid — \
         leftover mana means the Tiered additional cost was skipped"
    );
}

/// B5: layer ordering — SetPower/SetToughness (7b, absolute) applies before the
/// +1/+1 counter (7c, relative). A vanilla 6/6 carrying a +1/+1 counter becomes
/// 4/3 after Galian: 7b overrides the printed 6/6 to base 3/2, then 7c adds
/// +1/+1. If SetPower were dropped the result would be 7/7; if 7c wrongly
/// preceded 7b the counter would be overwritten. (CR 613.4b before CR 613.4c.)
#[test]
fn set_base_pt_applies_before_counter_layer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(P0, 6, 6);
    scenario.with_counter(creature, CounterType::Plus1Plus1, 1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Vincent's Limit Break", true, VINCENTS_ORACLE)
        .with_mana_cost(base_cost())
        .id();
    scenario.with_mana_pool(P0, black_pool(4));

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[0])
        .target_object(creature)
        .resolve();

    assert_eq!(
        outcome.power_toughness(creature),
        (4, 3),
        "base set 3/2 (7b) then +1/+1 counter (7c) = 4/3"
    );
}

/// B7: castability gate — with only {1}{B} available, choosing Hellmasker (whose
/// {3} additional cost makes the total {4}{B}) is unaffordable and the cast is
/// rejected. Proves the Tiered additional cost is a real cost, not free.
#[test]
fn hellmasker_unaffordable_with_only_base_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Vincent's Limit Break", true, VINCENTS_ORACLE)
        .with_mana_cost(base_cost())
        .id();
    // Only {1}{B} = 2 mana — enough for the base cost, NOT the {3} additional.
    scenario.with_mana_pool(P0, black_pool(2));

    let mut runner = scenario.build();
    let result = runner
        .cast(spell)
        .modes(&[2])
        .target_object(creature)
        .try_resolve();

    assert!(
        result.is_err(),
        "Hellmasker must be unaffordable/illegal with only the base {{1}}{{B}} in pool"
    );
}
