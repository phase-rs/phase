//! CR 122.1 + CR 608.2d: mandatory counter adjustment for a specified kind
//! or one kind currently present on a single exact recipient.
//!
//! The chosen counter KIND is decided at resolution from the kinds actually
//! present on the target (a runtime fact), so the per-kind branch set must be
//! built here rather than at parse time. The resolver delegates to the existing
//! `choose_one_of` machinery: it constructs a flat `Effect::ChooseOneOf` of
//! concrete `PutCounter`/`RemoveCounter` branches (one per present kind × allowed
//! operation) and hands it off, reusing the whole `ChooseOneOfBranch` interactive
//! surface (`WaitingFor`, `GameAction::ChooseBranch`, AI candidates, multiplayer
//! routing, frontend modal) with zero new surface.
//!
//! This is the choose-ONE-kind sibling of the `repeat_for:
//! DistinctCounterKindsAmong` for-EACH-kind loop (Bribe Taker / Quarry Hauler).

use super::counters::{
    preview_counter_addition, remove_counter_optional_is_infeasible, CounterAdditionPreview,
};
use crate::game::quantity::{
    quantity_expr_uses_recipient, resolve_quantity_with_targets,
    resolve_quantity_with_targets_and_recipient,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, EffectError, EffectKind, PlayerFilter,
    QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::counter::{positive_counter_types, CounterMatch};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};

/// CR 608.2h: determine this instruction's amount using its original authority.
/// CR 107.1b: calculated negative results are zero. Only an explicitly authored
/// negative Fixed is the existing removal sentinel; preserve outer UpTo (CR 107.1c).
fn bind_count(
    state: &GameState,
    ability: &ResolvedAbility,
    count: &QuantityExpr,
    recipient: ObjectId,
) -> QuantityExpr {
    let (inner, up_to) = count.peel_up_to();
    let value = match inner {
        QuantityExpr::Fixed { value } if *value < 0 => *value,
        _ => {
            let value = if quantity_expr_uses_recipient(inner) {
                resolve_quantity_with_targets_and_recipient(state, inner, ability, recipient)
            } else {
                resolve_quantity_with_targets(state, inner, ability)
            };
            value.max(0)
        }
    };
    let bound = QuantityExpr::Fixed { value };
    if up_to {
        QuantityExpr::up_to(bound)
    } else {
        bound
    }
}

/// CR 400.7 + CR 608.2b: select the original occurrence before filtering it.
/// A missing/stale first object never selects a later object or a board scan.
pub(super) fn recipient(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Option<ObjectIncarnationRef> {
    if matches!(target, TargetFilter::SelfRef) {
        let object = state.objects.get(&ability.source_id)?;
        if !ability.self_ref_is_current(state)
            || ability
                .trigger_source
                .as_ref()
                .is_some_and(|source| object.zone != source.identity.expected_zone)
        {
            return None;
        }
        return Some(ObjectIncarnationRef::from_object(object));
    }
    // CR 608.2k + CR 400.7: an untargeted context reference names its own
    // referent, even if the chain inherited an unrelated announced target.
    // Event roles use the shared mutation reader to check the event's stamp;
    // other context references use their ordinary filter/slot authority.
    if target.is_context_ref() && !matches!(target, TargetFilter::ParentTarget) {
        let ids = match target {
            TargetFilter::TriggeringSource | TargetFilter::EventTarget => {
                crate::game::targeting::resolve_event_referent_objects(
                    state,
                    target,
                    ability.source_id,
                )
            }
            _ => crate::game::targeting::resolved_object_ids_for_filter(state, ability, target),
        };
        let [id] = ids.as_slice() else {
            return None;
        };
        return state.objects.get(id).map(ObjectIncarnationRef::from_object);
    }
    let (index, id) = ability
        .targets
        .iter()
        .enumerate()
        .find_map(|(index, target)| {
            if let TargetRef::Object(id) = target {
                Some((index, *id))
            } else {
                None
            }
        })?;
    if !ability.target_occurrence_is_current(index, state)
        || !ability.target_pin_is_current(id, state)
    {
        return None;
    }
    let object = state.objects.get(&id)?;
    Some(
        ability
            .target_pin_at(index)
            .unwrap_or_else(|| ObjectIncarnationRef::from_object(object)),
    )
}

/// CR 122.1 + CR 608.2d: build possible mandatory operations, then delegate
/// unchanged ordinary counter definitions to the existing public choice route.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, owner| resolve_owned(state, executable, events, owner),
    )
}

pub(crate) fn resolve_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
) -> Result<(), EffectError> {
    let Effect::ChooseCounterAdjustment {
        adjustment,
        counters,
        count,
        target,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("ChooseCounterAdjustment".into()));
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseCounterAdjustment,
        source_id: ability.source_id,
        subject: None,
    });
    let recipient = recipient(state, ability, target);
    let result_binding =
        super::begin_counter_removal_result(state, ability, recipient, counters.clone())?;
    let Some(pin) = recipient else {
        return Ok(());
    };
    let object = &state.objects[&pin.object_id];
    let mut kinds = match counters {
        CounterMatch::Any => positive_counter_types(&object.counters),
        CounterMatch::OfType(kind) => vec![kind.clone()],
    };
    kinds.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    let bound = bind_count(state, ability, count, pin.object_id);
    let (inner, up_to) = bound.peel_up_to();
    let QuantityExpr::Fixed { value } = inner else {
        unreachable!("bound counter amount")
    };
    let mut branches = Vec::new();
    // Clone context/attribution but leave the outer resolution chain its sole tail.
    let delegated = delegated_choice(ability, pin, result_binding, Vec::new());
    for kind in kinds {
        if adjustment.allows_remove() {
            let effect = Effect::RemoveCounter {
                counter_type: Some(kind.clone()),
                count: bound.clone(),
                target: TargetFilter::ParentTarget,
            };
            let mut candidate = delegated.clone();
            candidate.effect = effect.clone();
            let zero = *value == 0
                || (*value < 0 && object.counters.get(&kind).copied().unwrap_or(0) == 0);
            if up_to || zero || !remove_counter_optional_is_infeasible(state, &candidate) {
                let amount = if up_to {
                    "any number of".into()
                } else if *value < 0 {
                    "all".into()
                } else {
                    value.to_string()
                };
                branches.push(AbilityDefinition {
                    description: Some(format!(
                        "remove {amount} {} counters",
                        kind.display_phrase()
                    )),
                    ..AbilityDefinition::new(AbilityKind::Spell, effect)
                });
            }
        }
        if adjustment.allows_add() {
            let preview = preview_counter_addition(
                state,
                ability.controller,
                pin,
                kind.clone(),
                (*value).max(0) as u32,
            );
            let possible = match preview {
                Some(
                    CounterAdditionPreview::Applied { .. }
                    | CounterAdditionPreview::Transformed { .. }
                    | CounterAdditionPreview::ChoiceRequired { .. }
                    | CounterAdditionPreview::Unsupported,
                ) => true,
                Some(prohibition @ CounterAdditionPreview::Prevented) => {
                    !prohibition.is_prohibited()
                }
                None => false,
            };
            if possible {
                branches.push(AbilityDefinition {
                    description: Some(format!(
                        "put {} {} counters",
                        (*value).max(0),
                        kind.display_phrase()
                    )),
                    ..AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            counter_type: kind,
                            count: bound.clone(),
                            target: TargetFilter::ParentTarget,
                        },
                    )
                });
            }
        }
    }
    if branches.is_empty() {
        return Ok(());
    }
    let delegated = delegated_choice(
        ability,
        pin,
        delegated.context.counter_removal_result,
        branches,
    );
    super::choose_one_of::resolve_owned(state, &delegated, events, declarations)
}

pub(crate) fn delegated_choice(
    ability: &ResolvedAbility,
    pin: ObjectIncarnationRef,
    result_binding: Option<crate::types::game_state::CounterRemovalResultBinding>,
    branches: Vec<AbilityDefinition>,
) -> ResolvedAbility {
    let mut delegated = ability.clone();
    delegated.declares_return_result = None;
    delegated.context.counter_removal_result = result_binding;
    delegated.sub_ability = None;
    delegated.else_ability = None;
    delegated.replace_target_occurrences(vec![(TargetRef::Object(pin.object_id), Some(pin))]);
    delegated.effect = Effect::ChooseOneOf {
        kind: ChooseOneOfKind::Ordinary,
        chooser: PlayerFilter::Controller,
        branches,
    };
    delegated
}

/// CR 608.2h: authenticate the existing instruction-time amount latch without
/// the later board. The ticket identifies the producer and recipient, not amount.
pub(crate) fn project_latched_choice(
    original: &ResolvedAbility,
    current: &ResolvedAbility,
) -> Option<ResolvedAbility> {
    let Effect::ChooseCounterAdjustment {
        adjustment,
        counters,
        count,
        ..
    } = &original.effect
    else {
        return None;
    };
    let Effect::ChooseOneOf {
        kind: ChooseOneOfKind::Ordinary,
        chooser: PlayerFilter::Controller,
        branches,
    } = &current.effect
    else {
        return None;
    };
    let occurrences = current.target_occurrences();
    let [(TargetRef::Object(id), Some(pin))] = occurrences.as_slice() else {
        return None;
    };
    let pin = *pin;
    if *id != pin.object_id || branches.is_empty() {
        return None;
    }
    let (authored, up_to) = count.peel_up_to();
    let mut amount = None;
    let mut previous = None;
    for branch in branches {
        let (kind, operand, remove) = match branch.effect.as_ref() {
            Effect::RemoveCounter {
                counter_type: Some(kind),
                count,
                target: TargetFilter::ParentTarget,
            } if adjustment.allows_remove() => (kind, count, true),
            Effect::PutCounter {
                counter_type,
                count,
                target: TargetFilter::ParentTarget,
            } if adjustment.allows_add() => (counter_type, count, false),
            _ => return None,
        };
        if !counters.matches(kind) {
            return None;
        }
        let (inner, wrapped) = operand.peel_up_to();
        let QuantityExpr::Fixed { value } = inner else {
            return None;
        };
        if wrapped != up_to || amount.is_some_and(|amount| amount != *value) {
            return None;
        }
        match authored {
            QuantityExpr::Fixed { value: expected } if expected != value => return None,
            QuantityExpr::Fixed { .. } => {}
            _ if *value < 0 => return None,
            _ => {}
        }
        amount = Some(*value);
        let order = (kind.as_str().to_owned(), if remove { 0 } else { 1 });
        if previous.as_ref().is_some_and(|previous| previous >= &order) {
            return None;
        }
        previous = Some(order);
        let description = if remove {
            let amount = if up_to {
                "any number of".into()
            } else if *value < 0 {
                "all".into()
            } else {
                value.to_string()
            };
            format!("remove {amount} {} counters", kind.display_phrase())
        } else {
            format!("put {} {} counters", (*value).max(0), kind.display_phrase())
        };
        let expected = AbilityDefinition {
            description: Some(description),
            ..AbilityDefinition::new(AbilityKind::Spell, branch.effect.as_ref().clone())
        };
        if branch != &expected {
            return None;
        }
    }
    let ticket = current.context.counter_removal_result.clone();
    match (&ticket, original.declares_return_result) {
        (Some(ticket), Some(result))
            if ticket.result == result
                && ticket.recipient == Some(pin)
                && ticket.counters == *counters
                && ticket.source.reference.object_id == original.source_id
                && ticket.process == original.return_result_process => {}
        (None, None) => {}
        _ => return None,
    }
    let projected = delegated_choice(original, pin, ticket, branches.clone());
    super::instruction_result_projection_eq(&projected, current).then_some(projected)
}

