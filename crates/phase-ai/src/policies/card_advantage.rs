use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::activation::arch_times_turn;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::deck_profile::DeckArchetype;
use crate::features::DeckFeatures;

pub struct CardAdvantagePolicy;

impl CardAdvantagePolicy {
    fn archetype_scale(archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 0.5,
            DeckArchetype::Control => 1.8,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.0,
            DeckArchetype::Combo => 1.5,
        }
    }

    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => {}
            _ => return 0.0,
        }

        let facts = match ctx.cast_facts() {
            Some(f) => f,
            None => return 0.0,
        };

        // Only relevant for card-generating spells
        if !facts.has_draw() && !facts.has_token_creation() {
            return 0.0;
        }

        // This policy provides ONLY positional/differential adjustment.
        // Flat draw bonuses already exist in best_proactive_cast_score (+0.1)
        // and EtbValuePolicy (+0.18 for draw ETBs).
        let differential = crate::card_advantage::differential(ctx.state, ctx.ai_player);

        let positional_bonus = if differential < -2.0 {
            // Behind on cards — card generation is extra valuable
            ctx.penalties().card_advantage_behind_extra
        } else if differential > 2.0 {
            // Ahead on cards — no additional bonus
            0.0
        } else {
            // Neutral — mild preference for card generation
            0.05
        };

        // Scale by game phase: early draws matter more.
        // Floor is 0.2 at turn 8+, meaning the bonus never fully disappears.
        let turn = ctx.state.turn_number.min(8) as f64;
        let turn_scale = (10.0 - turn) / 10.0;

        positional_bonus * turn_scale
    }
}

impl TacticalPolicy for CardAdvantagePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CardAdvantage
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
            reason: PolicyReason::new("card_advantage_score"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    #[test]
    fn bonus_when_behind_on_cards() {
        let mut state = GameState::new_two_player(42);
        // Opponent has larger hand (AI behind on cards)
        state.players[1].hand = (0..5)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(80 + i),
                    PlayerId(1),
                    format!("Opp Card {i}"),
                    Zone::Hand,
                )
            })
            .collect();

        // AI's draw spell
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
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

        let score = CardAdvantagePolicy.score(&ctx);
        assert!(
            score > 0.05,
            "Should bonus card draw when behind on cards, got {score}"
        );
    }

    #[test]
    fn no_bonus_for_non_draw_spells() {
        let mut state = GameState::new_two_player(42);
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: engine::types::ability::TargetFilter::Any,
                damage_source: None,
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

        let score = CardAdvantagePolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No bonus for non-draw spell, got {score}"
        );
    }
}
