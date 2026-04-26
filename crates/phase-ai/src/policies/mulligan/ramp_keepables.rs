//! `RampKeepablesMulligan` — feature-driven mulligan policy for mana-ramp decks.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding to keep after the
//! mulligan process. When a deck's mana-ramp commitment is meaningful, opening
//! hands that combine a ramp piece (dork/rock/fetch-spell/ritual/extra-landdrop)
//! with enough lands to use it are strongly preferred.
//!
//! Opts out for decks where `features.mana_ramp.commitment <= 0.3` — the
//! baseline `KeepablesByLandCount` policy is the sole voice for those decks.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::mana_ramp::is_ramp_piece_parts;
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

/// Commitment threshold below which this policy opts out. Matches the
/// plan-mandated 0.3 boundary — fewer than ~2-3 ramp pieces doesn't warrant
/// ramp-specific mulligan preferences.
const COMMITMENT_THRESHOLD: f32 = 0.3;

pub struct RampKeepablesMulligan;

impl MulliganPolicy for RampKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::RampKeepablesMulligan
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
        let commitment = features.mana_ramp.commitment;
        if commitment <= COMMITMENT_THRESHOLD {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("ramp_keepables_na")
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        let mut land_count: i64 = 0;
        let mut ramp_count: i64 = 0;

        // Classify each hand card structurally using the same parts-based
        // predicates the feature uses at detection time. This covers all four
        // ramp axes (dork/rock, land-fetch spell, ritual, extra-land-drop) —
        // Lightning Bolt on an Instant is NOT counted; Llanowar Elves on a
        // Creature IS counted.
        for &oid in hand {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            if obj.card_types.core_types.contains(&CoreType::Land) {
                land_count += 1;
                continue;
            }
            if is_ramp_piece_parts(
                &obj.card_types.core_types,
                &obj.abilities,
                obj.static_definitions.as_slice(),
            ) {
                ramp_count += 1;
            }
        }

        // Ideal: ramp piece + enough lands to deploy it early.
        if ramp_count >= 1 && land_count >= 2 {
            return MulliganScore::Score {
                delta: 2.0,
                reason: PolicyReason::new("ramp_keepable_ideal")
                    .with_fact("ramp_count", ramp_count)
                    .with_fact("land_count", land_count)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        // A ramp piece with only one land is risky but passable.
        if ramp_count >= 1 && land_count == 1 {
            return MulliganScore::Score {
                delta: 0.5,
                reason: PolicyReason::new("ramp_light_lands_ok")
                    .with_fact("ramp_count", ramp_count)
                    .with_fact("land_count", land_count),
            };
        }

        // Many lands but no ramp pieces in hand undermines a ramp deck's plan.
        if ramp_count == 0 && land_count >= 3 {
            return MulliganScore::Score {
                delta: -0.5,
                reason: PolicyReason::new("ramp_no_ramp_in_hand")
                    .with_fact("land_count", land_count)
                    .with_fact("commitment_x1000", (commitment * 1000.0) as i64),
            };
        }

        // Everything else — defer to the baseline keepables policy.
        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("ramp_defer_to_baseline")
                .with_fact("ramp_count", ramp_count)
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
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        QuantityExpr, TargetFilter, TypedFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use crate::features::{DeckFeatures, ManaRampFeature};

    fn features_with_commitment(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            mana_ramp: ManaRampFeature {
                dork_count: 4,
                land_fetch_count: 4,
                commitment,
                ..Default::default()
            },
            ..DeckFeatures::default()
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    /// Canonical tap-for-mana ability (mana dork / mana rock shape).
    fn tap_for_mana_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: Vec::new(),
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability
    }

    /// Land-fetch spell ability (Rampant Growth shape — search library, put
    /// land onto battlefield).
    fn land_fetch_spell_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                up_to: false,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                under_your_control: true,
                enter_tapped: true,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        ability
    }

    /// Non-ramp Spell-kind ability (Lightning Bolt / Opt shape) — used as a
    /// negative to verify the classifier doesn't count every instant/sorcery.
    fn non_ramp_spell_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        )
    }

    enum Card {
        Land,
        DorkCreature,
        FetchSpellSorcery,
        NonRampInstant,
    }

    fn add_card(state: &mut GameState, idx: u64, card: Card) -> ObjectId {
        let (name, core_types, ability) = match card {
            Card::Land => (format!("Forest {idx}"), vec![CoreType::Land], None),
            Card::DorkCreature => (
                format!("Llanowar Elves {idx}"),
                vec![CoreType::Creature],
                Some(tap_for_mana_ability()),
            ),
            Card::FetchSpellSorcery => (
                format!("Rampant Growth {idx}"),
                vec![CoreType::Sorcery],
                Some(land_fetch_spell_ability()),
            ),
            Card::NonRampInstant => (
                format!("Lightning Bolt {idx}"),
                vec![CoreType::Instant],
                Some(non_ramp_spell_ability()),
            ),
        };
        let oid = create_object(state, CardId(2000 + idx), PlayerId(0), name, Zone::Hand);
        let obj = state.objects.get_mut(&oid).expect("just created");
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types,
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::NoCost;
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
            Card::FetchSpellSorcery,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
        ]);
        let score =
            RampKeepablesMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "ramp_keepables_na");
            }
            _ => panic!("expected opt-out Score"),
        }
    }

    #[test]
    fn ideal_hand_fetch_spell_plus_two_lands() {
        let features = features_with_commitment(0.9);
        // 2 lands + 1 fetch spell + 4 filler = 7 cards
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::FetchSpellSorcery,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
        ]);
        let score =
            RampKeepablesMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert_eq!(reason.kind, "ramp_keepable_ideal");
            }
            _ => panic!("expected ideal Score"),
        }
    }

    #[test]
    fn ideal_hand_dork_creature_plus_two_lands() {
        // Llanowar Elves + 2 lands: classifier must count the dork as ramp.
        // Regression: old policy ignored permanent-type ramp entirely.
        let features = features_with_commitment(0.9);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::DorkCreature,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
        ]);
        let score =
            RampKeepablesMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert_eq!(reason.kind, "ramp_keepable_ideal");
            }
            _ => panic!("expected ideal Score"),
        }
    }

    #[test]
    fn non_ramp_instant_is_not_counted_as_ramp() {
        // Regression: an older classifier counted every instant/sorcery as
        // ramp because every spell has AbilityKind::Spell. Lightning Bolt on
        // an Instant must NOT boost ramp_count — 3 lands + only Bolts should
        // score as no-ramp-in-hand.
        let features = features_with_commitment(0.9);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::Land,
            Card::Land,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
        ]);
        let score =
            RampKeepablesMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta < 0.0, "expected negative delta, got {delta}");
                assert_eq!(reason.kind, "ramp_no_ramp_in_hand");
            }
            _ => panic!("expected penalty Score"),
        }
    }

    #[test]
    fn ramp_light_lands_ok() {
        let features = features_with_commitment(0.9);
        let (state, hand) = make_hand(vec![
            Card::Land,
            Card::FetchSpellSorcery,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
        ]);
        let score =
            RampKeepablesMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert_eq!(reason.kind, "ramp_light_lands_ok");
            }
            _ => panic!("expected light-lands Score"),
        }
    }

    #[test]
    fn defer_to_baseline_when_no_lands_no_ramp() {
        let features = features_with_commitment(0.9);
        let (state, hand) = make_hand(vec![
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
            Card::NonRampInstant,
        ]);
        let score =
            RampKeepablesMulligan.evaluate(&hand, &state, &features, &plan(), TurnOrder::OnPlay, 0);
        match score {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "ramp_defer_to_baseline");
            }
            _ => panic!("expected defer Score"),
        }
    }
}
