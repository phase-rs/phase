//! Energy payoff tactical policy — deck-plan recognition for energy-reserve
//! decks (CR 107.14 / CR 122.1).
//!
//! Raises the priority of energy-relevant casts (producers that grant {E} and
//! sink bodies that spend it) when the casting player's deck is
//! energy-committed (producer × sink density above `COMMITMENT_FLOOR`) and the
//! live reserve shows the engine has momentum.
//!
//! **Payoff-gated**: opts out entirely when `features.energy.commitment` is
//! below `COMMITMENT_FLOOR`. Incidental energy cards in non-energy decks
//! receive no bonus — the general `EtbValuePolicy` handles their one-shot value.
//! This is what keeps the policy from perturbing the quick-filter baseline
//! decks (red/affinity/enchantress), whose energy commitment is zero.
//!
//! **Reserve-aware momentum**: `verdict()` scales the base bonus by the casting
//! player's banked energy reserve. Three tiers:
//! - Building (0–1 {E}): ×1.0 — engine just starting, deploy normally.
//! - Online   (2–4 {E}): ×2.0 — reserve building, prioritize engine pieces.
//! - Humming  (≥ 5 {E}): ×3.0 — engine running, keep deploying energy-relevant
//!   bodies before the opponent pressures the reserve out.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::energy::{
    ability_tree_pays_energy, chain_includes_energy_gain, COMMITMENT_FLOOR,
};
use crate::features::DeckFeatures;

/// Energy reserve at or above which the momentum scale reaches ×3.0. Five
/// banked counters means the deck can threaten a major sink next turn.
pub(crate) const RESERVE_THRESHOLD_HIGH: usize = 5;

/// Energy reserve at or above which the momentum scale reaches ×2.0. Two
/// counters means the engine is producing surplus beyond the first spend.
pub(crate) const RESERVE_THRESHOLD_MID: usize = 2;

pub(crate) const MOMENTUM_SCALE_HIGH: f64 = 3.0;
pub(crate) const MOMENTUM_SCALE_MID: f64 = 2.0;
pub(crate) const MOMENTUM_SCALE_NORMAL: f64 = 1.0;

pub struct EnergyPayoffPolicy;

impl TacticalPolicy for EnergyPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::EnergyPayoff
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
        if features.energy.commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(features.energy.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("energy_payoff_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("energy_payoff_na"));
        };

        // Re-classify the live object structurally and per-chain — identical
        // isolation guard as MillPayoffPolicy. An energy-relevant cast is one
        // whose own ability chain grants energy (producer), whose trigger-execute
        // chain grants energy (ETB producers like Rogue Refiner), or whose
        // ability tree pays energy (sink body like Bristling Hydra). Each chain
        // is checked in isolation so two unrelated abilities cannot combine into
        // a false positive.
        let is_producer = object.abilities.iter().any(chain_includes_energy_gain)
            || object.trigger_definitions.iter_unchecked().any(|trigger| {
                trigger
                    .execute
                    .as_deref()
                    .is_some_and(chain_includes_energy_gain)
            });
        let is_sink = object.abilities.iter().any(ability_tree_pays_energy)
            || object.trigger_definitions.iter_unchecked().any(|trigger| {
                trigger
                    .execute
                    .as_deref()
                    .is_some_and(ability_tree_pays_energy)
            });

        if !is_producer && !is_sink {
            return PolicyVerdict::neutral(PolicyReason::new("energy_payoff_inert"));
        }

        // CR 122.1: energy counters are a player reserve. The casting player's
        // banked reserve is the engine's momentum — more reserve means the deck
        // is successfully executing its plan and should keep prioritizing
        // energy-relevant casts.
        let reserve = ctx.state.players[ctx.ai_player.0 as usize].energy as usize;

        let momentum_scale = if reserve >= RESERVE_THRESHOLD_HIGH {
            MOMENTUM_SCALE_HIGH
        } else if reserve >= RESERVE_THRESHOLD_MID {
            MOMENTUM_SCALE_MID
        } else {
            MOMENTUM_SCALE_NORMAL
        };

        let delta = ctx.penalties().energy_cast_bonus * momentum_scale;
        PolicyVerdict::score(
            delta,
            PolicyReason::new("energy_cast")
                .with_fact("energy_reserve", reserve as i64)
                .with_fact("urgency_x10", (momentum_scale * 10.0) as i64)
                .with_fact("is_producer", i64::from(is_producer))
                .with_fact("is_sink", i64::from(is_sink)),
        )
    }
}
