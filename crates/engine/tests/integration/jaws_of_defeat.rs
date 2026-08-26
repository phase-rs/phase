use engine::game::layers::evaluate_layers;
use engine::game::phasing::{phase_in_player, phase_out_player};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    ContinuousModification, Duration, StaticCondition, StaticDefinition, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const JAWS_ORACLE: &str = "Whenever a creature you control enters, target opponent loses life equal to the difference between that creature's power and its toughness.";

fn staged_entrant(
    scenario: &mut GameScenario,
    controller: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
    modification: ContinuousModification,
) -> ObjectId {
    let mut builder = scenario.add_creature_to_exile(controller, name, power, toughness);
    let id = builder.id();
    builder.with_static_definition(
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id })
            .modifications(vec![modification]),
    );
    id
}

fn enter_from_exile(runner: &mut GameRunner, entrant: ObjectId) -> Vec<GameEvent> {
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), entrant, Zone::Battlefield, &mut events);
    events
}

fn zone_changed_object(event: &GameEvent) -> Option<ObjectId> {
    match event {
        GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
        _ => None,
    }
}

fn pending_event_object(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .pending_trigger
        .as_ref()
        .and_then(|pending| pending.trigger_event.as_ref())
        .and_then(zone_changed_object)
        .expect("pending Jaws trigger must retain its ZoneChanged object identity")
}

fn surface_pending_trigger_target_selection(runner: &mut GameRunner) {
    if runner.state().pending_trigger.is_some()
        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    {
        runner
            .act(GameAction::PassPriority)
            .expect("production priority pipeline must surface Jaws target selection");
    }
}

fn choose_trigger_player(runner: &mut GameRunner, player: PlayerId) {
    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Jaws TriggerTargetSelection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Player(player)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(player)),
        })
        .expect("choose Jaws target opponent");
}

fn stack_jaws_bindings(runner: &GameRunner) -> Vec<(ObjectId, PlayerId)> {
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                ability,
                trigger_event: Some(event),
                ..
            } => {
                let object_id = zone_changed_object(event)?;
                let [TargetRef::Player(player)] = ability.targets.as_slice() else {
                    return None;
                };
                Some((object_id, *player))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn co_pending_entries_keep_event_identity_and_independent_life_amounts() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jaws of Defeat", 0, 1, JAWS_ORACLE)
        .as_enchantment();
    let entrant_a = staged_entrant(
        &mut scenario,
        P0,
        "Power Entrant",
        2,
        2,
        ContinuousModification::AddPower { value: 3 },
    );
    let entrant_b = staged_entrant(
        &mut scenario,
        P0,
        "Toughness Entrant",
        1,
        1,
        ContinuousModification::AddToughness { value: 5 },
    );
    let mut runner = scenario.build();

    let mut events = enter_from_exile(&mut runner, entrant_a);
    events.extend(enter_from_exile(&mut runner, entrant_b));
    process_triggers(runner.state_mut(), &events);

    assert_eq!(
        (
            runner.state().objects[&entrant_a].power,
            runner.state().objects[&entrant_a].toughness
        ),
        (Some(5), Some(2)),
        "CR 208.1: Jaws reads the entrant's effective power and toughness"
    );
    assert_eq!(
        (
            runner.state().objects[&entrant_b].power,
            runner.state().objects[&entrant_b].toughness
        ),
        (Some(1), Some(6)),
        "CR 208.1: the second entrant carries the opposite P/T skew"
    );

    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        panic!("two co-pending Jaws triggers must require ordering");
    };
    assert_eq!(triggers.len(), 2);
    let order = runner
        .state()
        .pending_trigger_order
        .as_ref()
        .expect("ordering state retains full trigger contexts");
    let mut event_ids: Vec<_> = order
        .groups
        .iter()
        .flat_map(|group| group.triggers.iter())
        .map(|context| {
            assert!(context.pending.ability.cost_paid_object.is_none());
            assert!(context.pending.ability.effect_context_object.is_none());
            context
                .pending
                .trigger_event
                .as_ref()
                .and_then(zone_changed_object)
                .expect("ordered trigger event identity")
        })
        .collect();
    event_ids.sort();
    let mut expected_ids = vec![entrant_a, entrant_b];
    expected_ids.sort();
    assert_eq!(event_ids, expected_ids);

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("order Jaws triggers");
    for _ in 0..2 {
        let event_object = pending_event_object(&runner);
        choose_trigger_player(&mut runner, if event_object == entrant_a { P1 } else { P2 });
    }

    let mut bindings = stack_jaws_bindings(&runner);
    bindings.sort();
    let mut expected_bindings = vec![(entrant_a, P1), (entrant_b, P2)];
    expected_bindings.sort();
    assert_eq!(
        bindings, expected_bindings,
        "CR 603.3d: each chosen player remains paired with its own trigger event"
    );

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 17);
    assert_eq!(runner.life(P2), 15);
}

