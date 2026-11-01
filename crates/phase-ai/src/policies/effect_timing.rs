use engine::ai_support::current_target_selection_targets;
use engine::game::combat::{
    attacker_blockability_in_maximum_free_declaration, attacker_declaration_pending_for,
    defending_player_for_attacker, is_on_attacking_team, MaximumBlockDeclarationBlockability,
};
use engine::game::{players, turn_control};
use engine::types::ability::{
    ContinuousModification, Duration, Effect, StaticDefinition, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, StackEntry, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::eval::StrategicIntent;
use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::{collect_ability_effects, PolicyContext};
use super::effect_classify::{
    effect_is_combat_trick_payload, extract_target_filter, precombat_main_is_last_trick_window,
    spell_resolution_effects, targets_creatures_only, CombatBinding, TimingSubject,
};
use super::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy, STRONG_MAX,
};
use super::stack_awareness::{
    assess_spell_impact, foreign_counter_target_of_ai, COUNTER_BREAK_EVEN_IMPACT,
    COUNTER_IMPACT_THRESHOLD,
};
use super::strategy_helpers::{
    targetable_threat_value, untapped_opponent_blocker_value, visible_opponent_creature_value,
};
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

pub struct EffectTimingPolicy;

impl EffectTimingPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let mut score = score_action_shape(ctx);

        for effect in ctx.effects() {
            score += match effect {
                Effect::Destroy { .. } => removal_score(ctx),
                Effect::DealDamage { .. } => burn_score(ctx),
                Effect::Counter { .. } => counterspell_score(ctx),
                _ => 0.0,
            };
        }

        // CR 611.2a + CR 514.2: a combat-window payload is priced only when the candidate itself
        // resolves it, so a permanent spell is not timed by what its abilities do later, and it is
        // priced once per candidate. A payload beside value that lands elsewhere keeps the combat
        // window and takes no penalty out of it: waiting would cost the rest.
        score += match CombatBinding::of_candidate(ctx, &own_resolution_effects(ctx)) {
            CombatBinding::Whole => combat_trick_score(ctx),
            CombatBinding::Partial => combat_trick_score(ctx).max(0.0),
            CombatBinding::Unbound => 0.0,
        };

        score
    }
}

/// The effects the candidate itself resolves. A cast spell resolves its spell abilities
/// ([`spell_resolution_effects`]); every other candidate's [`PolicyContext::effects`] is
/// already the one ability it activates, chooses modes for, or targets with.
fn own_resolution_effects<'a>(ctx: &PolicyContext<'a>) -> Vec<&'a Effect> {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } => ctx
            .cast_facts()
            .map(|facts| spell_resolution_effects(&facts))
            .unwrap_or_default(),
        _ => ctx.effects(),
    }
}

impl TacticalPolicy for EffectTimingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::EffectTiming
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[
            DecisionKind::PlayLand,
            DecisionKind::CastSpell,
            DecisionKind::ActivateAbility,
            DecisionKind::SelectTarget,
        ]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        if matches!(
            ctx.decision.waiting_for,
            WaitingFor::TargetSelection { .. }
                | WaitingFor::TriggerTargetSelection { .. }
                | WaitingFor::MultiTargetSelection { .. }
                | WaitingFor::CopyRetarget { .. }
                | WaitingFor::RetargetChoice { .. }
                | WaitingFor::DistributeAmong { .. }
                | WaitingFor::MoveCountersDistribution { .. }
                | WaitingFor::RemoveCountersChoice { .. }
        ) {
            return match &ctx.candidate.action {
                GameAction::ChooseTarget {
                    target: Some(target),
                } => evasion_target_verdict(ctx, target),
                _ => PolicyVerdict::neutral(PolicyReason::new(
                    "effect_timing_unsupported_target_selection",
                )),
            };
        }

        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("effect_timing_score"),
        }
    }
}

/// Scores only an ordinary, one-slot activated ability that grants its creature
/// target bare, until-end-of-turn unblockability.
fn evasion_target_verdict(ctx: &PolicyContext<'_>, target: &TargetRef) -> PolicyVerdict {
    if !is_single_target_unblockable_activation(ctx) {
        return PolicyVerdict::neutral(PolicyReason::new("effect_timing_evasion_target_na"));
    }
    let TargetRef::Object(target_id) = target else {
        return PolicyVerdict::neutral(PolicyReason::new("effect_timing_evasion_target_na"));
    };

    // CR 509.1b + CR 508.1a: a grant that cannot reach a declare-blockers step
    // is wasted no matter which phase it was activated in, so this runs ahead
    // of the declare-attackers gate below rather than inside it.
    if target_cannot_attack_this_turn(ctx.state, ctx.ai_player, *target_id) {
        return PolicyVerdict::strong(
            -STRONG_MAX,
            PolicyReason::new("effect_timing_futile_unattacking_evasion_target"),
        );
    }

    if !matches!(ctx.state.phase, Phase::DeclareAttackers) || prompt_has_team_defender(ctx.state) {
        return PolicyVerdict::neutral(PolicyReason::new("effect_timing_evasion_target_na"));
    }

    if !matches!(
        ai_controlled_declared_attacker_blockability(ctx.state, ctx.ai_player, *target_id),
        MaximumBlockDeclarationBlockability::NotBlockable
    ) {
        return PolicyVerdict::neutral(PolicyReason::new(
            "effect_timing_pair_blockable_evasion_target",
        ));
    }

    if current_target_selection_targets(ctx.state)
        .into_iter()
        .flatten()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .any(|sibling| {
            matches!(
                ai_controlled_declared_attacker_blockability(ctx.state, ctx.ai_player, sibling),
                MaximumBlockDeclarationBlockability::Blockable
            )
        })
    {
        PolicyVerdict::strong(
            -STRONG_MAX,
            PolicyReason::new("effect_timing_futile_evasion_target"),
        )
    } else {
        PolicyVerdict::neutral(PolicyReason::new(
            "effect_timing_no_pair_blockable_evasion_target",
        ))
    }
}

fn is_single_target_unblockable_activation(ctx: &PolicyContext<'_>) -> bool {
    let WaitingFor::TargetSelection {
        pending_cast,
        target_slots,
        selection,
        ..
    } = &ctx.decision.waiting_for
    else {
        return false;
    };
    if pending_cast.activation_ability_index.is_none()
        || pending_cast.activation_cost.is_none()
        || !matches!(target_slots.as_slice(), [_])
        || selection.current_slot != 0
        || target_slots[0].optional
        || target_slots[0].chooser.is_some()
        || !matches!(
            pending_cast.ability.kind,
            engine::types::ability::AbilityKind::Activated
        )
        || pending_cast.ability.condition.is_some()
        || pending_cast.ability.optional_targeting
        || pending_cast.ability.optional
        || pending_cast.ability.optional_player.is_some()
        || pending_cast.ability.optional_for.is_some()
        || pending_cast.ability.multi_target.is_some()
        || !pending_cast.ability.target_constraints.is_empty()
    {
        return false;
    }

    let effects = collect_ability_effects(&pending_cast.ability);
    let [Effect::GenericEffect {
        static_abilities,
        duration: Some(Duration::UntilEndOfTurn),
        target: Some(_),
        end_cost: None,
    }] = effects.as_slice()
    else {
        return false;
    };
    targets_creatures_only(effects[0])
        && extract_target_filter(effects[0]).is_some()
        && matches!(static_abilities.as_slice(), [static_ability] if bare_cant_be_blocked(static_ability))
}

/// Ignores display text, but rejects every non-default field that changes a
/// static definition's applicability or semantics.
fn bare_cant_be_blocked(static_ability: &StaticDefinition) -> bool {
    matches!(
        static_ability,
        StaticDefinition {
            mode: engine::types::statics::StaticMode::CantBeBlocked,
            affected: Some(engine::types::ability::TargetFilter::ParentTarget),
            modifications,
            condition: None,
            per_player_condition: None,
            affected_zone: None,
            effect_zone: None,
            active_zones,
            characteristic_defining: false,
            attack_defended: None,
            source_controller: None,
            source_object: None,
            bypass_beneficiary: None,
            protection_does_not_remove: None,
            room_door: None,
            ..
        } if active_zones.is_empty()
            && matches!(
                modifications.as_slice(),
                [ContinuousModification::AddStaticMode {
                    mode: engine::types::statics::StaticMode::CantBeBlocked,
                }]
            )
    )
}

/// CR 805.10d: a defending team declares blockers together, so the ordinary
/// blocker map's exact-controller relation is insufficient when a live teammate
/// could also block. `teammates` excludes the queried player and dead teammates.
fn prompt_has_team_defender(state: &GameState) -> bool {
    let Some(combat) = state.combat.as_ref() else {
        return false;
    };
    current_target_selection_targets(state)
        .into_iter()
        .flatten()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .filter_map(|target_id| {
            combat
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == target_id)
        })
        .any(|attacker| !players::teammates(state, attacker.defending_player).is_empty())
}

/// CR 509.1b-c: in a non-team game, whether the target attacker can appear in
/// some tax-free maximum-requirement declaration for its defending player.
fn declared_attacker_blockability(
    state: &GameState,
    target_id: engine::types::identifiers::ObjectId,
) -> MaximumBlockDeclarationBlockability {
    defending_player_for_attacker(state, target_id).map_or(
        MaximumBlockDeclarationBlockability::NotBlockable,
        |defender| attacker_blockability_in_maximum_free_declaration(state, defender, target_id),
    )
}

fn ai_controlled_declared_attacker_blockability(
    state: &GameState,
    ai_player: PlayerId,
    target_id: engine::types::identifiers::ObjectId,
) -> MaximumBlockDeclarationBlockability {
    if state
        .objects
        .get(&target_id)
        .is_some_and(|object| object.controller == ai_player)
    {
        declared_attacker_blockability(state, target_id)
    } else {
        MaximumBlockDeclarationBlockability::NotBlockable
    }
}

/// CR 509.1b: "can't be blocked" is an evasion restriction, and a restriction is
/// only ever checked when blockers are declared against an attacking creature.
/// A target that cannot be an attacker in this turn's combat therefore gains
/// nothing from the grant, in any phase — which is why this guard sits ahead of
/// the declare-attackers phase gate rather than behind it.
///
/// Gated on `is_on_attacking_team(state, ai_player)` first: the guard suppresses
/// itself on turns when the AI's own team is not the attacking team, since
/// `attacker_declaration_pending_for` answers only about the team that would
/// attack this turn (CR 508.1a + CR 805.10a) and says nothing about a
/// non-attacking-team creature's ability to attack a future turn's combat.
///
/// The per-creature question goes to `combat::attacker_declaration_pending_for`,
/// never to `tapped`: that authority is the single place where every declaration
/// still ahead this turn — the current phase's and each scheduled combat's, each
/// under its own CR 508.1c restriction — is checked against this one object.
///
/// CR 508.1k + CR 511.3: a creature that is currently an attacking creature in
/// the combat in progress is exempt — the exemption reads the live
/// `CombatState`, which CR 511.3 empties at end of combat.
fn target_cannot_attack_this_turn(
    state: &GameState,
    ai_player: PlayerId,
    target_id: engine::types::identifiers::ObjectId,
) -> bool {
    is_on_attacking_team(state, ai_player)
        && defending_player_for_attacker(state, target_id).is_none()
        && !attacker_declaration_pending_for(state, target_id)
}

fn score_action_shape(ctx: &PolicyContext<'_>) -> f64 {
    match &ctx.candidate.action {
        GameAction::PlayLand { .. } => 1.0,
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => {
            let Some(object) = ctx.source_object() else {
                return 0.0;
            };

            let mut score = 0.0;

            let is_pre_combat_preferred =
                object.card_types.core_types.contains(&CoreType::Creature)
                    || object.card_types.subtypes.iter().any(|s| s == "Aura");
            if is_pre_combat_preferred {
                if matches!(ctx.state.phase, Phase::PreCombatMain) {
                    score += 0.35;

                    // Haste creatures get extra pre-combat bonus — can attack immediately
                    if object.has_keyword(&Keyword::Haste)
                        && object.card_types.core_types.contains(&CoreType::Creature)
                    {
                        score += 0.2;
                    }
                } else {
                    score += 0.1;
                }
            }

            // Removal pre-combat bonus: opens combat lanes by removing blockers.
            // Uses effect_profile so activated removal abilities also benefit.
            // Only applies when untapped creatures exist — tapped creatures can't block.
            if matches!(ctx.state.phase, Phase::PreCombatMain) {
                if let Some(profile) = ctx.effect_profile() {
                    if profile.has_direct_removal_text
                        && untapped_opponent_blocker_value(ctx.state, ctx.ai_player) > 0.0
                    {
                        score += 0.2;
                    }
                }
            }

            // Draw post-combat bonus: draw after combat decisions are resolved
            if matches!(ctx.state.phase, Phase::PostCombatMain) {
                if let Some(profile) = ctx.effect_profile() {
                    if profile.has_draw {
                        score += 0.15;
                    }
                }
            }

            score
        }
        _ => 0.0,
    }
}

fn removal_score(ctx: &PolicyContext<'_>) -> f64 {
    // If the spell exclusively targets creatures, only consider creatures it can hit.
    // For broad/non-creature removal (Vindicate, "destroy target enchantment"), fall
    // back to all opponent creatures — targetable_threat_value only evaluates creatures
    // and would return 0.0 for non-creature-exclusive filters.
    let effects = ctx.effects();
    let max_threat = if let Some(source) = ctx.source_object() {
        let creature_filter = effects
            .iter()
            .filter(|e| targets_creatures_only(e))
            .find_map(|e| extract_target_filter(e));
        if let Some(filter) = creature_filter {
            targetable_threat_value(ctx.state, ctx.ai_player, filter, source.id)
        } else {
            all_opponent_creature_threat(ctx)
        }
    } else {
        all_opponent_creature_threat(ctx)
    };

    let stabilize_bonus = if matches!(ctx.strategic_intent(), StrategicIntent::Stabilize) {
        0.25
    } else {
        0.0
    };

    // Incentivize casting removal now when opponent has pump spells on the stack —
    // killing the pumped creature wastes both the creature and the pump (2-for-1).
    // This asks whether a pump is on the stack, not whether combat is its whole value, so it
    // reads any payload on the entry.
    let pump_response = if !ctx.state.stack.is_empty()
        && ctx.state.stack.iter().any(|entry| {
            entry.controller != ctx.ai_player
                && entry
                    .ability()
                    .map(|a| {
                        collect_ability_effects(a)
                            .iter()
                            .any(|e| effect_is_combat_trick_payload(e))
                    })
                    .unwrap_or(false)
        }) {
        0.5
    } else {
        0.0
    };

    0.3 + (max_threat / 25.0).min(0.8) + stabilize_bonus + pump_response
}

/// Fallback: max threat across all opponent creatures (no filter applied).
fn all_opponent_creature_threat(ctx: &PolicyContext<'_>) -> f64 {
    visible_opponent_creature_value(ctx.state, ctx.ai_player)
}

fn burn_score(ctx: &PolicyContext<'_>) -> f64 {
    let lethal_bias = if matches!(ctx.strategic_intent(), StrategicIntent::PushLethal) {
        0.35
    } else {
        0.0
    };

    removal_score(ctx) + lethal_bias
}

fn counterspell_score(ctx: &PolicyContext<'_>) -> f64 {
    let is_own_turn = turn_control::turn_decision_maker(ctx.state) == ctx.ai_player;
    let patience = ctx.config.profile.interaction_patience;
    let intent_bonus = match ctx.strategic_intent() {
        StrategicIntent::PreserveAdvantage => 0.15,
        StrategicIntent::Stabilize => 0.2,
        _ => 0.0,
    };

    // Creature spells on the stack represent recurring damage — urgency to counter
    // scales with existing opponent board pressure (each additional creature compounds).
    let creature_urgency = if !ctx.state.stack.is_empty() {
        let has_creature_on_stack = ctx.state.stack.iter().any(|entry| {
            entry.controller != ctx.ai_player
                && ctx.state.objects.get(&entry.source_id).is_some_and(|obj| {
                    obj.card_types
                        .core_types
                        .contains(&engine::types::card_type::CoreType::Creature)
                })
        });
        if has_creature_on_stack {
            let opponent_creatures = ctx
                .state
                .battlefield
                .iter()
                .filter(|&&id| {
                    ctx.state.objects.get(&id).is_some_and(|obj| {
                        obj.controller != ctx.ai_player
                            && obj
                                .card_types
                                .core_types
                                .contains(&engine::types::card_type::CoreType::Creature)
                    })
                })
                .count();
            // Base urgency + scaling per existing creature
            0.3 + 0.1 * (opponent_creatures as f64).min(3.0)
        } else {
            0.0
        }
    } else {
        0.0
    };

    // CR 601.2c: a counter's target is chosen while it is being cast, so the cast
    // decision is the last point at which the AI can decline — scale the stack
    // bracket by what the counter would actually hit rather than by the mere
    // presence of a stack.
    let best_impact = best_counter_impact(ctx);
    let stack_pressure = if best_impact < COUNTER_BREAK_EVEN_IMPACT {
        // Nothing on the stack is worth the card the counter itself costs.
        0.0
    } else {
        let impact_factor = ((best_impact - COUNTER_BREAK_EVEN_IMPACT)
            / (COUNTER_IMPACT_THRESHOLD - COUNTER_BREAK_EVEN_IMPACT))
            .clamp(0.0, 1.0);
        impact_factor * ((0.8 * patience) + intent_bonus) + creature_urgency
    };

    // Boost incentive to cast a counter when opponent is countering one of our spells
    let protect_bonus = threatened_own_spell_value(ctx.state, ctx.ai_player)
        * ctx.penalties().protect_spell_bonus_mult;

    if matches!(ctx.decision.waiting_for, WaitingFor::Priority { .. }) {
        if !is_own_turn && stack_pressure > 0.0 {
            stack_pressure + protect_bonus
        } else if protect_bonus > 0.0 {
            // Even on own turn, protect a threatened spell
            protect_bonus
        } else {
            -0.6 * patience
        }
    } else {
        stack_pressure + protect_bonus
    }
}

