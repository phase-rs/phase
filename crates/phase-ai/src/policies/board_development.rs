use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;

use super::context::PolicyContext;
use super::registry::TacticalPolicy;
use super::strategy_helpers::{best_proactive_cast_score, board_presence_score, is_own_main_phase};
use crate::deck_profile::DeckArchetype;

pub struct BoardDevelopmentPolicy;

impl TacticalPolicy for BoardDevelopmentPolicy {
    fn archetype_scale(&self, archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 1.0,
            DeckArchetype::Control => 1.0,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.4,
            DeckArchetype::Combo => 0.8,
        }
    }

    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if !is_own_main_phase(ctx) {
            return 0.0;
        }

        match &ctx.candidate.action {
            GameAction::CastSpell { .. } => score_cast(ctx),
            GameAction::PassPriority => {
                let best_proactive = best_proactive_cast_score(ctx);
                if best_proactive >= 0.26 {
                    -0.4
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }
}

fn score_cast(ctx: &PolicyContext<'_>) -> f64 {
    let Some(facts) = ctx.cast_facts() else {
        return 0.0;
    };
    let object = facts.object;

    if object.card_types.core_types.iter().all(|core_type| {
        !matches!(
            core_type,
            CoreType::Artifact
                | CoreType::Battle
                | CoreType::Creature
                | CoreType::Enchantment
                | CoreType::Planeswalker
        )
    }) {
        return 0.0;
    }

    let mana_scale = (facts.mana_value as f64 / 8.0).min(0.18);
    let presence = board_presence_score(object);
    let etb_discount = if facts.immediate_etb_triggers.is_empty() {
        0.0
    } else {
        0.04
    };

    0.12 + mana_scale + presence + etb_discount
}
