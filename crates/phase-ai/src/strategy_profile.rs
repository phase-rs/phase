use crate::deck_profile::DeckArchetype;

/// Per-archetype behavioral modifiers applied on top of difficulty-based `AiProfile`.
/// These compose multiplicatively: archetype modulates the base, difficulty modulates execution.
///
/// Separated from `deck_profile.rs` per single-responsibility: `DeckProfile` handles deck
/// composition analysis; `StrategyProfile` handles behavioral modulation.
#[derive(Debug, Clone)]
pub struct StrategyProfile {
    /// Multiplier on `AiProfile.risk_tolerance` (>1 = more aggressive).
    pub risk_tolerance_mult: f64,
    /// Multiplier on `AiProfile.interaction_patience` (>1 = more patient).
    pub interaction_patience_mult: f64,
    /// Multiplier on `AiProfile.stabilize_bias` (>1 = more defensive).
    pub stabilize_bias_mult: f64,
    /// Turn-phase posture: scales policy influence in early game (turns 0-3).
    pub early_game_mult: f64,
    /// Turn-phase posture: scales policy influence in late game (turns 8+).
    pub late_game_mult: f64,
}

impl StrategyProfile {
    /// Build a strategy profile for a pure archetype.
    /// Uses exhaustive match — compiler enforces coverage when new archetypes are added.
    pub fn for_archetype(archetype: DeckArchetype) -> Self {
        match archetype {
            DeckArchetype::Aggro => Self {
                risk_tolerance_mult: 1.3,
                interaction_patience_mult: 0.5,
                stabilize_bias_mult: 0.7,
                early_game_mult: 1.3,
                late_game_mult: 0.7,
            },
            DeckArchetype::Control => Self {
                risk_tolerance_mult: 0.7,
                interaction_patience_mult: 1.5,
                stabilize_bias_mult: 1.3,
                early_game_mult: 0.8,
                late_game_mult: 1.3,
            },
            DeckArchetype::Midrange => Self {
                risk_tolerance_mult: 1.0,
                interaction_patience_mult: 1.0,
                stabilize_bias_mult: 1.0,
                early_game_mult: 1.0,
                late_game_mult: 1.0,
            },
            DeckArchetype::Ramp => Self {
                risk_tolerance_mult: 0.8,
                interaction_patience_mult: 1.2,
                stabilize_bias_mult: 1.1,
                early_game_mult: 1.2,
                late_game_mult: 0.9,
            },
            DeckArchetype::Combo => Self {
                risk_tolerance_mult: 0.9,
                interaction_patience_mult: 1.3,
                stabilize_bias_mult: 1.0,
                early_game_mult: 1.0,
                late_game_mult: 1.0,
            },
        }
    }

    /// Build a strategy profile for a classified deck, supporting hybrid blending.
    pub fn for_profile(profile: &crate::deck_profile::DeckProfile) -> Self {
        match &profile.classification {
            crate::deck_profile::ArchetypeClassification::Pure(arch) => Self::for_archetype(*arch),
            crate::deck_profile::ArchetypeClassification::Hybrid {
                primary,
                primary_weight,
                secondary,
            } => {
                let primary_strat = Self::for_archetype(*primary);
                let secondary_strat = Self::for_archetype(*secondary);
                primary_strat.blend(&secondary_strat, *primary_weight)
            }
        }
    }

    /// Returns a scalar multiplier for policy scores based on the current turn.
    /// Aggro strategies intensify early and decay late; control does the opposite.
    /// Midrange (all 1.0) is unaffected.
    ///
    /// Turn breakpoints: 0-3 = early, 4-7 = mid (neutral 1.0), 8+ = late.
    /// Discrete steps matching `EvalWeightSet::for_turn()` boundaries — no interpolation.
    ///
    /// This adjusts policy scores (tactical action bonuses), complementing
    /// `EvalWeightSet::for_turn()` which adjusts eval weights (board state valuation).
    /// The two operate on orthogonal axes and compose additively.
    pub fn turn_phase_mult(&self, turn: u32) -> f64 {
        match turn {
            0..=3 => self.early_game_mult,
            4..=7 => 1.0,
            _ => self.late_game_mult,
        }
    }

    /// Linear interpolation blend of two strategy profiles.
    /// `self_weight` is the weight for `self` (0.0..=1.0); remainder goes to `other`.
    pub fn blend(&self, other: &Self, self_weight: f64) -> Self {
        let w = self_weight;
        let o = 1.0 - w;
        Self {
            risk_tolerance_mult: self.risk_tolerance_mult * w + other.risk_tolerance_mult * o,
            interaction_patience_mult: self.interaction_patience_mult * w
                + other.interaction_patience_mult * o,
            stabilize_bias_mult: self.stabilize_bias_mult * w + other.stabilize_bias_mult * o,
            early_game_mult: self.early_game_mult * w + other.early_game_mult * o,
            late_game_mult: self.late_game_mult * w + other.late_game_mult * o,
        }
    }
}

