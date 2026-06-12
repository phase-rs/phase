//! `SpellslingerKeepablesMulligan` — feature-driven mulligan policy for
//! spellslinger / prowess decks.
//!
//! CR 103.5: the mulligan process — each player may take a mulligan. This
//! policy opts out when the deck's spellslinger commitment is below
//! `MULLIGAN_FLOOR`, leaving baseline hand evaluation to `KeepablesByLandCount`.
//!
//! A keepable spellslinger hand needs:
//! - Enough mana to cast cheap spells (lands ≥ 2).
//! - Spell density to trigger prowess and chain cantrips (cheap_spells ≥ 3).
//! - OR a payoff creature that the spell density supports (payoff ≥ 1).
//!
//! Verdicts (first match wins, priority order):
//! 1. cheap_spell ≥ 3 ∧ payoff ≥ 1 ∧ lands ≥ 2 → +2.0 `spellslinger_keepable_ideal`
//! 2. cheap_spell ≥ 4 ∧ lands ≥ 2 → +1.0 `spellslinger_density_keepable`
//! 3. payoff ≥ 1 ∧ lands ≥ 2 → +0.5 `spellslinger_payoff_with_lands`
//! 4. cheap_spell == 0 ∧ payoff == 0 → -1.0 `spellslinger_no_density`
//! 5. else → 0.0 `spellslinger_defer_to_baseline`

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::spellslinger_prowess::{is_low_curve_spell_parts, MULLIGAN_FLOOR};
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

pub struct SpellslingerKeepablesMulligan;

