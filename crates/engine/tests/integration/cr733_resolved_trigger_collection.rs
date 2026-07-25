//! CR733 P2 coverage for journaled trigger/LKI collection appends.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::{
    apply_resolved_trigger_collection, resolve_and_apply_trigger_collection,
    ConsumedTriggerEventOccurrence, PendingTrigger, PendingTriggerContext,
    PendingTriggerDispatchOrigin,
};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    QuantityExpr, ReplacementDefinition, ResolvedAbility, SacrificeCost, TargetFilter, TargetRef,
    TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolved_commands::{
    ResolvedCommandOrdinal, ResolvedRulesCommand, ResolvedTriggerCollection,
    ResolvedTriggerCollectionCommand, RulesExecutionNodeRef,
};
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};

const DIES_OBSERVER: &str = "Whenever another creature dies, you gain 1 life.";

fn cause() -> RulesExecutionNodeRef {
    RulesExecutionNodeRef::Proposal(ResolvedCommandOrdinal(0))
}

fn pending_context(source_id: ObjectId) -> PendingTriggerContext {
    PendingTriggerContext {
        pending: PendingTrigger {
            source_id,
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                source_id,
                PlayerId(0),
            ),
            timestamp: 0,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: Vec::new(),
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        },
        trigger_events: Vec::new(),
        dispatch_origin: PendingTriggerDispatchOrigin::Normal,
    }
}

fn occurrence(player: PlayerId) -> ConsumedTriggerEventOccurrence {
    ConsumedTriggerEventOccurrence {
        event: GameEvent::PlayerPerformedAction {
            player_id: player,
            action: PlayerActionKind::Scry,
        },
        occurrence: 0,
    }
}

fn command(collection: ResolvedTriggerCollection) -> ResolvedTriggerCollectionCommand {
    ResolvedTriggerCollectionCommand {
        collection,
        cause: cause(),
    }
}

fn recorded_commands(state: &GameState) -> Vec<ResolvedRulesCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.clone())
        .collect()
}

fn current_action_occurrences(events: &[GameEvent]) -> Vec<ConsumedTriggerEventOccurrence> {
    events
        .iter()
        .enumerate()
        .map(|(index, event)| ConsumedTriggerEventOccurrence {
            event: event.clone(),
            occurrence: events[..index]
                .iter()
                .filter(|prior| *prior == event)
                .count(),
        })
        .collect()
}

fn redirect_self_moved_to(destination: Zone, redirected_to: Zone) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(destination)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: redirected_to,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

fn sacrifice_observer() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Sacrificed)
        .valid_card(TargetFilter::Any)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ))
}

fn artifact_sacrifice_observer() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Sacrificed)
        .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)))
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ))
}

fn run_trigger_collection_fixture(fixture: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        // Journaled zone-change LKI exceeds the default 2 MiB test-thread stack.
        .stack_size(4 * 1024 * 1024)
        .spawn(fixture)
        .expect("trigger collection fixture thread starts")
        .join()
        .expect("trigger collection fixture completes")
}

fn assert_current_action_consumed_occurrences_were_journaled(
    state: &GameState,
    events: &[GameEvent],
) {
    let expected = current_action_occurrences(events);
    assert!(
        state
            .resolved_rules_journal
            .entries()
            .iter()
            .any(|entry| matches!(
                entry.command.as_ref(),
                Some(ResolvedRulesCommand::TriggerCollection(ResolvedTriggerCollectionCommand {
                    collection: ResolvedTriggerCollection::ConsumeBeforePriority { occurrences },
                    ..
                })) if occurrences == &expected
            )),
        "the production cost settlement must journal its exact current-action consumed occurrences"
    );
}

#[test]
fn trigger_collection_command_variants_round_trip_inside_the_journal_envelope() {
    for command in [
        ResolvedRulesCommand::TriggerCollection(command(ResolvedTriggerCollection::DeferPending {
            contexts: vec![pending_context(ObjectId(11))],
        })),
        ResolvedRulesCommand::TriggerCollection(command(
            ResolvedTriggerCollection::ConsumeBeforePriority {
                occurrences: vec![occurrence(PlayerId(1))],
            },
        )),
    ] {
        let wire = serde_json::to_value(&command).expect("command serializes");
        assert_eq!(
            serde_json::from_value::<ResolvedRulesCommand>(wire).expect("command deserializes"),
            command
        );
    }
}

#[test]
fn apply_preserves_the_exact_append_order_for_both_trigger_collections() {
    let first = pending_context(ObjectId(12));
    let second = pending_context(ObjectId(13));
    let first_occurrence = occurrence(PlayerId(0));
    let second_occurrence = occurrence(PlayerId(1));
    let mut state = GameState::new_two_player(0x733);

    apply_resolved_trigger_collection(
        &mut state,
        &command(ResolvedTriggerCollection::DeferPending {
            contexts: vec![first.clone(), second.clone()],
        }),
    )
    .expect("append-only pending contexts apply");
    apply_resolved_trigger_collection(
        &mut state,
        &command(ResolvedTriggerCollection::ConsumeBeforePriority {
            occurrences: vec![first_occurrence.clone(), second_occurrence.clone()],
        }),
    )
    .expect("append-only consumed occurrences apply");

    assert_eq!(state.deferred_triggers, vec![first, second]);
    assert_eq!(
        state.consumed_before_priority_trigger_events,
        vec![first_occurrence, second_occurrence]
    );
}

