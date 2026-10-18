//! P2 replay coverage for resolved mana, scalar, status, counter, and ledger commands.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerCounterKind;
use engine::types::resolved_commands::ResolvedContinuousEffectEdit;
use engine::types::resolved_commands::{
    ResolvedLedgerEdit, ResolvedLedgerEditReplayInvariantError, ResolvedManaReplayInvariantError,
    ResolvedObjectCounterEdit, ResolvedObjectCounterReplayInvariantError,
    ResolvedObjectStatusReplayInvariantError, ResolvedPlayerEdit, ResolvedPlayerEditCommand,
    ResolvedPlayerEditReplayInvariantError, ResolvedRulesCommand, RulesExecutionNodeRef,
};

const DIMIR_SIGNET_ORACLE: &str = "{1}, {T}: Add {U}{B}.";
const STONY_STRENGTH_ORACLE: &str =
    "Put a +1/+1 counter on target creature you control. Untap that creature.";
const SHIELD_BROKER_ORACLE: &str = "When this creature enters, put a shield counter on target noncommander creature you don't control. You gain control of that creature for as long as it has a shield counter on it. (If it would be dealt damage or destroyed, remove a shield counter from it instead.)";
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";
const BOON_OF_SAFETY_ORACLE: &str = "Put a shield counter on target creature. (If it would be dealt damage or destroyed, remove a shield counter from it instead.)\nScry 1.";
const GIANT_GROWTH_ORACLE: &str = "Target creature gets +3/+3 until end of turn.";

