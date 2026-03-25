use engine::game::players;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

use crate::eval::{evaluate_creature, threat_level, StrategicIntent};

use super::context::PolicyContext;
use super::registry::TacticalPolicy;

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

            let is_pre_combat_preferred =
                object.card_types.core_types.contains(&CoreType::Creature)
                    || object.card_types.subtypes.iter().any(|s| s == "Aura");
            if is_pre_combat_preferred {
                if matches!(ctx.state.phase, Phase::PreCombatMain) {
                    0.35
                } else {
                    0.1
                }
            } else {
                0.0
            }
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

    0.3 + (max_threat / 25.0).min(0.8) + stabilize_bonus
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
    let is_own_turn = ctx.state.active_player == ctx.ai_player;
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

    if matches!(ctx.decision.waiting_for, WaitingFor::Priority { .. }) {
        if !is_own_turn && stack_pressure > 0.0 {
            stack_pressure
        } else {
            -0.6 * patience
        }
    } else {
        stack_pressure
    }
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
        };

        let score = combat_trick_score(&ctx);
        assert!(
            score < -1.5,
            "Combat trick should be strongly penalized during End step, got {score}"
        );
    }
}
