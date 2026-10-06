//! Source-identified chosen-target groups, exercised through cast and resolution.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Effect, EffectKind, ResolvedAbility, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Constructed grammar witness: the later instruction names a chosen target,
// rather than all currently eligible creatures.
const CHOSEN_RETURN: &str =
    "Choose target creature you own. Return each chosen creature to your hand.";
const KICKED_CHOSEN_RETURN: &str = "Kicker {0} (You may pay an additional {0} as you cast this spell.)\nChoose target creature you own. If this spell was kicked, instead choose any number of target creatures you own. Return each chosen creature to your hand.";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const ROLE_A: &str = "Choose target creature you own. Choose target creature of an opponent's choice. Return each creature you chose to its owner's hand.";
const ROLE_B: &str = "Choose target creature of an opponent's choice. Choose target creature you own. Return each creature you chose to its owner's hand.";
const ROLE_C: &str = "Choose target creature you own. Choose target creature of an opponent's choice. Return each creature that opponent chose to its owner's hand.";
const AMBIGUOUS: &str = "Choose target creature you own. Choose target creature of an opponent's choice. Return each chosen creature to its owner's hand.";
const POPULATION: &str = "Return each creature you own to your hand.";
const ZADA: &str = "Whenever you cast an instant or sorcery spell that targets only Zada, copy that spell for each other creature you control that the spell could target. Each copy targets a different one of those creatures.";

fn chosen_group_nodes(root: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![root];
    let mut cursor = root;
    while let Some(next) = cursor.sub_ability.as_deref() {
        nodes.push(next);
        cursor = next;
    }
    nodes
}

#[test]
fn chosen_return_binds_by_role_in_both_source_orders() {
    for (text, producer_index) in [(ROLE_A, 0), (ROLE_B, 1), (ROLE_C, 1)] {
        let parsed = parse_oracle_text(text, "Role Witness", &[], &["Instant".into()], &[]);
        let nodes = chosen_group_nodes(&parsed.abilities[0]);
        assert_eq!(nodes.len(), 3, "{text}: {nodes:#?}");
        let chosen = nodes[producer_index]
            .declares_chosen_group
            .expect("named producer must declare a group");
        assert_ne!(
            nodes[0].declares_chosen_group,
            nodes[1].declares_chosen_group
        );
        assert_eq!(
            nodes[2].reads_chosen_group,
            Some(chosen),
            "{text}: {nodes:#?}"
        );
    }
}

#[test]
fn ambiguous_bare_chosen_return_strictly_fails() {
    let parsed = parse_oracle_text(
        AMBIGUOUS,
        "Ambiguous Witness",
        &[],
        &["Instant".into()],
        &[],
    );
    let nodes = chosen_group_nodes(&parsed.abilities[0]);
    assert_eq!(nodes.len(), 3);
    assert!(nodes[0].declares_chosen_group.is_some());
    assert!(nodes[1].declares_chosen_group.is_some());
    assert!(matches!(&*nodes[2].effect, Effect::Unimplemented { .. }));
}

#[test]
fn missing_or_conflicting_chosen_producer_fails_without_changing_population_return() {
    for text in [
        "Return each chosen creature to your hand.",
        "Choose target artifact you own. Return each chosen creature to your hand.",
    ] {
        let parsed = parse_oracle_text(text, "Unbound Witness", &[], &["Instant".into()], &[]);
        let nodes = chosen_group_nodes(&parsed.abilities[0]);
        let reader = nodes.last().expect("return instruction");
        assert!(
            matches!(&*reader.effect, Effect::Unimplemented { .. }),
            "{text}"
        );
        assert!(reader.reads_chosen_group.is_none());
    }

    let parsed = parse_oracle_text(
        POPULATION,
        "Population Witness",
        &[],
        &["Instant".into()],
        &[],
    );
    let root = &parsed.abilities[0];
    assert!(matches!(&*root.effect, Effect::BounceAll { .. }));
    assert!(root.reads_chosen_group.is_none());
}

#[test]
fn population_return_still_moves_every_eligible_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let opponent_owned = scenario
        .add_creature(P1, "Borrowed Bear", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Population Return", true, POPULATION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[first, second], Zone::Hand);
    outcome.assert_zone(&[opponent_owned], Zone::Battlefield);
}

fn erase_chosen_group_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            fields.remove("declares_chosen_group");
            fields.remove("reads_chosen_group");
            for field in fields.values_mut() {
                erase_chosen_group_fields(field);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                erase_chosen_group_fields(item);
            }
        }
        _ => {}
    }
}

