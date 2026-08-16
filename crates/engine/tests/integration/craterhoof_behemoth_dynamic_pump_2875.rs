//! Craterhoof Behemoth — the +X/+X pump scales with the number of creatures you
//! control, not the {X} paid (which is 0 — Craterhoof has no {X} in its cost).
//!
//! Regression for issue #2875: "creatures you control gain trample and get +X/+X
//! until end of turn, where X is the number of creatures you control" bound the
//! grant's dynamic P/T to `QuantityRef::CostXPaid` (always 0), so the buff was
//! always +0/+0. The trailing "where X is …" clause must bind X to the
//! object-count (CR 107.3i + CR 613.4c). This drives the parsed pump through
//! `resolve_ability_chain` + `evaluate_layers` and asserts each creature gets
//! +N/+N where N is the number of creatures the controller has.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::phase::Phase;

const ORACLE: &str = "creatures you control gain trample and get +X/+X until end of turn, where X is the number of creatures you control";

#[test]
fn craterhoof_pump_scales_with_creatures_you_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut ids = Vec::new();
    for i in 0..3 {
        ids.push(scenario.add_creature(P0, &format!("Beast {i}"), 2, 2).id());
    }
    let opponent_creature = scenario.add_creature(P1, "Opponent Beast", 2, 2).id();
    let mut runner = scenario.build();

    // P0 controls three creatures, so X = 3. The P1 creature proves the count
    // and affected filter stay scoped to the source controller.
    let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, ids[0], P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the +X/+X pump must resolve");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    for id in &ids {
        let obj = &runner.state().objects[id];
        assert_eq!(
            obj.power,
            Some(5),
            "each creature gets +3/+3 (3 creatures you control), not +0/+0"
        );
        assert_eq!(obj.toughness, Some(5));
    }

    let obj = &runner.state().objects[&opponent_creature];
    assert_eq!(obj.power, Some(2));
    assert_eq!(obj.toughness, Some(2));
}
