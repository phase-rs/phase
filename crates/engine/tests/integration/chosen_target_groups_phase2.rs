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

// Full printed Mogg text. Two occurrences of "target" permit the same object
// in both groups (CR 115.3); no global Another restriction belongs here.
// CR 602.2b and CR 601.2c put both selections before the coin; CR 705.2
// determines the flipper's result. CR 608.2b and CR 400.7 keep legality and
// incarnation local to each occurrence. CR 109.5 and CR 113.8 keep the source's
// owner, controller and the chosen creature's current controller separate.
const MOGG_ASSASSIN: &str = "{T}: You choose target creature an opponent controls, and that opponent chooses target creature. Flip a coin. If you win the flip, destroy the creature you chose. If you lose the flip, destroy the creature your opponent chose.";
const THUMB: &str = "If you would flip a coin, instead flip two coins and ignore one.";
const THIRD_SEAT: engine::types::player::PlayerId = engine::types::player::PlayerId(2);

struct MoggGame {
    runner: GameRunner,
    source: ObjectId,
    first: ObjectId,
    second: ObjectId,
    decoys: Vec<ObjectId>,
}

fn mogg_game(thumb: bool) -> MoggGame {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // The source owner, group-zero owner, actual chooser and first opponent
    // seat deliberately disagree. Group zero's CURRENT controller is seat 2.
    let source = scenario
        .add_creature_from_oracle(P1, "Mogg Assassin", 2, 1, MOGG_ASSASSIN)
        .controlled_by(P0)
        .id();
    let first = scenario
        .add_creature(P0, "First selected", 2, 3)
        .controlled_by(THIRD_SEAT)
        .id();
    let second = scenario
        .add_creature(P1, "Second selected", 2, 3)
        .controlled_by(P0)
        .id();
    let decoys = vec![
        scenario
            .add_creature(P1, "First opponent seat decoy", 2, 3)
            .id(),
        scenario
            .add_creature(THIRD_SEAT, "Owner decoy", 2, 3)
            .controlled_by(P0)
            .id(),
    ];
    if thumb {
        scenario.add_artifact_from_oracle(P0, "Krark's Thumb", THUMB);
    }
    MoggGame {
        runner: scenario.build(),
        source,
        first,
        second,
        decoys,
    }
}

fn assert_mogg_actor(
    runner: &GameRunner,
    actor: engine::types::player::PlayerId,
    target: ObjectId,
) {
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "required announced Mogg prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, actor);
    assert!(target_slots[selection.current_slot]
        .legal_targets
        .contains(&TargetRef::Object(target)));
    assert_eq!(
        engine::game::turn_control::authorized_submitter(runner.state()),
        Some(actor)
    );
    for seat in [P0, P1, THIRD_SEAT] {
        let actions = engine::ai_support::legal_actions_for_viewer(runner.state(), seat).0;
        assert_eq!(
            !actions.is_empty(),
            seat == actor,
            "only actual chooser gets actions"
        );
        if seat == actor {
            assert!(actions.iter().any(|action| matches!(action, GameAction::ChooseTarget { target: Some(TargetRef::Object(id)) } if *id == target)), "engine publishes the actual nondefault public response");
        }
    }
}

fn announce_mogg(game: &mut MoggGame, same: bool) {
    game.runner
        .act(GameAction::ActivateAbility {
            source_id: game.source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("real printed tap activation");
    assert_mogg_actor(&game.runner, P0, game.first);
    game.runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(game.first)),
        })
        .expect("controller selects group zero");
    let second = if same { game.first } else { game.second };
    assert_mogg_actor(&game.runner, THIRD_SEAT, second);
    assert!(engine::game::engine::apply(
        game.runner.state_mut(),
        P0,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(second))
        }
    )
    .is_err());
    engine::game::engine::apply(
        game.runner.state_mut(),
        THIRD_SEAT,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(second)),
        },
    )
    .expect("actual group-zero controller selects group one");
    assert!(game.runner.state().objects[&game.source].tapped);
    assert!(matches!(
        game.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(game.runner.state().stack.len(), 1);
    let ability = game
        .runner
        .state()
        .stack
        .back()
        .unwrap()
        .ability()
        .expect("activation on stack before coin");
    assert_eq!(ability.context.chosen_group_targets.len(), 2);
    assert_eq!(
        ability.context.chosen_group_targets[0].1[0].0,
        TargetRef::Object(game.first)
    );
    assert_eq!(
        ability.context.chosen_group_targets[1].1[0].0,
        TargetRef::Object(second)
    );
    assert!(ability
        .context
        .chosen_group_targets
        .iter()
        .all(|(_, entries)| entries[0].1.is_some()));
}

fn pin_mogg(runner: &mut GameRunner, outcomes: &[bool]) {
    use engine::types::game_state::{GameRng, RandomDraw};
    use rand::Rng;
    let seed = (0..100_000)
        .find(|seed| {
            let mut rng = GameRng::seed_from_u64(*seed);
            outcomes
                .iter()
                .all(|won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == *won)
        })
        .expect("deterministic seed for prescribed coin sequence");
    runner.state_mut().rng_seed = seed;
    runner.state_mut().rng_word_pos = 0;
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
}

fn assert_no_public_group_capture(state: &engine::types::game_state::GameState) {
    use engine::game::derived_views::ClientGameStateRef;
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
    fn public_shape(actual: &ResolvedAbility, public: &ResolvedAbility) {
        assert_eq!(public.effect, actual.effect);
        assert_eq!(public.targets, actual.targets);
        assert_eq!(public.declares_chosen_group, actual.declares_chosen_group);
        assert_eq!(public.reads_chosen_group, actual.reads_chosen_group);
        assert!(public.context.chosen_group_targets.is_empty());
        assert_eq!(
            public.coin_branch_target_abilities.len(),
            actual.coin_branch_target_abilities.len()
        );
        for ((actual_outcome, actual_child), (public_outcome, public_child)) in actual
            .coin_branch_target_abilities
            .iter()
            .zip(&public.coin_branch_target_abilities)
        {
            assert_eq!(actual_outcome, public_outcome);
            public_shape(actual_child, public_child);
        }
        match (&actual.sub_ability, &public.sub_ability) {
            (Some(a), Some(p)) => public_shape(a, p),
            (None, None) => {}
            _ => panic!("public ordinary topology changed"),
        }
        match (&actual.else_ability, &public.else_ability) {
            (Some(a), Some(p)) => public_shape(a, p),
            (None, None) => {}
            _ => panic!("public alternative topology changed"),
        }
    }
    fn captures_empty(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Some(capture) = fields.get("chosen_group_targets") {
                    assert!(capture.as_array().is_some_and(Vec::is_empty));
                }
                for value in fields.values() {
                    captures_empty(value);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    captures_empty(item);
                }
            }
            _ => {}
        }
    }
    let authoritative = serde_json::to_value(state).unwrap();
    let raw: engine::types::game_state::GameState =
        serde_json::from_value(authoritative.clone()).expect("valid authoritative raw state");
    for (before, after) in state.stack.iter().zip(raw.stack.iter()) {
        assert_eq!(
            before.ability(),
            after.ability(),
            "raw authoritative custody retained"
        );
    }
    assert_eq!(state.active_coin_flip_frame(), raw.active_coin_flip_frame());
    assert!(
        serde_json::to_string(&authoritative)
            .unwrap()
            .contains("chosen_group_targets"),
        "positive authoritative capture reach"
    );
    for viewer in [Some(P0), Some(P1), Some(THIRD_SEAT), None] {
        let projected = viewer.map_or_else(
            || filter_state_for_unseated_viewer(state),
            |seat| filter_state_for_viewer(state, seat),
        );
        let plain = serde_json::to_value(&projected).unwrap();
        let client = serde_json::to_value(ClientGameStateRef::wrap(state, viewer)).unwrap();
        let envelope: engine::game::derived_views::ClientGameState =
            serde_json::from_slice(&serde_json::to_vec(&client).unwrap())
                .expect("actual client envelope transports its raw GameState member");
        assert_eq!(
            serde_json::to_value(&envelope.state.waiting_for).unwrap(),
            plain["waiting_for"]
        );
        captures_empty(&plain);
        captures_empty(&client);
        // A transport projection is still a real raw GameState message. Its
        // public reader definitions must decode without restoring private maps.
        for public in [&plain, &client["state"]] {
            let bytes = serde_json::to_vec(public).unwrap();
            let transported: engine::types::game_state::GameState =
                serde_json::from_slice(&bytes).expect("projected raw GameState transport");
            let transported_value = serde_json::to_value(&transported).unwrap();
            captures_empty(&transported_value);
            assert_eq!(transported_value["waiting_for"], public["waiting_for"]);
            assert_eq!(transported_value["stack"], plain["stack"]);
            for (actual, public) in state.stack.iter().zip(&transported.stack) {
                if let (Some(actual), Some(public)) = (actual.ability(), public.ability()) {
                    public_shape(actual, public);
                }
            }
            assert_eq!(
                state.resolving_stack_entry.is_some(),
                transported.resolving_stack_entry.is_some()
            );
            if let (Some(actual), Some(public)) = (
                &state.resolving_stack_entry,
                &transported.resolving_stack_entry,
            ) {
                public_shape(
                    actual.ability().expect("real public resolving carrier"),
                    public.ability().expect("transport retained carrier"),
                );
            }
            assert!(
                serde_json::from_slice::<PersistedGameState>(&bytes).is_err(),
                "a public transport is not saved-game authority"
            );
            assert!(
                serde_json::from_slice::<engine::types::resolution::ResolutionStateWire>(&bytes)
                    .is_err(),
                "a public transport is not resolution-wire authority"
            );
        }
        assert_eq!(plain["waiting_for"], client["state"]["waiting_for"]);
        assert_eq!(
            plain["waiting_for"], authoritative["waiting_for"],
            "public actor and options remain available for every audience"
        );
        assert_eq!(projected.stack.len(), state.stack.len());
        for (public, actual) in projected.stack.iter().zip(state.stack.iter()) {
            assert_eq!(
                public.ability().map(|a| &a.targets),
                actual.ability().map(|a| &a.targets)
            );
        }
    }
}

// Secondary typed class composition of the independently guarded nested IR.
// This is not additional printed Mogg text. Definitions alone are supplied;
// normal casting/target actions must create every occurrence, map and pin.
fn nested_reader(group: u32) -> AbilityDefinition {
    use engine::types::ability::{AbilityKind, ChosenGroupId, TargetFilter};
    let mut reader = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Destroy {
            target: TargetFilter::ParentTarget,
            cant_regenerate: false,
        },
    )
    .description(format!("Destroy exactly chosen group {group}."));
    reader.reads_chosen_group = Some(ChosenGroupId(group));
    reader
}

fn nested_declaration(group: u32) -> AbilityDefinition {
    use engine::types::ability::ChosenGroupId;
    let mut declaration = parse_oracle_text(
        "Choose target artifact you control.",
        "Constructed local declaration",
        &[],
        &["Instant".into()],
        &[],
    )
    .abilities[0]
        .clone();
    declaration.declares_chosen_group = Some(ChosenGroupId(group));
    declaration
}

fn nested_flip(win: AbilityDefinition, loss: AbilityDefinition) -> AbilityDefinition {
    use engine::types::ability::{AbilityKind, CoinFlipRun, TargetFilter};
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(win)),
            lose_effect: Some(Box::new(loss)),
            flipper: TargetFilter::Controller,
        },
    )
    .description("Constructed exact local/external coin branches".into())
}

