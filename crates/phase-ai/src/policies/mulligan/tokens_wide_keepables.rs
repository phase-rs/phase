//! `TokensWideKeepablesMulligan` — feature-driven mulligan policy for tokens-wide decks.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): the mulligan process — deciding to
//! keep based on hand composition. When a deck's tokens-wide commitment is
//! meaningful, opening hands with token generators, lands, and anthem effects
//! are preferred.
//!
//! Opts out for decks where `features.tokens_wide.commitment <= MULLIGAN_FLOOR`.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::tokens_wide::MULLIGAN_FLOOR;
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

pub struct TokensWideKeepablesMulligan;

impl MulliganPolicy for TokensWideKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::TokensWideMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        _plan: &PlanSnapshot, // input-unused: tokens opener scoring is card-composition only
        _turn_order: TurnOrder, // input-unused: tokens opener scoring is card-composition only
        _mulligans_taken: u8, // input-unused: tokens opener scoring is card-composition only
    ) -> MulliganScore {
        let commitment = features.tokens_wide.commitment;
        if commitment <= MULLIGAN_FLOOR {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("tokens_wide_opener_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        let mut lands: i64 = 0;
        let mut payoff_count: i64 = 0;
        let mut anthem_count: i64 = 0;
        let mut cheap_creature_count: i64 = 0;

        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            let core_types = &obj.card_types.core_types;

            if core_types.contains(&CoreType::Land) {
                lands += 1;
                continue;
            }

            // Payoff: identity lookup against token-generator names. CR 111.1.
            if features
                .tokens_wide
                .payoff_names
                .iter()
                .any(|n| n == &obj.name)
            {
                payoff_count += 1;
            }

            // Anthem/pump: identity lookup against anthem names. CR 613.4c.
            if features
                .tokens_wide
                .anthem_names
                .iter()
                .any(|n| n == &obj.name)
            {
                anthem_count += 1;
            }

            // Cheap creature (mana value ≤ 2) — provides immediate board presence.
            // CR 202.3: mana value. CR 302: creature type.
            if core_types.contains(&CoreType::Creature) && obj.mana_cost.mana_value() <= 2 {
                cheap_creature_count += 1;
            }
        }

        // Ideal: token generator + ≥2 lands + (cheap creature or anthem). CR 103.5.
        if payoff_count >= 1 && lands >= 2 && (cheap_creature_count >= 1 || anthem_count >= 1) {
            return MulliganScore::Score {
                delta: 2.0,
                reason: PolicyReason::new("tokens_wide_keepable_ideal")
                    .with_fact("payoff", payoff_count)
                    .with_fact("lands", lands)
                    .with_fact("cheap_creature", cheap_creature_count)
                    .with_fact("anthem", anthem_count),
            };
        }

        // Workable: generator + ≥2 lands (missing early threat or anthem).
        if payoff_count >= 1 && lands >= 2 {
            return MulliganScore::Score {
                delta: 0.5,
                reason: PolicyReason::new("tokens_wide_has_payoff")
                    .with_fact("payoff", payoff_count)
                    .with_fact("lands", lands),
            };
        }

        // No generator in a committed deck → prefer to find one. CR 103.5.
        // (`commitment > MULLIGAN_FLOOR` is implied by reaching this point —
        // the early opt-out at top-of-fn already filtered `<= MULLIGAN_FLOOR`.)
        if payoff_count == 0 {
            return MulliganScore::Score {
                delta: -1.0,
                reason: PolicyReason::new("tokens_wide_no_payoff")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        // Defer to baseline.
        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("tokens_wide_defer_to_baseline")
                .with_fact("payoff", payoff_count)
                .with_fact("lands", lands),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::features::tokens_wide::TokensWideFeature;
    use crate::features::DeckFeatures;
    use crate::plan::PlanSnapshot;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);

    fn features_with_commitment(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            tokens_wide: TokensWideFeature {
                commitment,
                token_generator_count: 8,
                mass_token_generator_count: 4,
                anthem_count: 4,
                mass_pump_count: 2,
                wide_payoff_count: 4,
                payoff_names: vec!["Token Factory".to_string()],
                anthem_names: vec!["Glorious Anthem".to_string()],
            },
            ..DeckFeatures::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    enum Card {
        Land,
        TokenFactory,   // named payoff
        GloriousAnthem, // named anthem
        CheapCreature,  // MV ≤ 2 creature
        ExpensiveSpell, // MV 5 non-creature
    }

    fn add_card(state: &mut GameState, idx: u64, card: Card) -> ObjectId {
        let card_id = CardId(200 + idx);
        match card {
            Card::Land => {
                let oid = create_object(state, card_id, AI, format!("Land {idx}"), Zone::Hand);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Land],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::NoCost;
                oid
            }
            Card::TokenFactory => {
                let oid =
                    create_object(state, card_id, AI, "Token Factory".to_string(), Zone::Hand);
                let token_effect = Effect::Token {
                    name: "Saproling".to_string(),
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(1),
                    types: vec!["Creature".to_string()],
                    colors: Vec::new(),
                    keywords: Vec::new(),
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    supertypes: Vec::new(),
                    static_abilities: Vec::new(),
                    enter_with_counters: Vec::new(),
                };
                let ability = AbilityDefinition::new(AbilityKind::Spell, token_effect);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Sorcery],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(3);
                Arc::make_mut(&mut obj.abilities).push(ability);
                oid
            }
            Card::GloriousAnthem => {
                let oid = create_object(
                    state,
                    card_id,
                    AI,
                    "Glorious Anthem".to_string(),
                    Zone::Hand,
                );
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Enchantment],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(3);
                oid
            }
            Card::CheapCreature => {
                let oid = create_object(state, card_id, AI, format!("Goblin {idx}"), Zone::Hand);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Creature],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(1);
                oid
            }
            Card::ExpensiveSpell => {
                let oid = create_object(state, card_id, AI, format!("Dragon {idx}"), Zone::Hand);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Sorcery],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(5);
                oid
            }
        }
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

    // ── Tests ──────────────────────────────────────────────────────────────

    #[test]
    fn opts_out_below_mulligan_floor() {
        let features = features_with_commitment(0.35); // ≤ MULLIGAN_FLOOR (0.40)
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::TokenFactory,
            Card::GloriousAnthem,
            Card::CheapCreature,
            Card::CheapCreature,
            Card::CheapCreature,
        ]);
        let score = TokensWideKeepablesMulligan.evaluate(
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
                assert_eq!(reason.kind, "tokens_wide_opener_na");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn ideal_hand_payoff_lands_anthem() {
        // Token Factory + 2 lands + anthem = ideal. CR 103.5.
        let features = features_with_commitment(0.7);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::TokenFactory,
            Card::GloriousAnthem,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
        ]);
        let score = TokensWideKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "ideal hand should be positive, got {delta}");
                assert_eq!(reason.kind, "tokens_wide_keepable_ideal");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn no_payoff_penalty_when_committed() {
        // No token generators with commitment above floor → penalty. CR 103.5.
        let features = features_with_commitment(0.7);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::GloriousAnthem,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
        ]);
        let score = TokensWideKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(
                    delta < 0.0,
                    "no-generator hand should penalize, got {delta}"
                );
                assert_eq!(reason.kind, "tokens_wide_no_payoff");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn ideal_hand_payoff_lands_cheap_creature() {
        // Token Factory + 2 lands + cheap creature = ideal (alternate path). CR 103.5.
        let features = features_with_commitment(0.7);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::TokenFactory,
            Card::CheapCreature,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
        ]);
        let score = TokensWideKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(
                    delta > 0.0,
                    "payoff+lands+cheap creature should be positive, got {delta}"
                );
                assert_eq!(reason.kind, "tokens_wide_keepable_ideal");
            }
            _ => panic!("expected Score"),
        }
    }
}