#[test]
fn chosen_group_fields_roundtrip_and_old_json_defaults() {
    let parsed = parse_oracle_text(
        CHOSEN_RETURN,
        "Chosen Return Witness",
        &[],
        &["Instant".into()],
        &[],
    );
    let definition = &parsed.abilities[0];
    assert!(definition.declares_chosen_group.is_some());
    assert_eq!(
        definition
            .sub_ability
            .as_ref()
            .and_then(|node| node.reads_chosen_group),
        definition.declares_chosen_group
    );
    let json = serde_json::to_value(definition).expect("serialize definition");
    let restored: AbilityDefinition =
        serde_json::from_value(json.clone()).expect("restore definition");
    assert_eq!(&restored, definition);
    let mut old_json = json;
    erase_chosen_group_fields(&mut old_json);
    let old: AbilityDefinition = serde_json::from_value(old_json).expect("restore old definition");
    assert!(old.declares_chosen_group.is_none());
    assert!(old
        .sub_ability
        .as_ref()
        .is_some_and(|node| node.reads_chosen_group.is_none()));

    let resolved = build_resolved_from_def_with_targets(definition, ObjectId(7), P0, Vec::new());
    let json = serde_json::to_value(&resolved).expect("serialize resolved ability");
    let restored: ResolvedAbility =
        serde_json::from_value(json.clone()).expect("restore resolved ability");
    assert_eq!(restored, resolved);
    let mut old_json = json;
    erase_chosen_group_fields(&mut old_json);
    let old: ResolvedAbility =
        serde_json::from_value(old_json).expect("restore old resolved ability");
    assert!(old.declares_chosen_group.is_none());
    assert!(old
        .sub_ability
        .as_ref()
        .is_some_and(|node| node.reads_chosen_group.is_none()));
}

#[test]
fn role_named_return_moves_only_the_named_creature() {
    for text in [ROLE_A, ROLE_B, ROLE_C] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let own_choice = scenario.add_creature(P0, "Own Choice", 2, 2).id();
        let opponent_choice = scenario.add_creature(P0, "Opponent Choice", 2, 2).id();
        let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Role Witness", true, text)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let targets = if text == ROLE_A || text == ROLE_C {
            [own_choice, opponent_choice]
        } else {
            [opponent_choice, own_choice]
        };
        let outcome = runner.cast(spell).target_objects(&targets).resolve();
        if text == ROLE_C {
            outcome.assert_zone(&[opponent_choice], Zone::Hand);
            outcome.assert_zone(&[own_choice, unselected], Zone::Battlefield);
        } else {
            outcome.assert_zone(&[own_choice], Zone::Hand);
            outcome.assert_zone(&[opponent_choice, unselected], Zone::Battlefield);
        }
    }
}

// Verbatim Oracle text from the local MTGJSON export (HOB).
const EAGLES: &str = "Kicker {2}{W}{W} (You may pay an additional {2}{W}{W} as you cast this spell.)\nChoose target creature you own. If this spell was kicked, instead choose any number of target creatures you own. Return each chosen creature to your hand. At the beginning of the next upkeep, create a 4/4 white Bird Soldier creature token with flying for each creature returned to your hand this way.";

#[test]
fn eagles_selected_return_binds_its_delayed_result() {
    let parsed = parse_oracle_text(
        EAGLES,
        "The Eagles Are Coming!",
        &[],
        &["Instant".into()],
        &[],
    );
    let root = &parsed.abilities[0];
    let paid = root.sub_ability.as_deref().expect("kicker instead branch");
    let selected_return = paid.sub_ability.as_deref().expect("selected return");
    let delayed = selected_return
        .sub_ability
        .as_deref()
        .expect("delayed instruction");

    assert_eq!(root.declares_chosen_group, paid.declares_chosen_group);
    assert_eq!(
        selected_return.reads_chosen_group,
        root.declares_chosen_group
    );
    assert!(matches!(
        paid.condition,
        Some(AbilityCondition::AdditionalCostPaidInstead)
    ));
    assert!(matches!(&*selected_return.effect, Effect::BounceAll { .. }));
    assert!(matches!(
        &*delayed.effect,
        Effect::CreateDelayedTrigger { .. }
    ));
    assert_eq!(
        selected_return.declares_return_result,
        delayed.reads_return_result.as_ref().map(|(id, _)| *id)
    );
    assert!(selected_return.declares_return_result.is_some());
}

#[test]
fn eagles_unpaid_return_reaches_hand_and_arms_the_delayed_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selected = scenario.add_creature(P0, "Selected Bear", 2, 2).id();
    let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "The Eagles Are Coming!", true, EAGLES)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(selected).resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == selected
    )));
    outcome.assert_zone(&[selected], Zone::Hand);
    outcome.assert_zone(&[unselected], Zone::Battlefield);
    assert_eq!(outcome.state().delayed_triggers.len(), 1);
}