#[test]
fn shield_broker_expiry_replays_before_later_growth_install() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_vanilla(P1, 4, 4);
    let broker = scenario
        .add_creature_to_hand_from_oracle(P0, "Shield Broker", 3, 4, SHIELD_BROKER_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let boon = scenario
        .add_spell_to_hand_from_oracle(P0, "Boon of Safety", true, BOON_OF_SAFETY_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(broker).target_object(victim).resolve();
    assert_eq!(
        runner.state().objects[&victim]
            .counters
            .get(&CounterType::Shield),
        Some(&1)
    );
    assert_eq!(runner.state().objects[&victim].controller, P0);
    assert_eq!(runner.state().transient_continuous_effects.len(), 1);
    let control_effect_id = runner.state().transient_continuous_effects[0].id;

    // The replay prefix begins after the trigger's counter and control install.
    let pre_removal = runner.state().clone();
    let journal_start = pre_removal.resolved_rules_journal.entries().len();

    // With a second shield already present, damage leaves the duration true.
    let mut remaining_shield = GameRunner::from_state(pre_removal.clone());
    remaining_shield.cast(boon).target_object(victim).resolve();
    assert_eq!(
        remaining_shield.state().objects[&victim]
            .counters
            .get(&CounterType::Shield),
        Some(&2)
    );
    remaining_shield.cast(shock).target_object(victim).resolve();
    assert_eq!(
        remaining_shield.state().objects[&victim]
            .counters
            .get(&CounterType::Shield),
        Some(&1)
    );
    assert_eq!(remaining_shield.state().objects[&victim].controller, P0);
    assert!(remaining_shield
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| effect.id == control_effect_id));

    runner.cast(shock).target_object(victim).resolve();
    let after_damage = runner.state().clone();
    runner.cast(boon).target_object(victim).resolve();
    let before_growth = runner.state().clone();
    runner.cast(growth).target_object(victim).resolve();
    let live = runner.state();
    let suffix: Vec<_> = live
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .collect();
    assert!(
        !suffix.iter().any(|command| matches!(command,
        ResolvedRulesCommand::ContinuousEffect(edit)
            if matches!(edit.as_ref(), ResolvedContinuousEffectEdit::Retire(retirement)
                if retirement.effects.iter().any(|effect| effect.id == control_effect_id)))),
        "the final-shield command already owns this retirement"
    );
    let shield_removals: Vec<_> = suffix
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match command {
            ResolvedRulesCommand::ObjectCounter(edit)
                if edit.object.object_id == victim
                    && edit.counter_type == CounterType::Shield
                    && matches!(edit.edit, ResolvedObjectCounterEdit::Remove { count: 1 }) =>
            {
                Some((index, command.clone()))
            }
            _ => None,
        })
        .collect();
    let shield_additions: Vec<_> = suffix
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match command {
            ResolvedRulesCommand::ObjectCounter(edit)
                if edit.object.object_id == victim
                    && edit.counter_type == CounterType::Shield
                    && matches!(edit.edit, ResolvedObjectCounterEdit::Add { count: 1, .. }) =>
            {
                Some((index, command.clone()))
            }
            _ => None,
        })
        .collect();
    let growth_installs: Vec<_> = suffix
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match command {
            ResolvedRulesCommand::ContinuousEffect(edit)
                if matches!(edit.as_ref(), ResolvedContinuousEffectEdit::Install(install)
                    if install.effect.source_name == "Giant Growth") =>
            {
                Some((index, command.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        shield_removals.len(),
        1,
        "damage consumes exactly one journaled shield removal"
    );
    assert_eq!(
        shield_additions.len(),
        1,
        "Boon delivers exactly one journaled shield addition"
    );
    assert_eq!(
        growth_installs.len(),
        1,
        "Growth delivers exactly one journaled TCE install"
    );
    assert!(
        shield_removals[0].0 < shield_additions[0].0
            && shield_additions[0].0 < growth_installs[0].0
    );
    let ResolvedRulesCommand::ContinuousEffect(edit) = &growth_installs[0].1 else {
        unreachable!()
    };
    let ResolvedContinuousEffectEdit::Install(growth_install) = edit.as_ref() else {
        unreachable!()
    };
    assert_eq!(
        growth_install.expected_installed_count, 0,
        "the recorded Growth install position must reflect permanent retirement"
    );
    assert_eq!(
        after_damage.objects[&victim]
            .counters
            .get(&CounterType::Shield),
        None
    );
    assert_eq!(after_damage.objects[&victim].controller, P1);
    assert!(after_damage
        .transient_continuous_effects
        .iter()
        .all(|effect| effect.id != control_effect_id));
    assert_eq!(
        before_growth.objects[&victim]
            .counters
            .get(&CounterType::Shield),
        Some(&1)
    );
    assert_eq!(
        before_growth.objects[&victim].controller, P1,
        "a later shield cannot revive Shield Broker's old control effect"
    );
    assert!(
        before_growth.transient_continuous_effects.is_empty(),
        "the old id is retired before Growth installs"
    );

    let mut replay = pre_removal;
    apply_semantic_command(&mut replay, &shield_removals[0].1);
    assert!(
        replay.transient_continuous_effects.is_empty(),
        "replaying the exact removal retires the same id"
    );
    assert!(
        matches!(&shield_removals[0].1, ResolvedRulesCommand::ObjectCounter(edit) if engine::game::effects::counters::apply_resolved_counter_edit(&mut replay.clone(), edit).is_err()),
        "replaying the same removal twice must fail its old-count precondition"
    );
    apply_semantic_command(&mut replay, &shield_additions[0].1);
    assert!(replay.transient_continuous_effects.is_empty());
    apply_semantic_command(&mut replay, &growth_installs[0].1);
    engine::game::layers::evaluate_layers(&mut replay);
    assert_eq!(
        replay.objects[&victim].counters,
        live.objects[&victim].counters
    );
    assert_eq!(
        replay.objects[&victim].controller,
        live.objects[&victim].controller
    );
    assert_eq!(
        replay.transient_continuous_effects,
        live.transient_continuous_effects
    );
}

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
        ResolvedRulesCommand::ObjectTransform(command) => {
            engine::game::transform::apply_resolved_transform(state, command).unwrap();
        }
        ResolvedRulesCommand::Attachment(command) => {
            engine::game::effects::attach::apply_resolved_attachment(state, command).unwrap();
        }
        ResolvedRulesCommand::DelayedTriggerInstall(command) => {
            engine::game::triggers::apply_resolved_delayed_trigger(state, command.as_ref())
                .unwrap();
        }
        ResolvedRulesCommand::ContinuousEffect(command) => {
            state
                .apply_resolved_continuous_effect_edit(command.as_ref())
                .unwrap();
        }
        ResolvedRulesCommand::CombatMembership(command) => {
            engine::game::combat::apply_resolved_combat_membership(state, command).unwrap();
        }
        ResolvedRulesCommand::ControllerOverride(command) => {
            engine::game::zones::apply_resolved_controller_override(state, command).unwrap();
        }
        ResolvedRulesCommand::EntryProvenance(command) => {
            engine::game::zones::apply_resolved_entry_provenance(state, command).unwrap();
        }
        ResolvedRulesCommand::ObjectCease(command) => {
            engine::game::zones::apply_resolved_object_cease(state, command).unwrap();
        }
        ResolvedRulesCommand::PlayerLeave(command) => {
            engine::game::elimination::apply_resolved_player_leave(state, command).unwrap();
        }
        ResolvedRulesCommand::TokenCreation(command) => {
            engine::game::effects::token::apply_resolved_token_creation(state, command).unwrap();
        }
        ResolvedRulesCommand::LedgerEdit(command) => {
            engine::game::ledger::apply_resolved_ledger_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::LibraryShuffle(command) => {
            engine::game::library::apply_resolved_library_shuffle(state, command, &mut Vec::new())
                .unwrap();
        }
        ResolvedRulesCommand::ZoneChange(command) => {
            engine::game::zones::apply_resolved_zone_change(state, command).unwrap();
        }
        ResolvedRulesCommand::Information(command) => {
            state.apply_resolved_information(command).unwrap();
        }
        ResolvedRulesCommand::FrameTransition(command) => {
            state
                .apply_resolved_frame_transition(command.as_ref())
                .unwrap();
        }
        ResolvedRulesCommand::TriggerCollection(command) => {
            engine::game::triggers::apply_resolved_trigger_collection(state, command).unwrap();
        }
        ResolvedRulesCommand::StackPush(command) => {
            engine::game::stack::apply_resolved_stack_push(state, command.as_ref()).unwrap();
        }
        ResolvedRulesCommand::StackEntryFinalize(command) => {
            engine::game::stack::apply_resolved_stack_entry_finalize(state, command.as_ref())
                .unwrap();
        }
        ResolvedRulesCommand::UncommittedTriggerRemoval(command) => {
            engine::game::stack::apply_resolved_uncommitted_trigger_removal(
                state,
                command.as_ref(),
            )
            .unwrap();
        }
        ResolvedRulesCommand::StackRemoval(command) => {
            engine::game::stack::apply_resolved_stack_removal(state, command.as_ref()).unwrap();
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
                .units()
                .map(|unit| unit.pip_id)
                .collect::<Vec<_>>(),
            ordinary
                .mana_pool
                .units()
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
            | ResolvedRulesCommand::ObjectTransform(_)
            | ResolvedRulesCommand::Attachment(_)
            | ResolvedRulesCommand::DelayedTriggerInstall(_)
            | ResolvedRulesCommand::ContinuousEffect(_)
            | ResolvedRulesCommand::CombatMembership(_)
            | ResolvedRulesCommand::ControllerOverride(_)
            | ResolvedRulesCommand::EntryProvenance(_)
            | ResolvedRulesCommand::ObjectCease(_)
            | ResolvedRulesCommand::PlayerLeave(_)
            | ResolvedRulesCommand::TokenCreation(_)
            | ResolvedRulesCommand::LedgerEdit(_)
            | ResolvedRulesCommand::LibraryShuffle(_)
            | ResolvedRulesCommand::ZoneChange(_)
            | ResolvedRulesCommand::Information(_)
            | ResolvedRulesCommand::FrameTransition(_)
            | ResolvedRulesCommand::TriggerCollection(_)
            | ResolvedRulesCommand::StackPush(_)
            | ResolvedRulesCommand::StackEntryFinalize(_)
            | ResolvedRulesCommand::UncommittedTriggerRemoval(_)
            | ResolvedRulesCommand::StackRemoval(_) => apply_semantic_command(&mut replay, command),
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

const ROOTWATER_ORACLE: &str =
    "{T}: Gain control of target creature for as long as that creature is enchanted.";
const HOLY_STRENGTH_ORACLE: &str = "Enchant creature\nEnchanted creature gets +1/+2.";
const DISENCHANT_ORACLE: &str = "Destroy target artifact or enchantment.";
const PACIFISM_ORACLE: &str = "Enchant creature\nEnchanted creature can't attack or block.";

fn rootwater_activation(runner: &mut GameRunner, source: ObjectId, recipient: ObjectId) {
    use engine::types::ability::{AbilityKind, EffectKind};
    use engine::types::events::GameEvent;
    let index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .unwrap();
    let outcome = runner
        .activate(source, index)
        .target_object(recipient)
        .resolve();
    assert!(outcome.state().objects[&source].tapped);
    assert_eq!(outcome.state().objects[&recipient].controller, P0);
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::ControllerChanged { object_id, old_controller: P1, new_controller: P0 }
            if *object_id == recipient)));
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::GainControl,
            ..
        }
    )));
}

