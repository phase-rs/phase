//! CR 608.2c: "They" after a multi-slot target list names the objects of every
//! slot, not only the last one.

use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Return up to one target creature card and up to one target land card from your graveyard to the battlefield. They are 5/5 Elemental creatures in addition to their other types.";

#[test]
fn plural_back_reference_animates_every_returned_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_graveyard(P0, "Fallen Bear", 2, 2)
        .id();
    let land = scenario.add_land_to_graveyard(P0, "Buried Plains").id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Relive Sketch", false, ORACLE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_objects(&[creature, land])
        .resolve();

    outcome.assert_zone(&[creature, land], Zone::Battlefield);
    for (label, id) in [("creature slot", creature), ("land slot", land)] {
        assert_eq!(
            outcome.state().objects[&id].power,
            Some(5),
            "{label} must be a 5/5 Elemental"
        );
    }
}