#[test]
fn selected_return_moves_only_the_announced_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let selected = scenario.add_creature(P0, "Selected Bear", 2, 2).id();
    let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let opponent_owned = scenario
        .add_creature(P1, "Borrowed Bear", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chosen Return Witness", true, CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(selected).resolve();

    // CR 608.2c: this positive move proves the selected-return instruction ran.
    outcome.assert_zone(&[selected], Zone::Hand);
    outcome.assert_zone(&[unselected, opponent_owned], Zone::Battlefield);
}

#[test]
fn chosen_return_uses_ownership_for_targeting_and_hand_destination() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let owned_by_caster = scenario
        .add_creature(P0, "Borrowed by Opponent", 2, 2)
        .controlled_by(P1)
        .id();
    let controlled_by_caster = scenario
        .add_creature(P1, "Borrowed by Caster", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chosen Return Witness", true, CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(owned_by_caster).resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == owned_by_caster
    )));
    outcome.assert_zone(&[owned_by_caster], Zone::Hand);
    outcome.assert_zone(&[controlled_by_caster], Zone::Battlefield);
}

#[test]
fn invalidated_paid_target_is_omitted_while_legal_member_returns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let legal = scenario.add_creature(P0, "Legal Bear", 2, 2).id();
    let invalidated = scenario.add_creature(P0, "Invalidated Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Chosen Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut committed = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[legal, invalidated])
        .commit();
    let announced = committed
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability())
        .expect("committed spell");
    assert_eq!(announced.targets.len(), 2);
    let mut removal_events = Vec::new();
    move_to_zone(
        committed.state_mut(),
        invalidated,
        Zone::Graveyard,
        &mut removal_events,
    );

    let outcome = committed.resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == legal
    )));
    outcome.assert_zone(&[legal], Zone::Hand);
    outcome.assert_zone(&[invalidated], Zone::Graveyard);
}

#[test]
fn all_invalid_paid_targets_fizzle_before_selected_return() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Chosen Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut committed = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[first, second])
        .commit();
    let announced = committed
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability())
        .expect("committed spell");
    assert_eq!(announced.targets.len(), 2);
    let mut removal_events = Vec::new();
    move_to_zone(
        committed.state_mut(),
        first,
        Zone::Graveyard,
        &mut removal_events,
    );
    move_to_zone(
        committed.state_mut(),
        second,
        Zone::Graveyard,
        &mut removal_events,
    );

    let outcome = committed.resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::StackResolved { object_id } if *object_id == spell
    )));
    assert!(!outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::BounceAll,
            ..
        }
    )));
    outcome.assert_zone(&[first, second], Zone::Graveyard);
}

#[test]
fn paid_chosen_return_moves_only_its_announced_pair() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Chosen Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[first, second])
        .resolve();
    outcome.assert_zone(&[first, second], Zone::Hand);
    outcome.assert_zone(&[unselected], Zone::Battlefield);
}

#[test]
fn paid_empty_chosen_return_does_not_scan_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Empty Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).accept_optional().resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::BounceAll,
            ..
        }
    )));
    outcome.assert_zone(&[unselected], Zone::Battlefield);
}

#[test]
fn copied_paid_pair_retargets_one_member_and_preserves_original() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let replacement = scenario.add_creature(P0, "Replacement Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Chosen Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let mut original = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[first, second])
        .commit();
    let copy_result = original
        .cast(twincast)
        .target_object(spell)
        .commit()
        .resolve();
    let WaitingFor::CopyRetarget {
        copy_id,
        target_slots,
        ..
    } = copy_result.final_waiting_for()
    else {
        panic!("paid copy must offer retargeting");
    };
    assert_eq!(target_slots.len(), 2);
    assert_eq!(target_slots[0].current, Some(TargetRef::Object(first)));
    assert_eq!(target_slots[1].current, Some(TargetRef::Object(second)));
    assert!(copy_result.events().iter().any(|event| matches!(
        event,
        GameEvent::SpellCopied { object_id, .. } if object_id == copy_id
    )));
    original
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(replacement)),
        })
        .expect("retarget first copied member");
    original
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep second copied member");
    let outcome = original.resolve();
    outcome.assert_zone(&[first, second, replacement], Zone::Hand);
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == replacement
    )));
}

#[test]
fn copied_paid_group_keeps_both_original_targets_when_declined() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Chosen Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut original = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[first, second])
        .commit();
    let copy_result = original
        .cast(twincast)
        .target_object(spell)
        .commit()
        .resolve();
    let WaitingFor::CopyRetarget { copy_id, .. } = copy_result.final_waiting_for() else {
        panic!("nonempty paid copy must offer a keep-target choice");
    };
    let copy_id = *copy_id;
    original
        .act(GameAction::KeepAllCopyTargets)
        .expect("decline new targets for the whole copy");
    let copied = original
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .and_then(|entry| entry.ability())
        .expect("copy on stack");
    assert!(copied.context.additional_cost_paid);
    assert_eq!(
        copied.sub_ability.as_deref().expect("paid branch").targets,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );
    let outcome = original.resolve();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == first
    )));
    outcome.assert_zone(&[first, second], Zone::Hand);
    outcome.assert_zone(&[unselected], Zone::Battlefield);
}