fn retirement_batches(
    state: &GameState,
    start: usize,
) -> Vec<engine::types::resolved_commands::ResolvedContinuousEffectRetirementCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                ResolvedContinuousEffectEdit::Retire(command) => Some(command.clone()),
                ResolvedContinuousEffectEdit::Install(_) => None,
            },
            _ => None,
        })
        .collect()
}

fn growth_install(
    state: &GameState,
    start: usize,
) -> engine::types::resolved_commands::ResolvedContinuousEffectCommand {
    let installs: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                ResolvedContinuousEffectEdit::Install(command)
                    if command.effect.source_name == "Giant Growth" =>
                {
                    Some(command.clone())
                }
                ResolvedContinuousEffectEdit::Install(_)
                | ResolvedContinuousEffectEdit::Retire(_) => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(installs.len(), 1);
    installs[0].clone()
}

#[test]
fn rootwater_aura_exit_replays_before_later_growth_install() {
    use engine::game::effects::attach::attach_to;
    use engine::types::game_state::TransientContinuousEffect;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::resolved_commands::ResolvedContinuousEffectReplayInvariantError;
    use engine::types::zones::Zone;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Rootwater Matriarch", 2, 3, ROOTWATER_ORACLE)
        .id();
    let recipient = scenario.add_vanilla(P1, 2, 2);
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Holy Strength", HOLY_STRENGTH_ORACLE)
        .with_subtypes(vec!["Aura"])
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, recipient);
    rootwater_activation(&mut runner, source, recipient);
    assert_eq!(runner.state().transient_continuous_effects.len(), 1);
    let old: TransientContinuousEffect = runner.state().transient_continuous_effects[0].clone();
    let recipient_ref = ObjectIncarnationRef::from_object(&runner.state().objects[&recipient]);
    assert_eq!(old.affected_recipient, Some(recipient_ref));
    assert_eq!(old.duration_subject, Some(recipient_ref));
    let committed = runner.cast(removal).target_object(aura).commit();
    let prefix = committed.state().clone();
    let start = prefix.resolved_rules_journal.entries().len();
    assert_eq!(prefix.objects[&removal].zone, Zone::Stack);
    assert_eq!(
        prefix.objects[&aura]
            .attached_to
            .and_then(|host| host.as_object()),
        Some(recipient)
    );
    assert_eq!(prefix.transient_continuous_effects[0], old);
    committed.resolve().assert_zone(&[aura], Zone::Graveyard);
    assert!(runner.state().objects[&aura].attached_to.is_none());
    assert!(!runner.state().objects[&recipient]
        .attachments
        .contains(&aura));
    assert_eq!(runner.state().objects[&recipient].controller, P1);
    assert!(runner.state().transient_continuous_effects.is_empty());
    runner.cast(growth).target_object(recipient).resolve();
    let live = runner.state();
    let install = growth_install(live, start);
    assert_eq!(install.expected_installed_count, 0);
    let batches = retirement_batches(live, start);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].effects, vec![old.clone()]);
    let commands: Vec<_> = live
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| entry.command.as_ref())
        .filter(|command| match command {
            ResolvedRulesCommand::ZoneChange(command) => command.object.object_id == aura,
            ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                ResolvedContinuousEffectEdit::Retire(_) => true,
                ResolvedContinuousEffectEdit::Install(command) => {
                    command.effect.source_name == "Giant Growth"
                }
            },
            _ => false,
        })
        .collect();
    assert!(matches!(
        commands.as_slice(),
        [
            ResolvedRulesCommand::ZoneChange(_),
            ResolvedRulesCommand::ContinuousEffect(_),
            ResolvedRulesCommand::ContinuousEffect(_)
        ]
    ));
    let mut replay = prefix;
    let journal = replay.resolved_rules_journal.clone();
    apply_semantic_command(&mut replay, commands[0]);
    assert!(replay.objects[&aura].attached_to.is_none());
    assert!(!replay.objects[&recipient].attachments.contains(&aura));
    assert_eq!(
        replay.transient_continuous_effects[0], old,
        "zone replay is structural, so this is the missing-receipt discriminator"
    );
    assert_eq!(
        replay.clone().apply_resolved_continuous_effect(&install),
        Err(
            ResolvedContinuousEffectReplayInvariantError::InstalledCountPreconditionMismatch {
                expected: 0,
                found: 1
            }
        )
    );
    let allocators = (
        replay.next_continuous_effect_id,
        replay.next_timestamp,
        replay.next_end_effect_group_id,
    );
    apply_semantic_command(&mut replay, commands[1]);
    assert!(replay.transient_continuous_effects.is_empty());
    assert_eq!(
        allocators,
        (
            replay.next_continuous_effect_id,
            replay.next_timestamp,
            replay.next_end_effect_group_id
        )
    );
    let mut extra = replay.clone();
    let mut unrelated = old.clone();
    unrelated.id += 1000;
    extra.transient_continuous_effects.push_back(unrelated);
    assert_eq!(
        extra.apply_resolved_continuous_effect(&install),
        Err(
            ResolvedContinuousEffectReplayInvariantError::InstalledCountPreconditionMismatch {
                expected: 0,
                found: 1
            }
        )
    );
    apply_semantic_command(&mut replay, commands[2]);
    assert_eq!(replay.resolved_rules_journal, journal);
    engine::game::layers::evaluate_layers(&mut replay);
    assert_eq!(replay.resolved_rules_journal, journal);
    // CR 611.2b: last Aura loss ends control; Growth alone makes the 2/2 a 5/5.
    for state in [&replay, live] {
        assert_eq!(state.objects[&recipient].controller, P1);
        assert_eq!(
            (
                state.objects[&recipient].power,
                state.objects[&recipient].toughness
            ),
            (Some(5), Some(5))
        );
        assert_eq!(
            state
                .transient_continuous_effects
                .iter()
                .collect::<Vec<_>>(),
            vec![&install.effect]
        );
    }
}

