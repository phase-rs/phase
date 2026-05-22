//! ComboLinePolicy — boosts priors on candidate actions that progress a
//! reachable combo line. Gating: `activation()` returns `None` unless
//! `features.is_cedh`, so non-cEDH decks pay zero cost (the per-DecisionKind
//! index in PolicyRegistry still includes us, but activation skips us).

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::combo::{ComboReachability, ComboRegistry};
use crate::features::DeckFeatures;
use crate::policies::context::PolicyContext;
use crate::policies::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy,
};

/// One-line policy: when a combo is reachable this turn, boost actions in
/// the combo's required sequence. When reachable next turn, boost
/// tutor/draw/ramp actions that close the gap.
///
/// Holds an owned `ComboRegistry`. Constructed once per policy registry
/// instantiation. The registry's `reachable_lines` call is cheap-enough to
/// run per candidate at the skeleton stage; caching is a Phase-N optimisation.
pub struct ComboLinePolicy {
    registry: ComboRegistry,
}

impl ComboLinePolicy {
    pub fn new() -> Self {
        Self {
            registry: ComboRegistry::default(),
        }
    }
}

impl Default for ComboLinePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl TacticalPolicy for ComboLinePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ComboLineProgress
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[
            DecisionKind::CastSpell,
            DecisionKind::ActivateAbility,
            DecisionKind::SelectTarget,
        ]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.is_cedh {
            // activation-constant: combo-line guidance is only active for cEDH decks.
            Some(1.0)
        } else {
            None
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // TODO(cedh-perf): cache reachable_lines() by (quick_state_hash(state), ai_player)
        // — verdict() runs per candidate, and CastSpell/ActivateAbility/SelectTarget
        // can each carry many candidates. The query is repeated for sibling search
        // nodes at the same game position. Defer until real combo lines populate
        // the registry (current skeleton uses one stub line — O(1) per call).
        let reachable = self.registry.reachable_lines(ctx.state, ctx.ai_player);
        for (_id, reachability) in &reachable {
            match reachability {
                ComboReachability::ReachableThisTurn { .. }
                    if action_progresses_combo(&ctx.candidate.action) =>
                {
                    let bonus = ctx.config.policy_penalties.combo_progress_this_turn_bonus;
                    return PolicyVerdict::Score {
                        delta: bonus,
                        reason: PolicyReason::new("combo_line_this_turn"),
                    };
                }
                ComboReachability::ReachableNextTurn { .. }
                    if action_is_tutor_or_draw_or_ramp(&ctx.candidate.action) =>
                {
                    let bonus = ctx.config.policy_penalties.combo_progress_next_turn_bonus;
                    return PolicyVerdict::Score {
                        delta: bonus,
                        reason: PolicyReason::new("combo_line_next_turn"),
                    };
                }
                _ => {}
            }
        }
        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("combo_line_no_match"),
        }
    }
}

/// MVP-shaped detector: action is "combo-progressing" if it casts/activates
/// any spell or ability. Tightening this against the line's
/// `action_sequence` is a follow-up.
fn action_progresses_combo(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::CastSpell { .. }
            | GameAction::ActivateAbility { .. }
            | GameAction::ChooseTarget { .. }
    )
}

/// Conservative MVP heuristic: ramp/tutor/draw all surface as a CastSpell or
/// ActivateAbility. Without inspecting the source card's effects, this
/// over-includes — that's acceptable for the skeleton (the boost is bounded
/// by `combo_progress_next_turn_bonus = +5.0`). Phase-N work tightens this
/// using `crate::effect_classify` once card-data feature tags are confirmed.
fn action_is_tutor_or_draw_or_ramp(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
    use engine::types::actions::GameAction;
    use engine::types::game_state::GameState;
    use engine::types::player::PlayerId;

    use crate::config::{create_config, AiDifficulty, Platform};
    use crate::context::AiContext;
    use crate::features::DeckFeatures;

    fn make_state() -> GameState {
        GameState::new_two_player(0)
    }

    fn make_features(is_cedh: bool) -> DeckFeatures {
        DeckFeatures {
            is_cedh,
            ..DeckFeatures::default()
        }
    }

    #[test]
    fn activation_returns_none_when_not_cedh() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let features = make_features(false);
        let activation = policy.activation(&features, &state, PlayerId(0));
        assert!(activation.is_none());
    }

    #[test]
    fn activation_returns_some_when_is_cedh() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let features = make_features(true);
        let activation = policy.activation(&features, &state, PlayerId(0));
        assert_eq!(activation, Some(1.0));
    }

    #[test]
    fn verdict_returns_zero_score_with_no_reachable_combo() {
        // ComboRegistry default has one stub line; empty state -> NotReachable
        // -> reachable_lines is empty -> verdict returns zero.
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        let context = AiContext::empty(&config.weights);

        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Pass,
            },
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = policy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, .. } => assert_eq!(delta, 0.0),
            _ => panic!("expected Score with zero delta, got {verdict:?}"),
        }
    }
}
