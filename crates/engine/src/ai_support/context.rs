use crate::types::game_state::{GameState, WaitingFor};

use crate::game::turn_control;
use crate::types::actions::GameAction;
use crate::types::player::PlayerId;

use super::{
    candidates::{candidate_actions_for_semantic_owner_with_probe, CandidateAction},
    FilterPipeline,
};

#[derive(Debug, Clone)]
pub struct AiDecisionContext {
    pub waiting_for: WaitingFor,
    pub candidates: Vec<CandidateAction>,
}

/// The action bounds issued by the engine for one AI decision.
///
/// `WaitingFor` carries the typed bounds for individual choice fields; this
/// contract is the corresponding complete bound for compound actions. Discrete
/// choices are issued as candidates, while combinatorial combat declarations
/// are bounded by the authoritative combat validators. A consumer must submit
/// an in-bound action for the semantic owner and authorized actor recorded here,
/// rather than reconstructing an action from partial UI state.
#[derive(Debug, Clone)]
pub struct AiDecisionContract {
    pub semantic_owner: PlayerId,
    pub authorized_actor: PlayerId,
    pub state_revision: u64,
    pub candidates: Vec<CandidateAction>,
}

impl AiDecisionContract {
    pub fn issue(state: &GameState, semantic_owner: PlayerId) -> Self {
        Self {
            semantic_owner,
            authorized_actor: resolve_all_frozen_actor(state, semantic_owner).unwrap_or_else(
                || turn_control::authorized_submitter_for_player(state, semantic_owner),
            ),
            state_revision: state.state_revision,
            // The engine's candidate enumerator is the authoritative finite
            // domain for this prompt. Combat and search continuations remain
            // reducer-owned. Choices that can change either a pending spell's
            // target requirements or its final mana obligation cross the reducer
            // before issue. Submission still performs the public action-boundary
            // apply after exact-membership and owner checks.
            candidates: {
                let mut candidates =
                    candidate_actions_for_semantic_owner_with_probe(state, semantic_owner, None);
                if decision_contract_requires_reducer_validation(state) {
                    candidates = FilterPipeline::default_pipeline().apply(state, candidates);
                }
                candidates.sort_by(|left, right| left.action.cmp_stable(&right.action));
                candidates
            },
        }
    }

    /// Checks the values that are stable within an engine state. Transport
    /// session/generation invalidation belongs to the authority that mints the
    /// opaque proposal token (WASM/server), because a restored state resets its
    /// serialized revision.
    pub fn permits(&self, state: &GameState, actor: PlayerId, action: &GameAction) -> bool {
        let semantic_owner_is_active = state
            .waiting_for
            .acting_players()
            .contains(&self.semantic_owner)
            || matches!(
                action,
                GameAction::RevokeResolveAllConsent { representative, .. }
                    if *representative == self.semantic_owner
            );
        let authorized_actor = match action {
            GameAction::RevokeResolveAllConsent {
                epoch,
                representative,
            } => turn_control::resolve_all_granted_submitter(state, *epoch, *representative),
            _ => Some(turn_control::authorized_submitter_for_player(
                state,
                self.semantic_owner,
            )),
        };
        self.state_revision == state.state_revision
            && semantic_owner_is_active
            && self.authorized_actor == actor
            && authorized_actor == Some(actor)
            && self.contains_action(state, action)
    }

    /// Whether an action is in this contract's finite domain.
    ///
    /// `SelectCards` is a selection, not an ordering instruction; the engine
    /// exposes separate actions for ordered choices. Preserve its issued card
    /// set and exact cardinality while accepting any presentation order.
    ///
    /// Combat declaration vectors have a combinatorial legal domain, so their
    /// bounds are checked by the engine's single combat validator rather than
    /// by the heuristic candidate sample.
    pub fn contains_action(&self, state: &GameState, action: &GameAction) -> bool {
        match (&state.waiting_for, action) {
            (
                WaitingFor::DeclareAttackers { player, .. },
                GameAction::DeclareAttackers { attacks, bands },
            ) if *player == self.semantic_owner => {
                crate::game::combat::validate_attack_declaration(state, attacks, bands).is_ok()
            }
            (
                WaitingFor::DeclareBlockers { player, .. },
                GameAction::DeclareBlockers { assignments },
            ) if *player == self.semantic_owner => {
                crate::game::combat::validate_blockers_for_player(state, *player, assignments)
                    .is_ok()
            }
            _ => self
                .candidates
                .iter()
                .any(|candidate| candidate_action_matches(&candidate.action, action)),
        }
    }
}

