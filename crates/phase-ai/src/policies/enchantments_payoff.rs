//! Enchantments-payoff tactical policy.
//!
//! For decks committed to the enchantments-matter axis *and* containing
//! enchantment payoffs (enchantress / constellation), values casting
//! enchantments — each enchantment cast/ETB feeds those payoffs (card draw,
//! triggers). Strictly **payoff-gated**: it opts out entirely when the deck has
//! no enchantment payoff, so a deck that merely contains incidental enchantments
//! is unaffected.
//!
//! CR 601.2i: enchantress ("whenever you cast an enchantment spell, …").
//! CR 603.6a: constellation ("whenever an enchantment you control enters, …").

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::enchantments::COMMITMENT_FLOOR;
use crate::features::DeckFeatures;

pub struct EnchantmentsPayoffPolicy;

impl TacticalPolicy for EnchantmentsPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::EnchantmentsPayoff
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
        let enchantments = &features.enchantments;
        // Payoff-gated: with no payoff, casting an enchantment converts to
        // nothing extra, so the policy is inert (keeps non-payoff decks
        // unaffected).
        if enchantments.payoff_count == 0 || enchantments.commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(enchantments.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("enchantments_payoff_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("enchantments_payoff_na"));
        };

        // Casting an enchantment in a deck that has enchantment payoffs (ensured
        // by `activation`) fuels enchantress draws and constellation triggers.
        // CR 301.1.
        if object
            .card_types
            .core_types
            .contains(&CoreType::Enchantment)
        {
            return PolicyVerdict::score(
                ctx.penalties().enchantment_cast_bonus,
                PolicyReason::new("enchantment_cast_for_payoff"),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("enchantments_payoff_inert"))
    }
}