fn nested_program(reversed: bool, second_local: bool) -> AbilityDefinition {
    let text = if reversed {
        "Choose target creature of an opponent's choice. Choose target creature you control. Return each creature you chose to its owner's hand."
    } else {
        MOGG_ASSASSIN.strip_prefix("{T}: ").unwrap()
    };
    let mut root = parse_oracle_text(
        text,
        "Constructed nested program",
        &[],
        &["Instant".into()],
        &[],
    )
    .abilities[0]
        .clone();
    let second = root.sub_ability.as_mut().expect("two real declarations");
    // Local ClauseId0 in this independently parsed body becomes runtime group2;
    // the namespace/source proof belongs to the companion parser regression.
    let body = nested_declaration(2)
        .sub_ability(nested_reader(1).sub_ability(nested_flip(nested_reader(2), nested_reader(0))));
    let lost = if second_local {
        nested_declaration(3).sub_ability(nested_flip(nested_reader(3), nested_reader(1)))
    } else {
        nested_reader(1)
    };
    second.sub_ability = Some(Box::new(nested_flip(body, lost)));
    root
}

fn nested_node<'a>(
    mut root: &'a ResolvedAbility,
    path: &[engine::types::game_state::ChainStep],
) -> &'a ResolvedAbility {
    use engine::types::game_state::ChainStep;
    for step in path {
        root = match step {
            ChainStep::SubAbility => root
                .sub_ability
                .as_deref()
                .expect("addressed ordinary child"),
            ChainStep::ElseAbility => root.else_ability.as_deref().expect("addressed alternative"),
            ChainStep::CoinBranch(outcome) => root
                .coin_branch_target_abilities
                .iter()
                .find(|(actual, _)| actual == outcome)
                .expect("addressed target custody")
                .1
                .as_ref(),
        };
    }
    root
}

struct NestedGame {
    runner: GameRunner,
    spell: ObjectId,
    a: ObjectId,
    b: ObjectId,
    c: ObjectId,
    d: ObjectId,
    decoy: ObjectId,
    responses: Vec<ObjectId>,
}

fn nested_game(program: AbilityDefinition, thumb: bool) -> NestedGame {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario
        .add_creature(P0, "Outer group zero", 2, 4)
        .controlled_by(P1)
        .id();
    let b = scenario
        .add_creature(THIRD_SEAT, "Outer group one", 2, 4)
        .controlled_by(P0)
        .id();
    let decoy = scenario.add_creature(P1, "Unselected creature", 2, 4).id();
    let c = scenario
        .add_artifact_from_oracle(P1, "Local group two", "")
        .controlled_by(P0)
        .id();
    let d = scenario
        .add_artifact_from_oracle(THIRD_SEAT, "Local group three or replacement", "")
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Constructed nested target custody", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_ability_definition(program)
        .id();
    let mut responses: Vec<_> = [
        "Destroy target artifact.",
        "Exile target artifact, then return it to the battlefield under its owner's control.",
        "Destroy all creatures.",
        "Destroy all nonland permanents.",
    ]
    .map(|text| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Constructed paid response", true, text)
            .with_mana_cost(ManaCost::generic(1))
            .id()
    })
    .into_iter()
    .collect();
    use engine::types::ability::{AbilityKind, TargetFilter};
    use engine::types::game_state::RetargetScope;
    for scope in [
        RetargetScope::All,
        RetargetScope::Single,
        RetargetScope::ForcedTo(TargetRef::Object(d)),
    ] {
        let forced_to = matches!(scope, RetargetScope::ForcedTo(_))
            .then_some(TargetFilter::SpecificObject { id: d });
        responses.push(
            scenario
                .add_spell_to_hand(P0, "Constructed addressed retarget", true)
                .with_mana_cost(ManaCost::generic(1))
                .with_ability_definition(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeTargets {
                        target: TargetFilter::StackSpell,
                        scope,
                        forced_to,
                    },
                ))
                .id(),
        );
    }
    responses.push(
        scenario
            .add_spell_to_hand_from_oracle(P0, "Constructed nested copy", true, TWINCAST)
            .with_mana_cost(ManaCost::generic(1))
            .id(),
    );
    if thumb {
        scenario.add_artifact_from_oracle(P0, "Krark's Thumb", THUMB);
    }
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    NestedGame {
        runner: scenario.build(),
        spell,
        a,
        b,
        c,
        d,
        decoy,
        responses,
    }
}

fn nested_begin(game: &mut NestedGame) {
    let card_id = game.runner.state().objects[&game.spell].card_id;
    game.runner
        .act(GameAction::CastSpell {
            object_id: game.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
}

fn nested_choose(game: &mut NestedGame, actor: engine::types::player::PlayerId, id: ObjectId) {
    assert_mogg_actor(&game.runner, actor, id);
    let wrong = if actor == P0 { P1 } else { P0 };
    assert!(engine::game::engine::apply(
        game.runner.state_mut(),
        wrong,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id))
        }
    )
    .is_err());
    engine::game::engine::apply(
        game.runner.state_mut(),
        actor,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        },
    )
    .unwrap();
}

fn nested_announce(game: &mut NestedGame, reversed: bool, second: bool) {
    nested_begin(game);
    let (a, b, c, d) = (game.a, game.b, game.c, game.d);
    for (actor, id) in if reversed {
        [(P1, a), (P0, b)]
    } else {
        [(P0, a), (P1, b)]
    } {
        nested_choose(game, actor, id);
    }
    nested_choose(game, P0, c);
    if second {
        nested_choose(game, P0, d);
    }
    assert_eq!(game.runner.state().objects[&game.spell].zone, Zone::Stack);
    assert_eq!(game.runner.state().players[0].mana_pool.total(), 1);
    assert!(matches!(
        game.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let root = game.runner.state().stack.back().unwrap().ability().unwrap();
    let targets = engine::game::ability_utils::declared_targets_in_chain(root);
    let expected = if second {
        vec![a, b, c, d]
    } else {
        vec![a, b, c]
    };
    assert_eq!(
        targets,
        expected
            .into_iter()
            .map(TargetRef::Object)
            .collect::<Vec<_>>()
    );
    let bindings = engine::game::ability_utils::chain_retarget_slots(root);
    assert_eq!(bindings.len(), if second { 4 } else { 3 });
    assert!(bindings[2].address.path.iter().any(|step| matches!(
        step,
        engine::types::game_state::ChainStep::CoinBranch(
            engine::types::ability::CoinFlipResult::Won
        )
    )));
    assert_eq!(
        nested_node(root, &bindings[2].address.path).declares_chosen_group,
        Some(engine::types::ability::ChosenGroupId(2))
    );
    assert!(bindings
        .iter()
        .all(|binding| nested_node(root, &binding.address.path)
            .target_occurrences()
            .into_iter()
            .all(|(_, pin)| pin.is_some())));
    assert_no_public_group_capture(game.runner.state());
    fn no_execution(node: &ResolvedAbility) {
        assert!(node.context.damage_source_choice.is_none());
        assert!(node.context.damage_opportunity.is_none());
        assert!(node.return_result_process.is_none());
        assert!(node.activation_record.is_none());
        assert!(node.chosen_players.is_empty());
        for (_, child) in &node.coin_branch_target_abilities {
            no_execution(child);
        }
        if let Some(sub) = &node.sub_ability {
            no_execution(sub);
        }
        if let Some(other) = &node.else_ability {
            no_execution(other);
        }
    }
    for (_, child) in &root
        .sub_ability
        .as_ref()
        .unwrap()
        .sub_ability
        .as_ref()
        .unwrap()
        .coin_branch_target_abilities
    {
        no_execution(child);
    }
}

fn nested_response(
    game: &mut NestedGame,
    index: usize,
    target: Option<ObjectId>,
) -> Vec<GameEvent> {
    let response = game.responses[index];
    let mut cast = game.runner.cast(response);
    if let Some(target) = target {
        cast = cast.target_object(target);
    }
    cast.commit();
    assert_eq!(game.runner.state().players[0].mana_pool.total(), 0);
    let mut events = vec![];
    for _ in 0..16 {
        if !game
            .runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == response)
        {
            assert_eq!(game.runner.state().objects[&response].zone, Zone::Graveyard);
            assert_eq!(game.runner.state().stack.len(), 1);
            return events;
        }
        assert!(matches!(
            game.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(game.runner.act(GameAction::PassPriority).unwrap().events);
    }
    panic!("actual response did not complete before original spell");
}

#[test]
fn nested_paid_late_holes_new_birth_and_all_invalid_use_one_declared_domain() {
    // CR 400.7 and CR 608.2b: the new birth is not the selected occurrence;
    // one surviving local declaration keeps the entire targeted spell live.
    for invalid in 0..4 {
        for codec in 0..4 {
            let mut game = nested_game(nested_program(false, false), invalid != 3);
            nested_announce(&mut game, false, false);
            let c = game.c;
            let response_events =
                nested_response(&mut game, invalid, if invalid < 2 { Some(c) } else { None });
            assert!(
                response_events
                    .iter()
                    .any(|event| matches!(event, GameEvent::ZoneChanged { .. })),
                "real paid response changes zones"
            );
            pin_mogg(&mut game.runner, &[false, true, false, true]);
            let events = pass_mogg_priority(&mut game.runner);
            if invalid == 3 {
                assert!(!events
                    .iter()
                    .any(|event| matches!(event, GameEvent::CoinFlipped { .. })));
                assert_eq!(
                    game.runner.state().objects[&game.spell].zone,
                    Zone::Graveyard
                );
                assert!(game.runner.state().stack.is_empty());
                assert!(game.runner.state().resolution_stack.is_empty());
                assert!(matches!(
                    game.runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                continue;
            }
            assert!(matches!(
                game.runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { .. }
            ));
            let frame = game.runner.state().active_coin_flip_frame().unwrap();
            for group in if invalid < 2 { vec![2] } else { vec![0, 1] } {
                assert!(
                    frame
                        .branch_context
                        .chosen_group_targets
                        .iter()
                        .find(|(id, _)| id.0 == group)
                        .unwrap()
                        .1
                        .is_empty(),
                    "illegal group is explicitly empty"
                );
            }
            let local = frame
                .branch_context
                .coin_branch_target_abilities
                .iter()
                .find(|(result, _)| *result == engine::types::ability::CoinFlipResult::Won)
                .unwrap()
                .1
                .as_ref();
            if invalid < 2 {
                assert!(local.targets.is_empty());
                assert_eq!(local.illegal_local_target_slots, vec![0]);
            }
            let retained = game
                .runner
                .state()
                .resolving_stack_entry
                .as_ref()
                .unwrap()
                .ability()
                .unwrap();
            assert_eq!(
                retained.illegal_target_slots,
                if invalid < 2 { vec![2] } else { vec![0, 1] }
            );
            assert!(
                retained.unjudged_target_slots.is_empty(),
                "every original occurrence was judged in one ordinal domain"
            );
            game.runner = nested_roundtrip(game.runner, codec);
            game.runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1],
                })
                .unwrap();
            assert!(matches!(
                game.runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { .. }
            ));
            game.runner = nested_roundtrip(game.runner, codec);
            game.runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1],
                })
                .unwrap();
            game.runner.advance_until_stack_empty();
            let destroyed = match invalid {
                0 => vec![game.b, game.c],
                1 => vec![game.b],
                2 => vec![game.a, game.b, game.c, game.decoy],
                _ => unreachable!(),
            };
            nested_finished(&game, &destroyed);
        }
    }
}

