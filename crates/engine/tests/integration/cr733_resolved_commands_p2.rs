//! P2 replay coverage for resolved mana, scalar, status, counter, and ledger commands.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerCounterKind;
use engine::types::resolved_commands::{
    ResolvedLedgerEdit, ResolvedLedgerEditReplayInvariantError, ResolvedManaReplayInvariantError,
    ResolvedObjectCounterReplayInvariantError, ResolvedObjectStatusReplayInvariantError,
    ResolvedPlayerEdit, ResolvedPlayerEditCommand, ResolvedPlayerEditReplayInvariantError,
    ResolvedRulesCommand, RulesExecutionNodeRef,
};

const DIMIR_SIGNET_ORACLE: &str = "{1}, {T}: Add {U}{B}.";
const STONY_STRENGTH_ORACLE: &str =
    "Put a +1/+1 counter on target creature you control. Untap that creature.";

fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.card_types.core_types = vec![CoreType::Artifact];
    object.base_card_types = object.card_types.clone();
    object.power = None;
    object.toughness = None;
    object.base_power = None;
    object.base_toughness = None;
}

fn activated_signet_states() -> (GameState, GameState, ObjectId) {
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
    (pre_state, runner.state().clone(), signet)
}

fn semantic_commands(state: &GameState) -> Vec<ResolvedRulesCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.clone())
        .collect()
}

fn apply_semantic_command(state: &mut GameState, command: &ResolvedRulesCommand) {
    match command {
        ResolvedRulesCommand::ManaInsert(command) => {
            state.apply_resolved_mana_insert(command).unwrap();
        }
        ResolvedRulesCommand::ManaSpend(command) => {
            state.apply_resolved_mana_spend(command).unwrap();
        }
        ResolvedRulesCommand::PlayerEdit(command) => {
            state.apply_resolved_player_edit(command).unwrap();
        }
        ResolvedRulesCommand::ObjectStatus(command) => {
            engine::game::object_state::apply_resolved_object_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::ObjectCounter(command) => {
            engine::game::effects::counters::apply_resolved_counter_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::LedgerEdit(command) => {
            engine::game::ledger::apply_resolved_ledger_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::LibraryShuffle(command) => {
            engine::game::library::apply_resolved_library_shuffle(state, command, &mut Vec::new())
                .unwrap();
        }
        ResolvedRulesCommand::Information(command) => {
            state.apply_resolved_information(command).unwrap();
        }
    }
}

/// The real activation inserts the auto-tapped land's exact pip, spends it for
/// the Signet, then inserts the two produced pips. Reapplying the recorded
/// commands in entry order must reproduce that pool and its pip high-water.
#[test]
fn real_mana_activation_replays_recorded_insert_and_spend_commands() {
    let (pre_state, ordinary_state, signet) = activated_signet_states();
    let commands = semantic_commands(&ordinary_state);

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
        apply_semantic_command(&mut replay, command);
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
        replay.objects[&signet].tapped, ordinary_state.objects[&signet].tapped,
        "replay preserves the activated source's exact tapped status"
    );
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
    let (pre_state, ordinary_state, _) = activated_signet_states();
    let commands = semantic_commands(&ordinary_state);
    let mut replay = pre_state;
    replay.resolved_rules_journal = ordinary_state.resolved_rules_journal.clone();

    let mut observed_spend = false;
    for command in &commands {
        match command {
            ResolvedRulesCommand::ManaInsert(_) => apply_semantic_command(&mut replay, command),
            ResolvedRulesCommand::ManaSpend(command) => {
                replay.apply_resolved_mana_spend(command).unwrap();
                assert!(matches!(
                    replay.apply_resolved_mana_spend(command),
                    Err(ResolvedManaReplayInvariantError::MissingExactManaUnit(_))
                ));
                observed_spend = true;
                break;
            }
            ResolvedRulesCommand::PlayerEdit(_)
            | ResolvedRulesCommand::ObjectStatus(_)
            | ResolvedRulesCommand::ObjectCounter(_)
            | ResolvedRulesCommand::LedgerEdit(_)
            | ResolvedRulesCommand::LibraryShuffle(_)
            | ResolvedRulesCommand::Information(_) => apply_semantic_command(&mut replay, command),
        }
    }
    assert!(
        observed_spend,
        "the real activation must include a mana spend command"
    );
}

fn damage_spell_states() -> (GameState, GameState) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    let pre_state = runner.state().clone();

    let outcome = runner.cast(bolt).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -3);

    (pre_state, runner.state().clone())
}

