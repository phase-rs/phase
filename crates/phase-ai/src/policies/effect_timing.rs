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
use super::effect_classify::{extract_target_filter, targets_creatures_only};
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
                Effect::Pump { .. } | Effect::DoublePT { .. } => combat_trick_score(ctx),
                _ => 0.0,
            };
        }

        score
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
    let pump_response = if !ctx.state.stack.is_empty()
        && ctx.state.stack.iter().any(|entry| {
            entry.controller != ctx.ai_player
                && entry
                    .ability()
                    .map(|a| {
                        collect_ability_effects(a)
                            .iter()
                            .any(|e| matches!(e, Effect::Pump { .. } | Effect::DoublePT { .. }))
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
/// matcher is not reachable from here), so a counter that can only hit creature
/// spells still sees a noncreature entry's impact. Accepted — the filter narrows
/// the set, so this can only over-estimate, and `SelectTarget` still picks
/// legally.
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
    // Pump effects expire at cleanup — casting outside combat has no lasting impact.
    // Penalty must exceed max search continuation bonus to prevent selection.
    if matches!(
        ctx.state.phase,
        Phase::End | Phase::Cleanup | Phase::Untap | Phase::Upkeep | Phase::Draw
    ) {
        return -2.0;
    }

    // Main phases with no active combat: pump spells waste mana for zero board impact.
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
    use engine::game::scenario::{GameScenario, P0};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification,
        ControllerRef, Duration, MultiTargetSpec, ResolvedAbility, StaticDefinition, TargetFilter,
        TargetRef, TypeFilter, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{
        ExtraPhase, GameState, PendingCast, StackEntryKind, TargetEffectDetail,
        TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
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
            P0,
            &config,
            &mut rng,
            &crate::AiSession::arc_from_game(state),
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
        state.phase = Phase::DeclareAttackers;
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
                if reason.kind == "effect_timing_no_pair_blockable_evasion_target"
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
                        target: TargetFilter::Controller,
                        phase: Phase::BeginCombat,
                        after: Phase::PreCombatMain,
                        followed_by: vec![Phase::PostCombatMain],
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
            })
            .expect("reach guard: the extra-combat activation must be legal");
        runner.advance_until_stack_empty();
        assert!(runner
            .state()
            .extra_phases
            .iter()
            .any(|extra| extra.phase == Phase::BeginCombat));
        assert!(get_valid_attacker_ids(runner.state()).contains(&ready));

        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
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
        assert!(
            runner
                .state()
                .extra_phases
                .iter()
                .any(|extra| extra.phase == Phase::BeginCombat
                    && extra.attacker_restriction.is_some())
        );
        assert!(get_valid_attacker_ids(runner.state()).contains(&unchosen));

        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
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
            phase: Phase::BeginCombat,
            attacker_restriction: None,
            attacker_restriction_source: None,
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
                        target: TargetFilter::Controller,
                        phase: Phase::BeginCombat,
                        after: Phase::PreCombatMain,
                        followed_by: vec![Phase::PostCombatMain],
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
            .any(|e| e.phase == Phase::BeginCombat));
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
            target: TargetFilter::Any,
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
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
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
}
