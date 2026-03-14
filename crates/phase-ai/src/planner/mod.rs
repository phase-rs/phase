use engine::ai_support::{build_decision_context, AiDecisionContext, CandidateAction};
use engine::game::engine::apply;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::config::AiConfig;

#[derive(Debug, Clone)]
pub struct RankedCandidate {
    pub candidate: CandidateAction,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct SearchBudget {
    pub max_nodes: u32,
    pub nodes_evaluated: u32,
}

impl SearchBudget {
    pub fn new(max_nodes: u32) -> Self {
        Self {
            max_nodes,
            nodes_evaluated: 0,
        }
    }

    pub fn exhausted(&self) -> bool {
        self.nodes_evaluated >= self.max_nodes
    }

    pub fn tick(&mut self) {
        self.nodes_evaluated += 1;
    }
}

pub struct PlannerContext<'a, FScore, FEval, FActor, FValidate> {
    pub ai_player: PlayerId,
    pub config: &'a AiConfig,
    pub score_candidate: &'a mut FScore,
    pub static_eval: &'a mut FEval,
    pub acting_player: &'a mut FActor,
    pub validate_candidates: &'a mut FValidate,
}

impl<FScore, FEval, FActor, FValidate> PlannerContext<'_, FScore, FEval, FActor, FValidate>
where
    FScore: FnMut(&GameState, &AiDecisionContext, &CandidateAction, PlayerId) -> f64,
    FEval: FnMut(&GameState, PlayerId) -> f64,
    FActor: FnMut(&GameState) -> Option<PlayerId>,
    FValidate: FnMut(&GameState, Vec<CandidateAction>) -> Vec<CandidateAction>,
{
    fn evaluate_state(&mut self, state: &GameState) -> f64 {
        (self.static_eval)(state, self.ai_player)
    }

    fn current_actor(&mut self, state: &GameState) -> Option<PlayerId> {
        (self.acting_player)(state)
    }

    fn validate(
        &mut self,
        state: &GameState,
        candidates: Vec<CandidateAction>,
    ) -> Vec<CandidateAction> {
        (self.validate_candidates)(state, candidates)
    }

    fn score(
        &mut self,
        state: &GameState,
        ctx: &AiDecisionContext,
        candidate: &CandidateAction,
        player: PlayerId,
    ) -> f64 {
        (self.score_candidate)(state, ctx, candidate, player)
    }
}

pub trait RolloutEvaluator {
    fn evaluate<FScore, FEval, FActor, FValidate>(
        &self,
        state: &GameState,
        context: &mut PlannerContext<'_, FScore, FEval, FActor, FValidate>,
    ) -> f64
    where
        FScore: FnMut(&GameState, &AiDecisionContext, &CandidateAction, PlayerId) -> f64,
        FEval: FnMut(&GameState, PlayerId) -> f64,
        FActor: FnMut(&GameState) -> Option<PlayerId>,
        FValidate: FnMut(&GameState, Vec<CandidateAction>) -> Vec<CandidateAction>;
}

#[derive(Debug, Clone, Copy)]
pub struct HeuristicRollout {
    pub depth: u32,
}

impl RolloutEvaluator for HeuristicRollout {
    fn evaluate<FScore, FEval, FActor, FValidate>(
        &self,
        state: &GameState,
        context: &mut PlannerContext<'_, FScore, FEval, FActor, FValidate>,
    ) -> f64
    where
        FScore: FnMut(&GameState, &AiDecisionContext, &CandidateAction, PlayerId) -> f64,
        FEval: FnMut(&GameState, PlayerId) -> f64,
        FActor: FnMut(&GameState) -> Option<PlayerId>,
        FValidate: FnMut(&GameState, Vec<CandidateAction>) -> Vec<CandidateAction>,
    {
        rollout_value(state, self.depth, context)
    }
}

