//! P2 replay coverage for resolved mana insert and spend commands.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{ResolvedManaReplayInvariantError, ResolvedRulesCommand};

const DIMIR_SIGNET_ORACLE: &str = "{1}, {T}: Add {U}{B}.";

fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.card_types.core_types = vec![CoreType::Artifact];
    object.base_card_types = object.card_types.clone();
    object.power = None;
    object.toughness = None;
    object.base_power = None;
    object.base_toughness = None;
}

fn activated_signet_states() -> (GameState, GameState) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    let signet = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, signet);
    let pre_state = runner.state().clone();
    runner
        .act(GameAction::ActivateAbility {
            source_id: signet,
            ability_index: 0,
        })
        .expect("the real Signet mana ability must activate");
    (pre_state, runner.state().clone())
}

fn semantic_mana_commands(state: &GameState) -> Vec<ResolvedRulesCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.clone())
        .collect()
}

fn apply_mana_command(state: &mut GameState, command: &ResolvedRulesCommand) {
    match command {
        ResolvedRulesCommand::ManaInsert(command) => {
            state.apply_resolved_mana_insert(command).unwrap();
        }
        ResolvedRulesCommand::ManaSpend(command) => {
            state.apply_resolved_mana_spend(command).unwrap();
        }
    }
}

/// The real activation inserts the auto-tapped land's exact pip, spends it for
/// the Signet, then inserts the two produced pips. Reapplying the recorded
/// commands in entry order must reproduce that pool and its pip high-water.
#[test]
fn real_mana_activation_replays_recorded_insert_and_spend_commands() {
    let (pre_state, ordinary_state) = activated_signet_states();
    let commands = semantic_mana_commands(&ordinary_state);

    assert!(
        commands
            .iter()
            .any(|command| matches!(command, ResolvedRulesCommand::ManaInsert(_))),
        "the ordinary activation must journal exact insert commands"
    );
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, ResolvedRulesCommand::ManaSpend(_))),
        "the ordinary activation must journal its exact solver-selected payment"
    );

    let mut replay = pre_state;
    replay.resolved_rules_journal = ordinary_state.resolved_rules_journal.clone();
    for command in &commands {
        apply_mana_command(&mut replay, command);
    }

    for (replayed, ordinary) in replay.players.iter().zip(&ordinary_state.players) {
        assert_eq!(replayed.mana_pool, ordinary.mana_pool);
        assert_eq!(
            replayed
                .mana_pool
                .mana
                .iter()
                .map(|unit| unit.pip_id)
                .collect::<Vec<_>>(),
            ordinary
                .mana_pool
                .mana
                .iter()
                .map(|unit| unit.pip_id)
                .collect::<Vec<_>>(),
            "replay preserves the exact surviving mana identities"
        );
    }
    assert_eq!(replay.next_pip_id, ordinary_state.next_pip_id);
    assert_eq!(
        replay.resolved_rules_journal,
        ordinary_state.resolved_rules_journal
    );
}

/// A mana-spend command composes after its producer's insert command and is
/// not idempotent: applying the same exact removal twice is a typed invariant
/// failure rather than a fresh payment-solver decision.
#[test]
fn exact_mana_spend_rejects_a_second_removal() {
    let (pre_state, ordinary_state) = activated_signet_states();
    let commands = semantic_mana_commands(&ordinary_state);
    let mut replay = pre_state;
    replay.resolved_rules_journal = ordinary_state.resolved_rules_journal.clone();

    let mut observed_spend = false;
    for command in &commands {
        match command {
            ResolvedRulesCommand::ManaInsert(_) => apply_mana_command(&mut replay, command),
            ResolvedRulesCommand::ManaSpend(command) => {
                replay.apply_resolved_mana_spend(command).unwrap();
                assert!(matches!(
                    replay.apply_resolved_mana_spend(command),
                    Err(ResolvedManaReplayInvariantError::MissingExactManaUnit(_))
                ));
                observed_spend = true;
                break;
            }
        }
    }
    assert!(
        observed_spend,
        "the real activation must include a mana spend command"
    );
}
