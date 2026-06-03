//! Armored Kincaller — `Or`-wrapped performed-gate on an optional ETB trigger.
//!
//! Oracle: "When this creature enters, you may reveal a Dinosaur card from your
//! hand. If you do or if you control another Dinosaur, you gain 3 life."
//!
//! The ETB trigger is `optional` ("you may reveal"); its `GainLife` sub-ability
//! carries the composite condition `Or { [IfYouDo, QuantityCheck(control
//! another Dinosaur)] }`. When the controller DECLINES the optional reveal but
//! controls another Dinosaur, the `Or`'s second disjunct is satisfied, so they
//! must still gain 3 life.
//!
//! Before the fix, `should_resolve_subability_on_optional_decline` classified
//! every `Or`/`And` condition as "not a decline branch" — so on decline the
//! `GainLife` sub-ability was dropped entirely and the player never gained
//! life, even though `QuantityCheck` was true.
//!
//! This drives the real cast -> stack -> ETB trigger -> `OptionalEffectChoice`
//! -> decline pipeline through `resolve()` (the resolution driver declines
//! optional "you may" effects by default — CR 609.3).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.3: once an ability triggers, its controller puts it on the stack.
//!   - CR 608.2c: "the controller follows the ability's instructions in the
//!     order written"; a condition that gates an instruction is evaluated as
//!     the ability resolves.
//!   - CR 608.2d: an effect offering a choice (here the optional "you may
//!     reveal") has the player announce it while applying the effect.

use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;

/// Armored Kincaller's printed Oracle text — byte-identical to
/// `client/public/card-data.json`.
const ARMORED_KINCALLER: &str = "When this creature enters, you may reveal a \
Dinosaur card from your hand. If you do or if you control another Dinosaur, \
you gain 3 life.";

/// Core fix: decline the optional reveal, but control another Dinosaur — the
/// `Or` condition's `QuantityCheck` disjunct is true, so the controller gains
/// 3 life.
#[test]
fn declined_reveal_still_gains_life_when_controlling_another_dinosaur() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    // Another Dinosaur already on the battlefield satisfies the `QuantityCheck`
    // disjunct of the `Or` condition.
    scenario
        .add_creature(P0, "Ranging Raptors", 2, 4)
        .with_subtypes(vec!["Dinosaur"]);

    let kincaller = scenario
        .add_creature_to_hand_from_oracle(P0, "Armored Kincaller", 1, 5, ARMORED_KINCALLER)
        .with_subtypes(vec!["Dinosaur"])
        .id();

    let mut runner = scenario.build();

    // Cast the 0-cost creature; the ETB trigger's `RevealHand` head targets the
    // controller ("your hand") — declare P0 for that slot — and the optional
    // "you may reveal" prompt is DECLINED by the resolution driver's default
    // policy (CR 609.3).
    let outcome = runner.cast(kincaller).target_player(P0).resolve();

    // Declining the reveal must still gain 3 life — the `Or` condition's
    // "control another Dinosaur" disjunct is satisfied.
    outcome.assert_life_delta(P0, 3);
}

/// Control check: decline the optional reveal with NO other Dinosaur on the
/// battlefield — both `Or` disjuncts are false, so no life is gained.
#[test]
fn declined_reveal_no_dinosaur_gains_no_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    let kincaller = scenario
        .add_creature_to_hand_from_oracle(P0, "Armored Kincaller", 1, 5, ARMORED_KINCALLER)
        .with_subtypes(vec!["Dinosaur"])
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(kincaller).target_player(P0).resolve();

    // Declining the reveal with no other Dinosaur gains no life — both `Or`
    // disjuncts are false.
    outcome.assert_life_delta(P0, 0);
}
