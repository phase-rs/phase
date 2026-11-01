//! Engine-generated targets retain the instruction that declared each slot.
//! The typed trigger is an internal contract fixture, not a printed card.

use engine::game::ability_utils::{
    auto_select_targets_for_ability, begin_target_selection_for_ability, build_resolved_from_def,
    build_target_slots, choose_target_for_ability, has_legal_target_assignment_for_ability,
    TargetSelectionAdvance,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, MultiTargetSpec, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, TargetSelectionConstraint, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

fn generated_ability(root: TargetFilter, child: TargetFilter) -> AbilityDefinition {
    // CR 115.6 + CR 603.3d: the paired root permits zero chosen targets,
    // while the child independently requires a legal target on stack entry.
    AbilityDefinition::new(
        AbilityKind::Database,
        Effect::ExchangeControl {
            target_a: TargetFilter::SelfRef,
            target_b: root,
        },
    )
    .multi_target(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }))
    .target_constraint(TargetSelectionConstraint::DifferentObjectControllers)
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Database,
        Effect::Destroy {
            target: child,
            cant_regenerate: false,
        },
    ))
}

fn phase_trigger(ability: AbilityDefinition) -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::BeginCombat)
        .execute(ability)
}

fn assert_source_trigger_installed(runner: &GameRunner, source: ObjectId) {
    let object = runner.state().objects.get(&source).expect("trigger source");
    assert_eq!(object.zone, Zone::Battlefield);
    assert!(object.base_trigger_definitions.iter().any(|definition| {
        definition.mode == TriggerMode::Phase
            && definition.phase == Some(Phase::BeginCombat)
            && definition.execute.is_some()
    }));
}

fn source_trigger_on_stack(runner: &GameRunner, source: ObjectId) -> Option<&ResolvedAbility> {
    runner.state().stack.iter().find_map(|entry| {
        if entry.source_id != source
            || !matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility {
                    trigger_event: Some(GameEvent::PhaseChanged {
                        phase: Phase::BeginCombat
                    }),
                    ..
                }
            )
        {
            return None;
        }
        entry.ability()
    })
}

fn assert_auto_staged_child(runner: &GameRunner, source: ObjectId, child: ObjectId) {
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_source_trigger_installed(runner, source);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the unique legal assignment must not request a target prompt"
    );
    assert!(runner.state().pending_trigger_entry.is_none());
    let ability = source_trigger_on_stack(runner, source)
        .expect("the phase event must put the source's trigger on the real stack");
    assert!(ability.targets.is_empty(), "the optional root was declined");
    assert_eq!(
        ability
            .sub_ability
            .as_deref()
            .expect("required child")
            .targets,
        vec![TargetRef::Object(child)],
        "the required child owns its selected target"
    );
}

#[test]
fn overlapping_filters_auto_assign_only_the_required_child() {
    let opponent_creature =
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
    let ability = generated_ability(opponent_creature.clone(), opponent_creature);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Generated Slot Source", 2, 2)
        .with_trigger_definition(phase_trigger(ability.clone()))
        .id();
    let victim = scenario
        .add_creature(P1, "Only Opponent Creature", 2, 2)
        .id();
    let mut runner = scenario.build();

    let resolved = build_resolved_from_def(&ability, source, P0);
    let slots = build_target_slots(runner.state(), &resolved).expect("declared target slots");
    assert_eq!(slots.len(), 2, "root and child each declare one slot");
    assert!(slots[0].optional);
    assert!(!slots[1].optional);
    assert_eq!(slots[0].legal_targets, vec![TargetRef::Object(victim)]);
    assert_eq!(slots[1].legal_targets, vec![TargetRef::Object(victim)]);
    assert!(has_legal_target_assignment_for_ability(
        runner.state(),
        &resolved,
        &slots,
        &resolved.target_constraints,
    ));
    assert!(
        auto_select_targets_for_ability(
            runner.state(),
            &resolved,
            &slots,
            &resolved.target_constraints,
        )
        .expect("generator has a legal completion")
        .is_some(),
        "declining the root and choosing the child is the unique completion"
    );

    runner.advance_to_phase(Phase::BeginCombat);
    assert_auto_staged_child(&runner, source, victim);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
}