#[test]
fn empty_trigger_collections_are_not_applied_or_journaled() {
    let mut state = GameState::new_two_player(0x733);

    resolve_and_apply_trigger_collection(
        &mut state,
        ResolvedTriggerCollection::DeferPending {
            contexts: Vec::new(),
        },
    )
    .expect("empty pending collection is a no-op");
    resolve_and_apply_trigger_collection(
        &mut state,
        ResolvedTriggerCollection::ConsumeBeforePriority {
            occurrences: Vec::new(),
        },
    )
    .expect("empty consumed collection is a no-op");

    assert!(state.deferred_triggers.is_empty());
    assert!(state.consumed_before_priority_trigger_events.is_empty());
    assert!(state.resolved_rules_journal.entries().is_empty());
    assert!(state.resolved_rules_journal.nodes().is_empty());
}

#[test]
fn resolved_trigger_collection_records_under_its_live_journal_cause() {
    let mut state = GameState::new_two_player(0x733);
    resolve_and_apply_trigger_collection(
        &mut state,
        ResolvedTriggerCollection::DeferPending {
            contexts: vec![pending_context(ObjectId(14))],
        },
    )
    .expect("a live cause records the trigger collection");

    let entry = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::TriggerCollection(command)) => Some((entry, command)),
            _ => None,
        })
        .expect("non-empty collection has a journal command");
    assert_eq!(entry.0.node, entry.1.cause);
}

#[test]
fn real_change_zone_resolver_journals_its_collected_trigger_occurrences() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let observer = scenario
        .add_creature_from_oracle(P0, "Death observer", 2, 2, DIES_OBSERVER)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let mut runner = scenario.build();

    let ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Graveyard,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: Vec::new(),
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![TargetRef::Object(victim)],
        observer,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::change_zone::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the production ChangeZone resolver moves its selected creature");
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);

    let commands = recorded_commands(runner.state());
    assert!(
        commands.iter().any(|command| matches!(
            command,
            ResolvedRulesCommand::TriggerCollection(ResolvedTriggerCollectionCommand {
                collection: ResolvedTriggerCollection::DeferPending { contexts },
                ..
            }) if contexts.iter().any(|context| context.pending.source_id == observer)
        )),
        "logical-zone segment collection must journal the observer context"
    );
}

#[test]
fn paused_mana_cost_settlement_journals_consumed_occurrences_without_recollecting() {
    run_trigger_collection_fixture(
        paused_mana_cost_settlement_journals_consumed_occurrences_without_recollecting_impl,
    );
}

fn paused_mana_cost_settlement_journals_consumed_occurrences_without_recollecting_impl() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let observer = scenario
        .add_creature(P0, "Mana cost sacrifice observer", 1, 1)
        .with_trigger_definition(sacrifice_observer())
        .id();
    let source = scenario
        .add_creature(P0, "Self-sacrifice mana replacement witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: Vec::new(),
                    grants: Vec::new(),
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            }),
        )
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let mut runner = scenario.build();

    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the mana cost reaches its real replacement choice after its tap prefix");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice resumes the mana cost settlement");
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(resumed.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, .. } if *object_id == source
    )));
    assert_current_action_consumed_occurrences_were_journaled(runner.state(), &resumed.events);
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == observer
            ))
            .count(),
        1,
        "the settled mana-cost zone change must not be collected again at priority"
    );
}

#[test]
fn paused_sacrifice_cost_settlement_journals_consumed_occurrences_without_recollecting() {
    run_trigger_collection_fixture(
        paused_sacrifice_cost_settlement_journals_consumed_occurrences_without_recollecting_impl,
    );
}

fn paused_sacrifice_cost_settlement_journals_consumed_occurrences_without_recollecting_impl() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let observer = scenario
        .add_creature(P0, "Sacrifice cost artifact observer", 1, 1)
        .with_trigger_definition(artifact_sacrifice_observer())
        .id();
    let source = scenario
        .add_creature(P0, "Count-two sacrifice activation witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                2,
            ))),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First sacrifice-cost witness", 1, 1)
        .id();
    let second = scenario
        .add_creature(P0, "Second sacrifice-cost replacement witness", 1, 1)
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
        .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
        .id();
    let mut runner = scenario.build();
    let second_object = runner
        .state_mut()
        .objects
        .get_mut(&second)
        .expect("the second sacrifice witness exists");
    second_object.card_types.core_types.push(CoreType::Artifact);
    second_object.base_card_types = second_object.card_types.clone();

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the sacrifice-cost activation begins");
    let paused = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("the second sacrifice reaches its real replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice resumes the sacrifice cost settlement");
    assert_eq!(runner.state().objects[&second].zone, Zone::Exile);
    assert!(resumed.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, .. } if *object_id == second
    )));
    let current_end = resumed
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::PermanentSacrificed { object_id, .. } if *object_id == second
            )
        })
        .expect("the settled second sacrifice emits its terminal sacrifice event")
        + 1;
    assert_current_action_consumed_occurrences_were_journaled(
        runner.state(),
        &resumed.events[..current_end],
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == observer
            ))
            .count(),
        1,
        "the second settled sacrifice must not be collected again at priority"
    );
}
