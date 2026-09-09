use std::collections::{HashMap, HashSet};

use engine::ai_support::{
    is_targeted_exchange_root, targeted_exchange_verdict, AiDecisionContext, CandidateAction,
    TargetedExchangeVerdict,
};
use engine::game::casting::{
    activated_ability_definitions, activation_source_and_activator_are_structurally_eligible,
    cast_spell_face_choice_available, effective_spell_cost,
    for_each_structurally_selectable_alternate_spell_payload,
    has_potentially_authorizing_object_cast_permission, spell_cost_is_payable_from_pool,
    spell_has_effective_keywords, spell_objects_available_to_cast,
    StructurallySelectableAlternateSpellPayload,
};
use engine::game::combat::AttackTarget;
use engine::game::functioning_abilities::{
    active_replacements, active_trigger_definitions, battlefield_active_triggers,
    game_active_statics, game_functioning_statics,
};
use engine::game::quantity::{
    ability_definition_has_only_unbound_variable_quantities_for_pre_cast,
    ability_definition_is_cast_stable_for_pre_cast, additional_cost_is_cast_stable_for_pre_cast,
    casting_permission_is_cast_stable_for_pre_cast, modal_choice_is_cast_stable_for_pre_cast,
    quantity_is_cast_stable_for_pre_cast, spell_casting_option_is_cast_stable_for_pre_cast,
    static_definition_is_cast_stable_for_pre_cast, trigger_definition_is_cast_stable_for_pre_cast,
    try_resolve_quantity_in_source_context,
};
use engine::game::triggers::{
    synthetic_keyword_spell_cast_trigger_applies, trigger_definition_functions_in_zone,
};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction,
    ContinuousModification, CostCategory, Effect, PtValue, TargetFilter, TargetRef, TypeFilter,
    TypedFilter,
};
use engine::types::ability_visit::visit_ability_def;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, DayNight, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaSourcePenalty, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{AdditionalCostTaxAction, StaticMode};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use std::ops::ControlFlow;

use crate::cast_facts::CastCostMode;
use crate::combat_ai::is_lethal_attack_available;
use crate::config::AiConfig;
use crate::context::AiContext;
use crate::planner::PreparedCandidate;
use crate::policies::context::{collect_ability_effects, PolicyContext};
use crate::policies::effect_classify::{
    effect_polarity, extract_target_filter, targets_creatures_only, EffectPolarity,
};
use crate::policies::stack_awareness::{has_pending_removal, will_target_die_from_stack};
use crate::policies::strategy_helpers::can_pay_ward_cost;
use crate::search::ability_is_temporary_combat_modifier;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateDecision {
    Reject,
    Allow,
    AllowWithPenalty(f64),
}

#[derive(Debug, Clone)]
pub struct GatedCandidate {
    pub source_index: usize,
    pub candidate: CandidateAction,
    pub penalty: f64,
}

/// Layering rule: `tactical_gate` owns rule-derived legality and futility
/// decisions that are provably never useful, such as impossible counters,
/// destroy-vs-indestructible targets, redundant removal on already-dying
/// creatures, and pump with no live combat window. Judgment-weighted
/// preferences stay in `policies/`; the same predicate must not be scored in
/// both layers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TacticalWindow {
    OwnPreCombatMain,
    OwnPostCombatMain,
    OpponentMain,
    CombatBeforeBlocks,
    CombatAfterBlocks,
    CombatDamage,
    StackResponse,
    EndStep,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct TacticalFacts {
    window: TacticalWindow,
    live_stack_response: bool,
    pass_preserves_stronger_window: bool,
}

impl TacticalFacts {
    fn derive(state: &GameState, ai_player: PlayerId) -> Self {
        let live_stack_response = !state.stack.is_empty();
        let own_turn = engine::game::turn_control::turn_decision_maker(state) == ai_player;
        let window = if live_stack_response {
            TacticalWindow::StackResponse
        } else {
            match state.phase {
                Phase::PreCombatMain if own_turn => TacticalWindow::OwnPreCombatMain,
                Phase::PostCombatMain if own_turn => TacticalWindow::OwnPostCombatMain,
                Phase::PreCombatMain | Phase::PostCombatMain => TacticalWindow::OpponentMain,
                Phase::BeginCombat | Phase::DeclareAttackers => TacticalWindow::CombatBeforeBlocks,
                Phase::DeclareBlockers | Phase::EndCombat => TacticalWindow::CombatAfterBlocks,
                Phase::CombatDamage => TacticalWindow::CombatDamage,
                Phase::End | Phase::Cleanup => TacticalWindow::EndStep,
                _ => TacticalWindow::Other,
            }
        };
        let pass_preserves_stronger_window = own_turn
            && state.stack.is_empty()
            && matches!(
                state.phase,
                Phase::PreCombatMain | Phase::BeginCombat | Phase::DeclareAttackers
            );

        Self {
            window,
            live_stack_response,
            pass_preserves_stronger_window,
        }
    }
}

pub fn gate_candidates(
    state: &GameState,
    decision: &AiDecisionContext,
    candidates: Vec<CandidateAction>,
    ai_player: PlayerId,
    config: &AiConfig,
    context: &AiContext,
) -> Vec<GatedCandidate> {
    let prepared = candidates
        .into_iter()
        .enumerate()
        .map(|(source_index, candidate)| PreparedCandidate {
            source_index,
            candidate,
            payment_successor: None,
        })
        .collect();
    gate_prepared_candidates(state, decision, prepared, ai_player, config, context)
}

pub(crate) fn gate_prepared_candidates(
    state: &GameState,
    decision: &AiDecisionContext,
    candidates: Vec<PreparedCandidate>,
    ai_player: PlayerId,
    config: &AiConfig,
    context: &AiContext,
) -> Vec<GatedCandidate> {
    candidates
        .into_iter()
        .filter_map(|prepared| {
            let source_index = prepared.source_index;
            let candidate = prepared.candidate;
            let decision_result = {
                let policy_ctx = PolicyContext {
                    state,
                    decision,
                    candidate: &candidate,
                    ai_player,
                    config,
                    context,
                    cast_facts: None,
                    search_depth: crate::policies::context::SearchDepth::Root,
                };
                assess_candidate(&policy_ctx)
            };
            match decision_result {
                GateDecision::Reject => None,
                GateDecision::Allow => Some(GatedCandidate {
                    source_index,
                    candidate,
                    penalty: 0.0,
                }),
                GateDecision::AllowWithPenalty(penalty) => Some(GatedCandidate {
                    source_index,
                    candidate,
                    penalty,
                }),
            }
        })
        .collect()
}

fn assess_candidate(ctx: &PolicyContext<'_>) -> GateDecision {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => assess_pre_cast(ctx),
        GameAction::ChooseTarget {
            target: Some(target),
        } => {
            if let Some(rejection) = reject_futile_target(ctx, target) {
                return rejection;
            }
            let penalty = target_choice_penalty(ctx, target);
            if penalty < 0.0 {
                GateDecision::AllowWithPenalty(penalty)
            } else {
                GateDecision::Allow
            }
        }
        GameAction::ChooseTarget { target: None } => GateDecision::Allow,
        // CR 702.51a (Convoke) / CR 702.126a (Improvise) / Waterbend: a
        // dual-purpose permanent's Colorless convoke-family marker must not be
        // taken while a sibling candidate for the SAME object could still pay
        // an outstanding colored pip via its native mana ability — spending
        // the Colorless marker first permanently strands that pip and
        // dead-ends `ManaPayment` (the Metallic Rebuke bug:
        // `search::fallback_action`'s "can_cast_object_now has a gap" panic).
        // Zero-cost dominance, not a scoring preference: the native ability
        // can always still cover the trailing generic slot afterward.
        GameAction::TapForConvoke {
            object_id,
            mana_type: ManaType::Colorless,
        } => {
            if matches!(ctx.decision.waiting_for, WaitingFor::ManaPayment { .. })
                && crate::mana_colors::convoke_native_tap_still_demanded(
                    ctx.state,
                    &ctx.decision.candidates,
                    *object_id,
                )
            {
                GateDecision::Reject
            } else {
                GateDecision::Allow
            }
        }
        GameAction::SelectTargets { targets } => {
            for target in targets {
                if let Some(rejection) = reject_futile_target(ctx, target) {
                    return rejection;
                }
            }
            let penalty = targets
                .iter()
                .map(|target| target_choice_penalty(ctx, target))
                .sum::<f64>();
            if penalty < 0.0 {
                GateDecision::AllowWithPenalty(penalty)
            } else {
                GateDecision::Allow
            }
        }
        // CR 601.2: Announcing a spell commits the caster — the rules provide
        // no strategic rewind. CancelCast exists only as a mechanical escape
        // when the cast cannot be completed (no legal targets after a
        // replacement effect, unaffordable cost after a cost-increase static).
        // Removing it from the strategic pool prevents regret-based cast/cancel
        // loops — once a ChooseTarget or pay-cost option exists, the AI must
        // pick one. The genuine-escape cases fall through to
        // `search::fallback_action`, which emits CancelCast when the scored
        // pool is empty.
        GameAction::CancelCast => GateDecision::Reject,
        _ => GateDecision::Allow,
    }
}

fn assess_pre_cast(ctx: &PolicyContext<'_>) -> GateDecision {
    if zero_direct_spell_is_safe_to_reject(ctx) {
        return GateDecision::Reject;
    }

    // CR 601.2c + CR 608.2c: Target-sourced self-damage and fight exchanges are
    // evaluated from reducer-issued, fully-bound target paths before scoring.
    // `Indeterminate` stays fail-open: this is a proof-backed veto only.
    if is_targeted_exchange_root(&ctx.candidate.action)
        && matches!(
            targeted_exchange_verdict(ctx.state, ctx.candidate),
            TargetedExchangeVerdict::Reject
        )
    {
        return GateDecision::Reject;
    }

    // CR 608.2c: Reject abilities whose source-type condition is known to fail.
    // E.g. Figure of Fable's "{1}{G/W}{G/W}: If this creature is a Scout, ..." when
    // the source is not currently a Scout. The ability is legal to activate but wastes mana.
    if let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = &ctx.candidate.action
    {
        if let Some(object) = ctx.state.objects.get(source_id) {
            if let Some(ability_def) = object.abilities.get(*ability_index) {
                if let Some(AbilityCondition::SourceMatchesFilter { ref filter }) =
                    ability_def.condition
                {
                    if !engine::game::filter::matches_target_filter(
                        ctx.state,
                        *source_id,
                        filter,
                        &engine::game::filter::FilterContext::from_source(ctx.state, *source_id),
                    ) {
                        return GateDecision::Reject;
                    }
                }
            }
        }
    }

    // When a lethal attack is available (opponent has no untapped blockers and AI has
    // enough power to kill them), reject pre-combat main spells. Attacking first is
    // almost always correct — spending mana dorks or convoke creatures before attacking
    // removes them from the attack and misses the lethal window.
    if matches!(
        TacticalFacts::derive(ctx.state, ctx.ai_player).window,
        TacticalWindow::OwnPreCombatMain
    ) && is_lethal_attack_available(ctx.state, ctx.ai_player)
    {
        // Carve-out: direct-damage spells that can target players may themselves be
        // lethal or supplement the attack — allow with a mild penalty.
        let effects = ctx.effects();
        let is_direct_damage = effects.iter().any(|e| {
            matches!(
                e,
                Effect::DealDamage {
                    target: TargetFilter::Any | TargetFilter::Player,
                    ..
                }
            )
        });
        if is_direct_damage {
            return GateDecision::AllowWithPenalty(-5.0);
        }
        return GateDecision::Reject;
    }

    let effects = ctx.effects();
    if effects.is_empty() {
        return GateDecision::Allow;
    }

    if effects
        .iter()
        .any(|effect| matches!(effect, Effect::Counter { .. }))
        && (ctx.state.stack.is_empty()
            || ctx
                .state
                .stack
                .iter()
                .all(|entry| entry.controller == ctx.ai_player))
    {
        return GateDecision::Reject;
    }

    if is_redundant_creature_only_removal(ctx, &effects) {
        return GateDecision::Reject;
    }

    if let Some((power_bonus, toughness_bonus)) = pure_fixed_pump_bonus(&effects) {
        let source_is_spell = ctx.source_object().is_some_and(|source| {
            source.card_types.core_types.contains(&CoreType::Instant)
                || source.card_types.core_types.contains(&CoreType::Sorcery)
        });
        if source_is_spell {
            let facts = TacticalFacts::derive(ctx.state, ctx.ai_player);
            if should_reject_pump_window(ctx, &facts, power_bonus, toughness_bonus) {
                return GateDecision::Reject;
            }
            if facts.pass_preserves_stronger_window && !facts.live_stack_response {
                return GateDecision::AllowWithPenalty(-1.0);
            }
        } else if let Some(source_id) = activated_ueot_pump_source(ctx) {
            // CR 514.2: an "until end of turn" pump ends at cleanup, so an
            // activation outside a live combat/stack window buys nothing —
            // the same waste the spell branch above rejects. Complementary to
            // `search::empty_stack_activation_is_low_value`, which only lets
            // the priority fast path skip searching when EVERY candidate is
            // low value; this is the per-candidate bound.
            let facts = TacticalFacts::derive(ctx.state, ctx.ai_player);
            // A repeatable mana-sink pump on our own unblocked attacker after
            // blockers are declared is a value judgment (how much face damage
            // is this mana worth?), not a provable waste — leave it to policy
            // scoring rather than the categorical gate.
            let mana_sink_into_open_board = power_bonus > 0
                && matches!(
                    facts.window,
                    TacticalWindow::CombatAfterBlocks | TacticalWindow::CombatDamage
                )
                && self_pump_on_unblocked_attacker(ctx.state, ctx.ai_player, source_id, &effects);
            if !mana_sink_into_open_board {
                if should_reject_pump_window(ctx, &facts, power_bonus, toughness_bonus) {
                    return GateDecision::Reject;
                }
                if facts.pass_preserves_stronger_window && !facts.live_stack_response {
                    return GateDecision::AllowWithPenalty(-1.0);
                }
            }
        }
    }

    GateDecision::Allow
}

/// Conservative root-only proof for ordinary casts whose complete direct effect
/// has a known zero magnitude. Any unmodelled cast consequence leaves the action
/// available for policy scoring.
fn zero_direct_spell_is_safe_to_reject(ctx: &PolicyContext<'_>) -> bool {
    let GameAction::CastSpell {
        object_id,
        payment_mode: CastPaymentMode::Auto,
        ..
    } = &ctx.candidate.action
    else {
        return false;
    };
    let Some(object) = ctx.state.objects.get(object_id) else {
        return false;
    };
    // CR 117.1b + CR 602.2a: an already-announced ability can read the cast
    // ledger at resolution after its source has left the relevant zone. The
    // narrow proof does not simulate those payloads, so any nonempty stack
    // fails open.
    if !ctx.state.stack.is_empty() {
        return false;
    }
    if object.zone != Zone::Hand
        || object.controller != ctx.ai_player
        || object.owner != ctx.ai_player
        || object.modal.is_some()
        || cast_spell_face_choice_available(object)
        || !object.parse_warnings.is_empty()
        || !(object.card_types.core_types.contains(&CoreType::Instant)
            || object.card_types.core_types.contains(&CoreType::Sorcery))
        || object.additional_cost.is_some()
        || object.strive_cost.is_some()
        || !object.casting_options.is_empty()
        || !object.casting_permissions.is_empty()
        || !object.trigger_definitions.is_empty()
        || !object.replacement_definitions.is_empty()
        || !object.static_definitions.is_empty()
        || spell_has_effective_keywords(ctx.state, *object_id)
        || !payment_population_is_stable(ctx.state, ctx.ai_player, *object_id)
        || !ctx.state.delayed_triggers.is_empty()
    {
        return false;
    }

    // These presence checks deliberately avoid duplicating the casting cost
    // and payment authorities. A possible external cost/grant is enough to
    // make this a non-proof.
    if game_active_statics(ctx.state).any(|(_, definition)| {
        matches!(
            definition.mode,
            StaticMode::CastWithAlternativeCost { .. } | StaticMode::CastWithKeyword { .. }
        )
    }) || game_functioning_statics(ctx.state).any(|(_, definition)| {
        matches!(
            definition.mode,
            StaticMode::ImposeAdditionalCost {
                action: AdditionalCostTaxAction::Cast,
                ..
            }
        )
    }) || !engine::game::static_abilities::player_life_payment_colors(ctx.state, ctx.ai_player)
        .is_empty()
        || ctx
            .state
            .pending_next_spell_modifiers
            .iter()
            .any(|modifier| modifier.player == ctx.ai_player)
        || ctx.state.transient_continuous_effects.iter().any(|effect| {
            effect.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::GrantStaticAbility { definition }
                        if matches!(definition.mode, StaticMode::CastWithKeyword { .. })
                )
            })
        })
    {
        return false;
    }

    let Some(facts) = ctx.cast_facts() else {
        return false;
    };
    let direct_effect_is_known_zero = facts.cost_mode == CastCostMode::Printed
        && facts.immediate_etb_triggers.is_empty()
        && facts.immediate_replacements.is_empty()
        && facts.primary_effects.len() == 1
        && facts.primary_effects.iter().all(|definition| {
            definition_is_componentwise_known_zero(
                ctx.state,
                definition,
                object.controller,
                *object_id,
                true,
                false,
            )
        });
    if !direct_effect_is_known_zero {
        return false;
    }

    if has_relevant_functioning_trigger(ctx.state, *object_id)
        || cast_has_relevant_payoff(ctx.state, ctx.ai_player, Some(*object_id))
    {
        return false;
    }

    true
}

/// Returns whether recording this cast leaves a currently available consumer
/// not proven unchanged. The candidate's already-proven direct spell tree is
/// not counted as its own consumer; all other metadata and definitions remain
/// fail-open. This remains a narrow check rather than a projection of later
/// game state.
fn cast_has_relevant_payoff(
    state: &GameState,
    caster: PlayerId,
    candidate_spell: Option<ObjectId>,
) -> bool {
    let casts_this_turn = state
        .spells_cast_this_turn_by_player
        .get(&caster)
        .map_or(0, |spells| spells.len());

    // CR 502.2a: Day/Night counts the active TEAM's spells. The day/night
    // transition reducer currently keys this on `active_player`, so do not
    // treat any shared-team boundary as proven until that separate authority
    // becomes team-aware.
    let caster_has_teammate = !engine::game::players::teammates(state, caster).is_empty();
    if state.day_night.is_some() && caster_has_teammate {
        return true;
    }
    if caster == state.active_player
        && matches!(
            (state.day_night, casts_this_turn),
            (Some(DayNight::Day), 0) | (Some(DayNight::Night), 1)
        )
    {
        return true;
    }

    // CR 702.117a: the cast can make a held Surge spell available to its
    // caster or teammate when none of them has cast a spell this turn yet.
    let enables_surge = casts_this_turn == 0
        && engine::game::players::teammates(state, caster)
            .into_iter()
            .chain(std::iter::once(caster))
            .all(|player| {
                state
                    .spells_cast_this_turn_by_player
                    .get(&player)
                    .is_none_or(|spells| spells.is_empty())
            })
        && std::iter::once(caster)
            .chain(engine::game::players::teammates(state, caster))
            .any(|player| {
                spell_objects_available_to_cast(state, player)
                    .into_iter()
                    .filter_map(|spell| state.objects.get(&spell))
                    .filter(|object| spell_identity_is_available_to_caster(state, caster, object))
                    .any(|object| object_has_surge_keyword(state, object))
            });

    // The casting authority includes hand, permission-backed exile/graveyard/
    // command cards, and top-of-library permissions. Only a card identity the
    // caster can actually observe may affect hard candidate support.
    let castable_spells: HashSet<_> = spell_objects_available_to_cast(state, caster)
        .into_iter()
        .collect();

    enables_surge
        || state.objects.values().any(|object| {
            (matches!(object.zone, Zone::Battlefield | Zone::Stack)
                || castable_spells.contains(&object.id))
                && spell_identity_is_available_to_caster(state, caster, object)
                && object_has_cast_unstable_consumer(state, caster, object, candidate_spell)
        })
        || state.objects.values().any(|object| {
            // Do not inspect a hidden opponent card's definitions. The identity
            // guard must precede runtime grants and every ability metadata read.
            spell_identity_is_available_to_caster(state, caster, object)
                && !matches!(object.zone, Zone::Battlefield | Zone::Stack)
                && candidate_spell != Some(object.id)
                && activated_ability_definitions(state, object.id)
                    .into_iter()
                    .any(|(_, definition)| {
                        definition.kind == AbilityKind::Activated
                            && activation_source_and_activator_are_structurally_eligible(
                                state,
                                caster,
                                object.id,
                                &definition,
                            )
                            && ability_has_cast_unstable_consumer(&definition)
                    })
        })
        || active_replacements(state).any(|(_, object, replacement)| {
            object.zone == Zone::Command
                && !replacement.is_consumed
                && spell_identity_is_available_to_caster(state, caster, object)
        })
        || state
            .pending_damage_replacements
            .iter()
            .any(|replacement| !replacement.is_consumed)
        || state.objects.values().any(|object| {
            spell_identity_is_available_to_caster(state, caster, object)
                && has_potentially_authorizing_object_cast_permission(object, caster)
                && object
                    .casting_permissions
                    .iter()
                    .any(|permission| !casting_permission_is_cast_stable_for_pre_cast(permission))
        })
        || game_functioning_statics(state)
            .any(|(_, definition)| !static_definition_is_cast_stable_for_pre_cast(definition))
}

fn object_has_surge_keyword(
    state: &GameState,
    object: &engine::game::game_object::GameObject,
) -> bool {
    object_has_cast_history_keyword(state, object)
}

fn spell_identity_is_available_to_caster(
    state: &GameState,
    caster: PlayerId,
    object: &engine::game::game_object::GameObject,
) -> bool {
    state.viewer_knows_card_identity(caster, object.id)
        || !object.face_down
            && (object.zone.is_public() || object.zone == Zone::Hand && object.owner == caster)
}

fn ability_has_cast_unstable_consumer(definition: &AbilityDefinition) -> bool {
    !ability_definition_is_cast_stable_for_pre_cast(definition)
}

fn object_has_cast_unstable_consumer(
    state: &GameState,
    caster: PlayerId,
    object: &engine::game::game_object::GameObject,
    candidate_spell: Option<ObjectId>,
) -> bool {
    !object.casting_restrictions.is_empty()
        || object
            .casting_options
            .iter()
            .any(|option| !spell_casting_option_is_cast_stable_for_pre_cast(option))
        || object
            .casting_permissions
            .iter()
            .any(|permission| !casting_permission_is_cast_stable_for_pre_cast(permission))
        || object
            .modal
            .as_ref()
            .is_some_and(|modal| !modal_choice_is_cast_stable_for_pre_cast(modal))
        || object
            .additional_cost
            .as_ref()
            .is_some_and(|cost| !additional_cost_is_cast_stable_for_pre_cast(cost))
        || object.abilities.iter().any(|ability| {
            ability_has_cast_unstable_consumer(ability)
                && !(candidate_spell == Some(object.id)
                    && definition_is_componentwise_known_zero(
                        state,
                        ability,
                        object.controller,
                        object.id,
                        true,
                        false,
                    ))
                && !(object.zone == Zone::Battlefield
                    && engine::game::mana_abilities::is_mana_ability(ability)
                    && mana_ability_has_only_unbound_variable_quantities(ability))
        })
        || object
            .static_definitions
            .as_slice()
            .iter()
            .any(|definition| !static_definition_is_cast_stable_for_pre_cast(definition))
        || object_has_cast_history_keyword(state, object)
        || !object.replacement_definitions.as_slice().is_empty()
        || object
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|trigger| trigger_definition_has_cast_unstable_consumer(&trigger.definition))
        || structurally_selectable_alternate_spell_payload_has_cast_unstable_consumer(
            state, caster, object,
        )
        || cast_spell_face_choice_available(object)
            && object
                .back_face
                .as_ref()
                .is_some_and(back_face_has_cast_unstable_consumer)
}

fn structurally_selectable_alternate_spell_payload_has_cast_unstable_consumer(
    state: &GameState,
    caster: PlayerId,
    object: &engine::game::game_object::GameObject,
) -> bool {
    let mut has_unstable_consumer = false;
    for_each_structurally_selectable_alternate_spell_payload(state, caster, object.id, |payload| {
        match payload {
            StructurallySelectableAlternateSpellPayload::BackFace(back_face) => {
                has_unstable_consumer |= back_face_has_cast_unstable_consumer(back_face);
            }
            StructurallySelectableAlternateSpellPayload::FuseRightSpellAbility(ability) => {
                has_unstable_consumer |= ability_has_cast_unstable_consumer(ability);
            }
            StructurallySelectableAlternateSpellPayload::Cleave(cleave_variant) => {
                has_unstable_consumer |= cleave_variant_has_cast_unstable_consumer(cleave_variant);
            }
        }
    });
    has_unstable_consumer
}

fn back_face_has_cast_unstable_consumer(
    back_face: &engine::game::game_object::BackFaceData,
) -> bool {
    !back_face.parse_warnings.is_empty()
        || !back_face.casting_restrictions.is_empty()
        || back_face
            .casting_options
            .iter()
            .any(|option| !spell_casting_option_is_cast_stable_for_pre_cast(option))
        || back_face
            .modal
            .as_ref()
            .is_some_and(|modal| !modal_choice_is_cast_stable_for_pre_cast(modal))
        || back_face
            .additional_cost
            .as_ref()
            .is_some_and(|cost| !additional_cost_is_cast_stable_for_pre_cast(cost))
        || back_face
            .abilities
            .iter()
            .any(ability_has_cast_unstable_consumer)
        || back_face
            .static_definitions
            .as_slice()
            .iter()
            .any(|definition| !static_definition_is_cast_stable_for_pre_cast(definition))
        || !back_face.replacement_definitions.as_slice().is_empty()
        || back_face
            .trigger_definitions
            .as_slice()
            .iter()
            .any(trigger_definition_has_cast_unstable_consumer)
        || back_face
            .keywords
            .iter()
            .any(|keyword| keyword.kind() == KeywordKind::Unknown)
}

