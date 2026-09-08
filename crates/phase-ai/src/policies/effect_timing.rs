use engine::ai_support::current_target_selection_targets;
use engine::game::combat::{
    attacker_blockability_in_maximum_free_declaration, defending_player_for_attacker,
    MaximumBlockDeclarationBlockability,
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
    if !is_single_target_unblockable_activation(ctx)
        || !matches!(ctx.state.phase, Phase::DeclareAttackers)
        || prompt_has_team_defender(ctx.state)
    {
        return PolicyVerdict::neutral(PolicyReason::new("effect_timing_evasion_target_na"));
    }

    let TargetRef::Object(target_id) = target else {
        return PolicyVerdict::neutral(PolicyReason::new("effect_timing_evasion_target_na"));
    };
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
    use engine::game::combat::{get_valid_block_targets, AttackTarget, AttackerInfo, CombatState};
    use engine::game::scenario::{GameScenario, P0};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification,
        ControllerRef, Duration, MultiTargetSpec, ResolvedAbility, StaticDefinition, TargetFilter,
        TargetRef, TypeFilter, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{
        GameState, PendingCast, StackEntryKind, TargetEffectDetail, TargetSelectionProgress,
        TargetSelectionSlot, WaitingFor,
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
                if delta < 0.0 && reason.kind == "effect_timing_futile_evasion_target"
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
        state.combat = Some(CombatState {
            attackers: vec![declared_attacker(mapped, PlayerId(2))],
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
        assert!(matches!(
            effect_timing_verdict(&state, &target_candidate(futile), &AiConfig::default()),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "effect_timing_evasion_target_na"
        ));
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