/// What countering `entry` is worth to the AI, on the [`assess_spell_impact`]
/// scale. A foreign counter is worth only the AI spell it threatens (CR 701.6a —
/// countering it un-cancels that spell); everything else is worth its own impact.
fn counter_target_worth(ctx: &PolicyContext<'_>, entry: &StackEntry) -> f64 {
    foreign_counter_target_of_ai(ctx.state, entry, ctx.ai_player)
        .unwrap_or_else(|| assess_spell_impact(ctx.state, entry))
}

/// Best impact a counter cast right now could remove: the maximum worth over
/// every stack entry the AI does not control.
///
/// Approximation: the counter's own target filter is not applied (the engine
/// matcher is not reachable from here), so a counter that can only hit
/// noncreature spells still sees a creature entry's impact. Accepted — the
/// filter narrows the set, so this can only over-estimate. Target selection
/// picks legally, but "legally" can mean the AI's own spell when that is all
/// the filter reaches; that cast is vetoed in `tactical_gate`
/// (`counter_reaches_nothing_foreign`), not here.
fn best_counter_impact(ctx: &PolicyContext<'_>) -> f64 {
    ctx.state
        .stack
        .iter()
        .filter(|entry| entry.controller != ctx.ai_player)
        .map(|entry| counter_target_worth(ctx, entry))
        .fold(0.0_f64, f64::max)
}

/// Check if any opponent counter spell on the stack threatens one of the AI's spells.
/// Returns the impact value of the most valuable threatened spell, or 0.0 if none.
fn threatened_own_spell_value(state: &GameState, ai_player: PlayerId) -> f64 {
    state
        .stack
        .iter()
        .filter(|entry| entry.controller != ai_player)
        .filter_map(|entry| foreign_counter_target_of_ai(state, entry, ai_player))
        .fold(0.0_f64, f64::max)
}

