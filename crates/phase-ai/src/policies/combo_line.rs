//! ComboLinePolicy — boosts priors on candidate actions that progress a
//! reachable combo line. Gating resolves every reconciled effective bracket tier
//! through one exhaustive table; only cEDH is enabled today, so all other tiers
//! pay zero per-candidate cost.

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
/// instantiation. Per-state reachability results are memoized by `AiSession`.
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
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        use engine::game::bracket_estimate::CommanderBracketTier::{
            Cedh, Core, Exhibition, Optimized, Upgraded,
        };

        match features.effective_bracket_tier.tier() {
            // `None` is the FREE opt-out: the registry skips `verdict()` entirely.
            Exhibition | Core | Upgraded => None,
            // U6 owns turning this row on with the P9 pod baseline attached.
            Optimized => None,
            // activation-constant: combo-line guidance is fully active for cEDH decks.
            Cedh => Some(1.0),
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // `quick_state_hash` is a lossy 64-bit digest; `ProjectionKey` accepts the
        // same exposure, while turn and active-player fields narrow it.
        let reachable =
            ctx.context
                .session
                .get_or_reachable_lines(ctx.state, ctx.ai_player, || {
                    self.registry.reachable_lines(ctx.state, ctx.ai_player)
                });
        for (_id, reachability) in reachable.iter() {
            match reachability {
                // Only fire the bonus when mana is actually available and
                // the candidate matches one of the line's resolved steps.
                // Without this guard the policy would over-boost any
                // spell/ability while mana is short.
                ComboReachability::ReachableThisTurn {
                    missing_mana: 0,
                    required_actions,
                } if required_actions
                    .iter()
                    .any(|step| action_matches_step(&ctx.candidate.action, step)) =>
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

/// True when `candidate` corresponds to one of the line's resolved
/// `GameAction` steps. Compares variants + source identifiers and the
/// ability index; targets are intentionally ignored because the policy fires
/// before target selection (the engine's target-prompt flow handles those
/// separately).
fn action_matches_step(candidate: &GameAction, step: &GameAction) -> bool {
    match (candidate, step) {
        (
            GameAction::ActivateAbility {
                source_id: c_src,
                ability_index: c_idx,
            },
            GameAction::ActivateAbility {
                source_id: s_src,
                ability_index: s_idx,
            },
        ) => c_src == s_src && c_idx == s_idx,
        (
            GameAction::CastSpell {
                object_id: c_obj,
                card_id: c_card,
                ..
            },
            GameAction::CastSpell {
                object_id: s_obj,
                card_id: s_card,
                ..
            },
        ) => c_obj == s_obj && c_card == s_card,
        _ => false,
    }
}

/// Conservative MVP heuristic: ramp/tutor/draw all surface as a CastSpell or
/// ActivateAbility. Without inspecting the source card's effects, this
/// over-includes — acceptable for the next-turn branch because the boost is
/// bounded by `combo_progress_next_turn_bonus = +5.0`. Phase-N work tightens
/// this using `crate::policies::effect_classify` once card-data feature tags
/// are confirmed.
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
    use crate::test_support::{context_with_plans, default_deck_plan};
    use engine::game::bracket_estimate::{effective_tier, CommanderBracketTier};
    use strum::IntoEnumIterator;

    fn make_state() -> GameState {
        GameState::new_two_player(0)
    }

    fn make_features(tier: CommanderBracketTier) -> DeckFeatures {
        DeckFeatures {
            effective_bracket_tier: effective_tier(tier, None),
            ..DeckFeatures::default()
        }
    }

    #[test]
    fn activation_returns_none_when_not_cedh() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let features = make_features(CommanderBracketTier::Core);
        let activation = policy.activation(&features, &state, PlayerId(0));
        assert!(activation.is_none());
    }

    #[test]
    fn activation_returns_some_when_is_cedh() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let features = make_features(CommanderBracketTier::Cedh);
        let activation = policy.activation(&features, &state, PlayerId(0));
        assert_eq!(activation, Some(1.0));
    }

    #[test]
    fn activation_table_is_exhaustive_and_only_cedh_is_on() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let actual: Vec<_> = CommanderBracketTier::iter()
            .map(|tier| policy.activation(&make_features(tier), &state, PlayerId(0)))
            .collect();
        assert_eq!(actual, vec![None, None, None, None, Some(1.0)]);
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
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let verdict = policy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, .. } => assert_eq!(delta, 0.0),
            _ => panic!("expected Score with zero delta, got {verdict:?}"),
        }
    }

    /// Places Heliod, Sun-Crowned + Walking Ballista on PlayerId(0)'s
    /// battlefield with two untapped Plains (so Heliod's {1}{W} is payable in
    /// the color-aware reachability check), making the Heliod/Ballista line
    /// `ReachableThisTurn { missing_mana: 0, .. }`.
    fn heliod_ballista_state() -> (
        GameState,
        engine::types::identifiers::ObjectId,
        engine::types::identifiers::ObjectId,
    ) {
        use engine::game::zones::create_object;
        use engine::types::card_type::CoreType;
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut state = make_state();
        // Two untapped Plains → WW, satisfying {1}{W}.
        for i in 0..2 {
            let land_id = create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                "Plains".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Plains".to_string());
        }
        let heliod_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Heliod, Sun-Crowned".to_string(),
            Zone::Battlefield,
        );
        let ballista_id = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Walking Ballista".to_string(),
            Zone::Battlefield,
        );
        (state, heliod_id, ballista_id)
    }

    fn make_context<'a>(
        state: &'a GameState,
        candidate: &'a CandidateAction,
        decision: &'a engine::ai_support::AiDecisionContext,
        config: &'a crate::config::AiConfig,
        context: &'a AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(0),
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        }
    }

    #[test]
    fn verdict_boosts_heliod_activation_when_reachable_this_turn() {
        let (state, heliod_id, _ballista_id) = heliod_ballista_state();
        let policy = ComboLinePolicy::new();
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        let context = AiContext::empty(&config.weights);

        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: heliod_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Ability),
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let ctx = make_context(&state, &candidate, &decision, &config, &context);

        let verdict = policy.verdict(&ctx);
        let expected = config.policy_penalties.combo_progress_this_turn_bonus;
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(delta, expected, "expected this-turn bonus, got {delta}");
                assert_eq!(reason.kind, "combo_line_this_turn");
            }
            other => panic!("expected Score, got {other:?}"),
        }
    }

    #[test]
    fn verdict_boosts_ballista_activation_when_reachable_this_turn() {
        let (state, _heliod_id, ballista_id) = heliod_ballista_state();
        let policy = ComboLinePolicy::new();
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        let context = AiContext::empty(&config.weights);

        // Ballista's damage ability sits at abilities[1] in card-data.
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: ballista_id,
                ability_index: 1,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Ability),
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let ctx = make_context(&state, &candidate, &decision, &config, &context);

        match policy.verdict(&ctx) {
            PolicyVerdict::Score { delta, .. } => {
                let expected = config.policy_penalties.combo_progress_this_turn_bonus;
                assert_eq!(delta, expected);
            }
            other => panic!("expected Score, got {other:?}"),
        }
    }

    #[test]
    fn verdict_ignores_unrelated_activation_even_with_combo_on_board() {
        // Combo is on the board, but the candidate is some unrelated land's
        // ability — must not receive the bonus.
        let (state, _heliod_id, _ballista_id) = heliod_ballista_state();
        let policy = ComboLinePolicy::new();
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        let context = AiContext::empty(&config.weights);

        // PassPriority is never in any combo line's required_actions.
        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let ctx = make_context(&state, &candidate, &decision, &config, &context);

        match policy.verdict(&ctx) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "combo_line_no_match");
            }
            other => panic!("expected zero-delta Score, got {other:?}"),
        }
    }

    #[test]
    fn reachable_lines_memo_holds_one_entry_per_state_and_isolates_turns() {
        use crate::planner::quick_state_hash;

        let policy = ComboLinePolicy::new();
        let state = make_state();
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        let context = context_with_plans(
            &state,
            PlayerId(0),
            &config,
            &[(PlayerId(0), default_deck_plan())],
        );
        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };

        let ctx = make_context(&state, &candidate, &decision, &config, &context);
        policy.verdict(&ctx);
        policy.verdict(&ctx);
        assert_eq!(
            context.session.reachable_lines_cache.read().unwrap().len(),
            1,
            "identical states must share one reachable-lines memo entry"
        );

        let mut changed_state = state.clone();
        changed_state.next_object_id += 1;
        assert_ne!(quick_state_hash(&state), quick_state_hash(&changed_state));
        let changed_ctx = make_context(&changed_state, &candidate, &decision, &config, &context);
        policy.verdict(&changed_ctx);
        assert_eq!(
            context.session.reachable_lines_cache.read().unwrap().len(),
            2,
            "a distinct state hash must add a second memo entry"
        );

        let mut later_state = state.clone();
        later_state.turn_number += 1;
        let later_ctx = make_context(&later_state, &candidate, &decision, &config, &context);
        policy.verdict(&later_ctx);
        assert_eq!(
            context.session.reachable_lines_cache.read().unwrap().len(),
            3,
            "a later turn must not hit an earlier-turn memo entry"
        );
    }

    #[test]
    fn reachable_lines_memo_never_changes_the_verdict() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        let context = context_with_plans(
            &state,
            PlayerId(0),
            &config,
            &[(PlayerId(0), default_deck_plan())],
        );
        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let ctx = make_context(&state, &candidate, &decision, &config, &context);

        let warm = policy.verdict(&ctx);
        context
            .session
            .reachable_lines_cache
            .write()
            .unwrap()
            .clear();
        let cold = policy.verdict(&ctx);

        match (warm, cold) {
            (
                PolicyVerdict::Score {
                    delta: warm_delta,
                    reason: warm_reason,
                },
                PolicyVerdict::Score {
                    delta: cold_delta,
                    reason: cold_reason,
                },
            ) => {
                assert_eq!(warm_delta, cold_delta);
                assert_eq!(warm_reason.kind, cold_reason.kind);
                assert_eq!(warm_reason.facts, cold_reason.facts);
            }
            verdicts => panic!("expected two Score verdicts, got {verdicts:?}"),
        }
    }
}