#[test]
fn three_player_target_selection_stores_and_consumes_only_the_chosen_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jaws of Defeat", 0, 1, JAWS_ORACLE)
        .as_enchantment();
    let entrant = staged_entrant(
        &mut scenario,
        P0,
        "Power Entrant",
        2,
        2,
        ContinuousModification::AddPower { value: 3 },
    );
    let mut runner = scenario.build();
    let events = enter_from_exile(&mut runner, entrant);
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|object| object.name == "Jaws of Defeat")
            .map(|object| object.trigger_definitions.len())
            .sum::<usize>(),
        1,
        "positive reach guard: Jaws installs its parsed trigger"
    );
    assert!(
        events
            .iter()
            .any(|event| zone_changed_object(event) == Some(entrant)),
        "positive reach guard: production move emits entrant ZoneChanged; got {events:?}"
    );
    process_triggers(runner.state_mut(), &events);
    surface_pending_trigger_target_selection(&mut runner);

    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "Jaws must request an opponent target; waiting={:?}, stack={:?}, pending={:?}",
            runner.state().waiting_for,
            runner.state().stack,
            runner.state().pending_trigger
        );
    };
    let legal = &target_slots[0].legal_targets;
    assert!(legal.contains(&TargetRef::Player(P1)));
    assert!(legal.contains(&TargetRef::Player(P2)));
    assert!(!legal.contains(&TargetRef::Player(P0)));

    choose_trigger_player(&mut runner, P2);
    assert_eq!(stack_jaws_bindings(&runner), vec![(entrant, P2)]);
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 17);
}

#[test]
fn chosen_opponent_is_revalidated_if_they_phase_out_before_resolution() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jaws of Defeat", 0, 1, JAWS_ORACLE)
        .as_enchantment();
    let entrant = staged_entrant(
        &mut scenario,
        P0,
        "Power Entrant",
        2,
        2,
        ContinuousModification::AddPower { value: 3 },
    );
    let mut runner = scenario.build();
    let events = enter_from_exile(&mut runner, entrant);
    process_triggers(runner.state_mut(), &events);
    surface_pending_trigger_target_selection(&mut runner);
    choose_trigger_player(&mut runner, P2);
    let mut phase_events = Vec::new();
    phase_out_player(runner.state_mut(), P2, &mut phase_events);

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P2),
        20,
        "CR 608.2b: an opponent who phased out is no longer a legal target"
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_trigger.is_none());
    assert!(runner.state().current_trigger_event.is_none());
    assert!(runner.state().current_trigger_events.is_empty());
}

#[test]
fn entrant_pt_uses_lki_after_it_leaves_before_jaws_resolves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jaws of Defeat", 0, 1, JAWS_ORACLE)
        .as_enchantment();
    let entrant = staged_entrant(
        &mut scenario,
        P0,
        "Power Entrant",
        2,
        2,
        ContinuousModification::AddPower { value: 3 },
    );
    let mut runner = scenario.build();
    let events = enter_from_exile(&mut runner, entrant);
    process_triggers(runner.state_mut(), &events);
    surface_pending_trigger_target_selection(&mut runner);
    assert_eq!(
        (
            runner.state().objects[&entrant].power,
            runner.state().objects[&entrant].toughness
        ),
        (Some(5), Some(2))
    );
    let pending = runner.state().pending_trigger.as_ref().unwrap();
    assert!(pending.ability.cost_paid_object.is_none());
    assert!(pending.ability.effect_context_object.is_none());
    choose_trigger_player(&mut runner, P1);

    let mut leave_events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        entrant,
        Zone::Graveyard,
        &mut leave_events,
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P1),
        17,
        "CR 608.2k: the trigger keeps referring to the specific entrant after it leaves"
    );
}