fn nested_roundtrip(runner: GameRunner, codec: u8) -> GameRunner {
    use engine::types::game_state::GameState;
    use engine::types::resolution::ResolutionStateWire;
    match codec {
        0 => GameRunner::from_state(
            serde_json::from_value::<GameState>(serde_json::to_value(runner.state()).unwrap())
                .unwrap(),
        ),
        1 => GameRunner::from_state(
            serde_json::from_value::<PersistedGameState>(
                serde_json::to_value(PersistedGameState::Raw(Box::new(runner.state().clone())))
                    .unwrap(),
            )
            .unwrap()
            .into_game_state()
            .unwrap(),
        ),
        2 => persisted_roundtrip(runner.state()),
        3 => GameRunner::from_state(
            serde_json::from_value::<ResolutionStateWire>(
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap(),
            )
            .unwrap()
            .into_game_state(),
        ),
        _ => unreachable!(),
    }
}

fn nested_finished(game: &NestedGame, destroyed: &[ObjectId]) {
    for id in [game.a, game.b, game.c, game.d, game.decoy] {
        assert_eq!(
            game.runner.state().objects[&id].zone,
            if destroyed.contains(&id) {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            },
            "exact nested payoff {id:?}"
        );
    }
    assert_eq!(
        game.runner.state().objects[&game.spell].zone,
        Zone::Graveyard
    );
    assert!(matches!(
        game.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(game.runner.state().stack.is_empty());
    assert!(game.runner.state().resolution_stack.is_empty());
}

#[test]
fn fresh_paradigm_copy_uses_the_selected_prefix_foreign_announcer() {
    // Constructed composition of the full Mogg announcement/body and the
    // existing Paradigm cast route. CR 601.2c / CR 702.192a: this fresh cast
    // announces targets; CR 707.10c's retarget actor belongs to another route.
    for codec in 0..4 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario
            .add_creature(P1, "First selected controller", 2, 3)
            .id();
        let b = scenario
            .add_creature(P0, "Foreign nondefault second selection", 2, 3)
            .id();
        let decoy = scenario
            .add_creature(P1, "Unselected alternative", 2, 3)
            .id();
        let program = parse_oracle_text(
            MOGG_ASSASSIN.strip_prefix("{T}: ").unwrap(),
            "Mogg Assassin",
            &[],
            &["Instant".into()],
            &[],
        )
        .abilities[0]
            .clone();
        let source = scenario
            .add_spell_to_hand(P0, "Constructed fresh Paradigm relation", true)
            .with_mana_cost(ManaCost::generic(3))
            .with_ability_definition(program)
            .id();
        let mut runner = scenario.build();
        move_to_zone(runner.state_mut(), source, Zone::Exile, &mut Vec::new());
        assert!(engine::game::effects::paradigm::arm_paradigm(
            runner.state_mut(),
            source,
            P0,
            "Constructed fresh Paradigm relation"
        ));
        assert!(engine::game::effects::paradigm::enqueue_offer_if_any(
            runner.state_mut(),
            P0
        ));
        runner.act(GameAction::CastParadigmCopy { source }).unwrap();
        let copy_id = match &runner.state().waiting_for {
            WaitingFor::CopyRetarget {
                player,
                current_slot,
                mode,
                copy_id,
                target_slots,
                ..
            } => {
                assert_eq!(*player, P0);
                assert_eq!(*current_slot, 0);
                assert_eq!(
                    *mode,
                    Some(engine::types::game_state::CopyChoiceMode::Announce)
                );
                assert!(!target_slots[0].can_keep);
                assert!(target_slots[0]
                    .legal_alternatives
                    .contains(&TargetRef::Object(a)));
                *copy_id
            }
            other => panic!("real fresh-copy announcement: {other:?}"),
        };
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(a)),
            })
            .unwrap();
        match &runner.state().waiting_for {
            WaitingFor::CopyRetarget {
                player,
                controller,
                current_slot,
                target_slots,
                ..
            } => {
                assert_eq!(
                    *player, P1,
                    "selected creature's controller owns the second announcement"
                );
                assert_eq!(*controller, Some(P0));
                assert_eq!(*current_slot, 1);
                assert!(target_slots[1]
                    .legal_alternatives
                    .contains(&TargetRef::Object(b)));
                assert_ne!(
                    target_slots[1].legal_alternatives.first(),
                    Some(&TargetRef::Object(b))
                );
                assert!(!target_slots[1].can_keep);
            }
            other => panic!("mandatory dependent fresh-copy prompt: {other:?}"),
        }
        runner = nested_roundtrip(runner, codec);
        assert!(engine::game::engine::apply(
            runner.state_mut(),
            P0,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(b))
            }
        )
        .is_err());
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(b)),
            })
            .unwrap();
        let announced = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == copy_id)
            .unwrap()
            .ability()
            .unwrap();
        assert_eq!(
            engine::game::ability_utils::declared_targets_in_chain(announced),
            vec![TargetRef::Object(a), TargetRef::Object(b)]
        );
        pin_mogg(&mut runner, &[false]);
        pass_mogg_priority(&mut runner);
        assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard);
        for id in [a, decoy] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
    }
}

#[test]
fn missing_entire_current_pending_custody_rejects_at_both_readers_and_public_keep() {
    use engine::types::resolution::ResolutionStateWire;
    for branch_only in [false, true] {
        let program = if branch_only {
            nested_flip(
                nested_declaration(2).sub_ability(nested_reader(2)),
                nested_declaration(3).sub_ability(nested_reader(3)),
            )
        } else {
            nested_program(false, false)
        };
        let mut game = nested_game(program, true);
        if branch_only {
            nested_begin(&mut game);
            let (c, d) = (game.c, game.d);
            nested_choose(&mut game, P0, c);
            nested_choose(&mut game, P0, d);
        } else {
            nested_announce(&mut game, false, false);
        }
        pin_mogg(&mut game.runner, &[true, false]);
        pass_mogg_priority(&mut game.runner);
        assert!(
            matches!(&game.runner.state().waiting_for, WaitingFor::CoinFlipKeepChoice { results, .. } if results == &vec![true, false])
        );
        let state = game.runner.state().clone();
        let frame = state.active_coin_flip_frame().unwrap();
        assert!(!frame.branch_context.coin_branch_target_abilities.is_empty());
        assert_eq!(frame.source_id, game.spell);
        if branch_only {
            assert!(
                frame.targets.is_empty(),
                "no flat target can replace the local declaration"
            );
        }
        // Valid current controls use each complete reader and the actual keep.
        for codec in [0, 3] {
            let mut valid = GameRunner::from_state(state.clone());
            valid = nested_roundtrip(valid, codec);
            valid
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1],
                })
                .unwrap();
            valid.advance_until_stack_empty();
            assert_eq!(
                valid.state().objects[&if branch_only { game.d } else { game.b }].zone,
                Zone::Graveyard
            );
            assert!(matches!(
                valid.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(valid.state().stack.is_empty() && valid.state().resolution_stack.is_empty());
        }
        let mut hostile = state;
        hostile
            .active_coin_flip_frame_mut()
            .unwrap()
            .branch_context
            .coin_branch_target_abilities
            .clear();
        let raw = serde_json::to_value(&hostile).unwrap();
        let wire =
            serde_json::to_value(ResolutionStateWire::from_game_state(hostile.clone())).unwrap();
        assert!(
            serde_json::to_value(hostile.active_coin_flip_frame().unwrap()).unwrap()
                ["branch_context"]
                .get("coin_branch_target_abilities")
                .is_none(),
            "the complete pending collection field is absent"
        );
        assert!(serde_json::from_value::<engine::types::game_state::GameState>(raw).is_err());
        assert!(serde_json::from_value::<ResolutionStateWire>(wire).is_err());
        let before = serde_json::to_value(&hostile).unwrap();
        assert!(engine::game::engine::apply(
            &mut hostile,
            P0,
            GameAction::SelectCoinFlips {
                keep_indices: vec![1]
            }
        )
        .is_err());
        assert_eq!(
            serde_json::to_value(&hostile).unwrap(),
            before,
            "missing current collection refuses before mutation"
        );
    }
}

#[test]
fn targetless_selected_coin_body_keeps_original_controller_input_during_player_fanout() {
    use engine::types::ability::{
        AbilityKind, CoinFlipRun, PlayerFilter, QuantityExpr, TargetFilter,
    };
    // Constructed class composition. CR 109.5 / CR 608.2c: iteration's recipient
    // does not replace the original ability controller named by its body.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut program = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::OriginalController,
                },
            ))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
    );
    program.player_scope = Some(PlayerFilter::All);
    let spell = scenario
        .add_spell_to_hand(
            P0,
            "Constructed targetless original-controller reader",
            true,
        )
        .with_mana_cost(ManaCost::zero())
        .with_ability_definition(program)
        .id();
    let mut runner = scenario.build();
    pin_mogg(&mut runner, &[true, true]);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_life_delta(P0, 4);
    outcome.assert_life_delta(P1, 0);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
}

