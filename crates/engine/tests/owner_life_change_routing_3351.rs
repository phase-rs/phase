//! Runtime regression for #3351 — "its owner"/"<noun>'s owner" life-change
//! subject must route to the OBJECT'S OWNER, not the spell/ability controller.
//!
//! CR 108.3: the owner of a card is the player who started the game with it in
//! their deck; CR 109.4 makes controller a distinct concept. When the spell's
//! controller does NOT own the targeted permanent (e.g. it was stolen, or these
//! tests poke a controller/owner split), "its owner gains/loses N life" must pay
//! the OWNER. Cards: Misfortune's Gain / Path of Peace ("Destroy target
//! creature. Its owner gains 4 life.") and the parallel "its owner loses N life"
//! subject (Thieving Amalgam's chained loss). Before the fix both emitted a
//! Controller-defaulted player slot and paid the wrong player.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const MISFORTUNES_GAIN: &str = "Destroy target creature. Its owner gains 4 life.";
const OWNER_LOSES: &str = "Destroy target creature. Its owner loses 2 life.";

/// CR 108.3 + CR 608.2c: "Its owner gains 4 life." pays the OWNER of the
/// destroyed creature, not the casting player. P0 casts targeting a creature it
/// controls but P1 owns; P1 must gain 4 and P0 must be unchanged. Reverting the
/// owner routing makes the GainLife player default to Controller → P0 gains 4
/// and this assertion fails.
#[test]
fn its_owner_gains_life_routes_to_owner_not_controller() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Creature controlled by P0 (the caster) but OWNED by P1.
    let creature = scenario.add_creature(P0, "Borrowed Beast", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Misfortune's Gain", false, MISFORTUNES_GAIN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // Poke the controller/owner split (no builder for this; precedent:
    // ability_utils.rs parent_target_owner tests poke `.owner` directly).
    runner.state_mut().objects.get_mut(&creature).unwrap().owner = P1;

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    runner.cast(spell).target_object(creature).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1) - p1_before,
        4,
        "the OWNER (P1) of the destroyed creature gains 4 life"
    );
    assert_eq!(
        runner.life(P0) - p0_before,
        0,
        "the spell controller (P0) must NOT gain the life — owner != controller"
    );
}

/// CR 108.3 + CR 119.3: the parallel "Its owner loses 2 life." subject routes the
/// LoseLife to the OWNER. P0 casts targeting a P1-owned creature it controls; P1
/// must lose 2 and P0 must be unchanged. Reverting the owner routing makes the
/// subject resolve to ParentTargetController → P0 (the controller) loses 2.
#[test]
fn its_owner_loses_life_routes_to_owner_not_controller() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_creature(P0, "Borrowed Beast", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Owner Drain", false, OWNER_LOSES)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&creature).unwrap().owner = P1;

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    runner.cast(spell).target_object(creature).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1) - p1_before,
        -2,
        "the OWNER (P1) of the destroyed creature loses 2 life"
    );
    assert_eq!(
        runner.life(P0) - p0_before,
        0,
        "the spell controller (P0) must NOT lose the life — owner != controller"
    );
}