fn resolve_all_frozen_actor(state: &GameState, representative: PlayerId) -> Option<PlayerId> {
    let epoch = match &state.waiting_for {
        WaitingFor::ResolveAllConsent { epoch, .. } | WaitingFor::ResolveAllReady { epoch } => {
            *epoch
        }
        _ => return None,
    };
    turn_control::resolve_all_granted_submitter(state, epoch, representative).or_else(|| {
        state
            .resolve_all_consent_run
            .as_ref()
            .filter(|run| run.epoch == epoch)
            .and_then(|run| run.authorized_submitter_for(representative))
    })
}

pub(crate) fn target_selection_requires_reducer_validation(state: &GameState) -> bool {
    // CR 601.2c + CR 601.2e-h + CR 602.2b: selecting a target can complete
    // target declaration and immediately check legality and pay the proposed
    // spell or activation cost. A later optional slot can become auto-skippable
    // only after this target is chosen, so the reducer is the sole authority for
    // whether a particular candidate completes the transition.
    matches!(&state.waiting_for, WaitingFor::TargetSelection { .. })
}

/// Whether a decision needs the reducer to validate an in-progress cast.
///
/// CR 601.2b-c: a kicker declaration precedes target selection and may replace
/// the spell's target requirements. The capability contract must therefore
/// simulate each such payment decision before issuing it; otherwise an AI can
/// decline the only target-enabling kicker and receive a targetless cast.
/// CR 601.2h: an unfinished mana payment cannot be finalized, so its pass
/// candidate must likewise be simulated before it enters the contract.
fn decision_contract_requires_reducer_validation(state: &GameState) -> bool {
    target_selection_requires_reducer_validation(state)
        || matches!(&state.waiting_for, WaitingFor::ManaPayment { .. })
        || matches!(
            &state.waiting_for,
            WaitingFor::OptionalCostChoice {
                pending_cast,
                ..
            } if pending_cast.deferred_target_selection
        )
}

fn candidate_action_matches(issued: &GameAction, submitted: &GameAction) -> bool {
    match (issued, submitted) {
        (
            GameAction::SelectCards { cards: issued },
            GameAction::SelectCards { cards: submitted },
        ) => same_unordered_objects(issued, submitted),
        (
            GameAction::ChooseKeptCreatures { kept: issued },
            GameAction::ChooseKeptCreatures { kept: submitted },
        )
        | (
            GameAction::ChooseKeptPermanents { kept: issued },
            GameAction::ChooseKeptPermanents { kept: submitted },
        ) => same_unordered_objects(issued, submitted),
        (
            GameAction::DeclareAttackers {
                attacks: issued,
                bands: issued_bands,
            },
            GameAction::DeclareAttackers {
                attacks: submitted,
                bands: submitted_bands,
            },
        ) => same_unordered(issued, submitted) && issued_bands == submitted_bands,
        (
            GameAction::DeclareBlockers {
                assignments: issued,
            },
            GameAction::DeclareBlockers {
                assignments: submitted,
            },
        ) => same_unordered(issued, submitted),
        _ => issued == submitted,
    }
}

fn same_unordered_objects(
    issued: &[crate::types::identifiers::ObjectId],
    submitted: &[crate::types::identifiers::ObjectId],
) -> bool {
    same_unordered(issued, submitted)
}

