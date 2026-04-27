use engine::game::players;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use crate::eval::board_stats;
use crate::features::DeckFeatures;

use super::activation::arch_times_turn;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::deck_profile::DeckArchetype;

pub struct LifeTotalResourcePolicy;

impl LifeTotalResourcePolicy {
    fn archetype_scale(archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 1.3,
            DeckArchetype::Control => 0.8,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.0,
            DeckArchetype::Combo => 1.0,
        }
    }

    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => {}
            _ => return 0.0,
        }

        let ai_life = ctx.state.players[ctx.ai_player.0 as usize].life;
        let opponents = players::opponents(ctx.state, ctx.ai_player);

        // Calculate opponent total power
        let opp_total_power: i32 = opponents
            .iter()
            .map(|&opp| {
                let (_, power, _, _) = board_stats(ctx.state, opp);
                power
            })
            .sum();

        // Calculate AI total power
        let (_, ai_power, _, _) = board_stats(ctx.state, ctx.ai_player);

        let min_opp_life = opponents
            .iter()
            .map(|&opp| ctx.state.players[opp.0 as usize].life)
            .min()
            .unwrap_or(20);

        let ai_critical = ai_life <= 5 || ai_life <= opp_total_power;
        let opp_critical = min_opp_life <= 5 || min_opp_life <= ai_power;

        if !ai_critical && !opp_critical {
            return 0.0;
        }

        let facts = match ctx.cast_facts() {
            Some(f) => f,
            None => return 0.0,
        };

        let mut score = 0.0;

        if ai_critical {
            score += score_defensive_play(ctx, facts.object, &facts);
        }

        if opp_critical {
            score += score_aggressive_close(facts.object, &facts);
        }

        score
    }
}

impl TacticalPolicy for LifeTotalResourcePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LifeTotalResource
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        arch_times_turn(features, state, Self::archetype_scale)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("life_total_resource_score"),
        }
    }
}

/// Score defensive card choices when AI life is critical.
fn score_defensive_play(
    ctx: &PolicyContext<'_>,
    obj: &engine::game::game_object::GameObject,
    facts: &crate::cast_facts::CastFacts<'_>,
) -> f64 {
    let mut score = 0.0;

    if obj.card_types.core_types.contains(&CoreType::Creature) {
        let power = obj.power.unwrap_or(0);
        let toughness = obj.toughness.unwrap_or(0);

        if toughness >= power {
            // Defensive body — can block effectively
            score += ctx.penalties().low_life_defensive_bonus;
        } else if toughness <= 1
            && !obj.has_keyword(&Keyword::Flying)
            && !obj.has_keyword(&Keyword::Menace)
        {
            // Pure aggro creature with no evasion — bad when under pressure
            score += ctx.penalties().low_life_aggro_penalty;
        }
    }

    // Removal is valuable when under lethal pressure
    if facts.has_direct_removal_text() {
        score += 0.4;
    }

    score
}

/// Score aggressive plays when opponent life is critical.
fn score_aggressive_close(
    obj: &engine::game::game_object::GameObject,
    facts: &crate::cast_facts::CastFacts<'_>,
) -> f64 {
    let mut score = 0.0;

    if obj.card_types.core_types.contains(&CoreType::Creature) {
        // Evasive creatures can close the game
        if obj.has_keyword(&Keyword::Flying)
            || obj.has_keyword(&Keyword::Menace)
            || obj.has_keyword(&Keyword::Shadow)
        {
            score += 0.2;
        }
    }

    // Burn can close the game (but don't bonus if lethal_burn_bonus already handles it)
    if facts.has_direct_removal_text()
        && facts.primary_effects.iter().any(|a| {
            matches!(
                &*a.effect,
                engine::types::ability::Effect::DealDamage { .. }
            )
        })
    {
        score += 0.3;
    }

    // Pure lifegain without board impact is less useful when closing
    if !obj.card_types.core_types.contains(&CoreType::Creature)
        && facts
            .primary_effects
            .iter()
            .all(|a| matches!(&*a.effect, engine::types::ability::Effect::GainLife { .. }))
        && !facts.primary_effects.is_empty()
    {
        score -= 0.2;
    }

    score
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    #[test]
    fn rewards_defensive_creature_when_low_life() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 4;

        // Opponent has threatening power
        let opp = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Threat".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&opp).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(5);
        obj.toughness = Some(5);

        // AI casting a defensive creature (1/3)
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Wall".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(3);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        ));

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell,
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

        let score = LifeTotalResourcePolicy.score(&ctx);
        assert!(
            score > 0.2,
            "Should reward defensive creature when low life, got {score}"
        );
    }

    #[test]
    fn no_adjustment_at_healthy_life() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[1].life = 20;

        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        ));

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell,
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

        let score = LifeTotalResourcePolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No adjustment at healthy life, got {score}"
        );
    }
}
