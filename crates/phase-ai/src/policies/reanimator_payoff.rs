//! Reanimator-payoff tactical policy.
//!
//! For decks committed to the reanimator axis *and* containing both reanimation
//! payoffs and worthwhile targets, values two kinds of casts:
//!   1. the reanimation itself — cheating a fat body out of a graveyard ahead of
//!      curve (CR 404.1 + CR 110.1); and
//!   2. graveyard enablers — self-mill / discard that load the graveyard so a
//!      reanimation has fuel (CR 701.17a / CR 701.9a).
//!
//! Strictly **payoff-gated**: it opts out entirely unless the deck has both a
//! reanimation payoff and a target, so incidental graveyard interaction in
//! non-reanimator decks is unaffected — and it only adds value to self-mill
//! where there is genuinely something to reanimate, so it does not blindly fight
//! `mill_targeting`'s self-mill penalty on non-reanimator decks.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::reanimator::{
    ability_is_discard_outlet, effects_include_reanimation, effects_include_self_graveyard_fill,
    COMMITMENT_FLOOR,
};
use crate::features::DeckFeatures;

pub struct ReanimatorPayoffPolicy;

impl TacticalPolicy for ReanimatorPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ReanimatorPayoff
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
        let reanimator = &features.reanimator;
        // Payoff-gated: a reanimation with no target has nothing to cheat out,
        // and a target with no reanimation can't be cheated out — either way the
        // plan is absent, so the policy is inert (keeps non-reanimator decks, and
        // the `mill_targeting` self-mill penalty, unaffected).
        if reanimator.reanimation_count == 0
            || reanimator.target_count == 0
            || reanimator.commitment < COMMITMENT_FLOOR
        {
            None
        } else {
            Some(reanimator.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("reanimator_payoff_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("reanimator_payoff_na"));
        };

        // Re-classify the live object structurally (not by deck-time tag) using
        // the predicates shared with the deck-time detector, so the two never
        // drift. A reanimation effect can live in a Spell/activated ability or a
        // trigger's executed chain, so both are walked.
        let effects: Vec<_> = object
            .abilities
            .iter()
            .flat_map(collect_chain_effects)
            .chain(
                object
                    .trigger_definitions
                    .iter_unchecked()
                    .filter_map(|trigger| trigger.execute.as_deref())
                    .flat_map(collect_chain_effects),
            )
            .collect();

        // Casting the reanimation itself is the marquee play — ensured by
        // `activation` that the deck has a target worth reanimating.
        if effects_include_reanimation(&effects) {
            return PolicyVerdict::score(
                ctx.penalties().reanimation_cast_bonus,
                PolicyReason::new("reanimation_cast_for_payoff"),
            );
        }

        // Otherwise, casting a graveyard enabler (self-mill / discard outlet)
        // sets up a future reanimation — a smaller, supporting bonus.
        if effects_include_self_graveyard_fill(&effects)
            || object.abilities.iter().any(ability_is_discard_outlet)
        {
            return PolicyVerdict::score(
                ctx.penalties().graveyard_enabler_bonus,
                PolicyReason::new("graveyard_enabler_for_reanimation"),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("reanimator_payoff_inert"))
    }
}
