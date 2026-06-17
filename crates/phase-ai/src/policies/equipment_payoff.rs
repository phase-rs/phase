//! Equipment / Voltron payoff tactical policy.
//!
//! For decks committed to the equipment axis *and* containing both an Equipment
//! package and equipment-matters support, values two kinds of casts:
//!   1. deploying an Equipment — growing the voltron package (CR 301.5); and
//!   2. casting an equipment payoff — a tutor / auto-attacher / equip-cost
//!      grant / equipment-cast trigger that makes the package matter (CR 701.23
//!      / CR 702.6 / CR 601.2).
//!
//! Strictly **payoff-gated**: it opts out entirely unless the deck has both
//! Equipment density and a payoff, so a deck that merely runs a couple of swords
//! is unaffected.
//!
//! Coexistence with `EquipmentPriorityPolicy`: this policy acts only on
//! `CastSpell`. It deliberately does NOT reward the equip activation itself
//! (`ActivateAbility` / `WaitingFor::EquipTarget`) — that decision is owned by
//! `EquipmentPriorityPolicy`, which vetoes wasteful same-host re-equips. Keeping
//! this policy on the cast step avoids fighting that anti-over-equip guard.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::equipment::{
    effect_is_equipment_support, static_grants_equip, subtypes_contain_equipment,
    trigger_references_equipment, COMMITMENT_FLOOR,
};
use crate::features::DeckFeatures;

pub struct EquipmentPayoffPolicy;

impl TacticalPolicy for EquipmentPayoffPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::EquipmentPayoff
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
        let equipment = &features.equipment;
        // Payoff-gated: Equipment with no support is not an equipment-matters
        // deck, and support with no Equipment has nothing to enable — either way
        // the plan is absent, so the policy is inert (keeps non-equipment decks
        // unaffected).
        if equipment.equipment_count == 0
            || equipment.payoff_count == 0
            || equipment.commitment < COMMITMENT_FLOOR
        {
            None
        } else {
            Some(equipment.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("equipment_payoff_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("equipment_payoff_na"));
        };

        // Deploying an Equipment grows the voltron package. CR 301.5.
        if subtypes_contain_equipment(&object.card_types.subtypes) {
            return PolicyVerdict::score(
                ctx.penalties().deploy_equipment_bonus,
                PolicyReason::new("deploy_equipment_for_payoff"),
            );
        }

        // Re-classify the live object lazily — statics, triggers, then effects —
        // short-circuiting as soon as any condition passes. The Equipment guard
        // above already returned, so checking subtypes again is not needed.
        let is_payoff = object
            .static_definitions
            .iter_unchecked()
            .any(static_grants_equip)
            || object
                .trigger_definitions
                .iter_unchecked()
                .any(trigger_references_equipment)
            || object
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
                .any(effect_is_equipment_support);

        if is_payoff {
            return PolicyVerdict::score(
                ctx.penalties().equipment_payoff_cast_bonus,
                PolicyReason::new("equipment_payoff_cast"),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("equipment_payoff_inert"))
    }
}
