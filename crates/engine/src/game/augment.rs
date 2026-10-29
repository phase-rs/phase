//! Augment runtime support for Unstable Host creatures.

use std::sync::Arc;

use crate::game::merge::{install_merge_layer_effect, remove_merge_layer_effect};
use crate::game::printed_cards::intrinsic_copiable_values;
use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, CombineSource, CopiableValues, Effect, EffectError, EffectKind,
    ResolvedAbility, TargetFilter, TargetRef, TriggerDefinition,
};
use crate::types::card_type::{CardType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingContinuation, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::game_object::{DisplayIdentity, MergeKind};
use super::printed_cards;
use super::sba::move_to_graveyard_via_pipeline;
use super::zones;

pub fn resolve_combine_host(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    resolve_combine_host_owned(state, ability, events, None)
}

pub(crate) fn resolve_combine_host_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    admitted: Option<super::effects::AdmittedInstructionResult<'_>>,
) -> Result<(), EffectError> {
    let (source_kind, host_filter) = match &ability.effect {
        Effect::CombineHost { source, host } => (*source, host.as_ref()),
        _ => {
            return Err(EffectError::MissingParam(
                "CombineHost source/host".to_string(),
            ))
        }
    };

    let Some(augment_id) = resolve_source_id(state, ability, source_kind) else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CombineHost,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    let mut hosts = resolved_object_ids_for_filter(state, ability, host_filter);
    hosts.retain(|id| {
        state.objects.get(id).is_some_and(|obj| {
            obj.zone == Zone::Battlefield && obj.card_types.supertypes.contains(&Supertype::Host)
        })
    });
    hosts.sort_by_key(|id| id.0);
    hosts.dedup();

    match hosts.as_slice() {
        [] => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CombineHost,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        [host_id] => {
            combine_card_with_host(state, augment_id, *host_id, ability.controller, events);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CombineHost,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        _ => {
            let continuation = continuation_for_host_choice(ability, augment_id);
            let pending = match admitted {
                Some(admitted) => {
                    PendingContinuation::new_owned(Box::new(continuation), state, admitted)?
                }
                None => PendingContinuation::new(Box::new(continuation), state),
            };
            state.park_ability_continuation(pending);
            state.waiting_for = WaitingFor::ChooseFromZoneChoice {
                player: ability.controller,
                cards: hosts,
                count: 1,
                up_to: false,
                constraint: None,
                source_id: ability.source_id,
                reciprocal_role: None,
            };
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::CombineHost,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
    }
}

pub fn resolve_choose_augment_and_combine(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    resolve_choose_augment_and_combine_owned(state, ability, events, None)
}

pub(crate) fn resolve_choose_augment_and_combine_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    admitted: Option<super::effects::AdmittedInstructionResult<'_>>,
) -> Result<(), EffectError> {
    let (zones_to_search, filter, host_filter) = match &ability.effect {
        Effect::ChooseAugmentAndCombineWithHost {
            zones,
            filter,
            host,
        } => (zones.clone(), filter.as_ref(), host.as_ref()),
        _ => {
            return Err(EffectError::MissingParam(
                "ChooseAugmentAndCombineWithHost".to_string(),
            ))
        }
    };

    let frozen_host = freeze_unique_host_filter(state, ability, host_filter)
        .unwrap_or_else(|| host_filter.clone());
    let candidates = resolve_candidates(state, ability.controller, &zones_to_search, filter);

    match candidates.as_slice() {
        [] => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ChooseAugmentAndCombineWithHost,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        [augment_id] => {
            let mut direct = ability.clone();
            direct.effect = Effect::CombineHost {
                source: CombineSource::SpecificObject { id: *augment_id },
                host: Box::new(frozen_host),
            };
            resolve_combine_host_owned(state, &direct, events, admitted)
        }
        _ => {
            let continuation = continuation_for_augment_choice(ability, frozen_host);
            let pending = match admitted {
                Some(admitted) => {
                    PendingContinuation::new_owned(Box::new(continuation), state, admitted)?
                }
                None => PendingContinuation::new(Box::new(continuation), state),
            };
            state.park_ability_continuation(pending);
            state.waiting_for = WaitingFor::ChooseFromZoneChoice {
                player: ability.controller,
                cards: candidates,
                count: 1,
                up_to: false,
                constraint: None,
                source_id: ability.source_id,
                reciprocal_role: None,
            };
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ChooseAugmentAndCombineWithHost,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
    }
}

pub(crate) fn continuation_for_host_choice(
    ability: &ResolvedAbility,
    augment_id: ObjectId,
) -> ResolvedAbility {
    let mut continuation = ability.clone();
    continuation.clear_targets();
    continuation.effect = Effect::CombineHost {
        source: CombineSource::SpecificObject { id: augment_id },
        host: Box::new(TargetFilter::ParentTarget),
    };
    continuation
}

pub(crate) fn continuation_for_augment_choice(
    ability: &ResolvedAbility,
    frozen_host: TargetFilter,
) -> ResolvedAbility {
    let mut continuation = ability.clone();
    continuation.clear_targets();
    continuation.effect = Effect::CombineHost {
        source: CombineSource::ParentTarget,
        host: Box::new(frozen_host),
    };
    continuation
}

pub(crate) fn check_standalone_augment_permanents(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
    battlefield_snapshot: &[ObjectId],
) {
    let standalone: Vec<ObjectId> = battlefield_snapshot
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.is_phased_in()
                    && obj
                        .keywords
                        .iter()
                        .any(|keyword| matches!(keyword, Keyword::Augment))
                    && obj.merged_components.is_empty()
            })
        })
        .collect();

    for object_id in standalone {
        if !state
            .objects
            .get(&object_id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield && obj.is_phased_in())
        {
            continue;
        }
        // CR 614.6: This SBA departure is a battlefield-to-
        // graveyard zone change, so it must use the same replacement-aware
        // authority as the other SBA moves. A parked CR 616 choice is resumed
        // by the SBA fixpoint after the player chooses.
        if move_to_graveyard_via_pipeline(state, object_id, events) {
            return;
        }
        *any_performed = true;
    }
}