fn cleave_variant_has_cast_unstable_consumer(
    cleave_variant: &engine::types::card::CleaveVariant,
) -> bool {
    cleave_variant
        .abilities
        .iter()
        .any(ability_has_cast_unstable_consumer)
        || cleave_variant
            .static_abilities
            .iter()
            .any(|definition| !static_definition_is_cast_stable_for_pre_cast(definition))
        || !cleave_variant.replacements.is_empty()
        || cleave_variant
            .triggers
            .iter()
            .any(trigger_definition_has_cast_unstable_consumer)
}

/// A mana ability's declared variable is selected while that ability is paid;
/// it is not a read of the spell cast that this gate is evaluating. This exact
/// narrow case avoids treating a presently irrelevant storage-mana choice as a
/// future payoff, while all zone, hand, journal, snapshot, and condition reads
/// remain conservative consumers.
fn mana_ability_has_only_unbound_variable_quantities(definition: &AbilityDefinition) -> bool {
    ability_definition_has_only_unbound_variable_quantities_for_pre_cast(definition)
}

#[cfg(test)]
fn casting_restriction_has_cast_unstable_condition(
    _: &engine::types::ability::CastingRestriction,
) -> bool {
    true
}

fn trigger_definition_has_cast_unstable_consumer(
    definition: &engine::types::ability::TriggerDefinition,
) -> bool {
    !trigger_definition_is_cast_stable_for_pre_cast(definition)
}

/// Storm and Surge share `KeywordKind::Unknown` with other keyword variants.
/// The public effective-keyword authority therefore yields a conservative
/// superset here: an unrelated unknown kind may retain a cast, but no
/// off-zone Storm or Surge payoff can be rejected as known-zero.
fn object_has_cast_history_keyword(
    state: &GameState,
    object: &engine::game::game_object::GameObject,
) -> bool {
    engine::game::keywords::object_has_effective_keyword_kind(
        state,
        object.id,
        KeywordKind::Unknown,
    )
}

fn payment_population_is_stable(state: &GameState, caster: PlayerId, spell_id: ObjectId) -> bool {
    let Some(cost) = effective_spell_cost(state, caster, spell_id) else {
        return false;
    };
    if mana_cost_has_x(&cost)
        || engine::game::mana_payment::classify_payment(&cost)
            != engine::game::mana_payment::PaymentClassification::Unambiguous
    {
        return false;
    }
    if state.players[caster.0 as usize]
        .mana_pool
        .mana
        .iter()
        .any(|unit| !unit.grants.is_empty())
    {
        return false;
    }
    if spell_cost_is_payable_from_pool(state, caster, spell_id) {
        return true;
    }
    engine::game::mana_sources::activatable_mana_source_selections(state, caster)
        .iter()
        .all(|selection| {
            selection.penalty == ManaSourcePenalty::None
                && !mana_source_selection_has_spell_grant(state, selection)
                && selection.ability_index.is_none_or(|index| {
                    state
                        .objects
                        .get(&selection.source.object_id)
                        .and_then(|object| object.abilities.get(index))
                        .and_then(|ability| ability.cost.as_ref())
                        .is_none_or(tap_only_cost)
                })
        })
}

fn mana_source_selection_has_spell_grant(
    state: &GameState,
    selection: &engine::types::mana::ManaSourceSelection,
) -> bool {
    selection.ability_index.is_some_and(|index| {
        state
            .objects
            .get(&selection.source.object_id)
            .and_then(|object| object.abilities.get(index))
            .is_some_and(ability_has_mana_spell_grant)
    }) || selection.taps_for_mana.iter().any(|tap| {
        state
            .objects
            .get(&tap.source.object_id)
            .is_some_and(|source| {
                active_trigger_definitions(state, source).any(|active| {
                    active.definition_ref.source == tap.source
                        && active.definition_ref.occurrence == tap.occurrence
                        && active
                            .definition
                            .execute
                            .as_deref()
                            .is_some_and(ability_has_mana_spell_grant)
                })
            })
    })
}

fn ability_has_mana_spell_grant(ability: &AbilityDefinition) -> bool {
    let mut has_grant = false;
    let _ = visit_ability_def(ability, &mut |effect| {
        if matches!(effect, Effect::Mana { grants, .. } if !grants.is_empty()) {
            has_grant = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    has_grant
}

fn mana_cost_has_x(cost: &engine::types::mana::ManaCost) -> bool {
    matches!(
        cost,
        engine::types::mana::ManaCost::Cost { shards, .. }
            if shards.contains(&engine::types::mana::ManaCostShard::X)
    )
}

fn tap_only_cost(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Tap => true,
        AbilityCost::Composite { costs } => costs.iter().all(tap_only_cost),
        _ => false,
    }
}

fn definition_is_componentwise_known_zero(
    state: &GameState,
    definition: &AbilityDefinition,
    controller: PlayerId,
    source_id: ObjectId,
    root: bool,
    parent_target_bound: bool,
) -> bool {
    if definition.kind != AbilityKind::Spell
        || definition.cost.is_some()
        || definition.else_ability.is_some()
        || definition.duration.is_some()
        || !definition.activation_restrictions.is_empty()
        || definition.activation_mana_payment_restriction.is_some()
        || definition.activator_filter.is_some()
        || definition.activation_zone.is_some()
        || definition.ability_tag.is_some()
        || definition.condition.is_some()
        || definition.optional_targeting
        || definition.optional
        || definition.optional_player.is_some()
        || definition.optional_for.is_some()
        || definition.multi_target.is_some()
        || !definition.target_constraints.is_empty()
        || !engine::types::ability::TargetChoiceTiming::is_stack(&definition.target_choice_timing)
        || definition.distribute.is_some()
        || definition.unless_pay.is_some()
        || definition.modal.is_some()
        || !definition.mode_abilities.is_empty()
        || definition.repeat_for.is_some()
        || definition.announced_x.is_some()
        || definition.min_x_value != 0
        || definition.cant_be_copied
        || definition.cost_reduction.is_some()
        || definition.forward_result
        || definition.player_scope.is_some()
        || definition.starting_with.is_some()
        || !definition.target_selection_mode.is_chosen()
        || definition.target_chooser.is_some()
        || definition.repeat_until.is_some()
        || !engine::types::ability::SubAbilityLink::is_continuation(&definition.sub_link)
        || definition.iteration_kind_binding.is_some()
        || !engine::types::ability::SiblingCondition::is_default(&definition.sibling_condition)
    {
        return false;
    }

    let effect_is_zero = match definition.effect.as_ref() {
        Effect::Draw { count, target }
            if accepted_zero_recipient(target, root, parent_target_bound)
                && quantity_is_cast_stable_for_pre_cast(count) =>
        {
            try_resolve_quantity_in_source_context(state, count, controller, source_id) == Some(0)
        }
        Effect::GainLife { amount, player }
            if accepted_zero_recipient(player, root, parent_target_bound)
                && quantity_is_cast_stable_for_pre_cast(amount) =>
        {
            try_resolve_quantity_in_source_context(state, amount, controller, source_id) == Some(0)
        }
        Effect::LoseLife { amount, target }
            if target.as_ref().is_none_or(|recipient| {
                accepted_zero_recipient(recipient, root, parent_target_bound)
            }) && quantity_is_cast_stable_for_pre_cast(amount) =>
        {
            try_resolve_quantity_in_source_context(state, amount, controller, source_id) == Some(0)
        }
        Effect::Discard {
            count,
            target,
            filter: None,
            selection,
            unless_filter: None,
        } if accepted_zero_recipient(target, root, parent_target_bound)
            && selection.is_chosen()
            && quantity_is_cast_stable_for_pre_cast(count) =>
        {
            try_resolve_quantity_in_source_context(state, count, controller, source_id) == Some(0)
        }
        _ => false,
    };

    let parent_target_bound =
        parent_target_bound || (root && effect_binds_parent_target(definition.effect.as_ref()));
    effect_is_zero
        && definition.sub_ability.as_ref().is_none_or(|sub| {
            definition_is_componentwise_known_zero(
                state,
                sub,
                controller,
                source_id,
                false,
                parent_target_bound,
            )
        })
}

fn effect_binds_parent_target(effect: &Effect) -> bool {
    match effect {
        Effect::Draw { target, .. } => matches!(target, TargetFilter::Player),
        Effect::GainLife { player, .. } => matches!(player, TargetFilter::Player),
        Effect::LoseLife { target, .. } => matches!(target, Some(TargetFilter::Player)),
        Effect::Discard { target, .. } => matches!(target, TargetFilter::Player),
        _ => false,
    }
}

fn accepted_zero_recipient(
    recipient: &TargetFilter,
    root: bool,
    parent_target_bound: bool,
) -> bool {
    matches!(recipient, TargetFilter::Controller)
        || (root && matches!(recipient, TargetFilter::Player))
        || (!root && parent_target_bound && matches!(recipient, TargetFilter::ParentTarget))
}

fn has_relevant_functioning_trigger(state: &GameState, spell_id: ObjectId) -> bool {
    let Some(spell) = state.objects.get(&spell_id) else {
        return true;
    };
    if state.battlefield.iter().copied().any(|source_id| {
        synthetic_keyword_spell_cast_trigger_applies(state, source_id, spell.controller, spell_id)
    }) {
        return true;
    }

    let battlefield_relevant = battlefield_active_triggers(state)
        .any(|(_, active)| !trigger_is_proven_irrelevant(spell, active.definition));
    if battlefield_relevant {
        return true;
    }

    state.objects.values().any(|object| {
        matches!(
            object.zone,
            Zone::Hand | Zone::Graveyard | Zone::Exile | Zone::Stack | Zone::Command
        ) && spell_identity_is_available_to_caster(state, spell.controller, object)
            && active_trigger_definitions(state, object).any(|active| {
                trigger_definition_functions_in_zone(active.definition, object.zone)
                    && !trigger_is_proven_irrelevant(spell, active.definition)
            })
    })
}

/// This intentionally does not evaluate a trigger filter. `matches_target_filter`
/// can return false for a context-dependent predicate, which is not a proof that a
/// future cast event cannot satisfy it. Only a contradictory type requirement on the
/// exact candidate spell is sufficient to make a cast/target hook irrelevant.
fn trigger_is_proven_irrelevant(
    spell: &engine::game::game_object::GameObject,
    definition: &engine::types::ability::TriggerDefinition,
) -> bool {
    let disjoint_cast_hook = matches!(
        definition.mode,
        TriggerMode::SpellCast
            | TriggerMode::SpellCastOrCopy
            | TriggerMode::SpellAbilityCast
            | TriggerMode::PlayCard
    ) && definition
        .valid_card
        .as_ref()
        .is_some_and(|filter| target_filter_is_proven_disjoint_from_spell(filter, spell));
    let disjoint_target_hook = matches!(
        definition.mode,
        TriggerMode::BecomesTarget | TriggerMode::BecomesTargetOnce
    ) && definition
        .valid_source
        .as_ref()
        .is_some_and(|filter| target_filter_is_proven_disjoint_from_spell(filter, spell));
    disjoint_cast_hook
        || disjoint_target_hook
        || matches!(definition.mode, TriggerMode::Attacks)
        || (matches!(definition.mode, TriggerMode::ChangesZone)
            && definition.zone_change_clauses.is_empty()
            && definition.destination == Some(Zone::Battlefield)
            && definition.valid_card == Some(TargetFilter::SelfRef))
}

fn target_filter_is_proven_disjoint_from_spell(
    filter: &TargetFilter,
    spell: &engine::game::game_object::GameObject,
) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_is_proven_disjoint_from_spell(typed, spell),
        TargetFilter::And { filters } => filters
            .iter()
            .any(|filter| target_filter_is_proven_disjoint_from_spell(filter, spell)),
        TargetFilter::Or { filters } if !filters.is_empty() => filters
            .iter()
            .all(|filter| target_filter_is_proven_disjoint_from_spell(filter, spell)),
        _ => false,
    }
}

fn typed_filter_is_proven_disjoint_from_spell(
    filter: &TypedFilter,
    spell: &engine::game::game_object::GameObject,
) -> bool {
    filter
        .type_filters
        .iter()
        .any(|type_filter| type_filter_is_proven_disjoint_from_spell(type_filter, spell))
}

fn type_filter_is_proven_disjoint_from_spell(
    filter: &TypeFilter,
    spell: &engine::game::game_object::GameObject,
) -> bool {
    let lacks = |card_type| !spell.card_types.core_types.contains(&card_type);
    match filter {
        TypeFilter::Creature => lacks(CoreType::Creature),
        TypeFilter::Land => lacks(CoreType::Land),
        TypeFilter::Artifact => lacks(CoreType::Artifact),
        TypeFilter::Enchantment => lacks(CoreType::Enchantment),
        TypeFilter::Instant => lacks(CoreType::Instant),
        TypeFilter::Sorcery => lacks(CoreType::Sorcery),
        TypeFilter::Planeswalker => lacks(CoreType::Planeswalker),
        TypeFilter::Battle => lacks(CoreType::Battle),
        TypeFilter::Kindred => lacks(CoreType::Kindred),
        TypeFilter::Permanent => !spell.card_types.core_types.iter().any(|card_type| {
            matches!(
                card_type,
                CoreType::Artifact
                    | CoreType::Battle
                    | CoreType::Creature
                    | CoreType::Enchantment
                    | CoreType::Land
                    | CoreType::Planeswalker
            )
        }),
        TypeFilter::AnyOf(filters) if !filters.is_empty() => filters
            .iter()
            .all(|filter| type_filter_is_proven_disjoint_from_spell(filter, spell)),
        TypeFilter::Card
        | TypeFilter::Any
        | TypeFilter::Non(_)
        | TypeFilter::Subtype(_)
        | TypeFilter::AnyOf(_) => false,
    }
}

/// Hard-reject targets that are provably futile (e.g., destroy vs indestructible).
/// Called before `target_choice_penalty` so these never reach scoring.
fn reject_futile_target(ctx: &PolicyContext<'_>, target: &TargetRef) -> Option<GateDecision> {
    let TargetRef::Object(object_id) = target else {
        return None;
    };
    let object = ctx.state.objects.get(object_id)?;
    let effects = ctx.effects();

    // CR 701.8 + CR 702.12b: destroy-based removal can't destroy an
    // indestructible permanent.
    let is_destroy = effects.iter().any(|e| matches!(e, Effect::Destroy { .. }));
    if is_destroy && object.has_keyword(&Keyword::Indestructible) {
        return Some(GateDecision::Reject);
    }

    // CR 702.12b: an indestructible creature ignores the lethal-damage SBA
    // (CR 704.5g), so a damage-only spell can NEVER kill it regardless of the
    // amount — provably futile. Shrink effects are exempt: reducing toughness to
    // 0 kills via CR 704.5f (which indestructible does not prevent), and two
    // shrink spells can combine, so those stay in the judgment layer.
    if object.has_keyword(&Keyword::Indestructible)
        && deals_damage(&effects)
        && !has_toughness_shrink(&effects)
    {
        return Some(GateDecision::Reject);
    }

    // CR 702.21a: targeting a warded permanent triggers ward; if the AI can't
    // pay the cost, the spell is simply countered — a strict card-down. Never
    // choose such a target.
    for keyword in &object.keywords {
        if let Keyword::Ward(ward) = keyword {
            if !can_pay_ward_cost(ctx, ward, object) {
                return Some(GateDecision::Reject);
            }
            break;
        }
    }

    None
}

/// Whether any effect deals damage (fixed or variable).
fn deals_damage(effects: &[&Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::DealDamage { .. }))
}

/// Whether any effect reduces toughness via a negative `Pump` or a negative P/T
/// counter. Variable pump toughness is treated as possible shrink (conservative:
/// never hard-reject a line that might reduce toughness to 0).
fn has_toughness_shrink(effects: &[&Effect]) -> bool {
    effects.iter().any(|e| match e {
        Effect::Pump { toughness, .. } => match toughness {
            PtValue::Fixed(v) => *v < 0,
            PtValue::Variable(_) | PtValue::Quantity(_) => true,
        },
        Effect::PutCounter { counter_type, .. } => counter_type
            .power_toughness_delta()
            .is_some_and(|(_, t)| t < 0),
        _ => false,
    })
}

fn target_choice_penalty(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    let TargetRef::Object(object_id) = target else {
        return 0.0;
    };

    let effects = ctx.effects();

    // Pumping a tapped creature not participating in combat deals no combat benefit.
    // CR 508.1d / CR 509.1a: Both attackers and blockers can benefit from pump.
    let is_pump = effects.iter().any(|e| matches!(e, Effect::Pump { .. }));
    if is_pump {
        if let Some(object) = ctx.state.objects.get(object_id) {
            if object.tapped {
                let in_combat = ctx.state.combat.as_ref().is_some_and(|c| {
                    c.attackers.iter().any(|a| a.object_id == *object_id)
                        || c.blocker_to_attacker.contains_key(object_id)
                });
                if !in_combat {
                    return -8.0;
                }
            }
        }
    }

    let harmful = effects
        .iter()
        .any(|effect| matches!(effect_polarity(effect), EffectPolarity::Harmful));
    if harmful
        && has_pending_removal(ctx.state, *object_id)
        && will_target_die_from_stack(ctx.state, *object_id)
    {
        -10.0
    } else {
        0.0
    }
}

fn is_redundant_creature_only_removal(ctx: &PolicyContext<'_>, effects: &[&Effect]) -> bool {
    // The source supplies the targeting quality (color/type) the engine needs
    // to evaluate Protection / HexproofFrom; without it, fail open.
    let Some(source) = ctx.source_object() else {
        return false;
    };

    // A MIXED spell carrying a useful MASS wipe is never "redundant creature-only
    // removal": the wipe's NON-targeted population (CR 115.10a) is an independent
    // line that can clear creatures hexproof/protected FROM TARGETING (CR 702.11b)
    // — so a creature-only half with no live opponent TARGET must not suppress the
    // cast. Consult the resolver-mirroring mass seam (`ctx.has_opposing_mass_population`)
    // before declaring redundancy.
    if ctx.has_opposing_mass_population() {
        return false;
    }

    let mut saw_creature_only_harm = false;
    for effect in effects {
        if !(matches!(effect_polarity(effect), EffectPolarity::Harmful)
            && targets_creatures_only(effect))
        {
            continue;
        }
        saw_creature_only_harm = true;
        let Some(filter) = extract_target_filter(effect) else {
            // Can't analyze the filter — not provably redundant.
            return false;
        };
        // CR 702.11/702.16/702.18 + CR 608.2b: defer targeting legality to the
        // engine (Shroud, Hexproof-vs-opponents, "Hexproof from [quality]",
        // Protection, ignore-hexproof) instead of re-checking keywords here.
        let has_live_opponent_target =
            ctx.has_legal_opponent_creature_target(filter, source.id, |id| {
                // A target already dying to a stack effect is not a reason to
                // keep this redundant removal.
                !will_target_die_from_stack(ctx.state, id)
            });
        if has_live_opponent_target {
            return false;
        }
    }

    saw_creature_only_harm
}

fn pure_fixed_pump_bonus(effects: &[&Effect]) -> Option<(i32, i32)> {
    if effects.is_empty()
        || !effects
            .iter()
            .all(|effect| matches!(effect, Effect::Pump { .. }))
    {
        return None;
    }

    let mut power_bonus = 0;
    let mut toughness_bonus = 0;
    for effect in effects {
        let Effect::Pump {
            power, toughness, ..
        } = effect
        else {
            return None;
        };
        let PtValue::Fixed(power) = power else {
            return None;
        };
        let PtValue::Fixed(toughness) = toughness else {
            return None;
        };
        power_bonus += *power;
        toughness_bonus += *toughness;
    }
    Some((power_bonus, toughness_bonus))
}

/// The activated ability behind an in-class temporary-pump candidate, or
/// `None` when the candidate is not one. Returns the ability's source object
/// id (the permanent whose ability is being activated).
///
/// `ability_is_temporary_combat_modifier` reads the DEFINITION — `ctx.effects()`
/// flattens the chain and drops `duration`, so a permanent pump would be
/// indistinguishable from an "until end of turn" one there.
///
/// Every check here is card-local and runs BEFORE `TacticalFacts::derive`, so a
/// candidate outside the class costs one map lookup and a cost-category walk.
/// The exclusions:
/// - sacrifice / loyalty / discard costs: the activation's payoff is the cost
///   itself (a sacrifice outlet, a planeswalker's loyalty economy, a discard
///   enabler), priced by `free_outlet_activation` / `self_cost_value`, so the
///   pump window is the wrong lens.
/// - `AsSorcery`: there is no later, stronger window to save the ability for.
///
/// PayLife stays in class — Desolation Prowler is the reported card, and its
/// life cost is priced separately by the self-cost policies.
fn activated_ueot_pump_source(ctx: &PolicyContext<'_>) -> Option<ObjectId> {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = &ctx.candidate.action
    else {
        return None;
    };
    let ability = ctx
        .state
        .objects
        .get(source_id)?
        .abilities
        .get(*ability_index)?;
    if !ability_is_temporary_combat_modifier(ability) {
        return None;
    }
    let cost_is_its_own_payoff = ability.cost.as_ref().is_some_and(|cost| {
        cost.categories().iter().any(|category| {
            matches!(
                category,
                CostCategory::SacrificesPermanent
                    | CostCategory::PaysLoyalty
                    | CostCategory::Discards
            )
        })
    });
    if cost_is_its_own_payoff
        || ability
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery)
    {
        return None;
    }
    Some(*source_id)
}

/// Whether the pump lands on the ability's own source (`SelfRef`) and that
/// permanent is one of the AI's unblocked attackers — CR 509.1h: an attacking
/// creature no blocker was assigned to remains unblocked.
///
/// CR 510.3: once combat damage has been dealt the active player gets priority
/// again while still inside the combat phase, and `Phase::EndCombat` maps to
/// `CombatAfterBlocks` too. Extra power at that point buys nothing, so
/// `regular_damage_done` closes the stand-down.
fn self_pump_on_unblocked_attacker(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effects: &[&Effect],
) -> bool {
    if !effects.iter().all(|effect| {
        matches!(
            effect,
            Effect::Pump {
                target: TargetFilter::SelfRef,
                ..
            }
        )
    }) {
        return false;
    }
    if state
        .objects
        .get(&source_id)
        .is_none_or(|source| source.controller != ai_player)
    {
        return false;
    }
    let Some(combat) = &state.combat else {
        return false;
    };
    if combat.regular_damage_done {
        return false;
    }
    combat.attackers.iter().any(|attacker| {
        attacker.object_id == source_id
            && !attacker.blocked
            && combat
                .blocker_assignments
                .get(&source_id)
                .is_none_or(|blockers| blockers.is_empty())
    })
}

fn should_reject_pump_window(
    ctx: &PolicyContext<'_>,
    facts: &TacticalFacts,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    if facts.live_stack_response
        && pump_can_save_from_hostile_stack(ctx.state, ctx.ai_player, toughness_bonus)
    {
        return false;
    }

    match facts.window {
        TacticalWindow::OwnPostCombatMain
        | TacticalWindow::OpponentMain
        | TacticalWindow::EndStep => {
            return true;
        }
        TacticalWindow::OwnPreCombatMain | TacticalWindow::CombatBeforeBlocks => {
            return facts.pass_preserves_stronger_window;
        }
        TacticalWindow::Other => return true,
        TacticalWindow::CombatAfterBlocks
        | TacticalWindow::CombatDamage
        | TacticalWindow::StackResponse => {}
    }

    !pump_changes_combat_outcome(ctx.state, ctx.ai_player, power_bonus, toughness_bonus)
}