#[test]
fn nested_paid_declarations_precede_both_coins_and_keep_local_external_roles_at_all_readers() {
    // CR 601.2c and CR 608.2c: declarations in both branches are announced;
    // selected instructions retain their written local/external referents.
    for reversed in [false, true] {
        for (outer, inner) in [(true, true), (true, false), (false, true), (false, false)] {
            for codec in 0..4 {
                let mut game = nested_game(nested_program(reversed, true), false);
                nested_announce(&mut game, reversed, true);
                let root = game.runner.state().stack.back().unwrap().ability().unwrap();
                assert_eq!(root.context.chosen_group_targets.len(), 4);
                assert_eq!(
                    root.context.chosen_group_targets[3].0,
                    engine::types::ability::ChosenGroupId(3)
                );
                pin_mogg(&mut game.runner, &[outer, inner]);
                game.runner = nested_roundtrip(game.runner, codec);
                let events = pass_mogg_priority(&mut game.runner);
                let actual: Vec<_> = events
                    .iter()
                    .filter_map(|event| {
                        if let GameEvent::CoinFlipped { won, .. } = event {
                            Some(*won)
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(actual, [outer, inner]);
                let destroyed = match (outer, inner) {
                    (true, true) => vec![game.b, game.c],
                    (true, false) => vec![game.b, game.a],
                    (false, true) => vec![game.d],
                    (false, false) => vec![game.b],
                };
                nested_finished(&game, &destroyed);
            }
        }
    }
}

#[test]
fn nested_real_krark_pause_preserves_custody_transport_and_nondefault_keeps_without_reflip() {
    for inner in [false, true] {
        for codec in 0..4 {
            let mut game = nested_game(nested_program(false, false), true);
            nested_announce(&mut game, false, false);
            pin_mogg(&mut game.runner, &[false, true, !inner, inner]);
            pass_mogg_priority(&mut game.runner);
            for (level, expected) in [[false, true], [!inner, inner]].into_iter().enumerate() {
                let WaitingFor::CoinFlipKeepChoice {
                    player,
                    results,
                    keep_count,
                } = &game.runner.state().waiting_for
                else {
                    panic!("actual nested Krark keep")
                };
                assert_eq!(*player, P0);
                assert_eq!(results, &expected);
                assert_eq!(*keep_count, 1);
                assert_no_public_group_capture(game.runner.state());
                let frame = game.runner.state().active_coin_flip_frame().unwrap();
                assert_eq!(frame.source_id, game.spell);
                assert_eq!(frame.controller, P0);
                assert_eq!(frame.flipper, P0);
                assert!(!frame.branch_context.chosen_group_targets.is_empty());
                game.runner.state_mut().capture_rng_word_pos();
                let pos = game.runner.state().rng_word_pos;
                game.runner = nested_roundtrip(game.runner, codec);
                game.runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![1],
                    })
                    .unwrap();
                // The first keep starts the second genuine flip; the second
                // consumes already drawn results, and cannot advance RNG.
                if level == 1 {
                    game.runner.state_mut().capture_rng_word_pos();
                    assert_eq!(game.runner.state().rng_word_pos, pos);
                }
            }
            game.runner.advance_until_stack_empty();
            nested_finished(&game, &[game.b, if inner { game.c } else { game.a }]);
        }
    }
}

#[test]
fn nested_all_retarget_changes_exact_branch_address_and_copy_keeps_original_custody() {
    // CR 115.7d and CR 707.10b: only elected occurrences change; a copy
    // retains its own announced custody while the original remains intact.
    for copy in [false, true] {
        for codec in 0..4 {
            let mut game = nested_game(nested_program(false, false), false);
            nested_announce(&mut game, false, false);
            let original_id = game.runner.state().stack.back().unwrap().id;
            let original = game
                .runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .clone();
            let spell = game.responses[if copy { 7 } else { 4 }];
            game.runner.cast(spell).target_object(original_id).commit();
            pass_mogg_priority(&mut game.runner);
            assert_eq!(game.runner.state().players[0].mana_pool.total(), 0);
            let affected = if copy {
                let WaitingFor::CopyRetarget {
                    player,
                    copy_id,
                    target_slots,
                    current_slot,
                    ..
                } = &game.runner.state().waiting_for
                else {
                    panic!("actual nested copy target walk")
                };
                assert_eq!(*player, P0);
                assert_eq!(*current_slot, 0);
                assert_eq!(target_slots.len(), 3);
                assert_eq!(target_slots[2].current, Some(TargetRef::Object(game.c)));
                *copy_id
            } else {
                let WaitingFor::RetargetChoice {
                    player,
                    slots,
                    slot_pools,
                    current_targets,
                    ..
                } = &game.runner.state().waiting_for
                else {
                    panic!("actual addressed retarget")
                };
                assert_eq!(*player, P0);
                assert_eq!(
                    current_targets,
                    &vec![
                        TargetRef::Object(game.a),
                        TargetRef::Object(game.b),
                        TargetRef::Object(game.c)
                    ]
                );
                assert_eq!(slots.len(), 3);
                assert_eq!(slot_pools.len(), 3);
                assert!(slot_pools[2].contains(&TargetRef::Object(game.d)));
                assert!(!slot_pools[2].contains(&TargetRef::Object(game.a)));
                assert!(slots[2].path.iter().any(|step| matches!(
                    step,
                    engine::types::game_state::ChainStep::CoinBranch(_)
                )));
                original_id
            };
            game.runner = nested_roundtrip(game.runner, codec);
            if copy {
                game.runner
                    .act(GameAction::ChooseTarget { target: None })
                    .unwrap();
                let WaitingFor::CopyRetarget {
                    player,
                    current_slot,
                    ..
                } = game.runner.state().waiting_for
                else {
                    panic!("copy controller's second retarget decision")
                };
                // CR 707.10c: the copy controller chooses every replacement;
                // the original declaration's chooser does not own retargeting.
                assert_eq!(player, P0);
                assert_eq!(current_slot, 1);
                assert!(engine::game::engine::apply(
                    game.runner.state_mut(),
                    P1,
                    GameAction::ChooseTarget { target: None }
                )
                .is_err());
                game.runner
                    .act(GameAction::ChooseTarget { target: None })
                    .unwrap();
                let WaitingFor::CopyRetarget {
                    player,
                    current_slot,
                    ..
                } = game.runner.state().waiting_for
                else {
                    panic!("local artifact copy chooser")
                };
                assert_eq!(player, P0);
                assert_eq!(current_slot, 2);
                assert!(engine::game::engine::apply(
                    game.runner.state_mut(),
                    P1,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(game.d))
                    }
                )
                .is_err());
                game.runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(game.d)),
                    })
                    .unwrap();
                assert_eq!(
                    game.runner
                        .state()
                        .stack
                        .iter()
                        .find(|entry| entry.id == original_id)
                        .unwrap()
                        .ability()
                        .unwrap(),
                    &original
                );
            } else {
                assert!(
                    game.runner
                        .act(GameAction::RetargetSpell {
                            new_targets: vec![None, Some(TargetRef::Object(game.d))]
                        })
                        .is_err(),
                    "unaligned answer rejects"
                );
                assert!(engine::game::engine::apply(
                    game.runner.state_mut(),
                    P1,
                    GameAction::RetargetSpell {
                        new_targets: vec![None, None, Some(TargetRef::Object(game.d))]
                    }
                )
                .is_err());
                game.runner
                    .act(GameAction::RetargetSpell {
                        new_targets: vec![None, None, Some(TargetRef::Object(game.d))],
                    })
                    .unwrap();
            }
            let changed = game
                .runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.id == affected)
                .unwrap()
                .ability()
                .unwrap();
            let bindings = engine::game::ability_utils::chain_retarget_slots(changed);
            assert_eq!(bindings[2].current, TargetRef::Object(game.d));
            let local = nested_node(changed, &bindings[2].address.path);
            assert_eq!(local.target_occurrences()[0].0, TargetRef::Object(game.d));
            assert!(local.target_occurrences()[0].1.is_some());
            pin_mogg(
                &mut game.runner,
                if copy {
                    &[true, true, true, true]
                } else {
                    &[true, true]
                },
            );
            pass_mogg_priority(&mut game.runner);
            nested_finished(
                &game,
                if copy {
                    &[game.b, game.c, game.d]
                } else {
                    &[game.b, game.d]
                },
            );
        }
    }
}

#[test]
fn branch_only_all_exposes_declared_custody_but_single_and_forced_never_alias_root() {
    for scope in 4..7 {
        let program = nested_flip(
            nested_declaration(2).sub_ability(nested_reader(2)),
            nested_declaration(3).sub_ability(nested_reader(3)),
        );
        let mut game = nested_game(program, false);
        nested_begin(&mut game);
        let (c, d) = (game.c, game.d);
        nested_choose(&mut game, P0, c);
        nested_choose(&mut game, P0, d);
        let original_id = game.runner.state().stack.back().unwrap().id;
        let original = game
            .runner
            .state()
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .clone();
        assert!(original.targets.is_empty());
        assert_eq!(
            engine::game::ability_utils::declared_targets_in_chain(&original),
            vec![TargetRef::Object(c), TargetRef::Object(d)]
        );
        game.runner
            .cast(game.responses[scope])
            .target_object(original_id)
            .commit();
        pin_mogg(&mut game.runner, &[true]);
        let events = pass_mogg_priority(&mut game.runner);
        if scope == 4 {
            let WaitingFor::RetargetChoice {
                slots,
                current_targets,
                slot_pools,
                ..
            } = &game.runner.state().waiting_for
            else {
                panic!("branch-only All has two real positions")
            };
            assert_eq!(slots.len(), 2);
            assert_eq!(
                current_targets,
                &vec![TargetRef::Object(c), TargetRef::Object(d)]
            );
            assert!(slots.iter().all(|slot| matches!(
                slot.path.first(),
                Some(engine::types::game_state::ChainStep::CoinBranch(_))
            )));
            assert!(slot_pools[0].contains(&TargetRef::Object(d)));
            game.runner
                .act(GameAction::RetargetSpell {
                    new_targets: vec![Some(TargetRef::Object(d)), None],
                })
                .unwrap();
            pass_mogg_priority(&mut game.runner);
            nested_finished(&game, &[d]);
        } else {
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::ChangeTargets,
                        ..
                    }
                )),
                "conservative operation completes"
            );
            nested_finished(&game, &[c]);
        }
    }
}

#[test]
fn paid_instead_nested_shape_owns_its_selected_custody_through_swap_and_all_admissions() {
    // CR 614.1a and CR 614.15: the selected self-replacement substitutes its
    // own effect shape; payment remains the actual cast's additional cost.
    use engine::types::ability::{
        AbilityCost, AbilityKind, AdditionalCost, AdditionalCostRepeatability, QuantityExpr,
        TargetFilter,
    };
    for conditional in [false, true] {
        for inner in [false, true] {
            for codec in 0..4 {
                let mut program = nested_program(false, false);
                let second = program.sub_ability.as_mut().unwrap();
                let mut selected = second.sub_ability.take().unwrap();
                selected.condition = Some(if conditional {
                    AbilityCondition::ConditionInstead {
                        inner: Box::new(AbilityCondition::additional_cost_paid_any()),
                    }
                } else {
                    AbilityCondition::AdditionalCostPaidInstead
                });
                second.sub_ability = Some(Box::new(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 10 },
                            player: TargetFilter::Controller,
                        },
                    )
                    .sub_ability(*selected),
                ));
                let mut game = nested_game(program, true);
                // Ordinary printed-class cost setup, before cast; no resolved
                // occurrence/capture/state is supplied by the fixture.
                game.runner
                    .state_mut()
                    .objects
                    .get_mut(&game.spell)
                    .unwrap()
                    .additional_cost = Some(AdditionalCost::Kicker {
                    costs: vec![AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    }],
                    repeatability: AdditionalCostRepeatability::Once,
                });
                nested_begin(&mut game);
                let WaitingFor::OptionalCostChoice {
                    player,
                    times_kicked,
                    ..
                } = game.runner.state().waiting_for
                else {
                    panic!("actual paid-instead intention")
                };
                assert_eq!(player, P0);
                assert_eq!(times_kicked, 0);
                game.runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .unwrap();
                let (a, b, c) = (game.a, game.b, game.c);
                nested_choose(&mut game, P0, a);
                nested_choose(&mut game, P1, b);
                nested_choose(&mut game, P0, c);
                assert_eq!(game.runner.state().players[0].mana_pool.total(), 0);
                let root = game.runner.state().stack.back().unwrap().ability().unwrap();
                assert!(root.context.additional_cost_paid);
                let bindings = engine::game::ability_utils::chain_retarget_slots(root);
                assert_eq!(bindings.len(), 3);
                assert!(bindings[2].address.path.iter().any(|step| matches!(
                    step,
                    engine::types::game_state::ChainStep::CoinBranch(_)
                )));
                assert_no_public_group_capture(game.runner.state());
                pin_mogg(&mut game.runner, &[false, true, !inner, inner]);
                game.runner = nested_roundtrip(game.runner, codec);
                pass_mogg_priority(&mut game.runner);
                for level in 0..2 {
                    let frame = game
                        .runner
                        .state()
                        .active_coin_flip_frame()
                        .expect("selected swap reaches real coin pause");
                    assert!(!frame.branch_context.coin_branch_target_abilities.is_empty());
                    assert_eq!(frame.source_id, game.spell);
                    let WaitingFor::CoinFlipKeepChoice {
                        player, results, ..
                    } = &game.runner.state().waiting_for
                    else {
                        panic!("actual swap keep")
                    };
                    assert_eq!(*player, P0);
                    assert_eq!(
                        results,
                        if level == 0 {
                            &vec![false, true]
                        } else {
                            &vec![!inner, inner]
                        }
                    );
                    game.runner = nested_roundtrip(game.runner, codec);
                    game.runner
                        .act(GameAction::SelectCoinFlips {
                            keep_indices: vec![1],
                        })
                        .unwrap();
                }
                game.runner.advance_until_stack_empty();
                assert_eq!(
                    game.runner.state().players[0].life,
                    20,
                    "replaced parent gain10 never executes"
                );
                nested_finished(&game, &[game.b, if inner { game.c } else { game.a }]);
            }
        }
    }
}

