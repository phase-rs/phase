//! `PlusOneCountersMulligan` — feature-driven mulligan policy for +1/+1
//! counter decks.
//!
//! CR 103.5: deciding to keep after the mulligan process. When a deck's +1/+1
//! counter commitment is meaningful, opening hands with a counter generator,
//! cheap creatures to target, and lands are strongly preferred.
//!
//! Opts out for decks where `features.plus_one_counters.commitment <= MULLIGAN_FLOOR`
//! — the baseline `KeepablesByLandCount` policy is the sole voice for those decks.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::plus_one_counters::{ability_places_plus_one_counter, MULLIGAN_FLOOR};
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

/// Cheap creature threshold — mana value ≤ 3 qualifies as an early counter target.
const CHEAP_CREATURE_MV: u32 = 3;

pub struct PlusOneCountersMulligan;

impl MulliganPolicy for PlusOneCountersMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::PlusOneCountersMulligan
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
        let commitment = features.plus_one_counters.commitment;
        if commitment <= MULLIGAN_FLOOR {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("p1p1_keepables_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        let mut land_count: i64 = 0;
        let mut cheap_creature_count: i64 = 0;
        let mut generator_in_hand: i64 = 0;
        let mut payoff_in_hand: i64 = 0;

        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };

            if obj.card_types.core_types.contains(&CoreType::Land) {
                land_count += 1;
                continue;
            }

            // Identity lookup for payoffs — structural classification already
            // happened at deck-build time in `plus_one_counters::detect`.
            if features
                .plus_one_counters
                .payoff_names
                .iter()
                .any(|name| name == &obj.name)
            {
                payoff_in_hand += 1;
            }

            // Structural walk for generators — re-classify the live ability.
            if obj.abilities.iter().any(ability_places_plus_one_counter) {
                generator_in_hand += 1;
            }

            // Cheap creature (≤ 3 mana value) that can receive counters.
            // CR 202.3: mana value of 0 for objects with no mana cost.
            if obj.card_types.core_types.contains(&CoreType::Creature)
                && obj.mana_cost.mana_value() <= CHEAP_CREATURE_MV
            {
                cheap_creature_count += 1;
            }
        }

        // Ideal: generator + cheap creature target + ≥2 lands.
        if generator_in_hand >= 1 && cheap_creature_count >= 1 && land_count >= 2 {
            return MulliganScore::Score {
                delta: 2.0,
                reason: PolicyReason::new("p1p1_keepable_ideal")
                    .with_fact("generator_in_hand", generator_in_hand)
                    .with_fact("cheap_creature_count", cheap_creature_count)
                    .with_fact("land_count", land_count),
            };
        }

        // Generator with lands — light but viable.
        if generator_in_hand >= 1 && land_count >= 2 {
            return MulliganScore::Score {
                delta: 0.8,
                reason: PolicyReason::new("p1p1_keepable_generator_lands")
                    .with_fact("generator_in_hand", generator_in_hand)
                    .with_fact("land_count", land_count),
            };
        }

        // No generator and no payoff — committed deck is stuck without either.
        if generator_in_hand == 0 && payoff_in_hand == 0 {
            return MulliganScore::Score {
                delta: -1.2,
                reason: PolicyReason::new("p1p1_no_sources_no_payoffs")
                    .with_fact("land_count", land_count)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("p1p1_defer_to_baseline")
                .with_fact("generator_in_hand", generator_in_hand)
                .with_fact("land_count", land_count),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use crate::features::plus_one_counters::PlusOneCountersFeature;
    use crate::features::DeckFeatures;
    use crate::plan::PlanSnapshot;

    const AI: PlayerId = PlayerId(0);

    fn features_with_commitment(commitment: f32, payoff_names: Vec<String>) -> DeckFeatures {
        DeckFeatures {
            plus_one_counters: PlusOneCountersFeature {
                generator_count: 4,
                proliferate_count: 1,
                doubler_count: 0,
                payoff_count: payoff_names.len() as u32,
                etb_with_counters_count: 2,
                commitment,
                payoff_names,
            },
            ..DeckFeatures::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    fn generator_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::AddCounter {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
            },
        )
    }

    enum Card {
        Land,
        GeneratorCreature,
        CheapCreature,
        ExpensiveCreature,
    }

    fn add_card(state: &mut GameState, idx: u64, card: Card) -> ObjectId {
        let (name, core_types, mana_value, ability) = match card {
            Card::Land => ("Forest".to_string(), vec![CoreType::Land], 0, None),
            Card::GeneratorCreature => (
                "Hardened Scales".to_string(),
                vec![CoreType::Enchantment],
                1,
                Some(generator_ability()),
            ),
            Card::CheapCreature => (format!("Servant {idx}"), vec![CoreType::Creature], 1, None),
            Card::ExpensiveCreature => (format!("Titan {idx}"), vec![CoreType::Creature], 6, None),
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
            obj.abilities.push(a);
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

    // ─── opts_out tests ───────────────────────────────────────────────────────

    #[test]
    fn opts_out_when_commitment_low() {
        let features = features_with_commitment(0.1, vec![]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::GeneratorCreature,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = PlusOneCountersMulligan.evaluate(
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
                assert_eq!(reason.kind, "p1p1_keepables_na");
            }
            _ => panic!("expected opt-out Score"),
        }
    }

    // ─── ideal hand ───────────────────────────────────────────────────────────

    #[test]
    fn ideal_hand_generator_creature_two_lands() {
        let features = features_with_commitment(0.9, vec!["Armorcraft Judge".to_string()]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::GeneratorCreature,
            Card::CheapCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = PlusOneCountersMulligan.evaluate(
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
                assert_eq!(reason.kind, "p1p1_keepable_ideal");
            }
            _ => panic!("expected ideal Score"),
        }
    }

    // ─── generator + lands ────────────────────────────────────────────────────

    #[test]
    fn generator_only_with_lands_acceptable() {
        let features = features_with_commitment(0.9, vec![]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::GeneratorCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = PlusOneCountersMulligan.evaluate(
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
                assert_eq!(reason.kind, "p1p1_keepable_generator_lands");
            }
            _ => panic!("expected generator-lands Score"),
        }
    }

    // ─── no generator no payoff ───────────────────────────────────────────────

    #[test]
    fn no_generator_no_payoff_committed_deck_penalized() {
        let features = features_with_commitment(0.9, vec!["Armorcraft Judge".to_string()]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = PlusOneCountersMulligan.evaluate(
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
                assert_eq!(reason.kind, "p1p1_no_sources_no_payoffs");
            }
            _ => panic!("expected penalty Score"),
        }
    }

    // ─── defer to baseline ────────────────────────────────────────────────────

    #[test]
    fn defer_to_baseline_when_commitment_at_threshold() {
        // commitment = 0.30 is exactly at MULLIGAN_FLOOR → opts out.
        let features = features_with_commitment(0.30, vec![]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::CheapCreature,
        ]);
        let score = PlusOneCountersMulligan.evaluate(
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
                assert_eq!(reason.kind, "p1p1_keepables_na");
            }
            _ => panic!("expected opt-out Score"),
        }
    }
}
