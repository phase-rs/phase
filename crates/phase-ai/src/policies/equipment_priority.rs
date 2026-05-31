//! Equipment equip-priority tactical policy.
//!
//! Report (Discord #ai-suggestions): the AI "spends all remaining mana every
//! turn moving an Equipment from one creature to another for no reason." It
//! re-activates an equip ability when the equipment is already on a perfectly
//! good creature, burning mana for no board improvement.
//!
//! The AI reaches equip via `GameAction::ActivateAbility` on the equipment's
//! equip ability (effect `Effect::Attach`), where mana is committed and
//! `PassPriority` is a sibling candidate — NOT via the human-UI
//! `GameAction::Equip`/`WaitingFor::EquipTarget` path. So this policy fires on
//! the activation and penalizes it when the equipment is already on the best
//! available body, making the AI keep its mana.
//!
//! It never *rewards* equipping (the reported problem is over-equipping); fresh
//! equips and genuine upgrades are neutral, leaving eval/other policies to
//! decide. The only thing penalized is a re-equip with no bigger body to move
//! to.
//!
//! CR 301.5: Equipment can be attached only to creatures (its host is always a
//! creature), so the "bigger body" comparison is over creatures you control.

use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Penalty for activating an equip ability when the equipment is already on the
/// best body (no own creature out-base-powers the current host). Modest — large
/// enough to make `PassPriority` win when nothing else pushes the re-equip.
const NO_BETTER_HOME_PENALTY: f64 = 2.0;

pub struct EquipmentPriorityPolicy;

impl TacticalPolicy for EquipmentPriorityPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::EquipmentPriority
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Applies to every deck; the verdict's equipment+Attach guard self-gates.
        // activation-constant: equipment equip-or-not decision, universal.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let score = |delta: f64, kind: &'static str| PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new(kind),
        };
        let na = || score(0.0, "equipment_priority_na");

        let (source_id, ability_index) = match &ctx.candidate.action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => (*source_id, *ability_index),
            _ => return na(),
        };

        let Some(equip) = ctx.state.objects.get(&source_id) else {
            return na();
        };
        if !equip.card_types.subtypes.iter().any(|s| s == "Equipment") {
            return na();
        }
        let Some(ability) = equip.abilities.get(ability_index) else {
            return na();
        };
        // The equip ability lowers to Effect::Attach (verified via card-data).
        if !matches!(&*ability.effect, Effect::Attach { .. }) {
            return na();
        }

        // Unattached → a fresh equip is fine; stay neutral.
        let Some(host_id) = equip.attached_to.as_ref().and_then(|a| a.as_object()) else {
            return score(0.0, "equipment_equip_fresh");
        };

        // Compare BASE power: the host's live `power` already includes this
        // equipment's own +P buff (folded in via `attached_to` in the layer
        // system), which would make the host spuriously out-power every bare
        // upgrade target. base_power is the printed baseline — the correct
        // "which body is bigger" proxy, consistent on both sides.
        let host_base = ctx
            .state
            .objects
            .get(&host_id)
            .and_then(|o| o.base_power)
            .unwrap_or(0);
        let best_other_base = ctx
            .state
            .battlefield
            .iter()
            .filter(|&&id| id != host_id)
            .filter_map(|&id| {
                let o = ctx.state.objects.get(&id)?;
                if o.controller != ctx.ai_player
                    || !o.card_types.core_types.contains(&CoreType::Creature)
                {
                    return None;
                }
                o.base_power
            })
            .max()
            .unwrap_or(i32::MIN);

        if best_other_base > host_base {
            // A genuinely bigger body to move to — allow the upgrade.
            score(0.0, "equipment_upgrade_available")
        } else {
            // Already on the best body — don't burn mana shuffling.
            score(-NO_BETTER_HOME_PENALTY, "equipment_no_better_home")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::game_object::AttachTarget;
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::zones::Zone;
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;

    const AI: PlayerId = PlayerId(0);

    /// An Equipment object with one activated equip ability (`Effect::Attach`).
    fn equipment(state: &mut GameState) -> ObjectId {
        let id = create_object(state, CardId(1), AI, "Sword".to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::Any,
            },
        ));
        id
    }

    fn creature(state: &mut GameState, base_power: i32) -> ObjectId {
        let id = create_object(state, CardId(2), AI, "Bear".to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(base_power);
        obj.power = Some(base_power);
        obj.base_toughness = Some(base_power);
        obj.toughness = Some(base_power);
        id
    }

    /// Attach `equip` to `host` and apply the equip's `+buff/+0` to the host's
    /// LIVE power (mirroring the layer system) so tests exercise the
    /// base-vs-live distinction.
    fn attach(state: &mut GameState, equip: ObjectId, host: ObjectId, live_buff: i32) {
        state.objects.get_mut(&equip).unwrap().attached_to = Some(AttachTarget::Object(host));
        let h = state.objects.get_mut(&host).unwrap();
        h.power = Some(h.power.unwrap_or(0) + live_buff);
    }

    fn equip_verdict(state: &GameState, equip: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: equip,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        EquipmentPriorityPolicy.verdict(&ctx)
    }

    fn assert_score(verdict: PolicyVerdict, kind: &str, delta: f64) {
        match verdict {
            PolicyVerdict::Score { delta: d, reason } => {
                assert_eq!(reason.kind, kind, "reason kind");
                assert_eq!(d, delta, "delta");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    #[test]
    fn unattached_equip_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let equip = equipment(&mut state);
        creature(&mut state, 3);
        assert_score(equip_verdict(&state, equip), "equipment_equip_fresh", 0.0);
    }

    #[test]
    fn reequip_no_better_home_penalized() {
        let mut state = GameState::new_two_player(42);
        let equip = equipment(&mut state);
        let host = creature(&mut state, 3);
        creature(&mut state, 2); // smaller alternative
        attach(&mut state, equip, host, 2);
        assert_score(
            equip_verdict(&state, equip),
            "equipment_no_better_home",
            -NO_BETTER_HOME_PENALTY,
        );
    }

    /// B1 trap: a +2/+0 equip on a base-2 host (LIVE power 4) with a base-3
    /// creature present. Using base_power, the 3 out-powers the host's base 2 →
    /// upgrade allowed. Comparing LIVE power (4 > 3) would wrongly penalize.
    #[test]
    fn equip_upgrade_allowed() {
        let mut state = GameState::new_two_player(42);
        let equip = equipment(&mut state);
        let host = creature(&mut state, 2);
        creature(&mut state, 3); // bigger base body
        attach(&mut state, equip, host, 2); // host live power becomes 4
        assert_score(
            equip_verdict(&state, equip),
            "equipment_upgrade_available",
            0.0,
        );
    }

    #[test]
    fn non_equip_activation_na() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(3),
            AI,
            "Rock".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        assert_score(equip_verdict(&state, id), "equipment_priority_na", 0.0);
    }
}
