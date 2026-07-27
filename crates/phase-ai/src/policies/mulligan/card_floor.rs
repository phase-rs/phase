//! `MulliganCardFloor` — the single authority for the AI's minimum kept-hand
//! size. Applies to every deck in every format.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`) defines the mulligan process and,
//! in its final sentence, permits a player to "take mulligans until their
//! opening hand would be zero cards." The rules therefore impose NO minimum
//! kept-hand size above zero — the floor below is an AI *strategy* heuristic
//! bounding that process, not a rules requirement. CR 103.5c
//! (`docs/MagicCompRules.txt:301`) supplies the free-first-mulligan discount,
//! consumed here via `kept_hand_size_after`.
//!
//! Without this floor an ordinary deck can chain-mulligan toward a zero-card
//! opening hand by two independent routes:
//!   1. an unbounded `ForceMulligan` from `KeepablesByLandCount`, and
//!   2. a negative additive total from the archetype policies, none of which
//!      consult `mulligans_taken` — so the acceptance threshold never relaxes
//!      with depth and the mulligan chain is geometric, with a fat tail at the
//!      engine's `MAX_MULLIGANS` cap.
//!
//! `ForceKeep` outranks both in `MulliganRegistry`'s three-way precedence, so a
//! single floor closes both routes.
//!
//! # Accepted interaction: Serum Powder
//!
//! CR 103.5b (`docs/MagicCompRules.txt:299`) lets Serum Powder exile the hand
//! and redraw at the same mulligan count. In the band where this floor engages
//! (`mulligans_taken >= 3` normally, `>= 4` with the free-first discount) a
//! floored keep suppresses a Powder activation that the pre-floor code would
//! have taken, because `search.rs` takes the keep whenever `decision.keep` is
//! true.
//!
//! This is **accepted**, and the trade is stated rather than assumed: the Powder
//! is NOT free here. `resolve_declare_point` only takes its zero-cost fast path
//! when `owed == 0`, and a first activation in this band always has
//! `owed >= 3`, so the Powder is routed through `BottomCards` first. The AI
//! therefore trades a *curated* best-N-of-seven (bottomed via
//! `plan_aware_bottom_cards`) for N *random* cards — card-neutral, better for a
//! degenerate hand, worse for a merely-mediocre one. **No expectation analysis
//! has been performed**, so this is neither claimed to be an improvement nor a
//! regression. A second activation from the post-Powder hand *is* free
//! (`owed == 0` once the bottoms are prepaid) and is also suppressed; that is a
//! real if narrow cost. Revisiting either belongs in its own scoped change with
//! a proper expectation analysis, not here.

use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

use super::{MulliganPolicy, MulliganScore, TurnOrder};

/// Minimum kept-hand size the AI will accept. A hand smaller than this cannot
/// realistically execute any deck's game plan, so no mulligan that would leave
/// fewer cards is worth taking. Strategy heuristic — see the module doc; the
/// Comprehensive Rules permit mulliganing all the way to zero. The value is
/// carried over unchanged from the retired `CEDH_MULLIGAN_FLOOR` so cEDH
/// behaviour is preserved exactly.
const MULLIGAN_CARD_FLOOR: usize = 4;

pub struct MulliganCardFloor;

impl MulliganPolicy for MulliganCardFloor {
    fn id(&self) -> PolicyId {
        PolicyId::MulliganCardFloor
    }

    fn evaluate(
        &self,
        _hand: &[ObjectId], // input-unused: the floor is a card-count bound, not a hand-quality judgement
        state: &GameState,
        _features: &DeckFeatures, // input-unused: the floor is universal, not archetype-scoped
        _plan: &PlanSnapshot, // input-unused: the floor is a card-count bound, not a curve judgement
        _turn_order: TurnOrder, // input-unused: play/draw does not change the minimum viable hand size
        mulligans_taken: u8,
    ) -> MulliganScore {
        // CR 103.5c: the free-first discount is carried on the mulligan step
        // itself. Evaluated outside that step (projection, direct unit calls)
        // there is no mulligan to bound, so abstain rather than guess.
        let WaitingFor::MulliganDecision {
            free_first_mulligan,
            ..
        } = &state.waiting_for
        else {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("mulligan_card_floor_off_step"),
            };
        };
        let free_first = *free_first_mulligan;

