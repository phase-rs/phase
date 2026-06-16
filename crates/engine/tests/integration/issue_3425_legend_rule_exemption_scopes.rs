//! Issue #3425 — legend-rule integration guardrail (control case).

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

#[test]
fn duplicate_legendaries_without_exemption_prompt_choose_legend() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Thalia", 2, 1)
        .as_legendary()
        .id();
    scenario
        .add_creature(P0, "Thalia", 2, 1)
        .as_legendary()
        .id();

    let mut runner = scenario.build();
    let mut events = Vec::<GameEvent>::new();
    check_state_based_actions(runner.state_mut(), &mut events);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseLegend { .. }),
        "duplicate legendaries must trigger the legend-rule SBA choice"
    );
}

// Scoped exemption runtime coverage lives in `game/sba.rs` unit tests
// (`sba_legend_rule_suppressed_for_bare_tokens_scope`,
// `sba_legend_rule_suppressed_for_commanders_scope`).