/// After selection the concrete operation is the latch. Its prompt no longer
/// exists; replacement/cost pauses retain this actual operation and its ticket.
pub(crate) fn project_latched_operation(
    original: &ResolvedAbility,
    current: &ResolvedAbility,
) -> Option<ResolvedAbility> {
    let (kind, count, remove) = match &current.effect {
        Effect::RemoveCounter {
            counter_type: Some(kind),
            count,
            target: TargetFilter::ParentTarget,
        } => (kind, count, true),
        Effect::PutCounter {
            counter_type,
            count,
            target: TargetFilter::ParentTarget,
        } => (counter_type, count, false),
        _ => return None,
    };
    let occurrences = current.target_occurrences();
    let (id, pin) = occurrences
        .first()
        .and_then(|(target, pin)| match (target, pin) {
            (TargetRef::Object(id), Some(pin)) => Some((*id, *pin)),
            _ => None,
        })?;
    if id != pin.object_id
        || occurrences
            .iter()
            .skip(1)
            .any(|(target, pin)| *target != TargetRef::Player(original.controller) || pin.is_some())
    {
        return None;
    }
    let (inner, up_to) = count.peel_up_to();
    let QuantityExpr::Fixed { value } = inner else {
        return None;
    };
    let description = if remove {
        let amount = if up_to {
            "any number of".into()
        } else if *value < 0 {
            "all".into()
        } else {
            value.to_string()
        };
        format!("remove {amount} {} counters", kind.display_phrase())
    } else {
        format!("put {} {} counters", (*value).max(0), kind.display_phrase())
    };
    let branch = AbilityDefinition {
        description: Some(description),
        ..AbilityDefinition::new(AbilityKind::Spell, current.effect.clone())
    };
    let mut parent = original.clone();
    // Additions do not consume or forward a removal receipt.
    if !remove {
        parent.declares_return_result = None;
    }
    let delegated = delegated_choice(
        &parent,
        pin,
        current.context.counter_removal_result.clone(),
        vec![branch.clone()],
    );
    project_latched_choice(&parent, &delegated)?;
    let selected =
        super::choose_one_of::project_selected_branch(&delegated, &branch, original.controller);
    super::instruction_result_projection_eq(&selected, current).then_some(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{CounterAdjustment, QuantityExpr};

    use crate::game::engine::apply;
    use crate::game::zones::create_object;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn public_dynamic_counter_choice_keeps_three_when_later_input_is_seven() {
        use crate::game::scenario::{GameRunner, GameScenario, P0};
        use crate::types::ability::{
            PlayerScope, QuantityRef, ReplacementDefinition, ReplacementMode, ReturnResultId,
        };
        use crate::types::game_state::TrustedGameStateEnvelope;
        use crate::types::mana::{ManaCost, ManaType, ManaUnit};
        use crate::types::phase::Phase;
        use crate::types::replacements::ReplacementEvent;
        for (index, replacement) in [(0, false), (1, false), (0, true)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_cards_in_hand(P0, &["First input", "Second input", "Third input"]);
            let recipient = scenario.add_creature(P0, "Counter recipient", 2, 2).id();
            scenario.with_counter(recipient, CounterType::Generic("charge".into()), 5);
            if replacement {
                scenario
                    .add_artifact_from_oracle(P0, "Supplemental counter replacement", "")
                    .with_replacement_definition(
                        ReplacementDefinition::new(ReplacementEvent::RemoveCounter)
                            .mode(ReplacementMode::Optional { decline: None }),
                    );
            }
            let mut definition = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::ChooseCounterAdjustment {
                    adjustment: CounterAdjustment::AddOrRemove,
                    counters: CounterMatch::OfType(CounterType::Generic("charge".into())),
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    },
                    target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                },
            )
            .cost(crate::types::ability::AbilityCost::Mana {
                cost: ManaCost::generic(1),
            })
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    player: TargetFilter::Controller,
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ));
            definition.declares_return_result = Some(ReturnResultId(7));
            let source = scenario
                .add_artifact_from_oracle(P0, "Supplemental paid adjustment", "")
                .with_ability_definition(definition)
                .id();
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Black, source, false, Vec::new())],
            );
            let mut runner = scenario.build();
            runner
                .activate(source, 0)
                .target_object(recipient)
                .resolve();
            let WaitingFor::ChooseOneOfBranch {
                branches, context, ..
            } = &runner.state().waiting_for
            else {
                panic!("real generated counter prompt");
            };
            assert_eq!(branches.len(), 2);
            assert!(context.counter_removal_result.is_some());
            match (branches[0].effect.as_ref(), branches[1].effect.as_ref()) {
                (
                    Effect::RemoveCounter {
                        counter_type: Some(remove_kind),
                        count: QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::ParentTarget,
                    },
                    Effect::PutCounter {
                        counter_type: add_kind,
                        count: QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::ParentTarget,
                    },
                ) => {
                    assert_eq!(remove_kind, &CounterType::Generic("charge".into()));
                    assert_eq!(add_kind, &CounterType::Generic("charge".into()));
                }
                effects => {
                    panic!("expected removal then addition of three charge counters: {effects:?}")
                }
            }
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            for n in 0..4 {
                create_object(
                    runner.state_mut(),
                    CardId(90_000 + n),
                    P0,
                    format!("Later input {n}"),
                    Zone::Hand,
                );
            }
            assert_eq!(runner.state().players[0].hand.len(), 7);
            let paused = runner.state().clone();
            // A candidate is not its own amount authority: disagreeing siblings
            // and fixed-parent/wrapper/recipient/ticket corruptions refuse first.
            for corruption in 0..5 {
                let mut hostile = paused.clone();
                let WaitingFor::ChooseOneOfBranch {
                    branches,
                    context,
                    source_id,
                    ..
                } = &mut hostile.waiting_for
                else {
                    unreachable!();
                };
                match corruption {
                    0 => {
                        let Effect::PutCounter { count, .. } = branches[1].effect.as_mut() else {
                            unreachable!();
                        };
                        *count = QuantityExpr::Fixed { value: 7 };
                    }
                    1 => {
                        branches[1].sub_ability = Some(Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::GainLife {
                                player: TargetFilter::Controller,
                                amount: QuantityExpr::Fixed { value: 999 },
                            },
                        )));
                    }
                    2 => {
                        context.counter_removal_result.as_mut().unwrap().recipient = None;
                    }
                    3 => {
                        context.counter_removal_result.as_mut().unwrap().result =
                            ReturnResultId(99);
                    }
                    _ => {
                        *source_id = recipient;
                    }
                }
                assert!(serde_json::from_value::<GameState>(
                    serde_json::to_value(&hostile).unwrap()
                )
                .is_err());
                let before = hostile.clone();
                assert!(apply(&mut hostile, P0, GameAction::ChooseBranch { index: 1 }).is_err());
                assert_eq!(hostile, before);
            }
            for ingress in 0..3 {
                let restored = match ingress {
                    0 => serde_json::from_value(serde_json::to_value(&paused).unwrap()).unwrap(),
                    1 => serde_json::from_value::<TrustedGameStateEnvelope>(
                        serde_json::to_value(TrustedGameStateEnvelope::capture(paused.clone()))
                            .unwrap(),
                    )
                    .unwrap()
                    .into_game_state(),
                    _ => serde_json::from_value::<crate::types::resolution::ResolutionStateWire>(
                        serde_json::to_value(
                            crate::types::resolution::ResolutionStateWire::from_game_state(
                                paused.clone(),
                            ),
                        )
                        .unwrap(),
                    )
                    .unwrap()
                    .into_game_state(),
                };
                let mut resumed = GameRunner::from_state(restored);
                resumed.act(GameAction::ChooseBranch { index }).unwrap();
                if replacement {
                    assert!(matches!(
                        resumed.state().waiting_for,
                        WaitingFor::ReplacementChoice { .. }
                    ));
                    *resumed.state_mut() =
                        serde_json::from_value(serde_json::to_value(resumed.state()).unwrap())
                            .unwrap();
                    resumed
                        .act(GameAction::ChooseReplacement { index: 1 })
                        .unwrap();
                }
                resumed.advance_until_stack_empty();
                assert_eq!(
                    resumed.state().objects[&recipient]
                        .counters
                        .get(&CounterType::Generic("charge".into())),
                    Some(&if index == 0 { 2 } else { 8 })
                );
                assert_eq!(resumed.state().players[0].life, 21);
                assert!(matches!(
                    resumed.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert!(resumed.state().stack.is_empty());
                assert!(resumed.state().resolution_stack.is_empty());
                assert!(resumed.state().return_result_frames.is_empty());
            }
        }
    }

    // Supplemental typed compositions: these fixtures make no printed-card claim.
    fn complete_counter_definition() -> AbilityDefinition {
        use crate::types::ability::{
            AbilityCost, PlayerScope, QuantityRef, ReturnResultId, SubAbilityLink,
        };
        let mut tail = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                player: TargetFilter::Controller,
                amount: QuantityExpr::Fixed { value: 1 },
            },
        );
        tail.sub_link = SubAbilityLink::SequentialSibling;
        let mut definition = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChooseCounterAdjustment {
                adjustment: CounterAdjustment::AddOrRemove,
                counters: CounterMatch::OfType(CounterType::Generic("charge".into())),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            },
        )
        .cost(AbilityCost::Mana {
            cost: crate::types::mana::ManaCost::generic(1),
        })
        .sub_ability(tail);
        definition.declares_return_result = Some(ReturnResultId(7));
        definition
    }

    fn paid_counter_fixture(
        definition: AbilityDefinition,
    ) -> (
        crate::game::scenario::GameRunner,
        crate::types::identifiers::ObjectId,
        crate::types::identifiers::ObjectId,
    ) {
        paid_counter_fixture_with_replacement(definition, false)
    }

    fn paid_counter_fixture_with_replacement(
        definition: AbilityDefinition,
        replacement: bool,
    ) -> (
        crate::game::scenario::GameRunner,
        crate::types::identifiers::ObjectId,
        crate::types::identifiers::ObjectId,
    ) {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::mana::{ManaType, ManaUnit};
        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        for player in [P0, P1] {
            scenario.with_cards_in_hand(player, &["First input", "Second input", "Third input"]);
        }
        let recipient = scenario.add_creature(P0, "Counter recipient", 2, 2).id();
        scenario.with_counter(recipient, CounterType::Generic("charge".into()), 5);
        if replacement {
            scenario
                .add_artifact_from_oracle(P0, "Supplemental removal replacement", "")
                .with_replacement_definition(
                    crate::types::ability::ReplacementDefinition::new(
                        crate::types::replacements::ReplacementEvent::RemoveCounter,
                    )
                    .mode(crate::types::ability::ReplacementMode::Optional { decline: None }),
                );
        }
        let source = scenario
            .add_artifact_from_oracle(P0, "Supplemental complete counter input", "")
            .with_ability_definition(definition)
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, source, false, vec![])],
        );
        let mut runner = scenario.build();
        runner
            .activate(source, 0)
            .target_object(recipient)
            .stop_at_optional()
            .resolve();
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        (runner, source, recipient)
    }

    #[track_caller]
    fn restore_counter_input(state: &GameState, ingress: usize) -> GameState {
        use crate::types::game_state::TrustedGameStateEnvelope;
        match ingress {
            0 => serde_json::from_value(serde_json::to_value(state).unwrap()),
            1 => serde_json::from_value::<TrustedGameStateEnvelope>(
                serde_json::to_value(TrustedGameStateEnvelope::capture(state.clone())).unwrap(),
            )
            .map(TrustedGameStateEnvelope::into_game_state),
            _ => serde_json::from_value::<crate::types::resolution::ResolutionStateWire>(
                serde_json::to_value(
                    crate::types::resolution::ResolutionStateWire::from_game_state(state.clone()),
                )
                .unwrap(),
            )
            .map(crate::types::resolution::ResolutionStateWire::into_game_state),
        }
        .unwrap_or_else(|error| panic!("counter restore ingress {ingress}: {error}"))
    }

    fn counter_prompt_root(state: &GameState) -> &ResolvedAbility {
        let WaitingFor::ChooseOneOfBranch { context, .. } = &state.waiting_for else {
            panic!(
                "required real generated counter prompt: {:?}",
                state.waiting_for
            );
        };
        let ticket = context
            .counter_removal_result
            .as_ref()
            .expect("real result ticket");
        state.return_result_frames[&ticket.occurrence]
            .admitted_program
            .as_deref()
            .unwrap()
    }

    fn counter_prompt_parent(state: &GameState) -> ResolvedAbility {
        use crate::game::effects::choose_one_of::{selection_program_owner, BranchSelection};
        let WaitingFor::ChooseOneOfBranch {
            return_result_occurrence,
            expected_return_result_process,
            player,
            controller,
            source_id,
            branches,
            parent_occurrences,
            context,
            continuation,
            replacement_applied,
            remaining_players,
            trigger_context,
            ..
        } = &state.waiting_for
        else {
            panic!("required generated counter prompt")
        };
        selection_program_owner(
            state,
            &BranchSelection {
                return_result_occurrence: *return_result_occurrence,
                expected_return_result_process: *expected_return_result_process,
                player: *player,
                controller: *controller,
                source_id: *source_id,
                branches: branches.clone(),
                parent_occurrences: parent_occurrences.clone(),
                context: context.clone(),
                continuation: continuation.clone(),
                replacement_applied: replacement_applied.clone(),
                remaining_players: remaining_players.clone(),
                trigger_context: trigger_context.clone(),
                index: 0,
            },
        )
        .unwrap()
        .unwrap()
        .2
    }

    fn assert_complete_counter_prompt(
        state: &GameState,
        actor: PlayerId,
        source: crate::types::identifiers::ObjectId,
        recipient: crate::types::identifiers::ObjectId,
    ) {
        let WaitingFor::ChooseOneOfBranch {
            player,
            controller,
            source_id,
            branches,
            context,
            parent_occurrences,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "required real generated counter prompt: {:?}",
                state.waiting_for
            );
        };
        assert_eq!((*player, *controller, *source_id), (actor, actor, source));
        assert_eq!(branches.len(), 2);
        assert!(matches!(
            branches[0].effect.as_ref(),
            Effect::RemoveCounter {
                count: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
        assert!(matches!(
            branches[1].effect.as_ref(),
            Effect::PutCounter {
                count: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
        let ticket = context.counter_removal_result.as_ref().unwrap();
        assert_eq!(ticket.result, crate::types::ability::ReturnResultId(7));
        assert_eq!(ticket.source.reference.object_id, source);
        assert!(
            matches!(parent_occurrences.occurrences().as_slice(), [(TargetRef::Object(id), Some(pin))]
            if *id == recipient && pin.object_id == recipient && Some(*pin) == ticket.recipient)
        );
        assert_eq!(counter_prompt_root(state).ability_index, Some(0));
        let parent = counter_prompt_parent(state);
        assert_eq!(parent.ability_index, Some(0));
        assert!(
            !parent.optional
                && parent.optional_for.is_none()
                && parent.unless_pay.is_none()
                && parent.player_scope.is_none()
        );
        super::super::choose_one_of::validate_waiting_result_owner(state, &state.waiting_for)
            .unwrap();
    }

    fn assert_counter_complete(
        state: &GameState,
        recipient: crate::types::identifiers::ObjectId,
        counters: u32,
        p1_life: i32,
    ) {
        assert_eq!(
            state.objects[&recipient]
                .counters
                .get(&CounterType::Generic("charge".into())),
            Some(&counters)
        );
        assert_eq!(state.players[0].life, 21);
        assert_eq!(state.players[1].life, p1_life);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        assert!(state.resolving_stack_entry.is_none());
        assert!(state.resolution_stack.is_empty());
        assert!(state.return_result_frames.is_empty());
        assert!(state.pending_replacement.is_none());
    }

    fn settle_latched_three(
        mut runner: crate::game::scenario::GameRunner,
        actor: PlayerId,
        hand_player: PlayerId,
        source: crate::types::identifiers::ObjectId,
        recipient: crate::types::identifiers::ObjectId,
        p1_life: i32,
    ) {
        assert_complete_counter_prompt(runner.state(), actor, source, recipient);
        assert_eq!(runner.state().players[hand_player.0 as usize].hand.len(), 3);
        for n in 0..4 {
            create_object(
                runner.state_mut(),
                CardId(95_000 + n),
                hand_player,
                format!("Later input {n}"),
                Zone::Hand,
            );
        }
        assert_eq!(runner.state().players[hand_player.0 as usize].hand.len(), 7);
        for ingress in 0..3 {
            let mut resumed = crate::game::scenario::GameRunner::from_state(restore_counter_input(
                runner.state(),
                ingress,
            ));
            apply(
                resumed.state_mut(),
                actor,
                GameAction::ChooseBranch { index: 1 },
            )
            .unwrap();
            resumed.advance_until_stack_empty();
            // CR 608.2h + CR 122.1: the original three, not the later seven.
            assert_counter_complete(resumed.state(), recipient, 8, p1_life);
        }
    }

    #[track_caller]
    fn assert_counter_refuses_intact(
        mut hostile: GameState,
        actor: PlayerId,
        action: GameAction,
        case: &str,
    ) {
        assert!(
            serde_json::from_value::<GameState>(serde_json::to_value(&hostile).unwrap()).is_err(),
            "{case}: raw decode must refuse {actor:?} / {action:?}"
        );
        let before = hostile.clone();
        assert!(
            apply(&mut hostile, actor, action.clone()).is_err(),
            "{case}: public action must refuse {actor:?} / {action:?}"
        );
        assert_eq!(
            hostile, before,
            "{case}: refused action leaves the whole state and RNG intact"
        );
    }

    fn restore_counter_without_cached_snapshot(state: &GameState, ingress: usize) -> GameState {
        let WaitingFor::ChooseOneOfBranch { context, .. } = &state.waiting_for else {
            panic!("snapshot recovery requires the genuine generated prompt")
        };
        let ticket = context.counter_removal_result.as_ref().unwrap();
        let expected = state.return_result_frames[&ticket.occurrence].clone();
        assert!(expected.admitted_program.is_some());
        assert!(expected.declarations.is_some());
        assert_eq!(expected.process, ticket.process);
        assert!(state
            .resolving_stack_entry
            .as_ref()
            .and_then(|entry| entry.ability())
            .is_some());
        let mut torn = state.clone();
        torn.return_result_frames
            .get_mut(&ticket.occurrence)
            .unwrap()
            .admitted_program = None;
        let saved = serde_json::to_value(&torn).unwrap();
        let recovered = restore_counter_input(&torn, ingress);
        assert_eq!(
            recovered.return_result_frames[&ticket.occurrence], expected,
            "ingress {ingress}: recover the complete root, declaration map, process and results"
        );
        assert_eq!(recovered.resolving_stack_entry, state.resolving_stack_entry);
        assert_eq!(recovered.waiting_for, state.waiting_for);
        assert_eq!(recovered.rng.get_word_pos(), state.rng.get_word_pos());
        assert_eq!(
            serde_json::to_value(&torn).unwrap(),
            saved,
            "ingress {ingress}: recovery must not mutate the saved input"
        );
        recovered
    }

    #[test]
    fn public_optional_counter_choice_preserves_complete_parent_and_latched_three() {
        use crate::game::scenario::{GameRunner, P0, P1};
        let mut definition = complete_counter_definition();
        definition.optional = true;
        let (mut runner, source, recipient) = paid_counter_fixture(definition);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        let gate = restore_counter_input(runner.state(), 0);
        let before = runner.state().clone();
        assert!(apply(
            runner.state_mut(),
            P1,
            GameAction::DecideOptionalEffect { accept: true }
        )
        .is_err());
        assert_eq!(runner.state(), &before);
        *runner.state_mut() = gate.clone();
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        assert!(counter_prompt_root(runner.state()).optional);
        assert_complete_counter_prompt(runner.state(), P0, source, recipient);
        let mut hostile = runner.state().clone();
        let WaitingFor::ChooseOneOfBranch { context, .. } = &mut hostile.waiting_for else {
            unreachable!()
        };
        assert!(context.optional_effect_performed);
        context.optional_effect_performed = false;
        assert_counter_refuses_intact(
            hostile,
            P0,
            GameAction::ChooseBranch { index: 1 },
            "plain optional performed",
        );
        settle_latched_three(runner, P0, P0, source, recipient, 20);
        let mut declined = GameRunner::from_state(gate);
        declined
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        declined.advance_until_stack_empty();
        assert_counter_complete(declined.state(), recipient, 5, 20);
    }

    #[test]
    fn public_opponent_accepted_counter_choice_keeps_accepting_scope() {
        use crate::game::scenario::{GameRunner, P0, P1};
        let mut definition = complete_counter_definition();
        definition.optional = true;
        definition.optional_for = Some(crate::types::ability::OpponentMayScope::AnyOpponent);
        let (mut runner, source, recipient) = paid_counter_fixture(definition);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OpponentMayChoice { player: P1, .. }
        ));
        let gate = restore_counter_input(runner.state(), 0);
        let before = runner.state().clone();
        assert!(apply(
            runner.state_mut(),
            P0,
            GameAction::DecideOptionalEffect { accept: true }
        )
        .is_err());
        assert_eq!(runner.state(), &before);
        *runner.state_mut() = gate.clone();
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        assert_eq!(
            counter_prompt_root(runner.state()).optional_for,
            Some(crate::types::ability::OpponentMayScope::AnyOpponent)
        );
        assert_complete_counter_prompt(runner.state(), P0, source, recipient);
        let WaitingFor::ChooseOneOfBranch { context, .. } = &runner.state().waiting_for else {
            unreachable!()
        };
        assert_eq!(context.accepting_player, Some(P1));
        let parent = counter_prompt_parent(runner.state());
        assert_eq!((parent.controller, parent.scoped_player), (P0, Some(P1)));
        let mut hostile = runner.state().clone();
        let WaitingFor::ChooseOneOfBranch { context, .. } = &mut hostile.waiting_for else {
            unreachable!()
        };
        context.accepting_player = Some(P0);
        assert_counter_refuses_intact(
            hostile,
            P0,
            GameAction::ChooseBranch { index: 1 },
            "AnyOpponent accepting controller",
        );
        settle_latched_three(runner, P0, P0, source, recipient, 20);
        let mut declined = GameRunner::from_state(gate);
        declined
            .act(GameAction::DecideOptionalEffect { accept: false })
            .unwrap();
        declined.advance_until_stack_empty();
        assert_counter_complete(declined.state(), recipient, 5, 20);
    }

    #[test]
    fn public_any_player_counter_choice_allows_controller_acceptance() {
        use crate::game::scenario::P0;
        let mut definition = complete_counter_definition();
        definition.optional = true;
        definition.optional_for = Some(crate::types::ability::OpponentMayScope::AnyPlayer);
        let (mut runner, source, recipient) = paid_counter_fixture(definition);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OpponentMayChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let WaitingFor::ChooseOneOfBranch { context, .. } = &runner.state().waiting_for else {
            unreachable!()
        };
        assert_eq!(context.accepting_player, Some(P0));
        settle_latched_three(runner, P0, P0, source, recipient, 20);
    }

    #[test]
    fn public_nested_counter_gate_does_not_bypass_owner() {
        use crate::game::scenario::{P0, P1};
        use crate::types::ability::OpponentMayScope;
        for scoped in [false, true] {
            let mut child = complete_counter_definition();
            let cost = child.cost.take();
            child.kind = AbilityKind::Spell;
            let mut definition =
                AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).sub_ability(child);
            definition.cost = cost;
            let actor = if scoped {
                definition.player_scope = Some(PlayerFilter::Opponent);
                definition.sub_ability.as_mut().unwrap().optional = true;
                definition.sub_ability.as_mut().unwrap().optional_for =
                    Some(OpponentMayScope::AnyOpponent);
                P1
            } else {
                definition.optional = true;
                let mut unrelated = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
                unrelated.optional_for = Some(OpponentMayScope::AnyPlayer);
                definition.else_ability = Some(Box::new(unrelated));
                P0
            };
            let (mut runner, source, recipient) = paid_counter_fixture(definition);
            if scoped {
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::OpponentMayChoice { player: P0, .. }
                ));
            } else {
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::OptionalEffectChoice { player: P0, .. }
                ));
            }
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            assert_complete_counter_prompt(runner.state(), actor, source, recipient);
            let mut hostile = runner.state().clone();
            let WaitingFor::ChooseOneOfBranch { context, .. } = &mut hostile.waiting_for else {
                unreachable!()
            };
            if scoped {
                context.accepting_player = Some(P1);
            } else {
                context.optional_effect_performed = false;
            }
            assert_counter_refuses_intact(
                hostile,
                actor,
                GameAction::ChooseBranch { index: 1 },
                &format!("nested scoped={scoped}"),
            );
            settle_latched_three(runner, actor, P0, source, recipient, 20);
        }
    }

    #[test]
    fn public_unless_counter_choice_preserves_unpaid_and_paid_successor_inputs() {
        use crate::game::scenario::{GameRunner, P0, P1};
        use crate::types::ability::{AbilityCost, SubAbilityLink, UnlessPayModifier};
        for sibling in [false, true] {
            let mut definition = complete_counter_definition();
            let modifier = UnlessPayModifier {
                payer: TargetFilter::SpecificPlayer { id: P1 },
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            };
            if sibling {
                let cost = definition.cost.take();
                definition.kind = AbilityKind::Spell;
                definition.sub_link = SubAbilityLink::SequentialSibling;
                definition = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp)
                    .sub_ability(definition);
                definition.cost = cost;
            }
            definition.unless_pay = Some(modifier);
            let (mut runner, source, recipient) = paid_counter_fixture(definition);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::UnlessPayment { player: P1, .. }
            ));
            let WaitingFor::UnlessPayment {
                return_result_occurrence: Some(occurrence),
                pending_effect,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("sibling={sibling}: actual owned payment prompt")
            };
            let expected_frame = runner.state().return_result_frames[occurrence].clone();
            assert!(expected_frame
                .admitted_program
                .as_ref()
                .unwrap()
                .unless_pay
                .is_some());
            assert_eq!(expected_frame.process, pending_effect.return_result_process);
            assert!(runner.state().resolving_stack_entry.is_some());
            for ingress in 0..3 {
                let restored = restore_counter_input(runner.state(), ingress);
                assert_eq!(
                    restored.return_result_frames[occurrence], expected_frame,
                    "sibling={sibling}, payment ingress={ingress}: retain the full payment owner"
                );
                assert_eq!(restored.waiting_for, runner.state().waiting_for);
            }
            let gate = restore_counter_input(runner.state(), 0);
            let before = runner.state().clone();
            assert!(apply(
                runner.state_mut(),
                P0,
                GameAction::PayUnlessCost { pay: !sibling }
            )
            .is_err());
            assert_eq!(runner.state(), &before);
            *runner.state_mut() = gate.clone();
            runner
                .act(GameAction::PayUnlessCost { pay: sibling })
                .unwrap();
            assert!(counter_prompt_root(runner.state()).unless_pay.is_some());
            assert_complete_counter_prompt(runner.state(), P0, source, recipient);
            for ingress in 0..3 {
                let recovered = restore_counter_without_cached_snapshot(runner.state(), ingress);
                settle_latched_three(
                    GameRunner::from_state(recovered),
                    P0,
                    P0,
                    source,
                    recipient,
                    if sibling { 19 } else { 20 },
                );
            }
            let mut hostile = runner.state().clone();
            let WaitingFor::ChooseOneOfBranch { context, .. } = &mut hostile.waiting_for else {
                unreachable!()
            };
            context.optional_effect_performed = !context.optional_effect_performed;
            assert_counter_refuses_intact(
                hostile,
                P0,
                GameAction::ChooseBranch { index: 1 },
                &format!("unless sibling={sibling}: performed"),
            );
            settle_latched_three(
                runner,
                P0,
                P0,
                source,
                recipient,
                if sibling { 19 } else { 20 },
            );
            let mut opposite = GameRunner::from_state(gate);
            opposite
                .act(GameAction::PayUnlessCost { pay: !sibling })
                .unwrap();
            if sibling {
                settle_latched_three(opposite, P0, P0, source, recipient, 20);
            } else {
                opposite.advance_until_stack_empty();
                assert_counter_complete(opposite.state(), recipient, 5, 19);
            }
        }
    }

    #[test]
    fn public_unless_counter_no_payer_and_unpayable_fallthrough_preserve_parent() {
        use crate::game::scenario::{P0, P1};
        use crate::types::ability::{AbilityCost, UnlessPayModifier};
        for no_payer in [false, true] {
            let mut definition = complete_counter_definition();
            definition.unless_pay = Some(UnlessPayModifier {
                payer: if no_payer {
                    TargetFilter::TriggeringPlayer
                } else {
                    TargetFilter::SpecificPlayer { id: P1 }
                },
                cost: if no_payer {
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    }
                } else {
                    AbilityCost::Mana {
                        cost: crate::types::mana::ManaCost::NoCost,
                    }
                },
            });
            let (runner, source, recipient) = paid_counter_fixture(definition);
            let modifier = counter_prompt_root(runner.state())
                .unless_pay
                .as_ref()
                .unwrap();
            if no_payer {
                assert_eq!(modifier.payer, TargetFilter::TriggeringPlayer);
                assert!(runner.state().current_trigger_event.is_none());
            } else {
                assert!(matches!(
                    modifier.cost,
                    AbilityCost::Mana {
                        cost: crate::types::mana::ManaCost::NoCost
                    }
                ));
            }
            settle_latched_three(runner, P0, P0, source, recipient, 20);
        }
    }

    #[test]
    fn public_scoped_counter_choice_preserves_bound_controller_and_outside_tail() {
        use crate::game::scenario::{P0, P1};
        use crate::types::ability::{PlayerScope, QuantityRef};
        let mut definition = complete_counter_definition();
        definition.player_scope = Some(PlayerFilter::Opponent);
        let Effect::ChooseCounterAdjustment { count, .. } = definition.effect.as_mut() else {
            unreachable!()
        };
        *count = QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::ScopedPlayer,
            },
        };
        let (mut runner, source, recipient) = paid_counter_fixture(definition);
        assert_eq!(
            counter_prompt_root(runner.state()).player_scope,
            Some(PlayerFilter::Opponent)
        );
        assert_complete_counter_prompt(runner.state(), P1, source, recipient);
        let parent = counter_prompt_parent(runner.state());
        assert_eq!(
            (parent.original_controller, parent.scoped_player),
            (Some(P0), Some(P1))
        );
        let before = runner.state().clone();
        assert!(apply(
            runner.state_mut(),
            P0,
            GameAction::ChooseBranch { index: 1 }
        )
        .is_err());
        assert_eq!(runner.state(), &before);
        for corruption in 0..4 {
            let mut hostile = runner.state().clone();
            let WaitingFor::ChooseOneOfBranch {
                controller,
                source_id,
                context,
                ..
            } = &mut hostile.waiting_for
            else {
                unreachable!()
            };
            match corruption {
                0 => context.optional_effect_performed = true,
                1 => *controller = P0,
                2 => *source_id = recipient,
                _ => context.counter_removal_result.as_mut().unwrap().recipient = None,
            }
            assert_counter_refuses_intact(
                hostile,
                P1,
                GameAction::ChooseBranch { index: 1 },
                &format!("scoped corruption={corruption}"),
            );
        }
        settle_latched_three(runner, P1, P1, source, recipient, 20);
    }

    #[test]
    fn public_readerless_counter_producer_authenticates_wait_queue_and_proposal() {
        use crate::game::scenario::{GameRunner, P0};
        use crate::types::ability::ReturnResultId;
        let (mut runner, source, recipient) =
            paid_counter_fixture_with_replacement(complete_counter_definition(), true);
        assert_complete_counter_prompt(runner.state(), P0, source, recipient);
        let WaitingFor::ChooseOneOfBranch { context, .. } = &runner.state().waiting_for else {
            unreachable!()
        };
        let ticket = context.counter_removal_result.clone().unwrap();
        let producer = super::super::readerless_generated_counter_producer(runner.state(), &ticket)
            .unwrap()
            .unwrap();
        assert_eq!(producer.declares_return_result, Some(ReturnResultId(7)));
        assert!(!crate::types::game_state::reads_return_result_id(
            &producer,
            ReturnResultId(7)
        ));
        assert_eq!(runner.state().players[0].hand.len(), 3);
        for n in 0..4 {
            create_object(
                runner.state_mut(),
                CardId(96_000 + n),
                P0,
                format!("Later readerless input {n}"),
                Zone::Hand,
            );
        }
        // CR 608.2h: restoring the original latch cannot consult this later hand.
        assert_eq!(runner.state().players[0].hand.len(), 7);
        for ingress in 0..3 {
            restore_counter_input(runner.state(), ingress);
        }
        let recovered_prompts: Vec<_> = (0..3)
            .map(|ingress| restore_counter_without_cached_snapshot(runner.state(), ingress))
            .collect();
        for corruption in 1..6 {
            let mut hostile = runner.state().clone();
            let frame = hostile
                .return_result_frames
                .get_mut(&ticket.occurrence)
                .unwrap();
            match corruption {
                1 => frame.declarations.as_mut().unwrap().clear(),
                2 => frame.admitted_program.as_mut().unwrap().source_id = recipient,
                3 => {
                    let Effect::ChooseCounterAdjustment { counters, .. } =
                        &mut frame.admitted_program.as_mut().unwrap().effect
                    else {
                        unreachable!()
                    };
                    *counters = CounterMatch::OfType(CounterType::Time);
                }
                4 => {
                    frame
                        .admitted_program
                        .as_mut()
                        .unwrap()
                        .declares_return_result = Some(ReturnResultId(99))
                }
                _ => frame
                    .admitted_program
                    .as_mut()
                    .unwrap()
                    .bind_return_result_process(None),
            }
            assert_counter_refuses_intact(
                hostile,
                P0,
                GameAction::ChooseBranch { index: 0 },
                &format!("readerless corruption={corruption}"),
            );
        }
        // Erase actual invocation custody as well as its cached snapshot. The
        // ticket, declaration/result records and latched branches remain intact.
        let mut lost = runner.state().clone();
        lost.return_result_frames
            .get_mut(&ticket.occurrence)
            .unwrap()
            .admitted_program = None;
        assert!(lost.resolving_stack_entry.take().is_some());
        let continuation = lost.take_active_ability_continuation().unwrap().unwrap();
        assert_eq!(
            continuation.return_result_occurrence,
            Some(ticket.occurrence)
        );
        assert_eq!(continuation.chain.return_result_process, ticket.process);
        assert_eq!(continuation.chain.source_id, source);
        let WaitingFor::ChooseOneOfBranch { continuation, .. } = &mut lost.waiting_for else {
            unreachable!()
        };
        *continuation = None;
        assert!(lost.stack.is_empty() && lost.resolution_stack.is_empty());
        assert!(lost.pending_cast.is_none() && lost.pending_replacement.is_none());
        assert!(lost.pending_triggered_mana_resume.is_none());
        assert!(lost.pending_die_roll_instruction.is_none());
        assert!(lost.pending_scoped_library_search.is_none());
        assert!(lost.pending_player_scope_sacrifice_choice.is_none());
        assert!(lost.pending_player_scope_unless_payment.is_none());
        assert!(lost.pending_discard_batch.is_none());
        assert!(lost.pending_library_search_delivery.is_none());
        assert!(lost.pending_cost_move_resume.is_none());
        assert!(lost.pending_deferred_life_cost_resume.is_none());
        assert_eq!(
            crate::types::game_state::retained_result_waits(&lost).len(),
            1
        );
        assert!(lost.waiting_for.continued_mana_ability().is_none());
        assert_eq!(lost.return_result_frames.len(), 1);
        let lost_frame = &lost.return_result_frames[&ticket.occurrence];
        let original_frame = &runner.state().return_result_frames[&ticket.occurrence];
        assert_eq!(lost_frame.declarations, original_frame.declarations);
        assert_eq!(lost_frame.results, original_frame.results);
        assert_eq!(lost_frame.process, original_frame.process);
        let WaitingFor::ChooseOneOfBranch {
            branches,
            context,
            parent_occurrences,
            ..
        } = &lost.waiting_for
        else {
            unreachable!()
        };
        let WaitingFor::ChooseOneOfBranch {
            branches: original_branches,
            parent_occurrences: original_occurrences,
            ..
        } = &runner.state().waiting_for
        else {
            unreachable!()
        };
        assert_eq!(branches, original_branches);
        assert_eq!(parent_occurrences, original_occurrences);
        assert_eq!(context.counter_removal_result.as_ref(), Some(&ticket));
        let saved = serde_json::to_value(&lost).unwrap();
        for ingress in 0..3 {
            let error = match ingress {
                0 => serde_json::from_value::<GameState>(saved.clone()).unwrap_err(),
                1 => serde_json::from_value::<crate::types::game_state::TrustedGameStateEnvelope>(
                    serde_json::to_value(
                        crate::types::game_state::TrustedGameStateEnvelope::capture(lost.clone()),
                    )
                    .unwrap(),
                )
                .unwrap_err(),
                _ => serde_json::from_value::<crate::types::resolution::ResolutionStateWire>(
                    serde_json::to_value(
                        crate::types::resolution::ResolutionStateWire::from_game_state(
                            lost.clone(),
                        ),
                    )
                    .unwrap(),
                )
                .unwrap_err(),
            };
            assert!(
                error.to_string().contains(
                    "unrecoverable full program origin after retained authority recovery"
                ),
                "ingress {ingress}: precise full-origin loss: {error}"
            );
        }
        assert_eq!(serde_json::to_value(&lost).unwrap(), saved);
        assert_counter_refuses_intact(
            lost,
            P0,
            GameAction::ChooseBranch { index: 0 },
            "full origin erased",
        );
        for prompt in std::iter::once(runner.state().clone()).chain(recovered_prompts) {
            runner = GameRunner::from_state(prompt);
            runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            let queue = runner
                .state()
                .active_counter_removals()
                .expect("actual removal queue");
            assert_eq!(queue.source_ability_id, source);
            assert_eq!(queue.result_binding.as_ref(), Some(&ticket));
            assert!(queue.in_flight.is_some());
            assert!(
                matches!(runner.state().pending_replacement.as_ref().map(|pending| &pending.proposed),
            Some(crate::types::proposed_event::ProposedEvent::RemoveCounter { result_binding: Some(binding), .. }) if binding == &ticket)
            );
            let mut foreign = runner.state().clone();
            foreign
                .active_counter_removals_mut()
                .unwrap()
                .source_ability_id = recipient;
            assert_counter_refuses_intact(
                foreign,
                P0,
                GameAction::ChooseReplacement { index: 1 },
                "foreign actual removal queue source",
            );
            for ingress in 0..3 {
                let mut resumed =
                    GameRunner::from_state(restore_counter_input(runner.state(), ingress));
                resumed
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                resumed.advance_until_stack_empty();
                assert_counter_complete(resumed.state(), recipient, 2, 20);
            }
        }
    }

    #[test]
    fn public_counter_true_reader_removal_cannot_become_readerless_by_erasure() {
        use crate::game::scenario::{GameRunner, P0};
        use crate::types::ability::{AbilityCondition, ReturnResultId};
        for generated in [false, true] {
            let mut definition = complete_counter_definition();
            definition.sub_ability.as_mut().unwrap().condition =
                Some(AbilityCondition::CounterRemovedThisWay {
                    result: ReturnResultId(7),
                    counters: CounterMatch::OfType(CounterType::Generic("charge".into())),
                });
            if !generated {
                definition.effect = Box::new(Effect::RemoveCounter {
                    counter_type: Some(CounterType::Generic("charge".into())),
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
                });
            }
            let (mut runner, source, recipient) =
                paid_counter_fixture_with_replacement(definition, true);
            if generated {
                assert_complete_counter_prompt(runner.state(), P0, source, recipient);
                let WaitingFor::ChooseOneOfBranch { context, .. } = &runner.state().waiting_for
                else {
                    unreachable!()
                };
                assert!(super::super::readerless_generated_counter_producer(
                    runner.state(),
                    context.counter_removal_result.as_ref().unwrap()
                )
                .unwrap()
                .is_none());
                for ingress in 0..3 {
                    restore_counter_input(runner.state(), ingress);
                }
                let mut hostile = runner.state().clone();
                assert!(hostile
                    .take_active_ability_continuation()
                    .unwrap()
                    .is_some());
                let WaitingFor::ChooseOneOfBranch { continuation, .. } = &mut hostile.waiting_for
                else {
                    unreachable!()
                };
                *continuation = None;
                assert_counter_refuses_intact(
                    hostile,
                    P0,
                    GameAction::ChooseBranch { index: 0 },
                    "true reader erased at generated prompt",
                );
                runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            let mut erased = runner.state().clone();
            let queue = erased.take_active_counter_removals().unwrap().unwrap();
            assert!(erased.take_active_ability_continuation().unwrap().is_some());
            erased.push_counter_removals(queue);
            assert_counter_refuses_intact(
                erased,
                P0,
                GameAction::ChooseReplacement { index: 1 },
                &format!("true reader erased below queue: generated={generated}"),
            );
            for ingress in 0..3 {
                let mut resumed =
                    GameRunner::from_state(restore_counter_input(runner.state(), ingress));
                resumed
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                resumed.advance_until_stack_empty();
                assert_counter_complete(resumed.state(), recipient, 2, 20);
            }
        }
    }

    #[test]
    fn public_terminal_counter_legacy_none_process_remains_compatible() {
        use crate::game::scenario::P0;
        use crate::types::game_state::ReturnResultProcessId;
        let (runner, source, recipient) = paid_counter_fixture(complete_counter_definition());
        let mut state = GameState::new_two_player(42);
        // This supplemental representation row enters through the real resolver,
        // without a stack entry, resolving entry or independent tail.
        state.objects = runner.state().objects.clone();
        state.players = runner.state().players.clone();
        let mut definition = complete_counter_definition();
        definition.cost = None;
        definition.kind = AbilityKind::Spell;
        definition.sub_ability = None;
        let mut original =
            crate::game::ability_utils::build_resolved_from_def(&definition, source, P0);
        original.replace_target_occurrences(vec![(
            TargetRef::Object(recipient),
            Some(ObjectIncarnationRef::from_object(
                &state.objects[&recipient],
            )),
        )]);
        super::resolve(&mut state, &original, &mut Vec::new()).unwrap();
        let WaitingFor::ChooseOneOfBranch { context, .. } = &state.waiting_for else {
            panic!("real terminal resolver prompt")
        };
        let occurrence = context.counter_removal_result.as_ref().unwrap().occurrence;
        assert!(state.return_result_frames[&occurrence].process.is_some());
        super::super::choose_one_of::validate_waiting_result_owner(&state, &state.waiting_for)
            .unwrap();
        let frame = state.return_result_frames.get_mut(&occurrence).unwrap();
        frame.process = None;
        frame
            .admitted_program
            .as_mut()
            .unwrap()
            .bind_return_result_process(None);
        let WaitingFor::ChooseOneOfBranch {
            expected_return_result_process,
            context,
            ..
        } = &mut state.waiting_for
        else {
            unreachable!()
        };
        *expected_return_result_process = None;
        context.counter_removal_result.as_mut().unwrap().process = None;
        assert!(state.stack.is_empty());
        assert!(state.resolving_stack_entry.is_none());
        assert!(state.resolution_stack.is_empty());
        super::super::choose_one_of::validate_waiting_result_owner(&state, &state.waiting_for)
            .unwrap();
        let parent = counter_prompt_parent(&state);
        assert_eq!(parent.return_result_process, None);
        assert_eq!(parent.source_id, source);
        assert!(parent.sub_ability.is_none());
        let mut wrong_process = state.clone();
        let WaitingFor::ChooseOneOfBranch {
            expected_return_result_process,
            ..
        } = &mut wrong_process.waiting_for
        else {
            unreachable!()
        };
        *expected_return_result_process = Some(ReturnResultProcessId(99));
        assert_counter_refuses_intact(
            wrong_process,
            P0,
            GameAction::ChooseBranch { index: 1 },
            "legacy None versus wrong expected Some process",
        );
        for ingress in 0..3 {
            let mut resumed = restore_counter_input(&state, ingress);
            apply(&mut resumed, P0, GameAction::ChooseBranch { index: 1 }).unwrap();
            assert_eq!(
                resumed.objects[&recipient]
                    .counters
                    .get(&CounterType::Generic("charge".into())),
                Some(&8)
            );
            assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
            assert!(resumed.stack.is_empty());
            assert!(resumed.resolution_stack.is_empty());
            assert!(resumed.return_result_frames.is_empty());
        }
    }

    #[test]
    fn public_selected_local_counter_choice_preserves_fresh_process_and_present_empty_outer_result()
    {
        use crate::game::scenario::{P0, P1};
        use crate::types::ability::{ChooseOneOfKind, ReturnResultId};
        let mut counter = complete_counter_definition();
        let cost = counter.cost.take();
        counter.kind = AbilityKind::Spell;
        let choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                kind: ChooseOneOfKind::Ordinary,
                chooser: PlayerFilter::Opponent,
                branches: vec![
                    AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                    counter,
                ],
            },
        );
        let choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            },
        )
        .sub_ability(choice);
        let mut root = AbilityDefinition::new(AbilityKind::Activated, serde_json::from_value(serde_json::json!({
            "type": "ChangeZoneAll", "origin": "Battlefield", "destination": "Exile",
            "target": TargetFilter::SpecificObject { id: crate::types::identifiers::ObjectId(0) }
        })).unwrap()).sub_ability(choice);
        root.cost = cost;
        root.declares_return_result = Some(ReturnResultId(6));
        let (mut runner, source, recipient) = paid_counter_fixture(root);
        let WaitingFor::ChooseOneOfBranch {
            player: P1,
            return_result_occurrence: Some(outer),
            expected_return_result_process: Some(outer_process),
            context,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual P1 local body choice")
        };
        assert!(context.counter_removal_result.is_none());
        let (outer, outer_process) = (*outer, *outer_process);
        assert!(
            matches!(runner.state().return_result_frames[&outer].results.get(&ReturnResultId(6)),
            Some(crate::types::game_state::InstructionResult::ZoneChanges(records)) if records.is_empty())
        );
        *runner.state_mut() = restore_counter_input(runner.state(), 0);
        runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
        let WaitingFor::ChooseOneOfBranch {
            expected_return_result_process: Some(child_process),
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual generated child prompt")
        };
        assert_ne!(*child_process, outer_process);
        let child_root = counter_prompt_root(runner.state());
        assert_eq!(child_root.scoped_player, Some(P1));
        assert_eq!(child_root.controller, P0);
        assert_eq!(child_root.declares_return_result, Some(ReturnResultId(7)));
        settle_latched_three(runner, P0, P0, source, recipient, 20);
    }

    #[test]
    fn public_any_other_counter_projection_preserves_real_accepted_path() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::ability::{OpponentMayScope, ReplacementDefinition, SubAbilityLink};
        use crate::types::actions::DebugAction;
        let p2 = PlayerId(2);
        for local in [false, true] {
            for acceptor in [Some(P0), Some(p2), None] {
                let mut scenario = GameScenario::new_n_player(3, 42);
                scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
                scenario.with_cards_in_hand(P0, &["First input", "Second input", "Third input"]);
                scenario.with_library_top(P1, &["Replaced draw"]);
                let mut execute = complete_counter_definition();
                execute.kind = AbilityKind::Spell;
                execute.cost = None;
                execute.optional = true;
                execute.optional_for = Some(OpponentMayScope::AnyOtherPlayer);
                let Effect::ChooseCounterAdjustment { target, .. } = execute.effect.as_mut() else {
                    unreachable!()
                };
                *target = TargetFilter::SelfRef;
                if local {
                    let mut tail = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
                    tail.sub_link = SubAbilityLink::SequentialSibling;
                    execute = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChooseOneOf {
                            kind: ChooseOneOfKind::Ordinary,
                            chooser: PlayerFilter::Controller,
                            branches: vec![
                                AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                                execute,
                            ],
                        },
                    )
                    .sub_ability(tail);
                    execute.sub_ability.as_mut().unwrap().sub_link =
                        SubAbilityLink::SequentialSibling;
                }
                let replacement =
                    ReplacementDefinition::new(crate::types::replacements::ReplacementEvent::Draw)
                        .draw_scope(crate::types::ability::DrawReplacementScope::IndividualDraw)
                        .execute(execute);
                assert_eq!(
                    replacement.draw_scope,
                    Some(crate::types::ability::DrawReplacementScope::IndividualDraw)
                );
                replacement.validate_draw_scope().unwrap();
                let source = scenario
                    .add_artifact_from_oracle(P0, "Supplemental draw counter replacement", "")
                    .with_replacement_definition(replacement)
                    .id();
                let recipient = source;
                scenario.with_counter(recipient, CounterType::Generic("charge".into()), 5);
                let mut runner = scenario.build();
                runner.state_mut().debug_mode = true;
                runner
                    .act(GameAction::Debug(DebugAction::DrawCards {
                        player_id: P1,
                        count: 1,
                    }))
                    .unwrap();
                if local {
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::ChooseOneOfBranch { player: P0, .. }
                    ));
                    runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::OpponentMayChoice { player: P0, .. }
                ));
                let before = runner.state().clone();
                assert!(apply(
                    runner.state_mut(),
                    P1,
                    GameAction::DecideOptionalEffect { accept: true }
                )
                .is_err());
                assert_eq!(runner.state(), &before);
                for _ in 0..3 {
                    let WaitingFor::OpponentMayChoice {
                        player, remaining, ..
                    } = &runner.state().waiting_for
                    else {
                        break;
                    };
                    assert_ne!(*player, P1);
                    assert!(!remaining.contains(&P1));
                    let accept = acceptor == Some(*player);
                    runner
                        .act(GameAction::DecideOptionalEffect { accept })
                        .unwrap();
                    if accept {
                        break;
                    }
                }
                if let Some(acceptor) = acceptor {
                    let WaitingFor::ChooseOneOfBranch { context, .. } = &runner.state().waiting_for
                    else {
                        panic!("real accepted AnyOther counter prompt")
                    };
                    assert_eq!(context.accepting_player, Some(acceptor));
                    let WaitingFor::ChooseOneOfBranch {
                        player,
                        controller,
                        branches,
                        context,
                        ..
                    } = &runner.state().waiting_for
                    else {
                        unreachable!()
                    };
                    assert_eq!((*player, *controller), (P0, P0));
                    assert_eq!(branches.len(), 2);
                    assert!(matches!(
                        branches[0].effect.as_ref(),
                        Effect::RemoveCounter {
                            count: QuantityExpr::Fixed { value: 3 },
                            ..
                        }
                    ));
                    assert!(matches!(
                        branches[1].effect.as_ref(),
                        Effect::PutCounter {
                            count: QuantityExpr::Fixed { value: 3 },
                            ..
                        }
                    ));
                    assert_eq!(
                        context
                            .counter_removal_result
                            .as_ref()
                            .unwrap()
                            .recipient
                            .unwrap()
                            .object_id,
                        recipient
                    );
                    let parent = counter_prompt_parent(runner.state());
                    assert_eq!(parent.scoped_player, Some(acceptor));
                    assert!(!parent.optional && parent.optional_for.is_none());
                    assert_eq!(runner.state().players[0].hand.len(), 3);
                    // Replacements have no activation index, but must retain
                    // their complete admitted replacement-owned input.
                    assert!(counter_prompt_root(runner.state())
                        .declares_return_result
                        .is_some());
                    for n in 0..4 {
                        create_object(
                            runner.state_mut(),
                            CardId(97_000 + n),
                            P0,
                            format!("Later {n}"),
                            Zone::Hand,
                        );
                    }
                    for ingress in 0..3 {
                        let mut resumed = crate::game::scenario::GameRunner::from_state(
                            restore_counter_input(runner.state(), ingress),
                        );
                        resumed.act(GameAction::ChooseBranch { index: 1 }).unwrap();
                        resumed.advance_until_stack_empty();
                        assert_counter_complete(resumed.state(), recipient, 8, 20);
                        assert!(resumed.state().pending_replacement.is_none());
                    }
                } else {
                    runner.advance_until_stack_empty();
                    assert_counter_complete(runner.state(), recipient, 5, 20);
                    assert!(runner.state().pending_replacement.is_none());
                }
                assert_eq!(source, runner.state().objects[&source].id);
            }
        }
    }

    fn adjust_ability(
        target: crate::types::identifiers::ObjectId,
        source: crate::types::identifiers::ObjectId,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChooseCounterAdjustment {
                adjustment: CounterAdjustment::AddOrRemove,
                counters: CounterMatch::Any,
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        )
    }

    /// CR 122.1 + CR 702.62b: a suspended card in EXILE with time counters can be
    /// adjusted — proves the counter mutation is zone-agnostic.
    #[test]
    fn adjusts_time_counter_on_suspended_card_in_exile() {
        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        // The suspended-card filter (suspend keyword + time counter) is exercised
        // in the parser test; the resolver reads `ability.targets` directly, so the
        // scenario only needs a card in EXILE bearing a time counter to prove the
        // counter mutation is zone-agnostic.
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Suspended Card".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Time, 3);

        // Remove path: 3 -> 2.
        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "single-kind target must surface a remove/add choice, got {:?}",
            state.waiting_for
        );
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 0 },
        )
        .unwrap();
        assert_eq!(
            state
                .objects
                .get(&target)
                .unwrap()
                .counters
                .get(&CounterType::Time)
                .copied(),
            Some(2),
            "branch 0 (remove) must take the time counter from 3 to 2 in exile"
        );

        // Add path: 2 -> 3.
        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 1 },
        )
        .unwrap();
        assert_eq!(
            state
                .objects
                .get(&target)
                .unwrap()
                .counters
                .get(&CounterType::Time)
                .copied(),
            Some(3),
            "branch 1 (add) must put a time counter back, 2 to 3 in exile"
        );
    }

    /// CR 122.1 + CR 608.2d + CR 702.62b (Clockspinning end-to-end): resolve the
    /// PARSED ability shape — a `TargetOnly` parent over the battlefield∪exile `Or`
    /// filter plus an EMPTY-target `ChooseCounterAdjustment` sub — through the real
    /// `resolve_ability_chain`. Discriminating vs the direct-resolve tests above:
    /// the sub carries no targets of its own, so the load-bearing parent→sub
    /// target threading (`sub.targets.is_empty() && !ability.targets.is_empty()` in
    /// `effects::mod`) must copy the parent's resolved object into the slot-less
    /// sub before the per-kind branch set is built. Without that threading the sub
    /// finds no target and the effect silently no-ops; this test would then fail to
    /// surface a prompt. Target is a suspended card in EXILE (time counter), so the
    /// same path also re-confirms zone-agnostic mutation across the full pipeline.
    #[test]
    fn parsed_target_only_threads_parent_target_into_empty_sub() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{Comparator, FilterProp, TypeFilter, TypedFilter};
        use crate::types::counter::CounterMatch;
        use crate::types::keywords::KeywordKind;

        let mut state = GameState::new_two_player(99);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Suspended Card".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .counters
            .insert(CounterType::Time, 3);

        // EMPTY-target sub: must inherit the parent's resolved target via threading.
        let sub = ResolvedAbility::new(
            Effect::ChooseCounterAdjustment {
                adjustment: CounterAdjustment::AddOrRemove,
                counters: CounterMatch::Any,
                target: TargetFilter::ParentTarget,
                count: QuantityExpr::Fixed { value: 1 },
            },
            Vec::new(),
            source,
            PlayerId(0),
        );

        // Parent `TargetOnly` over the real `Or[battlefield permanent | exiled
        // suspended card]` filter (the parser's output). The filter is a
        // resolution-time no-op — targeting is established at cast — but it
        // documents the parsed shape the integration point feeds from.
        let mut ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Permanent],
                            controller: None,
                            properties: vec![FilterProp::InZone {
                                zone: Zone::Battlefield,
                            }],
                        }),
                        TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Card],
                            controller: None,
                            properties: vec![
                                FilterProp::InZone { zone: Zone::Exile },
                                FilterProp::HasKeywordKind {
                                    value: KeywordKind::Suspend,
                                },
                                FilterProp::Counters {
                                    counters: CounterMatch::OfType(CounterType::Time),
                                    comparator: Comparator::GE,
                                    count: QuantityExpr::Fixed { value: 1 },
                                },
                            ],
                        }),
                    ],
                },
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(sub));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The threaded parent target carries a single kind (Time), so the sub
        // surfaces the remove/add choice rather than no-opping.
        assert!(
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "parent target must thread into the empty sub and surface a choice, got {:?}",
            state.waiting_for
        );
        // Branch 0 (remove): 3 -> 2, proving the chosen object resolved through
        // the chain and the mutation is zone-agnostic in exile.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 0 },
        )
        .unwrap();
        assert_eq!(
            state
                .objects
                .get(&target)
                .unwrap()
                .counters
                .get(&CounterType::Time)
                .copied(),
            Some(2),
            "threaded remove branch must take the exiled time counter 3 -> 2"
        );
    }

    /// CR 122.1: with two distinct kinds the controller selects ONE kind (not
    /// for-each) — choosing "remove a lore counter" affects only Lore.
    #[test]
    fn single_kind_selection_leaves_other_kind_untouched() {
        let mut state = GameState::new_two_player(11);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 2);
            obj.counters.insert(CounterType::Lore, 1);
        }

        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        // Deterministic order by `as_str()`: ["P1P1", "lore"]. AddOrRemove emits
        // remove-then-add per kind, so branches are:
        //   0: remove P1P1, 1: add P1P1, 2: remove lore, 3: add lore.
        let WaitingFor::ChooseOneOfBranch {
            ref branch_descriptions,
            ..
        } = state.waiting_for
        else {
            panic!("expected ChooseOneOfBranch, got {:?}", state.waiting_for);
        };
        assert_eq!(
            branch_descriptions.len(),
            4,
            "two kinds x two ops = 4 branches"
        );

        // Choose "remove a lore counter" (index 2).
        apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 2 },
        )
        .unwrap();
        let obj = state.objects.get(&target).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Lore).copied().unwrap_or(0),
            0,
            "removing the lore kind must take lore 1 -> 0"
        );
        assert_eq!(
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            2,
            "the unchosen P1P1 kind must be untouched (single-kind, not for-each)"
        );
    }

    /// CR 608.2d: a target with no counters offers no legal kind — the effect does
    /// nothing and surfaces no prompt.
    #[test]
    fn no_counters_is_a_noop() {
        let mut state = GameState::new_two_player(5);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = adjust_ability(target, source);
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "no counters present must not surface a prompt"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseCounterAdjustment,
                    ..
                }
            )),
            "no-op must still emit EffectResolved"
        );
        assert!(
            state.objects.get(&target).unwrap().counters.is_empty(),
            "no counters added or removed"
        );
    }

    /// Building-block coverage for the operation-set axis across all three
    /// variants.
    #[test]
    fn counter_adjustment_operation_set() {
        assert!(CounterAdjustment::Add.allows_add());
        assert!(!CounterAdjustment::Add.allows_remove());
        assert!(!CounterAdjustment::Remove.allows_add());
        assert!(CounterAdjustment::Remove.allows_remove());
        assert!(CounterAdjustment::AddOrRemove.allows_add());
        assert!(CounterAdjustment::AddOrRemove.allows_remove());
    }

    /// CR 122.1 + CR 702.62b + CR 115.1: verifies that a suspended card in
    /// exile (bearing the Suspend keyword and a time counter) is accepted by the
    /// battlefield∪exile-suspended `Or` target filter through the live
    /// `find_legal_targets` path, not just via direct target injection.
    ///
    /// Also confirms that a battlefield permanent with counters is offered, while
    /// an exiled card that lacks the Suspend keyword is not.
    #[test]
    fn suspended_card_is_a_legal_target_through_filter() {
        use crate::game::targeting::find_legal_targets;
        use crate::types::ability::{Comparator, FilterProp, TypeFilter, TypedFilter};
        use crate::types::counter::CounterMatch;
        use crate::types::keywords::{Keyword, KeywordKind};
        use crate::types::mana::ManaCost;

        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Clockspinning".to_string(),
            Zone::Battlefield,
        );

        // A suspended card in exile: must have the Suspend keyword + time counter
        // to satisfy both legs of the exile filter (CR 702.62b).
        //
        // Off-zone keyword evaluation (filter.rs `HasKeywordKind`) delegates to
        // `off_zone_has_keyword_kind` → `effective_off_zone_keywords` which
        // reads `base_keywords`, not the battlefield-layer `keywords` field.
        // Push to `base_keywords` here so the `HasKeywordKind{Suspend}` predicate
        // fires correctly for a card in exile.
        let suspended = create_object(
            &mut state,
            CardId(20),
            PlayerId(1),
            "Suspended Card".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&suspended).unwrap();
            obj.base_keywords.push(Keyword::Suspend {
                count: 3,
                cost: ManaCost::zero(),
            });
            obj.counters.insert(CounterType::Time, 3);
        }

        // A battlefield permanent with counters — must also be a legal target.
        let battlefield_perm = create_object(
            &mut state,
            CardId(30),
            PlayerId(1),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&battlefield_perm).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Plus1Plus1, 2);
        }

        // An exiled card WITHOUT the Suspend keyword — must NOT be offered even
        // though it carries a time counter (CR 702.62b: suspend requires the
        // keyword, not just the counter).
        let bare_exile = create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "Bare Exiled Card".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&bare_exile)
            .unwrap()
            .counters
            .insert(CounterType::Time, 1);

        // The Or filter mirrors the parser output for Clockspinning sentence 1:
        // battlefield permanent OR suspended card in exile with ≥1 time counter.
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Permanent],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }],
                }),
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: None,
                    properties: vec![
                        FilterProp::InZone { zone: Zone::Exile },
                        FilterProp::HasKeywordKind {
                            value: KeywordKind::Suspend,
                        },
                        FilterProp::Counters {
                            counters: CounterMatch::OfType(CounterType::Time),
                            comparator: Comparator::GE,
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                }),
            ],
        };

        let targets = find_legal_targets(&state, &filter, PlayerId(0), source);

        // The suspended card must be offered as a legal target via the live
        // targeting path (not just via direct injection).
        assert!(
            targets.contains(&TargetRef::Object(suspended)),
            "suspended card in exile must be a legal target; got {targets:?}"
        );
        // The battlefield permanent must also be legal.
        assert!(
            targets.contains(&TargetRef::Object(battlefield_perm)),
            "battlefield permanent must be a legal target; got {targets:?}"
        );
        // The bare exiled card (no Suspend keyword) must NOT be legal.
        assert!(
            !targets.contains(&TargetRef::Object(bare_exile)),
            "exiled card without Suspend must not be offered; got {targets:?}"
        );
    }

    #[test]
    fn quantity_binding_keeps_original_x_recipient_and_sentinel_authorities() {
        use crate::types::ability::{ObjectScope, QuantityRef};
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".into(),
            Zone::Battlefield,
        );
        let recipient = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Recipient".into(),
            Zone::Battlefield,
        );
        for (id, power) in [(source, 1), (target, 3), (recipient, 7)] {
            state.objects.get_mut(&id).unwrap().power = Some(power);
        }
        let mut ability = adjust_ability(target, source);
        ability.chosen_x = Some(3);
        let x = QuantityExpr::Ref {
            qty: QuantityRef::Variable { name: "X".into() },
        };
        assert_eq!(
            bind_count(
                &state,
                &ability,
                &QuantityExpr::Multiply {
                    factor: 2,
                    inner: Box::new(x.clone())
                },
                recipient
            ),
            QuantityExpr::Fixed { value: 6 }
        );
        assert_eq!(
            bind_count(&state, &ability, &QuantityExpr::up_to(x), recipient),
            QuantityExpr::up_to(QuantityExpr::Fixed { value: 3 })
        );
        for (scope, value) in [
            (ObjectScope::Source, 1),
            (ObjectScope::Target, 3),
            (ObjectScope::Recipient, 7),
        ] {
            assert_eq!(
                bind_count(
                    &state,
                    &ability,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::Power { scope }
                    },
                    recipient
                ),
                QuantityExpr::Fixed { value }
            );
        }
        state.objects.get_mut(&recipient).unwrap().power = Some(-1);
        let negative = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::Recipient,
            },
        };
        assert_eq!(
            resolve_quantity_with_targets_and_recipient(&state, &negative, &ability, recipient),
            -1,
            "generic signed reader remains unchanged"
        );
        assert_eq!(
            bind_count(&state, &ability, &negative, recipient),
            QuantityExpr::Fixed { value: 0 }
        );
        assert_eq!(
            bind_count(
                &state,
                &ability,
                &QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Fixed { value: 2 }),
                    offset: -3
                },
                recipient
            ),
            QuantityExpr::Fixed { value: 0 }
        );
        for explicit in [
            QuantityExpr::Fixed { value: -1 },
            QuantityExpr::up_to(QuantityExpr::Fixed { value: -1 }),
        ] {
            assert_eq!(bind_count(&state, &ability, &explicit, recipient), explicit);
        }
    }

    #[test]
    fn recipient_binding_keeps_source_and_original_first_occurrence_authorities() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Same Name".into(),
            Zone::Battlefield,
        );
        let first = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Same Name".into(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Decoy".into(),
            Zone::Battlefield,
        );
        let mut ability = adjust_ability(first, source);
        let first_pin = ObjectIncarnationRef::from_object(&state.objects[&first]);
        ability.replace_target_occurrences(vec![
            (TargetRef::Object(first), Some(first_pin)),
            (
                TargetRef::Object(second),
                Some(ObjectIncarnationRef::from_object(&state.objects[&second])),
            ),
        ]);
        assert_eq!(
            recipient(&state, &ability, &TargetFilter::SelfRef)
                .unwrap()
                .object_id,
            source
        );
        assert_eq!(
            recipient(&state, &ability, &TargetFilter::ParentTarget).unwrap(),
            first_pin
        );
        let context_id = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Context referent".into(),
            Zone::Battlefield,
        );
        let context_pin = ObjectIncarnationRef::from_object(&state.objects[&context_id]);
        state.current_trigger_event = Some(GameEvent::PermanentTapped {
            object_id: context_id,
            caused_by: None,
            incarnation: Some(context_pin.incarnation),
        });
        state.objects.get_mut(&source).unwrap().attached_to =
            Some(crate::game::game_object::AttachTarget::Object(context_id));
        state.last_created_token_ids = vec![context_id];
        for target in [
            TargetFilter::TriggeringSource,
            TargetFilter::AttachedTo,
            TargetFilter::SpecificObject { id: context_id },
            TargetFilter::GrantingObject {
                bound: Some(context_pin),
            },
            TargetFilter::LastCreated,
        ] {
            assert_eq!(
                recipient(&state, &ability, &target),
                Some(context_pin),
                "the context authority must override unrelated inherited targets: {target:?}"
            );
        }
        assert_eq!(
            recipient(
                &state,
                &ability,
                &TargetFilter::ParentTargetSlot { index: 1 }
            ),
            Some(ObjectIncarnationRef::from_object(&state.objects[&second]))
        );
        state.objects.get_mut(&context_id).unwrap().incarnation += 1;
        assert!(
            recipient(&state, &ability, &TargetFilter::TriggeringSource).is_none(),
            "the event's stale incarnation must not be recaptured"
        );
        assert!(recipient(
            &state,
            &ability,
            &TargetFilter::GrantingObject {
                bound: Some(context_pin)
            }
        )
        .is_none());
        state.last_created_token_ids.push(second);
        assert!(
            recipient(&state, &ability, &TargetFilter::LastCreated).is_none(),
            "a singular instruction cannot silently select one member of a context set"
        );
        state.current_trigger_event = None;
        state.objects.get_mut(&source).unwrap().attached_to = None;
        for target in [
            TargetFilter::TriggeringSource,
            TargetFilter::EventTarget,
            TargetFilter::AttachedTo,
        ] {
            assert!(
                recipient(&state, &ability, &target).is_none(),
                "absent context cannot substitute an inherited target: {target:?}"
            );
        }
        state.objects.get_mut(&first).unwrap().incarnation += 1;
        assert!(
            recipient(&state, &ability, &TargetFilter::ParentTarget).is_none(),
            "later live target cannot replace stale first occurrence"
        );
        assert_eq!(
            recipient(&state, &ability, &TargetFilter::SelfRef)
                .unwrap()
                .object_id,
            source,
            "inherited target never overrides source"
        );
    }
}
