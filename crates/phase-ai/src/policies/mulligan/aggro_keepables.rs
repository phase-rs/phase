//! `AggroKeepablesMulligan` — feature-driven mulligan policy for aggro decks.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding to keep after the
//! mulligan process. When a deck's aggro-pressure commitment is meaningful,
//! opening hands that combine early drops, lands, and evasion/burn are
//! strongly preferred.
//!
//! Opts out for decks where `features.aggro_pressure.commitment <= MULLIGAN_FLOOR` —
//! the baseline `KeepablesByLandCount` policy is the sole voice for those decks.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::aggro_pressure::{
    is_burn_spell_parts, is_evasion_creature_parts, is_low_curve_creature_parts, MULLIGAN_FLOOR,
};
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

pub struct AggroKeepablesMulligan;

impl MulliganPolicy for AggroKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::AggroKeepablesMulligan
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
        let commitment = features.aggro_pressure.commitment;
        if commitment <= MULLIGAN_FLOOR {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("aggro_opener_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        let mut lands: i64 = 0;
        let mut early_drops: i64 = 0;
        let mut evasion_or_burn: i64 = 0;

        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            let core_types = &obj.card_types.core_types;
            if core_types.contains(&CoreType::Land) {
                lands += 1;
                continue;
            }

            // Early drop: low-curve creature (MV ≤ 2). CR 302 + CR 202.3.
            if is_low_curve_creature_parts(core_types, &obj.mana_cost) {
                early_drops += 1;
            }

            // Evasion creature or burn spell — pressure and reach. CR 702.9+ / CR 120.3.
            if is_evasion_creature_parts(core_types, &obj.keywords, &obj.static_definitions)
                || is_burn_spell_parts(core_types, &obj.abilities)
            {
                evasion_or_burn += 1;
            }
        }

        // Slow: no early drops → can't apply pressure. (Tactical heuristic;
        // the engine's summoning-sickness check at CR 302.6 is downstream.)
        if early_drops == 0 {
            return MulliganScore::Score {
                delta: -1.2,
                reason: PolicyReason::new("aggro_opener_slow")
                    .with_fact("lands", lands)
                    .with_fact("early_drops", 0),
            };
        }

        // Flooded: too many lands kills aggro's threat density.
        if lands >= 5 {
            return MulliganScore::Score {
                delta: -0.8,
                reason: PolicyReason::new("aggro_opener_flooded").with_fact("lands", lands),
            };
        }

        // Ideal: ≥2 early drops + ≥2 lands + ≥1 evasion/burn.
        if early_drops >= 2 && lands >= 2 && evasion_or_burn >= 1 {
            return MulliganScore::Score {
                delta: 1.8,
                reason: PolicyReason::new("aggro_opener_ideal")
                    .with_fact("early_drops", early_drops)
                    .with_fact("lands", lands)
                    .with_fact("evasion_or_burn", evasion_or_burn),
            };
        }

        // Workable: ≥2 early drops + ≥2 lands (missing threat/reach).
        if early_drops >= 2 && lands >= 2 {
            return MulliganScore::Score {
                delta: 0.7,
                reason: PolicyReason::new("aggro_opener_workable")
                    .with_fact("early_drops", early_drops)
                    .with_fact("lands", lands),
            };
        }

        // Otherwise defer to the baseline.
        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("aggro_opener_defer")
                .with_fact("early_drops", early_drops)
                .with_fact("lands", lands),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::aggro_pressure::AggroPressureFeature;
    use crate::features::DeckFeatures;
    use crate::plan::PlanSnapshot;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);

    fn features_with_commitment(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            aggro_pressure: AggroPressureFeature {
                commitment,
                low_curve_creature_count: 16,
                hasty_creature_count: 8,
                evasion_creature_count: 8,
                burn_spell_count: 8,
                combat_pump_count: 4,
                total_nonland: 40,
                low_curve_density: 0.4,
            },
            ..DeckFeatures::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    enum Card {
        Land,
        EarlyDrop,         // MV 1 creature
        EarlyDropEvasion,  // MV 1 creature + flying
        Burn,              // Instant with DealDamage Any
        ExpensiveCreature, // MV 5 creature
    }

    fn add_card(state: &mut GameState, idx: u64, card: Card) -> ObjectId {
        let card_id = CardId(100 + idx);
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
            Card::EarlyDrop => {
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
            Card::EarlyDropEvasion => {
                let oid = create_object(state, card_id, AI, format!("Flyer {idx}"), Zone::Hand);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Creature],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(1);
                obj.keywords.push(Keyword::Flying);
                oid
            }
            Card::Burn => {
                let oid = create_object(state, card_id, AI, format!("Bolt {idx}"), Zone::Hand);
                let mut ability = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::Any,
                        damage_source: None,
                    },
                );
                ability.kind = AbilityKind::Spell;
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Instant],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(1);
                obj.abilities.push(ability);
                oid
            }
            Card::ExpensiveCreature => {
                let oid = create_object(state, card_id, AI, format!("Dragon {idx}"), Zone::Hand);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Creature],
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
    fn opts_out_when_commitment_low() {
        let features = features_with_commitment(0.4); // ≤ MULLIGAN_FLOOR 0.55
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::EarlyDrop,
            Card::EarlyDrop,
            Card::Burn,
            Card::EarlyDrop,
            Card::EarlyDrop,
        ]);
        let score = AggroKeepablesMulligan.evaluate(
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
                assert_eq!(reason.kind, "aggro_opener_na");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn ideal_hand_scores_positive() {
        let features = features_with_commitment(0.8);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::EarlyDrop,
            Card::EarlyDrop,
            Card::EarlyDropEvasion, // evasion counts for evasion_or_burn
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = AggroKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "ideal hand should score positive, got {delta}");
                assert_eq!(reason.kind, "aggro_opener_ideal");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn slow_hand_no_early_drops_scores_negative() {
        let features = features_with_commitment(0.8);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = AggroKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta < 0.0, "slow hand should score negative, got {delta}");
                assert_eq!(reason.kind, "aggro_opener_slow");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn flooded_hand_scores_negative() {
        let features = features_with_commitment(0.8);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::Land,
            Card::Land, // 5 lands
            Card::EarlyDrop,
            Card::EarlyDrop,
        ]);
        let score = AggroKeepablesMulligan.evaluate(
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
                    "flooded hand should score negative, got {delta}"
                );
                assert_eq!(reason.kind, "aggro_opener_flooded");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn medium_hand_defers_to_baseline() {
        // 2 early drops + 3 lands but no evasion/burn → workable (> 0)
        let features = features_with_commitment(0.8);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::EarlyDrop,
            Card::EarlyDrop,
            Card::ExpensiveCreature,
            Card::ExpensiveCreature,
        ]);
        let score = AggroKeepablesMulligan.evaluate(
            &hand,
            &state,
            &features,
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                // ≥2 early drops + ≥2 lands but no evasion/burn → workable
                assert!(
                    delta >= 0.0,
                    "medium hand should be non-negative, got {delta}"
                );
                assert!(
                    reason.kind == "aggro_opener_workable" || reason.kind == "aggro_opener_defer",
                    "unexpected reason: {}",
                    reason.kind
                );
            }
            _ => panic!("expected Score"),
        }
    }
}
