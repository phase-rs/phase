use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::mem::Discriminant;

use engine::ai_support::{
    build_decision_context, AiDecisionContext, CandidateAction, TacticalClass,
};
use engine::game::engine::apply;
use engine::game::players;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::card_hints::should_play_now;
use crate::config::{AiConfig, HiddenInfoMode, OpponentModel, PlannerMode};
use crate::eval::{
    evaluate_for_planner, evaluate_state, strategic_intent, threat_level, StrategicIntent,
};
use crate::policies::context::PolicyContext;
use crate::policies::PolicyRegistry;

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

#[derive(Debug, Clone)]
pub struct PolicyPrior {
    pub candidate: CandidateAction,
    pub prior: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueEstimate {
    pub value: f64,
    pub intent: StrategicIntent,
}

#[derive(Debug, Clone)]
pub struct PlannerEvaluation {
    pub priors: Vec<PolicyPrior>,
    pub value: ValueEstimate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchNodeKey {
    pub state_hash: u64,
    pub actor: Option<PlayerId>,
    pub waiting_for_kind: Discriminant<WaitingFor>,
}

impl SearchNodeKey {
    pub fn new(state: &GameState, actor: Option<PlayerId>) -> Self {
        let waiting_for_kind = std::mem::discriminant(&state.waiting_for);
        let mut hasher = DefaultHasher::new();
        serde_json::to_string(state)
            .unwrap_or_else(|_| format!("{:?}", waiting_for_kind))
            .hash(&mut hasher);
        actor.hash(&mut hasher);
        waiting_for_kind.hash(&mut hasher);
        Self {
            state_hash: hasher.finish(),
            actor,
            waiting_for_kind,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TreeEdge {
    pub candidate: CandidateAction,
    pub prior: f64,
    pub visits: u32,
    pub total_value: f64,
    pub virtual_loss: f64,
    pub child_key: Option<SearchNodeKey>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchNode {
    pub visits: u32,
    pub total_value: f64,
    pub edges: Vec<TreeEdge>,
    pub expanded: bool,
}

#[derive(Debug, Clone, Default)]
pub struct UtilityVector {
    pub self_value: f64,
    pub opponent_pressures: Vec<f64>,
    pub elimination_bonus: f64,
    pub crackback_risk: f64,
}

pub trait UtilityReducer: Send + Sync {
    fn reduce(&self, vector: &UtilityVector) -> f64;
}

#[derive(Debug, Clone, Copy)]
pub struct DuelUtilityReducer;

impl UtilityReducer for DuelUtilityReducer {
    fn reduce(&self, vector: &UtilityVector) -> f64 {
        vector.self_value
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ThreatWeightedUtilityReducer;

impl UtilityReducer for ThreatWeightedUtilityReducer {
    fn reduce(&self, vector: &UtilityVector) -> f64 {
        let pressure_cost = vector.opponent_pressures.iter().sum::<f64>() * 0.2;
        vector.self_value + vector.elimination_bonus - vector.crackback_risk - pressure_cost
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SampledReplyUtilityReducer;

impl UtilityReducer for SampledReplyUtilityReducer {
    fn reduce(&self, vector: &UtilityVector) -> f64 {
        let pressure_cost = vector.opponent_pressures.iter().sum::<f64>() * 0.15;
        vector.self_value + vector.elimination_bonus - vector.crackback_risk - pressure_cost
    }
}

pub trait InformationSetSampler: Send {
    fn determinize(&mut self, visible_state: &GameState, player: PlayerId) -> GameState;
}

#[derive(Debug, Default)]
pub struct PerfectInfoSampler;

impl InformationSetSampler for PerfectInfoSampler {
    fn determinize(&mut self, visible_state: &GameState, _player: PlayerId) -> GameState {
        visible_state.clone()
    }
}

pub struct PlannerServices<'a> {
    pub ai_player: PlayerId,
    pub config: &'a AiConfig,
    pub policies: &'a PolicyRegistry,
    pub information_set_sampler: Box<dyn InformationSetSampler + 'a>,
    pub utility_reducer: Box<dyn UtilityReducer + 'a>,
}

impl<'a> PlannerServices<'a> {
    pub fn new(ai_player: PlayerId, config: &'a AiConfig, policies: &'a PolicyRegistry) -> Self {
        let information_set_sampler: Box<dyn InformationSetSampler + 'a> =
            match config.search.hidden_info_mode {
                HiddenInfoMode::PerfectInfo
                | HiddenInfoMode::Determinized
                | HiddenInfoMode::RevealedOnlyBias => Box::new(PerfectInfoSampler),
            };
        let utility_reducer: Box<dyn UtilityReducer + 'a> = match config.search.opponent_model {
            OpponentModel::DeterministicBestReply if config.player_count <= 2 => {
                Box::new(DuelUtilityReducer)
            }
            OpponentModel::DeterministicBestReply | OpponentModel::ThreatWeightedReply => {
                Box::new(ThreatWeightedUtilityReducer)
            }
            OpponentModel::SampledReply => Box::new(SampledReplyUtilityReducer),
        };

        Self {
            ai_player,
            config,
            policies,
            information_set_sampler,
            utility_reducer,
        }
    }

    pub fn build_decision_context(&self, state: &GameState) -> AiDecisionContext {
        build_decision_context(state)
    }

    pub fn validate_candidates(
        &self,
        state: &GameState,
        candidates: Vec<CandidateAction>,
    ) -> Vec<CandidateAction> {
        candidates
            .into_iter()
            .filter(|candidate| match &candidate.action {
                engine::types::actions::GameAction::PassPriority
                | engine::types::actions::GameAction::ChooseTarget { .. } => true,
                _ => {
                    let mut sim = state.clone();
                    apply(&mut sim, candidate.action.clone()).is_ok()
                }
            })
            .collect()
    }

    pub fn apply_candidate(
        &self,
        state: &GameState,
        candidate: &CandidateAction,
    ) -> Option<GameState> {
        apply_candidate(state, candidate)
    }

    pub fn evaluate_state(&self, state: &GameState) -> f64 {
        evaluate_state(state, self.ai_player, &self.config.weights)
    }

    pub fn evaluate_for_planner(&self, state: &GameState) -> ValueEstimate {
        evaluate_for_planner(state, self.ai_player, &self.config.weights)
    }

    pub fn tactical_score(
        &self,
        state: &GameState,
        ctx: &AiDecisionContext,
        candidate: &CandidateAction,
        scoring_player: PlayerId,
    ) -> f64 {
        let mut score = should_play_now(state, &candidate.action, scoring_player);
        let intent = strategic_intent(state, scoring_player);
        let policy_ctx = PolicyContext {
            state,
            decision: ctx,
            candidate,
            ai_player: scoring_player,
            config: self.config,
        };
        score += self.policies.score(&policy_ctx);

        match candidate.metadata.tactical_class {
            TacticalClass::Pass => {
                score -= 0.1;
                if matches!(
                    intent,
                    StrategicIntent::Develop | StrategicIntent::PushLethal
                ) {
                    score -= 0.15;
                }
            }
            TacticalClass::Mana => score -= 0.05,
            TacticalClass::Land if matches!(intent, StrategicIntent::Develop) => score += 0.2,
            TacticalClass::Attack if matches!(intent, StrategicIntent::PushLethal) => score += 0.3,
            TacticalClass::Block if matches!(intent, StrategicIntent::Stabilize) => score += 0.25,
            _ => {}
        }

        score
    }

    pub fn policy_priors(
        &self,
        state: &GameState,
        ctx: &AiDecisionContext,
        candidates: &[CandidateAction],
        scoring_player: PlayerId,
    ) -> Vec<PolicyPrior> {
        self.policies
            .priors(state, ctx, candidates, scoring_player, self.config)
    }

    pub fn planner_evaluation(&self, state: &GameState) -> PlannerEvaluation {
        let ctx = self.build_decision_context(state);
        let candidates = self.validate_candidates(state, ctx.candidates.clone());
        let scoring_player = state.waiting_for.acting_player().unwrap_or(self.ai_player);
        PlannerEvaluation {
            priors: self.policy_priors(state, &ctx, &candidates, scoring_player),
            value: self.evaluate_for_planner(state),
        }
    }

    pub fn determinize(&mut self, state: &GameState) -> GameState {
        self.information_set_sampler
            .determinize(state, self.ai_player)
    }

    pub fn utility_vector(&self, state: &GameState, value: &ValueEstimate) -> UtilityVector {
        let opponents = players::opponents(state, self.ai_player);
        let elimination_bonus = opponents
            .iter()
            .filter(|&&opp| state.players[opp.0 as usize].life <= 0)
            .count() as f64
            * 25.0;
        let opponent_pressures: Vec<f64> = opponents
            .iter()
            .map(|&opp| threat_level(state, self.ai_player, opp) * 10.0)
            .collect();
        let crackback_risk = opponent_pressures.iter().sum::<f64>()
            - state.players[self.ai_player.0 as usize].life.max(0) as f64;

        UtilityVector {
            self_value: value.value,
            opponent_pressures,
            elimination_bonus,
            crackback_risk: crackback_risk.max(0.0),
        }
    }

    pub fn reduce_utility(&self, state: &GameState, value: &ValueEstimate) -> f64 {
        self.utility_reducer
            .reduce(&self.utility_vector(state, value))
    }

    pub fn rollout_estimate(&mut self, state: &GameState, depth: u32) -> f64 {
        if depth == 0 || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return self.reduce_utility(state, &self.evaluate_for_planner(state));
        }

        let evaluation = self.planner_evaluation(state);
        if evaluation.priors.is_empty() {
            return self.reduce_utility(state, &evaluation.value);
        }

        let rollout_player = state.waiting_for.acting_player().unwrap_or(self.ai_player);
        let sample_count = self.config.search.rollout_samples.max(1) as usize;
        let mut priors = evaluation.priors;
        priors.sort_by(|a, b| {
            b.prior
                .partial_cmp(&a.prior)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let candidates = priors.into_iter().take(sample_count);
        let is_maximizing = rollout_player == self.ai_player;
        candidates
            .filter_map(|prior| {
                let sim = self.apply_candidate(state, &prior.candidate)?;
                let continuation = self.rollout_estimate(&sim, depth - 1);
                Some(continuation + (prior.prior * 0.05))
            })
            .reduce(|best, value| {
                if is_maximizing {
                    best.max(value)
                } else {
                    best.min(value)
                }
            })
            .unwrap_or_else(|| self.reduce_utility(state, &self.evaluate_for_planner(state)))
    }
}

pub trait ContinuationPlanner {
    fn evaluate_after_action(
        &mut self,
        state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64;
}

#[derive(Debug, Clone, Copy)]
pub struct BeamContinuationPlanner {
    pub depth: u32,
    pub rollout_depth: u32,
}

impl BeamContinuationPlanner {
    fn search_value(
        &self,
        state: &GameState,
        depth: u32,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        budget.tick();
        if depth == 0 {
            return services.rollout_estimate(state, self.rollout_depth);
        }
        if budget.exhausted() || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return services.evaluate_state(state);
        }

        let ctx = services.build_decision_context(state);
        let candidates = services.validate_candidates(state, ctx.candidates.clone());
        if candidates.is_empty() {
            return services.evaluate_state(state);
        }

        let node_player = state.waiting_for.acting_player();
        let is_maximizing = node_player.is_none_or(|player| player == services.ai_player);
        let scoring_player = node_player.unwrap_or(services.ai_player);
        let ranked = rank_candidates(
            candidates,
            |candidate| services.tactical_score(state, &ctx, candidate, scoring_player),
            services.config.search.max_branching as usize,
        );

        ranked
            .into_iter()
            .filter_map(|ranked| {
                let sim = services.apply_candidate(state, &ranked.candidate)?;
                let continuation = self.search_value(&sim, depth - 1, services, budget);
                Some(continuation + (ranked.score * 0.05))
            })
            .reduce(|best, value| {
                if is_maximizing {
                    best.max(value)
                } else {
                    best.min(value)
                }
            })
            .unwrap_or_else(|| services.evaluate_state(state))
    }
}

impl ContinuationPlanner for BeamContinuationPlanner {
    fn evaluate_after_action(
        &mut self,
        state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        if self.depth == 0 {
            services.evaluate_state(state)
        } else {
            self.search_value(state, self.depth, services, budget)
        }
    }
}

#[derive(Debug, Clone)]
pub struct MctsPlanner {
    pub config: crate::config::MctsConfig,
    pub tree: HashMap<SearchNodeKey, SearchNode>,
}

impl MctsPlanner {
    pub fn new(config: crate::config::MctsConfig) -> Self {
        Self {
            config,
            tree: HashMap::new(),
        }
    }

    fn root_value(&self, key: &SearchNodeKey) -> Option<f64> {
        let node = self.tree.get(key)?;
        if node.visits == 0 {
            None
        } else {
            Some(node.total_value / node.visits as f64)
        }
    }

    fn select_edge_index(&self, node_key: &SearchNodeKey) -> Option<usize> {
        let node = self.tree.get(node_key)?;
        let parent_visits = node.visits.max(1) as f64;
        node.edges
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| {
                self.puct_score(parent_visits, left)
                    .partial_cmp(&self.puct_score(parent_visits, right))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
    }

    fn puct_score(&self, parent_visits: f64, edge: &TreeEdge) -> f64 {
        let mean_value = if edge.visits == 0 {
            0.0
        } else {
            edge.total_value / edge.visits as f64
        };
        let exploration =
            self.config.c_puct * edge.prior * (parent_visits.sqrt() / (1.0 + edge.visits as f64));
        mean_value + exploration - edge.virtual_loss
    }

    fn expand_node(
        &mut self,
        node_key: SearchNodeKey,
        state: &GameState,
        services: &mut PlannerServices<'_>,
    ) {
        let evaluation = services.planner_evaluation(state);
        let node = self.tree.entry(node_key).or_default();
        if node.expanded {
            return;
        }
        node.edges = evaluation
            .priors
            .into_iter()
            .map(|prior| TreeEdge {
                candidate: prior.candidate,
                prior: prior.prior,
                visits: 0,
                total_value: 0.0,
                virtual_loss: 0.0,
                child_key: None,
            })
            .collect();
        node.expanded = true;
    }

    fn run_simulation(
        &mut self,
        root_state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        let mut state = services.determinize(root_state);
        let mut path: Vec<(SearchNodeKey, Option<usize>)> = Vec::new();
        let utility = loop {
            if budget.exhausted() {
                break services.reduce_utility(&state, &services.evaluate_for_planner(&state));
            }
            budget.tick();

            let actor = state.waiting_for.acting_player();
            let node_key = SearchNodeKey::new(&state, actor);
            let is_terminal = matches!(state.waiting_for, WaitingFor::GameOver { .. });
            let needs_expansion = !self.tree.get(&node_key).is_some_and(|node| node.expanded);
            self.tree.entry(node_key).or_default();
            path.push((node_key, None));

            if is_terminal {
                break services.reduce_utility(&state, &services.evaluate_for_planner(&state));
            }

            if needs_expansion {
                self.expand_node(node_key, &state, services);
                break if self.config.rollout_depth == 0 {
                    services.reduce_utility(&state, &services.evaluate_for_planner(&state))
                } else {
                    services.rollout_estimate(&state, self.config.rollout_depth)
                };
            }

            let Some(edge_index) = self.select_edge_index(&node_key) else {
                break services.reduce_utility(&state, &services.evaluate_for_planner(&state));
            };
            if let Some((_, last_edge)) = path.last_mut() {
                *last_edge = Some(edge_index);
            }

            let Some(candidate) = self
                .tree
                .get(&node_key)
                .and_then(|node| node.edges.get(edge_index))
                .map(|edge| edge.candidate.clone())
            else {
                break services.reduce_utility(&state, &services.evaluate_for_planner(&state));
            };
            let Some(next_state) = services.apply_candidate(&state, &candidate) else {
                break services.reduce_utility(&state, &services.evaluate_for_planner(&state));
            };
            let child_key = SearchNodeKey::new(&next_state, next_state.waiting_for.acting_player());
            if let Some(node) = self.tree.get_mut(&node_key) {
                if let Some(edge) = node.edges.get_mut(edge_index) {
                    edge.child_key = Some(child_key);
                }
            }
            state = next_state;
        };

        for (node_key, edge_index) in path {
            if let Some(node) = self.tree.get_mut(&node_key) {
                node.visits += 1;
                node.total_value += utility;
                if let Some(edge_index) = edge_index {
                    if let Some(edge) = node.edges.get_mut(edge_index) {
                        edge.visits += 1;
                        edge.total_value += utility;
                    }
                }
            }
        }

        utility
    }
}

impl ContinuationPlanner for MctsPlanner {
    fn evaluate_after_action(
        &mut self,
        state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        self.tree.clear();
        let root_key = SearchNodeKey::new(state, state.waiting_for.acting_player());
        for _ in 0..self.config.simulations {
            if budget.exhausted() {
                break;
            }
            self.run_simulation(state, services, budget);
        }

        self.root_value(&root_key).unwrap_or_else(|| {
            services.reduce_utility(state, &services.evaluate_for_planner(state))
        })
    }
}

pub fn build_continuation_planner(config: &AiConfig) -> Box<dyn ContinuationPlanner> {
    match config.search.planner_mode {
        PlannerMode::BeamOnly => Box::new(BeamContinuationPlanner {
            depth: 0,
            rollout_depth: 0,
        }),
        PlannerMode::BeamPlusRollout => Box::new(BeamContinuationPlanner {
            depth: config.search.max_depth.saturating_sub(1),
            rollout_depth: config.search.rollout_depth,
        }),
        PlannerMode::BeamPlusMcts => {
            Box::new(MctsPlanner::new(config.search.mcts.clone().unwrap_or(
                crate::config::MctsConfig {
                    simulations: 24,
                    c_puct: 1.25,
                    rollout_depth: config.search.rollout_depth,
                    exploration_fraction: 0.0,
                    dirichlet_alpha: None,
                },
            )))
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::actions::GameAction;
    use engine::types::card_type::CoreType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    use crate::config::{create_config, AiDifficulty, Platform};

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

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
    fn node_key_is_stable_for_equivalent_state() {
        let state = make_state();
        let key_a = SearchNodeKey::new(&state, Some(PlayerId(0)));
        let key_b = SearchNodeKey::new(&state, Some(PlayerId(0)));
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn perfect_info_sampler_is_no_op() {
        let mut sampler = PerfectInfoSampler;
        let state = make_state();
        assert_eq!(sampler.determinize(&state, PlayerId(0)), state);
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
    fn planner_services_produce_positive_normalized_priors() {
        let state = make_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let services = PlannerServices::new(PlayerId(0), &config, &policies);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                target_slots: Vec::new(),
                target_constraints: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidates = vec![
            CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Player(PlayerId(0))),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Target,
                },
            },
            CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Player(PlayerId(1))),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Target,
                },
            },
        ];

        let priors = services.policy_priors(&state, &decision, &candidates, PlayerId(0));
        assert_eq!(priors.len(), 2);
        assert!(priors
            .iter()
            .all(|prior| prior.prior.is_finite() && prior.prior > 0.0));
        assert!(priors[1].prior > priors[0].prior);
    }

    #[test]
    fn puct_prefers_high_prior_when_visits_are_equal() {
        let planner = MctsPlanner::new(crate::config::MctsConfig {
            simulations: 8,
            c_puct: 1.25,
            rollout_depth: 1,
            exploration_fraction: 0.0,
            dirichlet_alpha: None,
        });
        let node_key = SearchNodeKey::new(&make_state(), Some(PlayerId(0)));
        let mut planner = planner;
        planner.tree.insert(
            node_key,
            SearchNode {
                visits: 4,
                total_value: 0.0,
                edges: vec![
                    TreeEdge {
                        candidate: CandidateAction {
                            action: GameAction::PassPriority,
                            metadata: ActionMetadata {
                                actor: Some(PlayerId(0)),
                                tactical_class: TacticalClass::Pass,
                            },
                        },
                        prior: 0.2,
                        visits: 0,
                        total_value: 0.0,
                        virtual_loss: 0.0,
                        child_key: None,
                    },
                    TreeEdge {
                        candidate: CandidateAction {
                            action: GameAction::MulliganDecision { keep: true },
                            metadata: ActionMetadata {
                                actor: Some(PlayerId(0)),
                                tactical_class: TacticalClass::Selection,
                            },
                        },
                        prior: 0.8,
                        visits: 0,
                        total_value: 0.0,
                        virtual_loss: 0.0,
                        child_key: None,
                    },
                ],
                expanded: true,
            },
        );

        assert_eq!(planner.select_edge_index(&node_key), Some(1));
    }

    #[test]
    fn expansion_uses_legal_engine_candidates() {
        let state = make_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new(PlayerId(0), &config, &policies);
        let mut planner = MctsPlanner::new(config.search.mcts.clone().unwrap());
        let key = SearchNodeKey::new(&state, Some(PlayerId(0)));
        planner.expand_node(key, &state, &mut services);
        let node = planner.tree.get(&key).unwrap();
        assert!(node.expanded);
        assert!(!node.edges.is_empty());
        assert!(node.edges.iter().all(|edge| services
            .apply_candidate(&state, &edge.candidate)
            .is_some()
            || matches!(edge.candidate.action, GameAction::PassPriority)));
    }

    #[test]
    fn backprop_updates_visits_and_value_totals() {
        let mut state = make_state();
        let land_id = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new(PlayerId(0), &config, &policies);
        let mut planner = MctsPlanner::new(config.search.mcts.clone().unwrap());
        let mut budget = SearchBudget::new(32);
        let _ = planner.evaluate_after_action(&state, &mut services, &mut budget);
        let root_key = SearchNodeKey::new(&state, Some(PlayerId(0)));
        let root = planner.tree.get(&root_key).unwrap();
        assert!(root.visits > 0);
        assert!(root.total_value.is_finite());
    }

    #[test]
    fn mcts_respects_budget() {
        let state = make_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new(PlayerId(0), &config, &policies);
        let mut planner = MctsPlanner::new(config.search.mcts.clone().unwrap());
        let mut budget = SearchBudget::new(2);
        let _ = planner.evaluate_after_action(&state, &mut services, &mut budget);
        assert!(budget.exhausted());
    }

    #[test]
    fn mcts_root_visits_do_not_exceed_simulation_budget() {
        let state = make_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new(PlayerId(0), &config, &policies);
        let mut planner = MctsPlanner::new(config.search.mcts.clone().unwrap());
        let mut budget = SearchBudget::new(256);
        let _ = planner.evaluate_after_action(&state, &mut services, &mut budget);
        let root_key = SearchNodeKey::new(&state, Some(PlayerId(0)));
        let root = planner.tree.get(&root_key).unwrap();
        assert!(root.visits <= planner.config.simulations);
    }
}
