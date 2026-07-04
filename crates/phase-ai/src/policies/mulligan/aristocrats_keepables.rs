//! `AristocratsKeepablesMulligan` — feature-driven mulligan policy for
//! aristocrats decks.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding to keep after the
//! mulligan process. When a deck's aristocrats commitment is meaningful,
//! opening hands combining a sacrifice outlet with cheap creatures and lands
//! are strongly preferred.
//!
//! Opts out for decks where `features.aristocrats.commitment <= 0.3` — the
//! baseline `KeepablesByLandCount` policy is the sole voice for those decks.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

/// Commitment threshold below which this policy opts out.
const COMMITMENT_THRESHOLD: f32 = 0.3;
/// Cheap creature threshold — mana value ≤ 2 qualifies as early fodder.
const CHEAP_CREATURE_MV: u32 = 2;

pub struct AristocratsKeepablesMulligan;

impl MulliganPolicy for AristocratsKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::AristocratsKeepablesMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        _plan: &PlanSnapshot, // input-unused: aristocrats opener scoring is card-composition only
        _turn_order: TurnOrder, // input-unused: aristocrats opener scoring is card-composition only
        _mulligans_taken: u8, // input-unused: aristocrats opener scoring is card-composition only
    ) -> MulliganScore {
        let commitment = features.aristocrats.commitment;
        if commitment <= COMMITMENT_THRESHOLD {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("aristocrats_keepables_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        let mut land_count: i64 = 0;
        let mut outlet_count: i64 = 0;
        let mut cheap_creature_count: i64 = 0;

        // Classify each hand card using identity lookup for outlets (structural
        // classification already happened in `aristocrats::detect`) and
        // mana-value for cheap creature fodder.
        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            if obj.card_types.core_types.contains(&CoreType::Land) {
                land_count += 1;
                continue;
            }
            // Identity lookup — outlet_names carries only structurally-verified
            // outlet names from deck-build time. CR 701.21.
            if features
                .aristocrats
                .outlet_names
                .iter()
                .any(|name| name == &obj.name)
            {
                outlet_count += 1;
            }
            // Cheap creature (≤ 2 mana value) that isn't a land — good fodder.
            // CR 202.3: mana value of 0 for objects with no mana cost.
            if obj.card_types.core_types.contains(&CoreType::Creature)
                && obj.mana_cost.mana_value() <= CHEAP_CREATURE_MV
            {
                cheap_creature_count += 1;
            }
        }

        // Ideal: outlet + ≥1 cheap creature + ≥2 lands.
        if outlet_count >= 1 && cheap_creature_count >= 1 && land_count >= 2 {
            return MulliganScore::Score {
                delta: 2.0,
                reason: PolicyReason::new("aristocrats_keepable_ideal")
                    .with_fact("outlet_count", outlet_count)
                    .with_fact("cheap_creature_count", cheap_creature_count)
                    .with_fact("land_count", land_count),
            };
        }

        // Outlet with some support — light but acceptable.
        if outlet_count >= 1 {
            return MulliganScore::Score {
                delta: 0.5,
                reason: PolicyReason::new("aristocrats_has_outlet_light_support")
                    .with_fact("outlet_count", outlet_count)
                    .with_fact("land_count", land_count),
            };
        }

        // No outlet in hand for a committed aristocrats deck — bad.
        if commitment > COMMITMENT_THRESHOLD {
            return MulliganScore::Score {
                delta: -1.0,
                reason: PolicyReason::new("aristocrats_no_outlet")
                    .with_fact("land_count", land_count)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("aristocrats_defer_to_baseline")
                .with_fact("outlet_count", outlet_count)
                .with_fact("land_count", land_count),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr,
        SacrificeCost, TargetFilter, TypedFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use crate::features::aristocrats::AristocratsFeature;
    use crate::features::DeckFeatures;
    use crate::plan::PlanSnapshot;

    const AI: PlayerId = PlayerId(0);

    fn features_with_commitment(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            aristocrats: AristocratsFeature {
                outlet_count: 4,
                free_outlet_count: 4,
                death_trigger_count: 4,
                fodder_source_count: 4,
                commitment,
                outlet_names: vec!["Goblin Bombardment".to_string()],
                death_trigger_names: vec!["Zulaport Cutthroat".to_string()],
            },
            ..DeckFeatures::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    fn sac_outlet_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        );
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            1,
        )));
        ability
    }

    enum Card {
        Land,
        OutletCreature,
        CheapCreature,
        ExpensiveCreature,
    }

    fn add_card(state: &mut GameState, idx: u64, card: Card) -> ObjectId {
        let (name, core_types, mana_value, ability) = match card {
            Card::Land => ("Forest".to_string(), vec![CoreType::Land], 0, None),
            Card::OutletCreature => (
                "Goblin Bombardment".to_string(),
                vec![CoreType::Creature],
                2,
                Some(sac_outlet_ability()),
            ),
            Card::CheapCreature => (
                format!("Llanowar Elves {idx}"),
                vec![CoreType::Creature],
                1,
                None,
            ),
            Card::ExpensiveCreature => (format!("Dragon {idx}"), vec![CoreType::Creature], 7, None),
        };
        let oid = create_object(state, CardId(3000 + idx), AI, name, Zone::Hand);
        let obj = state.objects.get_mut(&oid).expect("just created");
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types,
            subtypes: Vec::new(),
        };
        obj.mana_cost = if mana_value == 0 {
            ManaCost::NoCost
        } else {
            ManaCost::generic(mana_value)
        };
        if let Some(a) = ability {
            Arc::make_mut(&mut obj.abilities).push(a);
        }
        oid
    }

    fn make_hand(cards: Vec<Card>) -> (GameState, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();
        let mut hand = Vec::new();
        for (i, c) in cards.into_iter().enumerate() {
            hand.push(add_card(&mut state, i as u64, c));
        }
        (state, hand)
    }

    #[test]
    fn opts_out_when_commitment_low() {
        let features = features_with_commitment(0.1);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::OutletCreature,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::CheapCreature,
        ]);
        let score = AristocratsKeepablesMulligan.evaluate(
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
                assert_eq!(reason.kind, "aristocrats_keepables_na");
            }
            _ => panic!("expected opt-out Score"),
        }
    }

    #[test]
    fn ideal_hand_outlet_cheap_creature_two_lands() {
        let features = features_with_commitment(0.9);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::OutletCreature,
            Card::CheapCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = AristocratsKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert_eq!(reason.kind, "aristocrats_keepable_ideal");
            }
            _ => panic!("expected ideal Score"),
        }
    }

    #[test]
    fn outlet_only_hand_light_support() {
        // Outlet but no cheap creatures or insufficient lands.
        let features = features_with_commitment(0.9);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::OutletCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = AristocratsKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert_eq!(reason.kind, "aristocrats_has_outlet_light_support");
            }
            _ => panic!("expected light-support Score"),
        }
    }

    #[test]
    fn no_outlet_penalty_for_committed_deck() {
        // No outlet in hand when commitment > 0.3 → penalty.
        let features = features_with_commitment(0.9);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = AristocratsKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta < 0.0, "expected negative delta, got {delta}");
                assert_eq!(reason.kind, "aristocrats_no_outlet");
            }
            _ => panic!("expected penalty Score"),
        }
    }

    #[test]
    fn defer_to_baseline_when_commitment_exactly_at_threshold() {
        // commitment = 0.30 is ≤ COMMITMENT_THRESHOLD → opt out.
        let features = features_with_commitment(0.30);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::CheapCreature,
        ]);
        let score = AristocratsKeepablesMulligan.evaluate(
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
                assert_eq!(reason.kind, "aristocrats_keepables_na");
            }
            _ => panic!("expected defer Score"),
        }
    }
}
