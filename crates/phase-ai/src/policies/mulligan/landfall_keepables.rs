//! `LandfallKeepablesMulligan` — feature-driven mulligan policy for landfall decks.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding to keep after the
//! mulligan process. When a deck's landfall commitment is meaningful, opening
//! hands that combine a landfall payoff with a workable land count are
//! strongly preferred over hands without a payoff.
//!
//! Opts out for decks where `features.landfall.commitment <= 0.3` — the
//! baseline `KeepablesByLandCount` policy is the sole voice for those decks.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

/// Commitment threshold below which this policy opts out. Matches the
/// plan-mandated 0.3 boundary — fewer than ~1 payoff in the deck does not
/// warrant landfall-specific mulligan preferences.
const COMMITMENT_THRESHOLD: f32 = 0.3;

pub struct LandfallKeepablesMulligan;

impl MulliganPolicy for LandfallKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::LandfallKeepablesMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        _plan: &PlanSnapshot, // input-unused: landfall opener scoring is card-composition only
        _turn_order: TurnOrder, // input-unused: landfall opener scoring is card-composition only
        _mulligans_taken: u8, // input-unused: landfall opener scoring is card-composition only
    ) -> MulliganScore {
        let commitment = features.landfall.commitment;
        if commitment <= COMMITMENT_THRESHOLD {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("landfall_keepables_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        // Classification already happened at session-build time — we consult
        // the deck's payoff name set as an identifier lookup to see whether
        // any payoff is in hand. This is the same pattern Phase B uses for
        // battlefield payoff detection, not card-name feature classification.
        let mut payoff_count: i64 = 0;
        let mut land_count: i64 = 0;
        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            if obj.card_types.core_types.contains(&CoreType::Land) {
                land_count += 1;
            }
            if features.landfall.payoff_names.contains(&obj.name) {
                payoff_count += 1;
            }
        }

        if payoff_count >= 1 && land_count >= 3 {
            return MulliganScore::Score {
                delta: 2.0,
                reason: PolicyReason::new("landfall_keepable_ideal")
                    .with_fact("payoff_count", payoff_count)
                    .with_fact("land_count", land_count)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        if payoff_count >= 1 {
            return MulliganScore::Score {
                delta: 0.5,
                reason: PolicyReason::new("landfall_has_payoff_light_lands")
                    .with_fact("payoff_count", payoff_count)
                    .with_fact("land_count", land_count),
            };
        }

        if land_count >= 3 {
            return MulliganScore::Score {
                delta: -1.0,
                reason: PolicyReason::new("landfall_no_payoffs")
                    .with_fact("land_count", land_count)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        // No payoffs + few lands — defer to Keepables policy.
        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("landfall_defer_to_baseline")
                .with_fact("payoff_count", payoff_count)
                .with_fact("land_count", land_count),
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
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use crate::features::{DeckFeatures, LandfallFeature};

    fn features_with_commitment(commitment: f32, payoff_names: Vec<&'static str>) -> DeckFeatures {
        DeckFeatures {
            landfall: LandfallFeature {
                payoff_count: payoff_names.len() as u32,
                enabler_count: 0,
                commitment,
                payoff_names: payoff_names.into_iter().map(String::from).collect(),
            },
            ..DeckFeatures::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    fn add_card(
        state: &mut GameState,
        idx: u64,
        name: &str,
        core_types: Vec<CoreType>,
        subtypes: Vec<&str>,
    ) -> ObjectId {
        let oid = create_object(
            state,
            CardId(2000 + idx),
            PlayerId(0),
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).expect("just created");
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types,
            subtypes: subtypes.into_iter().map(String::from).collect(),
        };
        obj.mana_cost = ManaCost::NoCost;
        oid
    }

    fn make_hand(lands: usize, payoff_names: &[&str], filler: usize) -> (GameState, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();
        let mut hand = Vec::new();
        for i in 0..lands {
            hand.push(add_card(
                &mut state,
                i as u64,
                &format!("Mountain {i}"),
                vec![CoreType::Land],
                vec!["Mountain"],
            ));
        }
        for (j, name) in payoff_names.iter().enumerate() {
            hand.push(add_card(
                &mut state,
                (100 + j) as u64,
                name,
                vec![CoreType::Creature],
                Vec::new(),
            ));
        }
        for k in 0..filler {
            hand.push(add_card(
                &mut state,
                (200 + k) as u64,
                &format!("Filler {k}"),
                vec![CoreType::Creature],
                Vec::new(),
            ));
        }
        (state, hand)
    }

    #[test]
    fn opts_out_when_commitment_low() {
        let features = features_with_commitment(0.1, vec!["Omnath"]);
        let (state, hand) = make_hand(3, &[], 4);
        let score = LandfallKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "landfall_keepables_na");
            }
            _ => panic!("expected opt-out Score"),
        }
    }

    #[test]
    fn ideal_hand_high_commitment() {
        let features = features_with_commitment(0.9, vec!["Omnath"]);
        // 3 lands + 1 payoff + 3 filler = 7 cards
        let (state, hand) = make_hand(3, &["Omnath"], 3);
        let score = LandfallKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0);
                assert_eq!(reason.kind, "landfall_keepable_ideal");
            }
            _ => panic!("expected ideal Score"),
        }
    }

    #[test]
    fn payoff_light_lands() {
        let features = features_with_commitment(0.9, vec!["Omnath"]);
        // 1 land + 1 payoff + 5 filler.
        let (state, hand) = make_hand(1, &["Omnath"], 5);
        let score = LandfallKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0);
                assert_eq!(reason.kind, "landfall_has_payoff_light_lands");
            }
            _ => panic!("expected light-lands Score"),
        }
    }

    #[test]
    fn lands_no_payoffs_penalty() {
        let features = features_with_commitment(0.9, vec!["Omnath"]);
        let (state, hand) = make_hand(3, &[], 4);
        let score = LandfallKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta < 0.0);
                assert_eq!(reason.kind, "landfall_no_payoffs");
            }
            _ => panic!("expected negative Score"),
        }
    }

    #[test]
    fn default_defers_to_baseline() {
        let features = features_with_commitment(0.9, vec!["Omnath"]);
        // 0 lands, 0 payoffs, 7 filler.
        let (state, hand) = make_hand(0, &[], 7);
        let score = LandfallKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "landfall_defer_to_baseline");
            }
            _ => panic!("expected defer Score"),
        }
    }
}
