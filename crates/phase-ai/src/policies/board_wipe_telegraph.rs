use engine::game::players;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;

use super::context::PolicyContext;
use super::registry::TacticalPolicy;
use super::strategy_helpers::is_own_main_phase;
use crate::config::ThreatAwareness;
use crate::deck_profile::DeckArchetype;
use crate::threat_profile::castable_probabilities;

pub struct BoardWipeTelegraphPolicy;

impl TacticalPolicy for BoardWipeTelegraphPolicy {
    fn archetype_scale(&self, archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 0.5,
            DeckArchetype::Control => 1.5,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.0,
            DeckArchetype::Combo => 1.0,
        }
    }

    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        // Guard: only CastSpell during own main phase
        if !matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            return 0.0;
        }
        if !is_own_main_phase(ctx) {
            return 0.0;
        }

        // Only applies when casting creatures/permanents
        let facts = match ctx.cast_facts() {
            Some(f) => f,
            None => return 0.0,
        };
        if !facts
            .object
            .card_types
            .core_types
            .iter()
            .any(|t| matches!(t, CoreType::Creature | CoreType::Planeswalker))
        {
            return 0.0;
        }

        let opponents = players::opponents(ctx.state, ctx.ai_player);

        // AI creature count (used by both paths)
        let ai_creatures = ctx
            .state
            .battlefield
            .iter()
            .filter(|&&id| {
                ctx.state.objects.get(&id).is_some_and(|obj| {
                    obj.controller == ctx.ai_player
                        && obj.card_types.core_types.contains(&CoreType::Creature)
                })
            })
            .count();

        // When Full threat profile is active, use probability-based wrath_risk
        // and zero out the heuristic to prevent double-penalty with the eval-level
        // threat_adjustment().
        let wrath_risk = if ctx.config.search.threat_awareness == ThreatAwareness::Full {
            if let Some(threat) = &ctx.context.opponent_threat {
                let primary_opp = opponents.first().copied().unwrap_or(ctx.ai_player);
                castable_probabilities(threat, ctx.state, primary_opp).board_wipe
            } else {
                0.0
            }
        } else {
            // Heuristic path for None/ArchetypeOnly modes.
            // These are stable heuristic weights representing fixed signal strengths
            // of observable board indicators, not AI personality parameters.
            let mut risk = 0.0;

            let opp_has_mana = opponents
                .iter()
                .any(|&opp| crate::zone_eval::available_mana(ctx.state, opp) >= 4);
            if opp_has_mana {
                risk += 0.3;
            }

            let opp_has_no_creatures = !ctx.state.battlefield.iter().any(|&id| {
                ctx.state.objects.get(&id).is_some_and(|obj| {
                    opponents.contains(&obj.controller)
                        && obj.card_types.core_types.contains(&CoreType::Creature)
                })
            });
            if opp_has_no_creatures {
                risk += 0.3;
            }

            let opp_has_hand = opponents
                .iter()
                .any(|&opp| ctx.state.players[opp.0 as usize].hand.len() >= 2);
            if opp_has_hand {
                risk += 0.2;
            }

            if ai_creatures >= 3 {
                risk += 0.2;
            }

            risk
        };

        // Only penalize when risk is substantial and AI already has board presence
        if wrath_risk < 0.5 || ai_creatures < 2 {
            return 0.0;
        }

        let mut penalty = -((ai_creatures as f64 - 1.0)
            * ctx.penalties().wrath_overextend_penalty.abs())
            * wrath_risk;

        // Reduce penalty for creatures that provide immediate value
        if facts.object.has_keyword(&Keyword::Haste) {
            penalty *= 0.5;
        }
        if !facts.immediate_etb_triggers.is_empty() {
            penalty *= 0.6;
        }

        penalty.max(-2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    #[test]
    fn penalizes_overextension_into_wrath() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.turn_number = 5;

        // AI already has 3 creatures
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("Creature {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Opponent: no creatures, 2 cards in hand, 5 untapped lands
        state.players[1].hand = vec![
            engine::types::identifiers::ObjectId(90),
            engine::types::identifiers::ObjectId(91),
        ];
        for i in 10..15 {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(1),
                format!("Land {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
        }

        // Spell: another creature
        let spell = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Another Bear".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);

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
                card_id: CardId(50),
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

        let score = BoardWipeTelegraphPolicy.score(&ctx);
        assert!(
            score < -0.3,
            "Should penalize overextension into wrath risk, got {score}"
        );
    }

    #[test]
    fn no_penalty_when_low_wrath_risk() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.turn_number = 3;

        // Opponent has creatures (would lose from their own wipe)
        let opp_creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&opp_creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);

        // AI has 1 creature (not overextended)
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);

        let spell = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Another Bear".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);

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
                card_id: CardId(50),
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

        let score = BoardWipeTelegraphPolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No penalty when wrath risk is low, got {score}"
        );
    }
}
