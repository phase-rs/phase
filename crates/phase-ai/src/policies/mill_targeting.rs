//! Mill Targeting Optimization Policy
//!
//! Improves targeting for mill effects with conditional payoff, like Szarekh
//! which mills 3 cards and lets you put artifact creature/Vehicle cards from
//! those milled cards into your hand. This policy evaluates target selection
//! based on the probability of hitting desired card types.
//!
//! CR 701.17a: To mill a player, a player puts the top cards of their library
//! into their graveyard. This policy optimizes targeting for mill effects that
//! have conditional retrieval from the milled cards.

use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Bonus for targeting opponents likely to have desired card types.
const TARGET_BONUS: f64 = 0.3;

/// Penalty for self-milling when not beneficial.
const SELF_MILL_PENALTY: f64 = -0.2;

/// Penalty for targeting opponents with empty libraries.
const EMPTY_LIBRARY_PENALTY: f64 = -1.0;

pub struct MillTargetingPolicy;

impl TacticalPolicy for MillTargetingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::MillTargeting
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::SelectTarget]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        Some(1.0) // activation-constant:
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::SelectTargets { targets } = &ctx.candidate.action else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("mill_targeting_na"),
            };
        };

        if targets.is_empty() {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("mill_targeting_no_target"),
            };
        }

        // Check if the ability has mill with conditional payoff
        let has_conditional_payoff = has_mill_with_conditional_payoff(ctx);
        if !has_conditional_payoff {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("mill_targeting_no_conditional"),
            };
        }

        let target = &targets[0];
        let mut delta = 0.0;

        // Check if targeting self
        if let TargetRef::Player(player) = target {
            if *player == ctx.ai_player {
                delta += SELF_MILL_PENALTY;
            } else {
                // Bonus for targeting opponent
                delta += TARGET_BONUS;
            }

            // Check if target's library is empty
            if let Some(player_state) = ctx.state.players.get(player.0 as usize) {
                if player_state.library.is_empty() {
                    delta += EMPTY_LIBRARY_PENALTY;
                }
            }
        }

        PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new("mill_targeting_score"),
        }
    }
}

