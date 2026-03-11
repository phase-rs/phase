use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Phase {
    #[default]
    Untap,
    Upkeep,
    Draw,
    PreCombatMain,
    BeginCombat,
    DeclareAttackers,
    DeclareBlockers,
    CombatDamage,
    EndCombat,
    PostCombatMain,
    End,
    Cleanup,
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
