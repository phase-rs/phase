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
#[cfg(test)]
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::plus_one_counters::{
    ability_creates_token_with_plus_one_counters, ability_places_plus_one_counter, MULLIGAN_FLOOR,
};
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{is_land_source, MulliganPolicy, MulliganScore, TurnOrder};

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
        _plan: &PlanSnapshot, // input-unused: counters opener scoring is card-composition only
        _turn_order: TurnOrder, // input-unused: counters opener scoring is card-composition only
        _mulligans_taken: u8, // input-unused: counters opener scoring is card-composition only
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
        let mut token_with_counters_in_hand: i64 = 0;
        let mut payoff_in_hand: i64 = 0;

        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };

            if is_land_source(obj) {
                land_count += 1;
            }
            if obj.card_types.core_types.contains(&CoreType::Land) {
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
            if obj
                .abilities
                .iter()
                .any(ability_creates_token_with_plus_one_counters)
            {
                token_with_counters_in_hand += 1;
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
        // CR 122.6 + CR 122.6a: a card that creates a token with +1/+1 counters,
        // in either representation, is a counter source, so a hand holding one
        // is not "no sources"; it is not a generator onto existing creatures,
        // so it earns neither generator bonus above.
        if generator_in_hand == 0 && token_with_counters_in_hand == 0 && payoff_in_hand == 0 {
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
    use std::sync::Arc;

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
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
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

    // ─── token with counters (CR 122.6 + CR 122.6a) ───────────────────────────

    use crate::policies::tests::created_token_placement_parity::{
        declarative_token_with_counters, imperative_token_then_placement, x_quantity,
    };
    use engine::types::ability::SubAbilityLink;

    fn hand_with_spell(ability: Option<AbilityDefinition>) -> (GameState, Vec<ObjectId>) {
        let (mut state, mut hand) = make_hand(vec![Card::Land, Card::Land]);
        if let Some(ability) = ability {
            let oid = create_object(
                &mut state,
                CardId(3900),
                AI,
                "Fractal Summoning".to_string(),
                Zone::Hand,
            );
            let obj = state.objects.get_mut(&oid).expect("just created");
            obj.card_types = CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Sorcery],
                subtypes: Vec::new(),
            };
            obj.mana_cost = ManaCost::generic(2);
            Arc::make_mut(&mut obj.abilities).push(ability);
            hand.push(oid);
        }
        (state, hand)
    }

    fn evaluate(hand: &[ObjectId], state: &GameState) -> (f64, &'static str) {
        let features = features_with_commitment(0.9, vec![]);
        match PlusOneCountersMulligan.evaluate(
            hand,
            state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        ) {
            MulliganScore::Score { delta, reason } => (delta, reason.kind),
            _ => panic!("expected Score"),
        }
    }

    /// A hand whose only +1/+1 card makes a token with +1/+1 counters holds a
    /// counter source, so it is not "no sources"; the counters go on the new
    /// token rather than a creature already on the battlefield, so neither
    /// generator bonus applies. Same verdict in both representations.
    #[test]
    fn token_with_counters_hand_defers_to_baseline_in_both_representations() {
        let shapes = [
            declarative_token_with_counters(
                AbilityKind::Spell,
                (0, 0),
                CounterType::Plus1Plus1,
                x_quantity(),
            ),
            imperative_token_then_placement(
                AbilityKind::Spell,
                SubAbilityLink::ContinuationStep,
                (0, 0),
                CounterType::Plus1Plus1,
                x_quantity(),
            ),
            imperative_token_then_placement(
                AbilityKind::Spell,
                SubAbilityLink::SequentialSibling,
                (1, 1),
                CounterType::Plus1Plus1,
                QuantityExpr::Fixed { value: 1 },
            ),
        ];
        for ability in shapes {
            let (state, hand) = hand_with_spell(Some(ability));
            assert_eq!(evaluate(&hand, &state), (0.0, "p1p1_defer_to_baseline"));
        }
    }

    /// Reach guards on the same two-land hand: no +1/+1 card at all is still
    /// "no sources", and a real generator still earns the generator bonus.
    #[test]
    fn no_source_and_generator_hands_keep_their_verdicts() {
        let (state, hand) = hand_with_spell(None);
        assert_eq!(
            evaluate(&hand, &state),
            (-1.2, "p1p1_no_sources_no_payoffs")
        );

        let generator = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(engine::types::ability::TypedFilter::creature()),
            },
        );
        let (state, hand) = hand_with_spell(Some(generator));
        assert_eq!(
            evaluate(&hand, &state),
            (0.8, "p1p1_keepable_generator_lands")
        );
    }
}
