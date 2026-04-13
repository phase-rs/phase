//! Mulligan policies — sibling trait to `TacticalPolicy` for pre-game hand
//! evaluation.
//!
//! CR 103.5 (`docs/MagicCompRules.txt:295`): the mulligan process — each
//! player may take a mulligan; mulliganed hands shuffle back and the player
//! draws a new hand, putting `mulligan_count` cards on the bottom.
//! CR 103.6 (`docs/MagicCompRules.txt:305`): opening-hand actions after the
//! mulligan process is complete (companion reveals, "begin the game with ~"
//! abilities) — not modeled here, but motivates why the mulligan decision
//! is a first-class AI concern.
//!
//! Each `MulliganPolicy` returns a `MulliganScore` — either `ForceMulligan`
//! (hard veto) or `Score { delta, reason }` (additive). The registry runs all
//! registered policies and aggregates:
//!
//! - Any `ForceMulligan` → the hand is mulliganed (reason kept in trace).
//! - Otherwise `sum(delta) > 0.0` means keep.
//!
//! Structured `PolicyReason` values give observability parity with
//! `TacticalPolicy` — `RUST_LOG=phase_ai::decision_trace=debug` emits the
//! per-policy trace.

use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::registry::{PolicyId, PolicyReason};

pub mod aggro_keepables;
pub mod aristocrats_keepables;
pub mod keepables_by_land_count;
pub mod landfall_keepables;
pub mod plus_one_counters_keepables;
pub mod ramp_keepables;
pub mod tokens_wide_keepables;
pub mod tribal_density;

pub use aggro_keepables::AggroKeepablesMulligan;
pub use aristocrats_keepables::AristocratsKeepablesMulligan;
pub use keepables_by_land_count::KeepablesByLandCount;
pub use landfall_keepables::LandfallKeepablesMulligan;
pub use plus_one_counters_keepables::PlusOneCountersMulligan;
pub use ramp_keepables::RampKeepablesMulligan;
pub use tokens_wide_keepables::TokensWideKeepablesMulligan;
pub use tribal_density::TribalDensityMulligan;

/// Whether the player under consideration is on the play or on the draw this
/// game. Derived from `GameState::current_starting_player` at call time —
/// `OnPlay` when the mulliganing player started the game, otherwise `OnDraw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOrder {
    OnPlay,
    OnDraw,
}

/// A single mulligan policy's verdict on an opening hand.
#[derive(Debug, Clone)]
pub enum MulliganScore {
    /// Hard veto — if any policy returns this, the hand is mulliganed.
    ForceMulligan { reason: PolicyReason },
    /// Additive score contribution. Positive = prefer keeping; negative =
    /// prefer mulliganing.
    Score { delta: f64, reason: PolicyReason },
}

/// Aggregated decision produced by `MulliganRegistry::evaluate_hand`.
#[derive(Debug, Clone)]
pub struct MulliganDecision {
    pub keep: bool,
    pub trace: Vec<(PolicyId, MulliganScore)>,
}

/// Pre-game hand evaluation. Shares inputs with `TacticalPolicy` (features,
/// plan) but uses a different scoring interface — mulligan is a one-shot
/// choice, not a ranking over candidates.
pub trait MulliganPolicy: Send + Sync {
    fn id(&self) -> PolicyId;
    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        plan: &PlanSnapshot,
        turn_order: TurnOrder,
        mulligans_taken: u8,
    ) -> MulliganScore;
}

/// Registry of mulligan policies. Aggregates per-policy verdicts into a
/// single `MulliganDecision` per the plan-mandated rule:
/// any `ForceMulligan` → mulligan; otherwise `sum(delta) > 0.0` → keep.
pub struct MulliganRegistry {
    policies: Vec<Box<dyn MulliganPolicy>>,
}

impl Default for MulliganRegistry {
    fn default() -> Self {
        Self {
            policies: vec![
                Box::new(KeepablesByLandCount),
                Box::new(LandfallKeepablesMulligan),
                Box::new(RampKeepablesMulligan),
                Box::new(TribalDensityMulligan),
                Box::new(AristocratsKeepablesMulligan),
                Box::new(AggroKeepablesMulligan),
                Box::new(TokensWideKeepablesMulligan),
                Box::new(PlusOneCountersMulligan),
            ],
        }
    }
}

impl MulliganRegistry {
    pub fn evaluate_hand(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        plan: &PlanSnapshot,
        turn_order: TurnOrder,
        mulligans_taken: u8,
    ) -> MulliganDecision {
        let mut trace = Vec::with_capacity(self.policies.len());
        let mut forced = false;
        let mut total: f64 = 0.0;
        for policy in &self.policies {
            let score = policy.evaluate(hand, state, features, plan, turn_order, mulligans_taken);
            match &score {
                MulliganScore::ForceMulligan { .. } => forced = true,
                MulliganScore::Score { delta, .. } => total += *delta,
            }
            trace.push((policy.id(), score));
        }

        let keep = if forced { false } else { total > 0.0 };

        if tracing::event_enabled!(target: "phase_ai::decision_trace", tracing::Level::DEBUG) {
            tracing::debug!(
                target: "phase_ai::decision_trace",
                ?trace,
                keep,
                mulligans_taken,
                "mulligan decision"
            );
        }

        MulliganDecision { keep, trace }
    }
}

/// Derive `TurnOrder` from the game state for a given player. CR 103.5 —
/// the starting player declares first; subsequent mulligans follow turn
/// order. For the purpose of evaluating hand quality, what matters is
/// whether this player will be on the play (extra tempo, no free draw) or
/// on the draw (free card, slower clock).
pub fn turn_order_for(state: &GameState, player: PlayerId) -> TurnOrder {
    if state.current_starting_player == player {
        TurnOrder::OnPlay
    } else {
        TurnOrder::OnDraw
    }
}
