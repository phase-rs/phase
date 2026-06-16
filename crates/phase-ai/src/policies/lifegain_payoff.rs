//! Lifegain-payoff tactical policy.
//!
//! For decks committed to the lifegain-matters axis *and* containing lifegain
//! payoffs, values casting lifegain sources — each life-gain event the source
//! produces feeds the deck's "whenever you gain life, …" payoffs (card
//! advantage, counters, tokens, damage). Strictly **payoff-gated**: it opts out
//! entirely when the deck has no lifegain payoff, so incidental lifegain in
//! non-lifegain decks is unaffected (and it does not fight the
//! `redundancy_avoidance` penalty there).
//!
//! CR 702.15a: Lifelink — its controller gains life equal to damage dealt.
//! CR 119.3: "You gain N life." CR 603.6a: `LifeGained` payoff triggers.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::lifegain::{
    is_lifegain_source_parts, is_lifegain_source_trigger, COMMITMENT_FLOOR,
};
use crate::features::DeckFeatures;

pub struct LifegainPayoffPolicy;

impl TacticalPolicy for LifegainPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LifegainPayoff
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
        let lifegain = &features.lifegain;
        // Payoff-gated: with no payoff, gaining life converts to nothing, so the
        // policy is inert (this is what keeps non-lifegain decks unaffected).
        if lifegain.payoff_count == 0 || lifegain.commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(lifegain.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("lifegain_payoff_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("lifegain_payoff_na"));
        };

        // Casting a lifegain source in a deck that has lifegain payoffs (ensured
        // by `activation`) is what fuels those payoffs. Classification is shared
        // with the deck-time detector via `is_lifegain_source_parts` so the two
        // never drift. CR 702.15a / CR 119.3.
        let effects: Vec<_> = object
            .abilities
            .iter()
            .flat_map(collect_chain_effects)
            .collect();
        let trigger_borne_source = object
            .trigger_definitions
            .iter_unchecked()
            .any(is_lifegain_source_trigger);
        if is_lifegain_source_parts(&object.keywords, &effects) || trigger_borne_source {
            return PolicyVerdict::score(
                ctx.penalties().lifegain_source_bonus,
                PolicyReason::new("lifegain_source_for_payoff"),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("lifegain_payoff_inert"))
    }
}
