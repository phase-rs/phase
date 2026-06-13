//! Regression for GitHub issue #2919 — The Tarrasque's "has haste and ward {10}
//! as long as it was cast" static must gate on cast provenance, not apply
//! unconditionally via `StaticCondition::Unrecognized`.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::StaticCondition;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const TARRASQUE_ORACLE: &str = "The Tarrasque has haste and ward {10} as long as it was cast.\n\
Whenever The Tarrasque attacks, it fights target creature defending player controls.";

#[test]
fn tarrasque_was_cast_static_condition_parses() {
    let parsed = parse_oracle_text(
        TARRASQUE_ORACLE,
        "The Tarrasque",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let continuous = parsed
        .statics
        .iter()
        .find(|d| matches!(d.mode, StaticMode::Continuous))
        .expect("Tarrasque must produce a Continuous static");
    assert!(
        matches!(
            continuous.condition,
            Some(StaticCondition::WasCast { zone: None })
        ),
        "expected WasCast condition, got {:?}",
        continuous.condition
    );
}

#[test]
fn tarrasque_haste_only_when_cast_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cast_id = scenario
        .add_creature_to_hand_from_oracle(P0, "The Tarrasque", 10, 10, TARRASQUE_ORACLE)
        .id();

    let mut cast_runner = scenario.build();
    let outcome = cast_runner.cast(cast_id).resolve();
    outcome.assert_zone(&[cast_id], Zone::Battlefield);
    assert_eq!(
        cast_runner.state().objects[&cast_id].cast_from_zone,
        Some(Zone::Hand),
        "precondition: the real cast pipeline must stamp cast provenance"
    );

    assert!(
        has_keyword(&cast_runner.state().objects[&cast_id], &Keyword::Haste),
        "cast Tarrasque must have haste while WasCast condition is true"
    );

    let mut put_scenario = GameScenario::new();
    put_scenario.at_phase(Phase::PreCombatMain);
    let put_id = put_scenario
        .add_creature_from_oracle(P0, "The Tarrasque", 10, 10, TARRASQUE_ORACLE)
        .id();

    let mut put_runner = put_scenario.build();
    put_runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(put_runner.state_mut());

    assert!(
        !has_keyword(&put_runner.state().objects[&put_id], &Keyword::Haste),
        "put Tarrasque must not have haste without cast_from_zone"
    );
}
