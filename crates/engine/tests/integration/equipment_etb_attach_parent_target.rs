use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ETB_ATTACH_ORACLE: &str =
    "When Test Harness enters, attach it to target creature you control.\nEquip {0}";

#[test]
fn equipment_etb_attach_it_binds_entering_equipment_not_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_vanilla(P0, 2, 2);
    let equipment = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Harness", 0, 0, ETB_ATTACH_ORACLE)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(equipment).target_object(creature).resolve();
    let state = outcome.state();
    let equipment_obj = state.objects.get(&equipment).expect("equipment exists");

    assert_eq!(equipment_obj.zone, Zone::Battlefield);
    assert_eq!(
        equipment_obj.attached_to,
        Some(AttachTarget::Object(creature)),
        "ETB attach must attach the entering Equipment to the chosen creature"
    );
    assert!(
        state.objects[&creature].attachments.contains(&equipment),
        "chosen creature must list the entering Equipment as attached"
    );
}
