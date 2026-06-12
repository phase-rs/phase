//! Regression tests for GitHub issue #2900 — Blinkmoth Urn mana routing.
//!
//! At the beginning of each player's first main phase, if Blinkmoth Urn is
//! untapped, **that player** (the active player whose main phase is beginning)
//! adds {C} for each **artifact they control**. Before the fix, mana went to the
//! Urn's controller and the artifact count used the turn player's board while
//! crediting the wrong pool.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const BLINKMOTH_URN_ORACLE: &str = "At the beginning of each player's first main phase, \
    if this artifact is untapped, that player adds {C} for each artifact they control.";

fn seed_library(scenario: &mut GameScenario, player: PlayerId) {
    scenario.with_library_top(player, &["Lib A", "Lib B", "Lib C", "Lib D"]);
}

fn artifact_vanilla(scenario: &mut GameScenario, player: PlayerId, name: &str) {
    scenario.add_creature(player, name, 0, 0).as_artifact();
}

fn runner_at_untap(runner: &mut engine::game::scenario::GameRunner, active: PlayerId) {
    let state = runner.state_mut();
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = active;
    state.priority_player = active;
    state.waiting_for = WaitingFor::Priority { player: active };
}

/// CR 106.4 + CR 603.2b: On the opponent's first main phase, the active player
/// (P1) receives colorless mana equal to their own artifact count, not the
/// Urn controller's pool sized by P1's board.
#[test]
fn blinkmoth_urn_opponent_main_phase_mana_goes_to_active_player() {
    let mut scenario = GameScenario::new();

    scenario
        .add_creature_from_oracle(P0, "Blinkmoth Urn", 0, 0, BLINKMOTH_URN_ORACLE)
        .as_artifact();

    artifact_vanilla(&mut scenario, P1, "Opponent Artifact A");
    artifact_vanilla(&mut scenario, P1, "Opponent Artifact B");
    artifact_vanilla(&mut scenario, P1, "Opponent Artifact C");

    seed_library(&mut scenario, P0);
    seed_library(&mut scenario, P1);

    let mut runner = scenario.build();
    runner_at_untap(&mut runner, P1);

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "must reach the opponent's precombat main phase"
    );
    assert_eq!(
        runner.state().players[1]
            .mana_pool
            .count_color(ManaType::Colorless),
        3,
        "P1 must receive {{C}} equal to their three artifacts"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "P0 must not receive mana on the opponent's main phase"
    );
}

/// CR 106.4 + CR 109.4: On the controller's own first main phase, mana counts
/// the controller's artifacts (including the untapped Urn itself).
#[test]
fn blinkmoth_urn_controller_main_phase_counts_own_artifacts() {
    let mut scenario = GameScenario::new();

    scenario
        .add_creature_from_oracle(P0, "Blinkmoth Urn", 0, 0, BLINKMOTH_URN_ORACLE)
        .as_artifact();

    artifact_vanilla(&mut scenario, P0, "Controller Artifact");

    seed_library(&mut scenario, P0);
    seed_library(&mut scenario, P1);

    let mut runner = scenario.build();
    runner_at_untap(&mut runner, P0);

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Colorless),
        2,
        "P0 must receive {{C}} for the Urn plus one other artifact"
    );
}

/// Sanity: passing priority alone must not advance the scenario into a main
/// phase where triggers would fire spuriously.
#[test]
fn blinkmoth_urn_no_mana_before_main_phase() {
    let mut scenario = GameScenario::new();

    scenario
        .add_creature_from_oracle(P0, "Blinkmoth Urn", 0, 0, BLINKMOTH_URN_ORACLE)
        .as_artifact();

    artifact_vanilla(&mut scenario, P0, "Controller Artifact");

    let mut runner = scenario.build();
    runner_at_untap(&mut runner, P0);
    runner.state_mut().phase = Phase::Upkeep;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let _ = runner.act(GameAction::PassPriority);

    assert_ne!(
        runner.state().phase,
        Phase::PreCombatMain,
        "precondition: still before the first main phase"
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}
