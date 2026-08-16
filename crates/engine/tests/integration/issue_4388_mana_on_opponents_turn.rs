//! Regression for GitHub issue #4388 — Gaea's Cradle / Itlimoc activatable
//! during the opponent's turn when the controller holds priority (CR 117.1d).

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const GAEAS_CRADLE_ORACLE: &str = "{T}: Add {G} for each creature you control.";
const ITLIMOC_ORACLE: &str = "{T}: Add {G} for each creature you control.";

fn cradle_has_mana_action(
    grouped: &std::collections::HashMap<engine::types::identifiers::ObjectId, Vec<GameAction>>,
    cradle: engine::types::identifiers::ObjectId,
) -> bool {
    grouped.get(&cradle).is_some_and(|actions| {
        actions.iter().any(|a| {
            matches!(
                a,
                GameAction::TapLandForMana { selection }
                    if selection.source.object_id == cradle
            ) || matches!(
                a,
                GameAction::ActivateAbility { source_id, .. } if *source_id == cradle
            )
        })
    })
}

#[test]
fn issue_4388_gaeas_cradle_offered_on_opponents_turn_with_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cradle = scenario
        .add_creature(P0, "Gaea's Cradle", 0, 0)
        .as_artifact()
        .from_oracle_text(GAEAS_CRADLE_ORACLE)
        .id();
    let _bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let (_, _, grouped) = legal_actions_full(runner.state());
    assert!(
        cradle_has_mana_action(&grouped, cradle),
        "CR 117.1d: Gaea's Cradle mana must be offered when P0 holds priority on P1's turn; grouped={grouped:?}"
    );

    runner
        .act(match grouped.get(&cradle).and_then(|a| a.first()) {
            Some(action) => action.clone(),
            None => panic!("no action for cradle"),
        })
        .expect("activate Gaea's Cradle on opponent's turn");

    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "one creature should produce one green mana"
    );
}

#[test]
fn issue_4388_auto_pass_holds_priority_for_mana_on_opponents_turn() {
    use engine::ai_support::{auto_pass_recommended, flat_priority_actions};
    use engine::types::mana::{ManaCost, ManaCostShard};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cradle = scenario
        .add_creature(P0, "Gaea's Cradle", 0, 0)
        .as_artifact()
        .from_oracle_text(GAEAS_CRADLE_ORACLE)
        .id();
    let _bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    // A castable {G} instant gives the Cradle mana somewhere to go — this is
    // what makes the priority window meaningful (CR 117.1d + CR 601.2g).
    scenario
        .add_spell_to_hand_from_oracle(P0, "Test Bolt", true, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // Reach-guard FIRST: the Cradle mana action must actually be offered, so a
    // green HOLD assertion below reflects a real opponent-turn priority window.
    let (_, _, grouped) = legal_actions_full(runner.state());
    assert!(
        cradle_has_mana_action(&grouped, cradle),
        "precondition: Cradle mana is offered; grouped={grouped:?}"
    );
    assert!(
        !auto_pass_recommended(runner.state(), &flat_priority_actions(runner.state())),
        "CR 117.1d: castable instant + Cradle mana on opponent's turn → hold (#4388)"
    );
}

#[test]
fn issue_4388_auto_pass_releases_priority_for_lone_mana_on_opponents_turn() {
    use engine::ai_support::{auto_pass_recommended, flat_priority_actions};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cradle = scenario
        .add_creature(P0, "Gaea's Cradle", 0, 0)
        .as_artifact()
        .from_oracle_text(GAEAS_CRADLE_ORACLE)
        .id();
    let _bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // Reach-guard FIRST: the Cradle mana is still genuinely activatable...
    let (_, _, grouped) = legal_actions_full(runner.state());
    assert!(
        cradle_has_mana_action(&grouped, cradle),
        "precondition: Cradle mana is offered; grouped={grouped:?}"
    );
    // ...but with an empty hand there is nothing to spend it on, so auto-pass
    // fires (#4388 narrowing — mana permission is not an obligation to stop).
    assert!(
        auto_pass_recommended(runner.state(), &flat_priority_actions(runner.state())),
        "lone Cradle with empty hand on opponent's turn → auto-pass (#4388 narrowing)"
    );
}

#[test]
fn issue_4388_itlimoc_offered_on_opponents_turn_with_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let itlimoc = scenario
        .add_creature(P0, "Itlimoc, Cradle of the Sun", 0, 0)
        .from_oracle_text(ITLIMOC_ORACLE)
        .id();
    let _elf = scenario.add_creature(P0, "Elf", 1, 1).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let (_, _, grouped) = legal_actions_full(runner.state());
    assert!(
        cradle_has_mana_action(&grouped, itlimoc),
        "CR 117.1d: Itlimoc mana must be offered when P0 holds priority on P1's turn"
    );
}
