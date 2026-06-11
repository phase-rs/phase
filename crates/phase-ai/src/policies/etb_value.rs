use crate::cast_facts::collect_definition_effects;
use crate::features::DeckFeatures;
use engine::types::ability::{Effect, EffectScope, PtValue, TapStateChange};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::visible_opponent_creature_value;

pub struct EtbValuePolicy;

impl EtbValuePolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if !matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            return 0.0;
        }

        let Some(facts) = ctx.cast_facts() else {
            return 0.0;
        };
        if facts.immediate_etb_triggers.is_empty() && facts.immediate_replacements.is_empty() {
            return 0.0;
        }

        let opponent_creature_value = visible_opponent_creature_value(ctx.state, ctx.ai_player);
        let mut score = 0.0;

        for trigger in &facts.immediate_etb_triggers {
            let certainty =
                trigger_certainty_discount(trigger.optional, trigger.condition.is_some());
            if let Some(execute) = &trigger.execute {
                score += score_effects(
                    collect_definition_effects(execute),
                    opponent_creature_value,
                    certainty,
                );
            }
        }

        for replacement in &facts.immediate_replacements {
            let certainty = trigger_certainty_discount(
                matches!(
                    replacement.mode,
                    engine::types::ability::ReplacementMode::Optional { .. }
                ),
                replacement.condition.is_some(),
            );
            if let Some(execute) = &replacement.execute {
                score += score_effects(
                    collect_definition_effects(execute),
                    opponent_creature_value,
                    certainty,
                );
            } else {
                score += 0.06 * certainty;
            }
        }

        score.min(0.9)
    }
}

impl TacticalPolicy for EtbValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::EtbValue
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
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
            reason: PolicyReason::new("etb_value_score"),
        }
    }
}

fn score_effects(effects: Vec<&Effect>, opponent_creature_value: f64, certainty: f64) -> f64 {
    effects
        .into_iter()
        .map(|effect| score_effect(effect, opponent_creature_value) * certainty)
        .sum()
}

fn score_effect(effect: &Effect, opponent_creature_value: f64) -> f64 {
    match effect {
        Effect::Destroy { .. }
        | Effect::DealDamage { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::ChangeZone {
            destination: engine::types::zones::Zone::Exile | engine::types::zones::Zone::Graveyard,
            ..
        } => {
            if opponent_creature_value > 0.0 {
                0.22 + (opponent_creature_value / 30.0).min(0.28)
            } else {
                0.04
            }
        }
        Effect::Pump {
            power, toughness, ..
        } => match (power, toughness) {
            (PtValue::Fixed(power), PtValue::Fixed(toughness)) if *power < 0 || *toughness < 0 => {
                if opponent_creature_value > 0.0 {
                    0.18 + (opponent_creature_value / 35.0).min(0.2)
                } else {
                    0.03
                }
            }
            _ => 0.0,
        },
        Effect::Draw { .. } => 0.18,
        Effect::Token { .. } => 0.16,
        Effect::SearchLibrary { .. } => 0.12,
        // CR 701.26a: tapping a single permanent (legacy `Effect::Tap`) is the
        // only tap-family ETB worth a value here — untap and the mass scopes
        // fall through to `0.0`, matching their legacy absence from this match.
        Effect::SetTapState {
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
            ..
        }
        | Effect::Goad { .. }
        | Effect::Suspect { .. } => {
            if opponent_creature_value > 0.0 {
                0.12
            } else {
                0.02
            }
        }
        Effect::RevealHand { .. } | Effect::DiscardCard { .. } => 0.08,
        _ => 0.0,
    }
}

fn trigger_certainty_discount(optional: bool, conditional: bool) -> f64 {
    match (optional, conditional) {
        (true, true) => 0.55,
        (true, false) | (false, true) => 0.72,
        (false, false) => 1.0,
    }
}
