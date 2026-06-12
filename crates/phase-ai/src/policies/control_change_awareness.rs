//! Control-Change Awareness Policy
//!
//! Prevents the AI from activating abilities that give away control of its own
//! permanents to opponents. This addresses cards like Humble Defector which have
//! drawback abilities that exchange control or grant control to opponents.
//!
//! The policy detects `Effect::GainControl` and `Effect::ExchangeControl` in
//! ability effects and applies severe penalties when the target would hit the
//! AI's own permanents.
//!
//! CR 611.2c: A continuous effect that changes control determines the affected
//! objects when that effect begins. This policy avoids choosing AI-controlled
//! objects for effects that would give or exchange control.

use engine::game::targeting::find_legal_targets;
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Severe penalty for activating an ability that gives away the AI's own permanents.
/// This should be enough to make PassPriority win over such activations.
const CONTROL_CHANGE_PENALTY: f64 = -100.0;

pub struct ControlChangeAwarenessPolicy;

impl TacticalPolicy for ControlChangeAwarenessPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ControlChangeAwareness
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility, DecisionKind::SelectTarget]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Applies to every deck; the verdict's effect guard self-gates.
        Some(1.0) // activation-constant:
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        match &ctx.candidate.action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => score_activation(ctx, *source_id, *ability_index),
            GameAction::ChooseTarget {
                target: Some(target),
            } => score_selected_targets(ctx, std::slice::from_ref(target)),
            GameAction::SelectTargets { targets } => score_selected_targets(ctx, targets),
            _ => PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("control_change_na"),
            },
        }
    }
}

fn score_activation(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    ability_index: usize,
) -> PolicyVerdict {
    // Get the ability definition
    let Some(obj) = ctx.state.objects.get(&source_id) else {
        return PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("control_change_na"),
        };
    };

    let Some(ability_def) = obj.abilities.get(ability_index) else {
        return PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("control_change_na"),
        };
    };

    // Check if the ability effect involves control change
    let effects = crate::cast_facts::collect_definition_effects(ability_def);
    let mut control_change_effect = false;
    let mut gives_away_permanent = false;

    for effect in effects {
        match effect {
            Effect::GainControl { .. } | Effect::GainControlAll { .. } => {
                control_change_effect = true;
            }
            Effect::ExchangeControl { target_a, target_b } => {
                control_change_effect = true;
                if would_target_own_permanent(ctx, source_id, target_a)
                    || would_target_own_permanent(ctx, source_id, target_b)
                {
                    gives_away_permanent = true;
                }
            }
            Effect::GiveControl { target, .. } => {
                control_change_effect = true;
                if would_target_own_permanent(ctx, source_id, target) {
                    gives_away_permanent = true;
                }
            }
            _ => {}
        }
    }

    control_change_verdict(control_change_effect, gives_away_permanent)
}

fn score_selected_targets(ctx: &PolicyContext<'_>, targets: &[TargetRef]) -> PolicyVerdict {
    if targets.is_empty() {
        return PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("control_change_na"),
        };
    }

    let mut control_change_effect = false;
    let mut gives_away_permanent = false;

    for effect in ctx.effects() {
        match effect {
            Effect::GainControl { .. } | Effect::GainControlAll { .. } => {
                control_change_effect = true;
            }
            Effect::GiveControl { target, .. } => {
                control_change_effect = true;
                if selected_targets_include_own_permanent(ctx, targets, target) {
                    gives_away_permanent = true;
                }
            }
            Effect::ExchangeControl { target_a, target_b } => {
                control_change_effect = true;
                if selected_targets_include_own_permanent(ctx, targets, target_a)
                    || selected_targets_include_own_permanent(ctx, targets, target_b)
                {
                    gives_away_permanent = true;
                }
            }
            _ => {}
        }
    }

    control_change_verdict(control_change_effect, gives_away_permanent)
}

fn control_change_verdict(
    control_change_effect: bool,
    gives_away_permanent: bool,
) -> PolicyVerdict {
    if !control_change_effect {
        return PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("control_change_ok"),
        };
    }

    if gives_away_permanent {
        // Apply severe penalty for giving away own permanents
        PolicyVerdict::Score {
            delta: CONTROL_CHANGE_PENALTY,
            reason: PolicyReason::new("control_change_gives_away_permanent"),
        }
    } else {
        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("control_change_ok"),
        }
    }
}

/// Check if a target filter would hit the AI's own permanents.
fn would_target_own_permanent(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    target: &TargetFilter,
) -> bool {
    find_legal_targets(ctx.state, target, ctx.ai_player, source_id)
        .into_iter()
        .any(|target| match target {
            TargetRef::Object(object_id) => ctx
                .state
                .objects
                .get(&object_id)
                .is_some_and(|object| object.controller == ctx.ai_player),
            TargetRef::Player(_) => false,
        })
}

