//! CR 400.7 + CR 608.2b regression coverage for selected object targets.

use engine::game::effects::change_targets;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    Effect, ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastingVariant, RetargetScope, RetargetSlotAddress, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn selected_target_does_not_follow_object_id_after_zone_change_and_return() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Strip Mine Probe",
            1,
            1,
            "{T}: Destroy target creature.",
        )
        .id();
    let target = scenario.add_creature(P0, "Target Permanent", 2, 2).id();
    let blink = scenario
        .add_creature_from_oracle(
            P0,
            "Blink Probe",
            1,
            1,
            "{T}: Exile target creature you control, then return it to the battlefield under its owner's control.",
        )
        .id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activation announcement must be accepted");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(target)),
        })
        .expect("target selection must be accepted");
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.selected_target_incarnations.len()),
        Some(1),
        "the announced target must be pinned on the stack entry"
    );
    let announced_incarnation = runner.state().objects[&target].incarnation;
    runner
        .act(GameAction::ActivateAbility {
            source_id: blink,
            ability_index: 0,
        })
        .expect("the intervening blink activation must be accepted");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(target)),
        })
        .expect("the intervening blink target must be accepted");
    runner
        .act(GameAction::PassPriority)
        .expect("controller priority pass must be accepted for the blink");
    runner
        .act(GameAction::PassPriority)
        .expect("opponent priority pass must resolve the blink");
    assert_ne!(
        runner.state().objects[&target].incarnation,
        announced_incarnation,
        "the production zone-change pipeline must create a new object on return"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("controller priority pass must be accepted");
    runner
        .act(GameAction::PassPriority)
        .expect("opponent priority pass must resolve the ability");
    assert!(
        runner.state().battlefield.contains(&target),
        "CR 608.2b must not destroy the returned new object"
    );
}

fn stale_target_stack() -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Retarget Target", 2, 2).id();
    let mut runner = scenario.build();
    let source = create_object(
        runner.state_mut(),
        CardId(88),
        P0,
        "Retarget Probe".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("retarget source")
        .card_types
        .core_types = vec![CoreType::Instant];
    let creature_target_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: None,
        properties: vec![],
    });
    let mut target_ability = ResolvedAbility::new(
        Effect::Destroy {
            target: creature_target_filter.clone(),
            cant_regenerate: false,
        },
        vec![TargetRef::Object(target)],
        source,
        P0,
    );
    target_ability.capture_target_incarnations_recursive(runner.state());
    runner.state_mut().stack.push_back(StackEntry {
        id: source,
        source_id: source,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(88),
            ability: Some(Box::new(target_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), target, Zone::Graveyard, &mut events);
    move_to_zone(runner.state_mut(), target, Zone::Battlefield, &mut events);
    assert!(
        !runner
            .state()
            .stack
            .back()
            .and_then(|entry| entry.ability())
            .is_some_and(|ability| ability.selected_target_pin_is_current(target, runner.state())),
        "the round-trip must stale the original selected-target pin"
    );
    (runner, target)
}

#[test]
fn interactive_same_object_id_retarget_refreshes_selected_target_pin() {
    let (mut runner, target) = stale_target_stack();
    // CR 115.7d, INVARIANT SC (phase-rs/phase#8355 round-8 review finding H2):
    // production-shaped, not the outer-empty `#[serde(default)]` compatibility
    // shape (N16) — a real prompt always stores `slots`/`slot_pools` for the
    // position it addresses, and this row exists to exercise THAT path, not
    // the compatibility fallback (which routes through a different branch in
    // `apply_retarget` and would leave this row unable to detect H2's defect).
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: vec![TargetRef::Object(target)],
        slots: vec![RetargetSlotAddress {
            path: vec![],
            slot: 0,
        }],
        slot_pools: vec![vec![TargetRef::Object(target)]],
        legal_new_targets: vec![TargetRef::Object(target)],
    };

    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Object(target)],
        })
        .expect("same-ID retarget must be accepted");

    let ability = runner.state().stack[0].ability().expect("ability on stack");
    assert!(
        ability.selected_target_pin_is_current(target, runner.state()),
        "interactive same-ID retarget must refresh the selected-target pin"
    );
}

#[test]
fn forced_same_object_id_retarget_refreshes_selected_target_pin() {
    let (mut runner, target) = stale_target_stack();
    let creature_target_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Creature],
        controller: None,
        properties: vec![],
    });
    let stack_entry_id = runner.state().stack[0].id;
    let retarget_id = create_object(
        runner.state_mut(),
        CardId(99),
        P0,
        "Forced Retarget Probe".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&retarget_id)
        .expect("retarget source")
        .card_types
        .core_types = vec![CoreType::Instant];
    let retarget_ability = ResolvedAbility::new(
        Effect::ChangeTargets {
            target: TargetFilter::StackSpell,
            scope: RetargetScope::Single,
            forced_to: Some(creature_target_filter.clone()),
        },
        vec![TargetRef::Object(stack_entry_id)],
        retarget_id,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: retarget_id,
        source_id: retarget_id,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(99),
            ability: Some(Box::new(retarget_ability.clone())),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    assert!(
        engine::game::effects::change_targets::legal_new_targets_for_stack_entry(
            runner.state(),
            0,
        )
        .contains(&TargetRef::Object(target)),
        "the original target must remain legal for the forced retarget"
    );
    let forced_candidates = engine::game::targeting::find_legal_targets(
        runner.state(),
        &creature_target_filter,
        P0,
        retarget_id,
    );
    assert_eq!(
        forced_candidates,
        vec![TargetRef::Object(target)],
        "the forced candidate filter must select only the returned object"
    );
    let mut events = Vec::new();
    change_targets::resolve(runner.state_mut(), &retarget_ability, &mut events)
        .expect("forced retarget must resolve");

    let ability = runner.state().stack[0].ability().expect("ability on stack");
    assert!(
        ability.selected_target_pin_is_current(target, runner.state()),
        "forced same-ID retarget must refresh the selected-target pin"
    );
}
