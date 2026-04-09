use engine::game::players;
use engine::game::turn_control;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::eval::{evaluate_creature, threat_level, StrategicIntent};

use super::context::{collect_ability_effects, PolicyContext};
use super::registry::TacticalPolicy;
use super::stack_awareness::assess_spell_impact;
use super::strategy_helpers::visible_opponent_creature_value;

pub struct EffectTimingPolicy;

impl TacticalPolicy for EffectTimingPolicy {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
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

            // Removal pre-combat bonus: opens combat lanes by removing blockers
            if matches!(ctx.state.phase, Phase::PreCombatMain) {
                if let Some(facts) = ctx.cast_facts() {
                    if facts.has_direct_removal_text
                        && visible_opponent_creature_value(ctx.state, ctx.ai_player) > 0.0
                    {
                        score += 0.2;
                    }
                }
            }

            // Draw post-combat bonus: draw after combat decisions are resolved
            if matches!(ctx.state.phase, Phase::PostCombatMain) {
                if let Some(facts) = ctx.cast_facts() {
                    if facts.has_draw {
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
    let opponents = players::opponents(ctx.state, ctx.ai_player);
    let max_threat = ctx
        .state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let object = ctx.state.objects.get(&id)?;
            if opponents.contains(&object.controller)
                && object.card_types.core_types.contains(&CoreType::Creature)
            {
                let creature_value = evaluate_creature(ctx.state, id);
                let threat_weight = threat_level(ctx.state, ctx.ai_player, object.controller) + 0.5;
                Some(creature_value * threat_weight)
            } else {
                None
            }
        })
        .fold(0.0_f64, f64::max);

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
        StrategicIntent::Stabilize => 0.1,
        _ => 0.0,
    };
    let stack_pressure = if ctx.state.stack.is_empty() {
        0.0
    } else {
        (0.8 * patience) + intent_bonus
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

/// Check if any opponent counter spell on the stack threatens one of the AI's spells.
/// Returns the impact value of the most valuable threatened spell, or 0.0 if none.
fn threatened_own_spell_value(state: &GameState, ai_player: PlayerId) -> f64 {
    let mut max_value = 0.0_f64;

    for entry in state.stack.iter() {
        if entry.controller == ai_player {
            continue;
        }
        let Some(ability) = entry.ability() else {
            continue;
        };
        let has_counter = collect_ability_effects(ability)
            .iter()
            .any(|e| matches!(e, Effect::Counter { .. }));
        if !has_counter {
            continue;
        }
        // Find which AI spell this counter targets
        for target in &ability.targets {
            let TargetRef::Object(target_id) = target else {
                continue;
            };
            if let Some(threatened) = state.stack.iter().find(|e| e.id == *target_id) {
                if threatened.controller == ai_player {
                    max_value = max_value.max(assess_spell_impact(state, threatened));
                }
            }
        }
    }

    max_value
}

fn combat_trick_score(ctx: &PolicyContext<'_>) -> f64 {
    // Pump effects expire at cleanup — casting during End/Cleanup has no lasting impact.
    // Penalty must exceed max search continuation bonus to prevent selection.
    if matches!(ctx.state.phase, Phase::End | Phase::Cleanup) {
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
        -0.5 * patience
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;

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
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = combat_trick_score(&ctx);
        assert!(
            score < -1.5,
            "Combat trick should be strongly penalized during End step, got {score}"
        );
    }
}
