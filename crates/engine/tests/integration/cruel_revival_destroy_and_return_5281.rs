//! Runtime coverage for Cruel Revival (#5281):
//!
//!   "Destroy target non-Zombie creature. It can't be regenerated. Return up to
//!    one target Zombie card from your graveyard to your hand."
//!
//! The spell declares two distinct targets — a non-Zombie creature on the
//! battlefield (destroyed) and a Zombie card in the caster's graveyard (returned
//! to hand). The reported bug (#5281) is that the non-Zombie creature was
//! *returned to hand* instead of destroyed — i.e. the chained return effect
//! resolved against the destroy target instead of its own graveyard slot.
//!
//! This asserts the correct end state: the battlefield creature is in the
//! graveyard (destroyed) and the graveyard Zombie is in hand (returned).

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const ORACLE: &str = "Destroy target non-Zombie creature. It can't be regenerated. \
     Return up to one target Zombie card from your graveyard to your hand.";
const SAME_FILTER_OPTIONAL_SLOT_ORACLE: &str =
    "Tap target creature. Put a +1/+1 counter on up to one target creature.";

#[test]
fn cruel_revival_destroys_creature_and_returns_graveyard_zombie() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Ample black mana to auto-pay {4}{B}.
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );

    // Destroy target: a non-Zombie creature on the battlefield.
    let victim = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // Return target: a Zombie card in the caster's graveyard.
    let zombie = {
        let mut b = scenario.add_creature_to_graveyard(P0, "Walking Corpse", 2, 2);
        b.with_subtypes(vec!["Zombie"]);
        b.id()
    };
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cruel Revival", true, ORACLE)
        .id();

    let mut runner = scenario.build();

    // Slot order follows written order: slot 1 = Destroy (creature), slot 2 =
    // Return (graveyard Zombie).
    let outcome = runner
        .cast(spell)
        .target_objects(&[victim, zombie])
        .resolve();

    // The battlefield creature is destroyed (→ graveyard), NOT bounced to hand.
    outcome.assert_zone(&[victim], Zone::Graveyard);
    // The graveyard Zombie is returned to its owner's hand.
    outcome.assert_zone(&[zombie], Zone::Hand);
}

#[test]
fn cruel_revival_destroys_creature_when_bounce_declined() {
    // Edge: "Return UP TO ONE target Zombie card" is optional (CR 115.6). When the
    // caster declines the bounce (targets only the destroy slot), the non-Zombie
    // creature must still be destroyed — not returned to hand.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let victim = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // A Zombie exists in the graveyard, but the caster declines to target it.
    let zombie = {
        let mut b = scenario.add_creature_to_graveyard(P0, "Walking Corpse", 2, 2);
        b.with_subtypes(vec!["Zombie"]);
        b.id()
    };
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cruel Revival", true, ORACLE)
        .id();
    let mut runner = scenario.build();

    // Only the destroy slot is targeted; the up-to-one bounce is declined.
    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();

    outcome.assert_zone(&[victim], Zone::Graveyard);
    // The Zombie was not targeted, so it stays in the graveyard.
    outcome.assert_zone(&[zombie], Zone::Graveyard);
}

#[test]
fn independent_optional_sub_target_does_not_inherit_same_filter_parent_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Same Filter Optional Slot Probe",
            true,
            SAME_FILTER_OPTIONAL_SLOT_ORACLE,
        )
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[creature]).resolve();

    outcome.assert_tapped(creature, true);
    outcome.assert_counters(creature, CounterType::Plus1Plus1, 0);
}
