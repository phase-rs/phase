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
}
