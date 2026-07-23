//! CR733 P2 coverage for journaled trigger/LKI collection appends.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::{
    apply_resolved_trigger_collection, resolve_and_apply_trigger_collection,
    ConsumedTriggerEventOccurrence, PendingTrigger, PendingTriggerContext,
    PendingTriggerDispatchOrigin,
};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedCommandOrdinal, ResolvedRulesCommand, ResolvedTriggerCollection,
    ResolvedTriggerCollectionCommand, RulesExecutionNodeRef,
};
use engine::types::zones::Zone;

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