fn recipient_condition(
    property: engine::types::ability::FilterProp,
) -> engine::types::ability::StaticCondition {
    use engine::types::ability::{StaticCondition, TargetFilter, TypedFilter};
    StaticCondition::RecipientMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::default().properties(vec![property])),
    }
}

fn enchanted_condition() -> engine::types::ability::StaticCondition {
    use engine::types::ability::{AttachmentKind, FilterProp, SourceExclusion};
    recipient_condition(FilterProp::HasAttachment {
        kind: AttachmentKind::Aura,
        controller: None,
        exclude_source: SourceExclusion::Include,
    })
}

#[test]
fn settled_compound_and_successive_wave_retirements_replay_exactly() {
    use engine::types::ability::{
        ContinuousModification, Duration, FilterProp, StaticCondition, TargetFilter,
    };
    use engine::types::game_state::TransientContinuousEffectBindings;
    use engine::types::identifiers::ObjectIncarnationRef;
    // Typed contract fixtures, not invented Oracle abilities. The mutations
    // still run through real Disenchant and Giant Growth casts.
    for successive_waves in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario.add_vanilla(P0, 2, 2);
        let b = scenario.add_vanilla(P0, 2, 2);
        let holy = scenario
            .add_enchantment_from_oracle(P0, "Holy Strength", HOLY_STRENGTH_ORACLE)
            .with_subtypes(vec!["Aura"])
            .id();
        let pacifism = scenario
            .add_enchantment_from_oracle(P0, "Pacifism", PACIFISM_ORACLE)
            .with_subtypes(vec!["Aura"])
            .id();
        let removal = scenario
            .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT_ORACLE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let growth = scenario
            .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH_ORACLE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        engine::game::effects::attach::attach_to(runner.state_mut(), holy, a);
        engine::game::effects::attach::attach_to(
            runner.state_mut(),
            pacifism,
            if successive_waves { b } else { a },
        );
        assert!(runner.state().objects[&a].attachments.contains(&holy));
        assert!(
            runner.state().objects[&if successive_waves { b } else { a }]
                .attachments
                .contains(&pacifism)
        );
        let a_ref = ObjectIncarnationRef::from_object(&runner.state().objects[&a]);
        let b_ref = ObjectIncarnationRef::from_object(&runner.state().objects[&b]);
        let power = recipient_condition(FilterProp::PowerExceedsBase);
        let (subject, affected, condition, modification) = if successive_waves {
            (
                a_ref,
                b_ref,
                enchanted_condition(),
                ContinuousModification::AddPower { value: 1 },
            )
        } else {
            (
                a_ref,
                a_ref,
                StaticCondition::And {
                    conditions: vec![enchanted_condition(), power.clone()],
                },
                ContinuousModification::AddToughness { value: 1 },
            )
        };
        let first = runner
            .state_mut()
            .add_transient_continuous_effect_with_bindings(
                a,
                P0,
                Duration::ForAsLongAs { condition },
                TargetFilter::SpecificObject {
                    id: affected.object_id,
                },
                vec![modification],
                None,
                TransientContinuousEffectBindings {
                    affected_recipient: Some(affected),
                    duration_subject: Some(subject),
                    granting_object: None,
                },
            )
            .expect("the fixture's duration begins");
        engine::game::layers::flush_layers(runner.state_mut());
        let second = successive_waves.then(|| {
            let id = runner
                .state_mut()
                .add_transient_continuous_effect_with_bindings(
                    b,
                    P0,
                    Duration::ForAsLongAs { condition: power },
                    TargetFilter::SpecificObject { id: b },
                    vec![ContinuousModification::AddToughness { value: 1 }],
                    None,
                    TransientContinuousEffectBindings {
                        affected_recipient: Some(b_ref),
                        duration_subject: Some(b_ref),
                        granting_object: None,
                    },
                )
                .expect("the fixture's duration begins");
            engine::game::layers::flush_layers(runner.state_mut());
            id
        });
        assert_eq!(runner.state().objects[&affected.object_id].power, Some(3));
        assert_eq!(
            runner.state().transient_continuous_effects.len(),
            if successive_waves { 2 } else { 1 }
        );
        let committed = runner.cast(removal).target_object(holy).commit();
        let prefix = committed.state().clone();
        let start = prefix.resolved_rules_journal.entries().len();
        committed.resolve();
        assert!(runner.state().objects[&pacifism].attached_to.is_some());
        assert_eq!(runner.state().objects[&affected.object_id].power, Some(2));
        assert!(runner.state().transient_continuous_effects.is_empty());
        runner
            .cast(growth)
            .target_object(affected.object_id)
            .resolve();
        let batches = retirement_batches(runner.state(), start);
        assert_eq!(batches.len(), if successive_waves { 2 } else { 1 });
        assert_eq!(
            batches[0].effects.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![first]
        );
        if let Some(second) = second {
            assert_eq!(
                batches[1].effects.iter().map(|e| e.id).collect::<Vec<_>>(),
                vec![second]
            );
        }
        let install = growth_install(runner.state(), start);
        assert_eq!(install.expected_installed_count, 0);
        let mut replay = prefix;
        let journal = replay.resolved_rules_journal.clone();
        let ordered: Vec<_> = runner
            .state()
            .resolved_rules_journal
            .entries()
            .iter()
            .skip(start)
            .filter_map(|entry| entry.command.as_ref())
            .filter(|command| match command {
                ResolvedRulesCommand::ZoneChange(command) => command.object.object_id == holy,
                ResolvedRulesCommand::ContinuousEffect(_) => true,
                _ => false,
            })
            .collect();
        assert!(matches!(
            ordered.first(),
            Some(ResolvedRulesCommand::ZoneChange(_))
        ));
        assert_eq!(ordered.len(), batches.len() + 2);
        for command in ordered {
            apply_semantic_command(&mut replay, command);
        }
        engine::game::layers::evaluate_layers(&mut replay);
        assert_eq!(replay.resolved_rules_journal, journal);
        assert_eq!(
            replay
                .transient_continuous_effects
                .iter()
                .collect::<Vec<_>>(),
            vec![&install.effect]
        );
        assert_eq!(
            (
                replay.objects[&affected.object_id].power,
                replay.objects[&affected.object_id].toughness
            ),
            (Some(5), Some(5))
        );
    }
}