fn resolve_source_id(
    state: &GameState,
    ability: &ResolvedAbility,
    source: CombineSource,
) -> Option<ObjectId> {
    match source {
        CombineSource::Source => Some(ability.source_id),
        CombineSource::ParentTarget => ability.targets.iter().find_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        }),
        CombineSource::SpecificObject { id } => state.objects.contains_key(&id).then_some(id),
    }
}

fn freeze_unique_host_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    host_filter: &TargetFilter,
) -> Option<TargetFilter> {
    let mut hosts = resolved_object_ids_for_filter(state, ability, host_filter);
    hosts.retain(|id| {
        state
            .objects
            .get(id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield)
    });
    hosts.sort_by_key(|id| id.0);
    hosts.dedup();
    match hosts.as_slice() {
        [host_id] => Some(TargetFilter::SpecificObject { id: *host_id }),
        _ => None,
    }
}

fn resolve_candidates(
    state: &GameState,
    player: PlayerId,
    zones_to_search: &[Zone],
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    let mut candidates = Vec::new();
    for zone in zones_to_search {
        let ids: Vec<ObjectId> = match zone {
            Zone::Library => state.library_of(player).iter().copied().collect(),
            Zone::Graveyard => state.graveyard_of(player).iter().copied().collect(),
            Zone::Hand => state.players[player.0 as usize]
                .hand
                .iter()
                .copied()
                .collect(),
            Zone::Battlefield => state.battlefield.iter().copied().collect(),
            Zone::Exile => state.exile.iter().copied().collect(),
            _ => Vec::new(),
        };
        for id in ids {
            let ctx = crate::game::filter::FilterContext::from_source_with_controller(
                ObjectId(0),
                player,
            );
            if crate::game::filter::matches_target_filter(state, id, filter, &ctx)
                && !candidates.contains(&id)
            {
                candidates.push(id);
            }
        }
    }
    candidates
}

fn combine_card_with_host(
    state: &mut GameState,
    augment_id: ObjectId,
    host_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    if let Some(zone) = state.objects.get(&augment_id).map(|obj| obj.zone) {
        // CR 608.2h: no sever has run on this path, so the live attachment list is still
        // intact — capture it here for the LKI, through the one shared authority.
        let attachments = state
            .objects
            .get(&augment_id)
            .map(|obj| zones::capture_attachment_snapshot(state, obj))
            .unwrap_or_default();
        zones::apply_zone_exit_cleanup(state, augment_id, zone, Zone::Battlefield, attachments);
        zones::absorb_component(state, augment_id, Some(zone));
    }

    let Some((values, display)) = merged_copiable_values(state, augment_id, host_id) else {
        return;
    };
    remove_merge_layer_effect(state, host_id);

    if let Some(host) = state.objects.get_mut(&host_id) {
        let existing = std::mem::take(&mut host.merged_components);
        let mut ordered = if existing.is_empty() {
            vec![host_id]
        } else {
            existing
        };
        if !ordered.contains(&augment_id) {
            ordered.push(augment_id);
        }
        host.merged_components = ordered;
        host.merge_kind = Some(MergeKind::Augment);
        if host.pre_merge_is_token.is_none() {
            host.pre_merge_is_token = Some(host.is_token);
        }
    }

    install_merge_layer_effect(state, host_id, controller, values, display);
    events.push(GameEvent::Augmented {
        merged_id: host_id,
        augmenting_id: augment_id,
        controller,
    });
}

