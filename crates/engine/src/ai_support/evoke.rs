//! Authenticated, read-only Evoke prompt facts for AI choice selection.
//!
//! The engine owns prompt identity, cost payloads, target legality, and
//! controller polarity. Phase-AI receives only the choice that was actually
//! offered plus a deliberately narrow immediate-value result.

use crate::game::ability_utils::build_resolved_from_def;
use crate::game::game_object::GameObject;
use crate::game::targeting::find_legal_targets_for_ability;
use crate::types::ability::{
    AbilityDefinition, CastVariantPaid, Effect, QuantityExpr, ReplacementDefinition, TargetFilter,
    TargetRef, TriggerCondition, TriggerDefinition,
};
use crate::types::actions::{AlternativeCastDecision, GameAction};
use crate::types::game_state::{AlternativeCastKeyword, CastingVariant, GameState, WaitingFor};
use crate::types::replacements::ReplacementEvent;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// A live Evoke choice, preserving the engine's prompt representation.
#[derive(Debug, Clone, PartialEq)]
pub enum EvokePromptDescriptor {
    AlternativeCast {
        normal_action: Box<GameAction>,
        evoke_action: Box<GameAction>,
    },
    CastingVariant {
        normal_action: Option<Box<GameAction>>,
        evoke_action: Box<GameAction>,
    },
}

/// What the narrow immediate-Evoke evaluator established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvokeImmediateOutcome {
    /// Every immediate effect was a recognized beneficial effect with a live,
    /// opposing target where one is required.
    ProvenUseful,
    /// Every recognized immediate target effect lacks a beneficial candidate.
    NoBeneficialTarget,
    /// The immediate surface is absent, optional, conditional, modal, mixed,
    /// unimplemented, neutral, harmful, or outside this deliberately small set.
    Unknown,
}

/// Engine-owned facts for a currently displayed Evoke prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct EvokePromptFacts {
    pub descriptor: EvokePromptDescriptor,
    pub outcome: EvokeImmediateOutcome,
}

/// Project the live Evoke prompt to its authenticated choice and narrow ETB
/// value boundary.
///
/// CR 702.74a: Evoke is an alternative casting cost whose sacrifice rider
/// triggers when the permanent enters. CR 603.6a + CR 115.2 + CR 102.2: this
/// reads only that entering permanent's immediate triggered/replacement effects
/// and uses the ordinary engine target authority for legal target enumeration.
pub fn evoke_prompt_facts(state: &GameState) -> Option<EvokePromptFacts> {
    let (player, object_id, descriptor) = match &state.waiting_for {
        WaitingFor::AlternativeCastChoice {
            player,
            object_id,
            card_id,
            keyword: AlternativeCastKeyword::Evoke,
            normal_cost,
            alternative_cost,
            alternative_additional_cost,
            ..
        } => {
            let offer = crate::game::casting::current_evoke_cast_choice_offer(
                state, *player, *object_id, *card_id,
            )?;
            if offer.normal_cost != *normal_cost
                || offer.alternative_cost != *alternative_cost
                || offer.alternative_additional_cost != *alternative_additional_cost
            {
                return None;
            }
            (
                *player,
                *object_id,
                EvokePromptDescriptor::AlternativeCast {
                    normal_action: Box::new(GameAction::ChooseAlternativeCast {
                        choice: AlternativeCastDecision::Normal,
                    }),
                    evoke_action: Box::new(GameAction::ChooseAlternativeCast {
                        choice: AlternativeCastDecision::Alternative,
                    }),
                },
            )
        }
        WaitingFor::CastingVariantChoice {
            player,
            object_id,
            card_id,
            options,
            ..
        } => {
            let object = state.objects.get(object_id)?;
            let fresh_options = crate::game::casting::current_casting_variant_choice_options(
                state, *player, *object_id,
            );
            if object.card_id != *card_id || *options != fresh_options {
                return None;
            }
            let evoke_index = options
                .iter()
                .position(|option| option.variant == CastingVariant::Evoke)?;
            let normal_action = options
                .iter()
                .position(|option| option.variant == CastingVariant::Normal);
            (
                *player,
                *object_id,
                EvokePromptDescriptor::CastingVariant {
                    normal_action: normal_action
                        .map(|index| Box::new(GameAction::ChooseCastingVariant { index })),
                    evoke_action: Box::new(GameAction::ChooseCastingVariant { index: evoke_index }),
                },
            )
        }
        _ => return None,
    };
    let entry_state = crate::game::casting::project_evoke_entry_state(state, player, object_id)?;
    let object = entry_state.objects.get(&object_id)?;

    Some(EvokePromptFacts {
        descriptor,
        outcome: immediate_evoke_outcome(&entry_state, player, object_id, object),
    })
}

