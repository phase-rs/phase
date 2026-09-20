use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;

use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

/// Preserve the historical AI preference to return an owned commander while
/// leaving both CR 903.9a answers in the engine-issued action domain.
pub struct CommanderZoneReturnPolicy;

impl TacticalPolicy for CommanderZoneReturnPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CommanderZoneReturn
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &engine::types::game_state::GameState,
        _player: engine::types::player::PlayerId,
    ) -> Option<f32> {
        // activation-constant: unconditional return preference; prompt gating lives in `verdict`.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        if matches!(
            (&ctx.decision.waiting_for, &ctx.candidate.action),
            (
                WaitingFor::CommanderZoneChoice { .. },
                GameAction::DecideOptionalEffect { accept: false },
            )
        ) {
            PolicyVerdict::reject(PolicyReason::new("commander_zone_return_preferred"))
        } else {
            PolicyVerdict::neutral(PolicyReason::new("commander_zone_return_neutral"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::player::PlayerId;

    #[test]
    fn registered_policy_rejects_decline_but_not_accept() {
        let state = GameState::new_two_player(8874);
        let config = crate::config::AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);
        let waiting_for = WaitingFor::CommanderZoneChoice {
            player: PlayerId(0),
            commander_id: engine::types::identifiers::ObjectId(1),
            current_zone: engine::types::zones::Zone::Graveyard,
        };
        let decision = AiDecisionContext {
            waiting_for,
            candidates: Vec::new(),
        };
        let registry = crate::policies::PolicyRegistry::default();
        let verdict = |accept| {
            let candidate = CandidateAction {
                action: GameAction::DecideOptionalEffect { accept },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
            };
            registry
                .verdicts(&PolicyContext {
                    state: &state,
                    decision: &decision,
                    candidate: &candidate,
                    ai_player: PlayerId(0),
                    config: &config,
                    context: &context,
                    cast_facts: None,
                    search_depth: super::super::context::SearchDepth::Root,
                })
                .into_iter()
                .find(|(id, _)| *id == PolicyId::CommanderZoneReturn)
                .map(|(_, verdict)| verdict)
                .expect("the commander return policy must be registered")
        };

        assert!(matches!(verdict(false), PolicyVerdict::Reject { .. }));
        assert!(matches!(
            verdict(true),
            PolicyVerdict::Score { delta: 0.0, .. }
        ));

        let decline_candidate = CandidateAction {
            action: GameAction::DecideOptionalEffect { accept: false },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        assert_eq!(
            registry.score(&PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &decline_candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &context,
                cast_facts: None,
                search_depth: super::super::context::SearchDepth::Root,
            }),
            f64::NEG_INFINITY,
            "registered commander return rejection must gate aggregate scoring"
        );

        let adjacent_decision = AiDecisionContext {
            waiting_for: WaitingFor::OptionalEffectChoice {
                player: PlayerId(0),
                source_id: engine::types::identifiers::ObjectId(1),
                description: Some("adjacent optional effect".to_string()),
                may_trigger_key: None,
                same_card_may_trigger_choice_available: false,
            },
            candidates: Vec::new(),
        };
        let adjacent_candidate = CandidateAction {
            action: GameAction::DecideOptionalEffect { accept: false },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let adjacent = CommanderZoneReturnPolicy.verdict(&PolicyContext {
            state: &state,
            decision: &adjacent_decision,
            candidate: &adjacent_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: super::super::context::SearchDepth::Root,
        });
        assert!(matches!(adjacent, PolicyVerdict::Score { delta: 0.0, .. }));
    }
}