#[test]
fn selected_nested_source_producer_prompts_fresh_and_unselected_custody_installs_nothing() {
    // CR 609.7a: only actual execution chooses a source; announcement custody
    // cannot own a source-choice result or materialize a prevention shield.
    use engine::types::ability::{AbilityKind, TargetFilter};
    for selected in [false, true] {
        for codec in 0..4 {
            let mut materializer=parse_oracle_text("The next time a source of your choice would deal damage to you this turn, prevent that damage.",
                "Constructed fresh source materializer",&[],&["Instant".into()],&[]).abilities[0].clone();
            let Effect::PreventDamage {
                damage_source_filter,
                ..
            } = materializer.effect.as_mut()
            else {
                panic!("ordinary supported prevention class")
            };
            *damage_source_filter = Some(TargetFilter::ChosenDamageSource {
                chooser: None,
                filter: None,
            });
            let producer = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseDamageSource {
                    source_filter: TargetFilter::Any,
                },
            )
            .sub_ability(materializer);
            let mut program = nested_program(false, false);
            let flip = program
                .sub_ability
                .as_mut()
                .unwrap()
                .sub_ability
                .as_mut()
                .unwrap();
            let Effect::FlipCoins { lose_effect, .. } = flip.effect.as_mut() else {
                unreachable!()
            };
            *lose_effect = Some(Box::new(producer));
            let mut game = nested_game(program, false);
            nested_announce(&mut game, false, false);
            assert!(game.runner.state().pending_damage_replacements.is_empty());
            pin_mogg(
                &mut game.runner,
                if selected { &[false] } else { &[true, true] },
            );
            pass_mogg_priority(&mut game.runner);
            if selected {
                let WaitingFor::DamageSourceChoice {
                    player, options, ..
                } = &game.runner.state().waiting_for
                else {
                    panic!("selected branch must create its own fresh source prompt")
                };
                assert_eq!(*player, P0);
                let reference = options
                    .iter()
                    .find(|option| option.identity.reference.object_id == game.d)
                    .unwrap()
                    .identity
                    .reference;
                assert!(options
                    .iter()
                    .any(|option| option.identity.reference.object_id == game.c));
                assert!(game.runner.state().pending_damage_replacements.is_empty());
                game.runner = nested_roundtrip(game.runner, codec);
                assert!(engine::game::engine::apply(
                    game.runner.state_mut(),
                    P1,
                    GameAction::ChooseDamageSource { source: reference }
                )
                .is_err());
                game.runner
                    .act(GameAction::ChooseDamageSource { source: reference })
                    .unwrap();
                game.runner.advance_until_stack_empty();
                assert_eq!(game.runner.state().pending_damage_replacements.len(), 1);
                assert_eq!(
                    game.runner.state().pending_damage_replacements[0]
                        .selected_damage_source
                        .as_ref()
                        .unwrap()
                        .identity
                        .reference,
                    reference
                );
                nested_finished(&game, &[]);
            } else {
                assert!(
                    game.runner.state().pending_damage_replacements.is_empty(),
                    "unselected source producer installs no authority"
                );
                nested_finished(&game, &[game.b, game.c]);
            }
        }
    }
}

#[test]
fn nested_distinct_declaring_instances_can_capture_the_same_artifact_independently() {
    // CR 115.3: distinct target instances do not impose global distinctness.
    for outer in [false, true] {
        let mut game = nested_game(nested_program(false, true), false);
        nested_begin(&mut game);
        let (a, b, c) = (game.a, game.b, game.c);
        nested_choose(&mut game, P0, a);
        nested_choose(&mut game, P1, b);
        nested_choose(&mut game, P0, c);
        nested_choose(&mut game, P0, c);
        let root = game.runner.state().stack.back().unwrap().ability().unwrap();
        let groups = &root.context.chosen_group_targets;
        assert_ne!(groups[2].0, groups[3].0);
        assert_eq!(groups[2].1, groups[3].1);
        assert_eq!(groups[2].1[0].0, TargetRef::Object(c));
        assert!(groups[2].1[0].1.is_some());
        pin_mogg(&mut game.runner, &[outer, true]);
        pass_mogg_priority(&mut game.runner);
        nested_finished(&game, if outer { &[game.b, c] } else { &[c] });
    }
}

#[test]
fn nested_new_birth_keep_is_stale_but_actual_reelection_refreshes_only_that_pin() {
    // CR 400.7 and CR 115.7d: keeping retains the old occurrence; expressly
    // choosing that same ID again elects its current new incarnation.
    for reelect in [false, true] {
        for codec in 0..4 {
            let mut game = nested_game(nested_program(false, false), false);
            game.runner
                .state_mut()
                .objects
                .get_mut(&game.c)
                .unwrap()
                .owner = P0;
            nested_announce(&mut game, false, false);
            let original = game
                .runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .clone();
            let bindings = engine::game::ability_utils::chain_retarget_slots(&original);
            let before =
                nested_node(&original, &bindings[2].address.path).target_occurrences()[0].1;
            let c = game.c;
            nested_response(&mut game, 1, Some(c));
            assert_eq!(game.runner.state().objects[&c].zone, Zone::Battlefield);
            // Existing scenario mana funding for the second real response.
            game.runner.state_mut().players[0]
                .mana_pool
                .add(engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                ));
            let original_id = game.runner.state().stack.back().unwrap().id;
            game.runner
                .cast(game.responses[4])
                .target_object(original_id)
                .commit();
            pass_mogg_priority(&mut game.runner);
            let WaitingFor::RetargetChoice {
                current_targets,
                slot_pools,
                ..
            } = &game.runner.state().waiting_for
            else {
                panic!("actual post-birth reelection")
            };
            assert_eq!(current_targets[2], TargetRef::Object(c));
            assert!(slot_pools[2].contains(&TargetRef::Object(c)));
            game.runner = nested_roundtrip(game.runner, codec);
            game.runner
                .act(GameAction::RetargetSpell {
                    new_targets: vec![None, None, reelect.then_some(TargetRef::Object(c))],
                })
                .unwrap();
            let changed = game.runner.state().stack.back().unwrap().ability().unwrap();
            let after = nested_node(changed, &bindings[2].address.path).target_occurrences()[0].1;
            assert_eq!(before == after, !reelect);
            assert_eq!(
                nested_node(changed, &bindings[0].address.path).target_occurrences(),
                nested_node(&original, &bindings[0].address.path).target_occurrences()
            );
            assert_eq!(
                nested_node(changed, &bindings[1].address.path).target_occurrences(),
                nested_node(&original, &bindings[1].address.path).target_occurrences()
            );
            pin_mogg(&mut game.runner, &[true, true]);
            pass_mogg_priority(&mut game.runner);
            nested_finished(&game, if reelect { &[game.b, c] } else { &[game.b] });
        }
    }
}

#[test]
fn authentic_old_local_chosen_return_keeps_encoded_flat_target_without_phantom_announcement() {
    use engine::types::ability::{AbilityKind, ChosenGroupId, TargetFilter, TypedFilter};
    use engine::types::resolution::ResolutionStateWire;
    use serde_json::{json, Value};
    // BASE's actual old writer/delivery contract was FlipCoin + Single,
    // default branch_context, and build(def, frame.targets, chain_root).
    // The primary has no external reader or owed capture. The separately
    // labelled structural control retains a genuinely action-captured outer
    // group0; it makes no pre-group historical-writer claim for that map.
    for inherited_group_control in [false, true] {
        for persisted in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_artifact_from_oracle(P0, "Krark's Thumb", THUMB);
            let a = scenario.add_creature(P0, "Outer producer decoy", 2, 4).id();
            let b = scenario
                .add_creature(P1, "Original encoded local recipient", 2, 4)
                .id();
            let decoy = scenario
                .add_creature(P1, "Unselected population decoy", 2, 4)
                .id();
            let bare = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::TargetOnly {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            );
            let carried = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BounceAll {
                    target: TargetFilter::ParentTarget,
                    destination: Some(Zone::Hand),
                    count: None,
                },
            );
            let no_op = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
            let mut program = bare.clone().sub_ability(nested_flip(carried, no_op));
            if inherited_group_control {
                let mut prefix = bare;
                prefix.declares_chosen_group = Some(ChosenGroupId(0));
                program = prefix.sub_ability(program);
            }
            let spell = scenario
                .add_spell_to_hand(P0, "Constructed authentic old local program", true)
                .with_mana_cost(ManaCost::generic(1))
                .with_ability_definition(program)
                .id();
            scenario.with_mana_pool(
                P0,
                vec![engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let mut runner = scenario.build();
            pin_mogg(&mut runner, &[false, true]);
            if inherited_group_control {
                runner.cast(spell).target_objects(&[a, b]).resolve();
            } else {
                runner.cast(spell).target_object(b).resolve();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { .. }
            ));
            let frame = runner.state().active_coin_flip_frame().unwrap();
            assert_eq!(frame.targets, vec![TargetRef::Object(b)]);
            if inherited_group_control {
                assert_eq!(
                    frame.branch_context.chosen_group_targets[0].1[0].0,
                    TargetRef::Object(a)
                );
            } else {
                assert!(frame.branch_context.chosen_group_targets.is_empty());
            }
            let local = parse_oracle_text(
                "Choose target creature. Return each chosen creature to its owner's hand.",
                "Constructed encoded old local body",
                &[],
                &["Instant".into()],
                &[],
            )
            .abilities[0]
                .clone();
            assert_eq!(local.declares_chosen_group, Some(ChosenGroupId(0)));
            assert_eq!(
                local.sub_ability.as_ref().unwrap().reads_chosen_group,
                Some(ChosenGroupId(0))
            );
            let local = serde_json::to_value(local).unwrap();
            runner.state_mut().capture_rng_word_pos();
            let pos = runner.state().rng_word_pos;
            let mut encoded = if persisted {
                serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap()
            } else {
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap()
            };
            fn empty_custody(value: &Value) {
                match value {
                    Value::Object(fields) => {
                        if let Some(targets) = fields.get("targets") {
                            assert!(targets.as_array().is_some_and(Vec::is_empty),"only unowned empty current template scaffolding is absent in old writer");
                        }
                        for value in fields.values() {
                            empty_custody(value);
                        }
                    }
                    Value::Array(items) => {
                        for value in items {
                            empty_custody(value);
                        }
                    }
                    _ => {}
                }
            }
            fn old_writer(value: &mut Value, local: &Value, inherited: bool) -> (usize, usize) {
                match value {
                    Value::Object(fields) => {
                        if let Some(custody) = fields.remove("coin_branch_target_abilities") {
                            empty_custody(&custody);
                        }
                        let mut programs = 0;
                        let mut frames = 0;
                        if fields.get("type").and_then(Value::as_str) == Some("FlipCoins") {
                            if fields.get("win_effect").is_some_and(|win| !win.is_null()) {
                                fields.insert("win_effect".into(), local.clone());
                                programs += 1;
                            }
                            let flips = fields.remove("flips").unwrap();
                            assert_eq!(flips["type"], json!("Exactly"));
                            if flips["count"] == json!({"type":"Fixed","value":1}) {
                                fields.insert("type".into(), json!("FlipCoin"));
                            } else {
                                // The Thumb's own old multiple-flip writer.
                                fields.insert("count".into(), flips["count"].clone());
                            }
                        }
                        if fields.contains_key("source_id")
                            && fields.contains_key("chain_root_targets")
                            && fields.contains_key("kind")
                        {
                            fields.insert("win_effect".into(), local.clone());
                            fields.insert("kind".into(), json!("Single"));
                            if !inherited {
                                fields.remove("branch_context");
                            } // truly default original inherited context
                            frames += 1;
                        }
                        if fields.contains_key("resolution_state_version") {
                            fields.insert("resolution_state_version".into(), json!(7));
                        }
                        for value in fields.values_mut() {
                            let (p, f) = old_writer(value, local, inherited);
                            programs += p;
                            frames += f;
                        }
                        (programs, frames)
                    }
                    Value::Array(items) => items
                        .iter_mut()
                        .map(|value| old_writer(value, local, inherited))
                        .fold((0, 0), |(p, f), (a, b)| (p + a, f + b)),
                    _ => (0, 0),
                }
            }
            let (programs, frames) = old_writer(&mut encoded, &local, inherited_group_control);
            assert!(
                programs >= 1,
                "whole matching original encoded program, not just pending frame"
            );
            assert_eq!(frames, 1);
            runner = if persisted {
                GameRunner::from_state(
                    serde_json::from_value::<PersistedGameState>(encoded)
                        .unwrap()
                        .into_game_state()
                        .unwrap(),
                )
            } else {
                GameRunner::from_state(
                    serde_json::from_value::<ResolutionStateWire>(encoded)
                        .unwrap()
                        .into_game_state(),
                )
            };
            let expected = if inherited_group_control {
                vec![TargetRef::Object(a), TargetRef::Object(b)]
            } else {
                vec![TargetRef::Object(b)]
            };
            assert_eq!(
                engine::game::ability_utils::declared_targets_in_chain(
                    runner
                        .state()
                        .resolving_stack_entry
                        .as_ref()
                        .unwrap()
                        .ability()
                        .unwrap()
                ),
                expected,
                "legacy normalization adds no new announcement"
            );
            assert_eq!(
                runner.state().active_coin_flip_frame().unwrap().targets,
                vec![TargetRef::Object(b)]
            );
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1],
                })
                .unwrap();
            runner.advance_until_stack_empty();
            runner.state_mut().capture_rng_word_pos();
            assert_eq!(
                runner.state().rng_word_pos,
                pos,
                "old local resume does not reflip"
            );
            assert_eq!(runner.state().objects[&b].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&a].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
        }
    }
}