/// Check if pumping can actually save a creature from hostile stack effects.
/// Destroy/Exile/Counter/Bounce kill regardless of stats — pump doesn't help.
/// Only damage-based removal can be survived with a toughness boost.
fn pump_can_save_from_hostile_stack(
    state: &GameState,
    ai_player: PlayerId,
    toughness_bonus: i32,
) -> bool {
    use engine::types::ability::QuantityExpr;

    state.stack.iter().any(|entry| {
        let Some(ability) = entry.ability() else {
            return false;
        };
        ability.targets.iter().any(|target| {
            let TargetRef::Object(object_id) = target else {
                return false;
            };
            let Some(object) = state.objects.get(object_id) else {
                return false;
            };
            if object.controller != ai_player
                || !object.card_types.core_types.contains(&CoreType::Creature)
            {
                return false;
            }

            let effects = collect_ability_effects(ability);
            for effect in &effects {
                match effect {
                    // Destroy/Exile/Counter/Bounce — pump doesn't save
                    Effect::Destroy { .. } | Effect::Counter { .. } | Effect::Bounce { .. } => {
                        return false
                    }
                    Effect::ChangeZone { .. } => return false,
                    // Damage — pump saves if toughness + bonus > damage
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value },
                        ..
                    } => {
                        let toughness = object.toughness.unwrap_or(0);
                        let remaining = toughness - object.damage_marked as i32;
                        if remaining + toughness_bonus > *value {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            false
        })
    })
}

fn pump_changes_combat_outcome(
    state: &GameState,
    ai_player: PlayerId,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    let Some(combat) = &state.combat else {
        return false;
    };

    // CR 509.1: Aggregate AI's unblocked damage per defending player. A 4-player
    // pod where AI attacks player A for 5 and player B for 5 must NOT report
    // 10 unblocked damage when checking if pumping makes A's lethal threshold —
    // only attackers heading to A count toward A's life threat.
    let mut unblocked_per_defender: HashMap<PlayerId, i32> = HashMap::new();
    for attacker in &combat.attackers {
        let Some(attacker_obj) = state.objects.get(&attacker.object_id) else {
            continue;
        };
        if attacker_obj.controller != ai_player {
            continue;
        }
        let blocked = attacker.blocked
            || combat
                .blocker_assignments
                .get(&attacker.object_id)
                .is_some_and(|blockers| !blockers.is_empty());
        if !blocked {
            if let AttackTarget::Player(defending) = attacker.attack_target {
                *unblocked_per_defender.entry(defending).or_insert(0) +=
                    attacker_obj.power.unwrap_or(0);
            }
        }
    }

    for attacker in &combat.attackers {
        let Some(attacker_obj) = state.objects.get(&attacker.object_id) else {
            continue;
        };
        let blockers = combat
            .blocker_assignments
            .get(&attacker.object_id)
            .cloned()
            .unwrap_or_default();

        if attacker_obj.controller == ai_player {
            if blockers.is_empty() {
                let total_for_defender = match attacker.attack_target {
                    AttackTarget::Player(pid) => {
                        unblocked_per_defender.get(&pid).copied().unwrap_or(0)
                    }
                    _ => 0,
                };
                if unblocked_attack_becomes_lethal(state, attacker, total_for_defender, power_bonus)
                {
                    return true;
                }
                continue;
            }

            if blockers.len() == 1
                && combat_trade_improves(
                    state,
                    attacker.object_id,
                    blockers[0],
                    power_bonus,
                    toughness_bonus,
                )
            {
                return true;
            }
        } else {
            for blocker_id in
                combat
                    .blocker_to_attacker
                    .iter()
                    .filter_map(|(blocker_id, attacker_ids)| {
                        attacker_ids
                            .contains(&attacker.object_id)
                            .then_some(*blocker_id)
                    })
            {
                let Some(blocker_obj) = state.objects.get(&blocker_id) else {
                    continue;
                };
                if blocker_obj.controller == ai_player
                    && combat_trade_improves(
                        state,
                        blocker_id,
                        attacker.object_id,
                        power_bonus,
                        toughness_bonus,
                    )
                {
                    return true;
                }
            }
        }
    }

    false
}

fn combat_trade_improves(
    state: &GameState,
    my_creature_id: ObjectId,
    opposing_creature_id: ObjectId,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    let Some(my_creature) = state.objects.get(&my_creature_id) else {
        return false;
    };
    let Some(opposing_creature) = state.objects.get(&opposing_creature_id) else {
        return false;
    };

    let my_power = my_creature.power.unwrap_or(0);
    let my_toughness = my_creature.toughness.unwrap_or(0) - my_creature.damage_marked as i32;
    let opposing_power = opposing_creature.power.unwrap_or(0);
    let opposing_toughness =
        opposing_creature.toughness.unwrap_or(0) - opposing_creature.damage_marked as i32;

    let dies_without_pump = my_toughness <= opposing_power;
    let survives_with_pump = my_toughness + toughness_bonus > opposing_power;
    if dies_without_pump && survives_with_pump {
        return true;
    }

    let fails_to_kill_without_pump = my_power < opposing_toughness;
    let kills_with_pump = my_power + power_bonus >= opposing_toughness;
    fails_to_kill_without_pump && kills_with_pump
}

fn unblocked_attack_becomes_lethal(
    state: &GameState,
    attacker: &engine::game::combat::AttackerInfo,
    total_unblocked_damage: i32,
    power_bonus: i32,
) -> bool {
    let AttackTarget::Player(defending_player) = attacker.attack_target else {
        return false;
    };
    let life = state.players[defending_player.0 as usize].life;
    total_unblocked_damage < life && total_unblocked_damage + power_bonus >= life
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{create_config, AiDifficulty, Platform};
    use crate::determinize::determinize_opponents;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::combat::{AttackerInfo, CombatState};
    use engine::game::deck_loading::DeckEntry;
    use engine::game::game_object::BackFaceData;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::zones::create_object;
    use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AdditionalCost, BounceSelection, CardTypeSetSource,
        CastPermissionConstraint, CastingPermission, CastingRestriction, Comparator, CountScope,
        CounterCostSelection, DelayedTriggerCondition, Duration, EffectKind, FilterProp,
        ManaProduction, ModalChoice, MultiTargetSpec, ParsedCondition, PlayerFilter,
        PlayerRelation, PlayerScope, QuantityExpr, QuantityRef, ResolvedAbility,
        SpellCastingOption, StaticCondition, StaticDefinition, SubAbilityLink, TargetFilter,
        TurnJournalKind, REMOVE_COUNTER_COST_X,
    };
    use engine::types::ability::{
        QuantityModification, ReplacementDefinition, ReplacementPlayerScope,
    };
    use engine::types::card::{CardFace, CleaveVariant, LayoutKind};
    use engine::types::counter::{CounterMatch, CounterType};
    use engine::types::game_state::{
        CastingVariant, DelayedTrigger, NextSpellModifier, PendingCast, PendingNextSpellModifier,
        PlayerDeckPool, StackEntry, StackEntryKind, TargetEffectDetail, TargetSelectionProgress,
        TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::CardId;
    use engine::types::keywords::{Keyword, WardCost};
    use engine::types::mana::{
        ManaCost, ManaCostShard, ManaSourceOutput, ManaSpellGrant, ManaUnit,
    };
    use engine::types::replacements::ReplacementEvent;
    use engine::types::statics::CastFrequency;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;
    use std::sync::Arc;

    const CONGREGATE_ORACLE: &str =
        "Target player gains 2 life for each creature on the battlefield.";

    fn pooled_mana(color: ManaType, count: usize) -> Vec<ManaUnit> {
        vec![ManaUnit::new(color, ObjectId(0), false, vec![]); count]
    }

    fn funded_zero_congregate_state() -> (GameState, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let congregate = scenario
            .add_spell_to_hand_from_oracle(P0, "Congregate", true, CONGREGATE_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::White],
            })
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 3));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::White, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        (state.clone(), congregate)
    }

    fn funded_zero_harvest_state() -> (GameState, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        (state.clone(), harvest)
    }

    fn harvest_amount(state: &GameState, harvest: ObjectId) -> Option<i32> {
        let object = state.objects.get(&harvest)?;
        let Effect::GainLife { amount, .. } = object.abilities.first()?.effect.as_ref() else {
            return None;
        };
        try_resolve_quantity_in_source_context(state, amount, object.controller, harvest)
    }

    fn spells_cast_this_turn() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: None,
            },
        }
    }

    fn cast_history_activation(zone: Zone) -> AbilityDefinition {
        let mut definition = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: spells_cast_this_turn(),
                target: TargetFilter::Controller,
            },
        );
        definition.activation_zone = Some(zone);
        definition
            .activation_restrictions
            .push(ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: spells_cast_this_turn(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }),
            });
        definition
    }

    fn issued_activation(state: &GameState, source_id: ObjectId, ability_index: usize) -> bool {
        engine::ai_support::candidate_actions(state)
            .iter()
            .any(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::ActivateAbility {
                        source_id: candidate_source,
                        ability_index: candidate_index,
                    } if candidate_source == source_id && candidate_index == ability_index
                )
            })
    }

    fn zero_cast_is_retained(state: &GameState, spell: ObjectId) -> bool {
        let issued = engine::ai_support::candidate_actions(state);
        assert!(issued.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::CastSpell {
                    object_id,
                    payment_mode: CastPaymentMode::Auto,
                    ..
                } if object_id == spell
            )
        }));
        cast_is_retained_from_issued(state, spell, issued)
    }

    fn cast_is_retained_from_issued(
        state: &GameState,
        spell: ObjectId,
        issued: Vec<CandidateAction>,
    ) -> bool {
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: issued.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        gate_candidates(
            state,
            &decision,
            issued,
            P0,
            &config,
            &AiContext::empty(&config.weights),
        )
        .iter()
        .any(|candidate| {
            matches!(
                candidate.candidate.action,
                GameAction::CastSpell { object_id, .. } if object_id == spell
            )
        })
    }

    fn zero_cast_with_payment_mode_is_retained(
        state: &GameState,
        spell: ObjectId,
        payment_mode: CastPaymentMode,
    ) -> bool {
        let mut issued = engine::ai_support::candidate_actions(state);
        let cast = issued
            .iter_mut()
            .find(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::CastSpell { object_id, .. } if object_id == spell
                )
            })
            .expect("production cast candidate exists");
        let GameAction::CastSpell {
            payment_mode: candidate_mode,
            ..
        } = &mut cast.action
        else {
            unreachable!("selected a cast candidate");
        };
        *candidate_mode = payment_mode;
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: issued.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        gate_candidates(
            state,
            &decision,
            issued,
            P0,
            &config,
            &AiContext::empty(&config.weights),
        )
        .iter()
        .any(|candidate| {
            matches!(
                candidate.candidate.action,
                GameAction::CastSpell { object_id, .. } if object_id == spell
            )
        })
    }

    fn add_battlefield_trigger(state: &mut GameState, card_id: u64, mode: TriggerMode) -> ObjectId {
        let source = create_object(
            state,
            CardId(card_id),
            P0,
            format!("{mode:?} hook"),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .expect("hook source exists")
            .trigger_definitions
            .push(engine::types::ability::TriggerDefinition::new(mode));
        source
    }

    fn add_zone_trigger(
        state: &mut GameState,
        card_id: u64,
        name: &str,
        zone: Zone,
        definition: engine::types::ability::TriggerDefinition,
    ) -> ObjectId {
        let source = create_object(state, CardId(card_id), P0, name.to_string(), zone);
        state
            .objects
            .get_mut(&source)
            .expect("trigger source exists")
            .trigger_definitions
            .push(definition);
        source
    }

    fn add_battlefield_static(state: &mut GameState, card_id: u64, mode: StaticMode) -> ObjectId {
        add_battlefield_static_for_controller(state, card_id, P0, StaticDefinition::new(mode))
    }

    fn add_battlefield_static_for_controller(
        state: &mut GameState,
        card_id: u64,
        controller: PlayerId,
        definition: StaticDefinition,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(card_id),
            controller,
            format!("{:?} producer", definition.mode),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .expect("static producer exists")
            .static_definitions
            .push(definition);
        source
    }

    fn add_tapped_slagheap_mana_source(state: &mut GameState) -> ObjectId {
        let storage = CounterType::Generic("storage".to_string());
        let slagheap = create_object(
            state,
            CardId(91_401),
            P0,
            "Molten Slagheap".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&slagheap)
            .expect("Slagheap source exists");
        object.card_types.core_types.push(CoreType::Land);
        object.tapped = true;
        object.counters.insert(storage.clone(), 7);
        let abilities = Arc::make_mut(&mut object.abilities);
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::PutCounter {
                    counter_type: storage.clone(),
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    AbilityCost::Tap,
                ],
            }),
        );
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyCombination {
                        count: QuantityExpr::Ref {
                            qty: QuantityRef::Variable {
                                name: "X".to_string(),
                            },
                        },
                        color_options: vec![
                            engine::types::mana::ManaColor::Black,
                            engine::types::mana::ManaColor::Red,
                        ],
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    AbilityCost::RemoveCounter {
                        target: None,
                        count: REMOVE_COUNTER_COST_X,
                        counter_type: CounterMatch::OfType(storage),
                        selection: CounterCostSelection::SingleObject,
                    },
                ],
            }),
        );
        slagheap
    }

    fn add_plain_mana_source(
        state: &mut GameState,
        card_id: u64,
        color: engine::types::mana::ManaColor,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(card_id),
            P0,
            format!("{color:?} source"),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&source)
            .expect("plain mana source exists");
        object.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut object.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![color],
                        contribution: engine::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        source
    }

    fn add_plain_colorless_mana_source(state: &mut GameState, card_id: u64) -> ObjectId {
        let source = create_object(
            state,
            CardId(card_id),
            P0,
            "Colorless source".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&source)
            .expect("plain colorless mana source exists");
        object.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut object.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        source
    }

    fn add_grant_mana_source(state: &mut GameState, card_id: u64) -> ObjectId {
        let source = add_plain_colorless_mana_source(state, card_id);
        let ability = Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("grant source exists")
                .abilities,
        )
        .first_mut()
        .expect("grant source has one mana ability");
        let Effect::Mana { grants, .. } = &mut *ability.effect else {
            unreachable!("fixture mana source has a mana effect");
        };
        grants.push(ManaSpellGrant::TriggerOnSpend {
            filter: TargetFilter::Any,
            ability: Box::new(zero_gain_definition()),
        });
        source
    }

    fn add_otherwise_only_grant_mana_source(state: &mut GameState, card_id: u64) -> ObjectId {
        let source = add_plain_colorless_mana_source(state, card_id);
        let ability = Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("otherwise grant source exists")
                .abilities,
        )
        .first_mut()
        .expect("otherwise grant source has one mana ability");
        ability.sub_ability = Some(Box::new(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .condition(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::IsMonarch),
            })
            .with_else_ability(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![ManaSpellGrant::TriggerOnSpend {
                        filter: TargetFilter::Any,
                        ability: Box::new(zero_gain_definition()),
                    }],
                    expiry: None,
                    target: None,
                },
            )),
        ));
        source
    }

    fn add_sacrificial_mana_source(state: &mut GameState, card_id: u64) -> ObjectId {
        let source = create_object(
            state,
            CardId(card_id),
            P0,
            "Sacrificial mana source".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&source)
            .expect("sacrificial mana source exists");
        object.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut object.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(
                engine::types::ability::SacrificeCost::count(TargetFilter::SelfRef, 1),
            )),
        );
        source
    }

    #[test]
    fn zero_congregate_is_gated_but_positive_control_reaches_scoring() {
        let mut zero_scenario = GameScenario::new();
        zero_scenario.at_phase(Phase::PreCombatMain);
        let congregate = zero_scenario
            .add_spell_to_hand_from_oracle(P0, "Congregate", true, CONGREGATE_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::White],
            })
            .id();
        zero_scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 3));
        zero_scenario.with_mana_pool(P0, pooled_mana(ManaType::White, 1));
        let mut zero_runner = zero_scenario.build();
        let zero_state = zero_runner.state_mut();
        zero_state.active_player = P0;
        zero_state.priority_player = P0;
        zero_state.waiting_for = WaitingFor::Priority { player: P0 };
        let issued = engine::ai_support::candidate_actions(zero_state);
        assert!(issued.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::CastSpell {
                    object_id,
                    payment_mode: CastPaymentMode::Auto,
                    ..
                } if object_id == congregate
            )
        }));
        let decision = AiDecisionContext {
            waiting_for: zero_state.waiting_for.clone(),
            candidates: issued.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let gated = gate_candidates(
            zero_state,
            &decision,
            issued,
            P0,
            &config,
            &AiContext::empty(&config.weights),
        );
        assert!(
            gated.iter().all(|candidate| !matches!(
                candidate.candidate.action,
                GameAction::CastSpell { object_id, .. } if object_id == congregate
            )),
            "known-zero, pool-funded Congregate must be rejected before scoring"
        );
        assert!(
            gated
                .iter()
                .any(|candidate| matches!(candidate.candidate.action, GameAction::PassPriority)),
            "the gate removes only the no-op cast from the engine-issued root domain"
        );
        assert!(
            crate::search::score_candidates(zero_state, P0, &config)
                .iter()
                .all(|(action, _)| !matches!(
                    action,
                    GameAction::CastSpell { object_id, .. } if *object_id == congregate
                )),
            "the scored path must not reintroduce a root rejected by the tactical gate"
        );
        for search_enabled in [false, true] {
            let mut chooser_config = config.clone();
            chooser_config.search.enabled = search_enabled;
            let mut rng = ChaCha20Rng::seed_from_u64(7);
            assert!(
                !matches!(
                    crate::search::choose_action(zero_state, P0, &chooser_config, &mut rng),
                    Some(GameAction::CastSpell { object_id, .. }) if object_id == congregate
                ),
                "the {search_enabled:?} chooser route must not sample a gated zero cast"
            );
        }

        let mut positive_scenario = GameScenario::new();
        positive_scenario.at_phase(Phase::PreCombatMain);
        positive_scenario.add_creature(P0, "Witness", 1, 1);
        let positive_congregate = positive_scenario
            .add_spell_to_hand_from_oracle(P0, "Congregate", true, CONGREGATE_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::White],
            })
            .id();
        positive_scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 3));
        positive_scenario.with_mana_pool(P0, pooled_mana(ManaType::White, 1));
        let mut positive_runner = positive_scenario.build();
        let positive_state = positive_runner.state_mut();
        positive_state.active_player = P0;
        positive_state.priority_player = P0;
        positive_state.waiting_for = WaitingFor::Priority { player: P0 };
        let positive_issued = engine::ai_support::candidate_actions(positive_state);
        assert!(positive_issued.iter().any(|candidate| {
            matches!(
                candidate.action,
                GameAction::CastSpell {
                    object_id,
                    payment_mode: CastPaymentMode::Auto,
                    ..
                } if object_id == positive_congregate
            )
        }));
        let positive_decision = AiDecisionContext {
            waiting_for: positive_state.waiting_for.clone(),
            candidates: positive_issued.clone(),
        };
        assert_eq!(
            gate_candidates(
                positive_state,
                &positive_decision,
                positive_issued,
                P0,
                &config,
                &AiContext::empty(&config.weights),
            )
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate.candidate.action,
                    GameAction::CastSpell {
                        object_id,
                        payment_mode: CastPaymentMode::Auto,
                        ..
                    } if object_id == positive_congregate
                )
            })
            .count(),
            1,
            "a nonzero Congregate control must remain available"
        );
        assert!(
            crate::search::score_candidates(positive_state, P0, &config)
                .iter()
                .any(|(action, _)| matches!(
                    action,
                    GameAction::CastSpell { object_id, .. } if *object_id == positive_congregate
                )),
            "the nonzero control must reach the production scoring path"
        );

        let outcome = positive_runner
            .cast(positive_congregate)
            .target_player(P0)
            .resolve();
        outcome.assert_life_delta(P0, 2);
    }

    #[test]
    fn zero_cast_day_night_thresholds_are_paired() {
        for (day_night, prior_casts) in [(DayNight::Day, 0), (DayNight::Night, 1)] {
            let (mut state, congregate) = funded_zero_congregate_state();
            state.day_night = Some(day_night);
            if prior_casts > 0 {
                state
                    .spells_cast_this_turn_by_player
                    .insert(P0, engine::im::Vector::from(vec![Default::default()]));
            }
            assert_eq!(
                state
                    .spells_cast_this_turn_by_player
                    .get(&P0)
                    .map_or(0, |spells| spells.len()),
                prior_casts,
                "the fixture reaches the {day_night:?} cast-history threshold"
            );
            assert!(
                zero_cast_is_retained(&state, congregate),
                "a zero cast that changes the {day_night:?} transition must remain available"
            );

            state.day_night = None;
            assert!(
                !zero_cast_is_retained(&state, congregate),
                "removing only the day/night threshold restores known-zero rejection"
            );
        }
    }

    #[test]
    fn shared_team_day_night_boundaries_fail_open_for_nonactive_teammates() {
        for (day_night, prior_casts) in [(DayNight::Day, 0), (DayNight::Night, 1)] {
            let mut state = GameState::new(
                engine::types::format::FormatConfig::two_headed_giant(),
                4,
                91_215,
            );
            state.active_player = P0;
            state.day_night = Some(day_night);
            if prior_casts > 0 {
                state.spells_cast_this_turn_by_player.insert(
                    PlayerId(1),
                    engine::im::Vector::from(vec![Default::default()]),
                );
            }

            assert!(
                cast_has_relevant_payoff(&state, PlayerId(1), None),
                "a nonactive teammate's {day_night:?} boundary is retained until the transition authority is team-aware"
            );

            state.day_night = None;
            assert!(
                !cast_has_relevant_payoff(&state, PlayerId(1), None),
                "removing only the shared-team Day/Night boundary restores the no-payoff result"
            );
        }
    }

    #[test]
    fn zero_cast_surge_payoff_is_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let surge = create_object(
            &mut state,
            CardId(91_700),
            P0,
            "Surge payoff".to_string(),
            Zone::Hand,
        );
        let surge_object = state
            .objects
            .get_mut(&surge)
            .expect("Surge payoff exists in hand");
        surge_object.card_types.core_types.push(CoreType::Instant);
        surge_object
            .keywords
            .push(Keyword::Surge(ManaCost::generic(1)));
        surge_object
            .base_keywords
            .push(Keyword::Surge(ManaCost::generic(1)));

        assert!(
            state
                .spells_cast_this_turn_by_player
                .get(&P0)
                .is_none_or(|spells| spells.is_empty()),
            "the Surge payoff is not already enabled by a prior cast"
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a zero cast that enables the held Surge spell must remain available"
        );

        state.players[P0.0 as usize].hand.retain(|id| *id != surge);
        state.objects.remove(&surge);
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the Surge payoff restores known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_second_spell_condition_payoff_is_paired_and_changes_runtime_cost() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let uthros = scenario
            .add_creature_from_oracle(
                P0,
                "Uthros Psionicist",
                1,
                1,
                "The second spell you cast each turn costs {2} less to cast.",
            )
            .id();
        let bountiful_harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let second_spell = scenario
            .add_creature_to_hand(P0, "Second-spell witness", 1, 1)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 6));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        assert_eq!(
            effective_spell_cost(state, P0, second_spell),
            Some(ManaCost::generic(2)),
            "the condition is false before the first spell is cast"
        );
        assert!(
            zero_cast_is_retained(state, bountiful_harvest),
            "the supported second-spell condition must retain the first zero cast"
        );

        let mut without_payoff = state.clone();
        without_payoff.objects.remove(&uthros);
        without_payoff
            .battlefield
            .retain(|object_id| *object_id != uthros);
        assert!(
            !zero_cast_is_retained(&without_payoff, bountiful_harvest),
            "removing only the Uthros condition payoff restores known-zero rejection"
        );

        runner.cast(bountiful_harvest).resolve();
        assert_eq!(
            effective_spell_cost(runner.state(), P0, second_spell),
            Some(ManaCost::zero()),
            "the production cost authority activates the second-spell reduction"
        );
        let mana_before_second_spell = runner.state().players[P0.0 as usize].mana_pool.total();
        runner.cast(second_spell).resolve();
        assert_eq!(
            runner.state().players[P0.0 as usize].mana_pool.total(),
            mana_before_second_spell,
            "the condition-based reduction changes the actual second-spell payment"
        );
    }

    #[test]
    fn zero_cast_dynamic_object_option_and_permission_consumers_are_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let history = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: None,
            },
        };
        let option_spell = create_object(
            &mut state,
            CardId(91_860),
            P0,
            "Option consumer".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(91_864),
            P0,
            "Option draw witness".to_string(),
            Zone::Library,
        );
        {
            let option_object = state
                .objects
                .get_mut(&option_spell)
                .expect("option consumer exists");
            option_object.card_types.core_types.push(CoreType::Instant);
            option_object.mana_cost = ManaCost::generic(1);
            option_object.abilities = Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);
            option_object
                .casting_options
                .push(SpellCastingOption::free_cast().condition(
                    ParsedCondition::QuantityComparison {
                        lhs: history.clone(),
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    },
                ));
        }

        assert!(
            zero_cast_is_retained(&state, congregate),
            "a held free-cast option whose spell-history condition changes after the cast retains the zero spell"
        );
        let option_cast = engine::ai_support::candidate_actions(&state)
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == option_spell)
            })
            .expect("the production cast list offers the held spell's normal-cost path")
            .action;
        let mut pre_option_state = state.clone();
        pre_option_state.players[P0.0 as usize].mana_pool.clear();
        let pre_option_boundary = pre_option_state.clone();
        let rejected_option =
            engine::game::engine::apply_as_current(&mut pre_option_state, option_cast);
        assert!(
            matches!(
                rejected_option,
                Err(engine::game::engine::EngineError::ActionNotAllowed(_))
            ),
            "before cast history satisfies the condition, the reducer rejects the unfunded normal-cost fallback"
        );
        assert_eq!(
            pre_option_state.players[P0.0 as usize].hand,
            pre_option_boundary.players[P0.0 as usize].hand,
            "the rejected pre-history option cast rolls the held spell back"
        );
        assert_eq!(
            pre_option_state.stack, pre_option_boundary.stack,
            "the rejected pre-history option cast does not commit a stack entry"
        );
        assert_eq!(
            pre_option_state.pending_cast, pre_option_boundary.pending_cast,
            "the rejected pre-history option cast restores its pending boundary"
        );
        assert_eq!(
            pre_option_state.waiting_for, pre_option_boundary.waiting_for,
            "the rejected pre-history option cast restores priority"
        );
        let mut option_runner = GameRunner::from_state(state.clone());
        option_runner.cast(congregate).target_player(P0).resolve();
        option_runner
            .cast(option_spell)
            .accept_optional()
            .resolve()
            .assert_hand_drawn(P0, 1);
        state
            .objects
            .get_mut(&option_spell)
            .expect("option consumer remains")
            .casting_options
            .clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the dynamic option restores known-zero rejection"
        );

        let permission_spell = create_object(
            &mut state,
            CardId(91_861),
            P0,
            "Permission consumer".to_string(),
            Zone::Exile,
        );
        {
            let permission_object = state
                .objects
                .get_mut(&permission_spell)
                .expect("permission consumer exists");
            permission_object
                .card_types
                .core_types
                .push(CoreType::Instant);
            permission_object.mana_cost = ManaCost::generic(1);
            permission_object.abilities = Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);
            permission_object.casting_permissions.push(
                CastingPermission::ExileWithAltAbilityCost {
                    cost: AbilityCost::PayLife {
                        amount: history.clone(),
                    },
                    constraint: Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LE,
                        value: history,
                    }),
                    granted_to: Some(P0),
                    duration: None,
                    source_id: None,
                },
            );
        }

        assert!(
            zero_cast_is_retained(&state, congregate),
            "the grantee-authorized exile permission's dynamic cost and constraint retain the zero spell even before its constraint admits the cast"
        );
        let permission_cast = engine::ai_support::candidate_actions(&state)
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == permission_spell)
            })
            .expect("the deferred dynamic permission reaches the production cast reducer")
            .action;
        let mut pre_permission_state = state.clone();
        let pre_permission_boundary = pre_permission_state.clone();
        let rejected_permission =
            engine::game::engine::apply_as_current(&mut pre_permission_state, permission_cast);
        assert!(
            matches!(
                rejected_permission,
                Err(engine::game::engine::EngineError::ActionNotAllowed(_))
            ),
            "the production reducer rejects the deferred mana-value constraint before cast history changes"
        );
        assert_eq!(
            pre_permission_state.exile, pre_permission_boundary.exile,
            "the rejected permission cast preserves the exiled card"
        );
        assert_eq!(
            pre_permission_state.stack, pre_permission_boundary.stack,
            "the rejected permission cast does not commit a stack entry"
        );
        assert_eq!(
            pre_permission_state.pending_cast, pre_permission_boundary.pending_cast,
            "the rejected permission cast restores its pending boundary"
        );
        assert_eq!(
            pre_permission_state.waiting_for, pre_permission_boundary.waiting_for,
            "the rejected permission cast restores priority"
        );
        let mut permission_runner = GameRunner::from_state(state.clone());
        permission_runner
            .cast(congregate)
            .target_player(P0)
            .resolve();
        assert!(
            spell_objects_available_to_cast(permission_runner.state(), P0).contains(&permission_spell),
            "the production permission authority admits the exile spell after the cast-history threshold"
        );
        permission_runner
            .cast(permission_spell)
            .resolve()
            .assert_life_delta(P0, -1);
        state
            .objects
            .get_mut(&permission_spell)
            .expect("permission consumer remains")
            .casting_permissions
            .clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the dynamic permission restores known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_graveyard_activated_payoff_is_noncastable_and_uses_the_production_activation() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let source = create_object(
            &mut state,
            CardId(91_865),
            P0,
            "Graveyard activation payoff".to_string(),
            Zone::Graveyard,
        );
        create_object(
            &mut state,
            CardId(91_866),
            P0,
            "Graveyard payoff draw".to_string(),
            Zone::Library,
        );
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("graveyard source exists")
                .abilities,
        )
        .push(cast_history_activation(Zone::Graveyard));

        assert!(
            !spell_objects_available_to_cast(&state, P0).contains(&source),
            "the graveyard source has no casting permission"
        );
        assert!(
            !issued_activation(&state, source, 0),
            "the production activation authority rejects the false spell-count restriction"
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "the structurally available graveyard ability makes the zero cast fail open"
        );

        let mut without_payoff = state.clone();
        Arc::make_mut(
            &mut without_payoff
                .objects
                .get_mut(&source)
                .expect("graveyard source exists")
                .abilities,
        )
        .clear();
        assert!(
            !zero_cast_is_retained(&without_payoff, congregate),
            "removing only the nonfunctioning graveyard ability restores rejection"
        );

        let mut wrong_zone = state.clone();
        Arc::make_mut(
            &mut wrong_zone
                .objects
                .get_mut(&source)
                .expect("graveyard source exists")
                .abilities,
        )[0]
        .activation_zone = Some(Zone::Exile);
        assert!(
            !zero_cast_is_retained(&wrong_zone, congregate),
            "changing only the activation zone makes the retained graveyard metadata nonfunctioning"
        );

        let mut runner = GameRunner::from_state(state);
        runner.cast(congregate).target_player(P0).resolve();
        assert!(
            issued_activation(runner.state(), source, 0),
            "the engine issues the graveyard activation after recording the setup cast"
        );
        runner
            .activate(source, 0)
            .resolve()
            .assert_hand_drawn(P0, 1);
    }

    #[test]
    fn zero_cast_command_emblem_activation_is_discovered_through_runtime_authority() {
        let (mut state, congregate) = funded_zero_congregate_state();
        state.format_config.command_zone = true;
        create_object(
            &mut state,
            CardId(91_867),
            P0,
            "Command emblem payoff draw".to_string(),
            Zone::Library,
        );
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![cast_history_activation(Zone::Command)],
        );

        assert!(
            !issued_activation(&state, emblem, 0),
            "the command emblem's false restriction blocks activation before the cast"
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a command-zone emblem installed through grant_emblem is a payoff consumer"
        );

        let mut without_payoff = state.clone();
        Arc::make_mut(
            &mut without_payoff
                .objects
                .get_mut(&emblem)
                .expect("emblem exists")
                .abilities,
        )
        .clear();
        assert!(
            !zero_cast_is_retained(&without_payoff, congregate),
            "removing only the command activation restores rejection"
        );

        let mut runner = GameRunner::from_state(state);
        runner.cast(congregate).target_player(P0).resolve();
        assert!(
            issued_activation(runner.state(), emblem, 0),
            "the engine issues the command-zone activation after the setup cast"
        );
        runner
            .activate(emblem, 0)
            .resolve()
            .assert_hand_drawn(P0, 1);
    }

    #[test]
    fn zero_cast_hand_activation_is_discovered_while_its_spell_is_listed_but_not_castable() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let source = create_object(
            &mut state,
            CardId(91_868),
            P0,
            "Casting-blocked hand activation payoff".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(91_869),
            P0,
            "Hand payoff draw".to_string(),
            Zone::Library,
        );
        {
            let object = state.objects.get_mut(&source).expect("hand source exists");
            object.card_types.core_types.push(CoreType::Instant);
            object.abilities = Arc::new(vec![
                AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp),
                cast_history_activation(Zone::Hand),
            ]);
            object
                .casting_restrictions
                .push(CastingRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::QuantityComparison {
                        lhs: spells_cast_this_turn(),
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    }),
                });
        }

        // Preservation control: spell availability includes cards in hand;
        // `can_cast_object_now` separately applies the casting restriction.
        assert!(
            spell_objects_available_to_cast(&state, P0).contains(&source),
            "the hand source remains in spell availability despite its casting restriction"
        );
        assert!(
            !engine::game::casting::can_cast_object_now(&state, P0, source),
            "the sibling spell is blocked by its casting restriction"
        );
        assert!(
            !issued_activation(&state, source, 1),
            "the activation restriction is false before the setup cast"
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "the noncastable hand source's activated payoff retains the zero cast"
        );

        let mut without_payoff = state.clone();
        without_payoff.objects.remove(&source);
        without_payoff.players[P0.0 as usize]
            .hand
            .retain(|object_id| *object_id != source);
        assert!(
            !zero_cast_is_retained(&without_payoff, congregate),
            "removing only the casting-blocked hand source restores rejection"
        );

        let mut runner = GameRunner::from_state(state);
        runner.cast(congregate).target_player(P0).resolve();
        assert!(
            issued_activation(runner.state(), source, 1),
            "the engine issues the now-legal hand activation"
        );
        runner
            .activate(source, 1)
            .resolve()
            .assert_hand_drawn(P0, 1);
    }

    #[test]
    fn off_zone_activated_payoff_respects_identity_and_owner_controls() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let opponent_source = create_object(
            &mut state,
            CardId(91_870),
            P1,
            "Opponent graveyard activation".to_string(),
            Zone::Graveyard,
        );
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&opponent_source)
                .expect("opponent source exists")
                .abilities,
        )
        .push(cast_history_activation(Zone::Graveyard));
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "a visible opponent-owned source is not a payoff P0 may activate"
        );

        assert!(
            !state.viewer_knows_card_identity(P0, opponent_source),
            "the visible-zone fixture must not seed P0 with remembered opponent identity"
        );
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&opponent_source)
                .expect("opponent source exists")
                .abilities,
        )[0]
        .activator_filter = Some(PlayerFilter::All);
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a visible opponent source explicitly activatable by P0 is a payoff consumer"
        );

        state
            .objects
            .get_mut(&opponent_source)
            .expect("opponent source exists")
            .face_down = true;
        assert!(
            !spell_identity_is_available_to_caster(&state, P0, &state.objects[&opponent_source]),
            "the viewer cannot inspect the hidden opponent source's identity"
        );
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "hidden opponent activated metadata cannot affect P0's hard rejection"
        );
    }

    #[test]
    fn zero_cast_fails_open_for_a_production_sacrificed_activation_on_the_stack() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let source = create_object(
            &mut state,
            CardId(91_871),
            P0,
            "Sacrificial draw activation".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(91_872),
            P0,
            "Stack payoff draw".to_string(),
            Zone::Library,
        );
        let mut draw = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: spells_cast_this_turn(),
                target: TargetFilter::Controller,
            },
        );
        draw.cost = Some(AbilityCost::Sacrifice(
            engine::types::ability::SacrificeCost::count(TargetFilter::SelfRef, 1),
        ));
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("sacrificial source exists")
                .abilities,
        )
        .push(draw);

        assert!(issued_activation(&state, source, 0));
        let mut runner = GameRunner::from_state(state);
        let activation = engine::ai_support::candidate_actions(runner.state())
            .into_iter()
            .find(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::ActivateAbility {
                        source_id,
                        ability_index: 0,
                    } if source_id == source
                )
            })
            .expect("the engine issues the sacrificial activation")
            .action;
        runner
            .act(activation)
            .expect("production activation succeeds");
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert!(!runner.state().stack.is_empty());
        assert!(
            zero_cast_is_retained(runner.state(), congregate),
            "an activated ability already on the stack may read the later cast ledger"
        );

        let mut empty_stack = runner.state().clone();
        empty_stack.stack.clear();
        assert!(
            !zero_cast_is_retained(&empty_stack, congregate),
            "without the source or its announced stack ability, rejection is restored"
        );

        runner
            .cast(congregate)
            .target_player(P0)
            .resolve()
            .assert_hand_drawn(P0, 1);
    }

    #[test]
    fn zero_front_modal_spell_face_is_retained_and_back_face_resolves() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mdfc = scenario
            .add_spell_to_hand_from_oracle(P0, "Typed modal witness", true, "You gain 0 life.")
            .with_mana_cost(ManaCost::zero())
            .id();
        scenario.with_library_top(P0, &["drawn back face card"]);
        let mut runner = scenario.build();
        let object = runner
            .state_mut()
            .objects
            .get_mut(&mdfc)
            .expect("modal witness exists");
        let mut back_face = BackFaceData {
            name: "Typed modal draw face".to_string(),
            layout_kind: Some(LayoutKind::Modal),
            mana_cost: ManaCost::zero(),
            ..BackFaceData::default()
        };
        back_face.card_types.core_types.push(CoreType::Sorcery);
        back_face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        object.back_face = Some(back_face);

        assert!(
            zero_cast_is_retained(runner.state(), mdfc),
            "an uncommitted spell//spell face choice is not a front-only zero proof"
        );
        runner
            .cast(mdfc)
            .modal_face(true)
            .resolve()
            .assert_hand_drawn(P0, 1);
    }

    #[test]
    fn held_selectable_back_face_cast_history_payoff_is_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let held = create_object(
            &mut state,
            CardId(91_862),
            P0,
            "Held face witness".to_string(),
            Zone::Hand,
        );
        {
            let object = state.objects.get_mut(&held).expect("held witness exists");
            object.card_types.core_types.push(CoreType::Instant);
            object.abilities = Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::NoOp,
            )]);
            let mut back_face = BackFaceData {
                name: "Held history payoff".to_string(),
                layout_kind: Some(LayoutKind::Modal),
                ..BackFaceData::default()
            };
            back_face.card_types.core_types.push(CoreType::Sorcery);
            back_face.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn {
                            scope: CountScope::Controller,
                            filter: None,
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            object.back_face = Some(back_face);
        }

        assert!(
            zero_cast_is_retained(&state, congregate),
            "a visible, selectable back-face cast-history payoff retains the zero spell"
        );
        state
            .objects
            .get_mut(&held)
            .expect("held witness remains")
            .back_face = None;
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the selectable back-face payoff restores known-zero rejection"
        );
    }

    fn spell_count_after_this_cast() -> QuantityExpr {
        QuantityExpr::Difference {
            left: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: None,
                },
            }),
            right: Box::new(QuantityExpr::Fixed { value: 1 }),
        }
    }

    fn history_payoff_back_face(layout_kind: Option<LayoutKind>) -> BackFaceData {
        let mut back_face = BackFaceData {
            name: "Typed alternate history payoff".to_string(),
            layout_kind,
            ..BackFaceData::default()
        };
        back_face.card_types.core_types.push(CoreType::Sorcery);
        back_face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: spell_count_after_this_cast(),
                target: TargetFilter::Controller,
            },
        ));
        back_face
    }

    fn alternate_payload_count(state: &GameState, player: PlayerId, object_id: ObjectId) -> usize {
        let mut count = 0;
        for_each_structurally_selectable_alternate_spell_payload(state, player, object_id, |_| {
            count += 1
        });
        count
    }

    #[test]
    fn alternate_payload_routes_are_zone_owner_and_keyword_bounded() {
        let (mut state, _) = funded_zero_congregate_state();
        let adventure = create_object(
            &mut state,
            CardId(91_864),
            P0,
            "Typed Adventure payload".to_string(),
            Zone::Hand,
        );
        {
            let object = state
                .objects
                .get_mut(&adventure)
                .expect("Adventure payload exists");
            object.card_types.core_types.push(CoreType::Creature);
            let mut back_face = history_payoff_back_face(Some(LayoutKind::Adventure));
            back_face.card_types.subtypes.push("Adventure".to_string());
            object.back_face = Some(back_face);
        }

        let omen = create_object(
            &mut state,
            CardId(91_865),
            P0,
            "Typed Omen payload".to_string(),
            Zone::Hand,
        );
        {
            let object = state.objects.get_mut(&omen).expect("Omen payload exists");
            object.card_types.core_types.push(CoreType::Enchantment);
            let mut back_face = history_payoff_back_face(Some(LayoutKind::Omen));
            back_face.card_types.subtypes.push("Omen".to_string());
            object.back_face = Some(back_face);
        }

        let mtmte = create_object(
            &mut state,
            CardId(91_868),
            P0,
            "Typed MTMTE payload".to_string(),
            Zone::Hand,
        );
        {
            let object = state.objects.get_mut(&mtmte).expect("MTMTE payload exists");
            object.card_types.core_types.push(CoreType::Creature);
            object
                .keywords
                .push(Keyword::MoreThanMeetsTheEye(ManaCost::zero()));
            object.back_face = Some(history_payoff_back_face(Some(LayoutKind::Transform)));
        }

        let disturb = create_object(
            &mut state,
            CardId(91_869),
            P0,
            "Typed Disturb payload".to_string(),
            Zone::Graveyard,
        );
        {
            let object = state
                .objects
                .get_mut(&disturb)
                .expect("Disturb payload exists");
            object.card_types.core_types.push(CoreType::Creature);
            object.keywords.push(Keyword::Disturb(ManaCost::zero()));
            object
                .base_keywords
                .push(Keyword::Disturb(ManaCost::zero()));
            object.back_face = Some(history_payoff_back_face(Some(LayoutKind::Transform)));
        }

        for (family, object_id) in [
            ("Adventure", adventure),
            ("Omen", omen),
            ("More Than Meets the Eye", mtmte),
            ("Disturb", disturb),
        ] {
            assert_eq!(
                alternate_payload_count(&state, P0, object_id),
                1,
                "{family} admits its production alternate payload"
            );
            assert!(
                structurally_selectable_alternate_spell_payload_has_cast_unstable_consumer(
                    &state,
                    P0,
                    &state.objects[&object_id],
                ),
                "{family}'s alternate payload reaches the stability walker"
            );
        }

        state
            .objects
            .get_mut(&adventure)
            .expect("Adventure remains")
            .zone = Zone::Battlefield;
        assert_eq!(
            alternate_payload_count(&state, P0, adventure),
            0,
            "Adventure's stored spell face is not scanned outside its hand/commander route"
        );
        state.objects.get_mut(&mtmte).expect("MTMTE remains").owner = P1;
        assert_eq!(
            alternate_payload_count(&state, P0, mtmte),
            0,
            "another player's hand payload is not attributed to this caster"
        );
        state
            .objects
            .get_mut(&disturb)
            .expect("Disturb remains")
            .zone = Zone::Hand;
        assert_eq!(
            alternate_payload_count(&state, P0, disturb),
            0,
            "Disturb's transformed payload is limited to its graveyard route"
        );
    }

    #[test]
    fn held_fuse_right_half_history_payoff_is_paired_and_resolves() {
        let (mut state, congregate) = funded_zero_congregate_state();
        create_object(
            &mut state,
            CardId(91_870),
            P0,
            "Fuse payoff draw".to_string(),
            Zone::Library,
        );
        let fuse = create_object(
            &mut state,
            CardId(91_866),
            P0,
            "Typed Fuse witness".to_string(),
            Zone::Hand,
        );
        {
            let object = state.objects.get_mut(&fuse).expect("Fuse witness exists");
            object.card_types.core_types.push(CoreType::Instant);
            object.abilities = Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::NoOp,
            )]);
            object.keywords.push(Keyword::Fuse);
            let mut right_half = BackFaceData {
                name: "Typed Fuse right half".to_string(),
                layout_kind: Some(LayoutKind::Split),
                ..BackFaceData::default()
            };
            right_half.card_types.core_types.push(CoreType::Sorcery);
            right_half.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: spell_count_after_this_cast(),
                    target: TargetFilter::Controller,
                },
            ));
            object.back_face = Some(right_half);
        }

        assert!(
            zero_cast_is_retained(&state, congregate),
            "the Fuse right-half payload retains the setup cast"
        );

        let mut before_setup = GameRunner::from_state(state.clone());
        before_setup
            .cast(fuse)
            .casting_variant(CastingVariant::Fuse)
            .resolve()
            .assert_hand_drawn(P0, 0);

        let mut after_setup = GameRunner::from_state(state.clone());
        after_setup.cast(congregate).target_player(P0).resolve();
        after_setup
            .cast(fuse)
            .casting_variant(CastingVariant::Fuse)
            .resolve()
            .assert_hand_drawn(P0, 1);

        state
            .objects
            .get_mut(&fuse)
            .and_then(|object| object.back_face.as_mut())
            .expect("Fuse right half remains")
            .abilities
            .clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the Fuse right-half payload restores known-zero rejection"
        );

        state
            .objects
            .get_mut(&fuse)
            .and_then(|object| object.back_face.as_mut())
            .expect("Fuse right half remains")
            .abilities
            .push(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: spell_count_after_this_cast(),
                    target: TargetFilter::Controller,
                },
            ));
        assert_eq!(
            alternate_payload_count(&state, P0, fuse),
            0,
            "Fuse exposes only right-half spell definitions to the payload visitor"
        );
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "a typed non-spell right-half definition cannot retain the setup cast"
        );
    }

    #[test]
    fn held_cleave_history_payload_is_paired_and_resolves() {
        let (mut state, congregate) = funded_zero_congregate_state();
        create_object(
            &mut state,
            CardId(91_871),
            P0,
            "Cleave payoff draw".to_string(),
            Zone::Library,
        );
        let cleave = create_object(
            &mut state,
            CardId(91_867),
            P0,
            "Typed Cleave witness".to_string(),
            Zone::Hand,
        );
        {
            let object = state
                .objects
                .get_mut(&cleave)
                .expect("Cleave witness exists");
            object.card_types.core_types.push(CoreType::Instant);
            object.abilities = Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::NoOp,
            )]);
            object.keywords.push(Keyword::Cleave(ManaCost::zero()));
            object.cleave_variant = Some(CleaveVariant {
                abilities: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: spell_count_after_this_cast(),
                        target: TargetFilter::Controller,
                    },
                )],
                ..CleaveVariant::default()
            });
        }

        assert!(
            zero_cast_is_retained(&state, congregate),
            "the Cleave payload retains the setup cast"
        );

        let mut before_setup = GameRunner::from_state(state.clone());
        before_setup
            .cast(cleave)
            .alternative_cast(engine::types::actions::AlternativeCastDecision::Alternative)
            .resolve()
            .assert_hand_drawn(P0, 0);

        let mut after_setup = GameRunner::from_state(state.clone());
        after_setup.cast(congregate).target_player(P0).resolve();
        after_setup
            .cast(cleave)
            .alternative_cast(engine::types::actions::AlternativeCastDecision::Alternative)
            .resolve()
            .assert_hand_drawn(P0, 1);

        state
            .objects
            .get_mut(&cleave)
            .expect("Cleave witness remains")
            .cleave_variant = None;
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the Cleave payload restores known-zero rejection"
        );
    }

    #[test]
    fn currently_false_play_from_exile_filter_is_admitted_after_zero_cast() {
        let (mut state, harvest) = funded_zero_harvest_state();
        let consumer = create_object(
            &mut state,
            CardId(91_863),
            P0,
            "Filtered exile consumer".to_string(),
            Zone::Exile,
        );
        create_object(
            &mut state,
            CardId(91_865),
            P0,
            "Filtered permission draw witness".to_string(),
            Zone::Library,
        );
        {
            let object = state.objects.get_mut(&consumer).expect("consumer exists");
            object.card_types.core_types.push(CoreType::Instant);
            object.mana_cost = ManaCost::generic(1);
            object.abilities = Arc::new(vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);
            object
                .casting_permissions
                .push(CastingPermission::PlayFromExile {
                    provenance: engine::types::ability::PlayFromExileProvenance::Impulse,
                    duration: Duration::Permanent,
                    granted_to: P0,
                    mode: engine::types::ability::CardPlayMode::Cast,
                    frequency: engine::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    invalidation: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: Some(TargetFilter::Typed(TypedFilter {
                        controller: None,
                        type_filters: vec![TypeFilter::Instant],
                        properties: vec![FilterProp::Cmc {
                            comparator: Comparator::LE,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::ZoneCardCount {
                                    zone: engine::types::ability::ZoneRef::Graveyard,
                                    card_types: vec![],
                                    filter: None,
                                    scope: CountScope::All,
                                },
                            },
                        }],
                    })),
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    alt_ability_cost: Some(AbilityCost::Mana {
                        cost: ManaCost::NoCost,
                    }),
                    land_enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                });
        }

        assert!(
            !spell_objects_available_to_cast(&state, P0).contains(&consumer),
            "the empty graveyard keeps the permission-backed consumer out of the current cast list"
        );
        assert!(
            zero_cast_is_retained(&state, harvest),
            "the visible grantee's dynamically filtered permission is still a future consumer"
        );

        let mut runner = GameRunner::from_state(state.clone());
        runner.cast(harvest).resolve();
        assert!(
            spell_objects_available_to_cast(runner.state(), P0).contains(&consumer),
            "the zero spell's normal graveyard move satisfies the production permission filter"
        );
        runner.cast(consumer).resolve().assert_hand_drawn(P0, 1);

        state
            .objects
            .get_mut(&consumer)
            .expect("consumer remains")
            .casting_permissions
            .clear();
        assert!(
            !zero_cast_is_retained(&state, harvest),
            "removing only the filtered permission restores known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_plotted_lock_and_load_payoff_is_paired() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let congregate = scenario
            .add_spell_to_hand_from_oracle(P0, "Congregate", true, CONGREGATE_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::White],
            })
            .id();
        let lock_and_load = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Lock and Load",
                false,
                "Draw a card, then draw a card for each other instant and sorcery spell you've cast this turn.\nPlot {3}{U} (You may pay {3}{U} and exile this card from your hand. Cast it as a sorcery on a later turn without paying its mana cost. Plot only as a sorcery.)",
            )
            .id();
        scenario.with_library_top(P0, &["Lock and Load draw 1", "Lock and Load draw 2"]);
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 3));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::White, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.turn_number = 2;
        state.players[P0.0 as usize]
            .hand
            .retain(|object_id| *object_id != lock_and_load);
        state.exile.push_back(lock_and_load);
        let object = state
            .objects
            .get_mut(&lock_and_load)
            .expect("Lock and Load exists");
        object.zone = Zone::Exile;
        object
            .casting_permissions
            .push(CastingPermission::Plotted { turn_plotted: 1 });

        assert!(
            spell_objects_available_to_cast(state, P0).contains(&lock_and_load),
            "the production permission authority admits the plotted public exile card"
        );
        assert!(
            zero_cast_is_retained(state, congregate),
            "a plotted public Lock and Load payoff retains the prior zero cast"
        );

        let mut payoff_witness = engine::game::scenario::GameRunner::from_state(state.clone());
        payoff_witness.cast(congregate).target_player(P0).resolve();
        payoff_witness
            .cast(lock_and_load)
            .casting_variant(CastingVariant::Plot)
            .resolve()
            .assert_hand_drawn(P0, 2);

        state
            .objects
            .get_mut(&lock_and_load)
            .expect("Lock and Load exists")
            .casting_permissions
            .clear();
        assert!(
            !spell_objects_available_to_cast(state, P0).contains(&lock_and_load),
            "removing only Plot permission makes the exile card unavailable"
        );
        assert!(
            !zero_cast_is_retained(state, congregate),
            "the unavailable exile card no longer defeats known-zero rejection"
        );
    }

    fn hidden_cast_history_payoff_face() -> CardFace {
        let mut face = CardFace {
            name: "Hidden cast-history payoff".to_string(),
            mana_cost: ManaCost::generic(1),
            ..Default::default()
        };
        face.card_type.core_types.push(CoreType::Instant);
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: None,
                    },
                },
                target: TargetFilter::Controller,
            },
        ));
        face
    }

    fn hidden_spell_cast_trigger_face() -> CardFace {
        let mut face = CardFace {
            name: "Hidden spell-cast trigger".to_string(),
            mana_cost: ManaCost::generic(1),
            ..Default::default()
        };
        face.triggers.push(
            engine::types::ability::TriggerDefinition::new(TriggerMode::SpellCast)
                .trigger_zones(vec![Zone::Hand]),
        );
        face
    }

    #[test]
    fn hidden_sampled_cast_history_identity_cannot_change_zero_cast_support() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let hidden = create_object(
            &mut state,
            CardId(91_704),
            P1,
            "Hidden slot".to_string(),
            Zone::Hand,
        );
        state.deck_pools.push(PlayerDeckPool {
            player: P1,
            current_main: Arc::new(vec![
                DeckEntry {
                    card: hidden_cast_history_payoff_face(),
                    count: 1,
                },
                DeckEntry {
                    card: CardFace {
                        name: "Hidden non-payoff".to_string(),
                        mana_cost: ManaCost::generic(1),
                        ..Default::default()
                    },
                    count: 1,
                },
            ]),
            ..Default::default()
        });

        let mut sampled_identities = HashSet::new();
        let mut sampled_support = HashSet::new();
        for seed in 0..64 {
            let mut rng = ChaCha20Rng::seed_from_u64(seed);
            let sampled = determinize_opponents(&state, P0, &mut rng);
            sampled_identities.insert(
                sampled
                    .objects
                    .get(&hidden)
                    .expect("sampled hidden slot exists")
                    .name
                    .clone(),
            );
            sampled_support.insert(zero_cast_is_retained(&sampled, congregate));
        }

        assert!(
            sampled_identities.contains("Hidden cast-history payoff")
                && sampled_identities.contains("Hidden non-payoff"),
            "the paired samples must reach both hidden identities"
        );
        assert_eq!(
            sampled_support,
            HashSet::from([false]),
            "hidden opponent identities must not change hard zero-cast candidate support"
        );
    }

    #[test]
    fn hidden_sampled_trigger_identity_cannot_change_zero_cast_support() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let hidden = create_object(
            &mut state,
            CardId(91_705),
            P1,
            "Hidden slot".to_string(),
            Zone::Hand,
        );
        state.deck_pools.push(PlayerDeckPool {
            player: P1,
            current_main: Arc::new(vec![
                DeckEntry {
                    card: hidden_spell_cast_trigger_face(),
                    count: 1,
                },
                DeckEntry {
                    card: CardFace {
                        name: "Hidden triggerless card".to_string(),
                        mana_cost: ManaCost::generic(1),
                        ..Default::default()
                    },
                    count: 1,
                },
            ]),
            ..Default::default()
        });

        let mut sampled_identities = HashSet::new();
        let mut sampled_support = HashSet::new();
        for seed in 0..64 {
            let mut rng = ChaCha20Rng::seed_from_u64(seed);
            let sampled = determinize_opponents(&state, P0, &mut rng);
            sampled_identities.insert(
                sampled
                    .objects
                    .get(&hidden)
                    .expect("sampled hidden slot exists")
                    .name
                    .clone(),
            );
            sampled_support.insert(zero_cast_is_retained(&sampled, congregate));
        }

        assert!(
            sampled_identities.contains("Hidden spell-cast trigger")
                && sampled_identities.contains("Hidden triggerless card"),
            "the paired samples must reach both hidden trigger shapes"
        );
        assert_eq!(
            sampled_support,
            HashSet::from([false]),
            "unknown opponent trigger definitions must not decide zero-cast support"
        );
    }

    #[test]
    fn hidden_face_down_cast_restriction_cannot_change_zero_harvest_support() {
        let (mut state, harvest) = funded_zero_harvest_state();
        let payoff = create_object(
            &mut state,
            CardId(91_706),
            P1,
            "Public cast-history restriction".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&payoff)
            .expect("cast-history restriction source exists")
            .casting_restrictions
            .push(CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn {
                            scope: CountScope::Controller,
                            filter: None,
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }),
            });

        assert_eq!(
            harvest_amount(&state, harvest),
            Some(0),
            "the public fixture's engine quantity authority proves Bountiful Harvest is zero"
        );
        assert!(
            zero_cast_is_retained(&state, harvest),
            "the face-up public counterpart reaches the zero-direct-effect gate"
        );

        engine::game::morph::apply_face_down_creature_characteristics(
            state
                .objects
                .get_mut(&payoff)
                .expect("cast-history restriction source exists"),
            &engine::types::ability::FaceDownProfile::vanilla_2_2(),
        );

        assert!(
            state.objects[&payoff]
                .casting_restrictions
                .iter()
                .any(casting_restriction_has_cast_unstable_condition),
            "face-down application leaves non-characteristic casting metadata conservatively visible"
        );
        assert!(
            !spell_identity_is_available_to_caster(&state, P0, &state.objects[&payoff]),
            "the opponent's face-down permanent has no public identity for the caster"
        );
        assert_eq!(
            harvest_amount(&state, harvest),
            Some(0),
            "the hidden fixture still has a zero Bountiful Harvest under the engine quantity authority"
        );
        assert!(
            !zero_cast_is_retained(&state, harvest),
            "unknown face-down metadata cannot decide zero-cast candidate support"
        );
    }

    #[test]
    fn zero_harvest_rhino_attack_trigger_is_paired_and_draws_after_a_mana_value_five_cast() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let rhino = scenario
            .add_creature_from_oracle(
                P0,
                "Rhino, Barreling Brute",
                6,
                7,
                "Vigilance, trample, haste\nWhenever Rhino attacks, if you've cast a spell with mana value 4 or greater this turn, draw a card.",
            )
            .id();
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        scenario.with_library_top(P0, &["Rhino attack draw"]);
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        assert_eq!(
            harvest_amount(state, harvest),
            Some(0),
            "the mana-value-five Bountiful Harvest has zero direct magnitude with no lands"
        );
        assert!(
            state.objects[&rhino]
                .trigger_definitions
                .as_slice()
                .iter()
                .any(|trigger| trigger_definition_has_cast_unstable_consumer(&trigger.definition)),
            "the real Rhino Oracle trigger reaches the cast-stability trigger census"
        );
        assert!(
            zero_cast_is_retained(state, harvest),
            "Rhino's delayed attack payoff retains the otherwise-zero prior cast"
        );

        let mut without_rhino_trigger = state.clone();
        let rhino_object = without_rhino_trigger
            .objects
            .get_mut(&rhino)
            .expect("Rhino exists in the paired control");
        rhino_object.trigger_definitions.clear();
        rhino_object.base_trigger_definitions = Arc::new(vec![]);
        assert!(
            !zero_cast_is_retained(&without_rhino_trigger, harvest),
            "removing only Rhino's cast-history trigger restores known-zero rejection"
        );

        runner.cast(harvest).resolve();
        let hand_before_attack = runner.state().players[P0.0 as usize].hand.len();
        {
            let state = runner.state_mut();
            state.phase = Phase::BeginCombat;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(rhino, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("the production combat reducer accepts Rhino's attack");
        runner.pass_both_players();
        assert_eq!(
            runner.state().players[P0.0 as usize].hand.len(),
            hand_before_attack + 1,
            "Rhino's production attack trigger draws after Bountiful Harvest's mana-value-five cast"
        );
    }

    #[test]
    fn zero_harvest_cast_sensitive_attack_filter_is_paired_and_resolves() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let attacker = scenario.add_vanilla(P0, 1, 1);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        scenario.with_library_top(P0, &["attack-filter draw"]);
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.objects.get_mut(&attacker).unwrap().mana_cost = ManaCost::generic(1);

        let hook = add_battlefield_trigger(state, 91_004, TriggerMode::Attacks);
        let cast_count = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: None,
            },
        };
        {
            let trigger = state.objects.get_mut(&hook).expect("attack hook exists");
            trigger.trigger_definitions[0].definition.valid_source = Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: cast_count,
                }]),
            ));
            trigger.trigger_definitions[0].definition.execute =
                Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )));
        }

        assert_eq!(harvest_amount(state, harvest), Some(0));
        assert!(
            trigger_definition_has_cast_unstable_consumer(
                &state.objects[&hook].trigger_definitions[0].definition
            ),
            "the live mana-value threshold in valid_source must reach the engine-owned trigger proof"
        );
        assert!(
            zero_cast_is_retained(state, harvest),
            "the engine-issued zero Harvest is retained because it enables the later attack trigger"
        );

        let mut without_filter = state.clone();
        without_filter
            .objects
            .get_mut(&hook)
            .expect("paired hook exists")
            .trigger_definitions
            .clear();
        assert!(
            !zero_cast_is_retained(&without_filter, harvest),
            "removing only the cast-sensitive attack trigger restores known-zero rejection"
        );

        let mut before_harvest = engine::game::scenario::GameRunner::from_state(state.clone());
        let hand_before_unenabled_attack = before_harvest.state().players[P0.0 as usize].hand.len();
        {
            let state = before_harvest.state_mut();
            state.phase = Phase::BeginCombat;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }
        before_harvest
            .act(GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("the production combat reducer accepts the unenabled one-mana attack");
        before_harvest.pass_both_players();
        assert_eq!(
            before_harvest.state().players[P0.0 as usize].hand.len(),
            hand_before_unenabled_attack,
            "before Harvest, the one-mana attacker misses the zero spell-count threshold"
        );

        runner.cast(harvest).resolve();
        let hand_before_attack = runner.state().players[P0.0 as usize].hand.len();
        {
            let state = runner.state_mut();
            state.phase = Phase::BeginCombat;
            state.priority_player = P0;
            state.waiting_for = WaitingFor::Priority { player: P0 };
        }
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("the production combat reducer accepts the one-mana attack");
        runner.pass_both_players();
        assert_eq!(
            runner.state().players[P0.0 as usize].hand.len(),
            hand_before_attack + 1,
            "the production attack trigger draws only after Harvest records its spell cast"
        );
    }

    #[test]
    fn zero_harvest_graveyard_additional_cost_is_paired_and_payable() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let held_draw = scenario
            .add_spell_to_hand(P0, "Graveyard Payment Draw", true)
            .with_mana_cost(ManaCost::zero())
            .from_oracle_text("Draw a card.")
            .with_additional_cost(AdditionalCost::Required(AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: None,
            }))
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        assert_eq!(harvest_amount(state, harvest), Some(0));
        let pre_harvest_cast = engine::ai_support::candidate_actions(state)
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == held_draw)
            })
            .expect("the engine issues a held-spell cast before resource payment preflight")
            .action;
        let pre_harvest_state = state.clone();
        let mut rejected_state = state.clone();
        let rejected =
            engine::game::engine::apply_as_current(&mut rejected_state, pre_harvest_cast);
        assert!(
            matches!(
                rejected,
                Err(engine::game::engine::EngineError::ActionNotAllowed(_))
            ),
            "the casting authority rejects the unpayable graveyard-exile cost"
        );
        assert_eq!(
            rejected_state.players[P0.0 as usize].hand,
            pre_harvest_state.players[P0.0 as usize].hand,
            "the rejected cast rolls back the held spell"
        );
        assert_eq!(
            rejected_state.players[P0.0 as usize].graveyard,
            pre_harvest_state.players[P0.0 as usize].graveyard,
            "the rejected cast rolls back graveyard payment state"
        );
        assert_eq!(
            rejected_state.stack, pre_harvest_state.stack,
            "the rejected cast does not commit a stack entry"
        );
        assert_eq!(
            rejected_state.pending_cast, pre_harvest_state.pending_cast,
            "the rejected cast restores the pending-cast boundary"
        );
        assert_eq!(
            rejected_state.waiting_for, pre_harvest_state.waiting_for,
            "the rejected cast restores the priority boundary"
        );
        assert!(
            zero_cast_is_retained(state, harvest),
            "a held graveyard additional cost is an object-level cast consumer"
        );
        let mut without_cost = state.clone();
        without_cost
            .objects
            .get_mut(&held_draw)
            .expect("held draw exists")
            .additional_cost = None;
        assert!(
            !zero_cast_is_retained(&without_cost, harvest),
            "removing only the object additional cost restores known-zero rejection"
        );

        runner.cast(harvest).resolve();
        assert_eq!(runner.state().objects[&harvest].zone, Zone::Graveyard);
        let cast = engine::ai_support::candidate_actions(runner.state())
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == held_draw)
            })
            .expect("Harvest makes the held spell castable")
            .action;
        runner
            .act(cast)
            .expect("the production cast enters additional-cost payment");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: engine::types::game_state::PayCostKind::ExileFromZone {
                    zone: engine::types::zones::ExileCostSourceZone::Graveyard,
                },
                count: 1,
                ..
            }
        ));
        runner
            .act(GameAction::SelectCards {
                cards: vec![harvest],
            })
            .expect("the Harvest card pays the held spell's required exile cost");
        assert_eq!(runner.state().objects[&harvest].zone, Zone::Exile);
        assert!(
            runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.id == held_draw),
            "the held Draw spell reaches the stack after the production payment"
        );
    }

    #[test]
    fn zero_harvest_modal_chooser_is_paired_and_routes_to_the_new_player() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_vanilla(P0, 1, 1);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let modal_spell = scenario
            .add_spell_to_hand(P0, "Live Chooser Instant", true)
            .with_mana_cost(ManaCost::zero())
            .from_oracle_text("You gain 1 life.")
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        let spell_count = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery))),
            },
        };
        let object = state
            .objects
            .get_mut(&modal_spell)
            .expect("modal spell exists");
        object.modal = Some(ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            chooser: PlayerFilter::ControlsCount {
                relation: PlayerRelation::All,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                comparator: Comparator::LE,
                count: Box::new(spell_count),
            },
            ..Default::default()
        });
        *Arc::make_mut(&mut object.abilities) = vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::LoseLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: Some(TargetFilter::Controller),
                },
            ),
        ];

        assert_eq!(harvest_amount(state, harvest), Some(0));
        assert!(zero_cast_is_retained(state, harvest));
        let mut without_modal = state.clone();
        without_modal
            .objects
            .get_mut(&modal_spell)
            .expect("modal spell exists")
            .modal = None;
        assert!(
            !zero_cast_is_retained(&without_modal, harvest),
            "removing only the live chooser metadata restores known-zero rejection"
        );

        let mut before_harvest = state.clone();
        let before_cast = engine::ai_support::candidate_actions(&before_harvest)
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == modal_spell)
            })
            .expect("the modal instant is initially castable")
            .action;
        engine::game::engine::apply_as_current(&mut before_harvest, before_cast)
            .expect("the production casting reducer opens the initial mode choice");
        assert!(matches!(
            before_harvest.waiting_for,
            WaitingFor::ModeChoice { player: P1, .. }
        ));

        runner.cast(harvest).resolve();
        let after_cast = engine::ai_support::candidate_actions(runner.state())
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == modal_spell)
            })
            .expect("the modal instant remains castable after Harvest")
            .action;
        runner
            .act(after_cast)
            .expect("the production casting reducer opens the updated mode choice");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ModeChoice { player: P0, .. }
        ));
        assert_eq!(
            runner
                .state()
                .spells_cast_this_turn_by_player
                .get(&P0)
                .map_or(0, |spells| spells.len()),
            1,
            "the instant's unfinalized own cast does not inflate the sorcery-only chooser threshold"
        );
    }

    #[test]
    fn zero_harvest_loan_shark_etb_trigger_is_paired_and_draws_after_the_second_spell() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let loan_shark = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Loan Shark",
                3,
                4,
                "When Loan Shark enters the battlefield, if you've cast two or more spells this turn, draw a card.\nPlot {3}{U} (You may pay {3}{U} and exile this card from your hand. Cast it as a sorcery on a later turn without paying its mana cost. Plot only as a sorcery.)",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        scenario.with_library_top(P0, &["Loan Shark draw"]);
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 7));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Blue, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        assert_eq!(
            harvest_amount(state, harvest),
            Some(0),
            "the first spell has zero direct magnitude before Loan Shark enters"
        );
        assert!(
            state.objects[&loan_shark]
                .trigger_definitions
                .as_slice()
                .iter()
                .any(|trigger| trigger_definition_has_cast_unstable_consumer(&trigger.definition)),
            "the real Loan Shark Oracle trigger reaches the visible castable-card census"
        );
        assert!(
            zero_cast_is_retained(state, harvest),
            "Loan Shark's self-ETB payoff retains the first otherwise-zero spell"
        );

        let mut without_loan_shark = state.clone();
        without_loan_shark.players[P0.0 as usize]
            .hand
            .retain(|object_id| *object_id != loan_shark);
        without_loan_shark.objects.remove(&loan_shark);
        assert!(
            !zero_cast_is_retained(&without_loan_shark, harvest),
            "removing only the available Loan Shark payoff restores known-zero rejection"
        );

        runner.cast(harvest).resolve();
        runner.cast(loan_shark).resolve().assert_hand_drawn(P0, 1);
    }

    #[test]
    fn zero_cast_storm_payoff_is_paired_and_copies_after_the_prior_cast() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let grapeshot = create_object(
            &mut state,
            CardId(91_706),
            P0,
            "Grapeshot".to_string(),
            Zone::Hand,
        );
        {
            let object = state.objects.get_mut(&grapeshot).expect("Grapeshot exists");
            object.card_types.core_types.push(CoreType::Sorcery);
            object.base_card_types = object.card_types.clone();
            object.mana_cost = ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            };
            object.keywords.push(Keyword::Storm);
            object.base_keywords.push(Keyword::Storm);
            Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                    excess: None,
                },
            ));
        }
        for _ in 0..2 {
            state.add_mana_to_pool(P0, ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
        }

        assert!(
            zero_cast_is_retained(&state, congregate),
            "a castable Storm spell makes the known-zero cast strategically relevant"
        );

        let mut without_prior_cast = engine::game::scenario::GameRunner::from_state(state.clone());
        without_prior_cast
            .cast(grapeshot)
            .target_player(P1)
            .resolve();
        assert_eq!(
            without_prior_cast.state().players[P1.0 as usize].life,
            19,
            "Storm has no copy before the first spell"
        );

        let mut with_prior_cast = engine::game::scenario::GameRunner::from_state(state);
        with_prior_cast.cast(congregate).target_player(P0).resolve();
        let storm_outcome = with_prior_cast.cast(grapeshot).target_player(P1).resolve();
        assert!(
            matches!(
                storm_outcome.final_waiting_for(),
                WaitingFor::CopyRetarget { .. }
            ),
            "the real Storm copy reaches its production keep-or-retarget prompt"
        );
        with_prior_cast
            .act(GameAction::KeepAllCopyTargets)
            .expect("keeping Grapeshot's existing target resolves the Storm copy");
        with_prior_cast.advance_until_stack_empty();
        assert_eq!(
            with_prior_cast.state().players[P1.0 as usize].life,
            18,
            "the real Storm resolver copies Grapeshot after the prior zero cast"
        );
    }

    #[test]
    fn zero_cast_festival_of_trokin_enables_dark_petition_spell_mastery() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let festival = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Festival of Trokin",
                false,
                "You gain 2 life for each creature you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::White],
            })
            .id();
        let petition = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Dark Petition",
                false,
                "Search your library for a card, put that card into your hand, then shuffle.\nSpell mastery — If there are two or more instant and/or sorcery cards in your graveyard, add {B}{B}{B}.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            })
            .id();
        scenario.add_spell_to_graveyard(P0, "Prior instant", true);
        let searched_card = scenario.add_card_to_library_top(P0, "Petition target");
        scenario.with_mana_pool(P0, pooled_mana(ManaType::White, 1));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Black, 5));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        assert_eq!(
            harvest_amount(state, festival),
            Some(0),
            "Festival's actual life-gain quantity is zero before the paired payoff is considered"
        );
        assert!(
            zero_cast_is_retained(state, festival),
            "Festival's zero gain must remain available when it enables Dark Petition's spell mastery"
        );
        let mut without_petition = state.clone();
        without_petition.players[P0.0 as usize]
            .hand
            .retain(|object_id| *object_id != petition);
        without_petition.objects.remove(&petition);
        assert!(
            !zero_cast_is_retained(&without_petition, festival),
            "removing only Dark Petition restores known-zero rejection"
        );

        runner.cast(festival).resolve();
        assert_eq!(runner.state().objects[&festival].zone, Zone::Graveyard);
        runner.cast(petition).resolve();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::SearchChoice { .. }
        ));
        runner
            .act(GameAction::SelectCards {
                cards: vec![searched_card],
            })
            .expect("selecting Dark Petition's searched card continues its real resolution");
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().players[P0.0 as usize].mana_pool.total(),
            3,
            "Festival is the second instant or sorcery card in the graveyard, so Dark Petition adds {{B}}{{B}}{{B}}"
        );
    }

    #[test]
    fn zero_cast_held_thousand_year_storm_is_paired_and_copies_after_setup() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let storm = scenario
            .add_enchantment_from_oracle(
                P0,
                "Thousand-Year Storm",
                "Whenever you cast an instant or sorcery spell, copy it for each other instant and sorcery spell you've cast before it this turn. You may choose new targets for the copies.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
            })
            .id();
        let bolt = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Lightning Bolt",
                true,
                "Lightning Bolt deals 3 damage to any target.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Red],
            })
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 8));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Blue, 1));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Red, 2));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.battlefield.retain(|object_id| *object_id != storm);
        state.players[P0.0 as usize].hand.push_back(storm);
        state.objects.get_mut(&storm).expect("Storm exists").zone = Zone::Hand;

        assert_eq!(
            harvest_amount(state, harvest),
            Some(0),
            "Bountiful Harvest's actual life-gain quantity is zero before Thousand-Year Storm is cast"
        );
        assert!(
            state.objects[&storm]
                .trigger_definitions
                .as_slice()
                .iter()
                .any(|trigger| trigger_definition_has_cast_unstable_consumer(&trigger.definition)),
            "Thousand-Year Storm's repeat_for metadata reaches the held-card payoff census"
        );
        assert!(
            zero_cast_is_retained(state, harvest),
            "a held Thousand-Year Storm must retain the first instant or sorcery cast"
        );
        let mut without_storm = state.clone();
        without_storm.players[P0.0 as usize]
            .hand
            .retain(|object_id| *object_id != storm);
        without_storm.objects.remove(&storm);
        assert!(
            !zero_cast_is_retained(&without_storm, harvest),
            "removing only Thousand-Year Storm restores known-zero rejection"
        );

        runner.cast(harvest).resolve();
        runner.cast(storm).resolve();
        let outcome = runner.cast(bolt).target_player(P1).resolve();
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::CopyRetarget { .. }),
            "Thousand-Year Storm's extra copy reaches its keep-or-retarget continuation"
        );
        runner.act(GameAction::KeepAllCopyTargets).expect(
            "keeping Lightning Bolt's existing target continues the Thousand-Year Storm copy",
        );
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().players[P1.0 as usize].life,
            14,
            "one earlier instant or sorcery gives Lightning Bolt exactly one Thousand-Year Storm copy"
        );
    }

    #[test]
    fn zero_cast_battlefield_activated_quantity_consumers_remain_paired() {
        let (mut state, harvest) = funded_zero_harvest_state();
        let source = create_object(
            &mut state,
            CardId(91_707),
            P0,
            "Zone-count activator".to_string(),
            Zone::Battlefield,
        );
        let graveyard_cards = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
            },
        };
        let activation = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: graveyard_cards.clone(),
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        let object = state
            .objects
            .get_mut(&source)
            .expect("zone-count source exists");
        Arc::make_mut(&mut object.abilities).push(activation);

        assert!(
            object_has_cast_unstable_consumer(&state, P0, &state.objects[&source], None),
            "a battlefield non-mana activated ability reading the graveyard is a cast payoff"
        );
        assert!(
            zero_cast_is_retained(&state, harvest),
            "a non-mana activated graveyard consumer retains the zero cast"
        );
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("zone-count source exists")
                .abilities,
        )
        .clear();
        assert!(
            !zero_cast_is_retained(&state, harvest),
            "removing only the activated graveyard consumer restores rejection"
        );
    }

    #[test]
    fn zero_cast_battlefield_mana_ability_keeps_zone_reads_but_not_unbound_x() {
        let (mut state, harvest) = funded_zero_harvest_state();
        let source = create_object(
            &mut state,
            CardId(91_708),
            P0,
            "Graveyard mana source".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&source)
            .expect("graveyard mana source exists");
        Arc::make_mut(&mut object.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount {
                                filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                                    FilterProp::InZone {
                                        zone: Zone::Graveyard,
                                    },
                                ])),
                            },
                        },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let ability = &state.objects[&source].abilities[0];
        assert!(
            engine::game::mana_abilities::is_mana_ability(ability),
            "the production classifier recognizes the fixture as a mana ability"
        );
        assert!(
            zero_cast_is_retained(&state, harvest),
            "a mana ability that reads the graveyard remains a possible post-cast payoff"
        );

        let Effect::Mana { produced, .. } = &mut *Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("graveyard mana source exists")
                .abilities,
        )[0]
        .effect
        else {
            unreachable!("fixture has a mana effect");
        };
        *produced = ManaProduction::Colorless {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
        };
        assert!(
            !zero_cast_is_retained(&state, harvest),
            "an otherwise-clean unbound X mana choice is not a read of the zero cast"
        );

        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("graveyard mana source exists")
                .abilities,
        )[0]
        .repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
            },
        });
        assert!(
            zero_cast_is_retained(&state, harvest),
            "a typed latent repeat count prevents the unbound-X mana exception from bypassing the production census"
        );
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&source)
                .expect("graveyard mana source exists")
                .abilities,
        )[0]
        .repeat_for = None;
        assert!(
            !zero_cast_is_retained(&state, harvest),
            "clearing only the latent repeat metadata restores the unbound-X exception"
        );
    }

    #[test]
    fn zero_land_harvest_enables_a_non_cost_static_payoff() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let figment = scenario.add_creature(P0, "Haunting Figment", 2, 2).id();
        let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        let mut static_definition = StaticDefinition::new(StaticMode::CantBeBlocked);
        static_definition.condition = Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::SpellsCastThisTurn {
                    scope: CountScope::Controller,
                    filter: Some(TargetFilter::Or {
                        filters: vec![
                            TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                            TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                        ],
                    }),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        });
        let figment_object = state.objects.get_mut(&figment).expect("Figment exists");
        figment_object
            .static_definitions
            .push(static_definition.clone());
        figment_object.base_static_definitions = Arc::new(vec![static_definition]);

        assert!(
            zero_cast_is_retained(state, harvest),
            "a zero-land Harvest can enable a functioning non-cost static"
        );
        assert!(
            engine::game::combat::can_block_pair(state, blocker, figment),
            "the static is inactive before the first spell"
        );

        runner.cast(harvest).resolve();
        assert!(
            !engine::game::combat::can_block_pair(runner.state(), blocker, figment),
            "the production static authority makes Figment unblockable after Harvest"
        );
    }

    #[test]
    fn zero_cast_enables_a_castable_etb_replacement_counter_payoff() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let harvest = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bountiful Harvest",
                false,
                "You gain 1 life for each land you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let master = scenario
            .add_creature_to_hand(P0, "Effortless Master", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 6));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Green, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        let replacement =
            engine::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Battlefield)
                .valid_card(TargetFilter::SelfRef)
                .condition(
                    engine::types::ability::ReplacementCondition::OnlyIfQuantity {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::SpellsCastThisTurn {
                                scope: CountScope::Controller,
                                filter: None,
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 2 },
                        active_player_req: None,
                    },
                )
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 2 },
                        target: TargetFilter::SelfRef,
                    },
                ));
        let master_object = state.objects.get_mut(&master).expect("Master exists");
        master_object.base_replacement_definitions = Arc::new(vec![replacement.clone()]);
        master_object.replacement_definitions = vec![replacement].into();

        assert!(
            zero_cast_is_retained(state, harvest),
            "a castable replacement whose quantity condition changes must retain Harvest"
        );

        let mut without_prior_cast = engine::game::scenario::GameRunner::from_state(state.clone());
        let baseline = without_prior_cast.cast(master).resolve();
        assert_eq!(
            baseline.counters(master, CounterType::Plus1Plus1),
            0,
            "the second-spell replacement is inactive for the first spell"
        );

        runner.cast(harvest).resolve();
        let payoff = runner.cast(master).resolve();
        assert_eq!(
            payoff.counters(master, CounterType::Plus1Plus1),
            2,
            "the production replacement pipeline adds both counters after Harvest"
        );
    }

    fn add_hand_journal_damage_payoff(state: &mut GameState) -> ObjectId {
        let payoff = create_object(
            state,
            CardId(91_701),
            P0,
            "Thunder Salvo".to_string(),
            Zone::Hand,
        );
        let object = state
            .objects
            .get_mut(&payoff)
            .expect("journal damage payoff exists");
        object.card_types.core_types.push(CoreType::Instant);
        object.base_card_types = object.card_types.clone();
        object.mana_cost = ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        };
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: None,
                    },
                },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
        ));
        payoff
    }

    fn add_hand_demilich_cost_payoff(state: &mut GameState) -> ObjectId {
        let payoff = create_object(
            state,
            CardId(91_702),
            P0,
            "Demilich".to_string(),
            Zone::Hand,
        );
        let object = state
            .objects
            .get_mut(&payoff)
            .expect("Demilich cost payoff exists");
        object.card_types.core_types.push(CoreType::Creature);
        object.base_card_types = object.card_types.clone();
        object.mana_cost = ManaCost::Cost {
            generic: 0,
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
        };
        let mut modifier = StaticDefinition::new(StaticMode::ModifyCost {
            mode: engine::types::statics::CostModifyMode::Reduce,
            amount: ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            },
            spell_filter: None,
            dynamic_count: Some(QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                    ],
                }),
            }),
        })
        .affected(TargetFilter::SelfRef);
        modifier.active_zones = engine::types::zones::self_spell_cost_mod_active_zones();
        object.static_definitions.push(modifier);
        payoff
    }

    #[test]
    fn cast_stability_classifier_rejects_direct_and_journal_population_reads() {
        let direct_count = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisGame {
                scope: CountScope::Controller,
                filter: None,
            },
        };
        let journal_population = QuantityExpr::Ref {
            qty: QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::TurnJournal {
                    journal: TurnJournalKind::SpellsCast,
                    scope: CountScope::Controller,
                    filter: None,
                },
            },
        };

        assert!(!quantity_is_cast_stable_for_pre_cast(&direct_count));
        assert!(!quantity_is_cast_stable_for_pre_cast(&journal_population));
    }

    #[test]
    fn zero_cast_journal_quantity_payoff_is_paired_and_changes_resolution() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let thunder_salvo = add_hand_journal_damage_payoff(&mut state);
        for _ in 0..4 {
            state.add_mana_to_pool(P0, ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
        }
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a held supported spell whose effect reads the cast journal retains the zero cast"
        );

        let mut resolution_state = state.clone();
        let victim = create_object(
            &mut resolution_state,
            CardId(91_703),
            P1,
            "Journal witness".to_string(),
            Zone::Battlefield,
        );
        {
            let object = resolution_state
                .objects
                .get_mut(&victim)
                .expect("journal witness exists");
            object.card_types.core_types.push(CoreType::Creature);
            object.base_card_types = object.card_types.clone();
            object.power = Some(2);
            object.toughness = Some(2);
            object.base_power = Some(2);
            object.base_toughness = Some(2);
        }

        let mut without_prior_cast =
            engine::game::scenario::GameRunner::from_state(resolution_state.clone());
        let baseline = without_prior_cast
            .cast(thunder_salvo)
            .target_object(victim)
            .resolve();
        baseline.assert_zone(&[victim], Zone::Battlefield);

        let mut with_prior_cast = engine::game::scenario::GameRunner::from_state(resolution_state);
        with_prior_cast.cast(congregate).target_player(P0).resolve();
        let payoff = with_prior_cast
            .cast(thunder_salvo)
            .target_object(victim)
            .resolve();
        payoff.assert_zone(&[victim], Zone::Graveyard);

        state.objects.remove(&thunder_salvo);
        state.players[P0.0 as usize]
            .hand
            .retain(|object_id| *object_id != thunder_salvo);
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the journal quantity payoff restores known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_journal_cost_payoff_is_paired_and_changes_effective_cost() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let demilich = add_hand_demilich_cost_payoff(&mut state);
        assert_eq!(
            effective_spell_cost(&state, P0, demilich),
            Some(ManaCost::Cost {
                generic: 0,
                shards: vec![
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                ],
            }),
            "the cost authority sees no reduction before the ordinary cast"
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a held supported self cost modifier that reads the cast journal retains the zero cast"
        );

        let mut runner = engine::game::scenario::GameRunner::from_state(state.clone());
        runner.cast(congregate).target_player(P0).resolve();
        assert_eq!(
            effective_spell_cost(runner.state(), P0, demilich),
            Some(ManaCost::Cost {
                generic: 0,
                shards: vec![
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                ],
            }),
            "the production cost authority applies the cast-history reduction after the cast"
        );

        state.objects.remove(&demilich);
        state.players[P0.0 as usize]
            .hand
            .retain(|object_id| *object_id != demilich);
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the journal cost payoff restores known-zero rejection"
        );
    }

    fn zero_gain_definition() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                player: TargetFilter::Player,
            },
        )
    }

    fn zero_controller_gain_definition() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 0 },
                player: TargetFilter::Controller,
            },
        )
    }

    fn moved_exile_then_draw_replacement(spell: ObjectId) -> ReplacementDefinition {
        let mut redirect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
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
        );
        redirect.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )));
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Graveyard)
            .valid_card(TargetFilter::SpecificObject { id: spell })
            .execute(redirect)
    }

    fn add_zero_controller_gain_spell(
        scenario: &mut GameScenario,
        name: &str,
        mana_cost: ManaCost,
    ) -> ObjectId {
        scenario
            .add_spell_to_hand(P0, name, true)
            .with_mana_cost(mana_cost)
            .with_ability_definition(zero_controller_gain_definition())
            .id()
    }

    fn add_mana_creature_with_cost(scenario: &mut GameScenario, cost: AbilityCost) -> ObjectId {
        scenario
            .add_creature(P0, "Mana-cost witness", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(cost),
            )
            .id()
    }

    #[test]
    fn zero_cast_moved_replacements_in_command_and_floating_stores_are_paired() {
        let mut command_scenario = GameScenario::new();
        command_scenario.at_phase(Phase::PreCombatMain);
        let command_spell = add_zero_controller_gain_spell(
            &mut command_scenario,
            "Command replacement witness",
            ManaCost::zero(),
        );
        command_scenario.with_library_top(P0, &["Command replacement draw"]);
        let mut command_runner = command_scenario.build();
        let command_state = command_runner.state_mut();
        command_state.active_player = P0;
        command_state.priority_player = P0;
        command_state.waiting_for = WaitingFor::Priority { player: P0 };
        let emblem = create_object(
            command_state,
            CardId(91_240),
            P0,
            "Command replacement emblem".to_string(),
            Zone::Command,
        );
        let emblem_object = command_state
            .objects
            .get_mut(&emblem)
            .expect("command emblem exists");
        emblem_object.is_emblem = true;
        emblem_object.replacement_definitions =
            vec![moved_exile_then_draw_replacement(command_spell)].into();

        assert!(
            active_replacements(command_state).any(|(_, object, _)| object.id == emblem),
            "reach guard: the shared functioning-replacement authority recognizes the emblem"
        );
        assert!(
            zero_cast_is_retained(command_state, command_spell),
            "a functioning command-zone replacement keeps the engine-issued zero cast"
        );

        let mut nonfunctioning_command = command_state.clone();
        nonfunctioning_command
            .objects
            .get_mut(&emblem)
            .expect("command object remains present")
            .is_emblem = false;
        assert!(
            !active_replacements(&nonfunctioning_command).any(|(_, object, _)| object.id == emblem),
            "reach guard: a non-emblem command-zone object is not functioning"
        );
        assert!(
            !zero_cast_is_retained(&nonfunctioning_command, command_spell),
            "a nonfunctioning command-zone replacement cannot defeat the known-zero proof"
        );

        let mut without_command_replacement = command_state.clone();
        without_command_replacement
            .objects
            .get_mut(&emblem)
            .expect("command emblem remains present")
            .replacement_definitions
            .clear();
        assert!(
            !zero_cast_is_retained(&without_command_replacement, command_spell),
            "removing only the command replacement restores rejection"
        );

        // CR 614.1a: the mandatory replacement redirects the spell's stack-to-graveyard move.
        let command_outcome = command_runner.cast(command_spell).resolve();
        command_outcome.assert_zone(&[command_spell], Zone::Exile);
        command_outcome.assert_hand_drawn(P0, 1);

        let mut floating_scenario = GameScenario::new();
        floating_scenario.at_phase(Phase::PreCombatMain);
        let floating_spell = add_zero_controller_gain_spell(
            &mut floating_scenario,
            "Floating replacement witness",
            ManaCost::zero(),
        );
        let install = floating_scenario
            .add_spell_to_hand(P0, "Install floating replacement", true)
            .with_mana_cost(ManaCost::zero())
            .with_ability(Effect::AddTargetReplacement {
                replacement: Box::new(moved_exile_then_draw_replacement(floating_spell)),
                target: TargetFilter::None,
            })
            .id();
        floating_scenario.with_library_top(P0, &["Floating replacement draw"]);
        let mut floating_runner = floating_scenario.build();
        let floating_state = floating_runner.state_mut();
        floating_state.active_player = P0;
        floating_state.priority_player = P0;
        floating_state.waiting_for = WaitingFor::Priority { player: P0 };
        floating_runner.cast(install).resolve();
        assert!(
            floating_runner
                .state()
                .pending_damage_replacements
                .iter()
                .any(|replacement| !replacement.is_consumed),
            "reach guard: the production AddTargetReplacement None route installed a live floating replacement"
        );
        assert!(
            zero_cast_is_retained(floating_runner.state(), floating_spell),
            "an unconsumed floating replacement keeps the engine-issued zero cast"
        );

        let mut consumed_floating = floating_runner.state().clone();
        consumed_floating.pending_damage_replacements[0].is_consumed = true;
        assert!(
            !zero_cast_is_retained(&consumed_floating, floating_spell),
            "a consumed floating replacement cannot defeat the known-zero proof"
        );

        let mut without_floating = floating_runner.state().clone();
        without_floating.pending_damage_replacements.clear();
        assert!(
            !zero_cast_is_retained(&without_floating, floating_spell),
            "removing only the floating replacement restores rejection"
        );

        let floating_outcome = floating_runner.cast(floating_spell).resolve();
        floating_outcome.assert_zone(&[floating_spell], Zone::Exile);
        floating_outcome.assert_hand_drawn(P0, 1);
    }

    #[test]
    fn zero_cast_untap_mana_cost_is_retained_and_paid_through_the_reducer() {
        assert!(tap_only_cost(&AbilityCost::Tap));
        assert!(
            !tap_only_cost(&AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Composite {
                        costs: vec![AbilityCost::Untap],
                    }
                ],
            }),
            "a nested untap cost is not tap-only"
        );

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = add_zero_controller_gain_spell(
            &mut scenario,
            "Untap payment witness",
            ManaCost::generic(1),
        );
        let untap_source = add_mana_creature_with_cost(&mut scenario, AbilityCost::Untap);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.objects[&untap_source].tapped = true;

        let issued = engine::ai_support::candidate_actions(runner.state());
        assert!(
            cast_is_retained_from_issued(runner.state(), spell, issued.clone()),
            "a tapped {{Q}} source makes the engine-issued Auto cast strategically relevant"
        );
        let cast = issued
            .into_iter()
            .find_map(|candidate| match candidate.action {
                action @ GameAction::CastSpell {
                    object_id,
                    payment_mode: CastPaymentMode::Auto,
                    ..
                } if object_id == spell => Some(action),
                _ => None,
            })
            .expect("the engine must issue the Auto cast before the gate evaluates it");
        runner.act(cast).expect("the Auto cast must enter payment");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));

        let payment = engine::ai_support::candidate_actions(runner.state())
            .into_iter()
            .find(|candidate| {
                matches!(
                    &candidate.action,
                    GameAction::ActivateAbility {
                        source_id,
                        ability_index: 0,
                    } if *source_id == untap_source
                )
            })
            .map(|candidate| candidate.action)
            .expect("the engine must offer the tapped {Q} mana ability in the live payment domain");
        // CR 107.6 + CR 601.2h: paying {Q} untaps the source while completing the cast.
        runner
            .act(payment)
            .expect("the engine-issued {Q} payment action must be accepted");
        assert!(
            !runner.state().objects[&untap_source].tapped,
            "paying the untap cost improves the source's board state"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("the produced mana must finalize the pending cast");
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);

        let mut tap_control_scenario = GameScenario::new();
        tap_control_scenario.at_phase(Phase::PreCombatMain);
        let tap_control_spell = add_zero_controller_gain_spell(
            &mut tap_control_scenario,
            "Tap payment control",
            ManaCost::generic(1),
        );
        add_mana_creature_with_cost(&mut tap_control_scenario, AbilityCost::Tap);
        let mut tap_control_runner = tap_control_scenario.build();
        let tap_control_state = tap_control_runner.state_mut();
        tap_control_state.active_player = P0;
        tap_control_state.priority_player = P0;
        tap_control_state.waiting_for = WaitingFor::Priority { player: P0 };
        assert!(
            !zero_cast_is_retained(tap_control_runner.state(), tap_control_spell),
            "an ordinary tap-only source leaves the no-payoff zero cast rejected"
        );
    }

    fn add_battlefield_metadata_consumer(
        state: &mut GameState,
        card_id: u64,
        definition: AbilityDefinition,
    ) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(card_id),
            P0,
            "Typed latent metadata consumer".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&object_id)
                .expect("metadata consumer exists")
                .abilities,
        )
        .push(definition);
        object_id
    }

    fn graveyard_count() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    engine::types::ability::FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
            },
        }
    }

    #[test]
    fn zero_cast_direct_and_deferred_metadata_consumers_are_paired() {
        #[derive(Debug)]
        enum MetadataCase {
            TargetCap,
            RepeatUntil,
            Duration,
            ModalConditionalMax,
            DeferredManaGrant,
            LegacyHandRestriction,
            FilterBearingDuration,
            GraveyardExileCost,
            OptionalPlayerFilter,
            TargetChooserFilter,
        }

        for (index, case) in [
            MetadataCase::TargetCap,
            MetadataCase::RepeatUntil,
            MetadataCase::Duration,
            MetadataCase::ModalConditionalMax,
            MetadataCase::DeferredManaGrant,
            MetadataCase::LegacyHandRestriction,
            MetadataCase::FilterBearingDuration,
            MetadataCase::GraveyardExileCost,
            MetadataCase::OptionalPlayerFilter,
            MetadataCase::TargetChooserFilter,
        ]
        .into_iter()
        .enumerate()
        {
            let (mut state, congregate) = funded_zero_congregate_state();
            let count = graveyard_count();
            let mut definition = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
            match case {
                MetadataCase::TargetCap => definition.target_constraints.push(
                    engine::types::game_state::TargetSelectionConstraint::TotalManaValue {
                        comparator: Comparator::LE,
                        value: count,
                    },
                ),
                MetadataCase::RepeatUntil => {
                    definition.repeat_until =
                        Some(engine::types::ability::RepeatContinuation::WhileCondition {
                            condition: Box::new(AbilityCondition::QuantityCheck {
                                lhs: count,
                                comparator: Comparator::GE,
                                rhs: QuantityExpr::Fixed { value: 0 },
                            }),
                            max_iterations: None,
                        });
                }
                MetadataCase::Duration => {
                    definition.duration = Some(engine::types::ability::Duration::ForAsLongAs {
                        condition: StaticCondition::QuantityComparison {
                            lhs: count,
                            comparator: Comparator::GE,
                            rhs: QuantityExpr::Fixed { value: 0 },
                        },
                    });
                }
                MetadataCase::ModalConditionalMax => {
                    let mut modal = engine::types::ability::ModalChoice::default();
                    modal.constraints.push(
                        engine::types::ability::ModalSelectionConstraint::ConditionalMaxChoices {
                            condition: engine::types::ability::ModalSelectionCondition::Static {
                                condition: StaticCondition::QuantityComparison {
                                    lhs: count,
                                    comparator: Comparator::GE,
                                    rhs: QuantityExpr::Fixed { value: 0 },
                                },
                            },
                            max_choices: 1,
                            otherwise_max_choices: 0,
                        },
                    );
                    definition.modal = Some(modal);
                }
                MetadataCase::DeferredManaGrant => {
                    *definition.effect = Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                        restrictions: vec![],
                        grants: vec![engine::types::mana::ManaSpellGrant::TriggerOnSpend {
                            filter: TargetFilter::Any,
                            ability: Box::new(zero_gain_definition()),
                        }],
                        expiry: None,
                        target: None,
                    };
                }
                MetadataCase::LegacyHandRestriction => definition.activation_restrictions.push(
                    ActivationRestriction::RequiresCondition {
                        condition: Some(ParsedCondition::HandSizeExact { count: 0 }),
                    },
                ),
                MetadataCase::FilterBearingDuration => {
                    definition.duration = Some(engine::types::ability::Duration::ForAsLongAs {
                        condition: StaticCondition::IsPresent {
                            filter: Some(TargetFilter::Typed(TypedFilter::creature().properties(
                                vec![engine::types::ability::FilterProp::Cmc {
                                    comparator: Comparator::LE,
                                    value: count,
                                }],
                            ))),
                        },
                    });
                }
                MetadataCase::GraveyardExileCost => {
                    definition.cost = Some(AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Graveyard),
                        filter: None,
                    });
                }
                MetadataCase::OptionalPlayerFilter => {
                    definition.optional_player =
                        Some(TargetFilter::Typed(TypedFilter::creature().properties(
                            vec![engine::types::ability::FilterProp::Cmc {
                                comparator: Comparator::LE,
                                value: count,
                            }],
                        )));
                }
                MetadataCase::TargetChooserFilter => {
                    definition.target_chooser =
                        Some(TargetFilter::Typed(TypedFilter::creature().properties(
                            vec![engine::types::ability::FilterProp::Cmc {
                                comparator: Comparator::LE,
                                value: count,
                            }],
                        )));
                }
            }

            let consumer =
                add_battlefield_metadata_consumer(&mut state, 91_300 + index as u64, definition);
            assert!(
                zero_cast_is_retained(&state, congregate),
                "{case:?} typed latent consumer reaches the production zero-cast census"
            );

            let consumer_definition = Arc::make_mut(
                &mut state
                    .objects
                    .get_mut(&consumer)
                    .expect("metadata consumer remains")
                    .abilities,
            )
            .first_mut()
            .expect("metadata consumer has one ability");
            *consumer_definition = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
            assert!(
                !zero_cast_is_retained(&state, congregate),
                "replacing only the {case:?} consumer with stable metadata restores known-zero rejection"
            );
        }
    }

    #[test]
    fn zero_cast_static_granted_definition_metadata_is_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let mut granted = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        granted.duration = Some(Duration::ForAsLongAs {
            condition: StaticCondition::QuantityComparison {
                lhs: graveyard_count(),
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        });
        let mut static_definition = StaticDefinition::new(StaticMode::CantBeBlocked);
        static_definition.modifications = vec![ContinuousModification::GrantAbility {
            definition: Box::new(granted),
        }];
        let source =
            add_battlefield_static_for_controller(&mut state, 91_350, P0, static_definition);
        state.battlefield.retain(|object_id| *object_id != source);
        state.players[P0.0 as usize].hand.push_back(source);
        state
            .objects
            .get_mut(&source)
            .expect("static metadata source remains")
            .zone = Zone::Hand;

        assert!(
            zero_cast_is_retained(&state, congregate),
            "a held typed latent duration on a statically granted definition reaches the production zero-cast census"
        );

        let modifications = &mut state
            .objects
            .get_mut(&source)
            .expect("static metadata source remains")
            .static_definitions[0]
            .modifications;
        let ContinuousModification::GrantAbility { definition } = &mut modifications[0] else {
            panic!("fixture has one granted definition");
        };
        **definition = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "replacing only the granted definition's latent metadata with a stable definition restores known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_static_granted_source_state_gate_is_conservative() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let mut granted = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        granted
            .activation_restrictions
            .push(ActivationRestriction::IsSolved);
        let mut static_definition = StaticDefinition::new(StaticMode::CantBeBlocked);
        static_definition.modifications = vec![ContinuousModification::GrantAbility {
            definition: Box::new(granted),
        }];
        let source =
            add_battlefield_static_for_controller(&mut state, 91_351, P0, static_definition);

        assert!(
            zero_cast_is_retained(&state, congregate),
            "the active static's source-state activation gate reaches the production zero-cast census"
        );

        let modifications = &mut state
            .objects
            .get_mut(&source)
            .expect("static source remains")
            .static_definitions[0]
            .modifications;
        let ContinuousModification::GrantAbility { definition } = &mut modifications[0] else {
            panic!("fixture has one granted definition");
        };
        **definition = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the source-state gate restores known-zero rejection"
        );
    }

    #[test]
    fn zero_effect_fold_requires_recognized_nonempty_componentwise_zero() {
        let mut scenario = GameScenario::new();
        let source = scenario.add_creature(P0, "Source", 1, 1).id();
        let runner = scenario.build();
        let state = runner.state();
        let zero = zero_gain_definition();
        assert!(definition_is_componentwise_known_zero(
            state, &zero, P0, source, true, false,
        ));

        let mut zero_then_draw = zero.clone();
        zero_then_draw.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::ParentTarget,
            },
        )));
        assert!(
            !definition_is_componentwise_known_zero(
                state,
                &zero_then_draw,
                P0,
                source,
                true,
                false,
            ),
            "a nonzero continuation prevents cancellation-style zero inference"
        );

        let mut gain_then_loss = zero_gain_definition();
        *gain_then_loss.effect = Effect::GainLife {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Player,
        };
        gain_then_loss.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::ParentTarget),
            },
        )));
        assert!(
            !definition_is_componentwise_known_zero(
                state,
                &gain_then_loss,
                P0,
                source,
                true,
                false,
            ),
            "equal and opposite nonzero effects are not a direct no-op proof"
        );

        let mut zero_then_parent_target = zero_gain_definition();
        zero_then_parent_target.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::ParentTarget,
            },
        )));
        assert!(
            definition_is_componentwise_known_zero(
                state,
                &zero_then_parent_target,
                P0,
                source,
                true,
                false,
            ),
            "a root player target binds the continuation's ParentTarget"
        );

        let mut controller_then_parent_target = zero_gain_definition();
        *controller_then_parent_target.effect = Effect::GainLife {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 0 },
            player: TargetFilter::Controller,
        };
        controller_then_parent_target.sub_ability = zero_then_parent_target.sub_ability;
        assert!(
            !definition_is_componentwise_known_zero(
                state,
                &controller_then_parent_target,
                P0,
                source,
                true,
                false,
            ),
            "ParentTarget is not accepted when the root did not bind a target"
        );
    }

    #[test]
    fn zero_effect_fold_stands_down_on_metadata_and_scope() {
        let mut scenario = GameScenario::new();
        let source = scenario.add_creature(P0, "Source", 1, 1).id();
        let runner = scenario.build();
        let state = runner.state();
        let mut scoped = zero_gain_definition();
        scoped.player_scope = Some(engine::types::ability::PlayerFilter::Opponent);
        assert!(!definition_is_componentwise_known_zero(
            state, &scoped, P0, source, true, false,
        ));

        let mut random_target = zero_gain_definition();
        random_target.target_selection_mode = engine::types::ability::TargetSelectionMode::Random;
        assert!(!definition_is_componentwise_known_zero(
            state,
            &random_target,
            P0,
            source,
            true,
            false,
        ));

        let mut resolution_target = zero_gain_definition();
        resolution_target.target_choice_timing =
            engine::types::ability::TargetChoiceTiming::Resolution;
        assert!(!definition_is_componentwise_known_zero(
            state,
            &resolution_target,
            P0,
            source,
            true,
            false,
        ));

        let mut target_chooser = zero_gain_definition();
        target_chooser.target_chooser = Some(TargetFilter::Opponent);
        assert!(!definition_is_componentwise_known_zero(
            state,
            &target_chooser,
            P0,
            source,
            true,
            false,
        ));
    }

    #[test]
    fn zero_cast_unrestricted_discard_is_rejected_and_positive_is_retained() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mind_sludge = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Mind Sludge",
                false,
                "Target player discards a card for each Swamp you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Black],
            })
            .id();
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pooled_mana(ManaType::Black, 1));
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        assert!(
            !zero_cast_is_retained(state, mind_sludge),
            "the production gate rejects a pool-funded unrestricted zero Discard"
        );

        let swamp = create_object(
            state,
            CardId(91_601),
            P0,
            "Swamp".to_string(),
            Zone::Battlefield,
        );
        let swamp_object = state.objects.get_mut(&swamp).expect("Swamp exists");
        swamp_object.card_types.core_types.push(CoreType::Land);
        swamp_object.card_types.subtypes.push("Swamp".to_string());
        assert!(
            zero_cast_is_retained(state, mind_sludge),
            "the same production candidate remains when its unrestricted Discard is positive"
        );
    }

    #[test]
    fn zero_cast_bound_parent_target_chain_is_rejected_and_positive_is_retained() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let mut zero_chain = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 0 },
                player: TargetFilter::Player,
            },
        );
        zero_chain.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::ParentTarget,
            },
        )));
        *Arc::make_mut(
            &mut state
                .objects
                .get_mut(&congregate)
                .expect("candidate spell exists")
                .abilities,
        ) = vec![zero_chain];

        assert!(
            !zero_cast_is_retained(&state, congregate),
            "the production gate rejects a zero direct root with its bound ParentTarget continuation"
        );

        let Effect::GainLife { amount, .. } = &mut *Arc::make_mut(
            &mut state
                .objects
                .get_mut(&congregate)
                .expect("candidate spell exists")
                .abilities,
        )
        .first_mut()
        .expect("candidate has one root ability")
        .effect
        else {
            unreachable!("fixture root remains GainLife");
        };
        *amount = QuantityExpr::Fixed { value: 1 };
        assert!(
            zero_cast_is_retained(&state, congregate),
            "the same production candidate remains when its bound-root chain is positive"
        );
    }

    #[test]
    fn trigger_disjointness_requires_a_structural_spell_type_contradiction() {
        let mut scenario = GameScenario::new();
        let spell_id = scenario
            .add_spell_to_hand_from_oracle(P0, "Congregate", true, CONGREGATE_ORACLE)
            .id();
        let runner = scenario.build();
        let spell = runner.state().objects.get(&spell_id).unwrap();

        let creature_cast = engine::types::ability::TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::Typed(TypedFilter::creature()));
        assert!(trigger_is_proven_irrelevant(spell, &creature_cast));

        let context_dependent_cast = engine::types::ability::TriggerDefinition::new(
            TriggerMode::SpellCast,
        )
        .valid_card(TargetFilter::Typed(
            TypedFilter::default().controller(engine::types::ability::ControllerRef::Opponent),
        ));
        assert!(
            !trigger_is_proven_irrelevant(spell, &context_dependent_cast),
            "a controller-relative filter is not a disjointness proof"
        );

        let creature_target_source =
            engine::types::ability::TriggerDefinition::new(TriggerMode::BecomesTarget);
        let mut creature_target_source = creature_target_source;
        creature_target_source.valid_source = Some(TargetFilter::Typed(TypedFilter::creature()));
        assert!(trigger_is_proven_irrelevant(spell, &creature_target_source,));
    }

    #[test]
    fn zero_cast_contextual_cast_and_target_hooks_use_production_gate() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let nonmatching = add_battlefield_trigger(&mut state, 91_000, TriggerMode::SpellCast);
        state
            .objects
            .get_mut(&nonmatching)
            .unwrap()
            .trigger_definitions[0]
            .definition
            .valid_card = Some(TargetFilter::Typed(TypedFilter::creature()));
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "a structurally nonmatching cast hook is irrelevant at the gate"
        );
        state
            .objects
            .get_mut(&nonmatching)
            .unwrap()
            .trigger_definitions[0]
            .definition
            .valid_card = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)));
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a matching cast hook retains the engine-issued cast"
        );
        state
            .objects
            .get_mut(&nonmatching)
            .unwrap()
            .trigger_definitions
            .clear();

        let target_hook = add_battlefield_trigger(&mut state, 91_004, TriggerMode::BecomesTarget);
        state
            .objects
            .get_mut(&target_hook)
            .unwrap()
            .trigger_definitions[0]
            .definition
            .valid_source = Some(TargetFilter::Typed(TypedFilter::creature()));
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "a structurally nonmatching target hook is irrelevant at the gate"
        );
        state
            .objects
            .get_mut(&target_hook)
            .unwrap()
            .trigger_definitions[0]
            .definition
            .valid_source = Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)));
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a matching target hook retains the engine-issued cast"
        );
    }

    #[test]
    fn zero_cast_commit_crime_trigger_survives_but_attacks_is_irrelevant() {
        let (mut state, congregate) = funded_zero_congregate_state();
        assert!(!zero_cast_is_retained(&state, congregate));

        let crime = add_battlefield_trigger(&mut state, 91_001, TriggerMode::CommitCrime);
        assert!(zero_cast_is_retained(&state, congregate));
        state
            .objects
            .get_mut(&crime)
            .expect("crime hook source exists")
            .trigger_definitions
            .clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the crime hook restores the known-zero rejection"
        );

        add_battlefield_trigger(&mut state, 91_002, TriggerMode::Attacks);
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "an attacks-only trigger is proven irrelevant to an ordinary cast"
        );

        add_battlefield_trigger(&mut state, 91_003, TriggerMode::ChangesZone);
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a generic zone-change hook must stay fail-open; only scalar self-ETB is irrelevant"
        );
    }

    #[test]
    fn zero_cast_payment_and_trigger_hooks_survive() {
        for (card_id, mode) in [
            (91_101, TriggerMode::ManaExpend),
            (91_102, TriggerMode::ManaAdded),
            (91_103, TriggerMode::SpellCast),
        ] {
            let (mut state, congregate) = funded_zero_congregate_state();
            let hook = add_battlefield_trigger(&mut state, card_id, mode.clone());
            assert!(
                zero_cast_is_retained(&state, congregate),
                "{mode:?} can observe a four-mana cast and must preserve it"
            );
            state
                .objects
                .get_mut(&hook)
                .expect("hook source exists")
                .trigger_definitions
                .clear();
            assert!(
                !zero_cast_is_retained(&state, congregate),
                "removing only {mode:?} restores the known-zero control"
            );
        }
    }

    #[test]
    fn zero_cast_hand_and_stack_hooks_are_paired() {
        for (card_id, name, zone) in [
            (91_151, "Hand hook", Zone::Hand),
            (91_152, "Stack hook", Zone::Stack),
        ] {
            let (mut state, congregate) = funded_zero_congregate_state();
            let hook = add_zone_trigger(
                &mut state,
                card_id,
                name,
                zone,
                engine::types::ability::TriggerDefinition::new(TriggerMode::SpellCast)
                    .trigger_zones(vec![zone]),
            );
            assert!(
                zero_cast_is_retained(&state, congregate),
                "a functioning {zone:?} spell-cast hook is a future observable"
            );
            state
                .objects
                .get_mut(&hook)
                .expect("hook source exists")
                .trigger_definitions
                .clear();
            assert!(
                !zero_cast_is_retained(&state, congregate),
                "removing only the {zone:?} hook restores rejection"
            );
        }
    }

    #[test]
    fn zero_cast_pool_coverage_bypasses_but_unfunded_slagheap_stands_down() {
        let (mut funded, congregate) = funded_zero_congregate_state();
        let slagheap = add_tapped_slagheap_mana_source(&mut funded);
        add_plain_mana_source(&mut funded, 91_402, engine::types::mana::ManaColor::White);
        for card_id in 91_403..=91_405 {
            add_plain_colorless_mana_source(&mut funded, card_id);
        }
        assert!(spell_cost_is_payable_from_pool(&funded, P0, congregate));
        assert!(
            !zero_cast_is_retained(&funded, congregate),
            "a pool-funded cast does not need the impure Slagheap source"
        );

        let mut unfunded = funded.clone();
        unfunded.players[P0.0 as usize].mana_pool.mana = pooled_mana(ManaType::Colorless, 1);
        assert!(
            !spell_cost_is_payable_from_pool(&unfunded, P0, congregate),
            "removing only pool coverage forces the gate to inspect available sources"
        );
        assert!(
            engine::game::mana_sources::activatable_mana_source_selections(&unfunded, P0)
                .iter()
                .any(|selection| {
                    selection.source.object_id == slagheap
                        && selection.ability_index == Some(2)
                        && selection.output == ManaSourceOutput::DeferredColorChoice
                }),
            "the production source census includes the tapped, indexed deferred Slagheap ability"
        );
        assert!(
            zero_cast_is_retained(&unfunded, congregate),
            "the engine-issued Auto root remains available when the same Slagheap makes payment impure"
        );

        let mut plain_only = unfunded.clone();
        plain_only.objects.remove(&slagheap);
        plain_only.battlefield.retain(|id| *id != slagheap);
        assert!(
            !zero_cast_is_retained(&plain_only, congregate),
            "the paired Auto root is rejected when only ordinary tap-for-mana sources remain"
        );

        let sacrificial = add_sacrificial_mana_source(&mut plain_only, 91_406);
        assert!(
            engine::game::mana_sources::activatable_mana_source_selections(&plain_only, P0)
                .iter()
                .any(|selection| {
                    selection.source.object_id == sacrificial
                        && selection.penalty == ManaSourcePenalty::Sacrifices
                }),
            "the production census exposes the available sacrifice-for-mana penalty"
        );
        assert!(
            zero_cast_is_retained(&plain_only, congregate),
            "an available sacrificial source keeps the engine-issued Auto cast fail-open even though ordinary lands can pay"
        );
        plain_only.objects.remove(&sacrificial);
        plain_only.battlefield.retain(|id| *id != sacrificial);
        assert!(
            !zero_cast_is_retained(&plain_only, congregate),
            "removing only the sacrificial source restores the pure-land Auto rejection"
        );
    }

    #[test]
    fn zero_cast_funded_ambiguous_mana_cost_survives() {
        let (mut state, congregate) = funded_zero_congregate_state();
        for (label, shards) in [
            ("Phyrexian", vec![ManaCostShard::PhyrexianWhite]),
            ("hybrid", vec![ManaCostShard::WhiteBlue]),
            ("X", vec![ManaCostShard::X]),
        ] {
            state.objects.get_mut(&congregate).unwrap().mana_cost =
                ManaCost::Cost { generic: 3, shards };
            assert!(
                spell_cost_is_payable_from_pool(&state, P0, congregate),
                "the funded {label} fixture reaches the public pool authority"
            );
            let issued = engine::ai_support::candidate_actions(&state);
            assert!(issued.iter().any(|candidate| matches!(
                candidate.action,
                GameAction::CastSpell { object_id, payment_mode: CastPaymentMode::Auto, .. }
                    if object_id == congregate
            )));
            assert!(
                cast_is_retained_from_issued(&state, congregate, issued),
                "pool affordability does not erase an unresolved {label} payment choice"
            );
        }
    }

    #[test]
    fn zero_cast_unsupported_object_and_root_shapes_survive() {
        let (mut state, congregate) = funded_zero_congregate_state();
        assert!(!zero_cast_is_retained(&state, congregate));

        state.objects.get_mut(&congregate).unwrap().modal = Some(ModalChoice::default());
        assert!(zero_cast_is_retained(&state, congregate));
        state.objects.get_mut(&congregate).unwrap().modal = None;
        assert!(!zero_cast_is_retained(&state, congregate));

        state
            .objects
            .get_mut(&congregate)
            .unwrap()
            .parse_warnings
            .push(OracleDiagnostic::IgnoredRemainder {
                text: "unmodeled rider".to_string(),
                parser: "test".to_string(),
                line_index: 0,
            });
        assert!(zero_cast_is_retained(&state, congregate));
        state
            .objects
            .get_mut(&congregate)
            .unwrap()
            .parse_warnings
            .clear();
        assert!(!zero_cast_is_retained(&state, congregate));

        Arc::make_mut(&mut state.objects.get_mut(&congregate).unwrap().abilities)
            .push(zero_gain_definition());
        assert!(
            zero_cast_is_retained(&state, congregate),
            "multiple primary roots cannot share one root recipient proof"
        );
    }

    #[test]
    fn zero_cast_nested_quantity_and_multitarget_metadata_survive() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let unknown_x = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        let nested_filter_x = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: unknown_x,
                    },
                ])),
            },
        };
        let spell_ledger = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: None,
            },
        };
        for amount in [nested_filter_x, spell_ledger] {
            let object = state.objects.get_mut(&congregate).unwrap();
            let definition = Arc::make_mut(&mut object.abilities).first_mut().unwrap();
            *definition.effect = Effect::GainLife {
                amount,
                player: TargetFilter::Player,
            };
            assert!(
                zero_cast_is_retained(&state, congregate),
                "an unsupported nested or ledger quantity remains outside the root veto"
            );
        }

        {
            let object = state.objects.get_mut(&congregate).unwrap();
            let definition = Arc::make_mut(&mut object.abilities).first_mut().unwrap();
            *definition = zero_gain_definition();
            definition.multi_target = Some(MultiTargetSpec::fixed(1, 2));
        }
        assert!(
            zero_cast_is_retained(&state, congregate),
            "multi-target metadata retains an engine-issued Auto cast with an otherwise known-zero effect"
        );
        Arc::make_mut(&mut state.objects.get_mut(&congregate).unwrap().abilities)
            .first_mut()
            .expect("Congregate has one primary spell definition")
            .multi_target = None;
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only multi-target metadata restores the known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_independent_subability_metadata_survives() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let mut root = zero_gain_definition();
        let mut child = zero_gain_definition();
        *child.effect = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 0 },
            player: TargetFilter::Controller,
        };
        child.sub_link = SubAbilityLink::SequentialSibling;
        root.sub_ability = Some(Box::new(child));
        *Arc::make_mut(&mut state.objects.get_mut(&congregate).unwrap().abilities)
            .first_mut()
            .expect("Congregate has one primary spell definition") = root;
        assert!(
            zero_cast_is_retained(&state, congregate),
            "an independent sibling retains an engine-issued Auto cast with otherwise known-zero effects"
        );
        Arc::make_mut(&mut state.objects.get_mut(&congregate).unwrap().abilities)
            .first_mut()
            .expect("Congregate has one primary spell definition")
            .sub_ability
            .as_mut()
            .expect("fixture has one child ability")
            .sub_link = SubAbilityLink::ContinuationStep;
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the child sibling link restores the known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_synthetic_prowess_trigger_is_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        *Arc::make_mut(&mut state.objects.get_mut(&congregate).unwrap().abilities)
            .first_mut()
            .expect("Congregate has one primary spell definition") = zero_gain_definition();
        let prowess = create_object(
            &mut state,
            CardId(91_205),
            P0,
            "Prowess fixture".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&prowess)
            .expect("Prowess source exists");
        object.card_types.core_types.push(CoreType::Creature);
        object.keywords.push(Keyword::Prowess);

        assert!(
            zero_cast_is_retained(&state, congregate),
            "an engine-issued Auto cast with a synthetic Prowess payoff must remain selectable"
        );
        state.objects.get_mut(&prowess).unwrap().keywords.clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only Prowess restores rejection for the same known-zero root"
        );
    }

    #[test]
    fn zero_cast_mana_spell_grants_are_paired_for_pool_and_source_paths() {
        let (mut pool_state, congregate) = funded_zero_congregate_state();
        let pool_grant_source = add_plain_colorless_mana_source(&mut pool_state, 91_210);
        pool_state.players[P0.0 as usize].mana_pool.mana[0].source_id = pool_grant_source;
        pool_state.players[P0.0 as usize].mana_pool.mana[0]
            .grants
            .push(ManaSpellGrant::TriggerOnSpend {
                filter: TargetFilter::Any,
                ability: Box::new(zero_gain_definition()),
            });
        assert!(
            zero_cast_is_retained(&pool_state, congregate),
            "a potentially spent pool grant preserves the engine-issued Auto cast"
        );
        pool_state.players[P0.0 as usize].mana_pool.mana[0]
            .grants
            .clear();
        assert!(
            !zero_cast_is_retained(&pool_state, congregate),
            "removing only the pool grant restores the known-zero rejection"
        );

        let (mut source_state, source_congregate) = funded_zero_congregate_state();
        source_state.players[P0.0 as usize].mana_pool.mana.clear();
        add_plain_colorless_mana_source(&mut source_state, 91_206);
        add_plain_colorless_mana_source(&mut source_state, 91_207);
        let grant_source = add_grant_mana_source(&mut source_state, 91_208);
        add_plain_mana_source(
            &mut source_state,
            91_209,
            engine::types::mana::ManaColor::White,
        );
        assert!(
            zero_cast_is_retained(&source_state, source_congregate),
            "an activatable grant-bearing source preserves the same legal Auto cast"
        );
        let ability = Arc::make_mut(
            &mut source_state
                .objects
                .get_mut(&grant_source)
                .expect("grant source exists")
                .abilities,
        )
        .first_mut()
        .expect("grant source has one mana ability");
        let Effect::Mana { grants, .. } = &mut *ability.effect else {
            unreachable!("fixture mana source has a mana effect");
        };
        grants.clear();
        assert!(
            !zero_cast_is_retained(&source_state, source_congregate),
            "removing only the source grant restores rejection"
        );
    }

    #[test]
    fn zero_cast_otherwise_only_source_mana_grant_is_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        state.players[P0.0 as usize].mana_pool.mana.clear();
        add_plain_colorless_mana_source(&mut state, 91_211);
        add_plain_colorless_mana_source(&mut state, 91_212);
        let source = add_otherwise_only_grant_mana_source(&mut state, 91_213);
        add_plain_mana_source(&mut state, 91_214, engine::types::mana::ManaColor::White);

        assert!(
            engine::game::mana_sources::activatable_mana_source_selections(&state, P0)
                .iter()
                .any(|selection| {
                    selection.source.object_id == source
                        && selection.ability_index == Some(0)
                        && selection.penalty == ManaSourcePenalty::None
                }),
            "the engine must issue the indexed no-penalty source selection before the gate scans it"
        );

        assert!(
            zero_cast_is_retained(&state, congregate),
            "an otherwise-only source mana grant preserves the legal Auto cast"
        );
        let mut without_grants = state.clone();
        let otherwise = Arc::make_mut(
            &mut without_grants
                .objects
                .get_mut(&source)
                .expect("otherwise grant source exists")
                .abilities,
        )
        .first_mut()
        .expect("otherwise grant source has one root ability")
        .sub_ability
        .as_deref_mut()
        .expect("root carries the ConditionInstead sub-ability")
        .else_ability
        .as_deref_mut()
        .expect("conditional sub-ability carries the otherwise mana branch");
        let Effect::Mana { grants, .. } = &mut *otherwise.effect else {
            unreachable!("otherwise branch remains a mana effect");
        };
        grants.clear();
        assert!(
            !zero_cast_is_retained(&without_grants, congregate),
            "clearing only the otherwise mana grants restores the known-zero rejection"
        );
    }

    #[test]
    fn zero_cast_off_zone_archive_and_command_hooks_are_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        let slagheap = add_tapped_slagheap_mana_source(&mut state);
        assert!(!zero_cast_is_retained(&state, congregate));

        let solitude = add_zone_trigger(
            &mut state,
            91_201,
            "Solitude",
            Zone::Graveyard,
            engine::types::ability::TriggerDefinition::new(TriggerMode::ChangesZone)
                .trigger_zones(vec![Zone::Battlefield]),
        );
        state
            .objects
            .get_mut(&solitude)
            .unwrap()
            .trigger_definitions
            .push(engine::types::ability::TriggerDefinition::new(
                TriggerMode::ChangesZone,
            ));
        assert!(spell_cost_is_payable_from_pool(&state, P0, congregate));
        assert!(
            engine::game::mana_sources::activatable_mana_source_selections(&state, P0)
                .iter()
                .any(|selection| {
                    selection.source.object_id == slagheap
                        && selection.ability_index == Some(2)
                        && selection.output == ManaSourceOutput::DeferredColorChoice
                }),
            "the funded archive context retains the indexed Slagheap source"
        );
        assert!(
            state.objects[&solitude].trigger_definitions.iter_unchecked().all(|entry| {
                !trigger_definition_functions_in_zone(&entry.definition, Zone::Graveyard)
            }),
            "both the explicit-Battlefield and default Solitude definitions are inert in the graveyard"
        );
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "a graveyard card whose trigger cannot function there is irrelevant"
        );

        let _carnarium = add_zone_trigger(
            &mut state,
            91_202,
            "Rakdos Carnarium",
            Zone::Battlefield,
            engine::types::ability::TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Battlefield)
                .valid_card(TargetFilter::SelfRef),
        );
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "a scalar self-ETB cannot observe an unrelated instant cast"
        );
        let command = add_zone_trigger(
            &mut state,
            91_203,
            "Command hook",
            Zone::Command,
            engine::types::ability::TriggerDefinition::new(TriggerMode::SpellCast)
                .trigger_zones(vec![Zone::Command]),
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a functioning command-zone spell-cast trigger makes the cast observable"
        );
        state
            .objects
            .get_mut(&command)
            .expect("command hook exists")
            .trigger_definitions
            .clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the command-zone hook restores rejection"
        );

        let hand_to_stack = add_battlefield_trigger(&mut state, 91_204, TriggerMode::ChangesZone);
        let definition = &mut state
            .objects
            .get_mut(&hand_to_stack)
            .unwrap()
            .trigger_definitions[0]
            .definition;
        definition.destination = Some(Zone::Stack);
        definition.valid_card = Some(TargetFilter::Typed(TypedFilter::card()));
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a battlefield hand-to-stack hook is not mistaken for the scalar self-ETB exception"
        );
    }

    #[test]
    fn zero_cast_delayed_trigger_is_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        assert!(!zero_cast_is_retained(&state, congregate));
        state.delayed_triggers.push(DelayedTrigger::new(
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
                vec![],
                congregate,
                P0,
            )),
            P0,
            congregate,
            true,
        ));
        assert!(
            zero_cast_is_retained(&state, congregate),
            "an installed delayed trigger is a cast-adjacent future observable"
        );
        state.delayed_triggers.clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the delayed trigger restores rejection"
        );
    }

    #[test]
    fn zero_cast_external_cost_and_keyword_producers_are_paired() {
        let cases = [
            StaticMode::CastWithAlternativeCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::zero(),
                },
                timing_permission: None,
                frequency: CastFrequency::Unlimited,
            },
            StaticMode::ImposeAdditionalCost {
                cost: AbilityCost::Tap,
                spell_filter: None,
                action: AdditionalCostTaxAction::Cast,
            },
            StaticMode::PayLifeAsColoredMana {
                color: engine::types::mana::ManaColor::Black,
            },
            StaticMode::CastWithKeyword {
                keyword: Keyword::Flash,
            },
        ];
        for (index, mode) in cases.into_iter().enumerate() {
            let (mut state, congregate) = funded_zero_congregate_state();
            let producer = add_battlefield_static(&mut state, 91_300 + index as u64, mode);
            assert!(
                zero_cast_is_retained(&state, congregate),
                "a live external static changes cost or keyword semantics"
            );
            state
                .objects
                .get_mut(&producer)
                .expect("producer exists")
                .static_definitions
                .clear();
            assert!(
                !zero_cast_is_retained(&state, congregate),
                "removing only the external static restores rejection"
            );
        }
    }

    #[test]
    fn zero_cast_opponent_imposed_cost_is_caster_conditional_and_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        state.players[P1.0 as usize].life = 5;
        let producer = add_battlefield_static_for_controller(
            &mut state,
            91_350,
            P1,
            StaticDefinition::new(StaticMode::ImposeAdditionalCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Sacrifice(engine::types::ability::SacrificeCost::count(
                            TargetFilter::SelfRef,
                            1,
                        )),
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            selection: engine::types::ability::CardSelectionMode::Chosen,
                            self_scope: engine::types::ability::DiscardSelfScope::FromHand,
                        },
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                },
                spell_filter: None,
                action: AdditionalCostTaxAction::Cast,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::default().controller(engine::types::ability::ControllerRef::Opponent),
            ))
            .condition(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 10 },
            }),
        );
        assert!(
            game_functioning_statics(&state).any(|(source, _)| source.id == producer),
            "the conservative presence iterator reaches the source before condition application"
        );
        assert!(
            !game_active_statics(&state).any(|(source, _)| source.id == producer),
            "the active iterator applies the false source-controller condition"
        );
        state.objects.get_mut(&producer).unwrap().controller = P0;
        assert!(
            game_active_statics(&state).any(|(source, _)| source.id == producer),
            "the engine condition evaluator admits the same condition for P0's life total"
        );
        state.objects.get_mut(&producer).unwrap().controller = P1;
        assert!(
            zero_cast_is_retained(&state, congregate),
            "the functioning-source guard keeps the engine-issued cast when this external authority exists"
        );
        state
            .objects
            .get_mut(&producer)
            .expect("opponent tax source exists")
            .static_definitions
            .clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the opponent-conditioned tax restores rejection"
        );
    }

    #[test]
    fn zero_cast_effective_and_pending_keyword_producers_are_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        state.add_transient_continuous_effect(
            congregate,
            P0,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: congregate },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flash,
            }],
            None,
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "an effective off-zone keyword must stand down the zero-cast proof"
        );
        state.transient_continuous_effects.clear();
        assert!(!zero_cast_is_retained(&state, congregate));

        state
            .pending_next_spell_modifiers
            .push(PendingNextSpellModifier {
                player: P0,
                modifier: NextSpellModifier::HasKeyword {
                    keyword: Keyword::Flash,
                },
                spell_filter: None,
                source_id: None,
            });
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a pending next-spell keyword changes this cast's semantics"
        );
        state.pending_next_spell_modifiers.clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the pending modifier restores rejection"
        );
    }

    #[test]
    fn zero_cast_transient_granted_cast_keyword_is_paired() {
        let (mut state, congregate) = funded_zero_congregate_state();
        state.add_transient_continuous_effect(
            congregate,
            P0,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: congregate },
            vec![ContinuousModification::GrantStaticAbility {
                definition: Box::new(StaticDefinition::new(StaticMode::CastWithKeyword {
                    keyword: Keyword::Flash,
                })),
            }],
            None,
        );
        assert!(
            zero_cast_is_retained(&state, congregate),
            "a transient grant of a cast keyword invalidates the zero-cast proof"
        );
        state.transient_continuous_effects.clear();
        assert!(
            !zero_cast_is_retained(&state, congregate),
            "removing only the transient grant restores rejection"
        );
    }

    #[test]
    fn zero_cast_non_auto_payment_modes_are_not_root_rejected() {
        let (state, congregate) = funded_zero_congregate_state();
        assert!(zero_cast_with_payment_mode_is_retained(
            &state,
            congregate,
            CastPaymentMode::Manual,
        ));
        assert!(zero_cast_with_payment_mode_is_retained(
            &state,
            congregate,
            CastPaymentMode::AutoExceptSacrificialMana,
        ));
    }

    #[test]
    fn rejects_pump_after_combat_without_live_threat() {
        let mut scenario = GameScenario::new();
        scenario.add_creature(P0, "Bear", 2, 2);
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .id();

        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.phase = Phase::PostCombatMain;
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: growth,
                card_id: state.objects.get(&growth).unwrap().card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    #[test]
    fn allows_pump_that_wins_combat() {
        let mut scenario = GameScenario::new();
        let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
        let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .id();

        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
            blocker_assignments: [(attacker, vec![blocker])].into_iter().collect(),
            blocker_to_attacker: [(blocker, vec![attacker])].into_iter().collect(),
            ..Default::default()
        });

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: growth,
                card_id: state.objects.get(&growth).unwrap().card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    #[test]
    fn penalizes_targeting_already_dead_creature() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Target", 2, 2).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.stack.push_back(StackEntry {
            id: ObjectId(200),
            source_id: ObjectId(201),
            controller: P0,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ResolvedAbility::new(
                    Effect::Destroy {
                        target: TargetFilter::Any,
                        cant_regenerate: false,
                    },
                    vec![TargetRef::Object(creature)],
                    ObjectId(201),
                    P0,
                ))),
                card_id: CardId(201),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: P0,
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(202),
                    CardId(202),
                    ResolvedAbility::new(
                        Effect::Destroy {
                            target: TargetFilter::Any,
                            cant_regenerate: false,
                        },
                        Vec::new(),
                        ObjectId(202),
                        P0,
                    ),
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(creature)],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: TargetSelectionProgress::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_eq!(
            assess_candidate(&ctx),
            GateDecision::AllowWithPenalty(-10.0)
        );
    }

    /// CR 601.2: A spell is cast the moment it's announced — the rules provide
    /// no strategic rewind. The AI's strategic pool must reject CancelCast so
    /// that pre-cast commitment stays coherent with targeting and payment.
    /// (The fallback_action escape in `search.rs` still supplies CancelCast
    /// when the scored pool is empty, covering genuine "can't complete cast"
    /// cases like unaffordable post-cost-increase mana or all targets gone.)
    #[test]
    fn rejects_cancel_cast_as_strategic_candidate() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Elvish Mystic", 1, 1).id();
        let unsummon = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.waiting_for = WaitingFor::TargetSelection {
            player: P0,
            pending_cast: Box::new(PendingCast::new(
                unsummon,
                state.objects.get(&unsummon).unwrap().card_id,
                ResolvedAbility::new(
                    Effect::Bounce {
                        target: TargetFilter::Any,
                        destination: None,
                        selection: BounceSelection::Targeted,
                    },
                    Vec::new(),
                    unsummon,
                    P0,
                ),
                ManaCost::zero(),
            )),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(creature)],
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: TargetSelectionProgress::default(),
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CancelCast,
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Pass),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// Build a `ChooseTarget` decision for a damage spell aimed at `creature`.
    fn damage_target_decision(creature: ObjectId, damage: i32) -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: P0,
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(900),
                    CardId(900),
                    ResolvedAbility::new(
                        Effect::DealDamage {
                            amount: engine::types::ability::QuantityExpr::Fixed { value: damage },
                            target: TargetFilter::Any,
                            damage_source: None,
                            excess: None,
                        },
                        Vec::new(),
                        ObjectId(900),
                        P0,
                    ),
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(creature)],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: TargetSelectionProgress::default(),
            },
            candidates: Vec::new(),
        }
    }

    fn choose_target_candidate(creature: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        }
    }

    /// CR 702.12b: a damage-only spell can never kill an indestructible creature.
    #[test]
    fn rejects_damage_targeting_indestructible_creature() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Darksteel Wall", 0, 4)
            .with_keyword(Keyword::Indestructible)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// A normal creature targeted by damage is NOT gate-rejected — non-lethal
    /// burn is a judgment-layer preference, not a provable futility.
    #[test]
    fn allows_damage_targeting_normal_creature() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Wall", 0, 4).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: never target a warded creature whose ward cost the AI can't
    /// pay — the spell would just be countered.
    #[test]
    fn rejects_targeting_unpayable_ward() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::PayLife(100)))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        // P0 starts at 20 life — paying 100 is impossible.
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// A payable ward does not gate the target out — it's only priced (in the
    /// judgment layer), not vetoed.
    #[test]
    fn allows_targeting_payable_ward() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::PayLife(2)))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a + CR 119.4: Phyrexian Fleshgorger's Ward uses its current
    /// power as the life payment, so targeting is futile at or below that
    /// power and remains available when the AI can pay without losing.
    #[test]
    fn dynamic_life_ward_uses_the_warded_creatures_current_power() {
        const FLESHGORGER: &str =
            "Menace, lifelink\nWard—Pay life equal to Phyrexian Fleshgorger's power.";

        let gate_for = |life, current_power| {
            let mut scenario = GameScenario::new();
            scenario.with_life(P0, life);
            let creature = scenario
                .add_creature_from_oracle(P1, "Phyrexian Fleshgorger", 7, 5, FLESHGORGER)
                .id();
            let mut runner = scenario.build();
            let state = runner.state_mut();
            let fleshgorger = state.objects.get_mut(&creature).unwrap();
            fleshgorger.base_power = Some(current_power);
            fleshgorger.power = Some(current_power);

            let decision = damage_target_decision(creature, 3);
            let candidate = choose_target_candidate(creature);
            let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
            let ctx = PolicyContext {
                state,
                decision: &decision,
                candidate: &candidate,
                ai_player: P0,
                config: &config,
                context: &AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            };
            assess_candidate(&ctx)
        };

        assert_eq!(gate_for(6, 7), GateDecision::Reject);
        assert_eq!(gate_for(7, 7), GateDecision::Reject);
        assert_ne!(gate_for(8, 7), GateDecision::Reject);
        assert_ne!(gate_for(4, 3), GateDecision::Reject);
    }

    /// CR 702.21a + CR 104.3d: a Ward payment that would push the AI to ten
    /// or more poison counters must reject the target — the AI must never
    /// treat ending its own game as an ordinary payable ward cost.
    #[test]
    fn rejects_targeting_ward_that_would_be_lethal_poison() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 5,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.players[P0.0 as usize].poison_counters = 5;
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// A poison Ward payment that stays below the ten-poison threshold does
    /// not gate the target out — mirrors `allows_targeting_payable_ward`.
    #[test]
    fn allows_targeting_ward_with_nonlethal_poison_payment() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 5,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        // P0 starts at 0 poison — 0 + 5 = 5, well below the 10-poison SBA.
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a + CR 104.3d: two individually-nonlethal poison sub-costs in
    /// a `Compound` Ward can be jointly lethal — the aggregate across every
    /// sub-cost must be checked, not each sub-cost against the same
    /// unchanged starting total.
    #[test]
    fn rejects_targeting_ward_with_jointly_lethal_compound_poison() {
        let mut scenario = GameScenario::new();
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::Compound(vec![
                WardCost::GetPlayerCounters {
                    counter_kind: engine::types::player::PlayerCounterKind::Poison,
                    count: 3,
                },
                WardCost::GetPlayerCounters {
                    counter_kind: engine::types::player::PlayerCounterKind::Poison,
                    count: 3,
                },
            ])))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        // 4 existing + 3 + 3 = 10 (lethal), but 4 + 3 = 7 alone is not — a
        // per-sub-cost check against the same starting total would wrongly
        // allow this.
        state.players[P0.0 as usize].poison_counters = 4;
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: the ward-affordability gate applies to any targetable
    /// permanent, not just creatures — a lethal poison Ward on a noncreature
    /// permanent must be rejected identically.
    #[test]
    fn rejects_targeting_noncreature_ward_that_would_be_lethal_poison() {
        let mut scenario = GameScenario::new();
        let artifact = scenario
            .add_creature(P1, "Warded Artifact", 0, 0)
            .as_artifact()
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 5,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.players[P0.0 as usize].poison_counters = 5;
        let decision = damage_target_decision(artifact, 3);
        let candidate = choose_target_candidate(artifact);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 104.3d + CR 614.1a: a doubler on the poison counters the AI itself
    /// would receive can make an individually-nonlethal PRINTED count
    /// actually lethal once replacement-adjusted. The AI must project the
    /// real, replacement-adjusted result (`preview_player_counter_addition`)
    /// rather than trusting the printed count — the naive printed-count math
    /// (4 existing + 3 printed = 7) would wrongly call this safe, but the
    /// doubled result (4 + 6 = 10) is lethal.
    #[test]
    fn rejects_targeting_ward_with_lethal_poison_after_doubling_replacement() {
        let mut scenario = GameScenario::new();
        // A permanent the AI (P0) controls that doubles poison counters P0
        // would receive. `valid_player: Some(You)` + the default recipient
        // scope means this applies whenever P0 is the one gaining counters,
        // mirroring how `player_counter.rs`'s own Solemnity test constructs a
        // global player-counter replacement, parameterized to double instead
        // of prevent.
        let doubler_id = scenario.add_creature(P0, "Poison Doubler", 0, 0).id();
        let mut doubler_def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        doubler_def.valid_player = Some(ReplacementPlayerScope::You);
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 3,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&doubler_id)
            .unwrap()
            .replacement_definitions = vec![doubler_def].into();
        state.players[P0.0 as usize].poison_counters = 4;
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: a "players can't get counters" replacement (Solemnity) means
    /// the AI's Ward payment will actually FAIL — `costs.rs`'s
    /// `AbilityCost::GetPlayerCounters` treats `Prevented` as a failed payment,
    /// not a zero-cost one — so the AI must not target into this believing the
    /// Ward is safely (and freely) payable.
    #[test]
    fn rejects_targeting_ward_with_prevented_player_counter_payment() {
        let mut scenario = GameScenario::new();
        let solemnity_id = scenario.add_creature(P0, "Solemnity", 0, 0).id();
        let mut prevent_def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        prevent_def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::GetPlayerCounters {
                counter_kind: engine::types::player::PlayerCounterKind::Poison,
                count: 3,
            }))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&solemnity_id)
            .unwrap()
            .replacement_definitions = vec![prevent_def].into();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// CR 702.21a: a `Compound` Ward's sub-costs are conjoined — ALL must be
    /// payable, so a prevented `GetPlayerCounters` sub-cost must reject the
    /// whole cost even when its sibling sub-cost (here, a small life payment)
    /// is perfectly payable on its own. Proves the recursion through
    /// `Compound`'s `.all(|cost| can_pay_ward_cost(...))`, not just the direct
    /// leaf case covered by `rejects_targeting_ward_with_prevented_player_counter_payment`.
    #[test]
    fn rejects_compound_ward_with_prevented_player_counter_leaf() {
        let mut scenario = GameScenario::new();
        let solemnity_id = scenario.add_creature(P0, "Solemnity", 0, 0).id();
        let mut prevent_def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        prevent_def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        let creature = scenario
            .add_creature(P1, "Warded", 2, 2)
            .with_keyword(Keyword::Ward(WardCost::Compound(vec![
                WardCost::PayLife(2), // trivially payable on its own (P0 starts at 20 life)
                WardCost::GetPlayerCounters {
                    counter_kind: engine::types::player::PlayerCounterKind::Poison,
                    count: 3,
                },
            ])))
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state
            .objects
            .get_mut(&solemnity_id)
            .unwrap()
            .replacement_definitions = vec![prevent_def].into();
        let decision = damage_target_decision(creature, 3);
        let candidate = choose_target_candidate(creature);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }
    /// Build an Improvise `ManaPayment` decision context: a `TapForConvoke`
    /// Colorless candidate for `object_id`, plus whatever sibling candidates
    /// the caller supplies for that same dual-purpose permanent.
    fn improvise_mana_payment_decision(
        sibling_candidates: Vec<CandidateAction>,
        object_id: ObjectId,
    ) -> AiDecisionContext {
        let mut candidates = vec![CandidateAction {
            action: GameAction::TapForConvoke {
                object_id,
                mana_type: ManaType::Colorless,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Mana),
        }];
        candidates.extend(sibling_candidates);
        AiDecisionContext {
            waiting_for: WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: Some(engine::types::game_state::ConvokeMode::Improvise),
            },
            candidates,
        }
    }

    fn native_blue_tap_candidate(object_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::TapLandForMana {
                selection: engine::types::mana::ManaSourceSelection {
                    source: engine::types::identifiers::ObjectIncarnationRef {
                        object_id,
                        incarnation: 0,
                    },
                    ability_index: None,
                    mana_type: ManaType::Blue,
                    output: engine::types::mana::ManaSourceOutput::Concrete(ManaType::Blue),
                    atomic_combination: None,
                    restrictions: Vec::new(),
                    penalty: engine::types::mana::ManaSourcePenalty::None,
                    taps_for_mana: Vec::new(),
                },
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Mana),
        }
    }

    fn convoke_candidate_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        config: &'a crate::config::AiConfig,
        context: &'a AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate: &decision.candidates[0],
            ai_player: P0,
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        }
    }

    /// CR 702.51a + CR 702.126a: a dual-purpose permanent (an artifact land
    /// producing {U} natively) must not be tapped for its Colorless Improvise
    /// marker while the pending cast's {U} pip is still outstanding — that
    /// would strand the pip and dead-end `ManaPayment` (the Metallic Rebuke
    /// bug: crates/phase-ai/src/search.rs's `fallback_action` panic).
    #[test]
    fn rejects_convoke_colorless_tap_when_native_ability_still_covers_colored_demand() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::Blue],
                generic: 2,
            },
        )));
        let object_id = ObjectId(901);
        let decision =
            improvise_mana_payment_decision(vec![native_blue_tap_candidate(object_id)], object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = convoke_candidate_ctx(&state, &decision, &config, &context);
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    /// The sibling native-ability tap for the same permanent must stay
    /// `Allow` — the fix removes only the redundant Colorless path, not the
    /// source's usability.
    #[test]
    fn allows_native_tap_when_colorless_marker_is_gated() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::Blue],
                generic: 2,
            },
        )));
        let object_id = ObjectId(901);
        let decision =
            improvise_mana_payment_decision(vec![native_blue_tap_candidate(object_id)], object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &decision.candidates[1],
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// Once colored demand is satisfied (a generic-only remaining cost), the
    /// Colorless marker is fine again — this isn't a blanket ban on
    /// Improvise/Convoke for dual-purpose permanents.
    #[test]
    fn allows_convoke_colorless_tap_once_colored_demand_is_satisfied() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: Vec::new(),
                generic: 3,
            },
        )));
        let object_id = ObjectId(901);
        let decision =
            improvise_mana_payment_decision(vec![native_blue_tap_candidate(object_id)], object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = convoke_candidate_ctx(&state, &decision, &config, &context);
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// A permanent with no sibling native colored option (a plain
    /// non-mana-producing artifact) is unaffected by the gate — it's scoped
    /// to true tap-channel dominance, not all Colorless taps.
    #[test]
    fn allows_convoke_colorless_tap_on_permanent_with_no_native_colored_option() {
        let mut state = GameState::new_two_player(42);
        state.pending_cast = Some(Box::new(PendingCast::new(
            ObjectId(900),
            CardId(900),
            ResolvedAbility::new(
                Effect::Draw {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(900),
                P0,
            ),
            ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::Blue],
                generic: 2,
            },
        )));
        let object_id = ObjectId(901);
        let decision = improvise_mana_payment_decision(Vec::new(), object_id);
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = convoke_candidate_ctx(&state, &decision, &config, &context);
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// CR 702.51a: the production Convoke candidate path (`mana_payment_actions`
    /// via `candidate_actions_broad`) offers a Colorless marker AND a matching
    /// colored marker for the same creature when its color is in the cost. The
    /// Improvise-only tests above build a synthetic native-mana sibling and
    /// never exercise this real Convoke-generated pair -- confirmed missing by
    /// review on #6840: `sibling_native_tap_pays_demand` didn't recognize a
    /// colored `TapForConvoke` on the same object as a dominating sibling, so a
    /// real Convoke spell with a colored pip could still dead-end.
    #[test]
    fn rejects_convoke_colorless_tap_when_real_convoke_colored_sibling_covers_demand() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P0, "Convoke Creature", 2, 2).id();
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.objects.get_mut(&creature).unwrap().color =
                vec![engine::types::mana::ManaColor::Blue];
            state.pending_cast = Some(Box::new(PendingCast::new(
                ObjectId(900),
                CardId(900),
                ResolvedAbility::new(
                    Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                        target: TargetFilter::Controller,
                    },
                    Vec::new(),
                    ObjectId(900),
                    P0,
                ),
                ManaCost::Cost {
                    shards: vec![engine::types::mana::ManaCostShard::Blue],
                    generic: 1,
                },
            )));
            state.waiting_for = WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: Some(engine::types::game_state::ConvokeMode::Convoke),
            };
        }
        let state = runner.state();
        let candidates = engine::ai_support::candidate_actions_broad(state);
        let colorless = candidates
            .iter()
            .find(|c| {
                matches!(
                    c.action,
                    GameAction::TapForConvoke {
                        object_id,
                        mana_type: ManaType::Colorless,
                    } if object_id == creature
                )
            })
            .expect("production candidate path must offer the Colorless convoke tap")
            .clone();
        let colored = candidates
            .iter()
            .find(|c| {
                matches!(
                    c.action,
                    GameAction::TapForConvoke {
                        object_id,
                        mana_type: ManaType::Blue,
                    } if object_id == creature
                )
            })
            .expect("production candidate path must offer the matching colored convoke tap")
            .clone();

        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: candidates.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);

        let colorless_ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colorless,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&colorless_ctx), GateDecision::Reject);

        let colored_ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colored,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&colored_ctx), GateDecision::Allow);
    }

    /// Builds an Improvise-eligible artifact with one `Activated` mana ability
    /// producing Blue under the given `cost`, plus a `{1}{U}` pending cast, and
    /// returns the production `ManaPayment` candidates
    /// (`candidate_actions_broad` -> `mana_payment_actions`) for it.
    fn improvise_artifact_with_mana_ability_candidates(
        cost: Option<engine::types::ability::AbilityCost>,
    ) -> (
        engine::game::scenario::GameRunner,
        ObjectId,
        Vec<CandidateAction>,
    ) {
        let mut scenario = GameScenario::new();
        let artifact = scenario.add_creature(P0, "Improvise Artifact", 0, 0).id();
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            let mut mana_ability = engine::types::ability::AbilityDefinition::new(
                engine::types::ability::AbilityKind::Activated,
                Effect::Mana {
                    produced: engine::types::ability::ManaProduction::Fixed {
                        colors: vec![engine::types::mana::ManaColor::Blue],
                        contribution: engine::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            );
            mana_ability.cost = cost;
            std::sync::Arc::make_mut(&mut obj.abilities).push(mana_ability);

            state.pending_cast = Some(Box::new(PendingCast::new(
                ObjectId(900),
                CardId(900),
                ResolvedAbility::new(
                    Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 0 },
                        target: TargetFilter::Controller,
                    },
                    Vec::new(),
                    ObjectId(900),
                    P0,
                ),
                ManaCost::Cost {
                    shards: vec![engine::types::mana::ManaCostShard::Blue],
                    generic: 1,
                },
            )));
            state.waiting_for = WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: Some(engine::types::game_state::ConvokeMode::Improvise),
            };
        }
        let candidates = engine::ai_support::candidate_actions_broad(runner.state());
        (runner, artifact, candidates)
    }

    fn find_colorless_convoke_candidate(
        candidates: &[CandidateAction],
        object_id: ObjectId,
    ) -> CandidateAction {
        candidates
            .iter()
            .find(|c| {
                matches!(
                    c.action,
                    GameAction::TapForConvoke {
                        object_id: o,
                        mana_type: ManaType::Colorless,
                    } if o == object_id
                )
            })
            .expect("production candidate path must offer the Colorless convoke tap")
            .clone()
    }

    /// Review finding on #6840: a tapless mana ability (e.g. a
    /// sacrifice-based one) on the SAME permanent as the Colorless Improvise
    /// marker does not compete for the tap -- both can legally be used in the
    /// same payment (Colorless first, then sacrifice the permanent for its
    /// ability), so it must not gate the Colorless action. Drives the real
    /// production `ManaPayment` candidate set, not a synthetic sibling.
    #[test]
    fn allows_colorless_improvise_tap_when_sibling_mana_ability_is_tapless() {
        let (runner, artifact, candidates) = improvise_artifact_with_mana_ability_candidates(Some(
            engine::types::ability::AbilityCost::Sacrifice(
                engine::types::ability::SacrificeCost::count(TargetFilter::Any, 1),
            ),
        ));
        let state = runner.state();
        let colorless = find_colorless_convoke_candidate(&candidates, artifact);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: candidates.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colorless,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Allow);
    }

    /// Regression guard for the fix above: a genuine tap-cost native mana
    /// ability on the same permanent still gates the Colorless marker, via
    /// the real production candidate path.
    #[test]
    fn rejects_colorless_improvise_tap_when_sibling_mana_ability_taps() {
        let (runner, artifact, candidates) = improvise_artifact_with_mana_ability_candidates(Some(
            engine::types::ability::AbilityCost::Tap,
        ));
        let state = runner.state();
        let colorless = find_colorless_convoke_candidate(&candidates, artifact);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: candidates.clone(),
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &colorless,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    // ---------------------------------------------------------------------
    // S7: temporary-combat-modifier ACTIVATIONS outside a live combat window.
    // ---------------------------------------------------------------------

    /// Desolation Prowler's real Oracle text, verified against
    /// `data/card-data.json`: it parses to `Pump { Fixed 2, Fixed 2, SelfRef }`
    /// with cost `PayLife 2`, `duration: UntilEndOfTurn` and restriction
    /// `OnlyOnceEachTurn`.
    const PROWLER_ORACLE: &str =
        "Pay 2 life: This creature gets +2/+2 until end of turn. Activate only once each turn.";

    /// Nantuko Husk's real Oracle text (sacrifice outlet — the payoff IS the
    /// cost, so the pump window must not gate it).
    const HUSK_ORACLE: &str = "Sacrifice a creature: This creature gets +2/+2 until end of turn.";

    /// Shivan Dragon's firebreathing (repeatable mana sink).
    const FIREBREATHING_ORACLE: &str = "{R}: This creature gets +1/+0 until end of turn.";

    /// Non-vacuity guard: every fixture below depends on its Oracle text still
    /// parsing to an "until end of turn" pump. Without this, a parser change
    /// would silently turn the `assert_ne!(.., Reject)` tests green for the
    /// wrong reason.
    fn assert_parses_as_temporary_pump(state: &GameState, source_id: ObjectId) {
        let ability = &state.objects.get(&source_id).unwrap().abilities[0];
        assert!(
            ability_is_temporary_combat_modifier(ability),
            "fixture Oracle text no longer parses to an until-end-of-turn pump"
        );
    }

    fn gate_activation(state: &GameState, source_id: ObjectId) -> GateDecision {
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Ability),
        };
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assess_candidate(&ctx)
    }

    /// Put the AI at priority in `phase` on `active_player`'s turn.
    fn set_priority_window(state: &mut GameState, phase: Phase, active_player: PlayerId) {
        state.phase = phase;
        state.active_player = active_player;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    /// CR 514.2: the +2/+2 ends at cleanup, and no combat or stack window can
    /// consume it from the opponent's end step — the 2 life buys nothing.
    #[test]
    fn activated_ueot_pump_rejected_at_opponents_end_step() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::End, P1);

        assert_parses_as_temporary_pump(state, prowler);
        assert_eq!(gate_activation(state, prowler), GateDecision::Reject);
    }

    #[test]
    fn activated_ueot_pump_rejected_in_own_postcombat_main_with_no_combat() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::PostCombatMain, P0);

        assert_parses_as_temporary_pump(state, prowler);
        assert_eq!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// After blockers are declared, a 2/2 blocked by a 3/3 dies and kills
    /// nothing; +2/+2 flips both halves of the exchange, so the activation is a
    /// real play and must reach scoring.
    #[test]
    fn activated_ueot_pump_allowed_after_blocks_when_it_changes_the_outcome() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let blocker = scenario.add_creature(P1, "Blocker", 3, 3).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(prowler, P1)],
            blocker_assignments: [(prowler, vec![blocker])].into_iter().collect(),
            blocker_to_attacker: [(blocker, vec![prowler])].into_iter().collect(),
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, prowler);
        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// The mirror of the case above, and the proof that the new branch is a
    /// real gate rather than a blanket allow inside combat: blocked by a 6/6,
    /// +2/+2 neither saves the Prowler nor kills the blocker, so the 2 life is
    /// still wasted.
    #[test]
    fn activated_ueot_pump_rejected_after_blocks_when_outcome_is_unchanged() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let blocker = scenario.add_creature(P1, "Colossus", 6, 6).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(prowler, P1)],
            blocker_assignments: [(prowler, vec![blocker])].into_iter().collect(),
            blocker_to_attacker: [(blocker, vec![prowler])].into_iter().collect(),
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, prowler);
        assert_eq!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// A pump whose definition carries no `UntilEndOfTurn` duration outlives
    /// the combat window, so "no live window" proves nothing about its value.
    #[test]
    fn activated_pump_without_ueot_duration_not_gated() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::End, P1);
        let object = state.objects.get_mut(&prowler).unwrap();
        Arc::make_mut(&mut object.abilities)[0].duration = None;

        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// The reactive exception carried over from the spell path: with a 3-damage
    /// burn spell on the stack aimed at the 2/2 Prowler, +2/+2 saves it.
    #[test]
    fn activated_pump_in_response_to_burn_allowed() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::PostCombatMain, P1);
        state.stack.push_back(StackEntry {
            id: ObjectId(300),
            source_id: ObjectId(301),
            controller: P1,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: engine::types::ability::QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::Any,
                        damage_source: None,
                        excess: None,
                    },
                    vec![TargetRef::Object(prowler)],
                    ObjectId(301),
                    P1,
                ))),
                card_id: CardId(301),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert_parses_as_temporary_pump(state, prowler);
        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// Sacrifice-outlet shape (Nantuko Husk): the sacrifice IS the payoff, and
    /// `free_outlet_activation` / `self_cost_value` own that judgment, so the
    /// pump window must not veto it even at the opponent's end step.
    #[test]
    fn activated_sacrifice_outlet_pump_not_gated() {
        let mut scenario = GameScenario::new();
        let husk = scenario
            .add_creature_from_oracle(P0, "Nantuko Husk", 2, 2, HUSK_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::End, P1);

        assert_parses_as_temporary_pump(state, husk);
        assert_ne!(gate_activation(state, husk), GateDecision::Reject);
    }

    /// A sorcery-speed-only pump has no later, stronger window to be saved
    /// for, so "pass and use it in combat instead" is not available advice.
    /// No printed card carries this shape today, so the restriction is
    /// synthesized onto the Prowler ability.
    #[test]
    fn activated_as_sorcery_pump_not_gated() {
        let mut scenario = GameScenario::new();
        let prowler = scenario
            .add_creature_from_oracle(P0, "Desolation Prowler", 2, 2, PROWLER_ORACLE)
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::PostCombatMain, P0);
        let object = state.objects.get_mut(&prowler).unwrap();
        Arc::make_mut(&mut object.abilities)[0]
            .activation_restrictions
            .push(ActivationRestriction::AsSorcery);

        assert_parses_as_temporary_pump(state, prowler);
        assert_ne!(gate_activation(state, prowler), GateDecision::Reject);
    }

    /// CR 509.1h: with no blocker declared for it the Dragon is an unblocked
    /// creature, and CR 510.1b assigns its combat damage to the defending
    /// player — so a repeatable mana sink still pays off even when it changes
    /// no combat outcome the gate can prove. How much that mana is worth is a
    /// policy judgment, so the activated branch stands down here.
    #[test]
    fn firebreathing_unblocked_attacker_is_not_gated() {
        let mut scenario = GameScenario::new();
        let dragon = scenario
            .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, FIREBREATHING_ORACLE)
            .id();
        scenario.with_life(P1, 20);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(dragon, P1)],
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, dragon);
        assert_ne!(gate_activation(state, dragon), GateDecision::Reject);
    }

    /// CR 510.3: the active player gets priority again after combat damage is
    /// dealt, still inside a window that maps to `CombatAfterBlocks`. The same
    /// unblocked attacker has already connected, so the mana-sink stand-down
    /// must not apply — pumping now is pure waste.
    #[test]
    fn activated_pump_on_unblocked_attacker_after_damage_is_rejected() {
        let mut scenario = GameScenario::new();
        let dragon = scenario
            .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, FIREBREATHING_ORACLE)
            .id();
        scenario.with_life(P1, 20);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        set_priority_window(state, Phase::DeclareBlockers, P0);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(dragon, P1)],
            regular_damage_done: true,
            ..Default::default()
        });

        assert_parses_as_temporary_pump(state, dragon);
        assert_eq!(gate_activation(state, dragon), GateDecision::Reject);
    }
}