fn merged_copiable_values(
    state: &GameState,
    augment_id: ObjectId,
    host_id: ObjectId,
) -> Option<(CopiableValues, DisplayIdentity)> {
    let augment = state.objects.get(&augment_id)?;
    let host = state.objects.get(&host_id)?;
    let host_values = crate::game::layers::compute_current_copiable_values(state, host_id)
        .unwrap_or_else(|| intrinsic_copiable_values(host));

    let mut card_types = host_values.card_types.clone();
    merge_card_types(&mut card_types, &augment.base_card_types);
    card_types
        .supertypes
        .retain(|supertype| *supertype != Supertype::Host);

    let mut color = host_values.color.clone();
    push_unique_colors(&mut color, &augment.base_color);

    let mut keywords = host_values.keywords.clone();
    for keyword in &augment.base_keywords {
        if matches!(keyword, Keyword::Augment) {
            continue;
        }
        if !keywords.contains(keyword) {
            keywords.push(keyword.clone());
        }
    }

    let (abilities, triggers, trigger_printed_origins, statics, replacements) =
        merged_ability_sets(augment, &host_values);
    let values = CopiableValues {
        name: combine_name(&augment.base_name, &host_values.name),
        mana_cost: host_values.mana_cost,
        color,
        card_types,
        power: Some(host_values.power.unwrap_or(0) + augment.base_power.unwrap_or(0)),
        toughness: Some(host_values.toughness.unwrap_or(0) + augment.base_toughness.unwrap_or(0)),
        loyalty: host_values.loyalty,
        // CR 707.2: The merged object's copiable loyalty characteristic follows its host.
        printed_loyalty: host_values.printed_loyalty,
        keywords,
        abilities: Arc::new(abilities),
        trigger_definitions: Arc::new(triggers),
        trigger_printed_origins: Arc::new(trigger_printed_origins),
        replacement_definitions: Arc::new(replacements),
        static_definitions: Arc::new(statics),
        // An augment merge is a Host+Augment creature, never a Room — augment
        // is an Un-set mechanic with no Comprehensive Rules entry to cite.
        room_halves: None,
        // CR 722.2b: the merged object's prepare spell, if any, follows its
        // host's copiable values, as the loyalty characteristic does above.
        prepare_face: host_values.prepare_face.clone(),
        name_origin: Default::default(),
    };

    Some((values, host.display_identity()))
}

type MergedAbilitySets = (
    Vec<AbilityDefinition>,
    Vec<TriggerDefinition>,
    Vec<Option<crate::types::ability::TriggerPrintedOrigin>>,
    Vec<crate::types::ability::StaticDefinition>,
    Vec<crate::types::ability::ReplacementDefinition>,
);

fn merged_ability_sets(
    augment: &crate::game::game_object::GameObject,
    host_values: &CopiableValues,
) -> MergedAbilitySets {
    let host_body = host_values
        .trigger_definitions
        .iter()
        .find(|trigger| {
            matches!(
                trigger.mode,
                crate::types::triggers::TriggerMode::ChangesZone
            ) && trigger.destination == Some(Zone::Battlefield)
                && trigger.valid_card == Some(TargetFilter::SelfRef)
        })
        .and_then(|trigger| trigger.execute.as_deref().cloned());

    let mut abilities = Vec::new();
    for ability in augment.base_abilities.iter() {
        if ability.ability_tag == Some(crate::types::ability::AbilityTag::Augment) {
            continue;
        }
        if matches!(ability.effect.as_ref(), Effect::CombineHost { .. }) {
            continue;
        }
        if ability.kind == AbilityKind::Activated
            && matches!(ability.effect.as_ref(), Effect::NoOp)
            && ability
                .description
                .as_deref()
                .is_some_and(|description| description.trim_end().ends_with(':'))
        {
            if let Some(body) = host_body.clone() {
                abilities.push(splice_host_body_onto_activated_prefix(ability, &body));
            }
            continue;
        }
        abilities.push(ability.clone());
    }

    let mut triggers = Vec::new();
    let mut trigger_printed_origins = Vec::new();
    let augment_origins = printed_cards::base_trigger_printed_origins(augment);
    for (printed_occurrence, trigger) in augment.base_trigger_definitions.iter().enumerate() {
        if trigger.execute.is_none() {
            if let Some(body) = host_body.clone() {
                let mut combined = trigger.clone();
                combined.execute = Some(Box::new(body));
                triggers.push(combined);
                trigger_printed_origins
                    .push(augment_origins.get(printed_occurrence).cloned().flatten());
            }
            continue;
        }
        triggers.push(trigger.clone());
        trigger_printed_origins.push(augment_origins.get(printed_occurrence).cloned().flatten());
    }

    let statics = augment.base_static_definitions.iter().cloned().collect();
    let replacements = augment
        .base_replacement_definitions
        .iter()
        .filter(|definition| !printed_cards::is_runtime_non_copiable_replacement(definition))
        .cloned()
        .collect();

    (
        abilities,
        triggers,
        trigger_printed_origins,
        statics,
        replacements,
    )
}

