//! `TribalDensityMulligan` — feature-driven mulligan policy for tribal decks.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding to keep after the
//! mulligan process. When a deck's tribal commitment is meaningful, opening
//! hands with a dense mix of tribe members and payoffs are strongly preferred
//! over sparse hands where the tribal plan can't execute.
//!
//! Opts out for decks where `features.tribal.commitment <
//! tribal::MULLIGAN_FLOOR` (0.4) — below that threshold the baseline
//! `KeepablesByLandCount` policy is the sole voice.

use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::tribal::{statics_are_lord_for, MULLIGAN_FLOOR};
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};
use engine::parser::oracle_util::canonicalize_subtype_name;

use super::{MulliganPolicy, MulliganScore, TurnOrder};

pub struct TribalDensityMulligan;

impl MulliganPolicy for TribalDensityMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::TribalDensityMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        _plan: &PlanSnapshot,
        _turn_order: TurnOrder,
        _mulligans_taken: u8,
    ) -> MulliganScore {
        let commitment = features.tribal.commitment;
        if commitment < MULLIGAN_FLOOR {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("tribal_opener_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        let Some(dominant_tribe) = features.tribal.dominant_tribe.as_deref() else {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("tribal_opener_na"),
            };
        };

        // Count tribal cards in hand: faces whose subtypes include the dominant tribe
        // OR whose static abilities constitute a lord/payoff for the dominant tribe.
        let tribal_cards: i64 = hand
            .iter()
            .filter_map(|&oid| state.objects.get(&oid))
            .filter(|obj| {
                // On-tribe member check: CR 205.3.
                let is_member = obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| canonicalize_subtype_name(s) == dominant_tribe);
                // Lord/payoff check: has a lord-class static ability for the dominant tribe.
                // CR 613.4c: lords benefit the tribe at layer 7c.
                let is_lord_like = statics_are_lord_for(&obj.static_definitions, dominant_tribe);
                is_member || is_lord_like
            })
            .count() as i64;

        // Use card counts directly to avoid floating-point edge cases.
        // "density >= 0.43" corresponds to ≥3 of 7; "density < 0.15" to ≤1 of 7.
        if tribal_cards >= 3 {
            // 3+ of 7 tribal cards — strong opener.
            return MulliganScore::Score {
                delta: 1.5,
                reason: PolicyReason::new("tribal_opener_dense")
                    .with_fact("tribal_cards", tribal_cards)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        if tribal_cards <= 1 {
            // 0–1 tribal cards — thin opener, undermines the tribal plan.
            return MulliganScore::Score {
                delta: -1.0,
                reason: PolicyReason::new("tribal_opener_thin")
                    .with_fact("tribal_cards", tribal_cards)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        // 2 tribal cards — defer to baseline.
        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("tribal_opener_defer")
                .with_fact("tribal_cards", tribal_cards),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use crate::features::tribal::TribalFeature;
    use crate::features::DeckFeatures;

    const AI: PlayerId = PlayerId(0);

    fn tribal_features(commitment: f32, dominant: &str) -> DeckFeatures {
        DeckFeatures {
            tribal: TribalFeature {
                dominant_tribe: Some(dominant.to_string()),
                commitment,
                tribes: Vec::new(),
            },
            ..Default::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    fn add_elf(state: &mut GameState, idx: u64) -> ObjectId {
        let oid = create_object(
            state,
            CardId(3000 + idx),
            AI,
            format!("Elf {idx}"),
            Zone::Hand,
        );
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Elf".to_string()],
        };
        oid
    }

    fn add_filler(state: &mut GameState, idx: u64) -> ObjectId {
        let oid = create_object(
            state,
            CardId(4000 + idx),
            AI,
            format!("Filler {idx}"),
            Zone::Hand,
        );
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        oid
    }

    fn make_hand(elves: usize, fillers: usize) -> (GameState, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();
        let mut hand = Vec::new();
        for i in 0..elves {
            hand.push(add_elf(&mut state, i as u64));
        }
        for j in 0..fillers {
            hand.push(add_filler(&mut state, j as u64));
        }
        (state, hand)
    }

    #[test]
    fn opts_out_when_commitment_low() {
        let features = tribal_features(0.2, "Elf");
        let (state, hand) = make_hand(3, 4);
        let score =
            TribalDensityMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "tribal_opener_na");
            }
            _ => panic!("expected opt-out Score"),
        }
    }

    #[test]
    fn dense_hand_scores_positive() {
        // 3 elves + 4 filler = density 3/7 ≈ 0.43 → dense
        let features = tribal_features(0.9, "Elf");
        let (state, hand) = make_hand(3, 4);
        let score =
            TribalDensityMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert_eq!(reason.kind, "tribal_opener_dense");
            }
            _ => panic!("expected dense Score"),
        }
    }

    #[test]
    fn thin_hand_scores_negative() {
        // 0 elves + 7 filler = density 0/7 = 0.0 < 0.15 → thin
        let features = tribal_features(0.9, "Elf");
        let (state, hand) = make_hand(0, 7);
        let score =
            TribalDensityMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta < 0.0, "expected negative delta, got {delta}");
                assert_eq!(reason.kind, "tribal_opener_thin");
            }
            _ => panic!("expected thin Score"),
        }
    }

    #[test]
    fn medium_hand_defers_to_baseline() {
        // 2 elves + 5 filler = density 2/7 ≈ 0.29 → defer
        let features = tribal_features(0.9, "Elf");
        let (state, hand) = make_hand(2, 5);
        let score =
            TribalDensityMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "tribal_opener_defer");
            }
            _ => panic!("expected defer Score"),
        }
    }
}