/// A real damage spell records the final post-replacement life delta. Replaying
/// that semantic command changes only the retained prefix's player resources.
#[test]
fn real_damage_spell_replays_recorded_final_life_delta() {
    let (pre_state, ordinary_state) = damage_spell_states();
    let commands = semantic_commands(&ordinary_state);
    let life_command = commands
        .iter()
        .find(|command| {
            matches!(
                command,
                ResolvedRulesCommand::PlayerEdit(ResolvedPlayerEditCommand {
                    player: P1,
                    edit: ResolvedPlayerEdit::Life { delta: -3 },
                    ..
                })
            )
        })
        .expect("Lightning Bolt must journal its final delivered life delta");

    let mut replay = pre_state;
    replay.resolved_rules_journal = ordinary_state.resolved_rules_journal.clone();
    apply_semantic_command(&mut replay, life_command);

    let replayed = replay
        .players
        .iter()
        .find(|player| player.id == P1)
        .unwrap();
    let ordinary = ordinary_state
        .players
        .iter()
        .find(|player| player.id == P1)
        .unwrap();
    assert_eq!(replayed.life, ordinary.life);
    assert_eq!(
        replayed.life_lost_this_turn, ordinary.life_lost_this_turn,
        "the final delta carries life-loss bookkeeping without rerunning replacement"
    );
}

/// Exact status commands are intentionally non-idempotent: replaying a recorded
/// tap twice fails its old-status precondition, and a same-id new incarnation
/// fails rather than accepting a stale reference.
#[test]
fn recorded_tap_rejects_double_apply_and_stale_incarnation() {
    let (pre_state, ordinary_state, signet) = activated_signet_states();
    let command = semantic_commands(&ordinary_state)
        .into_iter()
        .find_map(|command| match command {
            ResolvedRulesCommand::ObjectStatus(command) if command.object.object_id == signet => {
                Some(command)
            }
            _ => None,
        })
        .expect("the real Signet activation must journal its tap cost");

    let mut replay = pre_state.clone();
    engine::game::object_state::apply_resolved_object_edit(&mut replay, &command).unwrap();
    assert!(matches!(
        engine::game::object_state::apply_resolved_object_edit(&mut replay, &command),
        Err(ResolvedObjectStatusReplayInvariantError::StatusPreconditionMismatch { .. })
    ));

    let mut stale = pre_state;
    stale
        .objects
        .get_mut(&command.object.object_id)
        .unwrap()
        .bump_incarnation();
    assert!(matches!(
        engine::game::object_state::apply_resolved_object_edit(&mut stale, &command),
        Err(ResolvedObjectStatusReplayInvariantError::StaleObject { .. })
    ));
}

/// Scalar deltas compose until the resource's actual precondition rejects a
/// duplicate removal; no player snapshot is restored over an independent edit.
#[test]
fn exact_scalar_resource_removal_rejects_a_second_underflowing_apply() {
    let mut state = GameState::new_two_player(7);
    state.players[0].energy = 1;
    let command = ResolvedPlayerEditCommand {
        player: P0,
        edit: ResolvedPlayerEdit::Energy { delta: -1 },
        cause: RulesExecutionNodeRef::Proposal(
            engine::types::resolved_commands::ResolvedCommandOrdinal(0),
        ),
    };

    state.apply_resolved_player_edit(&command).unwrap();
    assert!(matches!(
        state.apply_resolved_player_edit(&command),
        Err(ResolvedPlayerEditReplayInvariantError::ResourceUnderflow)
    ));
}

/// The scalar authority edits one resource axis at a time. It records semantic
/// deltas/transitions instead of replacing a player snapshot, so unrelated
/// retained resource edits remain intact.
#[test]
fn scalar_commands_compose_across_life_energy_counters_and_speed() {
    let mut state = GameState::new_two_player(7);

    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: 2 })
        .unwrap();
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Energy { delta: 3 })
        .unwrap();
    state
        .resolve_and_apply_player_edit(
            P0,
            ResolvedPlayerEdit::Counter {
                kind: PlayerCounterKind::Experience,
                delta: 1,
            },
        )
        .unwrap();
    state
        .resolve_and_apply_player_edit(
            P0,
            ResolvedPlayerEdit::Speed {
                old: None,
                new: Some(3),
            },
        )
        .unwrap();

    let player = state.players.iter().find(|player| player.id == P0).unwrap();
    assert_eq!(player.life, 22);
    assert_eq!(player.life_gained_this_turn, 2);
    assert_eq!(player.energy, 3);
    assert_eq!(
        player.player_counter(&PlayerCounterKind::Experience),
        1,
        "the counter edit must not overwrite the preceding scalar edits"
    );
    assert_eq!(player.speed, Some(3));
    assert_eq!(
        semantic_commands(&state).len(),
        4,
        "each final scalar edit has one journal command"
    );
}