fn splice_host_body_onto_activated_prefix(
    prefix: &AbilityDefinition,
    host_body: &AbilityDefinition,
) -> AbilityDefinition {
    let mut combined = host_body.clone();
    combined.kind = AbilityKind::Activated;
    combined.cost = prefix.cost.clone();
    combined.description = prefix.description.clone();
    combined.activation_restrictions = prefix.activation_restrictions.clone();
    combined.activator_filter = prefix.activator_filter.clone();
    combined.activation_zone = prefix.activation_zone;
    combined.ability_tag = prefix.ability_tag;
    combined
}

fn combine_name(augment_name: &str, host_name: &str) -> String {
    let host_suffix = host_name
        .rsplit_once(' ')
        .map(|(_, suffix)| suffix)
        .unwrap_or(host_name);
    if augment_name.ends_with('-') {
        format!("{augment_name}{host_suffix}")
    } else {
        format!("{augment_name} {host_suffix}")
    }
}

fn merge_card_types(into: &mut CardType, extra: &CardType) {
    for supertype in &extra.supertypes {
        if !into.supertypes.contains(supertype) {
            into.supertypes.push(*supertype);
        }
    }
    for core_type in &extra.core_types {
        if !into.core_types.contains(core_type) {
            into.core_types.push(*core_type);
        }
    }
    for subtype in &extra.subtypes {
        if !into.subtypes.contains(subtype) {
            into.subtypes.push(subtype.clone());
        }
    }
}

fn push_unique_colors(into: &mut Vec<ManaColor>, extra: &[ManaColor]) {
    for color in extra {
        if !into.contains(color) {
            into.push(*color);
        }
    }
}

