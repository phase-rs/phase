//! Teferi, Who Slows the Sunset — the [+1] has three independent optional
//! target slots and applies the two tap-state instructions to their union.
//!
//! CR 115.1c + CR 115.6: each printed "up to one target" is an independent
//! target occurrence that may be declined. CR 608.2c: "the chosen permanents"
//! names the complete set announced by that instruction. CR 701.26a-b: tap and
//! untap are status changes; CR 119.3: the ability still gains 2 life.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn plus_one_targets_artifact_creature_and_land_and_applies_to_all_three() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    let teferi = scenario.add_real_card(P0, "Teferi, Who Slows the Sunset", Zone::Battlefield, db);
    let own_artifact = scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db);
    let own_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let opponent_land = scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&own_artifact)
        .unwrap()
        .tapped = true;
    runner
        .state_mut()
        .objects
        .get_mut(&own_creature)
        .unwrap()
        .tapped = true;

    let ability_index = runner.state().objects[&teferi]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.cost,
                Some(engine::types::ability::AbilityCost::Loyalty { amount: 1 })
            )
        })
        .expect("Teferi's [+1] loyalty ability must be present");

    let outcome = runner
        .activate(teferi, ability_index)
        .target_objects(&[own_artifact, own_creature, opponent_land])
        .resolve();

    assert!(!outcome.state().objects[&own_artifact].tapped);
    assert!(!outcome.state().objects[&own_creature].tapped);
    assert!(outcome.state().objects[&opponent_land].tapped);
    assert_eq!(outcome.life_delta(P0), 2);
    assert!(!matches!(
        outcome.final_waiting_for(),
        WaitingFor::TargetSelection { .. }
    ));
}