#[test]
fn rootwater_retirement_is_per_recipient_and_only_after_the_last_aura() {
    use engine::types::ability::TargetRef;
    use engine::types::keywords::Keyword;
    use engine::types::zones::Zone;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sources = [0, 1].map(|_| {
        scenario
            .add_creature_from_oracle(P0, "Rootwater Matriarch", 2, 3, ROOTWATER_ORACLE)
            .id()
    });
    let recipients = [0, 1].map(|_| scenario.add_vanilla(P1, 2, 2));
    let auras = [0, 1, 2].map(|_| {
        scenario
            .add_enchantment_from_oracle(P1, "Holy Strength", HOLY_STRENGTH_ORACLE)
            .with_subtypes(vec!["Aura"])
            .id()
    });
    let removals = [0, 1].map(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT_ORACLE)
            .with_mana_cost(ManaCost::zero())
            .id()
    });
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let replacement = scenario
        .add_spell_to_hand_from_oracle(P0, "Holy Strength", false, HOLY_STRENGTH_ORACLE)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], HOLY_STRENGTH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    for (aura, recipient) in [
        (auras[0], recipients[0]),
        (auras[1], recipients[0]),
        (auras[2], recipients[1]),
    ] {
        engine::game::effects::attach::attach_to(runner.state_mut(), aura, recipient);
    }
    for (source, recipient) in sources.into_iter().zip(recipients) {
        rootwater_activation(&mut runner, source, recipient);
    }
    let effects: Vec<_> = sources
        .into_iter()
        .map(|source| {
            runner
                .state()
                .transient_continuous_effects
                .iter()
                .find(|e| e.source_id == source)
                .unwrap()
                .clone()
        })
        .collect();
    let first_start = runner.state().resolved_rules_journal.entries().len();
    runner
        .cast(removals[0])
        .target_object(auras[0])
        .resolve()
        .assert_zone(&[auras[0]], Zone::Graveyard);
    assert_eq!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .collect::<Vec<_>>(),
        effects.iter().collect::<Vec<_>>()
    );
    assert!(retirement_batches(runner.state(), first_start).is_empty());
    assert_eq!(runner.state().objects[&recipients[0]].controller, P0);
    assert!(runner.state().objects[&recipients[0]]
        .attachments
        .contains(&auras[1]));
    let committed = runner.cast(removals[1]).target_object(auras[1]).commit();
    let prefix = committed.state().clone();
    let start = prefix.resolved_rules_journal.entries().len();
    committed
        .resolve()
        .assert_zone(&[auras[1]], Zone::Graveyard);
    assert_eq!(runner.state().objects[&recipients[0]].controller, P1);
    assert_eq!(runner.state().objects[&recipients[1]].controller, P0);
    assert_eq!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .collect::<Vec<_>>(),
        vec![&effects[1]]
    );
    let batches = retirement_batches(runner.state(), start);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].effects, vec![effects[0].clone()]);
    runner.cast(growth).target_object(recipients[0]).resolve();
    let install = growth_install(runner.state(), start);
    assert_eq!(install.expected_installed_count, 1);
    let mut replay = prefix;
    let journal = replay.resolved_rules_journal.clone();
    for command in runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|e| e.command.as_ref())
    {
        match command {
            ResolvedRulesCommand::ZoneChange(c) if c.object.object_id == auras[1] => {
                apply_semantic_command(&mut replay, command)
            }
            ResolvedRulesCommand::ContinuousEffect(_) => {
                apply_semantic_command(&mut replay, command)
            }
            _ => {}
        }
    }
    engine::game::layers::evaluate_layers(&mut replay);
    assert_eq!(replay.resolved_rules_journal, journal);
    assert_eq!(
        replay
            .transient_continuous_effects
            .iter()
            .collect::<Vec<_>>(),
        vec![&effects[1], &install.effect]
    );
    assert_eq!(replay.objects[&recipients[0]].controller, P1);
    let object = &runner.state().objects[&replacement];
    assert_eq!(object.zone, Zone::Hand);
    assert_eq!(object.card_types.core_types, vec![CoreType::Enchantment]);
    assert!(object
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Aura"));
    assert!(object
        .keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Enchant(_))));
    let committed = runner
        .cast(replacement)
        .target_object(recipients[0])
        .commit();
    assert_eq!(committed.state().objects[&replacement].zone, Zone::Stack);
    let entry = committed
        .state()
        .stack
        .back()
        .expect("the Aura must be on the stack");
    assert_eq!(entry.source_id, replacement);
    // CR 303.4a: the replacement Aura must commit its actual enchant target.
    assert_eq!(
        entry
            .ability()
            .expect("the Aura must carry its enchant target")
            .targets,
        vec![TargetRef::Object(recipients[0])]
    );
    committed
        .resolve()
        .assert_zone(&[replacement], Zone::Battlefield);
    // CR 608.3c: reenchantment must actually reach the intended recipient.
    assert_eq!(
        runner.state().objects[&replacement]
            .attached_to
            .and_then(|host| host.as_object()),
        Some(recipients[0])
    );
    assert!(runner.state().objects[&recipients[0]]
        .attachments
        .contains(&replacement));
    // CR 611.2b: a new Aura cannot restart A's retired grant; B remains exact.
    assert_eq!(runner.state().objects[&recipients[0]].controller, P1);
    assert_eq!(runner.state().objects[&recipients[1]].controller, P0);
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|e| e == &effects[1]));
    assert!(!runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == effects[0].id));
}

