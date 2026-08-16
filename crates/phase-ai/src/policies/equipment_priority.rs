//! Equipment equip-priority tactical policy.
//!
//! Report (Discord #ai-suggestions): the AI "spends all remaining mana every
//! turn moving an Equipment from one creature to another for no reason." It
//! re-activates an equip ability when the equipment is already on a perfectly
//! good creature, burning mana for no board improvement.
//!
//! The AI reaches equip via `GameAction::ActivateAbility` on the equipment's
//! equip ability (effect `Effect::Attach`), then `GameAction::Equip` while
//! `WaitingFor::EquipTarget`. Both route to `DecisionKind::ActivateAbility`.
//! This policy rejects same-host activations and re-targets (including paying
//! {1} to re-equip Skullclamp to the creature it is already on — #1986).
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
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Penalty for choosing a strictly smaller equip target when already attached.
const DOWNGRADE_TARGET_PENALTY: f64 = 2.0;

fn reject(kind: &'static str) -> PolicyVerdict {
    PolicyVerdict::Reject {
        reason: PolicyReason::new(kind),
    }
}

fn score(delta: f64, kind: &'static str) -> PolicyVerdict {
    PolicyVerdict::Score {
        delta,
        reason: PolicyReason::new(kind),
    }
}

/// Whether `creature_id` is a better equip host than `current_host` (printed base power).
fn is_upgrade_host(state: &GameState, current_host: ObjectId, creature_id: ObjectId) -> bool {
    let host_base = state
        .objects
        .get(&current_host)
        .and_then(|o| o.base_power)
        .unwrap_or(0);
    let target_base = state
        .objects
        .get(&creature_id)
        .and_then(|o| o.base_power)
        .unwrap_or(0);
    target_base > host_base
}

/// Best base power among other creatures you control (excluding `current_host`).
fn best_other_creature_base(
    state: &GameState,
    ai_player: PlayerId,
    current_host: ObjectId,
) -> Option<i32> {
    state
        .battlefield
        .iter()
        .filter(|&&id| id != current_host)
        .filter_map(|&id| {
            let o = state.objects.get(&id)?;
            if o.controller != ai_player || !o.card_types.core_types.contains(&CoreType::Creature) {
                return None;
            }
            o.base_power
        })
        .max()
}

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
        let na = || score(0.0, "equipment_priority_na");

        let (equipment_id, target_id) = match &ctx.candidate.action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => {
                let Some(equip) = ctx.state.objects.get(source_id) else {
                    return na();
                };
                let Some(ability) = equip.abilities.get(*ability_index) else {
                    return na();
                };
                if !matches!(&*ability.effect, Effect::Attach { .. }) {
                    return na();
                }
                (*source_id, None)
            }
            GameAction::Equip {
                equipment_id,
                target_id,
            } => (*equipment_id, Some(*target_id)),
            _ => return na(),
        };

        let Some(equip) = ctx.state.objects.get(&equipment_id) else {
            return na();
        };
        if !equip.card_types.subtypes.iter().any(|s| s == "Equipment") {
            return na();
        }

        let Some(host_id) = equip.attached_to.as_ref().and_then(|a| a.as_object()) else {
            return score(0.0, "equipment_equip_fresh");
        };

        if let Some(target_id) = target_id {
            if target_id == host_id {
                return reject("equipment_reequip_same_host");
            }
            if is_upgrade_host(ctx.state, host_id, target_id) {
                return score(0.0, "equipment_upgrade_available");
            }
            return score(-DOWNGRADE_TARGET_PENALTY, "equipment_downgrade_target");
        }

        let host_base = ctx
            .state
            .objects
            .get(&host_id)
            .and_then(|o| o.base_power)
            .unwrap_or(0);
        match best_other_creature_base(ctx.state, ctx.ai_player, host_id) {
            Some(best_other_base) if best_other_base > host_base => {
                score(0.0, "equipment_upgrade_available")
            }
            Some(_) => score(-DOWNGRADE_TARGET_PENALTY, "equipment_no_better_home"),
            None => reject("equipment_no_other_home"),
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

    fn policy_verdict(state: &GameState, action: GameAction) -> PolicyVerdict {
        let candidate = CandidateAction {
            action,
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        EquipmentPriorityPolicy.verdict(&ctx)
    }

    fn activate_equip(state: &GameState, equip: ObjectId) -> PolicyVerdict {
        policy_verdict(
            state,
            GameAction::ActivateAbility {
                source_id: equip,
                ability_index: 0,
            },
        )
    }

    fn assert_score(verdict: PolicyVerdict, kind: &str, delta: f64) {
        match verdict {
            PolicyVerdict::Score { delta: d, reason } => {
                assert_eq!(reason.kind, kind, "reason kind");
                assert_eq!(d, delta, "delta");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject: {kind}"),
        }
    }

    fn assert_reject(verdict: PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, kind),
            PolicyVerdict::Score { .. } => panic!("expected reject: {kind}"),
        }
    }

    #[test]
    fn unattached_equip_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let equip = equipment(&mut state);
        creature(&mut state, 3);
        assert_score(activate_equip(&state, equip), "equipment_equip_fresh", 0.0);
    }

    #[test]
    fn reequip_no_other_home_rejected() {
        let mut state = GameState::new_two_player(42);
        let equip = equipment(&mut state);
        let host = creature(&mut state, 3);
        attach(&mut state, equip, host, 2);
        assert_reject(activate_equip(&state, equip), "equipment_no_other_home");
    }

    #[test]
    fn reequip_no_better_home_penalized_when_another_target_exists() {
        let mut state = GameState::new_two_player(42);
        let equip = equipment(&mut state);
        let host = creature(&mut state, 3);
        creature(&mut state, 2); // smaller alternative
        attach(&mut state, equip, host, 2);
        assert_score(
            activate_equip(&state, equip),
            "equipment_no_better_home",
            -DOWNGRADE_TARGET_PENALTY,
        );
    }

    /// #1986: Skullclamp-style loop — paying equip cost to re-attach to the same host.
    #[test]
    fn reequip_same_host_rejected() {
        let mut state = GameState::new_two_player(42);
        let equip = equipment(&mut state);
        let host = creature(&mut state, 2);
        attach(&mut state, equip, host, 0);
        assert_reject(
            policy_verdict(
                &state,
                GameAction::Equip {
                    equipment_id: equip,
                    target_id: host,
                },
            ),
            "equipment_reequip_same_host",
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
            activate_equip(&state, equip),
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
        assert_score(activate_equip(&state, id), "equipment_priority_na", 0.0);
    }
}