/// Check if the ability being activated has a mill effect with conditional payoff
/// (e.g., "mill X cards, you may put [type] cards from among them into your hand").
fn has_mill_with_conditional_payoff(ctx: &PolicyContext<'_>) -> bool {
    // Check if the source has a mill ability with conditional retrieval
    ctx.source_object()
        .map(|obj| {
            obj.abilities.iter().any(|ability| {
                let effects = crate::cast_facts::collect_definition_effects(ability);
                let has_mill = effects.iter().any(|e| matches!(e, Effect::Mill { .. }));
                let has_retrieval = effects.iter().any(|e| match e {
                    // CR 701.17a: milled cards go to the graveyard, so a payoff
                    // that retrieves milled cards must source from the graveyard.
                    // A bare library-top `Draw` (Thought Scour) is a cantrip, not
                    // milled-card retrieval, and must not qualify.
                    Effect::ChooseFromZone {
                        zone,
                        additional_zones,
                        ..
                    } => *zone == Zone::Graveyard || additional_zones.contains(&Zone::Graveyard),
                    Effect::ChangeZone {
                        origin: Some(Zone::Graveyard),
                        ..
                    } => true,
                    _ => false,
                });
                has_mill && has_retrieval
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Chooser, QuantityExpr, ResolvedAbility, TargetFilter,
        ZoneOwner,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn add_source_with_ability(state: &mut GameState, ability: AbilityDefinition) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            AI,
            "Mill Source".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(ability);
        id
    }

    fn mill_effect() -> Effect {
        Effect::Mill {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Player,
            destination: Zone::Graveyard,
        }
    }

    fn choose_from_zone_effect() -> Effect {
        Effect::ChooseFromZone {
            count: 1,
            zone: Zone::Graveyard,
            additional_zones: Vec::new(),
            zone_owner: ZoneOwner::Controller,
            filter: None,
            chooser: Chooser::Controller,
            up_to: false,
            selection: engine::types::ability::CardSelectionMode::Chosen,
            constraint: None,
        }
    }

    fn mill_with_payoff_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, mill_effect());
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            choose_from_zone_effect(),
        )));
        ability
    }

    fn score_target(state: &GameState, source_id: ObjectId, target: TargetRef) -> PolicyVerdict {
        let pending = ResolvedAbility::new(mill_effect(), Vec::new(), source_id, AI);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::MultiTargetSelection {
                player: AI,
                legal_targets: Vec::new(),
                min_targets: 1,
                max_targets: 1,
                pending_ability: Box::new(pending),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectTargets {
                targets: vec![target],
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Target),
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
        MillTargetingPolicy.verdict(&ctx)
    }

    fn add_library_card(state: &mut GameState, owner: PlayerId) {
        create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            owner,
            "Library Card".to_string(),
            Zone::Library,
        );
    }

    fn score_delta(verdict: PolicyVerdict, expected_reason: &str) -> f64 {
        let PolicyVerdict::Score { delta, reason } = verdict else {
            panic!("expected score verdict");
        };
        assert_eq!(reason.kind, expected_reason);
        delta
    }

    #[test]
    fn opponent_target_gets_bonus_for_mill_with_choose_from_zone_payoff() {
        let mut state = GameState::new_two_player(42);
        let source_id = add_source_with_ability(&mut state, mill_with_payoff_ability());
        add_library_card(&mut state, OPP);

        let delta = score_delta(
            score_target(&state, source_id, TargetRef::Player(OPP)),
            "mill_targeting_score",
        );

        assert_eq!(delta, TARGET_BONUS);
    }

    #[test]
    fn self_target_gets_self_mill_penalty_for_conditional_payoff() {
        let mut state = GameState::new_two_player(42);
        let source_id = add_source_with_ability(&mut state, mill_with_payoff_ability());
        add_library_card(&mut state, AI);

        let delta = score_delta(
            score_target(&state, source_id, TargetRef::Player(AI)),
            "mill_targeting_score",
        );

        assert_eq!(delta, SELF_MILL_PENALTY);
    }

    fn draw_effect() -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    }

    fn change_zone_from_graveyard_effect() -> Effect {
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Hand,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        }
    }

    fn mill_with_sub_effect(sub: Effect) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, mill_effect());
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            sub,
        )));
        ability
    }

    #[test]
    fn mill_plus_library_draw_is_not_conditional_payoff() {
        // Thought Scour class: `Mill{Player}` + `Draw{Controller}` is a library-top
        // cantrip, not milled-card retrieval (milled cards land in the graveyard,
        // CR 701.17a). It must NOT qualify — reverting the tightening (re-adding the
        // bare `Effect::Draw` arm) flips this to `mill_targeting_score`.
        let mut state = GameState::new_two_player(42);
        let source_id = add_source_with_ability(&mut state, mill_with_sub_effect(draw_effect()));
        add_library_card(&mut state, OPP);

        let delta = score_delta(
            score_target(&state, source_id, TargetRef::Player(OPP)),
            "mill_targeting_no_conditional",
        );

        assert_eq!(delta, 0.0);
    }

    #[test]
    fn mill_plus_change_zone_from_graveyard_qualifies() {
        // Positive reach-guard: a graveyard-sourced retrieval
        // (`ChangeZone{ origin: Graveyard }`) after a mill IS milled-card retrieval,
        // so it still qualifies for the opponent-target bonus.
        let mut state = GameState::new_two_player(42);
        let source_id = add_source_with_ability(
            &mut state,
            mill_with_sub_effect(change_zone_from_graveyard_effect()),
        );
        add_library_card(&mut state, OPP);

        let delta = score_delta(
            score_target(&state, source_id, TargetRef::Player(OPP)),
            "mill_targeting_score",
        );

        assert_eq!(delta, TARGET_BONUS);
    }

    #[test]
    fn mill_without_retrieval_payoff_is_neutral() {
        let mut state = GameState::new_two_player(42);
        let source_id = add_source_with_ability(
            &mut state,
            AbilityDefinition::new(AbilityKind::Activated, mill_effect()),
        );

        let delta = score_delta(
            score_target(&state, source_id, TargetRef::Player(OPP)),
            "mill_targeting_no_conditional",
        );

        assert_eq!(delta, 0.0);
    }
}