fn assert_mogg_finished(game: &MoggGame, destroyed: &[ObjectId]) {
    assert!(matches!(
        game.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(game.runner.state().stack.is_empty());
    assert!(game.runner.state().resolution_stack.is_empty());
    for id in [game.first, game.second]
        .into_iter()
        .chain(game.decoys.iter().copied())
    {
        assert_eq!(
            game.runner.state().objects[&id].zone,
            if destroyed.contains(&id) {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            },
            "exact chosen group payoff for {id:?}"
        );
    }
}

fn pass_mogg_priority(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = vec![];
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return events;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("ordinary priority resolution")
                .events,
        );
    }
    panic!("Mogg priority resolution exceeded its bound");
}

#[test]
fn mogg_actual_tap_win_and_loss_destroy_only_their_named_group() {
    for won in [true, false] {
        let mut game = mogg_game(false);
        announce_mogg(&mut game, false);
        assert_no_public_group_capture(game.runner.state());
        let seed = if won { 0 } else { 1 };
        game.runner.state_mut().rng_seed = seed;
        game.runner.state_mut().rng_word_pos = 0;
        game.runner.state_mut().rng = engine::types::game_state::GameRng::seed_from_u64(seed);
        let events = pass_mogg_priority(&mut game.runner);
        assert!(events.iter().any(|event| matches!(event, GameEvent::CoinFlipped { player_id: P0, won: actual } if *actual == won)));
        assert_mogg_finished(&game, &[if won { game.first } else { game.second }]);
    }
}

#[test]
fn mogg_two_target_instances_allow_same_object_with_independent_pins() {
    for won in [true, false] {
        let mut game = mogg_game(false);
        announce_mogg(&mut game, true);
        let ability = game.runner.state().stack.back().unwrap().ability().unwrap();
        assert_ne!(
            ability.context.chosen_group_targets[0].0,
            ability.context.chosen_group_targets[1].0
        );
        pin_mogg(&mut game.runner, &[won]);
        pass_mogg_priority(&mut game.runner);
        assert_mogg_finished(&game, &[game.first]);
    }
}

#[test]
fn mogg_announced_live_stack_capture_survives_both_authoritative_readers_before_coin() {
    use engine::types::resolution::ResolutionStateWire;
    for persisted in [false, true] {
        let mut game = mogg_game(false);
        announce_mogg(&mut game, false);
        let expected = game
            .runner
            .state()
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .context
            .chosen_group_targets
            .clone();
        assert_no_public_group_capture(game.runner.state());
        game.runner = if persisted {
            persisted_roundtrip(game.runner.state())
        } else {
            let value = serde_json::to_value(ResolutionStateWire::from_game_state(
                game.runner.state().clone(),
            ))
            .unwrap();
            GameRunner::from_state(
                serde_json::from_value::<ResolutionStateWire>(value)
                    .unwrap()
                    .into_game_state(),
            )
        };
        assert_eq!(
            game.runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .context
                .chosen_group_targets,
            expected
        );
        pin_mogg(&mut game.runner, &[true]);
        let events = pass_mogg_priority(&mut game.runner);
        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::CoinFlipped { won: true, .. })));
        assert_mogg_finished(&game, &[game.first]);
    }
}

#[test]
fn mogg_krark_nondefault_keep_survives_both_canonical_codecs_without_reflip() {
    use engine::types::resolution::ResolutionStateWire;
    for persisted in [false, true] {
        let mut game = mogg_game(true);
        announce_mogg(&mut game, false);
        pin_mogg(&mut game.runner, &[true, false]);
        pass_mogg_priority(&mut game.runner);
        let WaitingFor::CoinFlipKeepChoice {
            player,
            results,
            keep_count,
        } = &game.runner.state().waiting_for
        else {
            panic!("actual Krark keep pause");
        };
        assert_eq!(*player, P0);
        assert_eq!(*keep_count, 1);
        assert_eq!(results, &[true, false]);
        assert_no_public_group_capture(game.runner.state());
        let capture = game
            .runner
            .state()
            .active_coin_flip_frame()
            .unwrap()
            .branch_context
            .chosen_group_targets
            .clone();
        assert_eq!(capture.len(), 2);
        game.runner.state_mut().capture_rng_word_pos();
        let word_pos = game.runner.state().rng_word_pos;
        game.runner = if persisted {
            persisted_roundtrip(game.runner.state())
        } else {
            let value = serde_json::to_value(ResolutionStateWire::from_game_state(
                game.runner.state().clone(),
            ))
            .unwrap();
            GameRunner::from_state(
                serde_json::from_value::<ResolutionStateWire>(value)
                    .unwrap()
                    .into_game_state(),
            )
        };
        assert_eq!(
            game.runner
                .state()
                .active_coin_flip_frame()
                .unwrap()
                .branch_context
                .chosen_group_targets,
            capture
        );
        game.runner
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![1],
            })
            .expect("nondefault loss result after restore");
        game.runner.advance_until_stack_empty();
        game.runner.state_mut().capture_rng_word_pos();
        assert_eq!(
            game.runner.state().rng_word_pos,
            word_pos,
            "resume must not reflip"
        );
        assert_mogg_finished(&game, &[game.second]);
    }
}

