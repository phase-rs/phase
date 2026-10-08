//! `VehicleDeploymentPolicy` — value casting a Vehicle against whether the board
//! can actually crew it.
//!
//! ## The gap this closes
//!
//! CR 702.122a: an uncrewed Vehicle is not a creature. It sits on the battlefield
//! doing nothing until its controller taps *other* untapped creatures with total
//! power N or greater. So the same card is a threat on a developed board and a
//! blank on an empty one — and nothing in the AI distinguishes those two casts.
//!
//! `CrewTimingPolicy` decides whether a specific crew activation is worth it,
//! which is the question *after* the Vehicle is already down. This policy asks
//! the earlier one: is deploying it now going to produce a body, or a brick?
//!
//! It is deliberately one-directional in the same way `DrawPayoffPolicy` is: it
//! ADDS value when the crew requirement is already met and withholds it
//! otherwise. It never vetoes a Vehicle cast — holding a Vehicle for a board that
//! may never arrive is its own mistake, and the mana-efficiency and board-
//! development policies already price deploying a permanent.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. The card-local check — does
//! this candidate carry a crew requirement at all — reads one card's keywords and
//! rejects every non-Vehicle candidate. Only a confirmed Vehicle pays for the
//! battlefield walk, which is bounded by the AI's own permanents and delegates
//! per-creature power to the engine's `object_crew_power_contribution` authority.
//! No affordability sweep, no `find_legal_targets`.

use engine::game::engine::creature_can_pay_crew;
use engine::game::static_abilities::object_crew_power_contribution;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::statics::CrewAction;

use crate::features::vehicles::{crew_requirement_parts, VEHICLES_FLOOR};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct VehicleDeploymentPolicy;

/// Cap on the surplus crew power rewarded, so a huge board cannot push one
/// Vehicle cast out of the preference band.
///
/// `pub(crate)` so the bounded-score regression asserts against this constant
/// rather than a copied literal.
pub(crate) const MAX_REWARDED_SURPLUS: i32 = 3;

impl TacticalPolicy for VehicleDeploymentPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::VehicleDeployment
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
        if features.vehicles.commitment < VEHICLES_FLOOR {
            None
        } else {
            Some(features.vehicles.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Card-local first: is this candidate even a Vehicle?
        let Some(facts) = ctx.cast_facts() else {
            return PolicyVerdict::neutral(PolicyReason::new("vehicle_deployment_na"));
        };
        if !matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            return PolicyVerdict::neutral(PolicyReason::new("vehicle_deployment_na"));
        }
        // CR 702.122a: Crew is an ACTIVATED ABILITY. A Vehicle subtype without
        // the keyword grants no crew ability, so it has no requirement to meet —
        // routing through the strict authority keeps a `Crew 0` from being
        // synthesised and trivially satisfied by an empty board.
        let Some(crew_cost) = crew_requirement_parts(facts.object.keywords.iter()) else {
            return PolicyVerdict::neutral(PolicyReason::new("vehicle_deployment_na"));
        };

        // Only now pay for the board walk. Eligibility is NOT re-derived here:
        // `creature_can_pay_crew` is the engine's own composed authority, the
        // same rule the crew payment path enforces (controlled, untapped, a
        // creature, not `CantTap`, not `CantCrew`). Assembling that filter from
        // parts is how this policy previously over-counted — it omitted the
        // `object_cant_tap` term, so a `CantTap` 3/3 read as able to pay Crew 3.
        //
        // Per-creature power likewise goes through `object_crew_power_contribution`,
        // so a body crewing "as though its power were N greater" or by toughness
        // instead of power is counted exactly as the real payment would count it.
        let available: i32 = ctx
            .state
            .battlefield
            .iter()
            .filter(|id| creature_can_pay_crew(ctx.state, **id, ctx.ai_player))
            .map(|id| object_crew_power_contribution(ctx.state, *id, CrewAction::Crew))
            .sum();

        let required = i32::try_from(crew_cost).unwrap_or(i32::MAX);
        if available < required {
            // The Vehicle would enter as a blank. No penalty — see the module
            // docs: withholding the bonus is the whole signal.
            return PolicyVerdict::neutral(
                PolicyReason::new("vehicle_deployment_uncrewable")
                    .with_fact("required", i64::from(required))
                    .with_fact("available", i64::from(available)),
            );
        }

        // The board can turn this into a creature the turn it lands. Scale mildly
        // with surplus power, because spare bodies mean crewing costs less of the
        // attack step.
        let surplus = (available - required).min(MAX_REWARDED_SURPLUS);
        let scaled = 1.0 + f64::from(surplus) / f64::from(MAX_REWARDED_SURPLUS);
        PolicyVerdict::score(
            ctx.config.policy_penalties.vehicle_deployment_bonus * scaled,
            PolicyReason::new("vehicle_deployment_crewable")
                .with_fact("required", i64::from(required))
                .with_fact("available", i64::from(available)),
        )
    }
}
