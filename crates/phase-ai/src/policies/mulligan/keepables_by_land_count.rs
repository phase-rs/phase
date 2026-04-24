//! `KeepablesByLandCount` — baseline land-count + castability mulligan policy.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding whether to keep or
//! mulligan an opening hand. This policy is the deck-agnostic baseline — it
//! checks land count, color availability, and early castability.
//!
//! Outcomes are translated into structured `MulliganScore` verdicts:
//! - Always-keep short hands (≤ 4 cards post-mulligan) → `Score { +5.0 }`.
//! - Post-2-mulligan lenient accept (has land + has spell) → `Score { +2.0 }`.
//! - Post-2-mulligan lenient reject (missing land or spell) →
//!   `ForceMulligan`.
//! - Full-size hand with bad land ratio → `ForceMulligan`.
//! - Full-size hand with no early-castable spell → `ForceMulligan`.
//! - Full-size hand that passes all checks → `Score { +3.0 }`.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

pub struct KeepablesByLandCount;

impl MulliganPolicy for KeepablesByLandCount {
    fn id(&self) -> PolicyId {
        PolicyId::KeepablesByLandCount
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        _features: &DeckFeatures,
        _plan: &PlanSnapshot,
        _turn_order: TurnOrder,
        mulligans_taken: u8,
    ) -> MulliganScore {
        let hand_size = hand.len();

        // Defensive short-hand keep. Under the London Mulligan (CR 103.5) the
        // engine always presents a 7-card hand at `WaitingFor::MulliganDecision`
        // (bottoming happens *after* the keep decision), so this branch is
        // unreachable from production paths today. It stays as a guard against
        // direct `evaluate()` invocations (tests, hypothetical non-London
        // mulligan flows) so no caller can observe an unkeepable tiny hand.
        if hand_size <= 4 {
            return MulliganScore::Score {
                delta: 5.0,
                reason: PolicyReason::new("hand_short_force_keep")
                    .with_fact("hand_size", hand_size as i64)
                    .with_fact("mulligans_taken", mulligans_taken as i64),
            };
        }

        // After 2+ mulligans, be much more lenient — keep any hand with at
        // least 1 land + 1 spell.
        if mulligans_taken >= 2 {
            let has_land = hand.iter().any(|&oid| {
                state
                    .objects
                    .get(&oid)
                    .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Land))
            });
            let has_spell = hand.iter().any(|&oid| {
                state
                    .objects
                    .get(&oid)
                    .is_some_and(|o| !o.card_types.core_types.contains(&CoreType::Land))
            });
            if has_land && has_spell {
                return MulliganScore::Score {
                    delta: 2.0,
                    reason: PolicyReason::new("hand_lenient_after_mulligans")
                        .with_fact("hand_size", hand_size as i64)
                        .with_fact("mulligans_taken", mulligans_taken as i64),
                };
            }
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("hand_lenient_reject")
                    .with_fact("hand_size", hand_size as i64)
                    .with_fact("mulligans_taken", mulligans_taken as i64)
                    .with_fact("has_land", i64::from(has_land))
                    .with_fact("has_spell", i64::from(has_spell)),
            };
        }

        let mut land_count: i64 = 0;
        let mut available_colors: Vec<ManaType> = Vec::new();

        for &oid in hand.iter() {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            if obj.card_types.core_types.contains(&CoreType::Land) {
                land_count += 1;
                for subtype in &obj.card_types.subtypes {
                    if let Some(mana_type) =
                        engine::game::mana_payment::land_subtype_to_mana_type(subtype)
                    {
                        if !available_colors.contains(&mana_type) {
                            available_colors.push(mana_type);
                        }
                    }
                }
            }
        }

        let spell_count: i64 = hand_size as i64 - land_count;

        let land_ok = if hand_size >= 6 {
            (2..=5).contains(&land_count)
        } else {
            land_count >= 1 && spell_count >= 1
        };

        if !land_ok {
            let kind = if land_count < 2 {
                "hand_too_few_lands"
            } else {
                "hand_too_many_lands"
            };
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new(kind)
                    .with_fact("hand_size", hand_size as i64)
                    .with_fact("land_count", land_count)
                    .with_fact("mulligans_taken", mulligans_taken as i64),
            };
        }

        // Castability check: count spells castable in the first 3 turns given
        // available land colors and expected mana progression.
        let castable_early = hand
            .iter()
            .filter(|&&oid| {
                let Some(obj) = state.objects.get(&oid) else {
                    return false;
                };
                if obj.card_types.core_types.contains(&CoreType::Land) {
                    return false;
                }
                let mv = obj.mana_cost.mana_value();
                if mv > (land_count as u32 + 1) {
                    return false;
                }
                spell_colors_available(&obj.mana_cost, &available_colors)
            })
            .count();

        if castable_early == 0 && spell_count > 0 {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("hand_no_early_castable")
                    .with_fact("hand_size", hand_size as i64)
                    .with_fact("land_count", land_count)
                    .with_fact("spell_count", spell_count),
            };
        }

        MulliganScore::Score {
            delta: 3.0,
            reason: PolicyReason::new("hand_has_land_range")
                .with_fact("hand_size", hand_size as i64)
                .with_fact("land_count", land_count)
                .with_fact("castable_early", castable_early as i64),
        }
    }
}