fn immediate_evoke_outcome(
    state: &GameState,
    player: crate::types::player::PlayerId,
    object_id: crate::types::identifiers::ObjectId,
    object: &GameObject,
) -> EvokeImmediateOutcome {
    let mut saw_recognized_effect = false;
    let mut saw_no_beneficial_target = false;
    let mut saw_unknown_surface = false;
    let mut saw_proven_useful_effect = false;

    let immediate_abilities = object
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .filter(|trigger| qualifies_immediate_etb(object, trigger))
        .filter(|trigger| !is_evoke_sacrifice_rider(trigger))
        .map(|trigger| {
            (!trigger.optional)
                .then_some(trigger.execute.as_deref())
                .flatten()
        })
        .chain(
            object
                .replacement_definitions
                .iter_unchecked()
                .filter(|replacement| qualifies_immediate_replacement(replacement))
                .map(|replacement| replacement.execute.as_deref()),
        );

    for ability in immediate_abilities {
        let Some(ability) = ability else {
            saw_unknown_surface = true;
            continue;
        };
        let Some(effect_kind) = classify_immediate_ability(ability) else {
            saw_unknown_surface = true;
            continue;
        };
        saw_recognized_effect = true;
        match effect_kind {
            ImmediateEffect::ControllerDraw => saw_proven_useful_effect = true,
            ImmediateEffect::Destroy(target)
            | ImmediateEffect::ExilePermanent(target)
            | ImmediateEffect::Counter(target)
            | ImmediateEffect::ExileStackObject(target) => {
                let resolved = build_resolved_from_def(ability, object_id, player);
                let has_beneficial_target =
                    find_legal_targets_for_ability(state, target, &resolved)
                        .into_iter()
                        .any(|candidate| match effect_kind {
                            ImmediateEffect::Destroy(_) => {
                                candidate_is_opponent_permanent(state, player, &candidate)
                                    && destroy_would_succeed(state, &resolved, candidate)
                            }
                            ImmediateEffect::ExilePermanent(_) => {
                                candidate_is_opponent_permanent(state, player, &candidate)
                            }
                            ImmediateEffect::Counter(_) => {
                                candidate_is_opponent_stack_object(state, player, &candidate)
                                    && counter_would_succeed(state, &resolved, candidate)
                            }
                            ImmediateEffect::ExileStackObject(_) => {
                                candidate_is_opponent_stack_object(state, player, &candidate)
                            }
                            ImmediateEffect::ControllerDraw => false,
                        });
                if has_beneficial_target {
                    saw_proven_useful_effect = true;
                    continue;
                }
                saw_no_beneficial_target = true;
            }
        }
    }

    if saw_unknown_surface || !saw_recognized_effect {
        EvokeImmediateOutcome::Unknown
    } else if saw_proven_useful_effect {
        EvokeImmediateOutcome::ProvenUseful
    } else if saw_no_beneficial_target {
        EvokeImmediateOutcome::NoBeneficialTarget
    } else {
        EvokeImmediateOutcome::Unknown
    }
}