#[test]
fn copied_paid_empty_group_resolves_without_retarget_prompt_or_population_scan() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Empty Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let mut original = runner.cast(spell).accept_optional().commit();
    let copy_result = original
        .cast(twincast)
        .target_object(spell)
        .commit()
        .resolve();
    assert!(copy_result.events().iter().any(|event| matches!(
        event,
        GameEvent::SpellCopied { original_id, .. } if *original_id == spell
    )));
    assert!(!matches!(
        copy_result.final_waiting_for(),
        WaitingFor::CopyRetarget { .. }
    ));
    assert_eq!(
        copy_result
            .events()
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::BounceAll,
                    ..
                }
            ))
            .count(),
        2,
        "the copy and original must each reach the empty selected return"
    );
    copy_result.assert_zone(&[unselected], Zone::Battlefield);
}

fn persisted_roundtrip(state: &engine::types::game_state::GameState) -> GameRunner {
    let saved = serde_json::to_value(PersistedGameState::capture(state.clone()))
        .expect("serialize the authoritative paused game");
    let restored: PersistedGameState =
        serde_json::from_value(saved).expect("decode the authoritative paused game");
    GameRunner::from_state(restored.into_game_state().expect("restore the paused game"))
}

#[test]
fn paid_group_survives_paused_kicker_declaration_and_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let unselected = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Chosen Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("start the real cast");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));
    let mut runner = persisted_roundtrip(runner.state());
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("pay kicker after restore");
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("paid branch must request target selection");
    };
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(first)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(first)),
        })
        .expect("announce first target");
    let mut runner = persisted_roundtrip(runner.state());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(second)),
        })
        .expect("announce second target after restore");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("close the paid variable target set");
    }
    let announced = runner
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability())
        .expect("spell committed");
    assert!(announced.context.additional_cost_paid);
    assert_eq!(
        announced.targets,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&unselected].zone, Zone::Battlefield);
}

#[test]
fn copied_paid_group_survives_paused_retarget_and_restamps_child() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let replacement = scenario.add_creature(P0, "Replacement Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kicked Chosen Return", true, KICKED_CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut original = runner
        .cast(spell)
        .accept_optional()
        .target_objects(&[first, second])
        .commit();
    let copy_result = original
        .cast(twincast)
        .target_object(spell)
        .commit()
        .resolve();
    let WaitingFor::CopyRetarget { copy_id, .. } = copy_result.final_waiting_for() else {
        panic!("copy must reach target choice");
    };
    let copy_id = *copy_id;
    let mut resumed = persisted_roundtrip(copy_result.state());
    resumed
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(replacement)),
        })
        .expect("retarget first copied member after restore");
    resumed
        .act(GameAction::KeepAllCopyTargets)
        .expect("preserve second copied member");
    let copied = resumed
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .and_then(|entry| entry.ability())
        .expect("copied spell remains on the stack");
    let child = copied.sub_ability.as_deref().expect("paid branch");
    assert_eq!(
        child.targets,
        vec![TargetRef::Object(replacement), TargetRef::Object(second)]
    );
    let reader = child.sub_ability.as_deref().expect("selected return");
    assert_eq!(reader.targets, child.targets);
    let original = resumed
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == spell)
        .and_then(|entry| entry.ability())
        .expect("original spell remains on the stack");
    assert_eq!(
        original
            .sub_ability
            .as_deref()
            .expect("original paid branch")
            .targets,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );
    resumed.advance_until_stack_empty();
    assert_eq!(resumed.state().objects[&first].zone, Zone::Hand);
    assert_eq!(resumed.state().objects[&second].zone, Zone::Hand);
    assert_eq!(resumed.state().objects[&replacement].zone, Zone::Hand);
}

#[test]
fn automatic_zada_copy_rewrite_rebinds_chosen_return_to_each_member() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zada = scenario
        .add_creature_from_oracle(P0, "Zada, Hedron Grinder", 3, 3, ZADA)
        .id();
    let first = scenario.add_creature(P0, "First Ally", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Ally", 2, 2).id();
    let opponent_owned = scenario
        .add_creature(P1, "Borrowed Ally", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chosen Return Witness", true, CHOSEN_RETURN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    let outcome = runner.cast(spell).target_object(zada).resolve();
    let copied = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event, GameEvent::SpellCopied { original_id, .. } if *original_id == spell
            )
        })
        .count();
    assert_eq!(
        copied, 2,
        "Zada must directly rewrite two real spell copies"
    );
    outcome.assert_zone(&[zada, first, second], Zone::Hand);
    outcome.assert_zone(&[opponent_owned], Zone::Battlefield);
}