#[test]
fn blinked_entrant_uses_original_incarnation_lki_not_reentered_live_pt() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jaws of Defeat", 0, 1, JAWS_ORACLE)
        .as_enchantment();
    let mut entrant_builder = scenario.add_creature_to_exile(P0, "Blink Entrant", 2, 2);
    let entrant = entrant_builder.id();
    entrant_builder.with_static_definition(
        StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: entrant })
            .modifications(vec![ContinuousModification::AddPower { value: 3 }])
            .condition(StaticCondition::SourceIsTapped),
    );
    let mut runner = scenario.build();

    let events = enter_from_exile(&mut runner, entrant);
    runner.state_mut().objects.get_mut(&entrant).unwrap().tapped = true;
    evaluate_layers(runner.state_mut());
    assert_eq!(
        (
            runner.state().objects[&entrant].power,
            runner.state().objects[&entrant].toughness
        ),
        (Some(5), Some(2)),
        "the original entrant must have a discriminating 5/2 LKI"
    );
    process_triggers(runner.state_mut(), &events);
    surface_pending_trigger_target_selection(&mut runner);
    choose_trigger_player(&mut runner, P1);

    let original_incarnation =
        match runner
            .state()
            .stack
            .back()
            .and_then(|entry| match &entry.kind {
                StackEntryKind::TriggeredAbility {
                    trigger_event: Some(GameEvent::ZoneChanged { record, .. }),
                    ..
                } => record.entered_incarnation,
                _ => None,
            }) {
            Some(incarnation) => incarnation,
            None => panic!("Jaws stack event must retain the entrant incarnation"),
        };

    let mut blink_events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        entrant,
        Zone::Graveyard,
        &mut blink_events,
    );
    move_to_zone(
        runner.state_mut(),
        entrant,
        Zone::Battlefield,
        &mut blink_events,
    );
    evaluate_layers(runner.state_mut());
    let reentered = &runner.state().objects[&entrant];
    assert_ne!(reentered.incarnation, original_incarnation);
    assert_eq!(
        (reentered.power, reentered.toughness),
        (Some(2), Some(2)),
        "the re-entered incarnation must be a hostile live 2/2"
    );

    move_to_zone(
        runner.state_mut(),
        entrant,
        Zone::Graveyard,
        &mut blink_events,
    );
    assert_eq!(
        (
            runner.state().lki_cache[&entrant].power,
            runner.state().lki_cache[&entrant].toughness
        ),
        (Some(2), Some(2)),
        "the second departure must overwrite the legacy ObjectId-only LKI"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P1),
        17,
        "CR 400.7 + CR 608.2h: Jaws must use the original entrant's 5/2 LKI, not the re-entered 2/2"
    );
}

#[test]
fn no_legal_opponent_drops_then_a_later_entrant_can_trigger_after_phase_in() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Jaws of Defeat", 0, 1, JAWS_ORACLE)
        .as_enchantment();
    let first = scenario
        .add_creature_to_exile(P0, "First Entrant", 5, 2)
        .id();
    let second = scenario
        .add_creature_to_exile(P0, "Second Entrant", 5, 2)
        .id();
    let mut runner = scenario.build();
    let mut phase_events = Vec::new();
    phase_out_player(runner.state_mut(), P1, &mut phase_events);

    let first_events = enter_from_exile(&mut runner, first);
    process_triggers(runner.state_mut(), &first_events);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_trigger.is_none());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));

    phase_in_player(runner.state_mut(), P1, &mut phase_events);
    let second_events = enter_from_exile(&mut runner, second);
    process_triggers(runner.state_mut(), &second_events);
    surface_pending_trigger_target_selection(&mut runner);
    assert_eq!(
        stack_jaws_bindings(&runner),
        vec![(second, P1)],
        "the only legal opponent is selected without an unnecessary prompt"
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 17);
}

#[test]
fn jaws_tracks_its_current_controller_when_deciding_which_entries_trigger() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let jaws = scenario
        .add_creature_from_oracle(P0, "Jaws of Defeat", 0, 1, JAWS_ORACLE)
        .as_enchantment()
        .id();
    let old_controllers_entrant = scenario
        .add_creature_to_exile(P0, "Former Controller Entrant", 5, 2)
        .id();
    let current_controllers_entrant = scenario
        .add_creature_to_exile(P1, "Current Controller Entrant", 5, 2)
        .id();
    let mut runner = scenario.build();

    // CR 611.2c + CR 613.1b: a durable control-changing effect fixes its affected
    // object when created and is applied in layer 2.
    runner.state_mut().add_transient_continuous_effect(
        jaws,
        P1,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: jaws },
        vec![ContinuousModification::ChangeController],
        None,
    );
    evaluate_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&jaws].controller, P1);

    let old_controller_events = enter_from_exile(&mut runner, old_controllers_entrant);
    process_triggers(runner.state_mut(), &old_controller_events);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_trigger.is_none());

    let current_controller_events = enter_from_exile(&mut runner, current_controllers_entrant);
    process_triggers(runner.state_mut(), &current_controller_events);
    surface_pending_trigger_target_selection(&mut runner);
    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!("the stolen Jaws must trigger for its current controller's entrant");
    };
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Player(P0)));
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Player(P2)));
    assert!(!target_slots[0]
        .legal_targets
        .contains(&TargetRef::Player(P1)));

    choose_trigger_player(&mut runner, P2);
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 17);
}