fn same_unordered<T: Clone + Ord>(issued: &[T], submitted: &[T]) -> bool {
    let mut issued = issued.to_vec();
    let mut submitted = submitted.to_vec();
    issued.sort_unstable();
    submitted.sort_unstable();
    issued == submitted
}

pub fn build_decision_context(state: &GameState) -> AiDecisionContext {
    // The tactical layer must receive the same finite, engine-issued domain as
    // the action boundary. Returning a reconstructed action here is how a
    // policy could select arguments outside the prompt's bounds.
    let semantic_owner = state
        .waiting_for
        .acting_player()
        .or_else(|| state.waiting_for.acting_players().first().copied());
    let candidates = semantic_owner.map_or_else(Vec::new, |owner| {
        build_decision_context_for_semantic_owner(state, owner).candidates
    });
    AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates,
    }
}

/// Build the AI view for one named semantic decision owner. Callers that
/// already know which pending player they are selecting for (simultaneous
/// mulligans and controlled turns) must use this rather than accepting the
/// generic context's first pending owner.
pub fn build_decision_context_for_semantic_owner(
    state: &GameState,
    semantic_owner: PlayerId,
) -> AiDecisionContext {
    AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates: AiDecisionContract::issue(state, semantic_owner).candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::{
        actions::GameAction,
        card_type::CoreType,
        identifiers::{CardId, ObjectId},
        player::PlayerId,
        zones::Zone,
        Phase,
    };

    /// Issue #4878: the decision context is consumed directly by phase-ai, so
    /// it must canonicalize candidate enumeration order before trajectories
    /// score tied actions. The hand deliberately enumerates the two land
    /// actions in descending object-id order; removing the context sort makes
    /// this assertion fail while tests for other candidate consumers still pass.
    #[test]
    fn build_decision_context_canonicalizes_candidate_action_order() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        let first_land = create_object(
            &mut state,
            CardId(1),
            player,
            "First Land".to_string(),
            Zone::Hand,
        );
        let second_land = create_object(
            &mut state,
            CardId(2),
            player,
            "Second Land".to_string(),
            Zone::Hand,
        );
        for object_id in [first_land, second_land] {
            state
                .objects
                .get_mut(&object_id)
                .expect("created land must exist")
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        state.players[0].hand = [second_land, first_land].into_iter().collect();

        let context = build_decision_context(&state);
        let land_actions: Vec<_> = context
            .candidates
            .iter()
            .filter_map(|candidate| match &candidate.action {
                GameAction::PlayLand { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();

        assert_eq!(land_actions, vec![first_land, second_land]);
    }

    #[test]
    fn decision_contract_requires_an_exact_issued_action() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
        let land = create_object(
            &mut state,
            CardId(1),
            player,
            "Bounded Land".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .expect("created land must exist")
            .card_types
            .core_types
            .push(CoreType::Land);
        state.players[0].hand.push_back(land);

        let contract = AiDecisionContract::issue(&state, player);
        let issued = GameAction::PlayLand {
            object_id: land,
            card_id: CardId(1),
        };
        assert!(contract.permits(&state, player, &issued));
        assert!(!contract.permits(
            &state,
            player,
            &GameAction::PlayLand {
                object_id: ObjectId(999),
                card_id: CardId(1),
            },
        ));
    }

    #[test]
    fn decision_contract_accepts_an_issued_card_selection_in_any_order() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            player,
            "First".to_string(),
            Zone::Hand,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            player,
            "Second".to_string(),
            Zone::Hand,
        );
        state.waiting_for = WaitingFor::OpeningHandBottomCards {
            pending: vec![crate::types::game_state::MulliganBottomEntry { player, count: 2 }],
            reason: crate::types::game_state::OpeningHandBottomReason::TinyLeadersMultiCommander,
        };

        let contract = AiDecisionContract::issue(&state, player);
        assert!(contract.permits(
            &state,
            player,
            &GameAction::SelectCards {
                cards: vec![second, first],
            },
        ));
    }
}