fn combat_trick_score(ctx: &PolicyContext<'_>) -> f64 {
    // CR 514.2: until-end-of-turn effects end in the cleanup step.
    // Pump effects expire at cleanup — casting outside combat has no lasting impact.
    // Penalty must exceed max search continuation bonus to prevent selection.
    if matches!(
        ctx.state.phase,
        Phase::End | Phase::Cleanup | Phase::Untap | Phase::Upkeep | Phase::Draw
    ) {
        return -2.0;
    }

    // CR 505.1a + CR 307.1 + CR 307.5: a payload the candidate can deliver only at sorcery
    // speed has no later window before this turn's combat, and it lasts through that combat
    // (CR 611.2a + CR 514.2), so timing has nothing to price. Whether it is worth casting is
    // the continuation's and the other policies' question.
    if TimingSubject::of_candidate(ctx).is_some_and(|subject| {
        precombat_main_is_last_trick_window(ctx.state, ctx.ai_player, subject)
    }) {
        return 0.0;
    }

    // Main phases with no active combat: in the precombat main an instant-speed trick can wait
    // for combat, and a postcombat main usually has the turn's combat behind it (CR 505.1a); an
    // added combat phase (CR 500.8) is not read here.
    // Apply a strong penalty that overrides other positive signals.
    if matches!(
        ctx.state.phase,
        Phase::PreCombatMain | Phase::PostCombatMain
    ) && ctx.state.combat.is_none()
    {
        return -2.0;
    }

    let patience = ctx.config.profile.interaction_patience;
    let intent_bonus = match ctx.strategic_intent() {
        StrategicIntent::PushLethal => 0.2,
        StrategicIntent::PreserveAdvantage => 0.1,
        _ => 0.0,
    };
    if matches!(
        ctx.state.phase,
        Phase::BeginCombat | Phase::DeclareAttackers | Phase::DeclareBlockers | Phase::CombatDamage
    ) {
        (0.8 * patience.max(0.5)) + intent_bonus
    } else {
        // EndCombat or any unrecognized phase — mild penalty
        -0.5 * patience
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{
        build_decision_context, validated_candidate_actions_for_semantic_owner, ActionMetadata,
        AiDecisionContext, CandidateAction, TacticalClass,
    };
    use engine::game::combat::{
        get_valid_attacker_ids, get_valid_block_targets, AttackTarget, AttackerInfo, CombatState,
    };
    use engine::game::layers::evaluate_layers;
    use engine::game::restrictions::{
        ability_confined_to_sorcery_timing, spell_confined_to_sorcery_timing,
    };
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification,
        ControllerRef, Duration, ExtraPhaseAnchor, ExtraPhaseRecipient, MultiTargetSpec,
        ResolvedAbility, StaticDefinition, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{
        ExtraPhase, GameState, PendingCast, StackEntryKind, TargetEffectDetail,
        TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ExtraPhaseId, ObjectId};
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use engine::types::phase::{PhaseGroup, TurnSegment};
    use engine::types::player::PlayerId;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    /// The AI's seat in the counterspell fixtures; every other seat is foreign.
    const AI: PlayerId = PlayerId(1);

    fn cant_be_blocked_ability(
        affected: TargetFilter,
        target: Option<TargetFilter>,
        cost: AbilityCost,
    ) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::CantBeBlocked)
                    .affected(affected)
                    .modifications(vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantBeBlocked,
                    }])],
                duration: Some(Duration::UntilEndOfTurn),
                target,
                end_cost: None,
            },
        )
        .cost(cost)
    }

    fn whirler_ability() -> AbilityDefinition {
        cant_be_blocked_ability(
            TargetFilter::ParentTarget,
            Some(TargetFilter::Typed(TypedFilter::creature())),
            AbilityCost::TapCreatures {
                requirement: engine::types::ability::TapCreaturesRequirement::Count { count: 2 },
                filter: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                ),
            },
        )
    }

    fn effect_timing_verdict(
        state: &GameState,
        candidate: &CandidateAction,
        config: &AiConfig,
    ) -> PolicyVerdict {
        let decision = build_decision_context(state);
        let context = crate::context::AiContext::empty(&config.weights);
        crate::policies::registry::PolicyRegistry::shared()
            .verdicts(&PolicyContext {
                state,
                decision: &decision,
                candidate,
                ai_player: P0,
                config,
                context: &context,
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            })
            .into_iter()
            .find_map(|(id, verdict)| (id == PolicyId::EffectTiming).then_some(verdict))
            .expect("EffectTimingPolicy must be registered for this production decision")
    }

    fn creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    fn install_whirler_prompt(state: &mut GameState, source: ObjectId, targets: Vec<ObjectId>) {
        let mut ability = ResolvedAbility::new(*whirler_ability().effect, Vec::new(), source, P0);
        ability.kind = AbilityKind::Activated;
        let legal_targets: Vec<_> = targets.into_iter().map(TargetRef::Object).collect();
        let mut pending = PendingCast::new(source, CardId(99), ability, ManaCost::zero());
        pending.activation_ability_index = Some(0);
        pending.activation_cost = Some(AbilityCost::Tap);
        state.waiting_for = WaitingFor::TargetSelection {
            player: P0,
            pending_cast: Box::new(pending),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
                chooser: None,
                effect_kind: engine::types::ability::EffectKind::GenericEffect,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: legal_targets,
            },
        };
    }

    fn declared_attacker(object_id: ObjectId, defender: PlayerId) -> AttackerInfo {
        AttackerInfo {
            object_id,
            defending_player: defender,
            attack_target: AttackTarget::Player(defender),
            blocked: false,
            band_id: None,
        }
    }

    fn target_candidate(target: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        }
    }

    #[test]
    fn whirler_rogue_prefers_the_tapped_declared_blockable_attacker() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 2, 2)
            .with_ability_definition(whirler_ability())
            .id();
        let futile = scenario.add_creature(P0, "Tapped Sick Sibling", 3, 3).id();
        let useful = scenario.add_creature(P0, "Declared Attacker", 3, 3).id();
        let blocker = scenario
            .add_creature(PlayerId(1), "Ready Blocker", 2, 2)
            .id();
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let futile_object = state.objects.get_mut(&futile).unwrap();
        futile_object.tapped = true;
        futile_object.summoning_sick = true;
        runner.advance_to_phase(Phase::DeclareAttackers);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ));
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(useful, AttackTarget::Player(PlayerId(1)))],
                bands: Vec::new(),
            })
            .expect("reach guard: declared attacker must be engine-legal");
        let state = runner.state();
        assert!(
            state.objects[&useful].tapped,
            "declaring an attacker must tap it"
        );
        assert!(state.objects[&futile].tapped && state.objects[&futile].summoning_sick);
        assert!(state.combat.as_ref().is_some_and(|combat| {
            combat
                .attackers
                .iter()
                .any(|attacker| attacker.object_id == useful)
        }));
        assert!(
            engine::game::combat::get_valid_block_targets(state)
                .get(&blocker)
                .is_some_and(|targets| targets.contains(&useful)),
            "reach guard: the engine must identify the declared attacker as blockable"
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");
        let state = runner.state();
        let WaitingFor::TargetSelection {
            pending_cast,
            selection,
            ..
        } = &state.waiting_for
        else {
            panic!("Whirler Rogue's ordinary activation must reach TargetSelection");
        };
        assert_eq!(
            pending_cast.activation_ability_index,
            Some(0),
            "reach guard: target prompt must retain the activated-ability index"
        );
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(futile)));
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(useful)));

        let config = AiConfig::default();
        let mut rng = SmallRng::seed_from_u64(0);
        assert_eq!(
            crate::choose_action(state, P0, &config, &mut rng),
            Some(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(useful)),
            }),
            "the production chooser must rank Whirler Rogue's blockable declared attacker above a futile sibling"
        );
        let futile_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(futile)),
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        };
        assert!(matches!(
            effect_timing_verdict(state, &futile_candidate, &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0 && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
    }

    #[test]
    fn whirler_rogue_refuses_a_main_phase_evasion_grant_to_a_creature_that_cannot_attack() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 1, 1)
            .with_ability_definition(whirler_ability())
            .id();
        let futile = scenario.add_creature(P0, "Tapped Sick Body", 3, 3).id();
        let useful = scenario.add_creature(P0, "Ready Attacker", 1, 1).id();
        let new_sick = scenario
            .add_creature(P0, "Sick Rookie", 1, 1)
            .with_summoning_sickness()
            .id();
        let blocker = scenario
            .add_creature(PlayerId(1), "Ready Blocker", 2, 2)
            .id();
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let futile_object = state.objects.get_mut(&futile).unwrap();
        futile_object.tapped = true;
        futile_object.summoning_sick = true;
        // `futile` needs both `tapped` and `summoning_sick`, and there is no
        // `tapped` builder, so it is set post-`build()` here; `new_sick` uses
        // `CardBuilder::with_summoning_sickness()` above instead.

        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");
        let state = runner.state();
        assert_eq!(
            state.phase,
            Phase::PreCombatMain,
            "this pins the old phase gate as the defect"
        );
        let WaitingFor::TargetSelection {
            pending_cast,
            selection,
            ..
        } = &state.waiting_for
        else {
            panic!("Whirler Rogue's ordinary activation must reach TargetSelection");
        };
        assert_eq!(
            pending_cast.activation_ability_index,
            Some(0),
            "reach guard: target prompt must retain the activated-ability index"
        );
        assert!(
            pending_cast.activation_cost.is_some(),
            "reach guard: target prompt must retain the activation cost"
        );
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(futile)));
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(useful)));
        assert!(
            selection
                .current_legal_targets
                .contains(&TargetRef::Object(blocker)),
            "reach guard: the opponent's creature must be a production-reachable candidate, \
             not merely a synthetic candidate built by hand"
        );
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(new_sick)));
        assert_eq!(
            selection.current_legal_targets.len(),
            5,
            "pre-beam guard: a sixth legal target would truncate the root beam at \
             max_branching, dropping the lowest beam_priority candidate and silently \
             changing the ranking basis assertion 4 asserts against"
        );

        assert!(get_valid_attacker_ids(state).contains(&useful));
        assert!(!get_valid_attacker_ids(state).contains(&futile));
        assert!(!state.objects[&new_sick].tapped);
        assert!(!get_valid_attacker_ids(state).contains(&new_sick));

        let config = AiConfig::default();

        // Assertion 1 (primary revert-failing assertion): pre-fix this is
        // `Score { delta: 0.0, kind: "effect_timing_evasion_target_na" }`,
        // deterministic and independent of search and softmax.
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(futile), &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));

        // Assertion 2 (paired negative sibling / negative control): proves the
        // guard discriminates rather than blanket-penalising.
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(useful), &config),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_evasion_target_na"
        ));

        // Assertion 3 (the enumerated opponent-controlled cell, measured
        // reachable): on the AI's own turn the opponent's creature genuinely
        // cannot attack either, and `whirler_ability()` imposes no controller
        // restriction on its target.
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(blocker), &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));

        // Assertion 4 (chooser-level, deterministic).
        let mut rng = SmallRng::seed_from_u64(0); // any seed: rank/score/probability are seed-invariant here
        let selection = crate::search::choose_action_with_session_diagnostic(
            state,
            &config,
            &mut rng,
            &crate::AiSession::arc_from_game(state),
            &engine::ai_support::AiDecisionContract::issue(state, P0),
        );
        let receipt = selection
            .receipt
            .expect("reach guard: the ranked chooser must emit a receipt");
        assert_eq!(
            receipt.candidates.len(),
            5,
            "reach guard: nothing was gated out below the beam width. This canNOT detect a \
             sixth candidate -- the root beam truncates to max_branching before the receipt \
             is built; current_legal_targets.len() is the guard for that"
        );
        let row = receipt
            .candidates
            .iter()
            .find(|c| {
                c.action
                    == GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(futile)),
                    }
            })
            .expect("reach guard: the futile candidate must appear in the receipt");
        assert_eq!(row.rank, Some(3));
        assert!(!row.is_top_ranked);

        // Assertion 5 (the untapped / summoning-sick axis).
        // Assertion 1 cannot reach this axis, because `futile` is tapped as
        // well as sick, so a `.tapped` substitute still fires on it.
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(new_sick), &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
    }

    #[test]
    fn a_declared_attacker_is_exempt_from_the_unattacking_evasion_guard() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 1, 1)
            .with_ability_definition(whirler_ability())
            .id();
        let futile = scenario.add_creature(P0, "Tapped Sick Body", 3, 3).id();
        let useful = scenario.add_creature(P0, "Ready Attacker", 1, 1).id();
        scenario
            .add_creature(P0, "Sick Rookie", 1, 1)
            .with_summoning_sickness();
        scenario.add_creature(PlayerId(1), "Ready Blocker", 2, 2);
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let futile_object = state.objects.get_mut(&futile).unwrap();
        futile_object.tapped = true;
        futile_object.summoning_sick = true;

        runner.advance_to_phase(Phase::DeclareAttackers);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(useful, AttackTarget::Player(PlayerId(1)))],
                bands: Vec::new(),
            })
            .expect("reach guard: declared attacker must be engine-legal");
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");
        let state = runner.state();

        // Do NOT assert `.is_empty()` -- the untapped, non-sick Whirler Rogue
        // source remains eligible.
        assert!(state.objects[&useful].tapped);
        assert!(!get_valid_attacker_ids(state).contains(&useful));
        assert!(!get_valid_attacker_ids(state).contains(&futile));

        let config = AiConfig::default();
        // Two authorities on one board, opposite verdicts: `useful` is a
        // legitimately declared (and therefore tapped) attacker, exempt from
        // the new guard and judged instead by the unchanged blockability
        // comparison; `futile` is not a declared attacker and is judged by
        // the new guard.
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(useful), &config),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_pair_blockable_evasion_target"
        ));
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(futile), &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
    }

    #[test]
    fn an_opponents_turn_evasion_target_is_not_judged_by_the_attacking_teams_authority() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(1);
        let source = creature(&mut state, P0, "Whirler Rogue");
        let own_ready = creature(&mut state, P0, "Own Ready Nonattacker");
        let enemy_attacker = creature(&mut state, PlayerId(1), "Enemy Attacker");
        let blocker = creature(&mut state, P0, "Ready Blocker");
        state.combat = Some(CombatState {
            attackers: vec![declared_attacker(enemy_attacker, P0)],
            ..Default::default()
        });
        install_whirler_prompt(&mut state, source, vec![own_ready, enemy_attacker]);

        assert!(get_valid_block_targets(&state)
            .get(&blocker)
            .is_some_and(|targets| targets.contains(&enemy_attacker)));

        // (a) Asserted directly so the reason for the team gate is legible
        // from the test: on the opponent's turn
        // `get_valid_attacker_ids` answers about the *opponent's* attacking
        // team, so `own_ready`'s absence from that team-scoped set says
        // nothing about whether `own_ready` itself could attack — it is
        // excluded purely because its controller's team is not this turn's
        // attacking team.
        assert!(!state.objects[&own_ready].tapped);
        assert!(!get_valid_attacker_ids(&state).contains(&own_ready));

        let own_candidate = target_candidate(own_ready);
        let config = AiConfig::default();

        // (b) The instrument has no standing here (exact kind, not a bare
        // "neutral" check): the guard abstains because `is_on_attacking_team`
        // is false for the AI on the opponent's turn, so the verdict falls
        // through to the unchanged blockability comparison, matching the
        // adjacent
        // `opponent_combat_does_not_penalize_own_target_for_enemy_blockable_attacker`
        // fixture this test extends.
        assert!(matches!(
            effect_timing_verdict(&state, &own_candidate, &config),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_no_pair_blockable_evasion_target"
        ));
    }

    #[test]
    fn opponent_combat_does_not_penalize_own_target_for_enemy_blockable_attacker() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(1);
        let source = creature(&mut state, P0, "Whirler Rogue");
        let own_target = creature(&mut state, P0, "Own Nonattacker");
        let enemy_attacker = creature(&mut state, PlayerId(1), "Enemy Attacker");
        let blocker = creature(&mut state, P0, "Ready Blocker");
        state.combat = Some(CombatState {
            attackers: vec![declared_attacker(enemy_attacker, P0)],
            ..Default::default()
        });
        install_whirler_prompt(&mut state, source, vec![own_target, enemy_attacker]);

        assert!(get_valid_block_targets(&state)
            .get(&blocker)
            .is_some_and(|targets| targets.contains(&enemy_attacker)));
        let legal_actions = validated_candidate_actions_for_semantic_owner(&state, P0);
        assert!(legal_actions.iter().any(|candidate| {
            candidate.action
                == GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(own_target)),
                }
        }));
        assert!(legal_actions.iter().any(|candidate| {
            candidate.action
                == GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(enemy_attacker)),
                }
        }));

        let own_candidate = target_candidate(own_target);
        let decision = build_decision_context(&state);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);
        assert!(is_single_target_unblockable_activation(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &own_candidate,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        }));
        assert!(matches!(
            effect_timing_verdict(&state, &own_candidate, &config),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_no_pair_blockable_evasion_target"
        ));
    }

    /// Each attacker must use its actual FFA defender's declaration. The Player
    /// One pair is unrelated; Player Two's blocker can block only its ground
    /// attacker, not the flying Player Two attacker.
    #[test]
    fn declared_attacker_blockability_uses_the_actual_defender() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.phase = Phase::DeclareAttackers;
        let player_one_attacker = creature(&mut state, P0, "Player One Attacker");
        let player_two_attacker = creature(&mut state, P0, "Player Two Attacker");
        let player_two_flying_attacker = creature(&mut state, P0, "Player Two Flyer");
        state
            .objects
            .get_mut(&player_two_flying_attacker)
            .unwrap()
            .keywords
            .push(Keyword::Flying);
        let player_one_blocker = creature(&mut state, PlayerId(1), "Player One Blocker");
        let player_two_blocker = creature(&mut state, PlayerId(2), "Player Two Blocker");
        state.combat = Some(CombatState {
            attackers: vec![
                declared_attacker(player_one_attacker, PlayerId(1)),
                declared_attacker(player_two_attacker, PlayerId(2)),
                declared_attacker(player_two_flying_attacker, PlayerId(2)),
            ],
            ..Default::default()
        });

        let targets = get_valid_block_targets(&state);
        assert!(targets
            .get(&player_one_blocker)
            .is_some_and(|targets| targets.contains(&player_one_attacker)));
        assert!(targets.get(&player_two_blocker).is_some_and(|targets| {
            targets.contains(&player_two_attacker) && !targets.contains(&player_two_flying_attacker)
        }));
        assert_eq!(
            declared_attacker_blockability(&state, player_two_attacker),
            MaximumBlockDeclarationBlockability::Blockable
        );
        assert_eq!(
            declared_attacker_blockability(&state, player_two_flying_attacker),
            MaximumBlockDeclarationBlockability::NotBlockable
        );
    }

    #[test]
    fn team_defender_prompt_stands_down_despite_a_mapped_sibling() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.phase = Phase::DeclareAttackers;
        let source = creature(&mut state, P0, "Source");
        let mapped = creature(&mut state, P0, "Mapped Attacker");
        let futile = creature(&mut state, P0, "Futile Target");
        let defender_blocker = creature(&mut state, PlayerId(2), "Defender Blocker");
        let teammate_blocker = creature(&mut state, PlayerId(3), "Teammate Blocker");
        state
            .objects
            .get_mut(&futile)
            .unwrap()
            .keywords
            .push(Keyword::Flying);
        state.combat = Some(CombatState {
            attackers: vec![
                declared_attacker(mapped, PlayerId(2)),
                declared_attacker(futile, PlayerId(2)),
            ],
            ..Default::default()
        });
        install_whirler_prompt(&mut state, source, vec![futile, mapped]);

        assert!(players::teammates(&state, PlayerId(2)).contains(&PlayerId(3)));
        assert!(get_valid_block_targets(&state)
            .get(&defender_blocker)
            .is_some_and(|targets| targets.contains(&mapped)));
        assert!(engine::game::combat::validate_blockers_for_player(
            &state,
            PlayerId(3),
            &[(teammate_blocker, mapped)],
        )
        .is_ok());
        assert_eq!(
            declared_attacker_blockability(&state, futile),
            MaximumBlockDeclarationBlockability::NotBlockable
        );
        assert_eq!(
            declared_attacker_blockability(&state, mapped),
            MaximumBlockDeclarationBlockability::Blockable
        );
        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(futile), &AiConfig::default()),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_evasion_target_na"
        ));
    }

    /// Pins the guard-before-stand-down ordering in
    /// `evasion_target_verdict` -- `target_cannot_attack_this_turn` must run
    /// ahead of `prompt_has_team_defender`'s stand-down, not behind it.
    /// `futile` is tapped here, so it genuinely cannot attack this
    /// turn regardless of the team-blockability ambiguity affecting its
    /// sibling `mapped`. Hoisting `prompt_has_team_defender` above the guard
    /// must flip this assertion to neutral -- verified by mutation.
    #[test]
    fn team_defender_stand_down_does_not_mask_a_genuinely_unattacking_sibling() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.phase = Phase::DeclareAttackers;
        let source = creature(&mut state, P0, "Source");
        let mapped = creature(&mut state, P0, "Mapped Attacker");
        let futile = creature(&mut state, P0, "Futile Target");
        let defender_blocker = creature(&mut state, PlayerId(2), "Defender Blocker");
        let teammate_blocker = creature(&mut state, PlayerId(3), "Teammate Blocker");
        state.combat = Some(CombatState {
            attackers: vec![declared_attacker(mapped, PlayerId(2))],
            ..Default::default()
        });
        state.objects.get_mut(&futile).unwrap().tapped = true;
        install_whirler_prompt(&mut state, source, vec![futile, mapped]);

        assert!(players::teammates(&state, PlayerId(2)).contains(&PlayerId(3)));
        assert!(get_valid_block_targets(&state)
            .get(&defender_blocker)
            .is_some_and(|targets| targets.contains(&mapped)));
        assert!(engine::game::combat::validate_blockers_for_player(
            &state,
            PlayerId(3),
            &[(teammate_blocker, mapped)],
        )
        .is_ok());

        // Reach guard: the stand-down condition is genuinely live in this
        // fixture -- `mapped`'s defending player 2 has a teammate, exactly as
        // in the sibling fixture above, so a reviewer cannot dismiss this as
        // testing a fixture where the stand-down never applied.
        assert!(prompt_has_team_defender(&state));

        assert!(state.objects[&futile].tapped);
        assert!(!get_valid_attacker_ids(&state).contains(&futile));

        // Primary revert-failing assertion: with the guard checked ahead of
        // the stand-down (current code), `futile` scores strongly negative
        // even though `prompt_has_team_defender` is true. If the ordering is
        // reverted (stand-down checked first), this becomes
        // `Score { delta: 0.0, "effect_timing_evasion_target_na" }` instead.
        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(futile), &AiConfig::default()),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
    }

    /// The `players::teammates(...).contains(&player)` arm of
    /// `is_on_attacking_team` is reachable only in a team format (Two-Headed
    /// Giant here) when the AI itself is not the active player but is the
    /// active player's teammate. Nothing else in this file exercises it —
    /// every other fixture either has the AI as the active player or on the
    /// opponent's (non-teammate) side.
    #[test]
    fn two_headed_giant_teammate_evasion_target_is_judged_by_the_attacking_teams_authority() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.phase = Phase::BeginCombat;
        state.active_player = PlayerId(1);
        let source = creature(&mut state, P0, "Source");
        let futile = creature(&mut state, P0, "Futile Teammate Target");
        let ready = creature(&mut state, P0, "Ready Teammate Target");
        state.objects.get_mut(&futile).unwrap().tapped = true;
        install_whirler_prompt(&mut state, source, vec![futile, ready]);

        // Reach guard: P0 is not the active player but is its teammate, so
        // this fixture is on the teammate arm, not the `active_player ==
        // player` arm `team_defender_prompt_stands_down_despite_a_mapped_sibling`
        // and every other fixture above exercise.
        assert_ne!(state.active_player, P0);
        assert!(players::teammates(&state, PlayerId(1)).contains(&P0));
        assert!(!get_valid_attacker_ids(&state).contains(&futile));

        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(futile), &AiConfig::default()),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));

        // Negative control: `ready` is untapped and unrestricted, so on the
        // team-scoped `get_valid_attacker_ids` authority it IS eligible
        // to be declared. A regression that made the eligible-attacker scan
        // active-player-only (instead of whole-team, CR 508.1a + CR 805.10a)
        // would make the guard misfire on every creature P0 controls on its
        // teammate's turn; this sibling assertion would catch that even
        // though the primary assertion above stays green.
        assert!(get_valid_attacker_ids(&state).contains(&ready));
        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(ready), &AiConfig::default()),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_evasion_target_na"
        ));
    }

    #[test]
    fn a_ready_undeclared_creature_is_penalised_once_attackers_have_been_declared() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 2, 2)
            .with_ability_definition(whirler_ability())
            .id();
        let raider = scenario
            .add_creature(P0, "Skyway Raider", 2, 2)
            .flying()
            .id();
        let serra = scenario
            .add_creature(P0, "Watchful Serra", 2, 2)
            .flying()
            .vigilance()
            .id();
        let ready = scenario.add_creature(P0, "Ready Undeclared", 2, 2).id();
        scenario.add_creature(PlayerId(1), "Ground Blocker", 2, 2);
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        let mut runner = scenario.build();

        runner.advance_to_phase(Phase::DeclareAttackers);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![
                    (raider, AttackTarget::Player(PlayerId(1))),
                    (serra, AttackTarget::Player(PlayerId(1))),
                ],
                bands: Vec::new(),
            })
            .expect("reach guard: both fliers must be an engine-legal declaration");
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");
        let state = runner.state();

        assert!(matches!(state.phase, Phase::DeclareAttackers));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));
        assert!(state.combat.as_ref().is_some_and(|combat| {
            combat.attackers.iter().any(|a| a.object_id == raider)
                && combat.attackers.iter().any(|a| a.object_id == serra)
        }));
        assert!(!state.objects[&ready].tapped && !state.objects[&ready].summoning_sick);
        assert!(get_valid_attacker_ids(state).contains(&ready));
        assert!(defending_player_for_attacker(state, ready).is_none());

        // Vacuity guard: without this, the pre-existing relative-futility
        // sibling comparison in `evasion_target_verdict` fires ahead of the
        // guard under test and the primary assertion below would be green at
        // HEAD too.
        assert_eq!(
            declared_attacker_blockability(state, raider),
            MaximumBlockDeclarationBlockability::NotBlockable
        );
        assert_eq!(
            declared_attacker_blockability(state, serra),
            MaximumBlockDeclarationBlockability::NotBlockable
        );

        let config = AiConfig::default();
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(ready), &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));

        // Cell 2, vigilance: `serra` is a currently-attacking creature and
        // stays untapped, so it is exempt and judged by the unchanged
        // blockability comparison rather than by this guard.
        assert!(!state.objects[&serra].tapped);
        assert!(get_valid_attacker_ids(state).contains(&serra));
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(serra), &config),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_no_pair_blockable_evasion_target"
        ));

        // Cell 2, non-vigilant: `raider` is tapped by the declaration but is
        // exempt for the same reason -- it is currently an attacking
        // creature.
        assert!(state.objects[&raider].tapped);
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(raider), &config),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_no_pair_blockable_evasion_target"
        ));
    }

    /// Drives past the declare-attackers step to postcombat main, answering
    /// `WaitingFor::DeclareBlockers` if the board's opponent chooses to block.
    /// Bounded loop: a combat phase has finitely many priority windows.
    fn drive_to_postcombat_main(runner: &mut engine::game::scenario::GameRunner) {
        for _ in 0..40 {
            if runner.state().phase == Phase::PostCombatMain {
                return;
            }
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareBlockers { .. }
            ) {
                runner
                    .declare_blockers(&[])
                    .expect("reach guard: declining to block must be legal");
                continue;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
        assert_eq!(
            runner.state().phase,
            Phase::PostCombatMain,
            "drive_to_postcombat_main must reach PostCombatMain within its bounded loop"
        );
    }

    #[test]
    fn a_ready_creature_is_penalised_in_postcombat_main_after_its_combat() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 2, 2)
            .with_ability_definition(whirler_ability())
            .id();
        let declared = scenario.add_creature(P0, "Declared Attacker", 2, 2).id();
        let ready = scenario.add_creature(P0, "Ready Undeclared", 2, 2).id();
        scenario.add_creature(PlayerId(1), "Ready Opposing Blocker", 2, 2);
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        let mut runner = scenario.build();

        runner.advance_to_phase(Phase::DeclareAttackers);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(declared, AttackTarget::Player(PlayerId(1)))],
                bands: Vec::new(),
            })
            .expect("reach guard: the declaration must be engine-legal");
        drive_to_postcombat_main(&mut runner);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");
        let state = runner.state();

        assert!(matches!(state.phase, Phase::PostCombatMain));
        assert!(state.combat.is_none());
        assert!(get_valid_attacker_ids(state).contains(&ready));
        assert!(!state.objects[&ready].tapped);

        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(ready), &AiConfig::default()),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
    }

    #[test]
    fn a_creature_that_already_attacked_is_penalised_in_postcombat_main() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 2, 2)
            .with_ability_definition(whirler_ability())
            .id();
        let raider = scenario
            .add_creature(P0, "Skyway Raider", 2, 2)
            .flying()
            .id();
        let serra = scenario
            .add_creature(P0, "Watchful Serra", 2, 2)
            .flying()
            .vigilance()
            .id();
        let ready = scenario.add_creature(P0, "Ready Undeclared", 2, 2).id();
        scenario.add_creature(PlayerId(1), "Ground Blocker", 2, 2);
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        let mut runner = scenario.build();

        runner.advance_to_phase(Phase::DeclareAttackers);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![
                    (raider, AttackTarget::Player(PlayerId(1))),
                    (serra, AttackTarget::Player(PlayerId(1))),
                ],
                bands: Vec::new(),
            })
            .expect("reach guard: both fliers must be an engine-legal declaration");
        drive_to_postcombat_main(&mut runner);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");
        let state = runner.state();

        assert!(matches!(state.phase, Phase::PostCombatMain));
        assert!(state.combat.is_none());
        // The CR 511.3 teardown, asserted rather than assumed: neither
        // creature that attacked this turn remains a live attacking creature
        // once the combat phase is over.
        assert!(defending_player_for_attacker(state, serra).is_none());
        assert!(defending_player_for_attacker(state, raider).is_none());
        assert!(!state.objects[&serra].tapped);
        assert!(get_valid_attacker_ids(state).contains(&serra));

        let config = AiConfig::default();
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(serra), &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ), "primary, revert-failing: the vigilant attacker is no longer exempt once combat is over");
        assert!(
            matches!(
                effect_timing_verdict(state, &target_candidate(ready), &config),
                PolicyVerdict::Score { delta, reason }
                    if delta < 0.0
                        && reason.kind == "effect_timing_futile_unattacking_evasion_target"
            ),
            "paired sibling: the never-declared creature is penalised the same way"
        );
        // Negative control, not revert-failing: `raider` is already tapped
        // and already absent from the eligible set at HEAD, so this row is
        // unchanged by the fix -- included so a reader does not mistake it
        // for discrimination.
        assert!(state.objects[&raider].tapped);
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(raider), &config),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
    }

    #[test]
    fn a_scheduled_additional_combat_phase_keeps_a_ready_creature_unpenalised() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 2, 2)
            .with_ability_definition(whirler_ability())
            .id();
        let declared = scenario.add_creature(P0, "Declared Attacker", 2, 2).id();
        let ready = scenario.add_creature(P0, "Ready Undeclared", 2, 2).id();
        let extra_combat_source = scenario
            .add_creature(P0, "Aggravated Assault Stand-In", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::AdditionalPhase {
                        recipient: ExtraPhaseRecipient::Controller,
                        segment: TurnSegment::Phase(PhaseGroup::Combat),
                        after: ExtraPhaseAnchor::this_main_phase(),
                        followed_by: vec![TurnSegment::Phase(PhaseGroup::PostcombatMain)],
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        attacker_restriction: None,
                    },
                )
                .cost(AbilityCost::Tap),
            )
            .id();
        scenario.add_creature(PlayerId(1), "Ready Opposing Blocker", 2, 2);
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        let mut runner = scenario.build();

        runner.advance_to_phase(Phase::DeclareAttackers);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(declared, AttackTarget::Player(PlayerId(1)))],
                bands: Vec::new(),
            })
            .expect("reach guard: the declaration must be engine-legal");
        drive_to_postcombat_main(&mut runner);
        runner
            .act(GameAction::ActivateAbility {
                source_id: extra_combat_source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: the extra-combat activation must be legal");
        runner.advance_until_stack_empty();
        assert!(runner
            .state()
            .extra_phases
            .iter()
            .any(|extra| extra.segment == TurnSegment::Phase(PhaseGroup::Combat)));
        assert!(get_valid_attacker_ids(runner.state()).contains(&ready));

        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");
        let state = runner.state();
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(ready), &AiConfig::default()),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_evasion_target_na"
        ));
    }

    /// CR 508.1c + CR 611.2c: a restriction attached to a scheduled (not yet
    /// begun) combat phase penalises a creature it excludes, even though the
    /// current phase's own declare-attackers step is closed. Cast Last Night
    /// Together through the real pipeline — the `attacker_restriction` this
    /// row depends on is only produced by `additional_phase.rs::resolve`
    /// concretizing the spell's chosen targets into a `TrackedSet`, not by any
    /// hand-built `ExtraPhase`.
    #[test]
    fn a_restricted_scheduled_combat_penalises_a_creature_it_excludes() {
        use engine::types::mana::{ManaType, ManaUnit};

        const LNT_ORACLE: &str = "Choose two target creatures. Untap them. Put two +1/+1 \
            counters on each of them. They gain vigilance, indestructible, and haste until end \
            of turn. After this main phase, there is an additional combat phase. Only the \
            chosen creatures can attack during that combat phase.";

        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PostCombatMain);
        let source = scenario
            .add_creature(P0, "Whirler Rogue", 2, 2)
            .with_ability_definition(whirler_ability())
            .id();
        let chosen_a = scenario.add_creature(P0, "Chosen A", 2, 2).id();
        let chosen_b = scenario.add_creature(P0, "Chosen B", 2, 2).id();
        let unchosen = scenario.add_creature(P0, "Unchosen", 2, 2).id();
        scenario
            .add_creature(P0, "Thopter Payment One", 1, 1)
            .as_artifact();
        scenario
            .add_creature(P0, "Thopter Payment Two", 1, 1)
            .as_artifact();
        scenario.add_creature(PlayerId(1), "Ready Opposing Blocker", 2, 2);
        let lnt = scenario
            .add_spell_to_hand_from_oracle(P0, "Last Night Together", false, LNT_ORACLE)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ],
        );
        let mut runner = scenario.build();

        let outcome = runner
            .cast(lnt)
            .target_objects(&[chosen_a, chosen_b])
            .resolve();

        // Reach guards.
        assert!(outcome.state().stack.is_empty());
        assert!(runner.state().extra_phases.iter().any(|extra| extra.segment
            == TurnSegment::Phase(PhaseGroup::Combat)
            && extra.attacker_restriction.is_some()));
        assert!(get_valid_attacker_ids(runner.state()).contains(&unchosen));

        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: Whirler Rogue's two-artifact activation must be payable");

        let state = runner.state();
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(unchosen), &AiConfig::default()),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
        // Paired positive row: the restriction, not any queued combat and not
        // any creature, is what moved the verdict.
        assert!(matches!(
            effect_timing_verdict(state, &target_candidate(chosen_a), &AiConfig::default()),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_evasion_target_na"
        ));
    }

    /// CR 508.1c + CR 500.8: a restriction CURRENT on the combat in progress
    /// must not penalise a creature an UNRESTRICTED queued combat would still
    /// admit — the false-penalty direction, at the policy verdict rather than
    /// at the engine predicate (V4's board, one layer up).
    #[test]
    fn a_queued_unrestricted_combat_keeps_a_creature_the_live_restriction_excludes_unpenalised() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::BeginCombat;
        let source = creature(&mut state, P0, "Source");
        let plain = creature(&mut state, P0, "Plain");
        let land_creature = creature(&mut state, P0, "Land Creature");
        state
            .objects
            .get_mut(&land_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        install_whirler_prompt(&mut state, source, vec![plain, land_creature]);

        state.current_combat_attacker_restriction = Some(TargetFilter::Typed(
            TypedFilter::land().with_type(TypeFilter::Creature),
        ));
        state.current_combat_attacker_restriction_source = Some(source);
        state.extra_phases.push(ExtraPhase {
            anchor: Phase::PostCombatMain,
            segment: TurnSegment::Phase(PhaseGroup::Combat),
            attacker_restriction: None,
            attacker_restriction_source: None,
            id: ExtraPhaseId::default(),
        });

        // Reach guard: this is exactly the value HEAD's conjunct reads, so
        // with it false HEAD cannot produce the asserted verdict.
        assert!(!get_valid_attacker_ids(&state).contains(&plain));

        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(plain), &AiConfig::default()),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_evasion_target_na"
        ));

        // Paired positive control: without the queued unrestricted combat,
        // `plain` IS penalised -- proving `plain` is otherwise attack-eligible
        // on this board and that the primary assertion above is not vacuous.
        state.extra_phases.clear();
        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(plain), &AiConfig::default()),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0
                    && reason.kind == "effect_timing_futile_unattacking_evasion_target"
        ));
    }

    #[test]
    fn a_scheduled_additional_combat_phase_is_actually_entered() {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let declared = scenario.add_creature(P0, "Declared Attacker", 2, 2).id();
        // A creature that stays untapped through the first combat: without a
        // potential attacker, the engine auto-skips the extra combat's
        // declare-attackers step entirely (straight to EndCombat), and this
        // test would never reach the state it is pinning.
        scenario.add_creature(P0, "Ready Undeclared", 2, 2);
        let extra_combat_source = scenario
            .add_creature(P0, "Aggravated Assault Stand-In", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::AdditionalPhase {
                        recipient: ExtraPhaseRecipient::Controller,
                        segment: TurnSegment::Phase(PhaseGroup::Combat),
                        after: ExtraPhaseAnchor::this_main_phase(),
                        followed_by: vec![TurnSegment::Phase(PhaseGroup::PostcombatMain)],
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        attacker_restriction: None,
                    },
                )
                .cost(AbilityCost::Tap),
            )
            .id();
        scenario.add_creature(PlayerId(1), "Ready Opposing Blocker", 2, 2);
        let mut runner = scenario.build();

        runner.advance_to_phase(Phase::DeclareAttackers);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(declared, AttackTarget::Player(PlayerId(1)))],
                bands: Vec::new(),
            })
            .expect("reach guard: the declaration must be engine-legal");
        drive_to_postcombat_main(&mut runner);
        runner
            .act(GameAction::ActivateAbility {
                source_id: extra_combat_source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach guard: the extra-combat activation must be legal");
        runner.advance_until_stack_empty();

        for _ in 0..40 {
            if runner.state().phase == Phase::DeclareAttackers {
                break;
            }
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareBlockers { .. }
            ) {
                runner
                    .declare_blockers(&[])
                    .expect("reach guard: declining to block must be legal");
                continue;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
        let state = runner.state();

        assert_eq!(state.phase, Phase::DeclareAttackers);
        assert!(!state
            .extra_phases
            .iter()
            .any(|e| e.segment == TurnSegment::Phase(PhaseGroup::Combat)));
        assert!(state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.is_empty()));
    }

    #[test]
    fn menace_with_one_blocker_is_not_a_maximum_declaration_block() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::DeclareAttackers;
        let source = creature(&mut state, P0, "Source");
        let normal = creature(&mut state, P0, "Normal Attacker");
        let menace = creature(&mut state, P0, "Menace Attacker");
        state
            .objects
            .get_mut(&menace)
            .unwrap()
            .keywords
            .push(Keyword::Menace);
        let blocker = creature(&mut state, PlayerId(1), "Only Blocker");
        state.combat = Some(CombatState {
            attackers: vec![
                declared_attacker(normal, PlayerId(1)),
                declared_attacker(menace, PlayerId(1)),
            ],
            ..Default::default()
        });
        install_whirler_prompt(&mut state, source, vec![normal, menace]);

        assert!(get_valid_block_targets(&state)
            .get(&blocker)
            .is_some_and(|targets| targets.contains(&normal) && targets.contains(&menace)));
        assert_eq!(
            declared_attacker_blockability(&state, normal),
            MaximumBlockDeclarationBlockability::Blockable
        );
        assert_eq!(
            declared_attacker_blockability(&state, menace),
            MaximumBlockDeclarationBlockability::NotBlockable
        );
        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(menace), &AiConfig::default()),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0 && reason.kind == "effect_timing_futile_evasion_target"
        ));
    }

    #[test]
    fn menace_with_two_blockers_is_a_maximum_declaration_block() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::DeclareAttackers;
        let source = creature(&mut state, P0, "Source");
        let normal = creature(&mut state, P0, "Normal Attacker");
        let menace = creature(&mut state, P0, "Menace Attacker");
        state
            .objects
            .get_mut(&menace)
            .unwrap()
            .keywords
            .push(Keyword::Menace);
        let first_blocker = creature(&mut state, PlayerId(1), "First Blocker");
        let second_blocker = creature(&mut state, PlayerId(1), "Second Blocker");
        state.combat = Some(CombatState {
            attackers: vec![
                declared_attacker(normal, PlayerId(1)),
                declared_attacker(menace, PlayerId(1)),
            ],
            ..Default::default()
        });
        install_whirler_prompt(&mut state, source, vec![normal, menace]);

        for blocker in [first_blocker, second_blocker] {
            assert!(get_valid_block_targets(&state)
                .get(&blocker)
                .is_some_and(|targets| targets.contains(&normal) && targets.contains(&menace)));
        }
        assert_eq!(
            declared_attacker_blockability(&state, menace),
            MaximumBlockDeclarationBlockability::Blockable
        );
        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(menace), &AiConfig::default()),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_pair_blockable_evasion_target"
        ));
    }

    #[test]
    fn unsupported_target_selection_cannot_leak_an_old_removal_score() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        let source = creature(&mut state, P0, "Removal Source");
        let victim = creature(&mut state, PlayerId(1), "Removal Victim");
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
            Vec::new(),
            source,
            P0,
        );
        let targets = vec![TargetRef::Object(victim)];
        state.waiting_for = WaitingFor::TargetSelection {
            player: P0,
            pending_cast: Box::new(PendingCast::new(
                source,
                CardId(100),
                ability,
                ManaCost::zero(),
            )),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: targets.clone(),
                optional: true,
                chooser: None,
                effect_kind: engine::types::ability::EffectKind::Destroy,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: targets,
            },
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Target),
        };
        let config = AiConfig::default();
        let decision = build_decision_context(&state);
        let context = crate::context::AiContext::empty(&config.weights);
        assert!(
            EffectTimingPolicy.score(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: P0,
                config: &config,
                context: &context,
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            }) > 0.0,
            "reach guard: the pre-SelectTarget timing score must be nonzero"
        );
        assert!(matches!(
            effect_timing_verdict(&state, &candidate, &config),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_unsupported_target_selection"
        ));
    }

    fn whirler_prompt_is_classified(
        edit: impl FnOnce(&mut PendingCast, &mut TargetSelectionSlot),
    ) -> bool {
        let mut state = GameState::new_two_player(42);
        let source = creature(&mut state, P0, "Source");
        let target = creature(&mut state, P0, "Target");
        install_whirler_prompt(&mut state, source, vec![target]);
        let WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            ..
        } = &mut state.waiting_for
        else {
            unreachable!("fixture installs an ordinary target prompt");
        };
        edit(pending_cast, &mut target_slots[0]);

        let candidate = target_candidate(target);
        let decision = build_decision_context(&state);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);
        is_single_target_unblockable_activation(&PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        })
    }

    #[test]
    fn whirler_classifier_rejects_root_and_static_contract_variants() {
        assert!(whirler_prompt_is_classified(|_, _| {}));
        assert!(!whirler_prompt_is_classified(|pending, _| {
            pending.ability.condition = Some(AbilityCondition::EventOutcomeWon);
        }));
        assert!(!whirler_prompt_is_classified(|pending, _| {
            pending.ability.optional = true;
        }));
        assert!(!whirler_prompt_is_classified(|pending, _| {
            pending.ability.optional_targeting = true;
        }));
        assert!(!whirler_prompt_is_classified(|pending, _| {
            pending.ability.multi_target = Some(MultiTargetSpec::fixed(1, 1));
        }));
        assert!(!whirler_prompt_is_classified(|_, slot| {
            slot.chooser = Some(PlayerId(1));
        }));
        assert!(!whirler_prompt_is_classified(|pending, _| {
            let Effect::GenericEffect {
                static_abilities, ..
            } = &mut pending.ability.effect
            else {
                unreachable!("fixture installs a GenericEffect");
            };
            static_abilities[0].affected_zone = Some(Zone::Graveyard);
        }));
    }

    fn counter_effect() -> Effect {
        Effect::Counter {
            target: TargetFilter::StackSpell,
            source_rider: None,
            countered_spell_zone: None,
        }
    }

    /// Push a spell stack entry backed by a real object, so `assess_spell_impact`
    /// can read its mana value and (for `pt`) its creature stats. Returns the
    /// stack entry id.
    fn push_spell(
        state: &mut GameState,
        controller: PlayerId,
        mana_value: u32,
        pt: Option<(i32, i32)>,
        effect: Effect,
        targets: Vec<TargetRef>,
    ) -> ObjectId {
        let source_id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Spell".to_string(),
            Zone::Stack,
        );
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.mana_cost = ManaCost::generic(mana_value);
        if let Some((power, toughness)) = pt {
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(power);
            obj.toughness = Some(toughness);
        }
        let ability = ResolvedAbility::new(effect, targets, source_id, controller);
        let id = source_id;
        state.stack.push_back(StackEntry {
            id,
            source_id,
            controller,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ability)),
                card_id: CardId(id.0),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        id
    }

    /// A Priority decision on the opponent's turn with a `CastSpell` candidate —
    /// the seat from which `counterspell_score` is asked whether to hold up or fire.
    fn priority_fixture() -> (AiConfig, AiDecisionContext, CandidateAction) {
        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id: CardId(1),
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
        };
        (config, decision, candidate)
    }

    fn entry(state: &GameState, id: ObjectId) -> &StackEntry {
        state
            .stack
            .iter()
            .find(|e| e.id == id)
            .expect("stack entry exists")
    }

    #[test]
    fn counter_cast_scores_higher_against_commander_than_birds() {
        // Opponent's turn, one foreign creature spell on the stack in each case.
        let score_for = |mana_value: u32, pt: (i32, i32)| {
            let mut state = GameState::new_two_player(42);
            state.active_player = PlayerId(0);
            state.turn_number = 2;
            push_spell(
                &mut state,
                PlayerId(0),
                mana_value,
                Some(pt),
                Effect::NoOp,
                Vec::new(),
            );

            let (config, decision, candidate) = priority_fixture();
            let ctx = PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: AI,
                config: &config,
                context: &crate::context::AiContext::empty(&config.weights),
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            };
            counterspell_score(&ctx)
        };

        // Birds of Paradise: 0.3 (mana value) + 0.3 (0/1 body) = 0.6 impact, under
        // the one-card break-even — the counter is held instead of cast.
        let birds = score_for(1, (0, 1));
        // A 5-mana 4/4: 1.5 + 3.0 = 4.5 impact, at or above the full-value threshold.
        let commander = score_for(5, (4, 4));

        let hold = -0.6 * AiConfig::default().profile.interaction_patience;
        assert!(
            (birds - hold).abs() < 1e-9,
            "A Birds-shaped spell is not worth a card; expected the hold value {hold}, got {birds}"
        );
        assert!(
            commander > birds,
            "Countering a 4/4 commander must beat countering Birds, got {commander} vs {birds}"
        );
        assert!(
            commander > 0.0,
            "A high-impact spell must still draw a positive cast score, got {commander}"
        );
    }

    #[test]
    fn counter_cast_ignores_rival_counter_on_third_party_spell() {
        // Three players: B (P0) counters C (P2)'s spell. Countering B's counter only
        // resolves C's spell — worth nothing to the AI (P1).
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.active_player = PlayerId(0);
        state.turn_number = 2;
        let c_spell = push_spell(&mut state, PlayerId(2), 4, None, Effect::NoOp, Vec::new());
        let b_counter = push_spell(
            &mut state,
            PlayerId(0),
            2,
            None,
            counter_effect(),
            vec![TargetRef::Object(c_spell)],
        );

        let (config, decision, candidate) = priority_fixture();
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let b_worth = counter_target_worth(&ctx, entry(&state, b_counter));
        assert!(
            b_worth.abs() < 1e-9,
            "A rival counter aimed at a third player's spell is worth nothing, got {b_worth}"
        );
        // Without that rule B's counter would price at 2.1 via Effect::Counter and
        // outrank C's spell (1.2), so this pins the max onto C's spell.
        let best = best_counter_impact(&ctx);
        let c_impact = assess_spell_impact(&state, entry(&state, c_spell));
        assert!(
            (best - c_impact).abs() < 1e-9,
            "Best counter impact must come from C's spell ({c_impact}), got {best}"
        );
    }

    #[test]
    fn counter_cast_values_rival_counter_on_own_spell() {
        // B (P0) counters the AI's own 1-mana trick (impact 0.3, below break-even):
        // the stack-pressure term is silent, but the protect bonus still fires.
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.turn_number = 2;
        let own_spell = push_spell(&mut state, AI, 1, None, Effect::NoOp, Vec::new());
        push_spell(
            &mut state,
            PlayerId(0),
            2,
            None,
            counter_effect(),
            vec![TargetRef::Object(own_spell)],
        );

        let (config, decision, candidate) = priority_fixture();
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let expected = assess_spell_impact(&state, entry(&state, own_spell))
            * ctx.penalties().protect_spell_bonus_mult;
        let score = counterspell_score(&ctx);
        assert!(
            expected > 0.0,
            "Fixture must actually threaten a spell, got {expected}"
        );
        assert!(
            (score - expected).abs() < 1e-9,
            "Protecting a threatened own spell must score exactly the protect bonus {expected}, got {score}"
        );
    }

    #[test]
    fn combat_trick_strongly_penalized_end_step() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::End;
        state.active_player = PlayerId(0);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id: CardId(1),
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = combat_trick_score(&ctx);
        assert!(
            score < -1.5,
            "Combat trick should be strongly penalized during End step, got {score}"
        );
    }

    #[test]
    fn combat_trick_strongly_penalized_main_phase_no_combat() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        // No combat state — pump has no combat relevance
        state.combat = None;

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id: CardId(1),
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = combat_trick_score(&ctx);
        assert!(
            score < -1.5,
            "Combat trick should be strongly penalized during main phase with no combat, got {score}"
        );
    }

    #[test]
    fn combat_trick_strongly_penalized_postcombat_main() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PostCombatMain;
        state.active_player = PlayerId(0);
        state.combat = None;

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: ObjectId(0),
                card_id: CardId(1),
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = combat_trick_score(&ctx);
        assert!(
            score < -1.5,
            "Combat trick should be strongly penalized during post-combat main with no combat, got {score}"
        );
    }
    /// CR 601.2b + CR 700.2a: a mode is chosen while the spell is being cast, so
    /// a `SelectModes` candidate is where a modal card's removal mode has to be
    /// priced. This policy is an UNGATED consumer of the S11 mode plumbing — it
    /// reads `ctx.effects()` at every search depth — so with mode visibility the
    /// Destroy mode earns `removal_score` while the gain-life mode earns
    /// nothing. REVERT-FAILING: without the `SelectModes` arm of
    /// `PolicyContext::effects` both modes report an empty effect list and both
    /// score exactly 0.0, which is the reported "every mode looks the same"
    /// defect.
    #[test]
    fn select_modes_prices_a_removal_mode_above_a_lifegain_mode() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;

        // Something worth killing, so `removal_score`'s threat term is live.
        let victim = create_object(
            &mut state,
            CardId(31),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        let victim_obj = state.objects.get_mut(&victim).unwrap();
        victim_obj
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        victim_obj.power = Some(3);
        victim_obj.toughness = Some(3);

        // A two-mode spell on the stack: gain life, or destroy a creature.
        let spell_id = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Modal Removal".to_string(),
            Zone::Stack,
        );
        let modes = vec![
            engine::types::ability::AbilityDefinition::new(
                engine::types::ability::AbilityKind::Spell,
                Effect::GainLife {
                    amount: engine::types::ability::QuantityExpr::Fixed { value: 4 },
                    player: TargetFilter::Controller,
                },
            ),
            engine::types::ability::AbilityDefinition::new(
                engine::types::ability::AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Typed(engine::types::ability::TypedFilter::creature()),
                    cant_regenerate: false,
                },
            ),
        ];
        *std::sync::Arc::make_mut(&mut state.objects.get_mut(&spell_id).unwrap().abilities) =
            modes.clone();

        let resolved =
            ResolvedAbility::new(*modes[0].effect.clone(), Vec::new(), spell_id, PlayerId(0));
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ModeChoice {
                player: PlayerId(0),
                modal: engine::types::ability::ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 2,
                    ..Default::default()
                },
                pending_cast: Box::new(engine::types::game_state::PendingCast::new(
                    spell_id,
                    CardId(32),
                    resolved,
                    ManaCost::zero(),
                )),
                unavailable_modes: Vec::new(),
            },
            candidates: Vec::new(),
        };

        let config = AiConfig::default();
        let ai_context = crate::context::AiContext::empty(&config.weights);
        let score_for = |indices: Vec<usize>| {
            let candidate = CandidateAction {
                action: GameAction::SelectModes { indices },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
            };
            let ctx = PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &ai_context,
                cast_facts: None,
                search_depth: crate::policies::context::SearchDepth::Root,
            };
            EffectTimingPolicy.score(&ctx)
        };

        let removal = score_for(vec![1]);
        let lifegain = score_for(vec![0]);
        assert_eq!(
            lifegain, 0.0,
            "the gain-life mode carries no timing signal, got {lifegain}"
        );
        assert!(
            removal >= 0.3,
            "the removal mode must earn removal_score at the mode prompt, got {removal}"
        );
    }

    // ---- D1: a non-protective until-end-of-turn GenericEffect pump is a combat trick ----

    const COORDINATED_ASSAULT: &str = "Up to two target creatures each get +1/+0 and gain first strike until end of turn. (They deal combat damage before creatures without first strike.)";
    const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
    const UNLEASH_FURY: &str = "Double the power of target creature until end of turn.";
    const DWARVEN_SONG: &str = "One or more target creatures become red until end of turn.";
    const HEROIC_INTERVENTION: &str =
        "Permanents you control gain hexproof and indestructible until end of turn.";
    const ADAMANT_WILL: &str = "Target creature gets +2/+2 and gains indestructible until end of turn. (Damage and effects that say \"destroy\" don't destroy it.)";
    const MURDER: &str = "Destroy target creature.";
    const MOTHER_OF_RUNES: &str = "{T}: Target creature you control gains protection from the color of your choice until end of turn.";
    const WELLWISHER: &str = "{T}: You gain 1 life for each Elf on the battlefield.";

    fn parsed(oracle_text: &str, name: &str, core_type: &str) -> Vec<AbilityDefinition> {
        engine::parser::oracle::parse_oracle_text(
            oracle_text,
            name,
            &[],
            &[core_type.to_string()],
            &[],
        )
        .abilities
    }

    /// The first ability's effect of an instant parsed from `oracle_text`.
    fn instant_effect(oracle_text: &str, name: &str) -> Effect {
        (*parsed(oracle_text, name, "Instant")[0].effect).clone()
    }

    /// An instant in `controller`'s hand, built from its verbatim Oracle text.
    fn oracle_instant(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        oracle_text: &str,
    ) -> ObjectId {
        oracle_spell(state, controller, name, oracle_text, CoreType::Instant)
    }

    /// An instant or sorcery in `controller`'s hand, built from its verbatim Oracle text.
    fn oracle_spell(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        oracle_text: &str,
        core_type: CoreType,
    ) -> ObjectId {
        let abilities = parsed(oracle_text, name, &core_type.to_string());
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core_type);
        obj.abilities = std::sync::Arc::new(abilities);
        id
    }

    /// A creature on `controller`'s battlefield, able to tap, built from its verbatim Oracle text.
    fn oracle_creature(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        oracle_text: &str,
    ) -> ObjectId {
        let abilities = parsed(oracle_text, name, "Creature");
        let id = creature(state, controller, name);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.summoning_sick = false;
        obj.abilities = std::sync::Arc::new(abilities);
        id
    }

    fn cast_candidate(state: &GameState, object_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id: state.objects[&object_id].card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Spell),
        }
    }

    fn activate_candidate(source_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Ability),
        }
    }

    /// The registry-scaled EffectTiming delta for `candidate`.
    fn timing_delta(state: &GameState, candidate: &CandidateAction) -> f64 {
        match effect_timing_verdict(state, candidate, &AiConfig::default()) {
            PolicyVerdict::Score { delta, .. } => delta,
            PolicyVerdict::Reject { reason } => panic!("EffectTiming rejected: {}", reason.kind),
        }
    }

    /// P0 holds priority on an empty stack in `phase` of `active`'s turn, with a creature on each
    /// side. Turn 5, so the turn-phase activation is 1.0.
    fn d1_board(phase: Phase, active: PlayerId) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 5;
        state.phase = phase;
        state.active_player = active;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        creature(&mut state, P0, "Own Bear");
        creature(&mut state, PlayerId(1), "Opposing Bear");
        state
    }

    /// CR 611.2a + CR 514.2: Coordinated Assault's "+1/+0 and gain first strike until end of
    /// turn" (an until-end-of-turn `GenericEffect`, not a `Pump`) and Giant Growth's "+3/+3
    /// until end of turn" both end at cleanup, so EffectTiming prices them the same: the trick
    /// penalty in the own Draw step, the combat bonus in DeclareAttackers. Unleash Fury's
    /// doubled power (CR 701.10b: +X/+0) reads as Giant Growth does. Dwarven Song's colour
    /// change is not a trick in either row.
    #[test]
    fn effect_timing_prices_a_generic_effect_pump_as_a_combat_trick() {
        assert!(
            matches!(
                instant_effect(COORDINATED_ASSAULT, "Coordinated Assault"),
                Effect::GenericEffect {
                    duration: Some(Duration::UntilEndOfTurn),
                    ..
                }
            ),
            "premise: Coordinated Assault parses as an until-end-of-turn GenericEffect"
        );
        assert!(
            matches!(
                instant_effect(UNLEASH_FURY, "Unleash Fury"),
                Effect::DoublePT { .. }
            ),
            "premise: Unleash Fury parses as DoublePT"
        );
        assert!(
            matches!(
                instant_effect(GIANT_GROWTH, "Giant Growth"),
                Effect::Pump { .. }
            ),
            "premise: Giant Growth parses as Pump"
        );

        for (phase, combat) in [(Phase::Draw, false), (Phase::DeclareAttackers, true)] {
            let mut state = d1_board(phase, P0);
            if combat {
                state.combat = Some(CombatState::default());
            }
            let ca = oracle_instant(&mut state, P0, "Coordinated Assault", COORDINATED_ASSAULT);
            let gg = oracle_instant(&mut state, P0, "Giant Growth", GIANT_GROWTH);
            let uf = oracle_instant(&mut state, P0, "Unleash Fury", UNLEASH_FURY);
            let ds = oracle_instant(&mut state, P0, "Dwarven Song", DWARVEN_SONG);
            let d = |id| timing_delta(&state, &cast_candidate(&state, id));
            let (d_ca, d_gg, d_uf, d_ds) = (d(ca), d(gg), d(uf), d(ds));
            assert!(
                (d_ca - d_gg).abs() < 1e-9,
                "{phase:?}: Coordinated Assault {d_ca} must price as Giant Growth {d_gg}"
            );
            assert!(
                (d_uf - d_gg).abs() < 1e-9,
                "{phase:?}: Unleash Fury {d_uf} must price as Giant Growth {d_gg}"
            );
            if combat {
                assert!(d_gg > 0.0, "{phase:?}: the combat bonus fires, got {d_gg}");
            } else {
                assert!(d_gg < 0.0, "{phase:?}: the trick penalty fires, got {d_gg}");
            }
            assert_eq!(d_ds, 0.0, "{phase:?}: a colour change is not a trick");
        }
    }

    /// CR 608.2b: removal that kills a creature an opposing until-end-of-turn pump targets
    /// leaves the pump with nothing to do, so an opposing Coordinated Assault on the stack earns
    /// the AI's Murder the same response bonus as an opposing Giant Growth. The AI's own pump on
    /// the stack earns nothing.
    #[test]
    fn removal_answers_an_opposing_generic_effect_pump_on_the_stack() {
        let murder_delta = |stack: Option<(PlayerId, &str, &str)>| {
            let mut state = d1_board(Phase::PreCombatMain, PlayerId(1));
            let murder = oracle_instant(&mut state, P0, "Murder", MURDER);
            let own_target = creature(&mut state, P0, "Own Pump Target");
            let opposing_target = creature(&mut state, PlayerId(1), "Opposing Pump Target");
            if let Some((controller, name, oracle_text)) = stack {
                let target = if controller == P0 {
                    own_target
                } else {
                    opposing_target
                };
                push_spell(
                    &mut state,
                    controller,
                    1,
                    None,
                    instant_effect(oracle_text, name),
                    vec![TargetRef::Object(target)],
                );
            }
            timing_delta(&state, &cast_candidate(&state, murder))
        };
        let empty = murder_delta(None);
        let gg_bonus = murder_delta(Some((PlayerId(1), "Giant Growth", GIANT_GROWTH))) - empty;
        assert!(
            gg_bonus > 0.0,
            "reach: an opposing Giant Growth on the stack earns removal a bonus, got {gg_bonus}"
        );
        let ca_bonus = murder_delta(Some((
            PlayerId(1),
            "Coordinated Assault",
            COORDINATED_ASSAULT,
        ))) - empty;
        assert!(
            (ca_bonus - gg_bonus).abs() < 1e-9,
            "an opposing Coordinated Assault earns the Giant Growth bonus {gg_bonus}, got {ca_bonus}"
        );
        let undying = murder_delta(Some((PlayerId(1), "Undying Evil", UNDYING_EVIL))) - empty;
        assert!(
            undying.abs() < 1e-9,
            "a grant bound to no combat is not a pump to answer, got {undying}"
        );
        let own_ca = murder_delta(Some((P0, "Coordinated Assault", COORDINATED_ASSAULT))) - empty;
        assert!(
            own_ca.abs() < 1e-9,
            "the AI's own pump on the stack earns nothing, got {own_ca}"
        );
    }

    /// The opponent's main phase, with its Murder on the stack aimed at a creature of P0's.
    fn under_murder() -> GameState {
        let mut state = d1_board(Phase::PreCombatMain, PlayerId(1));
        let victim = creature(&mut state, P0, "Victim");
        push_spell(
            &mut state,
            PlayerId(1),
            3,
            None,
            instant_effect(MURDER, "Murder"),
            vec![TargetRef::Object(victim)],
        );
        state
    }

    /// CR 702.11b + CR 702.12b + CR 702.16b: hexproof, indestructible and protection pay off
    /// against removal in any phase, so a protective grant is not priced as a combat trick,
    /// whoever it affects. With an opposing Murder on the stack in the opponent's main phase,
    /// and in the AI's own End step, Heroic Intervention and Adamant Will price as Dwarven Song,
    /// and Mother of Runes' activation as Wellwisher's, while Coordinated Assault takes the
    /// trick penalty on the same boards.
    #[test]
    fn effect_timing_does_not_price_a_protective_grant_as_a_combat_trick() {
        use crate::policies::effect_classify::effect_is_temporary_combat_modifier;
        use crate::policies::self_protection_classify::{
            effect_has_defensive_payload, is_self_protection_effect,
        };

        let adamant = instant_effect(ADAMANT_WILL, "Adamant Will");
        assert!(
            effect_is_temporary_combat_modifier(&adamant) && effect_has_defensive_payload(&adamant),
            "premise: Adamant Will is a temporary combat modifier with a protective payload, so \
             its protective keyword is what refuses it: {adamant:?}"
        );
        assert!(
            !is_self_protection_effect(&adamant),
            "premise: Adamant Will is not self-scoped, so the boundary must be scope-free"
        );

        let boards = [
            ("opponent's main phase, Murder on the stack", under_murder()),
            ("own End step", d1_board(Phase::End, P0)),
        ];
        for (label, mut state) in boards {
            let hi = oracle_instant(&mut state, P0, "Heroic Intervention", HEROIC_INTERVENTION);
            let aw = oracle_instant(&mut state, P0, "Adamant Will", ADAMANT_WILL);
            let ds = oracle_instant(&mut state, P0, "Dwarven Song", DWARVEN_SONG);
            let ca = oracle_instant(&mut state, P0, "Coordinated Assault", COORDINATED_ASSAULT);
            let mor = oracle_creature(&mut state, P0, "Mother of Runes", MOTHER_OF_RUNES);
            let ww = oracle_creature(&mut state, P0, "Wellwisher", WELLWISHER);
            let cast = |id| timing_delta(&state, &cast_candidate(&state, id));
            let activate = |id| timing_delta(&state, &activate_candidate(id));
            let d_ds = cast(ds);
            assert!(
                cast(ca) < d_ds - 1.5,
                "{label}: reach: the trick penalty fires on this board"
            );
            assert!(
                (cast(hi) - d_ds).abs() < 1e-9,
                "{label}: Heroic Intervention must price as Dwarven Song"
            );
            assert!(
                (cast(aw) - d_ds).abs() < 1e-9,
                "{label}: Adamant Will must price as Dwarven Song"
            );
            assert!(
                (activate(mor) - activate(ww)).abs() < 1e-9,
                "{label}: Mother of Runes' activation must price as Wellwisher's"
            );
        }
    }

    const PAST_IN_FLAMES: &str = "Each instant and sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost.\nFlashback {4}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
    const FLASHBACK: &str = "Target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)";
    const UNDYING_EVIL: &str = "Target creature gains undying until end of turn. (When it dies, if it had no +1/+1 counters on it, return it to the battlefield under its owner's control with a +1/+1 counter on it.)";
    const CAULDRON_HAZE: &str = "Choose any number of target creatures. Each of those creatures gains persist until end of turn. (When it dies, if it had no -1/-1 counters on it, return it to the battlefield under its owner's control with a -1/-1 counter on it.)";
    const ANTLER_SKULKIN: &str = "{2}: Target white creature gains persist until end of turn. (When it dies, if it had no -1/-1 counters on it, return it to the battlefield under its owner's control with a -1/-1 counter on it.)";
    const AMOEBOID_CHANGELING: &str = "Changeling (This card is every creature type.)\n{T}: Target creature gains all creature types until end of turn.\n{T}: Target creature loses all creature types until end of turn.";
    const BLADES_OF_VELIS_VEL: &str = "Changeling (This card is every creature type.)\nUp to two target creatures each get +2/+0 and gain all creature types until end of turn.";
    const CORPOREAL_PROJECTION: &str = "Target creature you control gains myriad until end of turn. (Whenever it attacks, for each opponent other than defending player, you may create a token copy that's tapped and attacking that player or a planeswalker they control. Exile the tokens at end of combat.)\nOverload {3}{U}{U}{R}{R} (You may cast this spell for its overload cost. If you do, change \"target\" in its text to \"each.\")";

    /// CR 702.93a + CR 702.79a: undying and persist pay off when the creature dies, in whatever
    /// phase that happens, and CR 702.34a: flashback when the card is cast from the graveyard.
    /// None of that value is bound to this turn's combat, and a bare creature-type grant
    /// (CR 702.73a) has none there either, so each cast reads as Dwarven Song and each
    /// activation as Wellwisher's on every board. CR 702.116a: Corporeal Projection's myriad is
    /// paid out when the creature attacks, and Blades of Velis Vel still gives +2/+0, so both
    /// stay priced as Coordinated Assault is, and Akki Drillmaster's haste does too. The
    /// sorcery-speed row is exempt only in a precombat main, where no later window opens for it.
    #[test]
    fn effect_timing_does_not_price_a_grant_whose_value_lands_outside_combat() {
        use crate::policies::effect_classify::effect_is_temporary_combat_modifier;

        for (name, oracle_text) in [
            ("Past in Flames", PAST_IN_FLAMES),
            ("Flashback", FLASHBACK),
            ("Undying Evil", UNDYING_EVIL),
            ("Cauldron Haze", CAULDRON_HAZE),
            ("Blades of Velis Vel", BLADES_OF_VELIS_VEL),
            ("Corporeal Projection", CORPOREAL_PROJECTION),
        ] {
            let abilities = parsed(oracle_text, name, "Instant");
            assert!(
                crate::cast_facts::collect_definition_effects(&abilities[0])
                    .into_iter()
                    .any(effect_is_temporary_combat_modifier),
                "premise: {name} resolves a temporary combat modifier, so the keyword is what \
                 refuses or keeps it: {abilities:?}"
            );
        }

        let boards = [
            ("own precombat main", d1_board(Phase::PreCombatMain, P0)),
            ("own postcombat main", d1_board(Phase::PostCombatMain, P0)),
            ("own End step", d1_board(Phase::End, P0)),
            ("opponent's main phase, Murder on the stack", under_murder()),
        ];
        for (label, mut state) in boards {
            let ds = oracle_instant(&mut state, P0, "Dwarven Song", DWARVEN_SONG);
            let ca = oracle_instant(&mut state, P0, "Coordinated Assault", COORDINATED_ASSAULT);
            let bv = oracle_instant(&mut state, P0, "Blades of Velis Vel", BLADES_OF_VELIS_VEL);
            let cp = oracle_spell(
                &mut state,
                P0,
                "Corporeal Projection",
                CORPOREAL_PROJECTION,
                CoreType::Sorcery,
            );
            let pif = oracle_spell(
                &mut state,
                P0,
                "Past in Flames",
                PAST_IN_FLAMES,
                CoreType::Sorcery,
            );
            let unpriced_casts = [
                ("Past in Flames", pif),
                (
                    "Flashback",
                    oracle_instant(&mut state, P0, "Flashback", FLASHBACK),
                ),
                (
                    "Undying Evil",
                    oracle_instant(&mut state, P0, "Undying Evil", UNDYING_EVIL),
                ),
                (
                    "Cauldron Haze",
                    oracle_instant(&mut state, P0, "Cauldron Haze", CAULDRON_HAZE),
                ),
            ];
            let ww = oracle_creature(&mut state, P0, "Wellwisher", WELLWISHER);
            let akki = oracle_creature(&mut state, P0, "Akki Drillmaster", AKKI_DRILLMASTER);
            let unpriced_activations = [
                (
                    "Antler Skulkin",
                    oracle_creature(&mut state, P0, "Antler Skulkin", ANTLER_SKULKIN),
                ),
                (
                    "Amoeboid Changeling",
                    oracle_creature(&mut state, P0, "Amoeboid Changeling", AMOEBOID_CHANGELING),
                ),
            ];
            let cast = |id| timing_delta(&state, &cast_candidate(&state, id));
            let activate = |id| timing_delta(&state, &activate_candidate(id));
            let (d_ds, d_ww) = (cast(ds), activate(ww));

            for (name, id) in unpriced_casts {
                let delta = cast(id);
                assert!(
                    (delta - d_ds).abs() < 1e-9,
                    "{label}: {name} {delta} must price as Dwarven Song {d_ds}"
                );
            }
            for (name, id) in unpriced_activations {
                let delta = activate(id);
                assert!(
                    (delta - d_ww).abs() < 1e-9,
                    "{label}: {name} {delta} must price as Wellwisher {d_ww}"
                );
            }

            assert!(
                cast(ca) < d_ds - 1.5,
                "{label}: reach: the trick penalty fires on this board"
            );
            assert!(
                (cast(bv) - cast(ca)).abs() < 1e-9,
                "{label}: Blades of Velis Vel's +2/+0 is a trick, whatever types it adds"
            );
            assert!(
                activate(akki) < d_ww - 1.5,
                "{label}: Akki Drillmaster's haste stays priced"
            );
            if state.phase == Phase::PreCombatMain {
                assert!(
                    (cast(cp) - d_ds).abs() < 1e-9,
                    "{label}: a sorcery's last window before combat prices nothing"
                );
            } else {
                assert!(
                    cast(cp) < d_ds - 1.5,
                    "{label}: Corporeal Projection's myriad stays priced"
                );
            }
        }
    }

    const AKKI_DRILLMASTER: &str = "{T}: Target creature gains haste until end of turn.";
    const SHIVAN_DRAGON: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn.";

    /// A creature card in P0's hand with `mana_cost` and `abilities`.
    fn creature_in_hand(
        state: &mut GameState,
        name: &str,
        mana_cost: ManaCost,
        abilities: Vec<AbilityDefinition>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            P0,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = mana_cost;
        obj.abilities = std::sync::Arc::new(abilities);
        id
    }

    /// CR 113.3b + CR 608.3a + CR 611.2a: a permanent spell's activated pump resolves only once
    /// it is activated, after the permanent has entered, so it does not make the cast a combat
    /// trick. Akki Drillmaster's "{T}: Target creature gains haste until end of turn." (an
    /// until-end-of-turn `GenericEffect`) and Shivan Dragon's "{R}: This creature gets +1/+0
    /// until end of turn." (a `Pump`) price their precombat-main cast as a vanilla creature of
    /// the same cost does, on a board where Coordinated Assault takes the trick penalty.
    #[test]
    fn effect_timing_does_not_price_a_permanent_spells_activated_pump_as_a_combat_trick() {
        use engine::types::mana::ManaCostShard::Red;

        let cost = |shards: Vec<_>, generic| ManaCost::Cost { shards, generic };
        for (name, oracle_text, keywords, mana_cost) in [
            (
                "Akki Drillmaster",
                AKKI_DRILLMASTER,
                &[][..],
                cost(vec![Red], 2),
            ),
            (
                "Shivan Dragon",
                SHIVAN_DRAGON,
                &["Flying"][..],
                cost(vec![Red, Red], 4),
            ),
        ] {
            let keywords: Vec<String> = keywords.iter().map(|k| k.to_string()).collect();
            let abilities = engine::parser::oracle::parse_oracle_text(
                oracle_text,
                name,
                &keywords,
                &["Creature".to_string()],
                &[],
            )
            .abilities;
            assert!(
                abilities.iter().all(|a| a.kind != AbilityKind::Spell)
                    && abilities.iter().any(|a| a.kind == AbilityKind::Activated
                        && effect_is_combat_trick_payload(&a.effect)),
                "premise: {name} carries a combat-trick payload in an activated ability and has \
                 no spell ability: {abilities:?}"
            );

            let mut state = d1_board(Phase::PreCombatMain, P0);
            let card = creature_in_hand(&mut state, name, mana_cost.clone(), abilities);
            let vanilla = creature_in_hand(&mut state, "Vanilla", mana_cost, Vec::new());
            let ca = oracle_instant(&mut state, P0, "Coordinated Assault", COORDINATED_ASSAULT);
            let d = |id| timing_delta(&state, &cast_candidate(&state, id));
            let d_vanilla = d(vanilla);
            assert!(
                d(ca) < d_vanilla - 1.5,
                "{name}: reach: the trick penalty fires on this board"
            );
            assert!(
                (d(card) - d_vanilla).abs() < 1e-9,
                "{name}: the cast {} must price as a vanilla creature's {d_vanilla}",
                d(card)
            );
        }
    }

    const BARGE_IN: &str = "Target attacking creature gets +2/+2 until end of turn. Each attacking non-Human creature gains trample until end of turn.";
    const ARM_THE_CATHARS: &str = "Until end of turn, target creature gets +3/+3, up to one other target creature gets +2/+2, and up to one other target creature gets +1/+1. Those creatures gain vigilance until end of turn.";

    /// The combat-trick payload effects in the spell ability chain of a card parsed from
    /// `oracle_text`.
    fn trick_payloads(oracle_text: &str, name: &str, core_type: CoreType) -> Vec<Effect> {
        parsed(oracle_text, name, &core_type.to_string())
            .iter()
            .filter(|a| a.kind == AbilityKind::Spell)
            .flat_map(crate::cast_facts::collect_definition_effects)
            .filter(|effect| effect_is_combat_trick_payload(effect))
            .cloned()
            .collect()
    }

    /// CR 611.2a + CR 514.2: one spell is one combat trick, however many until-end-of-turn
    /// payloads its resolution carries. Barge In's `Pump` plus trample grant, and Arm the
    /// Cathars' three `Pump`s plus vigilance grant, price as Giant Growth's single `Pump` does:
    /// the trick penalty out of combat, the combat bonus in DeclareAttackers. Arm the Cathars is a
    /// sorcery, so it is priced in its controller's postcombat main phase, a window it can be cast in (CR 307.1) with the turn's combat behind it (CR 505.1a).
    #[test]
    fn effect_timing_prices_a_multi_payload_trick_once() {
        let barge_in = trick_payloads(BARGE_IN, "Barge In", CoreType::Instant);
        assert!(
            barge_in.len() >= 2
                && barge_in.iter().any(|e| matches!(e, Effect::Pump { .. }))
                && barge_in
                    .iter()
                    .any(|e| matches!(e, Effect::GenericEffect { .. })),
            "premise: Barge In carries a Pump and a GenericEffect trick payload: {barge_in:?}"
        );
        let arm = trick_payloads(ARM_THE_CATHARS, "Arm the Cathars", CoreType::Sorcery);
        assert!(
            arm.iter()
                .filter(|e| matches!(e, Effect::Pump { .. }))
                .count()
                >= 2,
            "premise: Arm the Cathars carries several Pump trick payloads: {arm:?}"
        );

        for (phase, combat) in [(Phase::End, false), (Phase::DeclareAttackers, true)] {
            let mut state = d1_board(phase, P0);
            if combat {
                state.combat = Some(CombatState::default());
            }
            let bi = oracle_instant(&mut state, P0, "Barge In", BARGE_IN);
            let gg = oracle_instant(&mut state, P0, "Giant Growth", GIANT_GROWTH);
            let d = |id| timing_delta(&state, &cast_candidate(&state, id));
            let (d_bi, d_gg) = (d(bi), d(gg));
            if combat {
                assert!(d_gg > 0.0, "{phase:?}: the combat bonus fires, got {d_gg}");
            } else {
                assert!(d_gg < 0.0, "{phase:?}: the trick penalty fires, got {d_gg}");
            }
            assert!(
                (d_bi - d_gg).abs() < 1e-9,
                "{phase:?}: Barge In {d_bi} must price as Giant Growth {d_gg}"
            );
        }

        let mut state = d1_board(Phase::PostCombatMain, P0);
        let arm = oracle_spell(
            &mut state,
            P0,
            "Arm the Cathars",
            ARM_THE_CATHARS,
            CoreType::Sorcery,
        );
        let gg = oracle_instant(&mut state, P0, "Giant Growth", GIANT_GROWTH);
        let d = |id| timing_delta(&state, &cast_candidate(&state, id));
        let (d_arm, d_gg) = (d(arm), d(gg));
        assert!(d_gg < 0.0, "reach: the trick penalty fires, got {d_gg}");
        assert!(
            (d_arm - d_gg).abs() < 1e-9,
            "Arm the Cathars {d_arm} must price as Giant Growth {d_gg}"
        );
    }

    // ---- A payload confined to sorcery timing has no later window before combat ----

    const OVERRUN: &str = "Creatures you control get +3/+3 and gain trample until end of turn. (Each of those creatures can deal excess combat damage to the player or planeswalker it's attacking.)";
    const OVERCOME: &str = "Creatures you control get +2/+2 and gain trample until end of turn. (Each of those creatures can deal excess combat damage to the player or planeswalker it's attacking.)";
    const BURST_OF_SPEED: &str = "Creatures you control gain haste until end of turn. (They can attack and {T} even if they just came under your control.)";
    const DEAFENING_CLARION: &str = "Choose one or both —\n• Deafening Clarion deals 3 damage to each creature.\n• Creatures you control gain lifelink until end of turn.";
    const SWELTERING_SUNS: &str =
        "Sweltering Suns deals 3 damage to each creature.\nCycling {3} ({3}, Discard this card: Draw a card.)";
    const GARRUK_WILDSPEAKER: &str = "[+1]: Untap two target lands.\n[−1]: Create a 3/3 green Beast creature token.\n[−4]: Creatures you control get +3/+3 and gain trample until end of turn.";
    const BRAVE_KIN_DUO: &str =
        "{1}, {T}: Target creature gets +1/+1 until end of turn. Activate only as a sorcery.";
    const ROCKFACE_VILLAGE: &str = "{T}: Add {C}.\n{T}: Add {R}. Spend this mana only to cast a creature spell.\n{R}, {T}: Target Lizard, Mouse, Otter, or Raccoon you control gets +1/+0 and gains haste until end of turn. Activate only as a sorcery.";
    const ELSPETH_KNIGHT_ERRANT: &str = "[+1]: Create a 1/1 white Soldier creature token.\n[+1]: Target creature gets +3/+3 and gains flying until end of turn.\n[−8]: You get an emblem with \"Artifacts, creatures, enchantments, and lands you control have indestructible.\"";
    const SORIN_VAMPIRE_LORD: &str = "[+1]: Up to one target creature gets +2/+0 until end of turn.\n[−2]: Sorin deals 4 damage to any target. You gain 4 life.\n[−8]: Until end of turn, each Vampire you control gains \"{T}: Gain control of target creature.\"";
    const NISSA_GENESIS_MAGE: &str = "[+2]: Untap up to two target creatures and up to two target lands.\n[−3]: Target creature gets +5/+5 until end of turn.\n[−10]: Look at the top ten cards of your library. You may put any number of creature and/or land cards from among them onto the battlefield. Put the rest on the bottom of your library in a random order.";
    const ROWANS_TALENT: &str = "Enchant planeswalker\nEnchanted planeswalker has \"[+1]: Up to one target creature gets +2/+0 and gains first strike and trample until end of turn.\"\nWhenever you activate a loyalty ability of enchanted planeswalker, copy that ability. You may choose new targets for the copy.";
    const THE_WANDERING_EMPEROR: &str = "Flash\nAs long as The Wandering Emperor entered this turn, you may activate her loyalty abilities any time you could cast an instant.\n[+1]: Put a +1/+1 counter on up to one target creature. It gains first strike until end of turn.\n[−1]: Create a 2/2 white Samurai creature token with vigilance.\n[−2]: Exile target tapped creature. You gain 2 life.";
    const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
    const HEROIC_REINFORCEMENTS: &str = "Create two 1/1 white Soldier creature tokens. Until end of turn, creatures you control get +1/+1 and gain haste. (They can attack and {T} this turn.)";
    const GOBLIN_WAR_PARTY: &str = "Choose one —\n• Create three 1/1 red Goblin creature tokens.\n• Creatures you control get +1/+1 and gain haste until end of turn.\nEntwine {2}{R} (Choose both if you pay the entwine cost.)";
    const MONSTROUS_RAGE: &str = "Target creature gets +2/+0 until end of turn. Create a Monster Role token attached to it. (If you control another Role on it, put that one into the graveyard. Enchanted creature gets +1/+1 and has trample.)";
    const AFFLICT: &str = "Target creature gets -1/-1 until end of turn.\nDraw a card.";
    const MOMENT_OF_TRIUMPH: &str =
        "Target creature gets +2/+2 until end of turn. You gain 2 life.";
    const GERRARDS_COMMAND: &str = "Untap target creature. It gets +3/+3 until end of turn.";
    const STABBING_PAIN: &str = "Target creature gets -1/-1 until end of turn. Tap that creature.";
    const ECHOING_COURAGE: &str = "Target creature and all other creatures with the same name as that creature get +2/+2 until end of turn.";
    const MONSTROUS_STEP: &str = "Target creature gets +7/+7 until end of turn. Up to one other target creature blocks it this turn if able.\nCycling {2} ({2}, Discard this card: Draw a card.)";
    const GIVE_NO_GROUND: &str = "Target creature gets +2/+6 until end of turn and can block any number of creatures this turn.";
    const EMERGENT_GROWTH: &str =
        "Target creature gets +5/+5 until end of turn and must be blocked this turn if able.";
    const INVIGORATED_RAMPAGE: &str = "Choose one —\n• Target creature gets +4/+0 and gains trample until end of turn.\n• Two target creatures each get +2/+0 and gain trample until end of turn.";
    const PYRIC_SALAMANDER: &str = "{R}: This creature gets +1/+0 until end of turn. Sacrifice this creature at the beginning of the next end step.";
    const VECTIS_AGENTS: &str =
        "{U}{B}: This creature gets -2/-0 until end of turn and can't be blocked this turn.";
    const MOBILE_FORT: &str = "Defender (This creature can't attack.)\n{3}: This creature gets +3/-1 until end of turn and can attack this turn as though it didn't have defender. Activate only once each turn.";
    const DRAGON_MASK: &str = "{3}, {T}: Target creature you control gets +2/+2 until end of turn. Return it to its owner's hand at the beginning of the next end step. (Return it only if it's on the battlefield.)";

    fn mana(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
        ManaCost::Cost { shards, generic }
    }

    /// P0 at its precombat main with three of each basic land and a Bear on each side.
    fn trick_scenario() -> GameScenario {
        let mut s = GameScenario::new_n_player(2, 42);
        s.at_phase(Phase::PreCombatMain);
        for color in [
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
            ManaColor::Green,
        ] {
            for _ in 0..3 {
                s.add_basic_land(P0, color);
            }
        }
        s.add_creature(P0, "Bear", 2, 2);
        s.add_creature(P1, "Bear", 2, 2);
        s
    }

    fn add_spell(
        s: &mut GameScenario,
        name: &str,
        is_instant: bool,
        oracle_text: &str,
        cost: ManaCost,
    ) -> ObjectId {
        s.add_spell_to_hand_from_oracle(P0, name, is_instant, oracle_text)
            .with_mana_cost(cost)
            .id()
    }

    fn add_overrun(s: &mut GameScenario) -> ObjectId {
        use ManaCostShard::Green;
        add_spell(
            s,
            "Overrun",
            false,
            OVERRUN,
            mana(2, vec![Green, Green, Green]),
        )
    }

    fn add_assault(s: &mut GameScenario) -> ObjectId {
        add_spell(
            s,
            "Coordinated Assault",
            true,
            COORDINATED_ASSAULT,
            mana(0, vec![ManaCostShard::Red]),
        )
    }

    fn add_leyline(s: &mut GameScenario, controller: PlayerId) {
        s.add_enchantment_from_oracle(
            controller,
            "Leyline of Anticipation",
            LEYLINE_OF_ANTICIPATION,
        );
    }

    /// Turn 5, so the turn-phase activation is 1.0, and a layer pass, which refreshes the
    /// static-mode presence index the timing authorities read.
    fn settle(runner: &mut GameRunner) {
        let state = runner.state_mut();
        state.turn_number = 5;
        evaluate_layers(state);
    }

    /// As `granted_loyalty_ability_timing.rs` builds her: she has no planeswalker subtype, and
    /// "entered this turn" reads `entered_battlefield_turn`.
    fn set_emperor_entered(runner: &mut GameRunner, emperor: ObjectId, entered_this_turn: bool) {
        let state = runner.state_mut();
        let turn = state.turn_number;
        let obj = state.objects.get_mut(&emperor).unwrap();
        obj.card_types.subtypes.clear();
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = Some(if entered_this_turn { turn } else { turn - 1 });
    }

    fn loyalty_index(state: &GameState, id: ObjectId, amount: i32) -> usize {
        state.objects[&id]
            .abilities
            .iter()
            .position(|a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: n }) if n == amount))
            .expect("the loyalty ability is printed")
    }

    /// The last ability at this loyalty cost: Elspeth's second [+1], or a granted one, which
    /// follows the printed abilities.
    fn last_loyalty_index(state: &GameState, id: ObjectId, amount: i32) -> usize {
        state.objects[&id]
            .abilities
            .iter()
            .rposition(
                |a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: n }) if n == amount),
            )
            .expect("the loyalty ability is on the permanent")
    }

    /// The printed index of the card's last activated ability: its only one, or the one after
    /// its mana abilities.
    fn last_activated_index(state: &GameState, id: ObjectId) -> usize {
        state.objects[&id]
            .abilities
            .iter()
            .rposition(|a| a.kind == AbilityKind::Activated)
            .expect("the card prints an activated ability")
    }

    /// The engine's own candidate for casting `id`. A cast it does not offer is a reach failure.
    fn offered_cast(state: &GameState, id: ObjectId) -> CandidateAction {
        build_decision_context(state)
            .candidates
            .into_iter()
            .find(
                |c| matches!(c.action, GameAction::CastSpell { object_id, .. } if object_id == id),
            )
            .unwrap_or_else(|| panic!("reach: {}'s cast must be offered", state.objects[&id].name))
    }

    /// The engine's own candidate for activating `index` of `source`.
    fn offered_activation(state: &GameState, source: ObjectId, index: usize) -> CandidateAction {
        build_decision_context(state)
            .candidates
            .into_iter()
            .find(|c| {
                matches!(
                    c.action,
                    GameAction::ActivateAbility { source_id, ability_index, .. }
                        if source_id == source && ability_index == index
                )
            })
            .unwrap_or_else(|| {
                panic!(
                    "reach: {}'s ability {index} must be offered",
                    state.objects[&source].name
                )
            })
    }

    fn cast_delta(state: &GameState, id: ObjectId) -> f64 {
        timing_delta(state, &offered_cast(state, id))
    }

    fn activation_delta(state: &GameState, source: ObjectId, index: usize) -> f64 {
        timing_delta(state, &offered_activation(state, source, index))
    }

    /// CR 505.1a + CR 307.1 + CR 307.5: a sorcery's cast (Overrun, Overcome, Burst of Speed, Arm
    /// the Cathars), a loyalty ability (CR 606.3: Garruk's −4) and an "activate only as a
    /// sorcery" ability (CR 602.5d: Rockface Village, Brave-Kin Duo) have no later window before
    /// this turn's combat, and their until-end-of-turn payload lasts through it (CR 611.2a +
    /// CR 514.2), so timing prices them neutral in the precombat main, while the instants and the
    /// unrestricted Shivan Dragon on the same board keep the trick penalty. Deafening Clarion's
    /// cast reads as Sweltering Suns, which has only its damage clause. In a postcombat main the
    /// turn's combat is behind every one of them (CR 505.1a), so the penalty stands.
    #[test]
    fn sorcery_confined_trick_is_neutral_in_its_own_precombat_main() {
        use ManaCostShard::{Green, Red, White};

        let mut s = trick_scenario();
        let overrun = add_overrun(&mut s);
        let overcome = add_spell(
            &mut s,
            "Overcome",
            false,
            OVERCOME,
            mana(3, vec![Green, Green]),
        );
        let burst = add_spell(
            &mut s,
            "Burst of Speed",
            false,
            BURST_OF_SPEED,
            mana(0, vec![Red]),
        );
        let arm = add_spell(
            &mut s,
            "Arm the Cathars",
            false,
            ARM_THE_CATHARS,
            mana(1, vec![White, White]),
        );
        let clarion = add_spell(
            &mut s,
            "Deafening Clarion",
            false,
            DEAFENING_CLARION,
            mana(1, vec![Red, White]),
        );
        let suns = add_spell(
            &mut s,
            "Sweltering Suns",
            false,
            SWELTERING_SUNS,
            mana(1, vec![Red, Red]),
        );
        let assault = add_assault(&mut s);
        let giant_growth = add_spell(
            &mut s,
            "Giant Growth",
            true,
            GIANT_GROWTH,
            mana(0, vec![Green]),
        );
        // Printed loyalty 3; 4 is the first board that can pay the −4.
        let garruk = s
            .add_planeswalker_from_oracle(P0, "Garruk Wildspeaker", "Garruk", 4, GARRUK_WILDSPEAKER)
            .id();
        let brave_kin = s
            .add_creature_from_oracle(P0, "Brave-Kin Duo", 1, 1, BRAVE_KIN_DUO)
            .with_mana_cost(mana(0, vec![White]))
            .with_subtypes(vec!["Rabbit", "Mouse"])
            .id();
        let rockface = s
            .add_land_from_oracle(P0, "Rockface Village", ROCKFACE_VILLAGE)
            .id();
        let shivan = s
            .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, SHIVAN_DRAGON)
            .with_mana_cost(mana(4, vec![Red, Red]))
            .id();
        let wellwisher = s
            .add_creature_from_oracle(P0, "Wellwisher", 1, 1, WELLWISHER)
            .with_mana_cost(mana(1, vec![Green]))
            .with_subtypes(vec!["Elf"])
            .id();
        let mut runner = s.build();
        settle(&mut runner);
        let state = runner.state();
        assert!(
            matches!(
                *state.objects[&overrun].abilities[0].effect,
                Effect::GenericEffect {
                    duration: Some(Duration::UntilEndOfTurn),
                    ..
                }
            ),
            "premise: Overrun parses as an until-end-of-turn GenericEffect"
        );
        let minus_four = loyalty_index(state, garruk, -4);
        let brave_kin_ix = last_activated_index(state, brave_kin);
        let rockface_ix = last_activated_index(state, rockface);
        let shivan_ix = last_activated_index(state, shivan);
        let wellwisher_ix = last_activated_index(state, wellwisher);

        let d_wellwisher = activation_delta(state, wellwisher, wellwisher_ix);
        assert!(
            cast_delta(state, assault) < -1.5 && cast_delta(state, giant_growth) < -1.5,
            "reach: the instants keep the trick penalty on this board"
        );
        assert!(
            activation_delta(state, shivan, shivan_ix) < d_wellwisher - 1.5,
            "reach: the activation path fires on this board"
        );

        for (name, id) in [
            ("Overrun", overrun),
            ("Overcome", overcome),
            ("Burst of Speed", burst),
            ("Arm the Cathars", arm),
        ] {
            assert_eq!(cast_delta(state, id), 0.0, "{name} is neutral");
        }
        assert_eq!(
            activation_delta(state, garruk, minus_four),
            0.0,
            "Garruk's −4 is neutral"
        );
        assert_eq!(
            activation_delta(state, rockface, rockface_ix),
            0.0,
            "Rockface Village's ability is neutral"
        );
        assert!(
            (activation_delta(state, brave_kin, brave_kin_ix) - d_wellwisher).abs() < 1e-9,
            "Brave-Kin Duo's activation prices as Wellwisher's {d_wellwisher}"
        );
        assert!(
            (cast_delta(state, clarion) - cast_delta(state, suns)).abs() < 1e-9,
            "Deafening Clarion's cast prices as Sweltering Suns'"
        );

        // A stale candidate reads the object the cast authority falls back to, in hand.
        let stale = CandidateAction {
            action: GameAction::CastSpell {
                object_id: ObjectId(u64::MAX),
                card_id: state.objects[&overrun].card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Spell),
        };
        assert_eq!(
            timing_delta(state, &stale),
            0.0,
            "the subject is the cast authority's object, not the candidate's id"
        );

        let mut postcombat = state.clone();
        postcombat.phase = Phase::PostCombatMain;
        let state = &postcombat;
        let d_giant_growth = cast_delta(state, giant_growth);
        assert!(d_giant_growth < -1.5, "reach, got {d_giant_growth}");
        for (name, delta) in [
            ("Overrun", cast_delta(state, overrun)),
            ("Arm the Cathars", cast_delta(state, arm)),
            ("Garruk's −4", activation_delta(state, garruk, minus_four)),
            (
                "Rockface Village",
                activation_delta(state, rockface, rockface_ix),
            ),
        ] {
            assert!(
                (delta - d_giant_growth).abs() < 1e-9,
                "{name} {delta} must price as Giant Growth {d_giant_growth} postcombat"
            );
        }
    }

    /// CR 601.3b + CR 702.8a + CR 304.5: the engine's authority decides, not the card type. With
    /// its controller's Leyline of Anticipation, Overrun can wait for combat and prices as
    /// Coordinated Assault; an opponent's Leyline opens nothing for P0. The Wandering Emperor's
    /// counter keeps her +1 neutral in both rows: the turn she entered, the permission's window,
    /// and a later turn.
    #[test]
    fn a_timing_permission_keeps_a_sorcery_speed_trick_priced() {
        for (leyline_controller, confined) in [(P0, false), (P1, true)] {
            let mut s = trick_scenario();
            let overrun = add_overrun(&mut s);
            let assault = add_assault(&mut s);
            add_leyline(&mut s, leyline_controller);
            let mut runner = s.build();
            settle(&mut runner);
            let state = runner.state();
            assert_eq!(
                spell_confined_to_sorcery_timing(state, P0, overrun),
                confined,
                "premise: the query, {leyline_controller:?}'s Leyline"
            );
            let (d_overrun, d_assault) = (cast_delta(state, overrun), cast_delta(state, assault));
            assert!(d_assault < -1.5, "reach: the instant is priced");
            if confined {
                assert_eq!(d_overrun, 0.0, "an opponent's Leyline opens nothing for P0");
            } else {
                assert!(
                    (d_overrun - d_assault).abs() < 1e-9,
                    "Overrun {d_overrun} under its Leyline must price as Assault {d_assault}"
                );
            }
        }

        for (entered_this_turn, confined) in [(true, false), (false, true)] {
            let mut s = trick_scenario();
            let emperor = s
                .add_planeswalker_from_oracle(
                    P0,
                    "The Wandering Emperor",
                    "",
                    3,
                    THE_WANDERING_EMPEROR,
                )
                .id();
            let shivan = s
                .add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, SHIVAN_DRAGON)
                .with_mana_cost(mana(4, vec![ManaCostShard::Red, ManaCostShard::Red]))
                .id();
            let mut runner = s.build();
            settle(&mut runner);
            set_emperor_entered(&mut runner, emperor, entered_this_turn);
            settle(&mut runner);
            let state = runner.state();
            let plus_one = loyalty_index(state, emperor, 1);
            assert_eq!(
                ability_confined_to_sorcery_timing(state, P0, emperor, plus_one),
                confined,
                "premise: the query, entered this turn: {entered_this_turn}"
            );
            let d_shivan = activation_delta(state, shivan, last_activated_index(state, shivan));
            assert!(
                d_shivan < -1.5,
                "reach: Shivan Dragon is priced, {d_shivan}"
            );
            assert_eq!(
                activation_delta(state, emperor, plus_one),
                0.0,
                "her counter stays, entered this turn: {entered_this_turn}"
            );
        }
    }

    /// Own-turn sorcery `name`, cast in `phase` with no modes chosen: its `[0]`, `[1]` and
    /// `[0, 1]` deltas at the mode prompt. The prompt and all three candidates are unconditional.
    fn mode_deltas(
        phase: Phase,
        leyline: bool,
        name: &str,
        oracle_text: &str,
        cost: ManaCost,
    ) -> [f64; 3] {
        let mut s = trick_scenario();
        let spell = add_spell(&mut s, name, false, oracle_text, cost);
        if leyline {
            add_leyline(&mut s, P0);
        }
        let mut runner = s.build();
        runner.state_mut().phase = phase;
        settle(&mut runner);
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("reach: the cast begins");
        let state = runner.state();
        let WaitingFor::ModeChoice {
            player,
            pending_cast,
            ..
        } = &state.waiting_for
        else {
            panic!("reach: expected the mode prompt: {:?}", state.waiting_for);
        };
        assert_eq!(*player, P0);
        assert_eq!(pending_cast.object_id, spell);
        assert_eq!(
            spell_confined_to_sorcery_timing(state, P0, spell),
            !leyline,
            "premise: the engine query, read mid-cast"
        );

        let candidates = build_decision_context(state).candidates;
        [vec![0], vec![1], vec![0, 1]].map(|indices| {
            let candidate = candidates
                .iter()
                .find(|c| {
                    c.action
                        == GameAction::SelectModes {
                            indices: indices.clone(),
                        }
                })
                .unwrap_or_else(|| panic!("reach: SelectModes {indices:?} must be offered"));
            timing_delta(state, candidate)
        })
    }

    fn clarion_mode_deltas(phase: Phase, leyline: bool) -> [f64; 3] {
        use ManaCostShard::{Red, White};

        let cost = mana(1, vec![Red, White]);
        mode_deltas(phase, leyline, "Deafening Clarion", DEAFENING_CLARION, cost)
    }

    /// CR 601.2b + CR 700.2a: a spell's modes are chosen while it is being cast, so the mode
    /// prompt reads the pending cast's object. Deafening Clarion's lifelink mode (CR 611.2a +
    /// CR 514.2) has its last window in the precombat main and reads as its damage mode; a
    /// postcombat main keeps the penalty. The Leyline run reads the engine query mid-cast, and
    /// there a selection with a lasting mode is neutral.
    #[test]
    fn mode_prompt_of_a_sorcery_confined_spell_prices_its_pump_mode_neutral() {
        let [damage, lifelink, _] = clarion_mode_deltas(Phase::PostCombatMain, false);
        assert!(
            lifelink < damage - 1.5,
            "reach: the trick term fires on this path, {lifelink} vs {damage}"
        );

        let [damage, lifelink, both] = clarion_mode_deltas(Phase::PreCombatMain, false);
        assert_eq!(damage, 0.0, "the damage mode carries no timing term");
        assert_eq!(lifelink, 0.0, "the lifelink mode is in its last window");
        assert_eq!(both, 0.0, "both modes together are in their last window");

        let [damage, _, both] = clarion_mode_deltas(Phase::PreCombatMain, true);
        assert_eq!(damage, 0.0, "the damage mode carries no timing term");
        assert_eq!(both, 0.0, "the damage in both modes lands in any phase");
    }

    /// CR 700.2a: only the chosen modes are read at the prompt. A selection with a lasting mode
    /// is neutral out of combat (CR 611.2a: the tokens stay), the pump mode alone keeps the
    /// price, and entwine's `[0, 1]` (CR 702.42a) reads as any other pair.
    #[test]
    fn mode_prompt_prices_a_selection_by_its_own_modes() {
        use ManaCostShard::Red;

        let phase = Phase::PostCombatMain;
        for (name, [lasting, pump, both]) in [
            ("Deafening Clarion", clarion_mode_deltas(phase, false)),
            (
                "Goblin War Party",
                mode_deltas(
                    phase,
                    false,
                    "Goblin War Party",
                    GOBLIN_WAR_PARTY,
                    mana(3, vec![Red]),
                ),
            ),
        ] {
            assert_eq!(lasting, 0.0, "{name}: the lasting mode carries no price");
            assert!(pump < -1.5, "{name}: the pump mode is priced, {pump}");
            assert_eq!(both, 0.0, "{name}: both modes keep the lasting value");
        }
    }

    /// CR 611.2a + CR 514.2: the trick price needs combat to be the candidate's whole value. A
    /// counter, tokens, a card, life, a damage mode or a Role lands outside combat, so those
    /// candidates take no penalty there, while a pump with only riders does: an untap or a tap,
    /// a group pump, a forced block, an attack or block static, or a delayed removal of what was
    /// pumped. In combat the payload still earns the window (combat is priced by policy, so only
    /// the sign is the card's).
    #[test]
    fn trick_price_needs_combat_to_be_the_whole_value() {
        use ManaCostShard::{Black, Blue, Green, Red, White};

        let mut s = trick_scenario();
        let reinforcements = add_spell(
            &mut s,
            "Heroic Reinforcements",
            false,
            HEROIC_REINFORCEMENTS,
            mana(2, vec![Red, White]),
        );
        let war_party = add_spell(
            &mut s,
            "Goblin War Party",
            false,
            GOBLIN_WAR_PARTY,
            mana(3, vec![Red]),
        );
        let clarion = add_spell(
            &mut s,
            "Deafening Clarion",
            false,
            DEAFENING_CLARION,
            mana(1, vec![Red, White]),
        );
        let suns = add_spell(
            &mut s,
            "Sweltering Suns",
            false,
            SWELTERING_SUNS,
            mana(1, vec![Red, Red]),
        );
        let rage = add_spell(
            &mut s,
            "Monstrous Rage",
            true,
            MONSTROUS_RAGE,
            mana(0, vec![Red]),
        );
        let triumph = add_spell(
            &mut s,
            "Moment of Triumph",
            true,
            MOMENT_OF_TRIUMPH,
            mana(0, vec![White]),
        );
        let afflict = add_spell(&mut s, "Afflict", true, AFFLICT, mana(2, vec![Black]));
        let assault = add_assault(&mut s);
        let giant_growth = add_spell(
            &mut s,
            "Giant Growth",
            true,
            GIANT_GROWTH,
            mana(0, vec![Green]),
        );
        let riders = [
            add_spell(
                &mut s,
                "Gerrard's Command",
                true,
                GERRARDS_COMMAND,
                mana(0, vec![Green, White]),
            ),
            add_spell(
                &mut s,
                "Echoing Courage",
                true,
                ECHOING_COURAGE,
                mana(1, vec![Green]),
            ),
            add_spell(
                &mut s,
                "Stabbing Pain",
                true,
                STABBING_PAIN,
                mana(0, vec![Black]),
            ),
            add_spell(
                &mut s,
                "Monstrous Step",
                false,
                MONSTROUS_STEP,
                mana(4, vec![Green]),
            ),
            add_spell(
                &mut s,
                "Give No Ground",
                true,
                GIVE_NO_GROUND,
                mana(3, vec![White]),
            ),
            add_spell(
                &mut s,
                "Emergent Growth",
                false,
                EMERGENT_GROWTH,
                mana(3, vec![Green]),
            ),
            add_spell(
                &mut s,
                "Invigorated Rampage",
                true,
                INVIGORATED_RAMPAGE,
                mana(1, vec![Red]),
            ),
        ];
        let emperor = s
            .add_planeswalker_from_oracle(P0, "The Wandering Emperor", "", 3, THE_WANDERING_EMPEROR)
            .id();
        let salamander = s
            .add_creature_from_oracle(P0, "Pyric Salamander", 1, 1, PYRIC_SALAMANDER)
            .with_mana_cost(mana(1, vec![Red]))
            .id();
        let vectis = s
            .add_creature_from_oracle(P0, "Vectis Agents", 4, 3, VECTIS_AGENTS)
            .with_mana_cost(mana(3, vec![Blue, Black]))
            .id();
        let fort = s
            .add_creature_from_oracle(P0, "Mobile Fort", 0, 6, MOBILE_FORT)
            .with_mana_cost(mana(4, vec![]))
            .id();
        let mask = s
            .add_artifact_from_oracle(P0, "Dragon Mask", DRAGON_MASK)
            .with_mana_cost(mana(3, vec![]))
            .id();
        let mut runner = s.build();
        runner.state_mut().phase = Phase::PostCombatMain;
        settle(&mut runner);
        set_emperor_entered(&mut runner, emperor, false);
        settle(&mut runner);
        let state = runner.state();

        let d_giant_growth = cast_delta(state, giant_growth);
        assert!(d_giant_growth < -1.5, "reach, got {d_giant_growth}");
        let d_assault = cast_delta(state, assault);
        assert!(d_assault < -1.5, "reach, got {d_assault}");

        for (name, delta) in [
            ("Heroic Reinforcements", cast_delta(state, reinforcements)),
            ("Goblin War Party", cast_delta(state, war_party)),
            ("Monstrous Rage", cast_delta(state, rage)),
            ("Moment of Triumph", cast_delta(state, triumph)),
            (
                "The Wandering Emperor's +1",
                activation_delta(state, emperor, loyalty_index(state, emperor, 1)),
            ),
        ] {
            assert_eq!(delta, 0.0, "{name}'s lasting value takes no penalty");
        }
        let (d_clarion, d_suns) = (cast_delta(state, clarion), cast_delta(state, suns));
        assert!(
            (d_clarion - d_suns).abs() < 1e-9,
            "Deafening Clarion {d_clarion} prices as Sweltering Suns {d_suns}"
        );
        let d_afflict = cast_delta(state, afflict);
        assert!(
            (d_afflict - 0.15).abs() < 1e-9,
            "Afflict keeps only the postcombat draw bonus, got {d_afflict}"
        );

        for rider in riders {
            let delta = cast_delta(state, rider);
            assert!(
                (delta - d_giant_growth).abs() < 1e-9,
                "{} {delta} must price as Giant Growth {d_giant_growth}",
                state.objects[&rider].name
            );
        }
        for id in [salamander, vectis, fort, mask] {
            let delta = activation_delta(state, id, last_activated_index(state, id));
            assert!(
                delta < -1.5,
                "{} is priced, got {delta}",
                state.objects[&id].name
            );
        }

        // The payload still earns the window, and the rest does not cost it.
        let mut state = d1_board(Phase::DeclareAttackers, P0);
        state.combat = Some(engine::game::combat::CombatState::default());
        let growth = oracle_instant(&mut state, P0, "Giant Growth", GIANT_GROWTH);
        let rage = oracle_instant(&mut state, P0, "Monstrous Rage", MONSTROUS_RAGE);
        let afflict = oracle_instant(&mut state, P0, "Afflict", AFFLICT);
        let d_growth = timing_delta(&state, &cast_candidate(&state, growth));
        assert!(d_growth > 0.0, "reach: the window pays, got {d_growth}");
        for (name, id) in [("Monstrous Rage", rage), ("Afflict", afflict)] {
            let delta = timing_delta(&state, &cast_candidate(&state, id));
            assert!(
                (delta - d_growth).abs() < 1e-9,
                "{name} {delta} must keep Giant Growth's window {d_growth}"
            );
        }

        let mut state = d1_board(Phase::End, P0);
        let growth = oracle_instant(&mut state, P0, "Giant Growth", GIANT_GROWTH);
        let rage = oracle_instant(&mut state, P0, "Monstrous Rage", MONSTROUS_RAGE);
        let d_growth = timing_delta(&state, &cast_candidate(&state, growth));
        assert!(d_growth < -1.5, "reach, got {d_growth}");
        assert_eq!(
            timing_delta(&state, &cast_candidate(&state, rage)),
            0.0,
            "the Role outlasts the End step"
        );
    }

    /// CR 606.4 + CR 606.3: a positive loyalty cost keeps its counters whatever the phase, and a
    /// permanent's one loyalty activation of the turn is not banked by passing, so combat is not
    /// the activation's whole value. A negative cost pays counters out for the pump alone. The
    /// effect reading is `Whole` on every row, so the cost is the whole difference; a cost read
    /// through the printed abilities would miss Rowan's Talent's grant.
    #[test]
    fn a_positive_loyalty_cost_is_value_outside_combat() {
        use crate::cast_facts::{collect_definition_effects_with, ModeWalk};

        let mut s = trick_scenario();
        let elspeth = s
            .add_planeswalker_from_oracle(
                P0,
                "Elspeth, Knight-Errant",
                "Elspeth",
                4,
                ELSPETH_KNIGHT_ERRANT,
            )
            .id();
        let sorin = s
            .add_planeswalker_from_oracle(P0, "Sorin, Vampire Lord", "Sorin", 4, SORIN_VAMPIRE_LORD)
            .id();
        let nissa = s
            .add_planeswalker_from_oracle(P0, "Nissa, Genesis Mage", "Nissa", 5, NISSA_GENESIS_MAGE)
            .id();
        let garruk = s
            .add_planeswalker_from_oracle(P0, "Garruk Wildspeaker", "Garruk", 4, GARRUK_WILDSPEAKER)
            .id();
        let talent = s
            .add_enchantment_from_oracle(P0, "Rowan's Talent", ROWANS_TALENT)
            .with_subtypes(vec!["Aura"])
            .id();
        let mut runner = s.build();
        runner.state_mut().phase = Phase::PostCombatMain;
        engine::game::effects::attach::attach_to(runner.state_mut(), talent, garruk);
        settle(&mut runner);
        let state = runner.state();

        let elspeth_pump = last_loyalty_index(state, elspeth, 1);
        let granted = last_loyalty_index(state, garruk, 1);
        assert!(
            granted >= GARRUK_WILDSPEAKER.lines().count(),
            "reach: Rowan's Talent's [+1] follows Garruk's printed abilities, got {granted}"
        );
        let sorin_pump = loyalty_index(state, sorin, 1);
        let nissa_pump = loyalty_index(state, nissa, -3);
        let garruk_pump = loyalty_index(state, garruk, -4);
        for (name, source, index) in [
            ("Elspeth's +1", elspeth, elspeth_pump),
            ("Sorin's +1", sorin, sorin_pump),
            ("Rowan's Talent's +1", garruk, granted),
            ("Nissa's −3", nissa, nissa_pump),
            ("Garruk's −4", garruk, garruk_pump),
        ] {
            let walk = collect_definition_effects_with(
                &state.objects[&source].abilities[index],
                ModeWalk::RootOnly,
            );
            assert_eq!(
                CombatBinding::of_resolution(&walk),
                CombatBinding::Whole,
                "premise: {name}'s effects read as a whole trick"
            );
        }

        for (name, delta) in [
            (
                "Elspeth's +1",
                activation_delta(state, elspeth, elspeth_pump),
            ),
            ("Sorin's +1", activation_delta(state, sorin, sorin_pump)),
            (
                "Rowan's Talent's +1",
                activation_delta(state, garruk, granted),
            ),
        ] {
            assert_eq!(delta, 0.0, "{name} keeps its counter out of combat");
        }
        for (name, delta) in [
            ("Nissa's −3", activation_delta(state, nissa, nissa_pump)),
            ("Garruk's −4", activation_delta(state, garruk, garruk_pump)),
        ] {
            assert!(delta < -1.5, "{name} is priced, got {delta}");
        }
    }
}