#[test]
fn distinct_filter_pools_auto_assign_only_the_required_child() {
    let root = TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::Opponent)
            .subtype("Scout".to_string()),
    );
    let child = TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::Opponent)
            .subtype("Knight".to_string()),
    );
    let ability = generated_ability(root, child);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Generated Slot Source", 2, 2)
        .with_trigger_definition(phase_trigger(ability.clone()))
        .id();
    let scout = scenario
        .add_creature(P1, "Optional Scout", 2, 2)
        .with_subtypes(vec!["Scout"])
        .id();
    let knight = scenario
        .add_creature(P1, "Required Knight", 2, 2)
        .with_subtypes(vec!["Knight"])
        .id();
    let mut runner = scenario.build();

    let resolved = build_resolved_from_def(&ability, source, P0);
    let slots = build_target_slots(runner.state(), &resolved).expect("declared target slots");
    assert_eq!(slots.len(), 2);
    assert!(slots[0].optional);
    assert!(!slots[1].optional);
    assert_eq!(slots[0].legal_targets, vec![TargetRef::Object(scout)]);
    assert_eq!(slots[1].legal_targets, vec![TargetRef::Object(knight)]);
    assert!(has_legal_target_assignment_for_ability(
        runner.state(),
        &resolved,
        &slots,
        &resolved.target_constraints,
    ));

    let progress = begin_target_selection_for_ability(
        runner.state(),
        &resolved,
        &slots,
        &resolved.target_constraints,
    )
    .expect("interactive slot authority starts");
    assert_eq!(
        runner.state().objects[&scout].controller,
        runner.state().objects[&knight].controller,
        "[Scout, Knight] violates DifferentObjectControllers"
    );
    // The selection authority eliminates [Scout, Knight] and automatically
    // advances past the now-empty optional root to the required child.
    assert_eq!(progress.current_slot, 1);
    assert_eq!(progress.selected_slots, vec![None]);
    assert!(
        !progress
            .current_legal_targets
            .contains(&TargetRef::Object(scout)),
        "choosing Scout would leave no legal completion for the required child"
    );
    assert_eq!(
        progress.current_legal_targets,
        vec![TargetRef::Object(knight)]
    );
    let TargetSelectionAdvance::Complete(selected) = choose_target_for_ability(
        runner.state(),
        &resolved,
        &slots,
        &resolved.target_constraints,
        &progress,
        Some(TargetRef::Object(knight)),
    )
    .expect("required child accepts Knight") else {
        panic!("two declared slots must complete selection");
    };
    assert_eq!(selected, vec![None, Some(TargetRef::Object(knight))]);

    runner.advance_to_phase(Phase::BeginCombat);
    assert_auto_staged_child(&runner, source, knight);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&knight].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&scout].zone, Zone::Battlefield);
}

#[test]
fn no_legal_required_child_removes_trigger_after_phase_event() {
    let root = TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
    let child = TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::Opponent)
            .subtype("Knight".to_string()),
    );
    let ability = generated_ability(root, child);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Generated Slot Source", 2, 2)
        .with_trigger_definition(phase_trigger(ability.clone()))
        .id();
    let witness = scenario
        .add_creature(P1, "Phase Event Witness", 1, 1)
        .with_trigger_definition(phase_trigger(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::NoOp,
        )))
        .id();
    let mut runner = scenario.build();
    let resolved = build_resolved_from_def(&ability, source, P0);
    assert!(
        build_target_slots(runner.state(), &resolved).is_err(),
        "the required Knight slot has no legal target despite the optional root's witness"
    );

    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_source_trigger_installed(&runner, source);
    assert!(
        source_trigger_on_stack(&runner, witness).is_some(),
        "the separate witness trigger proves the phase event reached real dispatch"
    );
    assert!(source_trigger_on_stack(&runner, source).is_none());
    assert!(runner.state().pending_trigger_entry.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
