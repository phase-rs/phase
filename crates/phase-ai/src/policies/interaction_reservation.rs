use engine::game::keywords::has_flash;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::cast_facts::cast_facts_for_action;
use crate::features::DeckFeatures;

use super::activation::arch_times_turn;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::{
    battlefield_pressure_delta, best_proactive_cast_score, is_own_main_phase,
};
use crate::deck_profile::DeckArchetype;
use engine::types::game_state::CastPaymentMode;

pub struct InteractionReservationPolicy;

impl InteractionReservationPolicy {
    fn archetype_scale(archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 0.4,
            DeckArchetype::Control => 2.0,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.0,
            DeckArchetype::Combo => 1.0,
        }
    }

    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if !is_own_main_phase(ctx) || !matches!(ctx.candidate.action, GameAction::PassPriority) {
            return 0.0;
        }

        let mut has_removal_interaction = false;
        let mut has_counter_interaction = false;
        for object in ctx.state.players[ctx.ai_player.0 as usize]
            .hand
            .iter()
            .filter_map(|object_id| ctx.state.objects.get(object_id))
        {
            let instant_speed = object.card_types.core_types.contains(&CoreType::Instant)
                || (object.card_types.core_types.contains(&CoreType::Creature)
                    && has_flash(object));
            if !instant_speed {
                continue;
            }
            if let Some(facts) = cast_facts_for_action(
                ctx.state,
                &GameAction::CastSpell {
                    object_id: object.id,
                    card_id: object.card_id,
                    targets: Vec::new(),

                    payment_mode: CastPaymentMode::Auto,
                },
                ctx.ai_player,
            ) {
                if facts.has_counter_spell() {
                    has_counter_interaction = true;
                }
                if facts.has_direct_removal_text() || facts.has_reveal_hand_or_discard() {
                    has_removal_interaction = true;
                }
            }
        }
        if !has_removal_interaction && !has_counter_interaction {
            return 0.0;
        }

        let board_is_stable = battlefield_pressure_delta(ctx.state, ctx.ai_player) >= -1.5
            && ctx.state.players[ctx.ai_player.0 as usize].life >= 8;
        let proactive_score = best_proactive_cast_score(ctx);

        // If opponent has negligible counterspell probability (Full threat profile),
        // reduce the reservation bonus — no need to hold mana against aggro with no counters.
        let counter_discount = ctx
            .context
            .opponent_threat
            .as_ref()
            .and_then(|threat| {
                if ctx.config.search.threat_awareness == crate::config::ThreatAwareness::Full
                    && threat.probabilities.counterspell < 0.1
                {
                    Some(0.5) // halve the bonus
                } else {
                    None
                }
            })
            .unwrap_or(1.0);

        // Counter spells are time-critical — they only work during the opponent's cast.
        // Hold mana for counters even when behind (that's when control needs them most).
        if has_counter_interaction && proactive_score < 0.5 {
            0.3 * counter_discount
        } else if board_is_stable && proactive_score < 0.42 {
            0.18 * counter_discount
        } else if proactive_score >= 0.42 {
            -0.16
        } else {
            0.0
        }
    }
}

impl TacticalPolicy for InteractionReservationPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::InteractionReservation
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility, DecisionKind::CastSpell]
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
            reason: PolicyReason::new("interaction_reservation_score"),
        }
    }
}
