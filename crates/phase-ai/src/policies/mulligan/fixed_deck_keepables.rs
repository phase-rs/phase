//! `FixedDeckKeepMulligan` — force-keep for engine-supplied fixed decks.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): a player mulligans to find a
//! workable opening hand. That premise assumes a varied deck where some hands
//! are better than others. The Momir family of formats inverts it: the engine
//! supplies a fixed 60-card all-basic-land deck (`FormatConfig::
//! supplies_fixed_deck`) and the entire game plan is the command-zone emblem
//! (`{X}, Discard a card: create a random creature token`). Every legal hand is
//! seven lands, and one seven-land hand is as good as any other — there is
//! nothing to mulligan *toward*.
//!
//! Without this force-keep, the deck-agnostic `KeepablesByLandCount` policy
//! reads an all-land / no-spell hand as unkeepable, force-mulligans every
//! redraw to the maximum (CR 103.5 final sentence), and bottoms the AI down to
//! a zero-card opening hand. This policy emits `ForceKeep`, which outranks every
//! `ForceMulligan` in the registry's three-way precedence, whenever the format
//! supplies a fixed deck. Non-fixed-deck formats abstain with a neutral
//! additive score, exactly as the archetype keepables do when not applicable.

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
        _turn_order: TurnOrder, // input-unused: every fixed-deck hand is equivalent
        _mulligans_taken: u8, // input-unused: a fixed all-land deck is always kept
    ) -> MulliganScore {
        if state.format_config.supplies_fixed_deck {
            MulliganScore::ForceKeep {
                reason: PolicyReason::new("fixed_deck_force_keep")
                    .with_fact("supplies_fixed_deck", 1),
            }
        } else {
            MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("fixed_deck_not_applicable")
                    .with_fact("supplies_fixed_deck", 0),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::format::FormatConfig;
    use engine::types::game_state::GameState;

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
            "Momir (supplies_fixed_deck) must ForceKeep, got {score:?}"
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
