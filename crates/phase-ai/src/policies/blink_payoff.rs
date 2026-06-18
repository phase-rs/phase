//! Blink / flicker payoff tactical policy.
//!
//! For decks committed to the blink axis *and* containing both a flicker engine
//! and value-ETB creatures, values two kinds of casts:
//!   1. deploying a flicker enabler — the engine that re-triggers ETBs
//!      (CR 603.7); and
//!   2. casting a value-ETB creature — the payoff whose ETB the flicker engine
//!      re-uses (CR 603.6a), worth more here than its one-shot value because the
//!      deck can re-trigger it.
//!
//! Strictly **payoff-gated**: it opts out entirely unless the deck has both a
//! flicker enabler and an ETB payoff, so incidental flicker or value creatures
//! in non-blink decks are unaffected.
//!
//! Coexistence with `EtbValuePolicy`: that policy scores an ETB trigger's value
//! at cast time for *any* deck (one-shot). This policy adds the blink-specific
//! re-trigger premium on top, only when the deck can actually flicker — so the
//! two are complementary, not duplicative. It acts only on `CastSpell` and does
//! not score the flicker *activation* itself (targeting which creature to blink
//! is left to the general targeting/value policies).

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::blink::{effects_include_flicker, trigger_is_value_etb, COMMITMENT_FLOOR};
use crate::features::DeckFeatures;

pub struct BlinkPayoffPolicy;

impl TacticalPolicy for BlinkPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::BlinkPayoff
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
        let blink = &features.blink;
        // Payoff-gated: a flicker engine with no ETB payoff has nothing worth
        // re-triggering, and ETB creatures with no flicker have nothing to
        // re-trigger them — either way the plan is absent, so the policy is inert
        // (keeps non-blink decks, and the general `etb_value` scoring, unaffected).
        if blink.flicker_count == 0
            || blink.etb_payoff_count == 0
            || blink.commitment < COMMITMENT_FLOOR
        {
            None
        } else {
            Some(blink.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("blink_payoff_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("blink_payoff_na"));
        };

        // Re-classify the live object structurally (shared with the deck-time
        // detector) so the two never drift. Each ability/trigger chain is
        // checked in isolation — merging all chains into one flat slice could
        // produce false positives if two unrelated abilities happen to contain
        // an exile step and a battlefield-return step independently.
        let has_flicker = object
            .abilities
            .iter()
            .any(|ability| effects_include_flicker(&collect_chain_effects(ability)))
            || object.trigger_definitions.iter_unchecked().any(|trigger| {
                trigger
                    .execute
                    .as_deref()
                    .is_some_and(|execute| effects_include_flicker(&collect_chain_effects(execute)))
            });

        // Deploying the flicker engine is the marquee play — `activation` has
        // ensured the deck has ETB payoffs worth re-triggering.
        if has_flicker {
            return PolicyVerdict::score(
                ctx.penalties().deploy_flicker_engine_bonus,
                PolicyReason::new("deploy_flicker_engine"),
            );
        }

        // Otherwise, casting a value-ETB creature deploys a re-triggerable
        // payoff — a smaller, supporting bonus on top of the one-shot ETB value.
        if object.card_types.core_types.contains(&CoreType::Creature)
            && object
                .trigger_definitions
                .iter_unchecked()
                .any(trigger_is_value_etb)
        {
            return PolicyVerdict::score(
                ctx.penalties().etb_payoff_cast_bonus,
                PolicyReason::new("etb_payoff_cast"),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("blink_payoff_inert"))
    }
}