pub fn rank_candidates<F>(
    candidates: impl IntoIterator<Item = CandidateAction>,
    mut scorer: F,
    limit: usize,
) -> Vec<RankedCandidate>
where
    F: FnMut(&CandidateAction) -> f64,
{
    let mut ranked: Vec<RankedCandidate> = candidates
        .into_iter()
        .map(|candidate| RankedCandidate {
            score: scorer(&candidate),
            candidate,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(limit);
    ranked
}

pub fn apply_candidate(state: &GameState, candidate: &CandidateAction) -> Option<GameState> {
    let mut sim = state.clone();
    apply(&mut sim, candidate.action.clone()).ok()?;
    Some(sim)
}

pub fn search_frontier<R, FScore, FEval, FActor, FValidate>(
    state: &GameState,
    depth: u32,
    mut alpha: f64,
    mut beta: f64,
    budget: &mut SearchBudget,
    rollout: &R,
    context: &mut PlannerContext<'_, FScore, FEval, FActor, FValidate>,
) -> f64
where
    R: RolloutEvaluator,
    FScore: FnMut(&GameState, &AiDecisionContext, &CandidateAction, PlayerId) -> f64,
    FEval: FnMut(&GameState, PlayerId) -> f64,
    FActor: FnMut(&GameState) -> Option<PlayerId>,
    FValidate: FnMut(&GameState, Vec<CandidateAction>) -> Vec<CandidateAction>,
{
    budget.tick();

    if depth == 0 {
        return rollout.evaluate(state, context);
    }
    if budget.exhausted() || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        return context.evaluate_state(state);
    }

    let ctx = build_decision_context(state);
    let candidates = context.validate(state, ctx.candidates.clone());
    if candidates.is_empty() {
        return context.evaluate_state(state);
    }

    let node_player = context.current_actor(state);
    let is_maximizing = node_player.is_none_or(|player| player == context.ai_player);
    let scoring_player = node_player.unwrap_or(context.ai_player);
    let max_branch = context.config.search.max_branching as usize;
    let actions_to_search: Vec<_> = rank_candidates(
        candidates,
        |candidate| context.score(state, &ctx, candidate, scoring_player),
        max_branch,
    )
    .into_iter()
    .map(|ranked| ranked.candidate.action)
    .collect();

    if is_maximizing {
        let mut best = f64::NEG_INFINITY;
        for action in actions_to_search {
            let mut sim = state.clone();
            if apply(&mut sim, action).is_ok() {
                let value = search_frontier(&sim, depth - 1, alpha, beta, budget, rollout, context);
                best = best.max(value);
                alpha = alpha.max(value);
                if alpha >= beta {
                    break;
                }
            }
        }
        best
    } else {
        let mut best = f64::INFINITY;
        for action in actions_to_search {
            let mut sim = state.clone();
            if apply(&mut sim, action).is_ok() {
                let value = search_frontier(&sim, depth - 1, alpha, beta, budget, rollout, context);
                best = best.min(value);
                beta = beta.min(value);
                if alpha >= beta {
                    break;
                }
            }
        }
        best
    }
}

fn rollout_value<FScore, FEval, FActor, FValidate>(
    state: &GameState,
    depth: u32,
    context: &mut PlannerContext<'_, FScore, FEval, FActor, FValidate>,
) -> f64
where
    FScore: FnMut(&GameState, &AiDecisionContext, &CandidateAction, PlayerId) -> f64,
    FEval: FnMut(&GameState, PlayerId) -> f64,
    FActor: FnMut(&GameState) -> Option<PlayerId>,
    FValidate: FnMut(&GameState, Vec<CandidateAction>) -> Vec<CandidateAction>,
{
    if depth == 0 || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        return context.evaluate_state(state);
    }

    let ctx = build_decision_context(state);
    let candidates = context.validate(state, ctx.candidates.clone());
    if candidates.is_empty() {
        return context.evaluate_state(state);
    }

    let rollout_player = context.current_actor(state).unwrap_or(context.ai_player);
    let sample_count = context.config.search.rollout_samples.max(1) as usize;
    let sampled_candidates = rank_candidates(
        candidates,
        |candidate| context.score(state, &ctx, candidate, rollout_player),
        sample_count,
    );
    if sampled_candidates.is_empty() {
        return context.evaluate_state(state);
    }

    let is_maximizing = rollout_player == context.ai_player;
    sampled_candidates
        .into_iter()
        .filter_map(|ranked| {
            let sim = apply_candidate(state, &ranked.candidate)?;
            let continuation = rollout_value(&sim, depth - 1, context);
            Some(continuation + (ranked.score * 0.05))
        })
        .reduce(|best, value| {
            if is_maximizing {
                best.max(value)
            } else {
                best.min(value)
            }
        })
        .unwrap_or_else(|| context.evaluate_state(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::player::PlayerId;

    use crate::config::{create_config, AiDifficulty, Platform};

    #[test]
    fn rank_candidates_sorts_and_limits() {
        let candidates = vec![
            CandidateAction {
                action: GameAction::PassPriority,
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Pass,
                },
            },
            CandidateAction {
                action: GameAction::MulliganDecision { keep: true },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
        ];

        let ranked = rank_candidates(
            candidates,
            |candidate| match candidate.action {
                GameAction::MulliganDecision { .. } => 2.0,
                _ => 1.0,
            },
            1,
        );

        assert_eq!(ranked.len(), 1);
        assert!(matches!(
            ranked[0].candidate.action,
            GameAction::MulliganDecision { .. }
        ));
    }

    #[test]
    fn heuristic_rollout_uses_static_eval_when_depth_is_zero() {
        let state = GameState::new_two_player(42);
        let rollout = HeuristicRollout { depth: 0 };
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let mut score_fn = |_state: &GameState,
                            _ctx: &AiDecisionContext,
                            _candidate: &CandidateAction,
                            _player: PlayerId| 1.0;
        let mut eval_fn = |_state: &GameState, _player: PlayerId| 7.5;
        let mut actor_fn = |_state: &GameState| Some(PlayerId(0));
        let mut validate_fn = |_state: &GameState, candidates: Vec<CandidateAction>| candidates;
        let mut context = PlannerContext {
            ai_player: PlayerId(0),
            config: &config,
            score_candidate: &mut score_fn,
            static_eval: &mut eval_fn,
            acting_player: &mut actor_fn,
            validate_candidates: &mut validate_fn,
        };

        let score = rollout.evaluate(&state, &mut context);

        assert_eq!(score, 7.5);
    }

    #[test]
    fn search_budget_tracks_node_count() {
        let mut budget = SearchBudget::new(3);
        assert!(!budget.exhausted());
        budget.tick();
        budget.tick();
        budget.tick();
        assert!(budget.exhausted());
    }

    #[test]
    fn search_frontier_returns_static_eval_for_terminal_state() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };

        let config = create_config(AiDifficulty::Medium, Platform::Native);
        let rollout = HeuristicRollout { depth: 1 };
        let mut budget = SearchBudget::new(4);
        let mut score_fn = |_state: &GameState,
                            _ctx: &AiDecisionContext,
                            _candidate: &CandidateAction,
                            _player: PlayerId| 1.0;
        let mut eval_fn = |_state: &GameState, _player: PlayerId| 99.0;
        let mut actor_fn = |_state: &GameState| Some(PlayerId(0));
        let mut validate_fn = |_state: &GameState, candidates: Vec<CandidateAction>| candidates;
        let mut context = PlannerContext {
            ai_player: PlayerId(0),
            config: &config,
            score_candidate: &mut score_fn,
            static_eval: &mut eval_fn,
            acting_player: &mut actor_fn,
            validate_candidates: &mut validate_fn,
        };
        let score = search_frontier(
            &state,
            2,
            f64::NEG_INFINITY,
            f64::INFINITY,
            &mut budget,
            &rollout,
            &mut context,
        );

        assert_eq!(score, 99.0);
    }
}
