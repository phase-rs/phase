//! `DevotionPolicy` — makes CR 700.5 pip density a resource the AI can see.
//!
//! ## The defect this closes
//!
//! CR 700.5: devotion to a color is the number of that color's mana symbols
//! among the mana costs of permanents you control. It is the payoff currency
//! for the Theros gods (not creatures below their threshold), Gray Merchant
//! drains, and X = devotion scalers. The AI's evaluation models mana value and
//! board presence but not pip density, so between two comparable permanents it
//! could not prefer the double-pip one, and it could not see that casting one
//! more colored permanent flips a dormant god into a lethal beater.
//!
//! ## Why the god-threshold crossing is a distinct branch
//!
//! Below a god's threshold the god is not a creature; the cast that reaches the
//! threshold turns a dead enchantment into a large indestructible body — a
//! multi-card swing, not the marginal +1 that the previous pip was. That
//! discontinuity is scored as its own term (`devotion_god_activation`), the same
//! "last missing piece" structure `graveyard_types` uses for the fourth card
//! type. Every other pip is a smooth preference (`devotion_pip_progress`).
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. The card-local check — pips
//! in the candidate's own mana cost, a handful of shard matches — runs FIRST
//! and rejects every non-permanent and off-color cast. Only a confirmed
//! primary-color permanent pays for `count_devotion`, one pass over the AI's
//! battlefield permanents (the CR 700.5 runtime authority). No affordability
//! sweep, no `find_legal_targets`.

use engine::game::devotion::count_devotion;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::devotion::DEVOTION_FLOOR;
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct DevotionPolicy;

impl TacticalPolicy for DevotionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::Devotion
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.devotion.commitment < DEVOTION_FLOOR {
            None
        } else {
            Some(features.devotion.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let feature = match ctx.context.session.features.get(&ctx.ai_player) {
            Some(f) => &f.devotion,
            None => return PolicyVerdict::neutral(PolicyReason::new("devotion_na")),
        };
        // `activation` already gated on the floor, but the color is what the
        // whole verdict keys on — no primary color, nothing to score.
        let Some(color) = feature.primary_color else {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        };

        // Card-local first: how many primary-color pips does THIS cast add, and
        // is it even a permanent? CR 700.5 — only permanents contribute.
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        };
        let Some(obj) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        };
        if !is_permanent(&obj.card_types.core_types) {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_na"));
        }
        let added = obj.mana_cost.count_colored_pips(Some(color)).max(0) as u32;
        if added == 0 {
            return PolicyVerdict::neutral(PolicyReason::new("devotion_off_color"));
        }

        // Only now pay for the battlefield devotion scan (CR 700.5 authority).
        let current = count_devotion(ctx.state, ctx.ai_player, &[color]);
        let pip_scalar = ctx.config.policy_penalties.devotion_pip_progress;

        // CR 700.5 + the god's `DevotionGE` gate: does this cast cross a god's
        // threshold, turning it from a non-creature into a body?
        if let Some(threshold) = feature.highest_threshold {
            if current < threshold && current + added >= threshold {
                let activation = ctx.config.policy_penalties.devotion_god_activation;
                return PolicyVerdict::score(
                    activation + pip_scalar * f64::from(added),
                    PolicyReason::new("devotion_god_activation")
                        .with_fact("devotion", current as i64)
                        .with_fact("threshold", threshold as i64),
                );
            }
        }

        // Otherwise a smooth preference proportional to the pips added.
        PolicyVerdict::score(
            pip_scalar * f64::from(added),
            PolicyReason::new("devotion_pip_progress")
                .with_fact("devotion", current as i64)
                .with_fact("added", added as i64),
        )
    }
}

/// CR 205.2a: a permanent can enter the battlefield and thus contribute
/// devotion; an instant or sorcery never does.
fn is_permanent(core_types: &[CoreType]) -> bool {
    core_types.iter().any(|t| {
        matches!(
            t,
            CoreType::Creature
                | CoreType::Artifact
                | CoreType::Enchantment
                | CoreType::Planeswalker
                | CoreType::Land
                | CoreType::Battle
        )
    })
}
