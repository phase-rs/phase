//! `CedhKeepablesMulligan` — stub aggressive mulligan policy for cEDH decks.
//! Gated internally on `features.bracket_tier == Cedh` (`MulliganPolicy` has no
//! `activation()` method; every registered policy is consulted on every hand).
//!
//! CR 103.5: deciding to keep after the
//! mulligan process.
//!
//! Real cEDH mulligan strategy ("keep only hands that win or stop the opponent
//! from winning by turn 4") lands when the `ComboRegistry` is populated and
//! the policy can ask `ComboRegistry::reachable_lines(hand_pseudo_state)`.
//!
use engine::game::bracket_estimate::CommanderBracketTier;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter, TypeFilter};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::ability_chain::collect_chain_effects;
use crate::combo::ComboRegistry;
use crate::features::control::is_counterspell_parts;
use crate::features::mana_ramp::{is_mana_dork_parts, is_ritual_parts};
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{is_land_only_source, is_land_source, MulliganPolicy, MulliganScore, TurnOrder};

#[derive(Default)]
pub struct CedhKeepablesMulligan {
    combo_registry: ComboRegistry,
}

impl CedhKeepablesMulligan {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MulliganPolicy for CedhKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::CedhKeepablesMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        _plan: &PlanSnapshot, // input-unused: cEDH structure checks do not use curve plan
        _turn_order: TurnOrder, // input-unused: cEDH structure checks do not use play/draw
        _mulligans_taken: u8, // input-unused: depth is bounded by `card_floor::MulliganCardFloor`; this policy judges composition only
    ) -> MulliganScore {
        // Internal gate: non-cEDH decks see a zero-delta Score (cheap no-op).
        if features.bracket_tier != CommanderBracketTier::Cedh {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("cedh_keepables_na"),
            };
        }

        let land_count = count_lands_in_hand(hand, state);
        let land_only_count = count_land_only_cards_in_hand(hand, state);

        // < 2 lands: can't cast spells or accelerate. CR 103.5 — even cEDH
        // hands must be able to develop a mana base.
        if land_count < 2 {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("cedh_keepables_too_few_lands")
                    .with_fact("lands", land_count as i64),
            };
        }

        // > 4 lands: too land-heavy for a 37-land cEDH list; threat/combo
        // density too diluted to win at a speed-focused table.
        if land_only_count > 4 {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("cedh_keepables_too_many_lands")
                    .with_fact("lands", land_only_count as i64),
            };
        }

        // Combo-in-hand check overrides the staple heuristics: if the AI has
        // already drawn a complete in-hand combo (e.g., Thoracle + Consult),
        // the hand is a strong keep regardless of whether a tutor / fast-mana
        // staple is also present. Real cEDH mulligan policy: "keep hands
        // that can win." This is the cheapest expression of that idea
        // available without modeling pseudo-state mana progression.
        let combo_lines = self.combo_registry.lines_with_pieces_in_hand(hand, state);
        if !combo_lines.is_empty() {
            return MulliganScore::Score {
                delta: 5.0,
                reason: PolicyReason::new("cedh_keepables_combo_in_hand")
                    .with_fact("combo_lines", combo_lines.len() as i64),
            };
        }

        let has_fast_mana = hand_has_any(hand, state, is_fast_mana_object);
        let has_tutor = hand_has_any(hand, state, is_tutor_object);
        let has_interaction = hand_has_any(hand, state, is_interaction_object);

        // No acceleration, no tutor, no interaction: the hand has neither a
        // fast-mana clock nor a disruption piece — untenable at a cEDH table.
        if !has_fast_mana && !has_tutor && !has_interaction {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("cedh_keepables_no_acceleration_tutor_or_interaction"),
            };
        }

        // Positive baseline so a cEDH-tagged hand is kept absent forced
        // mulligans from this policy or other registered policies.
        MulliganScore::Score {
            delta: 1.0,
            reason: PolicyReason::new("cedh_keepables_baseline_keep"),
        }
    }
}

fn count_lands_in_hand(hand: &[ObjectId], state: &GameState) -> u32 {
    hand.iter()
        .filter(|&&id| state.objects.get(&id).is_some_and(is_land_source))
        .count() as u32
}

