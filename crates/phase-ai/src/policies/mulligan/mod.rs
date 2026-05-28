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
//! Each `MulliganPolicy` returns a `MulliganScore` — `ForceKeep`, `ForceMulligan`
//! (hard veto), or `Score { delta, reason }` (additive). The registry runs all
//! registered policies and aggregates with three-way precedence:
//!
//! - Any `ForceKeep` → keep (overrides every other verdict including `ForceMulligan`).
//! - Otherwise any `ForceMulligan` → the hand is mulliganed (reason kept in trace).
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
pub mod cedh_keepables;
pub mod keepables_by_land_count;
pub mod landfall_keepables;
pub mod plus_one_counters_keepables;
pub mod ramp_keepables;
pub mod spellslinger_keepables;
pub mod tokens_wide_keepables;
pub mod tribal_density;

pub use aggro_keepables::AggroKeepablesMulligan;
pub use aristocrats_keepables::AristocratsKeepablesMulligan;
pub use cedh_keepables::CedhKeepablesMulligan;
pub use keepables_by_land_count::KeepablesByLandCount;
pub use landfall_keepables::LandfallKeepablesMulligan;
pub use plus_one_counters_keepables::PlusOneCountersMulligan;
pub use ramp_keepables::RampKeepablesMulligan;
pub use spellslinger_keepables::SpellslingerKeepablesMulligan;
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
    /// Hard veto toward keeping — outranks `ForceMulligan`. A policy emits this
    /// when the hand must not be mulliganed regardless of other verdicts
    /// (e.g. a card-count floor).
    ForceKeep { reason: PolicyReason },
    /// Hard veto — if any policy returns this (and none returns `ForceKeep`),
    /// the hand is mulliganed.
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
/// single `MulliganDecision` with three-way precedence:
/// any `ForceKeep` → keep (overrides everything); else any `ForceMulligan` →
/// mulligan; else `sum(delta) > 0.0` → keep.
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
                Box::new(SpellslingerKeepablesMulligan),
                Box::new(CedhKeepablesMulligan::new()),
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
        let mut forced_keep = false;
        let mut forced_mulligan = false;
        let mut total: f64 = 0.0;
        for policy in &self.policies {
            let score = policy.evaluate(hand, state, features, plan, turn_order, mulligans_taken);
            match &score {
                MulliganScore::ForceKeep { .. } => forced_keep = true,
                MulliganScore::ForceMulligan { .. } => forced_mulligan = true,
                MulliganScore::Score { delta, .. } => total += *delta,
            }
            trace.push((policy.id(), score));
        }

        let keep = if forced_keep {
            true
        } else if forced_mulligan {
            false
        } else {
            total > 0.0
        };

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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod cedh_registration_tests {
    use engine::game::bracket_estimate::CommanderBracketTier;

    use super::*;
    use crate::features::DeckFeatures;
    use crate::plan::PlanSnapshot;
    use crate::policies::registry::PolicyId;

    #[test]
    fn default_registry_contains_cedh_keepables() {
        let reg = MulliganRegistry::default();
        let has = reg
            .policies
            .iter()
            .any(|p| p.id() == PolicyId::CedhKeepablesMulligan);
        assert!(
            has,
            "MulliganRegistry::default() must register CedhKeepablesMulligan"
        );
    }

    /// Minimal policy that always emits `ForceKeep`.
    struct AlwaysForceKeep;
    impl MulliganPolicy for AlwaysForceKeep {
        fn id(&self) -> PolicyId {
            PolicyId::CedhKeepablesMulligan
        }
        fn evaluate(
            &self,
            _hand: &[engine::types::identifiers::ObjectId],
            _state: &GameState,
            _features: &DeckFeatures,
            _plan: &PlanSnapshot,
            _turn_order: TurnOrder,
            _mulligans_taken: u8,
        ) -> MulliganScore {
            MulliganScore::ForceKeep {
                reason: PolicyReason::new("test_force_keep"),
            }
        }
    }

    /// Minimal policy that always emits `ForceMulligan`.
    struct AlwaysForceMulligan;
    impl MulliganPolicy for AlwaysForceMulligan {
        fn id(&self) -> PolicyId {
            PolicyId::KeepablesByLandCount
        }
        fn evaluate(
            &self,
            _hand: &[engine::types::identifiers::ObjectId],
            _state: &GameState,
            _features: &DeckFeatures,
            _plan: &PlanSnapshot,
            _turn_order: TurnOrder,
            _mulligans_taken: u8,
        ) -> MulliganScore {
            MulliganScore::ForceMulligan {
                reason: PolicyReason::new("test_force_mulligan"),
            }
        }
    }

    /// `ForceKeep` must override a co-occurring `ForceMulligan` — the hand is kept.
    #[test]
    fn force_keep_overrides_force_mulligan() {
        let registry = MulliganRegistry {
            policies: vec![Box::new(AlwaysForceKeep), Box::new(AlwaysForceMulligan)],
        };
        let state = GameState::new_two_player(0);
        let decision = registry.evaluate_hand(
            &[],
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            decision.keep,
            "ForceKeep must override ForceMulligan; expected keep=true, got keep=false"
        );
    }

    /// Without `ForceKeep`, a lone `ForceMulligan` produces `keep=false`.
    #[test]
    fn force_mulligan_alone_produces_mulligan() {
        let registry = MulliganRegistry {
            policies: vec![Box::new(AlwaysForceMulligan)],
        };
        let state = GameState::new_two_player(0);
        let decision = registry.evaluate_hand(
            &[],
            &state,
            &DeckFeatures::default(),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            !decision.keep,
            "ForceMulligan without ForceKeep must produce keep=false"
        );
    }

    /// End-to-end: the REAL `CedhKeepablesMulligan` floor must override a real
    /// `ForceMulligan` through the registry. This exercises the actual cEDH
    /// policy (not a synthetic `ForceKeep` stub) so the floor's `ForceKeep`
    /// wins the three-way aggregation — the whole point of the feature.
    #[test]
    fn cedh_floor_force_keep_overrides_force_mulligan_in_registry() {
        let cedh_features = DeckFeatures {
            bracket_tier: CommanderBracketTier::Cedh,
            ..DeckFeatures::default()
        };
        // Default `waiting_for` → free_first = false, so the floor engages at
        // mulligans_taken == 3 (`kept_hand_size_after(4, false) == 3 < 4`). An
        // empty hand is fine — the floor check runs before the land-count branch.
        let state = GameState::new_two_player(0);

        let registry = MulliganRegistry {
            policies: vec![
                Box::new(CedhKeepablesMulligan::new()),
                Box::new(AlwaysForceMulligan),
            ],
        };
        let decision = registry.evaluate_hand(
            &[],
            &state,
            &cedh_features,
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            3,
        );
        assert!(
            decision.keep,
            "real cEDH floor ForceKeep must override a real ForceMulligan; \
             expected keep=true at mulligans_taken=3, got keep=false"
        );

        // Contrast: at mulligans_taken == 0 the floor is not engaged, so the
        // real cEDH policy force-mulligans the empty hand (< 2 lands) and the
        // registry mulligans.
        let decision_no_floor = registry.evaluate_hand(
            &[],
            &state,
            &cedh_features,
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(
            !decision_no_floor.keep,
            "without the floor engaged, the real cEDH policy must mulligan; \
             expected keep=false at mulligans_taken=0, got keep=true"
        );
    }
}