fn counter_spell_states() -> (GameState, GameState, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Counter Target", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Stony Strength", false, STONY_STRENGTH_ORACLE)
        .id();
    let mut runner = scenario.build();
    let pre_state = runner.state().clone();

    runner.cast(spell).target_object(target).resolve();

    (pre_state, runner.state().clone(), target)
}

/// A real counter spell records the final object-counter delivery. Replaying
/// the semantic journal never consults the replacement pipeline a second time.
#[test]
fn real_counter_spell_replays_recorded_object_counter_delivery() {
    let (pre_state, ordinary_state, target) = counter_spell_states();
    let commands = semantic_commands(&ordinary_state);
    assert!(commands.iter().any(|command| matches!(
        command,
        ResolvedRulesCommand::ObjectCounter(command)
            if command.object.object_id == target
                && command.counter_type == CounterType::Plus1Plus1
    )));

    let mut replay = pre_state;
    replay.resolved_rules_journal = ordinary_state.resolved_rules_journal.clone();
    for command in &commands {
        apply_semantic_command(&mut replay, command);
    }

    assert_eq!(
        replay.objects[&target].counters, ordinary_state.objects[&target].counters,
        "replay preserves the final post-replacement counter count"
    );
    assert_eq!(
        replay.counter_added_this_turn, ordinary_state.counter_added_this_turn,
        "counter history is part of the semantic counter delivery"
    );
}

/// Counter deliveries are exact occurrence transitions: a duplicate does not
/// add more counters, and an object with the same storage id but a new
/// incarnation is rejected.
#[test]
fn recorded_counter_rejects_double_apply_and_stale_incarnation() {
    let (pre_state, ordinary_state, target) = counter_spell_states();
    let command = semantic_commands(&ordinary_state)
        .into_iter()
        .find_map(|command| match command {
            ResolvedRulesCommand::ObjectCounter(command) if command.object.object_id == target => {
                Some(command)
            }
            _ => None,
        })
        .expect("Stony Strength must journal its object-counter delivery");

    let mut replay = pre_state.clone();
    engine::game::effects::counters::apply_resolved_counter_edit(&mut replay, &command).unwrap();
    assert!(matches!(
        engine::game::effects::counters::apply_resolved_counter_edit(&mut replay, &command),
        Err(ResolvedObjectCounterReplayInvariantError::CounterPreconditionMismatch { .. })
    ));

    let mut stale = pre_state;
    stale.objects.get_mut(&target).unwrap().bump_incarnation();
    assert!(matches!(
        engine::game::effects::counters::apply_resolved_counter_edit(&mut stale, &command),
        Err(ResolvedObjectCounterReplayInvariantError::StaleObject { .. })
    ));
}

/// A finalized cast records an append-only spell history command. Applying it
/// twice fails its captured prefix rather than appending a duplicate history.
#[test]
fn real_spell_cast_replays_its_exact_ledger_record_once() {
    let (pre_state, ordinary_state, _) = counter_spell_states();
    let command = semantic_commands(&ordinary_state)
        .into_iter()
        .find_map(|command| match command {
            ResolvedRulesCommand::LedgerEdit(command)
                if matches!(&command.edit, ResolvedLedgerEdit::SpellCast { .. }) =>
            {
                Some(command)
            }
            _ => None,
        })
        .expect("the real spell cast must journal its exact ledger record");

    let mut replay = pre_state;
    engine::game::ledger::apply_resolved_ledger_edit(&mut replay, &command).unwrap();
    assert_eq!(
        replay.spells_cast_this_turn,
        ordinary_state.spells_cast_this_turn
    );
    assert_eq!(
        replay.spells_cast_this_game,
        ordinary_state.spells_cast_this_game
    );
    assert_eq!(
        replay.spells_cast_this_turn_by_player,
        ordinary_state.spells_cast_this_turn_by_player
    );
    assert!(matches!(
        engine::game::ledger::apply_resolved_ledger_edit(&mut replay, &command),
        Err(ResolvedLedgerEditReplayInvariantError::SpellCastPreconditionMismatch)
    ));
}