fn count_land_only_cards_in_hand(hand: &[ObjectId], state: &GameState) -> u32 {
    hand.iter()
        .filter(|&&id| state.objects.get(&id).is_some_and(is_land_only_source))
        .count() as u32
}

fn hand_has_any<F>(hand: &[ObjectId], state: &GameState, pred: F) -> bool
where
    F: Fn(&engine::game::GameObject) -> bool,
{
    hand.iter()
        .any(|&id| state.objects.get(&id).is_some_and(&pred))
}

fn is_fast_mana_object(obj: &engine::game::GameObject) -> bool {
    is_mana_dork_parts(&obj.card_types.core_types, &obj.abilities)
        || is_ritual_parts(&obj.card_types.core_types, &obj.abilities)
}

fn is_tutor_object(obj: &engine::game::GameObject) -> bool {
    obj.abilities
        .iter()
        .any(|ability| ability.kind == AbilityKind::Spell && chain_searches_nonland(ability))
}

fn is_interaction_object(obj: &engine::game::GameObject) -> bool {
    is_counterspell_parts(&obj.abilities)
}

fn chain_searches_nonland(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability).iter().any(|effect| {
        matches!(
            effect,
            Effect::SearchLibrary { filter, .. } if !target_filter_references_land(filter)
        )
    })
}

fn target_filter_references_land(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(type_filter_references_land),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(target_filter_references_land)
        }
        _ => false,
    }
}