impl Default for StrategyProfile {
    /// Neutral profile: all 1.0 multipliers (midrange behavior).
    fn default() -> Self {
        Self {
            risk_tolerance_mult: 1.0,
            interaction_patience_mult: 1.0,
            stabilize_bias_mult: 1.0,
            early_game_mult: 1.0,
            late_game_mult: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiProfile;

    #[test]
    fn aggro_profile_is_aggressive() {
        let profile = StrategyProfile::for_archetype(DeckArchetype::Aggro);
        assert!(
            profile.risk_tolerance_mult > 1.0,
            "Aggro should increase risk tolerance"
        );
        assert!(
            profile.interaction_patience_mult < 1.0,
            "Aggro should decrease patience"
        );
        assert!(
            profile.early_game_mult > 1.0,
            "Aggro should intensify early game"
        );
        assert!(
            profile.late_game_mult < 1.0,
            "Aggro should weaken late game"
        );
    }

    #[test]
    fn control_profile_is_patient() {
        let profile = StrategyProfile::for_archetype(DeckArchetype::Control);
        assert!(
            profile.interaction_patience_mult > 1.0,
            "Control should increase patience"
        );
        assert!(
            profile.stabilize_bias_mult > 1.0,
            "Control should increase stabilize bias"
        );
        assert!(
            profile.risk_tolerance_mult < 1.0,
            "Control should decrease risk tolerance"
        );
        assert!(
            profile.late_game_mult > 1.0,
            "Control should intensify late game"
        );
    }

    #[test]
    fn all_archetypes_covered() {
        // Exhaustive match enforced by compiler, but verify all return non-default
        // (except Midrange which IS the default).
        let archetypes = [
            DeckArchetype::Aggro,
            DeckArchetype::Control,
            DeckArchetype::Midrange,
            DeckArchetype::Ramp,
            DeckArchetype::Combo,
        ];
        for arch in &archetypes {
            let profile = StrategyProfile::for_archetype(*arch);
            // Every archetype should produce a valid profile
            assert!(profile.risk_tolerance_mult > 0.0);
            assert!(profile.interaction_patience_mult > 0.0);
            assert!(profile.stabilize_bias_mult > 0.0);
            assert!(profile.early_game_mult > 0.0);
            assert!(profile.late_game_mult > 0.0);
        }
    }

    #[test]
    fn composition_clamps_correctly() {
        // Test all difficulty x archetype combinations stay in valid ranges
        let profiles = [
            AiProfile {
                risk_tolerance: 0.9,
                interaction_patience: 0.2,
                stabilize_bias: 0.8,
            },
            AiProfile {
                risk_tolerance: 0.45,
                interaction_patience: 1.0,
                stabilize_bias: 1.2,
            },
            AiProfile {
                risk_tolerance: 0.65,
                interaction_patience: 0.7,
                stabilize_bias: 1.0,
            },
        ];
        let archetypes = [
            DeckArchetype::Aggro,
            DeckArchetype::Control,
            DeckArchetype::Midrange,
            DeckArchetype::Ramp,
            DeckArchetype::Combo,
        ];
        for base in &profiles {
            for arch in &archetypes {
                let strategy = StrategyProfile::for_archetype(*arch);
                let effective = base.with_strategy(&strategy);
                assert!(
                    (0.2..=1.0).contains(&effective.risk_tolerance),
                    "risk_tolerance {:.3} out of range for {:?}",
                    effective.risk_tolerance,
                    arch,
                );
                assert!(
                    (0.1..=1.0).contains(&effective.interaction_patience),
                    "interaction_patience {:.3} out of range for {:?}",
                    effective.interaction_patience,
                    arch,
                );
                assert!(
                    (0.5..=2.0).contains(&effective.stabilize_bias),
                    "stabilize_bias {:.3} out of range for {:?}",
                    effective.stabilize_bias,
                    arch,
                );
            }
        }
    }

    #[test]
    fn blend_weighted() {
        let aggro = StrategyProfile::for_archetype(DeckArchetype::Aggro);
        let control = StrategyProfile::for_archetype(DeckArchetype::Control);
        let blended = aggro.blend(&control, 0.5);

        // 50/50 blend should produce intermediate values
        let expected_risk = (1.3 + 0.7) / 2.0;
        assert!(
            (blended.risk_tolerance_mult - expected_risk).abs() < f64::EPSILON,
            "Expected {expected_risk}, got {}",
            blended.risk_tolerance_mult,
        );
        let expected_patience = (0.5 + 1.5) / 2.0;
        assert!(
            (blended.interaction_patience_mult - expected_patience).abs() < f64::EPSILON,
            "Expected {expected_patience}, got {}",
            blended.interaction_patience_mult,
        );
    }

    #[test]
    fn default_is_neutral() {
        let profile = StrategyProfile::default();
        assert!(
            (profile.risk_tolerance_mult - 1.0).abs() < f64::EPSILON,
            "Default risk_tolerance_mult should be 1.0"
        );
        assert!(
            (profile.interaction_patience_mult - 1.0).abs() < f64::EPSILON,
            "Default interaction_patience_mult should be 1.0"
        );
        assert!(
            (profile.stabilize_bias_mult - 1.0).abs() < f64::EPSILON,
            "Default stabilize_bias_mult should be 1.0"
        );
        assert!(
            (profile.early_game_mult - 1.0).abs() < f64::EPSILON,
            "Default early_game_mult should be 1.0"
        );
        assert!(
            (profile.late_game_mult - 1.0).abs() < f64::EPSILON,
            "Default late_game_mult should be 1.0"
        );
    }

    #[test]
    fn turn_phase_mult_early_game() {
        let aggro = StrategyProfile::for_archetype(DeckArchetype::Aggro);
        assert!((aggro.turn_phase_mult(0) - 1.3).abs() < f64::EPSILON);
        assert!((aggro.turn_phase_mult(3) - 1.3).abs() < f64::EPSILON);
    }

    #[test]
    fn turn_phase_mult_mid_game() {
        let aggro = StrategyProfile::for_archetype(DeckArchetype::Aggro);
        assert!((aggro.turn_phase_mult(4) - 1.0).abs() < f64::EPSILON);
        assert!((aggro.turn_phase_mult(7) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn turn_phase_mult_late_game() {
        let aggro = StrategyProfile::for_archetype(DeckArchetype::Aggro);
        assert!((aggro.turn_phase_mult(8) - 0.7).abs() < f64::EPSILON);
        assert!((aggro.turn_phase_mult(15) - 0.7).abs() < f64::EPSILON);
    }
}
