//! `GraveyardTypesPolicy` — makes graveyard card-type diversity a resource the
//! AI can see (delirium / descend / threshold).
//!
//! ## The defect this closes
//!
//! CR 207.2c lists delirium, descend and threshold as **ability words** with no
//! rules meaning — the mechanical content is entirely the underlying "N or more
//! card types among cards in your graveyard" condition (CR 205.2a). 95 cards in
//! the corpus read that quantity, and nothing in the AI modelled the graveyard's
//! type spread. A self-mill that put the fourth distinct card type into the
//! graveyard — switching every delirium payoff on — scored exactly the same as
//! one that put in a redundant fifth creature.
//!
//! ## Why the threshold-met branch scores zero
//!
//! Once the count is at or above the deck's highest threshold, every payoff is
//! already live and additional diversity buys nothing on this axis. Scoring it
//! anyway would make the AI keep durdling with self-mill after delirium is on,
//! which is exactly the failure mode this policy exists to avoid. Same
//! no-progress-no-score backbone as `PoisonClockPolicy`, different resource.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node, so predicate order matters.
//! The card-local AST check (`fills_own_graveyard_parts` over the candidate's
//! own abilities) runs FIRST and rejects the overwhelming majority of
//! candidates; only a confirmed graveyard-filler pays for the graveyard scan,
//! which walks one zone's objects and never touches the battlefield, mana
//! affordability, or `find_legal_targets`.

use std::collections::HashSet;

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::features::graveyard_types::{fills_own_graveyard_parts, GRAVEYARD_TYPES_FLOOR};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct GraveyardTypesPolicy;

/// CR 404.1 + CR 205.2a: how many distinct card types sit in this player's
/// graveyard. Uses `owner`, not `controller` — control is a battlefield notion
/// and a card in a graveyard belongs to its owner.
pub(crate) fn distinct_graveyard_types(state: &GameState, player: PlayerId) -> u32 {
    let mut seen: HashSet<CoreType> = HashSet::new();
    for object in state.objects.values() {
        if object.zone != Zone::Graveyard || object.owner != player {
            continue;
        }
        for core_type in &object.card_types.core_types {
            seen.insert(*core_type);
        }
    }
    seen.len() as u32
}

impl TacticalPolicy for GraveyardTypesPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::GraveyardTypes
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
        if features.graveyard_types.commitment < GRAVEYARD_TYPES_FLOOR {
            None
        } else {
            Some(features.graveyard_types.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Cheapest discriminator first: a card-local AST walk over just this
        // candidate's abilities. Everything below is gated behind it.
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
            return PolicyVerdict::neutral(PolicyReason::new("graveyard_types_na"));
        };
        if !fills_own_graveyard_parts(abilities) {
            return PolicyVerdict::neutral(PolicyReason::new("graveyard_types_na"));
        }

        let threshold = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .map(|f| f.graveyard_types.highest_threshold)
            .unwrap_or(0);
        let current = distinct_graveyard_types(ctx.state, ctx.ai_player);
        let scalar = ctx.config.policy_penalties.graveyard_types_progress;

        // Every payoff is already live — more diversity buys nothing here.
        if threshold == 0 || current >= threshold {
            return PolicyVerdict::neutral(
                PolicyReason::new("graveyard_types_threshold_met")
                    .with_fact("graveyard_types", current as i64),
            );
        }

        let deficit = threshold - current;
        let reason = PolicyReason::new("graveyard_types_progress")
            .with_fact("graveyard_types", current as i64)
            .with_fact("deficit", deficit as i64);

        // The last missing type is worth far more than the first: it is what
        // actually switches the payoffs on.
        if deficit == 1 {
            PolicyVerdict::strong(scalar, reason)
        } else {
            PolicyVerdict::preference(scalar / f64::from(deficit), reason)
        }
    }
}
