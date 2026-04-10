use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::TacticalPolicy;
use super::strategy_helpers::{is_own_main_phase, opponent_lethal_damage};
use crate::deck_profile::DeckArchetype;

pub struct LethalityAwarenessPolicy;

impl TacticalPolicy for LethalityAwarenessPolicy {
    fn archetype_scale(&self, archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 1.5,
            DeckArchetype::Control => 1.0,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.0,
            DeckArchetype::Combo => 1.0,
        }
    }

    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let ai_life = ctx.state.players[ctx.ai_player.0 as usize].life;
        let lethal_damage = opponent_lethal_damage(ctx.state, ctx.ai_player);

        // Only relevant when opponent threatens lethal
        if lethal_damage < ai_life {
            return 0.0;
        }

        match &ctx.candidate.action {
            GameAction::CastSpell { .. } if is_own_main_phase(ctx) => {
                score_cast_under_lethal(ctx, ai_life, lethal_damage)
            }
            GameAction::PassPriority if is_own_main_phase(ctx) => {
                score_pass_under_lethal(ctx.state, ctx.ai_player)
            }
            _ => 0.0,
        }
    }
}

/// Penalize tapping out for non-removal when opponent has lethal on board.
/// Only applies when the spell would consume most remaining mana, leaving
/// no resources for instant-speed interaction.
fn score_cast_under_lethal(ctx: &PolicyContext<'_>, _ai_life: i32, _lethal_damage: i32) -> f64 {
    // If this spell IS removal, it might address the lethal threat
    if let Some(facts) = ctx.cast_facts() {
        if facts.has_direct_removal_text() {
            return 0.5;
        }

        // Only penalize if casting this spell leaves <= 1 mana open (tapout)
        let available = crate::zone_eval::available_mana(ctx.state, ctx.ai_player) as i32;
        let spell_mv = facts.mana_value as i32;
        if available - spell_mv > 1 {
            return 0.0;
        }
    }
    // Non-removal that taps us out: penalize under lethal
    ctx.penalties().lethality_tapout_penalty
}

/// Bonus for passing priority when holding instant-speed interaction under lethal threat.
/// Only triggers for actual interaction (removal, damage, counters) — not cantrips.
fn score_pass_under_lethal(state: &GameState, ai_player: PlayerId) -> f64 {
    use engine::types::ability::Effect;

    let has_instant_interaction = state.players[ai_player.0 as usize]
        .hand
        .iter()
        .any(|&obj_id| {
            state.objects.get(&obj_id).is_some_and(|obj| {
                obj.card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Instant)
                    && obj.abilities.iter().any(|a| {
                        matches!(
                            &*a.effect,
                            Effect::Destroy { .. }
                                | Effect::DealDamage { .. }
                                | Effect::Counter { .. }
                                | Effect::Bounce { .. }
                                | Effect::ChangeZone { .. }
                        )
                    })
            })
        });

    if has_instant_interaction {
        0.3
    } else {
        0.0
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
    use engine::types::identifiers::CardId;
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn make_lethal_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 4;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        // AI at 5 life
        state.players[0].life = 5;

        // Opponent has a 6/6 flyer (unblockable lethal)
        let attacker = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Big Flyer".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&attacker).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(6);
        obj.toughness = Some(6);
        obj.keywords.push(engine::types::keywords::Keyword::Flying);

        state
    }

    #[test]
    fn penalizes_non_removal_cast_under_lethal() {
        let mut state = make_lethal_state();
        let spell = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Grizzly Bears".to_string(),
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
                card_id: CardId(20),
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

        let score = LethalityAwarenessPolicy.score(&ctx);
        assert!(
            score < -2.0,
            "Should heavily penalize non-removal under lethal, got {score}"
        );
    }

    #[test]
    fn no_penalty_when_not_under_lethal() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 4;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        // AI at 20 life, no threats
        state.players[0].life = 20;

        let spell_id = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(20),
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

        let score = LethalityAwarenessPolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No penalty when not under lethal, got {score}"
        );
    }
}
