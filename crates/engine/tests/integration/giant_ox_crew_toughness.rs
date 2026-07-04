//! Giant Ox — "This creature crews Vehicles using its toughness rather than its power."
//!
//! Coverage regression: parameterized `CrewContribution` statics are supported
//! via runtime enforcement in `object_crew_power_contribution`.
//!
//! Runtime regression: a parsed 0/4 Ox must contribute toughness 4 toward crew.

use engine::game::scenario::{GameScenario, P0};
use engine::game::static_abilities::object_crew_power_contribution;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::CardId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::{CrewAction, StaticMode};
use engine::types::zones::Zone;

const GIANT_OX_ORACLE: &str =
    "This creature crews Vehicles using its toughness rather than its power.";

#[test]
fn giant_ox_parses_crew_contribution_static() {
    let parsed = parse_oracle_text(
        GIANT_OX_ORACLE,
        "Giant Ox",
        &[],
        &["Creature".to_string()],
        &["Ox".to_string()],
    );
    assert_eq!(parsed.statics.len(), 1);
    assert_eq!(
        parsed.statics[0].mode,
        StaticMode::CrewContribution {
            kind: engine::types::statics::CrewContributionKind::ToughnessInsteadOfPower,
            actions: vec![CrewAction::Crew],
        }
    );
}

#[test]
fn giant_ox_parsed_oracle_contributes_toughness_for_crew() {
    let mut scenario = GameScenario::new();
    let ox = scenario
        .add_creature_from_oracle(P0, "Giant Ox", 0, 4, GIANT_OX_ORACLE)
        .id();
    let runner = scenario.build();
    assert_eq!(
        object_crew_power_contribution(runner.state(), ox, CrewAction::Crew),
        4,
        "parsed Giant Ox static must substitute toughness for crew contribution"
    );
}

#[test]
fn giant_ox_crews_vehicle_using_toughness_not_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ox = scenario
        .add_creature_from_oracle(P0, "Giant Ox", 0, 4, GIANT_OX_ORACLE)
        .id();
    let mut runner = scenario.build();
    let next_id = runner.state().next_object_id;
    let vehicle = create_object(
        runner.state_mut(),
        CardId(next_id),
        P0,
        "Test Vehicle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&vehicle).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Vehicle".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.keywords.push(Keyword::Crew {
            power: 3,
            once_per_turn: None,
        });
        obj.power = Some(6);
        obj.toughness = Some(5);
        obj.base_power = Some(6);
        obj.base_toughness = Some(5);
        obj.summoning_sick = false;
    }

    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .expect("enter crew mode");
    let result = runner.act(GameAction::CrewVehicle {
        vehicle_id: vehicle,
        creature_ids: vec![ox],
    });
    assert!(
        result.is_ok(),
        "0/4 Ox must crew a Crew-3 Vehicle via toughness contribution: {result:?}"
    );
}
