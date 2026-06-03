use serde::{Deserialize, Serialize};

/// Represents the phases and steps of a turn (CR 500.1).
///
/// A turn consists of five phases: beginning, precombat main, combat,
/// postcombat main, and ending. The beginning, combat, and ending phases
/// are further broken down into steps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
    // --- Beginning phase (CR 501.1): untap, upkeep, draw ---
    /// CR 502: Untap step. No player receives priority (CR 502.4).
    #[default]
    Untap,
    /// CR 503: Upkeep step. Active player gets priority after triggered abilities are put on stack.
    Upkeep,
    /// CR 504: Draw step. Active player draws a card as a turn-based action (CR 504.1).
    Draw,

    // --- Main phase (CR 505) ---
    /// CR 505.1: Precombat main phase. Players may cast non-instant spells (CR 505.6a).
    PreCombatMain,

    // --- Combat phase (CR 506): five steps ---
    /// CR 507: Beginning of combat step.
    BeginCombat,
    /// CR 508: Declare attackers step. Active player declares attackers (CR 508.1).
    DeclareAttackers,
    /// CR 509: Declare blockers step. Defending player declares blockers (CR 509.1).
    DeclareBlockers,
    /// CR 510: Combat damage step. Attacking/blocking creatures assign and deal damage.
    CombatDamage,
    /// CR 511: End of combat step. "At end of combat" triggered abilities trigger.
    EndCombat,

    // --- Postcombat main phase (CR 505.1) ---
    /// CR 505.1: Postcombat main phase. Follows the combat phase.
    PostCombatMain,

    // --- Ending phase (CR 512): end step + cleanup step ---
    /// CR 513: End step. "At the beginning of the end step" abilities trigger.
    End,
    /// CR 514: Cleanup step. Active player discards to hand size, damage is removed (CR 514.1).
    Cleanup,
}

impl Phase {
    /// CR 506.1: The combat phase has five steps: beginning of combat, declare
    /// attackers, declare blockers, combat damage, and end of combat.
    pub fn is_combat(self) -> bool {
        matches!(
            self,
            Phase::BeginCombat
                | Phase::DeclareAttackers
                | Phase::DeclareBlockers
                | Phase::CombatDamage
                | Phase::EndCombat
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_covers_all_mtg_turn_phases() {
        let phases = [
            Phase::Untap,
            Phase::Upkeep,
            Phase::Draw,
            Phase::PreCombatMain,
            Phase::BeginCombat,
            Phase::DeclareAttackers,
            Phase::DeclareBlockers,
            Phase::CombatDamage,
            Phase::EndCombat,
            Phase::PostCombatMain,
            Phase::End,
            Phase::Cleanup,
        ];
        assert_eq!(phases.len(), 12);
    }

    #[test]
    fn phase_serializes_as_string() {
        let phase = Phase::PreCombatMain;
        let json = serde_json::to_value(phase).unwrap();
        assert_eq!(json, "PreCombatMain");
    }

    #[test]
    fn phase_default_is_untap() {
        assert_eq!(Phase::default(), Phase::Untap);
    }

    #[test]
    fn phase_roundtrips() {
        let phase = Phase::CombatDamage;
        let serialized = serde_json::to_string(&phase).unwrap();
        let deserialized: Phase = serde_json::from_str(&serialized).unwrap();
        assert_eq!(phase, deserialized);
    }
}