#[test]
fn mogg_invalid_first_or_second_occurrence_does_not_borrow_other_group_or_new_birth() {
    for (won, stale_group, reenter) in [
        (true, 0, false),
        (false, 1, false),
        (true, 0, true),
        (false, 1, true),
    ] {
        let mut game = mogg_game(false);
        announce_mogg(&mut game, false);
        let stale = if stale_group == 0 {
            game.first
        } else {
            game.second
        };
        move_to_zone(game.runner.state_mut(), stale, Zone::Graveyard, &mut vec![]);
        if reenter {
            move_to_zone(
                game.runner.state_mut(),
                stale,
                Zone::Battlefield,
                &mut vec![],
            );
        }
        pin_mogg(&mut game.runner, &[won]);
        let events = pass_mogg_priority(&mut game.runner);
        assert!(events.iter().any(|event| matches!(event, GameEvent::CoinFlipped { won: actual, .. } if *actual == won)), "other target keeps the ability live");
        assert_eq!(
            game.runner.state().objects[&stale].zone,
            if reenter {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        let other = if stale_group == 0 {
            game.second
        } else {
            game.first
        };
        assert_eq!(game.runner.state().objects[&other].zone, Zone::Battlefield);
        assert!(game.runner.state().stack.is_empty());
        assert!(game.runner.state().resolution_stack.is_empty());
        assert!(matches!(
            game.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn mogg_all_invalid_paid_activation_fizzles_with_tap_paid() {
    let mut game = mogg_game(false);
    announce_mogg(&mut game, false);
    for id in [game.first, game.second] {
        move_to_zone(game.runner.state_mut(), id, Zone::Graveyard, &mut vec![]);
    }
    let events = pass_mogg_priority(&mut game.runner);
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::CoinFlipped { .. })));
    assert!(game.runner.state().objects[&game.source].tapped);
    assert_mogg_finished(&game, &[game.first, game.second]);
}

#[test]
fn paid_spell_foreign_first_chooser_rejects_caster_and_accepts_nondefault_opponent() {
    // Explicit class composition, not fabricated printed Mogg text.
    let text = "Choose target creature of an opponent's choice. Return each chosen creature to its owner's hand.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "First eligible", 2, 2).id();
    let selected = scenario.add_creature(P0, "Nondefault eligible", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Foreign chooser class witness", true, text)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert_mogg_actor(&runner, P1, selected);
    assert!(engine::game::engine::apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(selected))
        }
    )
    .is_err());
    engine::game::engine::apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(selected)),
        },
    )
    .unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&selected].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn skipped_optional_prefix_and_loyalty_start_at_actual_foreign_current_slot() {
    // CR 606.3: the loyalty sibling starts in its controller's main phase;
    // CR 601.2c chooses only the remaining required target after an empty prefix.
    const BODY: &str = "Choose up to one target artifact you control. Choose target creature of an opponent's choice. Return each creature that opponent chose to its owner's hand.";
    for loyalty in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let decoy = scenario.add_creature(P0, "First legal creature", 2, 2).id();
        let selected = scenario.add_creature(P0, "Nondefault creature", 2, 2).id();
        let source = if loyalty {
            scenario
                .add_planeswalker_from_oracle(
                    P0,
                    "Synthetic chooser walker",
                    "Witness",
                    3,
                    &format!("+1: {BODY}"),
                )
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Synthetic skipped prefix spell", true, BODY)
                .with_mana_cost(ManaCost::generic(1))
                .id()
        };
        scenario.with_mana_pool(
            P0,
            vec![engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let mut runner = scenario.build();
        let action = if loyalty {
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            }
        } else {
            GameAction::CastSpell {
                object_id: source,
                card_id: runner.state().objects[&source].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            }
        };
        runner
            .act(action)
            .expect("real producer skips empty optional artifact prefix");
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!("foreign initial prompt after prefix");
        };
        assert_eq!(selection.current_slot, 1);
        assert_eq!(selection.selected_slots, vec![None]);
        assert_mogg_actor(&runner, P1, selected);
        assert!(engine::game::engine::apply(
            runner.state_mut(),
            P0,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(selected))
            }
        )
        .is_err());
        engine::game::engine::apply(
            runner.state_mut(),
            P1,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(selected)),
            },
        )
        .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&selected].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
        if loyalty {
            assert_eq!(
                runner.state().objects[&source]
                    .counters
                    .get(&engine::types::counter::CounterType::Loyalty),
                Some(&4)
            );
        } else {
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        }
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn unrelated_earlier_target_cannot_supply_dependent_mogg_style_announcer_or_branch() {
    const TEXT: &str = "Target creature gets +1/+1 until end of turn. You choose target creature an opponent controls, and that opponent chooses target creature. Flip a coin. If you win the flip, destroy the creature you chose. If you lose the flip, destroy the creature your opponent chose.";
    for won in [true, false] {
        let mut scenario = GameScenario::new_n_player(3, 12);
        scenario.at_phase(Phase::PreCombatMain);
        let unrelated = scenario.add_creature(P1, "Earlier target decoy", 2, 4).id();
        let first = scenario
            .add_creature(P0, "Actual group zero", 2, 4)
            .controlled_by(THIRD_SEAT)
            .id();
        let second = scenario.add_creature(P0, "Actual group one", 2, 4).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic Mogg class composition", true, TEXT)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        for target in [unrelated, first] {
            assert_mogg_actor(&runner, P0, target);
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap();
        }
        assert_mogg_actor(&runner, THIRD_SEAT, second);
        assert!(engine::game::engine::apply(
            runner.state_mut(),
            P1,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second))
            }
        )
        .is_err());
        engine::game::engine::apply(
            runner.state_mut(),
            THIRD_SEAT,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second)),
            },
        )
        .unwrap();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        pin_mogg(&mut runner, &[won]);
        let events = pass_mogg_priority(&mut runner);
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::CoinFlipped { won: actual, .. } if *actual == won)
        ));
        assert_eq!(runner.state().objects[&unrelated].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&unrelated].power, Some(3));
        assert_eq!(
            runner.state().objects[&first].zone,
            if won {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(
            runner.state().objects[&second].zone,
            if won {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn current_inherited_map_only_coin_body_keeps_distinct_flat_target_at_both_readers() {
    use engine::types::ability::{
        AbilityKind, ChosenGroupId, QuantityExpr, TargetFilter, TypedFilter,
    };

    // Secondary typed class composition of the ordinary two-target producer
    // and bare GainLife -> Destroy program below, not printed Oracle text.
    // The first declaration supplies only inherited group input; the second
    // supplies the distinct flat ParentTarget used by the selected body.
    for thumb in [false, true] {
        for codec in [0, 3] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let thumb_id = thumb.then(|| {
                scenario
                    .add_artifact_from_oracle(P0, "Krark's Thumb", THUMB)
                    .id()
            });
            let anchor = scenario.add_creature(P0, "Inherited map anchor", 2, 3).id();
            let victim = scenario
                .add_creature(P1, "Current flat recipient", 2, 3)
                .id();
            let decoy = scenario
                .add_creature(P1, "Unselected current decoy", 2, 3)
                .id();
            let win = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::ParentTarget,
                    cant_regenerate: false,
                },
            ));
            let bare = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::TargetOnly {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            );
            let mut prefix = bare.clone();
            prefix.declares_chosen_group = Some(ChosenGroupId(0));
            let program = prefix.sub_ability(bare.sub_ability(nested_flip(
                win,
                AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
            )));
            let spell = scenario
                .add_spell_to_hand(P0, "Constructed current inherited-input coin program", true)
                .with_mana_cost(ManaCost::generic(1))
                .with_ability_definition(program)
                .id();
            scenario.with_mana_pool(
                P0,
                vec![engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let mut runner = scenario.build();
            pin_mogg(&mut runner, if thumb { &[false, true] } else { &[true] });
            runner
                .cast(spell)
                .target_objects(&[anchor, victim])
                .commit();
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
            let root = runner.state().stack.back().unwrap().ability().unwrap();
            let flip = root
                .sub_ability
                .as_ref()
                .unwrap()
                .sub_ability
                .as_ref()
                .unwrap();
            // CR 601.2c: the two actual announcements own separate targets.
            assert_eq!(root.targets, vec![TargetRef::Object(anchor)]);
            assert_eq!(
                root.sub_ability.as_ref().unwrap().targets,
                vec![TargetRef::Object(victim)]
            );
            assert!(
                flip.targets.is_empty(),
                "flat input arrives during chain execution"
            );
            let inherited = flip.context.chosen_group_targets.clone();
            assert_eq!(inherited.len(), 1);
            assert_eq!(inherited[0].0, ChosenGroupId(0));
            assert_eq!(inherited[0].1, root.target_occurrences());
            assert!(inherited[0].1[0].1.is_some());
            let captured = flip.coin_branch_target_abilities.clone();
            assert_eq!(captured.len(), 2);
            for (_, child) in &captured {
                let mut cursor = Some(child.as_ref());
                while let Some(node) = cursor {
                    assert!(node.declares_chosen_group.is_none());
                    assert!(node.reads_chosen_group.is_none());
                    assert!(node.targets.is_empty() && node.target_pins.is_empty());
                    assert!(node.chosen_target_slots.slots().is_empty());
                    assert_eq!(node.context.chosen_group_targets, inherited);
                    assert_eq!(node.source_id, spell);
                    assert_eq!(node.controller, P0);
                    assert_eq!(node.cast_occurrence, root.cast_occurrence);
                    assert!(node.cast_occurrence.is_some());
                    assert_eq!(node.source_incarnation, root.source_incarnation);
                    cursor = node.sub_ability.as_deref();
                }
            }
            // Both canonical readers retain this current, meaningful capture.
            runner = nested_roundtrip(runner, codec);
            let restored = runner.state().stack.back().unwrap().ability().unwrap();
            assert_eq!(
                restored
                    .sub_ability
                    .as_ref()
                    .unwrap()
                    .sub_ability
                    .as_ref()
                    .unwrap()
                    .coin_branch_target_abilities,
                captured
            );
            let mut events = pass_mogg_priority(&mut runner);
            if thumb {
                let WaitingFor::CoinFlipKeepChoice {
                    player,
                    results,
                    keep_count,
                } = &runner.state().waiting_for
                else {
                    panic!("actual Thumb keep pause");
                };
                assert_eq!(*player, P0);
                assert_eq!(results, &vec![false, true]);
                assert_eq!(*keep_count, 1);
                let frame = runner.state().active_coin_flip_frame().unwrap();
                assert_eq!(frame.targets, vec![TargetRef::Object(victim)]);
                assert_eq!(frame.branch_context.chosen_group_targets, inherited);
                assert_eq!(frame.branch_context.coin_branch_target_abilities.len(), 2);
                for (_, child) in &frame.branch_context.coin_branch_target_abilities {
                    assert_eq!(child.context.chosen_group_targets, inherited);
                    assert!(child.cast_occurrence.is_some());
                    assert_eq!(child.source_id, spell);
                    assert_eq!(child.controller, P0);
                    assert!(child.targets.is_empty() && child.target_pins.is_empty());
                }
                assert!(!frame.branch_context.is_empty());
                assert_eq!(
                    serde_json::to_value(frame).unwrap()["branch_context"],
                    serde_json::to_value(&frame.branch_context).unwrap(),
                    "current writer must retain the owed capture"
                );
                let context = frame.branch_context.clone();
                runner.state_mut().capture_rng_word_pos();
                let pos = runner.state().rng_word_pos;
                runner = nested_roundtrip(runner, codec);
                assert_eq!(
                    runner
                        .state()
                        .active_coin_flip_frame()
                        .unwrap()
                        .branch_context,
                    context
                );
                // CR 705.2: keep the nondefault winning result through apply.
                events.extend(
                    runner
                        .act(GameAction::SelectCoinFlips {
                            keep_indices: vec![1],
                        })
                        .unwrap()
                        .events,
                );
                runner.state_mut().capture_rng_word_pos();
                assert_eq!(runner.state().rng_word_pos, pos, "Keep must not reflip");
            }
            runner.advance_until_stack_empty();
            // CR 608.2c: the selected body gains life and then affects its
            // genuine flat target; the inherited named map is a separate input.
            assert!(events.iter().any(|event| matches!(
                event,
                GameEvent::CoinFlipped {
                    player_id: P0,
                    won: true
                }
            )));
            assert_eq!(runner.state().players[0].life, 22);
            assert_eq!(runner.state().players[1].life, 20);
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&anchor].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
            if let Some(thumb_id) = thumb_id {
                assert_eq!(runner.state().objects[&thumb_id].zone, Zone::Battlefield);
            }
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(runner.state().active_ability_continuation().is_none());
        }
    }
}

#[test]
fn authentic_old_coin_kinds_with_no_external_reader_keep_encoded_program_at_both_readers() {
    use engine::types::ability::{
        AbilityKind, CoinFlipRun, QuantityExpr, TargetFilter, TypedFilter,
    };
    use engine::types::resolution::ResolutionStateWire;
    use serde_json::json;
    for (old_kind, run, sequence, expected_life) in [
        (
            json!("Single"),
            CoinFlipRun::single(),
            vec![false, true],
            22,
        ),
        (
            json!({"FlipN":{"remaining":1}}),
            CoinFlipRun::Exactly {
                count: QuantityExpr::Fixed { value: 2 },
            },
            vec![false, true, true, false],
            24,
        ),
        (
            json!({"UntilLose":{"wins_so_far":0}}),
            CoinFlipRun::UntilLoss,
            vec![true, false],
            20,
        ),
    ] {
        for persisted in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_artifact_from_oracle(P0, "Krark's Thumb", THUMB);
            let victim = scenario
                .add_creature(P1, "Old bare-target recipient", 2, 3)
                .id();
            let decoy = scenario
                .add_creature(P1, "Old bare-target decoy", 2, 3)
                .id();
            // Exact old target-bearing program class: gain two life per win,
            // then destroy the explicitly encoded bare parent target. No
            // chosen declaration/reader metadata or owed map exists here.
            let win = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::ParentTarget,
                    cant_regenerate: false,
                },
            ));
            let flip = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    flips: run.clone(),
                    win_effect: Some(Box::new(win)),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
            );
            let program = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::TargetOnly {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            )
            .sub_ability(flip);
            let spell = scenario
                .add_spell_to_hand(P0, "Old coin program witness", true)
                .with_mana_cost(ManaCost::generic(1))
                .with_ability_definition(program)
                .id();
            scenario.with_mana_pool(
                P0,
                vec![engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(0),
                    false,
                    vec![],
                )],
            );
            let mut runner = scenario.build();
            pin_mogg(&mut runner, &sequence);
            runner.cast(spell).target_object(victim).resolve();
            let WaitingFor::CoinFlipKeepChoice {
                player, results, ..
            } = &runner.state().waiting_for
            else {
                panic!("real legacy-program keep pause");
            };
            assert_eq!(*player, P0);
            assert_eq!(results, &sequence[..2]);
            assert_eq!(
                runner.state().active_coin_flip_frame().unwrap().targets,
                vec![TargetRef::Object(victim)],
                "authentic nonempty bare-target program is retained"
            );
            assert!(
                !runner
                    .state()
                    .active_coin_flip_frame()
                    .unwrap()
                    .branch_context
                    .is_empty(),
                "current cast input authority is meaningful before old encoding"
            );
            let current_context = &runner
                .state()
                .active_coin_flip_frame()
                .unwrap()
                .branch_context;
            assert!(current_context
                .coin_branch_target_abilities
                .iter()
                .any(|(_, child)| child.cast_occurrence.is_some()));
            assert_eq!(
                serde_json::to_value(runner.state().active_coin_flip_frame().unwrap()).unwrap()
                    ["branch_context"],
                serde_json::to_value(current_context).unwrap(),
                "current writer retains the complete nondefault cast input"
            );
            for codec in [0, 3] {
                let mut current =
                    nested_roundtrip(GameRunner::from_state(runner.state().clone()), codec);
                assert_eq!(
                    &current
                        .state()
                        .active_coin_flip_frame()
                        .unwrap()
                        .branch_context,
                    current_context
                );
                current
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![1],
                    })
                    .unwrap();
                if matches!(old_kind, serde_json::Value::Object(ref map) if map.contains_key("FlipN"))
                {
                    assert!(
                        matches!(&current.state().waiting_for, WaitingFor::CoinFlipKeepChoice { results, .. } if results == &vec![true, false])
                    );
                    current
                        .act(GameAction::SelectCoinFlips {
                            keep_indices: vec![0],
                        })
                        .unwrap();
                }
                current.advance_until_stack_empty();
                assert_eq!(current.state().players[0].life, expected_life);
                assert_eq!(
                    current.state().objects[&victim].zone,
                    if expected_life > 20 {
                        Zone::Graveyard
                    } else {
                        Zone::Battlefield
                    }
                );
                assert_eq!(current.state().objects[&decoy].zone, Zone::Battlefield);
                assert!(matches!(
                    current.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(
                    current.state().stack.is_empty() && current.state().resolution_stack.is_empty()
                );
            }
            runner.state_mut().capture_rng_word_pos();
            let mut value = if persisted {
                serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap()
            } else {
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap()
            };
            fn old_frame(value: &mut serde_json::Value, kind: &serde_json::Value) -> usize {
                match value {
                    serde_json::Value::Object(fields) => {
                        let mut found = 0;
                        if let Some(custody) = fields.remove("coin_branch_target_abilities") {
                            // This is the authentic old writer's absent field,
                            // not recovery of a malformed current capture.
                            // Its bare program's only target/pin is still in
                            // the original owner/frame; none is local here.
                            fn no_local_authority(node: &ResolvedAbility) {
                                assert!(node.declares_chosen_group.is_none());
                                assert!(node.reads_chosen_group.is_none());
                                assert!(node.chosen_target_slots.slots().is_empty());
                                assert!(node.targets.is_empty() && node.target_pins.is_empty());
                                assert!(node.target_incarnations.is_empty());
                                assert!(node.context.chosen_group_targets.is_empty());
                                for (_, child) in &node.coin_branch_target_abilities {
                                    no_local_authority(child);
                                }
                                for child in
                                    [node.sub_ability.as_deref(), node.else_ability.as_deref()]
                                        .into_iter()
                                        .flatten()
                                {
                                    no_local_authority(child);
                                }
                            }
                            let custody: Vec<(
                                engine::types::ability::CoinFlipResult,
                                Box<ResolvedAbility>,
                            )> = serde_json::from_value(custody).unwrap();
                            for (_, child) in custody {
                                no_local_authority(&child);
                            }
                        }
                        // Exact pre-v8 Effect writer shapes, including the
                        // Thumb's own old multiple-flip replacement program.
                        if fields.get("type").and_then(serde_json::Value::as_str)
                            == Some("FlipCoins")
                            && fields.contains_key("flips")
                        {
                            let flips = fields.remove("flips").unwrap();
                            match flips["type"].as_str().expect("typed flip run") {
                                "Exactly" => {
                                    let count = flips["count"].clone();
                                    if count == serde_json::json!({"type":"Fixed","value":1}) {
                                        fields.insert("type".into(), serde_json::json!("FlipCoin"));
                                    } else {
                                        fields.insert("count".into(), count);
                                    }
                                }
                                "UntilLoss" => {
                                    fields.insert(
                                        "type".into(),
                                        serde_json::json!("FlipCoinUntilLose"),
                                    );
                                    fields.remove("lose_effect");
                                    fields.remove("flipper");
                                }
                                other => panic!("unexpected encoded run {other}"),
                            }
                        }
                        if fields.contains_key("resolution_state_version") {
                            fields.insert("resolution_state_version".into(), serde_json::json!(7));
                        }
                        if fields.contains_key("source_id")
                            && fields.contains_key("chain_root_targets")
                            && fields.contains_key("kind")
                        {
                            if let Some(context) = fields.get_mut("branch_context") {
                                found += old_frame(context, kind);
                                assert_eq!(*context, serde_json::json!({}), "old program owes no chosen player/source/result or local target authority");
                            }
                            fields.insert("kind".into(), kind.clone());
                            fields.remove("branch_context"); // old writer has no target-custody input copy
                            found += 1;
                        }
                        for child in fields.values_mut() {
                            found += old_frame(child, kind);
                        }
                        found
                    }
                    serde_json::Value::Array(items) => {
                        items.iter_mut().map(|child| old_frame(child, kind)).sum()
                    }
                    _ => 0,
                }
            }
            assert_eq!(old_frame(&mut value, &old_kind), 1);
            runner = if persisted {
                GameRunner::from_state(
                    serde_json::from_value::<PersistedGameState>(value)
                        .unwrap()
                        .into_game_state()
                        .unwrap(),
                )
            } else {
                GameRunner::from_state(
                    serde_json::from_value::<ResolutionStateWire>(value)
                        .unwrap()
                        .into_game_state(),
                )
            };
            assert!(
                runner
                    .state()
                    .active_coin_flip_frame()
                    .unwrap()
                    .branch_context
                    .is_empty(),
                "genuine default old context at both canonical readers"
            );
            assert_eq!(
                runner.state().active_coin_flip_frame().unwrap().targets,
                vec![TargetRef::Object(victim)]
            );
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![1],
                })
                .unwrap();
            if matches!(old_kind, serde_json::Value::Object(ref map) if map.contains_key("FlipN")) {
                let WaitingFor::CoinFlipKeepChoice { results, .. } = &runner.state().waiting_for
                else {
                    panic!("second encoded flip");
                };
                assert_eq!(results, &[true, false]);
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .unwrap();
            }
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().players[0].life, expected_life);
            assert_eq!(
                runner.state().objects[&victim].zone,
                if expected_life > 20 {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
            assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
}

#[test]
fn copied_mogg_activation_retargets_occurrence_capture_without_changing_original_groups() {
    // CR 707.10b-c: the copy keeps the source, while choosing a new legal
    // target replaces only that occurrence and leaves the original unchanged.
    use engine::types::resolution::ResolutionStateWire;
    for persisted in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(P0, "Mogg Assassin", 2, 1, MOGG_ASSASSIN)
            .id();
        let first = scenario.add_creature(P1, "Original group zero", 2, 3).id();
        let replacement = scenario.add_creature(P1, "Copied group zero", 2, 3).id();
        let second = scenario.add_creature(P0, "Shared group one", 2, 3).id();
        let copy_spell = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic activation-copy witness", true,
            "Copy target activated or triggered ability. You may choose new targets for the copy.").with_mana_cost(ManaCost::generic(1)).id();
        scenario.with_mana_pool(
            P0,
            vec![engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert_mogg_actor(&runner, P0, first);
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(first)),
            })
            .unwrap();
        assert_mogg_actor(&runner, P1, second);
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second)),
            })
            .unwrap();
        let original_entry = runner
            .state()
            .stack
            .back()
            .expect("actual activated ability on stack")
            .id;
        let original_capture = runner
            .state()
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .context
            .chosen_group_targets
            .clone();
        let paused = runner
            .cast(copy_spell)
            .target_object(original_entry)
            .resolve();
        let WaitingFor::CopyRetarget {
            copy_id,
            target_slots,
            ..
        } = paused.final_waiting_for()
        else {
            panic!("actual activation copy must offer new targets");
        };
        let copy_id = *copy_id;
        assert_eq!(target_slots.len(), 2);
        assert_eq!(target_slots[0].current, Some(TargetRef::Object(first)));
        assert_eq!(target_slots[1].current, Some(TargetRef::Object(second)));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        runner = if persisted {
            persisted_roundtrip(runner.state())
        } else {
            let value =
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap();
            GameRunner::from_state(
                serde_json::from_value::<ResolutionStateWire>(value)
                    .unwrap()
                    .into_game_state(),
            )
        };
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(replacement)),
            })
            .unwrap();
        runner.act(GameAction::KeepAllCopyTargets).unwrap();
        let original = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == original_entry)
            .unwrap()
            .ability()
            .unwrap();
        assert_eq!(original.context.chosen_group_targets, original_capture);
        let copy = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == copy_id)
            .unwrap()
            .ability()
            .unwrap();
        let copied = &copy.context.chosen_group_targets;
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[0].1[0].0, TargetRef::Object(replacement));
        assert_eq!(copied[1].1[0].0, TargetRef::Object(second));
        assert!(copied[0].1[0].1.is_some());
        pin_mogg(&mut runner, &[true, true]);
        let events = pass_mogg_priority(&mut runner);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CoinFlipped { won: true, .. }))
                .count(),
            2
        );
        assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&replacement].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn positive_paid_empty_group_survives_both_codecs_and_never_becomes_a_population_scan() {
    use engine::types::resolution::ResolutionStateWire;
    for persisted in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let untouched = scenario
            .add_creature(P0, "Unselected positive decoy", 2, 2)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Paid empty declaration witness",
                true,
                KICKED_CHOSEN_RETURN,
            )
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![engine::types::mana::ManaUnit::new(
                engine::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
        let mut runner = scenario.build();
        runner.cast(spell).accept_optional().commit();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert!(ability.context.additional_cost_paid);
        let capture = ability.context.chosen_group_targets.clone();
        assert!(!capture.is_empty());
        assert!(capture
            .iter()
            .all(|(_, occurrences)| occurrences.is_empty()));
        runner = if persisted {
            persisted_roundtrip(runner.state())
        } else {
            let value =
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap();
            GameRunner::from_state(
                serde_json::from_value::<ResolutionStateWire>(value)
                    .unwrap()
                    .into_game_state(),
            )
        };
        assert_eq!(
            runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .context
                .chosen_group_targets,
            capture
        );
        let events = pass_mogg_priority(&mut runner);
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::BounceAll,
                    ..
                }
            )),
            "completed empty reader positively executes"
        );
        assert_eq!(runner.state().objects[&untouched].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}