const MASTER_THIEF_ORACLE: &str = "When this creature enters, gain control of target artifact for as long as you control this creature.";
const SWITCHEROO_ORACLE: &str = "Exchange control of two target creatures.";
const ACT_OF_TREASON_ORACLE: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
const CLEVER_CONCEALMENT_ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nAny number of target nonland permanents you control phase out. (Treat them and anything attached to them as though they don't exist until your next turn.)";

/// A zero-cost spell in P0's hand: (name, is_instant, keywords, Oracle text).
type HandSpell = (&'static str, bool, &'static [&'static str], &'static str);

const SWITCHEROO: HandSpell = ("Switcheroo", false, &[], SWITCHEROO_ORACLE);
const ACT_OF_TREASON: HandSpell = ("Act of Treason", false, &[], ACT_OF_TREASON_ORACLE);
const CLEVER_CONCEALMENT: HandSpell = (
    "Clever Concealment",
    true,
    &["Convoke"],
    CLEVER_CONCEALMENT_ORACLE,
);
const GIANT_GROWTH: HandSpell = ("Giant Growth", true, &[], GIANT_GROWTH_ORACLE);

/// Master Thief (P0) has stolen P1's Loot. Returns the runner just after the
/// steal resolves, `[thief, loot, P1 wolf]` and the given spells in P0's hand.
fn thief_steals_loot(spells: &[HandSpell]) -> (GameRunner, [ObjectId; 3], Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wolf = scenario.add_vanilla(P1, 2, 2);
    let loot = scenario.add_artifact_from_oracle(P1, "Loot", "").id();
    let thief = scenario
        .add_creature_to_hand_from_oracle(P0, "Master Thief", 2, 2, MASTER_THIEF_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let spells = spells
        .iter()
        .map(|&(name, is_instant, keywords, oracle)| {
            scenario
                .add_spell_to_hand_from_oracle(P0, name, is_instant, oracle)
                .from_oracle_text_with_keywords(keywords, oracle)
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.cast(thief).target_object(loot).resolve();
    assert_eq!(runner.state().objects[&loot].controller, P0);
    assert!(matches!(
        runner.state().transient_continuous_effects[0].duration,
        engine::types::ability::Duration::WhileControllingHost
    ));
    (runner, [thief, loot, wolf], spells)
}

/// Replays every command journaled after `prefix`, settles, and requires the
/// replayed board to match the live one exactly.
fn assert_suffix_replays_exactly(prefix: GameState, live: &GameState, objects: &[ObjectId]) {
    let start = prefix.resolved_rules_journal.entries().len();
    let mut replay = prefix;
    for command in live
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(start)
        .filter_map(|entry| entry.command.as_ref())
    {
        apply_semantic_command(&mut replay, command);
    }
    engine::game::layers::evaluate_layers(&mut replay);
    for id in objects {
        assert_eq!(replay.objects[id].controller, live.objects[id].controller);
        assert_eq!(replay.objects[id].power, live.objects[id].power);
    }
    assert_eq!(
        replay.transient_continuous_effects,
        live.transient_continuous_effects
    );
}

/// CR 611.2b: losing control of Master Thief ends its steal for good, so Loot
/// stays with its owner after the Thief comes back. The ending is a settled
/// lapse with no command of its own, so exact replay must carry its receipt.
#[test]
fn master_thief_control_loss_replays_before_its_return_and_later_install() {
    let (mut runner, [thief, loot, wolf], spells) =
        thief_steals_loot(&[SWITCHEROO, ACT_OF_TREASON, GIANT_GROWTH]);
    let prefix = runner.state().clone();
    runner
        .cast(spells[0])
        .target_objects(&[thief, wolf])
        .resolve();
    assert_eq!(runner.state().objects[&thief].controller, P1);
    assert_eq!(runner.state().objects[&loot].controller, P1);
    runner.cast(spells[1]).target_object(thief).resolve();
    assert_eq!(runner.state().objects[&thief].controller, P0);
    assert_eq!(runner.state().objects[&loot].controller, P1);
    runner.cast(spells[2]).target_object(thief).resolve();
    assert_suffix_replays_exactly(prefix, runner.state(), &[thief, loot, wolf]);
}

/// CR 702.26f + CR 611.2b: phasing Master Thief out ends its steal, so Loot
/// returns to its owner and stays there; exact replay must agree.
#[test]
fn master_thief_phase_out_replays_before_later_install() {
    let (mut runner, [thief, loot, wolf], spells) =
        thief_steals_loot(&[CLEVER_CONCEALMENT, GIANT_GROWTH]);
    let prefix = runner.state().clone();
    runner.cast(spells[0]).target_object(thief).resolve();
    assert_eq!(runner.state().objects[&loot].controller, P1);
    runner.cast(spells[1]).target_object(wolf).resolve();
    assert_suffix_replays_exactly(prefix, runner.state(), &[thief, loot, wolf]);
}
