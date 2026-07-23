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
//! ## When the axis stops rewarding
//!
//! A *threshold* payoff (delirium, descend N) goes live at its threshold; once
//! the graveyard reaches it, more diversity buys nothing and the branch scores
//! zero — otherwise the AI would durdle with self-mill after delirium is on. A
//! *scaling* payoff (Consuming Blob, Tarmogoyf) has no threshold and keeps
//! wanting a bigger, more diverse graveyard, so it is rewarded continuously
//! with a diminishing signal. A deck's threshold is modelled as `Option<u32>`
//! precisely so a scaling-only deck is never handed a fabricated four-type
//! ceiling. Same no-progress-no-score backbone as `PoisonClockPolicy`.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node, so predicate order matters.
//! The card-local AST check (`fills_own_graveyard_parts`, over the action's
//! authoritative effect chain) runs FIRST and rejects the overwhelming majority
//! of candidates; only a confirmed graveyard-filler pays for the graveyard
//! scan, which walks one zone's objects and never touches the battlefield, mana
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
        // Cheapest discriminator first, and it must inspect exactly the effect
        // the action performs — via the engine's authoritative ability
        // enumeration, not every ability sitting on the object.
        if !candidate_fills_own_graveyard(ctx) {
            return PolicyVerdict::neutral(PolicyReason::new("graveyard_types_na"));
        }

        let feature = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .map(|f| &f.graveyard_types);
        let threshold = feature.and_then(|f| f.highest_threshold);
        let has_scaling = feature.is_some_and(|f| f.scaling_payoff_count > 0);
        let current = distinct_graveyard_types(ctx.state, ctx.ai_player);
        let scalar = ctx.config.policy_penalties.graveyard_types_progress;

        // Below an unmet threshold: race to switch every delirium payoff on.
        // The last missing type is worth far more than the first — it is what
        // actually turns the payoffs on. Routes through `PolicyVerdict::score`
        // so a tuned-up `scalar` auto-bands instead of tripping a band assert.
        if let Some(threshold) = threshold {
            if current < threshold {
                let deficit = threshold - current;
                let delta = if deficit == 1 {
                    scalar
                } else {
                    scalar / f64::from(deficit)
                };
                return PolicyVerdict::score(
                    delta,
                    PolicyReason::new("graveyard_types_progress")
                        .with_fact("graveyard_types", current as i64)
                        .with_fact("deficit", deficit as i64),
                );
            }
        }

        // At/over the threshold, or no threshold at all: only a SCALING payoff
        // still wants a bigger, more diverse graveyard. Diminishing (the nth
        // type matters less than the first) but never zero, so a Tarmogoyf /
        // Consuming Blob deck keeps being rewarded past four types.
        if has_scaling {
            return PolicyVerdict::score(
                scalar / f64::from(current + 1),
                PolicyReason::new("graveyard_types_scaling")
                    .with_fact("graveyard_types", current as i64),
            );
        }

        // A threshold-only deck already at its threshold: delirium is on and
        // nothing scales, so more diversity buys nothing on this axis.
        PolicyVerdict::neutral(
            PolicyReason::new("graveyard_types_threshold_met")
                .with_fact("graveyard_types", current as i64),
        )
    }
}

/// True when the candidate action ACTUALLY fills the AI's own graveyard.
///
/// The lookup respects the action's authoritative ability semantics:
/// * `CastSpell` → the spell's own resolution chain (`CastFacts::primary_effects`,
///   which is the `AbilityKind::Spell` abilities only). Casting a permanent that
///   merely *has* an activated self-mill ability does not qualify — the cast
///   itself fills no graveyard (CR 601.2).
/// * `ActivateAbility` → the ability at the engine's runtime-enumerated index
///   (`effective_activated_ability`), which is the correct index space for
///   runtime-granted abilities where `GameObject::abilities` is not (CR 602.2).
fn candidate_fills_own_graveyard(ctx: &PolicyContext<'_>) -> bool {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } => ctx
            .cast_facts()
            .is_some_and(|facts| fills_own_graveyard_parts(facts.primary_effects.iter().copied())),
        GameAction::ActivateAbility { .. } => ctx
            .effective_activated_ability()
            .is_some_and(|ability| fills_own_graveyard_parts(std::iter::once(&ability))),
        _ => false,
    }
}