#[cfg(test)]
mod owned_continuation_tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::types::ability::{
        AggregateFunction, ControllerRef, FilterProp, ObjectProperty, QuantityExpr, QuantityRef,
        ReturnResultId, SubAbilityLink, TypedFilter, ZoneChangeRecordSource,
    };
    use crate::types::actions::GameAction;

    #[test]
    fn generated_augment_choices_preserve_the_admitted_prior_result_and_tail() {
        // Supplemental typed program; both actual generated continuation forms
        // must finish through public responses with a nonempty prior result.
        for choose_augment in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
            let departed = scenario.add_creature(P0, "Prior departure", 3, 3).id();
            let first_host = scenario.add_creature(P0, "First Host", 2, 2).id();
            let second_host = scenario.add_creature(P0, "Second Host", 4, 4).id();
            let (first_augment, second_augment) = if choose_augment {
                (
                    scenario.add_card_to_library_top(P0, "First augment"),
                    scenario.add_card_to_library_top(P0, "Second augment"),
                )
            } else {
                (
                    scenario
                        .add_creature_to_hand(P0, "Selected augment", 1, 1)
                        .id(),
                    scenario
                        .add_creature_to_hand(P0, "Unselected augment", 2, 2)
                        .id(),
                )
            };
            let operation = if choose_augment {
                Effect::ChooseAugmentAndCombineWithHost {
                    zones: vec![Zone::Library],
                    filter: Box::new(TargetFilter::Any),
                    host: Box::new(TargetFilter::SpecificObject { id: first_host }),
                }
            } else {
                Effect::CombineHost {
                    source: CombineSource::SpecificObject { id: first_augment },
                    host: Box::new(TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasSupertype {
                                value: Supertype::Host,
                            }]),
                    )),
                }
            };
            let mut combine = AbilityDefinition::new(AbilityKind::Spell, operation);
            combine.target_choice_timing = crate::types::ability::TargetChoiceTiming::Resolution;
            let mut tail = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    player: TargetFilter::Controller,
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::ZoneChangeAggregate {
                            source: ZoneChangeRecordSource::InstructionResult {
                                result: ReturnResultId(7),
                            },
                            from: Some(Zone::Battlefield),
                            to: Some(Zone::Exile),
                            filter: TargetFilter::Typed(TypedFilter::creature()),
                            function: AggregateFunction::Sum,
                            property: ObjectProperty::Power,
                        },
                    },
                },
            );
            tail.sub_link = SubAbilityLink::SequentialSibling;
            combine.sub_ability = Some(Box::new(tail));
            let producer = serde_json::from_value(serde_json::json!({
                "type": "ChangeZoneAll", "origin": "Battlefield", "destination": "Exile",
                "target": TargetFilter::SpecificObject { id: departed }
            }))
            .unwrap();
            let mut root = AbilityDefinition::new(AbilityKind::Activated, producer);
            root.declares_return_result = Some(ReturnResultId(7));
            root.sub_ability = Some(Box::new(combine));
            let source = scenario
                .add_artifact_from_oracle(P0, "Supplemental augment owner", "")
                .with_ability_definition(root)
                .id();
            let mut runner = scenario.build();
            for host in [first_host, second_host] {
                let object = runner.state_mut().objects.get_mut(&host).unwrap();
                object.card_types.supertypes.push(Supertype::Host);
                object.base_card_types.supertypes.push(Supertype::Host);
            }
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                })
                .unwrap();
            for _ in 0..8 {
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::ChooseFromZoneChoice { .. }
                ) {
                    break;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                runner.act(GameAction::PassPriority).unwrap();
            }
            let WaitingFor::ChooseFromZoneChoice {
                player: P0,
                cards,
                count: 1,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("actual generated augment prompt must be reached");
            };
            let (selected, host, augment) = if choose_augment {
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&first_augment) && cards.contains(&second_augment));
                (second_augment, first_host, second_augment)
            } else {
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&first_host) && cards.contains(&second_host));
                (second_host, second_host, first_augment)
            };
            assert_eq!(runner.state().objects[&departed].zone, Zone::Exile);
            assert_eq!(runner.life(P0), 20);
            assert_eq!(runner.state().return_result_frames.len(), 1);
            let saved = serde_json::to_value(runner.state()).unwrap();
            *runner.state_mut() =
                serde_json::from_value(saved).expect("generated augment owner restores");
            let mut hostile = runner.state().clone();
            hostile.return_result_frames.clear();
            let before = hostile.clone();
            assert!(crate::game::engine::apply(
                &mut hostile,
                P0,
                GameAction::SelectCards {
                    cards: vec![selected]
                }
            )
            .is_err());
            assert_eq!(hostile, before);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![selected],
                })
                .unwrap();
            assert!(runner.state().objects[&host]
                .merged_components
                .contains(&augment));
            let unselected = if choose_augment {
                first_augment
            } else {
                second_augment
            };
            assert_eq!(
                runner.state().objects[&unselected].zone,
                if choose_augment {
                    Zone::Library
                } else {
                    Zone::Hand
                }
            );
            // CR 608.2c + CR 608.2i: the generated choice resumes the same
            // invocation and consumes the original departure's power once.
            assert_eq!(runner.life(P0), 23);
            assert_eq!(runner.life(P1), 20);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(runner.state().return_result_frames.is_empty());
        }
    }
}

#[cfg(test)]
mod dandan_read_sweep_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;

    /// CR 400.1: an augment search over the controller's library and graveyard
    /// reads the shared pile for the non-canonical seat, and the seat's own
    /// zones otherwise.
    #[test]
    fn candidates_read_the_storage_authority() {
        for (format, shared) in [
            (FormatConfig::dandan(), true),
            (FormatConfig::standard(), false),
        ] {
            let mut state = GameState::new(format, 2, 1);
            let p1 = PlayerId(1);
            let library = create_object(&mut state, CardId(1), p1, "Library".into(), Zone::Library);
            let graveyard = create_object(
                &mut state,
                CardId(2),
                p1,
                "Graveyard".into(),
                Zone::Graveyard,
            );

            let found = resolve_candidates(
                &state,
                p1,
                &[Zone::Library, Zone::Graveyard],
                &TargetFilter::Any,
            );

            assert_eq!(found, vec![library, graveyard], "shared={shared}");
            assert_eq!(state.players[1].library.is_empty(), shared);
        }
    }
}