        // CR 103.5 + CR 103.5c: `kept_hand_size_after` is the engine's single
        // authority for post-keep hand size; never re-derive it here.
        let kept_after_next =
            engine::game::mulligan::kept_hand_size_after(mulligans_taken + 1, free_first);
        if kept_after_next < MULLIGAN_CARD_FLOOR {
            return MulliganScore::ForceKeep {
                reason: PolicyReason::new("mulligan_card_floor")
                    .with_fact("mulligans_taken", mulligans_taken as i64)
                    .with_fact("kept_after_next", kept_after_next as i64)
                    .with_fact("floor", MULLIGAN_CARD_FLOOR as i64)
                    .with_fact("free_first", i64::from(free_first)),
            };
        }

        MulliganScore::Score {
            delta: 0.0,
            reason: PolicyReason::new("mulligan_card_floor_not_reached")
                .with_fact("kept_after_next", kept_after_next as i64),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use engine::types::game_state::{MulliganDecisionEntry, MulliganDecisionPhase};
    use engine::types::player::PlayerId;

    use super::*;

    /// A `GameState` sitting on the mulligan step, with the given free-first
    /// regime and no pending entries. Every test that expects the floor to
    /// *engage* needs this — the `GameState::new_two_player` default
    /// `waiting_for` is not `MulliganDecision`, so the policy abstains there.
    fn state_on_mulligan_step(free_first_mulligan: bool) -> GameState {
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![],
            free_first_mulligan,
        };
        state
    }

    /// `MulliganCardFloor` never reads the hand (`_hand` is `input-unused`), so
    /// policy-level tests pass an empty slice. Registry-level tests in `mod.rs`
    /// must NOT — `KeepablesByLandCount` does read it.
    fn evaluate(state: &GameState, mulligans_taken: u8) -> MulliganScore {
        MulliganCardFloor.evaluate(
            &[],
            state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            mulligans_taken,
        )
    }

    /// V1 — the floor force-keeps below the threshold. At `mulligans_taken = 4`
    /// non-free-first, `kept_hand_size_after(5, false) == 2 < 4`, so an ordinary
    /// deck cannot mulligan any further.
    #[test]
    fn floor_force_keeps_below_threshold() {
        let state = state_on_mulligan_step(false);
        let score = evaluate(&state, 4);
        match score {
            MulliganScore::ForceKeep { reason } => assert_eq!(
                reason.kind, "mulligan_card_floor",
                "unexpected reason kind: {}",
                reason.kind
            ),
            other => panic!(
                "floor must ForceKeep at mulligans_taken=4 (kept_hand_size_after(5,false)==2), got {other:?}"
            ),
        }
    }

