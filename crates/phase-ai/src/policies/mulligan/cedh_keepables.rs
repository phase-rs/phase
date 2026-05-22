//! `CedhKeepablesMulligan` — stub aggressive mulligan policy for cEDH decks.
//! Gated internally on `features.is_cedh` (`MulliganPolicy` has no `activation()`
//! method; every registered policy is consulted on every hand).
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): deciding to keep after the
//! mulligan process.
//!
//! Real cEDH mulligan strategy ("keep only hands that win or stop the opponent
//! from winning by turn 4") lands when the `ComboRegistry` is populated and
//! the policy can ask `ComboRegistry::reachable_lines(hand_pseudo_state)`.
//!
//! **Stub heuristics** — all card classification here is name-based against a
//! small static staple set. This is explicitly documented as a stub; it is
//! refined to card-data feature tags once the right tag names are confirmed.

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

pub struct CedhKeepablesMulligan;

impl MulliganPolicy for CedhKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::CedhKeepablesMulligan
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
        // Internal gate: non-cEDH decks see a zero-delta Score (cheap no-op).
        if !features.is_cedh {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("cedh_keepables_na"),
            };
        }

        let land_count = count_lands_in_hand(hand, state);

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
        if land_count > 4 {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("cedh_keepables_too_many_lands")
                    .with_fact("lands", land_count as i64),
            };
        }

        let has_fast_mana = hand_has_any(hand, state, is_fast_mana_card);
        let has_tutor = hand_has_any(hand, state, is_tutor_card);
        let has_interaction = hand_has_any(hand, state, is_interaction_card);

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
        .filter(|&&id| {
            state
                .objects
                .get(&id)
                .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Land))
        })
        .count() as u32
}

fn hand_has_any<F>(hand: &[ObjectId], state: &GameState, pred: F) -> bool
where
    F: Fn(&str) -> bool,
{
    hand.iter()
        .any(|&id| state.objects.get(&id).is_some_and(|obj| pred(&obj.name)))
}

/// Canonical cEDH fast-mana staple set — stub heuristic.
/// Replace with card-data feature tag lookups once the tag name is confirmed.
fn is_fast_mana_card(name: &str) -> bool {
    matches!(
        name,
        "Sol Ring"
            | "Mana Crypt"
            | "Mox Diamond"
            | "Chrome Mox"
            | "Mana Vault"
            | "Jeweled Lotus"
            | "Lotus Petal"
            | "Dark Ritual"
    )
}

/// Canonical cEDH tutor staple set — stub heuristic.
fn is_tutor_card(name: &str) -> bool {
    matches!(
        name,
        "Demonic Tutor"
            | "Vampiric Tutor"
            | "Mystical Tutor"
            | "Enlightened Tutor"
            | "Worldly Tutor"
            | "Imperial Seal"
            | "Grim Tutor"
    )
}

/// Canonical cEDH interaction staple set — stub heuristic.
fn is_interaction_card(name: &str) -> bool {
    matches!(
        name,
        "Force of Will"
            | "Force of Negation"
            | "Mana Drain"
            | "Counterspell"
            | "Swan Song"
            | "Mindbreak Trap"
            | "Pact of Negation"
    )
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use engine::game::zones::create_object;
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
            is_cedh,
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

    #[test]
    fn not_applicable_when_not_cedh() {
        let policy = CedhKeepablesMulligan;
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
        let policy = CedhKeepablesMulligan;
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
        let policy = CedhKeepablesMulligan;
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.clear();

        // 3 plain lands, none of which match any fast-mana / tutor / interaction name.
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

    /// Baseline-keep path: 2-4 lands AND at least one fast-mana card
    /// (Sol Ring) passes the gate and yields a positive Score.
    #[test]
    fn cedh_hand_with_fast_mana_baseline_keeps() {
        let policy = CedhKeepablesMulligan;
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
        // Sol Ring is a canonical fast-mana staple — must trigger the keep path.
        hand.push(add_hand_card(
            &mut state,
            20,
            "Sol Ring",
            vec![CoreType::Artifact],
        ));
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

    /// Second ForceMulligan branch: > 4 lands. Too land-heavy for a cEDH list;
    /// threat/combo density is diluted.
    #[test]
    fn cedh_hand_too_many_lands_force_mulligans() {
        let policy = CedhKeepablesMulligan;
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
}
