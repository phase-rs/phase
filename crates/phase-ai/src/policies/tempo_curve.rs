use engine::types::actions::GameAction;

use super::context::PolicyContext;
use super::registry::TacticalPolicy;
use super::strategy_helpers::is_own_main_phase;
use crate::deck_profile::DeckArchetype;
use crate::zone_eval;

/// Rewards playing spells on-curve (matching mana value to available mana),
/// with stronger incentive in the early game (turns 1-4).
///
/// Differentiated from `ManaEfficiencyPolicy`: that policy rewards spending
/// mana (any spell vs passing). This policy rewards *which* spell to cast
/// by preferring higher mana-value plays that use more of the available mana,
/// especially during the critical early turns when falling off-curve is costly.
pub struct TempoCurvePolicy;

impl TacticalPolicy for TempoCurvePolicy {
    fn archetype_scale(&self, archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 1.8,
            DeckArchetype::Control => 0.5,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.3,
            DeckArchetype::Combo => 0.8,
        }
    }

    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if !is_own_main_phase(ctx) {
            return 0.0;
        }

        let GameAction::CastSpell { card_id, .. } = &ctx.candidate.action else {
            return 0.0;
        };

        let object = ctx
            .state
            .objects
            .values()
            .find(|obj| obj.card_id == *card_id);

        let Some(object) = object else {
            return 0.0;
        };

        let mana_value = object.mana_cost.mana_value();
        if mana_value == 0 {
            return 0.0;
        }

        let available = zone_eval::available_mana(ctx.state, ctx.ai_player);
        if available == 0 {
            return 0.0;
        }

        let efficiency = (mana_value as f64 / available as f64).min(1.0);

        // Early game (turns 1-4) gets full bonus; later turns decay to 40% floor.
        let turn = ctx.state.turn_number;
        let early_game_scale = if turn <= 4 {
            1.0
        } else {
            let decay = ((turn - 4) as f64 * 0.15).min(0.6);
            1.0 - decay
        };

        efficiency * early_game_scale * ctx.config.policy_penalties.tempo_curve_bonus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn setup_main_phase(turn: u32, untapped_lands: u32) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.turn_number = turn;
        for _ in 0..untapped_lands {
            let id = create_object(
                &mut state,
                CardId(100),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.controller = PlayerId(0);
        }
        state
    }

    fn make_spell(state: &mut GameState, mana_value: u32) -> (ObjectId, CardId) {
        let card_id = CardId(50);
        let obj_id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: mana_value,
        };
        (obj_id, card_id)
    }

    fn make_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        }
    }

    #[test]
    fn on_curve_early_game_gets_full_bonus() {
        // Turn 3, 3 lands, casting a 3-drop = perfect curve
        let mut state = setup_main_phase(3, 3);
        let (obj_id, card_id) = make_spell(&mut state, 3);

        let config = AiConfig::default();
        let decision = make_decision();
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: obj_id,
                card_id,
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

        let score = TempoCurvePolicy.score(&ctx);
        // 1.0 efficiency * 1.0 early_game_scale * 0.3 bonus = 0.3
        assert!(
            (score - 0.3).abs() < 0.01,
            "Perfect on-curve early game should get near-max bonus, got {score}"
        );
    }

    #[test]
    fn returns_zero_outside_main_phase() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::BeginCombat;
        state.active_player = PlayerId(0);
        state.turn_number = 3;
        let (obj_id, card_id) = make_spell(&mut state, 3);

        let config = AiConfig::default();
        let decision = make_decision();
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: obj_id,
                card_id,
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

        assert_eq!(
            TempoCurvePolicy.score(&ctx),
            0.0,
            "Should return 0.0 outside main phase"
        );
    }

    #[test]
    fn under_curve_gets_reduced_bonus() {
        // Turn 3, 3 lands, casting a 1-drop = low efficiency
        let mut state = setup_main_phase(3, 3);
        let (obj_id, card_id) = make_spell(&mut state, 1);

        let config = AiConfig::default();
        let decision = make_decision();
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: obj_id,
                card_id,
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

        let score = TempoCurvePolicy.score(&ctx);
        assert!(
            score < 0.15,
            "1-drop with 3 mana available should get reduced bonus, got {score}"
        );
        assert!(score > 0.0, "Should still be positive, got {score}");
    }

    #[test]
    fn late_game_reduces_bonus() {
        // Turn 8, 5 lands, casting a 5-drop = perfect efficiency but late game
        let mut state = setup_main_phase(8, 5);
        let (obj_id, card_id) = make_spell(&mut state, 5);

        let config = AiConfig::default();
        let decision = make_decision();
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: obj_id,
                card_id,
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

        let score = TempoCurvePolicy.score(&ctx);
        // efficiency=1.0, early_game_scale = 1.0 - min(0.6, 0.6) = 0.4, bonus=0.3
        // => 0.12
        assert!(
            score < 0.2,
            "Late game should get reduced bonus, got {score}"
        );
        assert!(score > 0.0, "Should still be positive, got {score}");
    }
}