fn type_filter_references_land(filter: &TypeFilter) -> bool {
    match filter {
        TypeFilter::Land => true,
        TypeFilter::AnyOf(filters) => filters.iter().any(type_filter_references_land),
        _ => false,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::game::printed_cards::snapshot_object_face;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, ManaContribution, ManaProduction, QuantityExpr, TypedFilter,
    };
    use engine::types::card::LayoutKind;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use super::*;
    use crate::plan::PlanSnapshot;

    fn make_state() -> GameState {
        GameState::new_two_player(0)
    }

    fn features_cedh(is_cedh: bool) -> DeckFeatures {
        DeckFeatures {
            bracket_tier: if is_cedh {
                CommanderBracketTier::Cedh
            } else {
                CommanderBracketTier::Core
            },
            ..DeckFeatures::default()
        }
    }

    /// Add a card to the given state in `Zone::Hand` for player 0.
    /// Returns the `ObjectId` of the newly created object.
    fn add_hand_card(
        state: &mut GameState,
        idx: u64,
        name: &str,
        core_types: Vec<CoreType>,
    ) -> ObjectId {
        let oid = create_object(
            state,
            CardId(3000 + idx),
            PlayerId(0),
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).expect("just created");
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types,
            subtypes: Vec::new(),
        };
        obj.mana_cost = ManaCost::NoCost;
        oid
    }

    fn push_ability(state: &mut GameState, oid: ObjectId, ability: AbilityDefinition) {
        Arc::make_mut(&mut state.objects.get_mut(&oid).unwrap().abilities).push(ability);
    }

    fn add_modal_spell_face(state: &mut GameState, object_id: ObjectId) {
        let object = state.objects.get_mut(&object_id).expect("hand card");
        let mut back_face = snapshot_object_face(object);
        back_face.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        };
        back_face.mana_cost = ManaCost::generic(2);
        back_face.layout_kind = Some(LayoutKind::Modal);
        object.back_face = Some(back_face);
    }

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
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability
    }

    fn counterspell_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
        )
    }

    fn tutor_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                source_zones: vec![Zone::Library],
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Card)),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: Default::default(),
                split: None,
            },
        )
    }

    #[test]
    fn not_applicable_when_not_cedh() {
        let policy = CedhKeepablesMulligan::new();
        let score = policy.evaluate(
            &[],
            &make_state(),
            &features_cedh(false),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, .. } => assert_eq!(delta, 0.0),
            _ => panic!("expected zero-delta Score, got {score:?}"),
        }
    }

    #[test]
    fn empty_hand_is_cedh_force_mulligan_too_few_lands() {
        let policy = CedhKeepablesMulligan::new();
        let score = policy.evaluate(
            &[],
            &make_state(),
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            matches!(score, MulliganScore::ForceMulligan { .. }),
            "empty cEDH hand must be a ForceMulligan (< 2 lands), got {score:?}"
        );
    }

    /// Third ForceMulligan branch: 2-4 lands, but no fast-mana, no tutor, no
    /// interaction. Even though the land count is legal, the hand is untenable
    /// at a cEDH table without any clock or disruption piece.
    #[test]
    fn cedh_hand_with_no_acceleration_tutor_or_interaction_force_mulligans() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        // 3 plain lands, none of which carry fast-mana / tutor / interaction structure.
        let mut hand = Vec::new();
        for i in 0..3 {
            hand.push(add_hand_card(
                &mut state,
                i,
                &format!("Forest {i}"),
                vec![CoreType::Land],
            ));
        }
        // A filler non-staple spell (no match on any staple list).
        hand.push(add_hand_card(
            &mut state,
            10,
            "Grizzly Bears",
            vec![CoreType::Creature],
        ));

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );

        match score {
            MulliganScore::ForceMulligan { reason } => {
                assert_eq!(
                    reason.kind,
                    "cedh_keepables_no_acceleration_tutor_or_interaction",
                    "unexpected reason kind: {}",
                    reason.kind
                );
            }
            _ => panic!(
                "expected ForceMulligan(cedh_keepables_no_acceleration_tutor_or_interaction), got {score:?}"
            ),
        }
    }

    /// Baseline-keep path: 2-4 lands AND at least one fast-mana object passes
    /// the structural gate and yields a positive Score.
    #[test]
    fn cedh_hand_with_fast_mana_baseline_keeps() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        let mut hand = Vec::new();
        // 2 lands.
        for i in 0..2 {
            hand.push(add_hand_card(
                &mut state,
                i,
                &format!("Island {i}"),
                vec![CoreType::Land],
            ));
        }
        let rock = add_hand_card(&mut state, 20, "Mana Rock", vec![CoreType::Artifact]);
        push_ability(&mut state, rock, tap_for_mana_ability());
        hand.push(rock);
        // Some filler.
        for i in 0..3 {
            hand.push(add_hand_card(
                &mut state,
                30 + i,
                &format!("Filler {i}"),
                vec![CoreType::Instant],
            ));
        }

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );

        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(
                    (delta - 1.0).abs() < f64::EPSILON,
                    "expected delta 1.0, got {delta}"
                );
                assert_eq!(
                    reason.kind, "cedh_keepables_baseline_keep",
                    "unexpected reason kind: {}",
                    reason.kind
                );
            }
            _ => panic!("expected baseline-keep Score, got {score:?}"),
        }
    }

    #[test]
    fn cedh_hand_with_tutor_baseline_keeps() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        let mut hand = Vec::new();
        for i in 0..3 {
            hand.push(add_hand_card(
                &mut state,
                i,
                &format!("Island {i}"),
                vec![CoreType::Land],
            ));
        }
        let tutor = add_hand_card(&mut state, 20, "Tutor Shape", vec![CoreType::Sorcery]);
        push_ability(&mut state, tutor, tutor_ability());
        hand.push(tutor);

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );

        assert!(
            matches!(score, MulliganScore::Score { delta, .. } if (delta - 1.0).abs() < f64::EPSILON),
            "expected structural tutor hand to baseline keep, got {score:?}"
        );
    }

    #[test]
    fn cedh_hand_with_interaction_baseline_keeps() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        let mut hand = Vec::new();
        for i in 0..3 {
            hand.push(add_hand_card(
                &mut state,
                i,
                &format!("Island {i}"),
                vec![CoreType::Land],
            ));
        }
        let counter = add_hand_card(&mut state, 20, "Counter Shape", vec![CoreType::Instant]);
        push_ability(&mut state, counter, counterspell_ability());
        hand.push(counter);

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );

        assert!(
            matches!(score, MulliganScore::Score { delta, .. } if (delta - 1.0).abs() < f64::EPSILON),
            "expected structural counterspell hand to baseline keep, got {score:?}"
        );
    }

    /// Second ForceMulligan branch: > 4 lands. Too land-heavy for a cEDH list;
    /// threat/combo density is diluted.
    #[test]
    fn cedh_hand_too_many_lands_force_mulligans() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        let mut hand = Vec::new();
        // 5 lands — over the high threshold.
        for i in 0..5 {
            hand.push(add_hand_card(
                &mut state,
                i,
                &format!("Forest {i}"),
                vec![CoreType::Land],
            ));
        }
        // Some filler non-land cards.
        for i in 0..2 {
            hand.push(add_hand_card(
                &mut state,
                10 + i,
                &format!("Filler {i}"),
                vec![CoreType::Creature],
            ));
        }

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );

        match score {
            MulliganScore::ForceMulligan { reason } => {
                assert_eq!(
                    reason.kind, "cedh_keepables_too_many_lands",
                    "unexpected reason kind: {}",
                    reason.kind
                );
            }
            _ => panic!("expected ForceMulligan(cedh_keepables_too_many_lands), got {score:?}"),
        }
    }

    #[test]
    fn flexible_modal_lands_do_not_trigger_cedh_too_many_lands_mulligan() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        let mut hand = Vec::new();
        for i in 0..5 {
            let land = add_hand_card(
                &mut state,
                i,
                &format!("Modal land {i}"),
                vec![CoreType::Land],
            );
            add_modal_spell_face(&mut state, land);
            hand.push(land);
        }
        let counter = add_hand_card(&mut state, 20, "Counter Shape", vec![CoreType::Instant]);
        push_ability(&mut state, counter, counterspell_ability());
        hand.push(counter);

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(matches!(
            score,
            MulliganScore::Score { reason, .. } if reason.kind == "cedh_keepables_baseline_keep"
        ));
    }

    /// Combo-in-hand path: hand contains both Thassa's Oracle and Demonic
    /// Consultation. Returns a strong-keep Score with the `combo_in_hand`
    /// reason, even though the hand has no fast-mana / tutor / interaction
    /// staple.
    #[test]
    fn cedh_hand_with_complete_combo_returns_strong_keep() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        let mut hand = Vec::new();
        for i in 0..3 {
            hand.push(add_hand_card(
                &mut state,
                i,
                &format!("Island {i}"),
                vec![CoreType::Land],
            ));
        }
        hand.push(add_hand_card(
            &mut state,
            10,
            "Thassa's Oracle",
            vec![CoreType::Creature],
        ));
        hand.push(add_hand_card(
            &mut state,
            11,
            "Demonic Consultation",
            vec![CoreType::Instant],
        ));

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );

        match score {
            MulliganScore::Score { delta, reason } => {
                assert!(
                    (delta - 5.0).abs() < f64::EPSILON,
                    "expected delta 5.0 for combo-in-hand, got {delta}"
                );
                assert_eq!(reason.kind, "cedh_keepables_combo_in_hand");
            }
            _ => panic!("expected strong-keep Score, got {score:?}"),
        }
    }

    /// Build a minimal bad cEDH hand (fewer than 2 lands — a `ForceMulligan` on
    /// the land-count branch for a cEDH deck).
    fn make_bad_cedh_hand(state: &mut GameState) -> Vec<ObjectId> {
        state.players[0].hand.clear();
        let mut hand = Vec::new();
        // 1 land (< 2 → force-mulligan on land count).
        hand.push(add_hand_card(state, 50, "Island", vec![CoreType::Land]));
        // Filler — no fast-mana / tutor / interaction.
        for i in 0..3 {
            hand.push(add_hand_card(
                state,
                60 + i,
                &format!("Filler {i}"),
                vec![CoreType::Creature],
            ));
        }
        hand
    }

    /// `CedhKeepablesMulligan` stays archetype-scoped: non-cEDH decks get a
    /// zero-delta Score at any `mulligans_taken`. The universal card-count floor
    /// lives in `policies::mulligan::card_floor::MulliganCardFloor`, not here —
    /// this policy must never grow a process-level responsibility again.
    #[test]
    fn non_cedh_unaffected_at_high_mulligan_count() {
        let policy = CedhKeepablesMulligan::new();
        let mut state = GameState::new_two_player(42);
        let hand = make_bad_cedh_hand(&mut state);

        let score = policy.evaluate(
            &hand,
            &state,
            &features_cedh(false),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            5,
        );

        match score {
            MulliganScore::Score { delta, .. } => {
                assert_eq!(delta, 0.0, "non-cEDH bracket must return zero-delta Score")
            }
            _ => panic!(
                "expected zero-delta Score for non-cEDH deck at high mulligan count, got {score:?}"
            ),
        }
    }
}