/// Check whether the colors required by a spell's mana cost can be produced
/// by the available mana types (from lands in hand). Not a rule-bearing
/// function — a castability heuristic used only by this policy.
fn spell_colors_available(cost: &ManaCost, available: &[ManaType]) -> bool {
    let ManaCost::Cost { shards, .. } = cost else {
        return true; // NoCost or SelfManaCost — always castable
    };

    for shard in shards {
        let satisfied = match shard {
            ManaCostShard::White | ManaCostShard::PhyrexianWhite | ManaCostShard::TwoWhite => {
                available.contains(&ManaType::White)
            }
            ManaCostShard::Blue | ManaCostShard::PhyrexianBlue | ManaCostShard::TwoBlue => {
                available.contains(&ManaType::Blue)
            }
            ManaCostShard::Black | ManaCostShard::PhyrexianBlack | ManaCostShard::TwoBlack => {
                available.contains(&ManaType::Black)
            }
            ManaCostShard::Red | ManaCostShard::PhyrexianRed | ManaCostShard::TwoRed => {
                available.contains(&ManaType::Red)
            }
            ManaCostShard::Green | ManaCostShard::PhyrexianGreen | ManaCostShard::TwoGreen => {
                available.contains(&ManaType::Green)
            }
            ManaCostShard::WhiteBlue | ManaCostShard::PhyrexianWhiteBlue => {
                available.contains(&ManaType::White) || available.contains(&ManaType::Blue)
            }
            ManaCostShard::BlueBlack | ManaCostShard::PhyrexianBlueBlack => {
                available.contains(&ManaType::Blue) || available.contains(&ManaType::Black)
            }
            ManaCostShard::BlackRed | ManaCostShard::PhyrexianBlackRed => {
                available.contains(&ManaType::Black) || available.contains(&ManaType::Red)
            }
            ManaCostShard::RedGreen | ManaCostShard::PhyrexianRedGreen => {
                available.contains(&ManaType::Red) || available.contains(&ManaType::Green)
            }
            ManaCostShard::GreenWhite | ManaCostShard::PhyrexianGreenWhite => {
                available.contains(&ManaType::Green) || available.contains(&ManaType::White)
            }
            ManaCostShard::WhiteBlack | ManaCostShard::PhyrexianWhiteBlack => {
                available.contains(&ManaType::White) || available.contains(&ManaType::Black)
            }
            ManaCostShard::BlueRed | ManaCostShard::PhyrexianBlueRed => {
                available.contains(&ManaType::Blue) || available.contains(&ManaType::Red)
            }
            ManaCostShard::BlackGreen | ManaCostShard::PhyrexianBlackGreen => {
                available.contains(&ManaType::Black) || available.contains(&ManaType::Green)
            }
            ManaCostShard::RedWhite | ManaCostShard::PhyrexianRedWhite => {
                available.contains(&ManaType::Red) || available.contains(&ManaType::White)
            }
            ManaCostShard::GreenBlue | ManaCostShard::PhyrexianGreenBlue => {
                available.contains(&ManaType::Green) || available.contains(&ManaType::Blue)
            }
            ManaCostShard::Colorless
            | ManaCostShard::Snow
            | ManaCostShard::X
            | ManaCostShard::ColorlessWhite
            | ManaCostShard::ColorlessBlue
            | ManaCostShard::ColorlessBlack
            | ManaCostShard::ColorlessRed
            | ManaCostShard::ColorlessGreen => true,
        };
        if !satisfied {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    struct HandCard {
        name: String,
        core_types: Vec<CoreType>,
        subtypes: Vec<String>,
        mana_cost: ManaCost,
    }

    fn setup_game(hand_objs: Vec<HandCard>) -> GameState {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        // Clear auto-initialized hand (if any).
        state.players[player.0 as usize].hand.clear();
        for (idx, card) in hand_objs.into_iter().enumerate() {
            let oid = create_object(
                &mut state,
                CardId(1000 + idx as u64),
                player,
                card.name,
                Zone::Hand,
            );
            let obj = state.objects.get_mut(&oid).expect("just created");
            obj.card_types = CardType {
                supertypes: Vec::new(),
                core_types: card.core_types,
                subtypes: card.subtypes,
            };
            obj.mana_cost = card.mana_cost;
        }
        state
    }

    fn land(name: &str, subtype: &str) -> HandCard {
        HandCard {
            name: name.to_string(),
            core_types: vec![CoreType::Land],
            subtypes: vec![subtype.to_string()],
            mana_cost: ManaCost::NoCost,
        }
    }

    fn spell_cheap(name: &str, color: ManaCostShard) -> HandCard {
        HandCard {
            name: name.to_string(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
            mana_cost: ManaCost::Cost {
                shards: vec![color],
                generic: 0,
            },
        }
    }

    fn spell_expensive(name: &str) -> HandCard {
        HandCard {
            name: name.to_string(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
            mana_cost: ManaCost::Cost {
                shards: Vec::new(),
                generic: 6,
            },
        }
    }

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    fn features() -> DeckFeatures {
        DeckFeatures::default()
    }

    #[test]
    fn short_hand_is_kept() {
        // 4-card hand — always keep regardless of contents.
        let state = setup_game(vec![
            spell_expensive("A"),
            spell_expensive("B"),
            spell_expensive("C"),
            spell_expensive("D"),
        ]);
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();
        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            3,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0);
                assert_eq!(reason.kind, "hand_short_force_keep");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn full_hand_with_ok_lands_keeps() {
        // 7-card hand: 3 Mountains + 4 Red creatures, all early-castable.
        let state = setup_game(vec![
            land("Mountain 1", "Mountain"),
            land("Mountain 2", "Mountain"),
            land("Mountain 3", "Mountain"),
            spell_cheap("Bolt 1", ManaCostShard::Red),
            spell_cheap("Bolt 2", ManaCostShard::Red),
            spell_cheap("Bolt 3", ManaCostShard::Red),
            spell_cheap("Bolt 4", ManaCostShard::Red),
        ]);
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();
        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(delta > 0.0);
                assert_eq!(reason.kind, "hand_has_land_range");
            }
            _ => panic!("expected Score"),
        }
    }

    #[test]
    fn full_hand_no_lands_force_mulligan() {
        let state = setup_game(vec![
            spell_cheap("Bolt 1", ManaCostShard::Red),
            spell_cheap("Bolt 2", ManaCostShard::Red),
            spell_cheap("Bolt 3", ManaCostShard::Red),
            spell_cheap("Bolt 4", ManaCostShard::Red),
            spell_cheap("Bolt 5", ManaCostShard::Red),
            spell_cheap("Bolt 6", ManaCostShard::Red),
            spell_cheap("Bolt 7", ManaCostShard::Red),
        ]);
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();
        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(matches!(score, MulliganScore::ForceMulligan { .. }));
    }

    #[test]
    fn full_hand_all_lands_force_mulligan() {
        let state = setup_game(vec![
            land("Mountain 1", "Mountain"),
            land("Mountain 2", "Mountain"),
            land("Mountain 3", "Mountain"),
            land("Mountain 4", "Mountain"),
            land("Mountain 5", "Mountain"),
            land("Mountain 6", "Mountain"),
            land("Mountain 7", "Mountain"),
        ]);
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();
        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(matches!(score, MulliganScore::ForceMulligan { .. }));
    }

    #[test]
    fn full_hand_wrong_colors_force_mulligan() {
        // 3 Islands + 4 Red creatures — no Red mana available; 0 castable early.
        let state = setup_game(vec![
            land("Island 1", "Island"),
            land("Island 2", "Island"),
            land("Island 3", "Island"),
            spell_cheap("Bolt 1", ManaCostShard::Red),
            spell_cheap("Bolt 2", ManaCostShard::Red),
            spell_cheap("Bolt 3", ManaCostShard::Red),
            spell_cheap("Bolt 4", ManaCostShard::Red),
        ]);
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();
        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::ForceMulligan { reason } => {
                assert_eq!(reason.kind, "hand_no_early_castable");
            }
            _ => panic!("expected ForceMulligan"),
        }
    }

    #[test]
    fn lenient_after_two_mulligans() {
        // 5-card hand (mulligan_count=2 still checked because hand_size>4)
        // with 1 land + 4 spells — lenient accept.
        let state = setup_game(vec![
            land("Mountain", "Mountain"),
            spell_cheap("Bolt 1", ManaCostShard::Red),
            spell_cheap("Bolt 2", ManaCostShard::Red),
            spell_cheap("Bolt 3", ManaCostShard::Red),
            spell_cheap("Bolt 4", ManaCostShard::Red),
        ]);
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();
        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            2,
        );
        // hand_size 5 → falls through to mulligans_taken>=2 arm? No — short-hand
        // arm requires hand_size <= 4. 5 goes to the lenient branch.
        match score {
            MulliganScore::Score { reason, .. } => {
                assert_eq!(reason.kind, "hand_lenient_after_mulligans");
            }
            _ => panic!("expected Score"),
        }
    }
}
