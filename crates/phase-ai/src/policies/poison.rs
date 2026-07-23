//! `PoisonClockPolicy` — makes the CR 104.3d poison clock visible to an AI
//! whose evaluation is otherwise life-total-centric.
//!
//! ## The defect this closes
//!
//! CR 104.3d: "If a player has ten or more poison counters, that player loses
//! the game the next time a player would receive priority." That is a win
//! condition entirely independent of life total, tracked in a dedicated engine
//! field (`Player.poison_counters`). Nothing in the AI scored progress along
//! it, so an infect/toxic deck's whole plan registered as doing nothing: a
//! proliferate that takes an opponent from 9 to 10 poison scored the same as
//! one that took them from 0 to 1.
//!
//! ## Rules-correctness note that drives the branch structure
//!
//! CR 701.34a defines proliferate over "permanents and/or players that **have
//! a counter**". Proliferating when no opponent is poisoned adds nothing on
//! this axis — so that branch scores zero rather than a nudge. Getting this
//! backwards would push the AI to durdle with proliferate before the clock has
//! started.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. Every predicate here is
//! card-local (`obj.abilities` / `obj.keywords`) or a plain `u32` field read on
//! `Player.poison_counters`. No board-wide sweep, no affordability call, no
//! `find_legal_targets` — nothing this policy touches is on the documented
//! inner-loop landmine list.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::features::poison::{LETHAL_POISON, POISON_CLOCK_FLOOR};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct PoisonClockPolicy;

/// What the candidate action contributes to the poison clock. Typed rather
/// than a pair of bools so the branch set stays exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoisonContribution {
    /// Adds poison counters to a player who can be an opponent (CR 122.1f).
    DirectPoison,
    /// Proliferates (CR 701.34a) — only advances the clock on an already
    /// poisoned opponent.
    Proliferate,
    /// Nothing to do with the poison clock.
    None,
}

impl PoisonClockPolicy {
    /// Re-classify the LIVE candidate structurally. Deck-time classification is
    /// deliberately not trusted here — the object on the battlefield may have
    /// been modified since the deck was analyzed.
    fn contribution(&self, ctx: &PolicyContext<'_>) -> PoisonContribution {
        let abilities = match &ctx.candidate.action {
            GameAction::CastSpell { object_id, .. } => ctx
                .state
                .objects
                .get(object_id)
                .map(|obj| obj.abilities.as_slice()),
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => ctx
                .state
                .objects
                .get(source_id)
                .and_then(|obj| obj.abilities.get(*ability_index))
                .map(std::slice::from_ref),
            _ => None,
        };
        let Some(abilities) = abilities else {
            return PoisonContribution::None;
        };

        // Cheapest discriminator first: direct poison outranks proliferate
        // because it advances the clock without needing an existing counter.
        if crate::features::poison::gives_opponents_poison_parts(abilities) {
            PoisonContribution::DirectPoison
        } else if crate::features::poison::proliferates_parts(abilities) {
            PoisonContribution::Proliferate
        } else {
            PoisonContribution::None
        }
    }
}

/// CR 104.3d: the highest poison total among the AI's opponents, and whether
/// any opponent is "poisoned" (CR 122.1f — one or more poison counters).
pub(crate) fn most_poisoned_opponent(state: &GameState, ai_player: PlayerId) -> u32 {
    state
        .players
        .iter()
        .enumerate()
        .filter(|(index, _)| PlayerId(*index as u8) != ai_player)
        .map(|(_, player)| player.poison_counters)
        .max()
        .unwrap_or(0)
}

/// CR 104.3d: would one more poison counter put this player at ten or more,
/// losing them the game the next time a player would receive priority?
pub(crate) fn reaches_lethal(current_poison: u32) -> bool {
    current_poison.saturating_add(1) >= LETHAL_POISON
}

impl TacticalPolicy for PoisonClockPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::PoisonClock
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.poison.commitment < POISON_CLOCK_FLOOR {
            None
        } else {
            Some(features.poison.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let contribution = self.contribution(ctx);
        if contribution == PoisonContribution::None {
            return PolicyVerdict::neutral(PolicyReason::new("poison_clock_na"));
        }

        let highest = most_poisoned_opponent(ctx.state, ctx.ai_player);
        let scalar = ctx.config.policy_penalties.poison_clock_pressure;

        // CR 701.34a: proliferate needs an existing counter. With no poisoned
        // opponent it advances nothing, so it earns nothing on this axis.
        if contribution == PoisonContribution::Proliferate && highest == 0 {
            return PolicyVerdict::neutral(PolicyReason::new(
                "poison_clock_no_counters_to_proliferate",
            ));
        }

        // CR 104.3d: one more poison counter ends the game.
        if reaches_lethal(highest) {
            return PolicyVerdict::critical(
                scalar,
                PolicyReason::new("poison_clock_lethal")
                    .with_fact("opponent_poison", highest as i64),
            );
        }

        // Below lethal, value scales with how far the clock has already run —
        // the last counters are worth more than the first.
        let progress = f64::from(highest) / f64::from(LETHAL_POISON);
        PolicyVerdict::preference(
            scalar * progress.max(0.25),
            PolicyReason::new("poison_clock_pressure").with_fact("opponent_poison", highest as i64),
        )
    }
}
