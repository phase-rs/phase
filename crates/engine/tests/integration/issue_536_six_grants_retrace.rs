//! Issue #536: Six grants retrace to nonland permanent cards in your graveyard.
//!
//! The parser must emit the off-zone keyword grant as a continuous AddKeyword
//! static so legal action generation sees the granted Retrace keyword on
//! matching graveyard cards.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;

const SIX_ORACLE: &str =
    "During your turn, nonland permanent cards in your graveyard have retrace.";

#[test]
fn six_grants_retrace_to_graveyard_permanents_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Six", 2, 4, SIX_ORACLE);
    let graveyard_creature = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .id();
    let graveyard_sorcery = scenario
        .add_spell_to_graveyard(P0, "Graveyard Sorcery", false)
        .id();
    scenario.add_land_to_hand(P0, "Forest");
    let runner = scenario.build();

    let actions = legal_actions(runner.state());
    assert!(
        actions.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == graveyard_creature
        )),
        "Six must grant retrace so nonland permanent cards in your graveyard are castable"
    );
    assert!(
        !actions.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == graveyard_sorcery
        )),
        "Six must not grant retrace to non-permanent cards in your graveyard"
    );
}