    /// V2 — the floor is a *boundary*, not an unconditional keep. At
    /// `mulligans_taken = 2`, `kept_hand_size_after(3, false) == 4`, which is
    /// NOT `< 4`. Asserting the reason kind discriminates a legitimate
    /// on-step abstention from the off-step one (V4), and an implementation
    /// using `<=` instead of `<` fails this row.
    #[test]
    fn floor_abstains_at_and_above_threshold() {
        let state = state_on_mulligan_step(false);
        match evaluate(&state, 2) {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0, "abstention must be delta-neutral");
                assert_eq!(
                    reason.kind, "mulligan_card_floor_not_reached",
                    "unexpected reason kind: {}",
                    reason.kind
                );
            }
            other => panic!("floor must abstain at mulligans_taken=2, got {other:?}"),
        }
    }

    /// V3 — CR 103.5c's free-first discount shifts the floor by exactly one.
    /// With `free_first_mulligan: true`, `kept_hand_size_after(5, true) == 3`
    /// (fires at `m = 4`) and `kept_hand_size_after(4, true) == 4` (abstains at
    /// `m = 3`). Both halves share one state, so the `ForceKeep` half proves the
    /// fixture is genuinely on-step for the abstain half too.
    ///
    /// This is the ONLY coverage of the free-first arm: `ai-gate` is
    /// structurally two-player 60-card, so it never exercises CR 103.5c.
    #[test]
    fn floor_respects_free_first_discount() {
        let state = state_on_mulligan_step(true);

        match evaluate(&state, 4) {
            MulliganScore::ForceKeep { reason } => assert_eq!(
                reason.kind, "mulligan_card_floor",
                "unexpected reason kind: {}",
                reason.kind
            ),
            other => panic!(
                "free-first floor must ForceKeep at mulligans_taken=4 (kept_hand_size_after(5,true)==3), got {other:?}"
            ),
        }

        match evaluate(&state, 3) {
            MulliganScore::Score { reason, .. } => assert_eq!(
                reason.kind, "mulligan_card_floor_not_reached",
                "free-first floor must NOT engage at mulligans_taken=3 \
                 (kept_hand_size_after(4,true)==4); unexpected reason kind: {}",
                reason.kind
            ),
            other => panic!("free-first floor must abstain at mulligans_taken=3, got {other:?}"),
        }
    }

    /// V4 — the floor abstains outside the mulligan step. This is the one test
    /// that deliberately does NOT set `waiting_for`: the
    /// `GameState::new_two_player` default *is* the fixture. Off-step there is
    /// no mulligan to bound, so force-keeping a projection state would be a
    /// guess. (This is also the one observable difference from the
    /// `CedhKeepablesMulligan` floor this policy replaces, which fell back to
    /// `free_first = false` and evaluated the floor anyway.)
    #[test]
    fn floor_abstains_outside_mulligan_step() {
        let state = GameState::new_two_player(0);
        match evaluate(&state, 6) {
            MulliganScore::Score { delta, reason } => {
                assert_eq!(delta, 0.0, "off-step abstention must be delta-neutral");
                assert_eq!(
                    reason.kind, "mulligan_card_floor_off_step",
                    "unexpected reason kind: {}",
                    reason.kind
                );
            }
            other => panic!(
                "floor must abstain off-step even at mulligans_taken=6, never ForceKeep; got {other:?}"
            ),
        }
    }

    /// V9 — multi-authority: the floor bounds *this* player's mulligan count,
    /// which arrives as the `mulligans_taken` parameter (`search.rs` sources it
    /// from the `MulliganDecisionEntry` matching the AI player). Both halves run
    /// against the SAME state, whose `pending` head is a different seat at
    /// `mulligan_count = 0`, so a future refactor reading `pending[0]` off the
    /// state instead of the parameter fails here. `pending[0]`-shaped bugs are
    /// real in this repo (`server-core/src/session.rs`).
    #[test]
    fn floor_uses_caller_supplied_count_not_pending_head() {
        let mut state = GameState::new_two_player(0);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![
                MulliganDecisionEntry {
                    player: PlayerId(0),
                    mulligan_count: 0,
                    phase: MulliganDecisionPhase::Declare,
                },
                MulliganDecisionEntry {
                    player: PlayerId(1),
                    mulligan_count: 4,
                    phase: MulliganDecisionPhase::Declare,
                },
            ],
            free_first_mulligan: false,
        };

        assert!(
            matches!(evaluate(&state, 4), MulliganScore::ForceKeep { .. }),
            "floor must read the caller-supplied mulligans_taken=4, not pending[0].mulligan_count=0"
        );
        assert!(
            matches!(
                evaluate(&state, 0),
                MulliganScore::Score { reason, .. } if reason.kind == "mulligan_card_floor_not_reached"
            ),
            "floor must abstain at caller-supplied mulligans_taken=0, not read pending[1].mulligan_count=4"
        );
    }
}