enum ImmediateEffect<'a> {
    ControllerDraw,
    Destroy(&'a TargetFilter),
    ExilePermanent(&'a TargetFilter),
    Counter(&'a TargetFilter),
    ExileStackObject(&'a TargetFilter),
}

fn classify_immediate_ability(ability: &AbilityDefinition) -> Option<ImmediateEffect<'_>> {
    if ability.optional
        || ability.condition.is_some()
        || ability.modal.is_some()
        || ability.else_ability.is_some()
        || !ability.mode_abilities.is_empty()
    {
        return None;
    }

    if !has_supported_rider_chain(ability.sub_ability.as_deref()) {
        return None;
    }

    match ability.effect.as_ref() {
        Effect::Draw {
            count: QuantityExpr::Fixed { value },
            target: TargetFilter::Controller,
        } if *value > 0 => Some(ImmediateEffect::ControllerDraw),
        Effect::Destroy { target, .. } => Some(ImmediateEffect::Destroy(target)),
        Effect::ChangeZone {
            origin: Some(Zone::Stack),
            destination: Zone::Exile,
            target,
            ..
        } => Some(ImmediateEffect::ExileStackObject(target)),
        Effect::ChangeZone {
            destination: Zone::Exile,
            target,
            ..
        } => Some(ImmediateEffect::ExilePermanent(target)),
        Effect::Counter { target, .. } => Some(ImmediateEffect::Counter(target)),
        _ => None,
    }
}

/// Ask the real destroy resolver whether this candidate is destroyable. This
/// inherits its indestructible and replacement-event handling rather than
/// attempting to infer a result from target legality alone.
fn destroy_would_succeed(
    state: &GameState,
    ability: &crate::types::ability::ResolvedAbility,
    candidate: TargetRef,
) -> bool {
    let mut preview = state.clone();
    let mut effect = ability.clone();
    effect.targets = vec![candidate];
    let mut events = Vec::new();
    crate::game::effects::destroy::resolve(&mut preview, &effect, &mut events).is_ok()
        && events.iter().any(|event| {
            matches!(
                event,
                crate::types::events::GameEvent::CreatureDestroyed { .. }
            )
        })
}

/// Ask the real counter resolver whether this candidate can actually be
/// countered. This preserves every `CantBeCountered` authority, including
/// both global statics and the stack spell's own functioning definition.
fn counter_would_succeed(
    state: &GameState,
    ability: &crate::types::ability::ResolvedAbility,
    candidate: TargetRef,
) -> bool {
    let mut preview = state.clone();
    let mut effect = ability.clone();
    effect.targets = vec![candidate];
    let mut events = Vec::new();
    crate::game::effects::counter::resolve(&mut preview, &effect, &mut events).is_ok()
        && events.iter().any(|event| {
            matches!(
                event,
                crate::types::events::GameEvent::SpellCountered { .. }
            )
        })
}

/// Accept the target-controller life rider shared by Solitude-class exile
/// triggers while refusing every other chained effect until its value is modeled.
fn has_supported_rider_chain(ability: Option<&AbilityDefinition>) -> bool {
    ability.is_none_or(|ability| {
        !ability.optional
            && ability.condition.is_none()
            && ability.modal.is_none()
            && ability.else_ability.is_none()
            && ability.mode_abilities.is_empty()
            && ability.sub_ability.is_none()
            && matches!(
                ability.effect.as_ref(),
                Effect::GainLife {
                    player: TargetFilter::ParentTargetController,
                    ..
                }
            )
    })
}

fn candidate_is_opponent_permanent(
    state: &GameState,
    player: crate::types::player::PlayerId,
    candidate: &TargetRef,
) -> bool {
    let TargetRef::Object(object_id) = candidate else {
        return false;
    };
    state.objects.get(object_id).is_some_and(|object| {
        object.zone == Zone::Battlefield
            && crate::game::players::is_opponent(state, player, object.controller)
    })
}

fn candidate_is_opponent_stack_object(
    state: &GameState,
    player: crate::types::player::PlayerId,
    candidate: &TargetRef,
) -> bool {
    let TargetRef::Object(object_id) = candidate else {
        return false;
    };
    state.stack.iter().any(|entry| {
        entry.id == *object_id && crate::game::players::is_opponent(state, player, entry.controller)
    })
}

fn qualifies_immediate_etb(object: &GameObject, trigger: &TriggerDefinition) -> bool {
    object.zone == Zone::Battlefield
        && object.card_types.core_types.iter().any(|core_type| {
            matches!(
                core_type,
                crate::types::card_type::CoreType::Artifact
                    | crate::types::card_type::CoreType::Battle
                    | crate::types::card_type::CoreType::Creature
                    | crate::types::card_type::CoreType::Enchantment
                    | crate::types::card_type::CoreType::Land
                    | crate::types::card_type::CoreType::Planeswalker
            )
        })
        && trigger.mode == TriggerMode::ChangesZone
        && trigger.valid_card == Some(TargetFilter::SelfRef)
        && trigger.destination == Some(Zone::Battlefield)
}

/// Ignore Evoke's compulsory sacrifice rider: it describes the cost of choosing
/// Evoke, not an ETB benefit that should compete with the printed trigger.
fn is_evoke_sacrifice_rider(trigger: &TriggerDefinition) -> bool {
    matches!(
        (
            &trigger.condition,
            trigger
                .execute
                .as_deref()
                .map(|ability| ability.effect.as_ref())
        ),
        (
            Some(TriggerCondition::CastVariantPaid {
                variant: CastVariantPaid::Evoke,
            }),
            Some(Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            })
        )
    )
}

fn qualifies_immediate_replacement(replacement: &ReplacementDefinition) -> bool {
    matches!(
        replacement.event,
        ReplacementEvent::ChangeZone | ReplacementEvent::Moved
    ) && replacement.valid_card == Some(TargetFilter::SelfRef)
        && replacement.destination_zone == Some(Zone::Battlefield)
}
