use engine::game::keywords::has_flash;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::TacticalPolicy;
use crate::zone_eval;

pub struct ManaEfficiencyPolicy;

impl TacticalPolicy for ManaEfficiencyPolicy {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if !is_own_main_phase(ctx) {
            return 0.0;
        }

        if matches!(ctx.candidate.action, GameAction::PassPriority) {
            let holdback = instant_speed_mana_needed(ctx.state, ctx.ai_player);
            let available = zone_eval::available_mana(ctx.state, ctx.ai_player) as usize;
            let wasteable = available.saturating_sub(holdback);
            let total = total_mana_sources(ctx.state, ctx.ai_player);
            if total == 0 {
                return 0.0;
            }
            let waste_ratio = wasteable as f64 / total as f64;
            let patience_scale = 1.0 - ctx.config.profile.interaction_patience;
            -waste_ratio * patience_scale * 0.4
        } else if let Some(mv) = spell_mana_value(ctx) {
            let available = zone_eval::available_mana(ctx.state, ctx.ai_player) as usize;
            if available == 0 {
                return 0.0;
            }
            (mv as f64 / available as f64).min(1.0) * 0.2
        } else {
            0.0
        }
    }
}

fn is_own_main_phase(ctx: &PolicyContext<'_>) -> bool {
    ctx.state.active_player == ctx.ai_player
        && matches!(
            ctx.state.phase,
            Phase::PreCombatMain | Phase::PostCombatMain
        )
        && ctx.state.stack.is_empty()
}

/// Count all lands (tapped or untapped) controlled by the player.
fn total_mana_sources(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|&&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player && obj.card_types.core_types.contains(&CoreType::Land)
            })
        })
        .count()
}

/// If the candidate action is casting a spell, return its mana value; otherwise None.
fn spell_mana_value(ctx: &PolicyContext<'_>) -> Option<u32> {
    if let GameAction::CastSpell { card_id, .. } = &ctx.candidate.action {
        ctx.state
            .objects
            .values()
            .find(|obj| obj.card_id == *card_id)
            .map(|obj| obj.mana_cost.mana_value())
    } else {
        None
    }
}

/// Maximum mana value among instant-speed spells in the player's hand.
/// Considers instants and creatures with Flash.
fn instant_speed_mana_needed(state: &GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter_map(|&id| state.objects.get(&id))
        .filter(|obj| {
            obj.card_types.core_types.contains(&CoreType::Instant)
                || (obj.card_types.core_types.contains(&CoreType::Creature) && has_flash(obj))
        })
        .map(|obj| obj.mana_cost.mana_value() as usize)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn make_pass_candidate(player: PlayerId) -> CandidateAction {
        CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata {
                actor: Some(player),
                tactical_class: TacticalClass::Pass,
            },
        }
    }

    fn make_cast_candidate(
        object_id: ObjectId,
        card_id: CardId,
        player: PlayerId,
    ) -> CandidateAction {
        CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(player),
                tactical_class: TacticalClass::Spell,
            },
        }
    }

    fn make_priority_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        }
    }

    fn add_land(state: &mut GameState, player: PlayerId, tapped: bool) -> ObjectId {
        let id = create_object(
            state,
            CardId(100),
            player,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.controller = player;
        obj.tapped = tapped;
        id
    }

    fn add_instant_to_hand(state: &mut GameState, player: PlayerId, mv: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(200),
            player,
            "Counterspell".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.controller = player;
        obj.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: mv,
        };
        id
    }

    #[test]
    fn no_penalty_outside_main_phase() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::BeginCombat;
        state.active_player = PlayerId(0);
        // Add 3 untapped lands — if main phase logic applied this would produce a penalty
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);

        let config = AiConfig::default();
        let decision = make_priority_decision();
        let candidate = make_pass_candidate(PlayerId(0));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        assert_eq!(
            ManaEfficiencyPolicy.score(&ctx),
            0.0,
            "Should return 0.0 outside main phase"
        );
    }

    #[test]
    fn penalty_scales_with_waste() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        // 4 untapped lands with nothing to cast
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);

        // Low patience so we see a real penalty
        let mut config = AiConfig::default();
        config.profile.interaction_patience = 0.0;

        let decision = make_priority_decision();
        let candidate = make_pass_candidate(PlayerId(0));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = ManaEfficiencyPolicy.score(&ctx);
        assert!(
            score < -0.3,
            "Should apply meaningful penalty for wasted mana, got {score}"
        );
    }

    #[test]
    fn instant_in_hand_reduces_penalty() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        // 4 untapped lands + a 2-mana instant in hand
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_instant_to_hand(&mut state, PlayerId(0), 2);

        let mut config = AiConfig::default();
        config.profile.interaction_patience = 0.0;

        let mut state_no_instant = GameState::new_two_player(42);
        state_no_instant.phase = Phase::PreCombatMain;
        state_no_instant.active_player = PlayerId(0);
        add_land(&mut state_no_instant, PlayerId(0), false);
        add_land(&mut state_no_instant, PlayerId(0), false);
        add_land(&mut state_no_instant, PlayerId(0), false);
        add_land(&mut state_no_instant, PlayerId(0), false);

        let decision = make_priority_decision();
        let candidate = make_pass_candidate(PlayerId(0));

        let ctx_with = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let ctx_without = PolicyContext {
            state: &state_no_instant,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score_with = ManaEfficiencyPolicy.score(&ctx_with);
        let score_without = ManaEfficiencyPolicy.score(&ctx_without);
        assert!(
            score_with > score_without,
            "Holding an instant should reduce the waste penalty: {score_with} > {score_without}"
        );
    }

    #[test]
    fn high_patience_reduces_penalty() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);

        let mut config_patient = AiConfig::default();
        config_patient.profile.interaction_patience = 1.0;

        let decision = make_priority_decision();
        let candidate = make_pass_candidate(PlayerId(0));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config_patient,
            context: &crate::context::AiContext::empty(&config_patient.weights),
        };

        let score = ManaEfficiencyPolicy.score(&ctx);
        // patience_scale = 1.0 - 1.0 = 0.0, so penalty approaches 0
        assert!(
            score.abs() < 0.001,
            "interaction_patience=1.0 should produce near-zero penalty, got {score}"
        );
    }

    #[test]
    fn casting_spell_gets_efficiency_bonus() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        // 3 untapped lands = 3 available mana
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);
        add_land(&mut state, PlayerId(0), false);

        // Add a 3-mana sorcery as the spell being cast
        let card_id = CardId(50);
        let obj_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }

        let config = AiConfig::default();
        let decision = make_priority_decision();
        let candidate = make_cast_candidate(obj_id, card_id, PlayerId(0));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = ManaEfficiencyPolicy.score(&ctx);
        assert!(
            score > 0.0,
            "Casting a spell using most of available mana should give a positive score, got {score}"
        );
    }
}