fn selected_targets_include_own_permanent(
    ctx: &PolicyContext<'_>,
    targets: &[TargetRef],
    filter: &TargetFilter,
) -> bool {
    targets.iter().any(|target| match target {
        TargetRef::Object(object_id) => ctx.state.objects.get(object_id).is_some_and(|object| {
            object.controller == ctx.ai_player
                && would_selected_target_match_filter(ctx, *object_id, filter)
        }),
        TargetRef::Player(_) => false,
    })
}

fn would_selected_target_match_filter(
    ctx: &PolicyContext<'_>,
    object_id: ObjectId,
    filter: &TargetFilter,
) -> bool {
    let Some(source) = ctx.source_object() else {
        return false;
    };
    find_legal_targets(ctx.state, filter, ctx.ai_player, source.id)
        .into_iter()
        .any(|target| target == TargetRef::Object(object_id))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, ResolvedAbility};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn add_creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    fn make_config_context(config: &AiConfig) -> AiContext {
        AiContext::empty(&config.weights)
    }

    fn score_target(
        state: &GameState,
        source_id: ObjectId,
        effect: Effect,
        selected: TargetRef,
    ) -> PolicyVerdict {
        let ability = ResolvedAbility::new(effect, Vec::new(), source_id, AI);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::MultiTargetSelection {
                player: AI,
                legal_targets: Vec::new(),
                min_targets: 1,
                max_targets: 1,
                pending_ability: Box::new(ability),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectTargets {
                targets: vec![selected],
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Target,
            },
        };
        let config = AiConfig::default();
        let context = make_config_context(&config);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        ControlChangeAwarenessPolicy.verdict(&ctx)
    }

    fn score_activation(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        let config = AiConfig::default();
        let context = make_config_context(&config);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        ControlChangeAwarenessPolicy.verdict(&ctx)
    }

    fn assert_score(verdict: PolicyVerdict, expected_delta: f64, expected_reason: &str) {
        let PolicyVerdict::Score { delta, reason } = verdict else {
            panic!("expected score verdict");
        };
        assert_eq!(delta, expected_delta);
        assert_eq!(reason.kind, expected_reason);
    }

    #[test]
    fn selected_give_control_penalizes_own_permanent_only() {
        let mut state = GameState::new_two_player(42);
        let source_id = add_creature(&mut state, AI, "Donation Source");
        let own_creature = add_creature(&mut state, AI, "Own Bear");
        let opp_creature = add_creature(&mut state, OPP, "Opp Bear");
        let effect = Effect::GiveControl {
            target: TargetFilter::Any,
            recipient: TargetFilter::Player,
        };

        assert_score(
            score_target(
                &state,
                source_id,
                effect.clone(),
                TargetRef::Object(own_creature),
            ),
            CONTROL_CHANGE_PENALTY,
            "control_change_gives_away_permanent",
        );
        assert_score(
            score_target(&state, source_id, effect, TargetRef::Object(opp_creature)),
            0.0,
            "control_change_ok",
        );
    }

    #[test]
    fn selected_gain_control_does_not_count_as_giving_away() {
        let mut state = GameState::new_two_player(42);
        let source_id = add_creature(&mut state, AI, "Control Source");
        let own_creature = add_creature(&mut state, AI, "Own Bear");

        assert_score(
            score_target(
                &state,
                source_id,
                Effect::GainControl {
                    target: TargetFilter::Any,
                },
                TargetRef::Object(own_creature),
            ),
            0.0,
            "control_change_ok",
        );
    }

    #[test]
    fn activation_scans_sub_abilities_for_give_control() {
        let mut state = GameState::new_two_player(42);
        let source_id = add_creature(&mut state, AI, "Nested Donation");
        add_creature(&mut state, AI, "Own Bear");

        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GiveControl {
                target: TargetFilter::Any,
                recipient: TargetFilter::Player,
            },
        )));
        Arc::make_mut(&mut state.objects.get_mut(&source_id).unwrap().abilities).push(ability);

        assert_score(
            score_activation(&state, source_id),
            CONTROL_CHANGE_PENALTY,
            "control_change_gives_away_permanent",
        );
    }

    #[test]
    fn non_control_target_selection_is_neutral() {
        let mut state = GameState::new_two_player(42);
        let source_id = add_creature(&mut state, AI, "Pump Source");
        let own_creature = add_creature(&mut state, AI, "Own Bear");

        assert_score(
            score_target(
                &state,
                source_id,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                TargetRef::Object(own_creature),
            ),
            0.0,
            "control_change_ok",
        );
    }
}