impl MulliganPolicy for SpellslingerKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::SpellslingerKeepablesMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        _plan: &PlanSnapshot, // input-unused: spellslinger opener scoring is card-composition only
        _turn_order: TurnOrder, // input-unused: spellslinger opener scoring is card-composition only
        _mulligans_taken: u8, // input-unused: spellslinger opener scoring is card-composition only
    ) -> MulliganScore {
        let commitment = features.spellslinger_prowess.commitment;

        // Opt out when deck is not spellslinger-committed. CR 103.5: hand quality
        // evaluation should be archetype-aware. For low-commitment decks the
        // baseline KeepablesByLandCount policy is the sole voice.
        if commitment <= MULLIGAN_FLOOR {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("spellslinger_keepables_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        let mut lands: i64 = 0;
        let mut cheap_spell_count: i64 = 0;
        let mut payoff_count: i64 = 0;

        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            let core_types = &obj.card_types.core_types;

            if core_types.contains(&CoreType::Land) {
                lands += 1;
                continue;
            }

            // Low-curve instant or sorcery. CR 202.3b + CR 304.1 + CR 307.1.
            if is_low_curve_spell_parts(core_types, &obj.mana_cost) {
                cheap_spell_count += 1;
            }

            // Payoff: card name appears in the deck's payoff_names list (prowess
            // + cast-payoff creatures). Identity lookup — not structural. CR 702.108a.
            if features
                .spellslinger_prowess
                .payoff_names
                .iter()
                .any(|n| n == &obj.name)
            {
                payoff_count += 1;
            }
        }

        // Priority-ordered verdict matching — first match wins.

        // Ideal: density + payoff + mana base. CR 702.108a + CR 601.2i.
        if cheap_spell_count >= 3 && payoff_count >= 1 && lands >= 2 {
            return MulliganScore::Score {
                delta: 2.0,
                reason: PolicyReason::new("spellslinger_keepable_ideal")
                    .with_fact("cheap_spells", cheap_spell_count)
                    .with_fact("payoffs", payoff_count)
                    .with_fact("lands", lands),
            };
        }

        // Density-only: Burn-shape hand — high spell count but no named payoff.
        if cheap_spell_count >= 4 && lands >= 2 {
            return MulliganScore::Score {
                delta: 1.0,
                reason: PolicyReason::new("spellslinger_density_keepable")
                    .with_fact("cheap_spells", cheap_spell_count)
                    .with_fact("lands", lands),
            };
        }

        // Payoff-only: one payoff creature + lands — workable but needs draws.
        if payoff_count >= 1 && lands >= 2 {
            return MulliganScore::Score {
                delta: 0.5,
                reason: PolicyReason::new("spellslinger_payoff_with_lands")
                    .with_fact("payoffs", payoff_count)
                    .with_fact("lands", lands),
            };
        }

        // No density: zero cheap spells AND zero payoffs — unkeepable for
        // a spellslinger deck. CR 702.108a: prowess needs spells to trigger.
        if cheap_spell_count == 0 && payoff_count == 0 {
            return MulliganScore::Score {
                delta: -1.0,
                reason: PolicyReason::new("spellslinger_no_density")
                    .with_fact("cheap_spells", 0)
                    .with_fact("payoffs", 0),
            };
        }

        // Defer to baseline for hands that don't fit any pattern above.
        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("spellslinger_defer_to_baseline")
                .with_fact("cheap_spells", cheap_spell_count)
                .with_fact("payoffs", payoff_count)
                .with_fact("lands", lands),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::spellslinger_prowess::SpellslingerProwessFeature;
    use crate::features::DeckFeatures;
    use crate::plan::PlanSnapshot;
    use engine::game::zones::create_object;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);

    fn features_with(commitment: f32, payoff_names: Vec<String>) -> DeckFeatures {
        DeckFeatures {
            spellslinger_prowess: SpellslingerProwessFeature {
                commitment,
                payoff_names,
                ..Default::default()
            },
            ..DeckFeatures::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    enum Card {
        Land,
        CheapSpell,     // MV 1 instant
        Payoff(String), // named payoff creature
        ExpensiveSpell, // MV 5 sorcery
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
            Card::CheapSpell => {
                let oid = create_object(state, card_id, AI, format!("Bolt {idx}"), Zone::Hand);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Instant],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(1);
                oid
            }
            Card::Payoff(name) => {
                let oid = create_object(state, card_id, AI, name.clone(), Zone::Hand);
                let obj = state.objects.get_mut(&oid).unwrap();
                obj.card_types = CardType {
                    supertypes: Vec::new(),
                    core_types: vec![CoreType::Creature],
                    subtypes: Vec::new(),
                };
                obj.mana_cost = ManaCost::generic(2);
                oid
            }
            Card::ExpensiveSpell => {
                let oid = create_object(state, card_id, AI, format!("Expensive {idx}"), Zone::Hand);
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

    // ── Tests ──────────────────────────────────────────────────────────────────

    #[test]
    fn opts_out_when_commitment_low() {
        let features = features_with(0.3, vec![]); // ≤ MULLIGAN_FLOOR (0.40)
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::CheapSpell,
            Card::CheapSpell,
            Card::CheapSpell,
            Card::CheapSpell,
            Card::Payoff("Monk".to_string()),
        ]);
        let score = SpellslingerKeepablesMulligan.evaluate(
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
                assert_eq!(reason.kind, "spellslinger_keepables_na");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn commitment_at_threshold_opts_out() {
        // Exactly == MULLIGAN_FLOOR (0.40) → opts out (≤ not <).
        let features = features_with(MULLIGAN_FLOOR, vec![]);
        let (state, hand) = make_hand(vec![Card::Land, Card::Land, Card::CheapSpell]);
        let score = SpellslingerKeepablesMulligan.evaluate(
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
                assert_eq!(reason.kind, "spellslinger_keepables_na");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn ideal_hand_three_cheap_spells_payoff_two_lands() {
        // 3 cheap spells + 1 payoff + 2 lands → +2.0.
        let features = features_with(0.8, vec!["Monastery Swiftspear".to_string()]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::CheapSpell,
            Card::CheapSpell,
            Card::CheapSpell,
            Card::Payoff("Monastery Swiftspear".to_string()),
            Card::ExpensiveSpell,
        ]);
        let score = SpellslingerKeepablesMulligan.evaluate(
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
                    (delta - 2.0).abs() < 1e-5,
                    "ideal hand should score +2.0, got {delta}"
                );
                assert_eq!(reason.kind, "spellslinger_keepable_ideal");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn density_only_burn_hand_keepable() {
        // 4 cheap spells + 2 lands but no named payoff → +1.0.
        let features = features_with(0.8, vec![]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::CheapSpell,
            Card::CheapSpell,
            Card::CheapSpell,
            Card::CheapSpell,
        ]);
        let score = SpellslingerKeepablesMulligan.evaluate(
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
                    (delta - 1.0).abs() < 1e-5,
                    "density-only hand should score +1.0, got {delta}"
                );
                assert_eq!(reason.kind, "spellslinger_density_keepable");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn payoff_only_keepable_with_lands() {
        // 1 prowess creature + 2 lands, no cheap spells → +0.5.
        let features = features_with(0.8, vec!["Monastery Swiftspear".to_string()]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Payoff("Monastery Swiftspear".to_string()),
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
        ]);
        let score = SpellslingerKeepablesMulligan.evaluate(
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
                    (delta - 0.5).abs() < 1e-5,
                    "payoff-with-lands hand should score +0.5, got {delta}"
                );
                assert_eq!(reason.kind, "spellslinger_payoff_with_lands");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn no_density_no_payoff_penalized() {
        // All lands + expensive spells, no cheap spells or payoffs → -1.0.
        let features = features_with(0.8, vec!["Monastery Swiftspear".to_string()]);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
            Card::ExpensiveSpell,
        ]);
        let score = SpellslingerKeepablesMulligan.evaluate(
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
                    "no-density hand should score negative, got {delta}"
                );
                assert_eq!(reason.kind, "spellslinger_no_density");
            }
            _ => panic!("expected Score"),
        }
    }
}
