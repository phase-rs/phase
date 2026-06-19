//! Mill payoff tactical policy — deck-plan recognition for
//! opponent-library-depletion decks.
//!
//! Raises the priority of opponent-mill spells when the casting player's deck
//! is mill-committed (density above `COMMITMENT_FLOOR`) and opponent library
//! size makes closing the game progressively more urgent.
//!
//! **Payoff-gated**: opts out entirely when `features.mill.commitment` is below
//! `COMMITMENT_FLOOR`. Incidental mill spells in non-mill decks receive no
//! bonus from this policy — the general `EtbValuePolicy` handles one-shot
//! value.
//!
//! **Library-size-aware urgency**: `verdict()` scales the base bonus by how
//! close the lowest-library opponent is to decking (CR 104.3c). Three tiers:
//! - Normal  (≥ 15 cards): ×1.0
//! - Elevated (< 15 cards): ×2.0
//! - Urgent   (<  5 cards): ×3.0

use engine::game::players;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::mill::{effect_is_opponent_mill, COMMITMENT_FLOOR};
use crate::features::DeckFeatures;

/// Library size below which mill urgency escalates to ×3.0. At this count the
/// opponent is one moderate mill spell away from an empty library.
pub(crate) const LIBRARY_THRESHOLD_URGENT: usize = 5;

/// Library size below which mill urgency escalates to ×2.0. Fewer than 15
/// cards puts the opponent within two-spell range.
pub(crate) const LIBRARY_THRESHOLD_ELEVATED: usize = 15;

pub(crate) const URGENCY_SCALE_HIGH: f64 = 3.0;
pub(crate) const URGENCY_SCALE_MID: f64 = 2.0;
pub(crate) const URGENCY_SCALE_NORMAL: f64 = 1.0;

pub struct MillPayoffPolicy;

impl TacticalPolicy for MillPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::MillPayoff
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
        if features.mill.commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(features.mill.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("mill_payoff_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("mill_payoff_na"));
        };

        // Re-classify the live object structurally and per-chain — identical
        // isolation guard as BlinkPayoffPolicy. Two separate abilities where
        // ability A exiles and ability B returns must NOT combine into a false-
        // positive mill detection; checking each chain independently prevents this.
        let casts_opponent_mill =
            object.abilities.iter().any(|ability| {
                collect_chain_effects(ability)
                    .iter()
                    .copied()
                    .any(effect_is_opponent_mill)
            }) || object.trigger_definitions.iter_unchecked().any(|trigger| {
                trigger.execute.as_deref().is_some_and(|execute| {
                    collect_chain_effects(execute)
                        .iter()
                        .copied()
                        .any(effect_is_opponent_mill)
                })
            });

        if !casts_opponent_mill {
            return PolicyVerdict::neutral(PolicyReason::new("mill_payoff_inert"));
        }

        // Find the opponent with the fewest cards remaining — that opponent is
        // the clock the deck is racing against.
        // CR 104.3c: a player who draws from an empty library loses. Tracking the
        // minimum-library opponent gives the most urgency-accurate scaling when
        // there are multiple opponents.
        let min_library = players::opponents(ctx.state, ctx.ai_player)
            .iter()
            .map(|&opp_id| ctx.state.players[opp_id.0 as usize].library.len())
            .min()
            .unwrap_or(60);

        let urgency_scale = if min_library < LIBRARY_THRESHOLD_URGENT {
            URGENCY_SCALE_HIGH
        } else if min_library < LIBRARY_THRESHOLD_ELEVATED {
            URGENCY_SCALE_MID
        } else {
            URGENCY_SCALE_NORMAL
        };

        let delta = ctx.penalties().mill_cast_bonus * urgency_scale;
        PolicyVerdict::score(
            delta,
            PolicyReason::new("mill_cast")
                .with_fact("library_remaining", min_library as i64)
                .with_fact("urgency_x10", (urgency_scale * 10.0) as i64),
        )
    }
}
