//! Indestructible keyword — "destroy" effects and lethal damage cannot remove
//! a creature with indestructible (CR 702.12b).
//!
//! Three complementary regressions in a single file:
//!
//!  1. "Destroy target creature" targeting an indestructible creature must leave
//!     the creature on the battlefield.
//!
//!  2. "Destroy all creatures" (Wrath) destroys normally-killable creatures and
//!     leaves indestructible ones on the battlefield.
//!
//!  3. A spell that deals lethal damage to an indestructible creature must leave
//!     the creature on the battlefield — state-based actions do not destroy a
//!     creature with indestructible even when damage ≥ toughness.
//!
//! CR 702.12a: "Indestructible is a static ability."
//! CR 702.12b: "A permanent with indestructible can't be destroyed. Such
//!   permanents are not destroyed by lethal damage, and the 'destroy' keyword
//!   doesn't affect them."
//! CR 701.8a: "To destroy a permanent, move it from the battlefield to its
//!   owner's graveyard."

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MURDER: &str = "Destroy target creature.";
const WRATH: &str = "Destroy all creatures.";

/// CR 702.12b: a targeted "destroy" effect cast by the controller must leave
/// their own indestructible creature on the battlefield.
///
/// Discriminating: without indestructible, the creature moves to the graveyard
/// after "Destroy target creature" resolves, failing the Zone::Battlefield assert.
#[test]
fn targeted_destroy_does_not_remove_indestructible_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 has an indestructible 3/3 on the battlefield.
    let target = scenario
        .add_creature(P0, "Indestructible Golem", 3, 3)
        .indestructible()
        .id();

    // P0 casts "Destroy target creature" targeting their own indestructible
    // creature. Controllers are free to target their own permanents.
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    runner.cast(murder).target_object(target).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "an indestructible creature targeted by 'Destroy target creature' must remain \
         on the battlefield — indestructible prevents the destroy event (CR 702.12b)"
    );
}

/// CR 702.12b: "Destroy all creatures" must destroy normally-killable creatures
/// while leaving indestructible creatures on the battlefield.
///
/// Discriminating: reverting indestructible handling causes the Wrath to also
/// move the indestructible creature to the graveyard.
#[test]
fn wrath_destroys_normal_creatures_but_not_indestructible() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls one normal creature and one indestructible creature.
    let normal_creature = scenario.add_creature(P0, "Fragile Bear", 2, 2).id();
    let indestructible_creature = scenario
        .add_creature(P0, "Adamant Titan", 3, 3)
        .indestructible()
        .id();

    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    runner.cast(wrath).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&normal_creature].zone,
        Zone::Graveyard,
        "a normal creature must be moved to the graveyard by 'Destroy all creatures'"
    );
    assert_eq!(
        runner.state().objects[&indestructible_creature].zone,
        Zone::Battlefield,
        "an indestructible creature must survive 'Destroy all creatures' and \
         remain on the battlefield (CR 702.12b)"
    );
}

/// CR 702.12b + CR 704.5g: a creature with indestructible that receives lethal damage from
/// a spell remains on the battlefield — state-based actions that check for
/// lethal damage (CR 704.5g) skip indestructible permanents.
///
/// Discriminating: reverting state-based-action immunity causes the 5-point
/// burn spell (lethal for a 1/1) to trigger destruction of the creature,
/// moving it to the graveyard and failing the Zone::Battlefield assertion.
#[test]
fn lethal_spell_damage_does_not_destroy_indestructible_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A 1/1 with indestructible receives 5 damage — well above its 1 toughness.
    let target = scenario
        .add_creature(P0, "Indestructible 1/1", 1, 1)
        .indestructible()
        .id();

    // Burn spell that deals 5 damage directly to the creature.
    let burn = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Arc Lightning",
            true,
            "Arc Lightning deals 5 damage to target creature.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    runner.cast(burn).target_object(target).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "an indestructible 1/1 that receives 5 lethal damage from a spell must remain \
         on the battlefield — state-based actions skip indestructible creatures (CR 702.12b)"
    );
}
