use engine::game::turn_control;
use engine::types::ability::Effect;
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
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
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
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("effect_timing_score"),
        }
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
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{ResolvedAbility, TargetFilter, TargetRef};
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    /// The AI's seat in the counterspell fixtures; every other seat is foreign.
    const AI: PlayerId = PlayerId(1);

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
