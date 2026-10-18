//! `FixedDeckKeepMulligan` — force-keep when every opening hand is equivalent.
//!
//! CR 103.5: a player mulligans to find a workable opening hand, which only
//! matters when hands differ. The Momir family inverts that: the engine
//! supplies a fixed 60-card all-basic-land deck and the entire game plan is the
//! command-zone emblem (`{X}, Discard a card: create a random creature token`),
//! so every legal hand is seven lands and there is nothing to mulligan *toward*.
//! The format declares this through `GameFormat::opening_hand_equivalence`;
//! a fixed deck with a varied pile (Dandan) is `Distinguishable` and is judged
//! by the archetype policies.
//!
//! Without this force-keep, the deck-agnostic `KeepablesByLandCount` policy
//! reads an all-land / no-spell hand as unkeepable, force-mulligans every
//! redraw to the maximum (CR 103.5 final sentence), and bottoms the AI down to
//! a zero-card opening hand. This policy emits `ForceKeep`, which outranks every
//! `ForceMulligan` in the registry's three-way precedence, whenever the format
//! declares its opening hands equivalent. Other formats abstain with a neutral
//! additive score, exactly as the archetype keepables do when not applicable.

use engine::types::format::OpeningHandEquivalence;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

pub struct FixedDeckKeepMulligan;

impl MulliganPolicy for FixedDeckKeepMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::FixedDeckKeepMulligan
    }

    fn evaluate(
        &self,
        _hand: &[ObjectId],
        state: &GameState,
        _features: &DeckFeatures,
        _plan: &PlanSnapshot, // input-unused: the keep decision depends only on the format
        _turn_order: TurnOrder, // input-unused: an equivalent-hands format has one hand's worth of information
        _mulligans_taken: u8,   // input-unused: equivalent hands are always kept
    ) -> MulliganScore {
        match state.format_config.format.opening_hand_equivalence() {
            OpeningHandEquivalence::Equivalent => MulliganScore::ForceKeep {
                reason: PolicyReason::new("fixed_deck_force_keep")
                    .with_fact("opening_hand_equivalent", 1),
            },
            OpeningHandEquivalence::Distinguishable => MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("fixed_deck_not_applicable")
                    .with_fact("opening_hand_equivalent", 0),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::format::{FormatConfig, GameFormat, OpeningHandEquivalence};
    use engine::types::game_state::GameState;
    use strum::IntoEnumIterator;

    fn evaluate_for(config: FormatConfig) -> MulliganScore {
        let mut state = GameState::new_two_player(0);
        state.format_config = config;
        FixedDeckKeepMulligan.evaluate(
            &[],
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        )
    }

    /// Dandan supplies its deck but the 80-card pile is varied, so a mulligan
    /// can improve a hand and the policy must not force-keep.
    #[test]
    fn dandan_format_abstains() {
        let config = FormatConfig::dandan();
        assert!(
            config.supplies_fixed_deck,
            "Dandan supplies its deck; the abstention must come from the axis, not the flag"
        );
        match evaluate_for(config) {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.facts, vec![("opening_hand_equivalent", 0)]);
            }
            other => panic!("expected neutral Score for Dandan, got {other:?}"),
        }
    }

    /// The gate follows the format's hand-equivalence axis for every format.
    #[test]
    fn force_keep_iff_format_declares_equivalent_hands() {
        let (mut keeping, mut abstaining) = (0, 0);
        for format in GameFormat::iter() {
            let config = FormatConfig::for_format(format)
                .expect("every enumerated GameFormat has a built-in config");
            let kept = matches!(evaluate_for(config), MulliganScore::ForceKeep { .. });
            let equivalent =
                format.opening_hand_equivalence() == OpeningHandEquivalence::Equivalent;
            assert_eq!(kept, equivalent, "{format:?}");
            if kept {
                keeping += 1;
            } else {
                abstaining += 1;
            }
        }
        assert!(keeping > 0 && abstaining > 0);
    }

    /// A fixed-deck format (Momir) must force-keep regardless of hand contents.
    #[test]
    fn fixed_deck_format_force_keeps() {
        let mut state = GameState::new_two_player(0);
        state.format_config = FormatConfig::momir();
        let score = FixedDeckKeepMulligan.evaluate(
            &[],
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            matches!(score, MulliganScore::ForceKeep { .. }),
            "Momir (equivalent opening hands) must ForceKeep, got {score:?}"
        );
    }

    /// A normal constructed format must abstain (neutral score), leaving the
    /// keep/mulligan decision to the archetype policies.
    #[test]
    fn non_fixed_deck_format_abstains() {
        let state = GameState::new_two_player(0);
        let score = FixedDeckKeepMulligan.evaluate(
            &[],
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, .. } => assert_eq!(delta, 0.0),
            other => panic!("expected neutral Score for non-fixed-deck format, got {other:?}"),
        }
    }
}
