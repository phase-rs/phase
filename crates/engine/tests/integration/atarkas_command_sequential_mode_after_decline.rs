//! Gap-4 regression: a later chosen modal mode must resolve even when an
//! EARLIER chosen mode is an optional ("you may") effect that is DECLINED.
//!
//! Atarka's Command "Choose two —" with mode 3 ("You may put a land card from
//! your hand onto the battlefield") and mode 4 ("Creatures you control get
//! +1/+1 and gain reach until end of turn"). Declining mode 3 must NOT skip
//! mode 4 — chained modes are independent instructions (CR 700.2d), so the
//! appended mode root is tagged `SubAbilityLink::SequentialSibling` and the
//! optional-decline handler resolves it regardless of the decline.
//!
//! Before the modal-chaining fix, the appended mode was a `ContinuationStep`
//! (the default), so a declined optional parent would swallow the following
//! mode and the pump would never apply.
//!
//! CR 700.2d + CR 608.2c + CR 609.3.

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const ATARKAS_COMMAND: &str = "Choose two —\n\
    • Your opponents can't gain life this turn.\n\
    • Atarka's Command deals 3 damage to each opponent.\n\
    • You may put a land card from your hand onto the battlefield.\n\
    • Creatures you control get +1/+1 and gain reach until end of turn.";

#[test]
fn atarkas_command_pump_mode_resolves_after_declined_optional_land_mode() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's creature that mode 4 must pump (+1/+1).
    let creature = scenario.add_creature(P0, "Wild Bear", 2, 2).id();
    // A land in hand so mode 3's optional put has a legal action to decline.
    scenario.add_land_to_hand(P0, "Forest");

    let command = scenario
        .add_spell_to_hand_from_oracle(P0, "Atarka's Command", true, ATARKAS_COMMAND)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();

    // Choose modes 3 (optional land — 0-indexed 2) and 4 (pump — 0-indexed 3).
    // The default ResolutionPolicy declines the optional land put.
    let outcome = runner.cast(command).modes(&[2, 3]).resolve();

    // DISCRIMINATOR: mode 4 STILL resolves — the creature is pumped to 3/3 —
    // even though mode 3 (the optional land put) was declined.
    assert_eq!(
        outcome.power_toughness(creature),
        (3, 3),
        "mode 4 pump (+1/+1) must apply after the declined optional mode 3"
    );
}
