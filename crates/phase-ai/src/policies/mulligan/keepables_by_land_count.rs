//! `KeepablesByLandCount` — baseline land-count + castability mulligan policy.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding whether to keep or
//! mulligan an opening hand. This policy is the deck-agnostic baseline — it
//! checks land count, color availability, and early castability.
//!
//! The minimum kept-hand size is NOT this policy's concern — it belongs to
//! `card_floor::MulliganCardFloor`, the single process-level authority, whose
//! `ForceKeep` outranks every verdict below.
//!
//! Outcomes are translated into structured `MulliganScore` verdicts:
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

use super::{
    has_spell_face, is_land_source, modal_back_face, MulliganPolicy, MulliganScore, TurnOrder,
};

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
        _plan: &PlanSnapshot, // input-unused: land-count policy uses mulligan count directly
        _turn_order: TurnOrder, // input-unused: land-count policy uses mulligan count directly
        mulligans_taken: u8,
    ) -> MulliganScore {
        let hand_size = hand.len();
        // After 2+ mulligans, be much more lenient — keep any hand with at
        // least 1 land + 1 spell.
        if mulligans_taken >= 2 {
            let has_land = hand
                .iter()
                .any(|&oid| state.objects.get(&oid).is_some_and(is_land_source));
            let has_spell = hand
                .iter()
                .any(|&oid| state.objects.get(&oid).is_some_and(has_spell_face));
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
        let mut land_only_count: i64 = 0;
        let mut spell_count: i64 = 0;
        let mut available_colors: Vec<ManaType> = Vec::new();
        let mut has_two_or_more_color_source = false;

        for &oid in hand.iter() {
            let Some(obj) = state.objects.get(&oid) else {
                continue;
            };
            if is_land_source(obj) {
                land_count += 1;
                let produced_colors = land_produced_color_types(obj);
                for mana_type in &produced_colors {
                    if !available_colors.contains(mana_type) {
                        available_colors.push(*mana_type);
                    }
                }
                if produced_colors.len() >= 2 {
                    has_two_or_more_color_source = true;
                }
            }
            if has_spell_face(obj) {
                spell_count += 1;
            } else {
                land_only_count += 1;
            }
        }

        let land_ok = if hand_size >= 6 {
            land_count >= 2 && land_only_count <= 5
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
                spell_face_is_early_castable(
                    &obj.card_types.core_types,
                    &obj.mana_cost,
                    land_count,
                    &available_colors,
                    has_two_or_more_color_source,
                ) || modal_back_face(obj).is_some_and(|face| {
                    spell_face_is_early_castable(
                        &face.card_types.core_types,
                        &face.mana_cost,
                        land_count,
                        &available_colors,
                        has_two_or_more_color_source,
                    )
                })
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
fn spell_colors_available(
    cost: &ManaCost,
    available: &[ManaType],
    has_two_or_more_color_source: bool,
) -> bool {
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
            ManaCostShard::TwoOrMoreColorSource => has_two_or_more_color_source,
        };
        if !satisfied {
            return false;
        }
    }
    true
}

fn land_produced_color_types(obj: &engine::game::game_object::GameObject) -> Vec<ManaType> {
    let mut colors = Vec::new();
    if obj.card_types.core_types.contains(&CoreType::Land) {
        colors.extend(crate::mana_colors::land_produced_color_types(
            &obj.card_types.subtypes,
            &obj.abilities,
        ));
    }
    if let Some(face) = modal_back_face(obj) {
        if face.card_types.core_types.contains(&CoreType::Land) {
            for color in crate::mana_colors::land_produced_color_types(
                &face.card_types.subtypes,
                &face.abilities,
            ) {
                if !colors.contains(&color) {
                    colors.push(color);
                }
            }
        }
    }
    colors
}

fn spell_face_is_early_castable(
    core_types: &[CoreType],
    mana_cost: &ManaCost,
    land_count: i64,
    available_colors: &[ManaType],
    has_two_or_more_color_source: bool,
) -> bool {
    if core_types.contains(&CoreType::Land) || mana_cost.mana_value() > (land_count as u32 + 1) {
        return false;
    }
    spell_colors_available(mana_cost, available_colors, has_two_or_more_color_source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::printed_cards::snapshot_object_face;
    use engine::game::zones::create_object;
    use engine::types::card::LayoutKind;
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

    fn plan() -> PlanSnapshot {
        PlanSnapshot::default()
    }

    fn features() -> DeckFeatures {
        DeckFeatures::default()
    }

    fn add_modal_back_face(
        state: &mut GameState,
        object_id: ObjectId,
        core_types: Vec<CoreType>,
        subtypes: Vec<&str>,
        mana_cost: ManaCost,
    ) {
        let object = state.objects.get_mut(&object_id).expect("hand card");
        let mut back_face = snapshot_object_face(object);
        back_face.card_types = CardType {
            supertypes: Vec::new(),
            core_types,
            subtypes: subtypes.into_iter().map(String::from).collect(),
        };
        back_face.mana_cost = mana_cost;
        back_face.layout_kind = Some(LayoutKind::Modal);
        object.back_face = Some(back_face);
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
    fn spell_front_mdfc_land_back_counts_as_source_and_spell() {
        // One front spell / back Mountain MDFC plus an Island supplies two
        // viable land plays and a Red spell. The back face supplies the only
        // red source, so both face metadata and spell-face accounting matter.
        let mut state = setup_game(vec![
            spell_cheap("Modal Bolt", ManaCostShard::Red),
            land("Island", "Island"),
            spell_cheap("Bolt 1", ManaCostShard::Red),
            spell_cheap("Bolt 2", ManaCostShard::Red),
            spell_cheap("Bolt 3", ManaCostShard::Red),
            spell_cheap("Bolt 4", ManaCostShard::Red),
            spell_cheap("Bolt 5", ManaCostShard::Red),
        ]);
        let modal = state.players[0].hand[0];
        add_modal_back_face(
            &mut state,
            modal,
            vec![CoreType::Land],
            vec!["Mountain"],
            ManaCost::NoCost,
        );
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
            MulliganScore::Score { reason, .. } => {
                assert_eq!(reason.kind, "hand_has_land_range");
                assert!(reason.facts.contains(&("land_count", 2)));
                assert!(reason.facts.contains(&("castable_early", 6)));
            }
            _ => panic!("expected MDFC hand to keep"),
        }
    }

    #[test]
    fn modal_spell_faces_prevent_flexible_hands_from_looking_flooded() {
        // These five MDFCs are spells as well as viable land sources. Treating
        // them as land-only would falsely reject the seven-card hand as flooded.
        let mut state = setup_game(vec![
            land("Island 1", "Island"),
            land("Island 2", "Island"),
            spell_cheap("Modal 1", ManaCostShard::Red),
            spell_cheap("Modal 2", ManaCostShard::Red),
            spell_cheap("Modal 3", ManaCostShard::Red),
            spell_cheap("Modal 4", ManaCostShard::Red),
            spell_cheap("Modal 5", ManaCostShard::Red),
        ]);
        let modal_ids: Vec<_> = state.players[0].hand.iter().copied().skip(2).collect();
        for modal in modal_ids {
            add_modal_back_face(
                &mut state,
                modal,
                vec![CoreType::Land],
                vec!["Mountain"],
                ManaCost::NoCost,
            );
        }
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();

        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            0,
        );

        assert!(matches!(
            score,
            MulliganScore::Score { ref reason, .. } if reason.kind == "hand_has_land_range"
        ));
    }

    #[test]
    fn lenient_mulligan_accepts_land_front_mdfc_spell_back() {
        // Face orientation is not a policy concern: a front-land MDFC with a
        // spell back face satisfies the post-mulligan land-and-spell floor.
        let mut state = setup_game(vec![
            land("Modal Land", "Island"),
            land("Island 1", "Island"),
            land("Island 2", "Island"),
            land("Island 3", "Island"),
            land("Island 4", "Island"),
        ]);
        let modal = state.players[0].hand[0];
        add_modal_back_face(
            &mut state,
            modal,
            vec![CoreType::Instant],
            Vec::new(),
            ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            },
        );
        let hand: Vec<_> = state.players[0].hand.iter().copied().collect();

        let score = KeepablesByLandCount.evaluate(
            &hand,
            &state,
            &features(),
            &plan(),
            TurnOrder::OnPlay,
            2,
        );

        assert!(matches!(
            score,
            MulliganScore::Score { ref reason, .. } if reason.kind == "hand_lenient_after_mulligans"
        ));
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
        // 5-card hand at mulligans_taken == 2 → the lenient branch; hand size is
        // no longer consulted. 1 land + 4 spells — lenient accept.
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
        // mulligans_taken == 2 → the lenient branch, which accepts any hand
        // carrying at least one land and one spell regardless of size.
        match score {
            MulliganScore::Score { reason, .. } => {
                assert_eq!(reason.kind, "hand_lenient_after_mulligans");
            }
            _ => panic!("expected Score"),
        }
    }
}
