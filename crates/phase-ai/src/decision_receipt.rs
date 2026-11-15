use std::cmp::Ordering;

use engine::ai_support::AiDecisionContract;
use engine::types::GameAction;
use serde::Serialize;

/// A read-only explanation of an already-minted AI proposal. This is deliberately
/// separate from selection: consumers may inspect it, but cannot use it to mint
/// or alter a game action.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiDecisionDiagnosticReceipt {
    pub semantic_owner: u8,
    pub authorized_actor: u8,
    pub selected_action: GameAction,
    pub status: AiDecisionReceiptStatus,
    /// Engine-authored selection outcome; shown verbatim by local diagnostics.
    pub selection_explanation: String,
    /// Temperature used by the ranked softmax selector. `None` means a direct
    /// policy chose the action without a scored candidate distribution.
    pub sampling_temperature: Option<f64>,
    pub candidates: Vec<AiDecisionDiagnosticCandidate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AiDecisionReceiptStatus {
    Ranked,
    Direct,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiDecisionDiagnosticCandidate {
    pub action: GameAction,
    /// Engine-resolved name of the object this action operates on, when it has
    /// one. WASM enriches this from the authoritative game state for display.
    pub object_name: Option<String>,
    /// Engine-authored display fields for the action payload. The frontend
    /// renders these directly instead of exposing serialized JSON.
    pub details: Vec<AiDecisionDiagnosticField>,
    pub rank: Option<usize>,
    pub is_top_ranked: bool,
    pub is_selected: bool,
    pub score: Option<f64>,
    pub weight: Option<f64>,
    pub probability: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiDecisionDiagnosticField {
    pub label: String,
    pub value: String,
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

/// The diagnostic rank order mirrors `softmax_select_pairs`' degenerate
/// fallback. It is intentionally used only for receipt annotations; selection
/// retains the caller's score-vector order and its existing softmax behavior.
pub fn ranked_candidate_cmp(
    left: &(GameAction, f64),
    left_index: usize,
    right: &(GameAction, f64),
    right_index: usize,
) -> Ordering {
    right
        .1
        .partial_cmp(&left.1)
        .unwrap_or(Ordering::Equal)
        .then_with(|| right.0.cmp_stable(&left.0))
        .then_with(|| left_index.cmp(&right_index))
}

pub fn ranked_receipt(
    contract: &AiDecisionContract,
    scored: &[(GameAction, f64)],
    selected_index: Option<usize>,
    temperature: f64,
    selected_action: GameAction,
) -> AiDecisionDiagnosticReceipt {
    let max_score = scored
        .iter()
        .map(|(_, score)| *score)
        .fold(f64::NEG_INFINITY, f64::max);
    let weights: Vec<f64> = scored
        .iter()
        .map(|(_, score)| ((*score - max_score) / temperature).exp())
        .collect();
    let total: f64 = weights.iter().sum();
    let probabilities = (total.is_finite() && total > 0.0).then(|| {
        weights
            .iter()
            .map(|weight| *weight / total)
            .collect::<Vec<_>>()
    });

    let mut order: Vec<usize> = (0..scored.len()).collect();
    order.sort_by(|left, right| {
        ranked_candidate_cmp(&scored[*left], *left, &scored[*right], *right)
    });
    let mut ranks = vec![0; scored.len()];
    for (position, index) in order.into_iter().enumerate() {
        ranks[index] = position + 1;
    }

    let selection_explanation = match selected_index {
        Some(index) if ranks[index] == 1 => format!(
            "Softmax sampled the top-ranked action ({:.1}%) at temperature {temperature:.2}.",
            probabilities.as_ref().map_or(0.0, |items| items[index] * 100.0),
        ),
        Some(index) => format!(
            "Softmax sampled rank {} ({:.1}%) instead of rank 1 ({:.1}%) at temperature {temperature:.2}.",
            ranks[index],
            probabilities.as_ref().map_or(0.0, |items| items[index] * 100.0),
            probabilities.as_ref().map_or(0.0, |items| {
                let top_index = ranks.iter().position(|rank| *rank == 1).expect("rank one exists");
                items[top_index] * 100.0
            }),
        ),
        None => "No ranked action was selected.".to_string(),
    };

    AiDecisionDiagnosticReceipt {
        semantic_owner: contract.semantic_owner.0,
        authorized_actor: contract.authorized_actor.0,
        selected_action,
        status: AiDecisionReceiptStatus::Ranked,
        selection_explanation,
        sampling_temperature: finite(temperature),
        candidates: scored
            .iter()
            .enumerate()
            .map(|(index, (action, score))| AiDecisionDiagnosticCandidate {
                action: action.clone(),
                object_name: None,
                details: Vec::new(),
                rank: Some(ranks[index]),
                is_top_ranked: ranks[index] == 1,
                is_selected: selected_index == Some(index),
                score: finite(*score),
                weight: finite(weights[index]),
                probability: probabilities
                    .as_ref()
                    .and_then(|items| finite(items[index])),
            })
            .collect(),
    }
}

pub fn direct_receipt(
    contract: &AiDecisionContract,
    selected_action: GameAction,
) -> AiDecisionDiagnosticReceipt {
    let mut selected_row_found = false;
    AiDecisionDiagnosticReceipt {
        semantic_owner: contract.semantic_owner.0,
        authorized_actor: contract.authorized_actor.0,
        selected_action: selected_action.clone(),
        status: AiDecisionReceiptStatus::Direct,
        selection_explanation:
            "A direct AI policy selected this action; no scored distribution was used.".to_string(),
        sampling_temperature: None,
        candidates: contract
            .candidates
            .iter()
            .map(|candidate| {
                // Direct strategies retain the selected action separately for
                // synthesized actions. When the action was contract-issued,
                // mark its first issuance exactly once, preserving candidate
                // vector order even if an issuer contains duplicate actions.
                let is_selected = !selected_row_found && candidate.action == selected_action;
                selected_row_found |= is_selected;
                AiDecisionDiagnosticCandidate {
                    action: candidate.action.clone(),
                    object_name: None,
                    details: Vec::new(),
                    is_selected,
                    rank: None,
                    is_top_ranked: false,
                    score: None,
                    weight: None,
                    probability: None,
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
    use engine::types::PlayerId;

    fn contract(actions: Vec<GameAction>) -> AiDecisionContract {
        AiDecisionContract {
            semantic_owner: PlayerId(0),
            authorized_actor: PlayerId(0),
            state_revision: 1,
            candidates: actions
                .into_iter()
                .map(|action| CandidateAction {
                    action,
                    metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Utility),
                })
                .collect(),
        }
    }

    #[test]
    fn direct_receipt_marks_the_single_issued_selected_row() {
        let pass = GameAction::PassPriority;
        let receipt = direct_receipt(&contract(vec![pass.clone()]), pass.clone());

        assert_eq!(receipt.selected_action, pass);
        assert_eq!(receipt.status, AiDecisionReceiptStatus::Direct);
        assert!(receipt.candidates[0].is_selected);
        assert_eq!(receipt.candidates[0].rank, None);
    }

    #[test]
    fn ranked_receipt_preserves_vector_order_and_annotates_selection() {
        let pass = GameAction::PassPriority;
        let choice = GameAction::ChoosePlayDraw { play_first: true };
        let contract = contract(vec![pass.clone(), choice.clone()]);
        let receipt = ranked_receipt(
            &contract,
            &[(pass.clone(), 0.0), (choice.clone(), 0.0)],
            Some(1),
            1.0,
            choice,
        );

        assert_eq!(receipt.candidates[0].action, pass);
        assert!(receipt.candidates[1].is_selected);
        assert_eq!(
            receipt
                .candidates
                .iter()
                .filter(|candidate| candidate.is_top_ranked)
                .count(),
            1
        );
        assert_eq!(
            receipt.candidates[0].rank.unwrap() + receipt.candidates[1].rank.unwrap(),
            3
        );
        assert_eq!(receipt.candidates[0].weight, Some(1.0));
        assert_eq!(receipt.candidates[0].probability, Some(0.5));
    }

    #[test]
    fn nonfinite_metrics_serialize_as_null_options() {
        let pass = GameAction::PassPriority;
        let receipt = ranked_receipt(
            &contract(vec![pass.clone()]),
            &[(pass.clone(), f64::NAN)],
            Some(0),
            1.0,
            pass,
        );

        assert_eq!(receipt.candidates[0].score, None);
        assert_eq!(receipt.candidates[0].weight, None);
        assert_eq!(receipt.candidates[0].probability, None);
    }
}
