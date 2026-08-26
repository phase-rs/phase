use super::player::PlayerId;
use serde::{Deserialize, Serialize};

/// Represents the phases and steps of a turn (CR 500.1).
///
/// A turn consists of five phases: beginning, precombat main, combat,
/// postcombat main, and ending. The beginning, combat, and ending phases
/// are further broken down into steps.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
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

/// CR 103.1 + CR 101.4: The direction turns and APNAP ordering proceed around
/// the table. `Normal` is the game's default turn order, which "begins with the
/// starting player and proceeds clockwise" (CR 103.1). `Reversed` flips turn
/// progression, APNAP ordering (CR 101.4), and priority passing (CR 117.3d); it
/// does NOT change physical seating, so left/right neighbor resolution
/// (`players::neighbor`, Pramikon-style effects) is unaffected. Toggled by
/// `Effect::ReverseTurnOrder` (Temple of Atropos, Aeon Engine, Time Distortion).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TurnDirection {
    /// CR 103.1: the default clockwise turn order.
    #[default]
    Normal,
    /// The turn order runs counterclockwise (opposite the default).
    Reversed,
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

/// Turn-direction scope for a phase stop (MTGO-style). Determines on whose
/// turns a stop fires, by comparing the stop's owner against the active player.
///
/// CR 102.1: The active player is the player whose turn it is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub enum PhaseStopScope {
    /// Fire on every turn (legacy behavior; the migration default).
    #[default]
    AllTurns,
    /// Fire only on the stop owner's own turns.
    OwnTurn,
    /// Fire only on turns where the stop owner is NOT the active player.
    OpponentsTurns,
}

/// A single phase stop: the phase to pause at, plus the turn-direction scope.
///
/// Backward compatibility: older persisted/serialized stops were a bare `Phase`
/// string. `#[serde(from = "PhaseStopCompat")]` accepts both the legacy bare
/// string (→ `AllTurns`) and the new `{ phase, scope }` object form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(from = "PhaseStopCompat")]
pub struct PhaseStop {
    pub phase: Phase,
    pub scope: PhaseStopScope,
}

impl PhaseStop {
    /// CR 102.1: `active_player` is the player whose turn it is. Scope compares
    /// the stop's `owner` against that to decide whether this stop fires.
    pub fn applies(&self, owner: PlayerId, active_player: PlayerId) -> bool {
        match self.scope {
            PhaseStopScope::AllTurns => true,
            PhaseStopScope::OwnTurn => owner == active_player,
            PhaseStopScope::OpponentsTurns => owner != active_player,
        }
    }
}

/// Private serde shim: deserializes either the new scoped object form or the
/// legacy bare-`Phase` string, mapping the latter to `AllTurns`.
#[derive(Deserialize)]
#[serde(untagged)]
enum PhaseStopCompat {
    Scoped {
        phase: Phase,
        #[serde(default)]
        scope: PhaseStopScope,
    },
    Bare(Phase),
}

impl From<PhaseStopCompat> for PhaseStop {
    fn from(compat: PhaseStopCompat) -> Self {
        match compat {
            PhaseStopCompat::Scoped { phase, scope } => PhaseStop { phase, scope },
            PhaseStopCompat::Bare(phase) => PhaseStop {
                phase,
                scope: PhaseStopScope::AllTurns,
            },
        }
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
    fn turn_direction_default_is_normal_and_roundtrips() {
        assert_eq!(TurnDirection::default(), TurnDirection::Normal);
        let json = serde_json::to_string(&TurnDirection::Reversed).unwrap();
        assert_eq!(json, "\"Reversed\"");
        let back: TurnDirection = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TurnDirection::Reversed);
    }

    #[test]
    fn phase_roundtrips() {
        let phase = Phase::CombatDamage;
        let serialized = serde_json::to_string(&phase).unwrap();
        let deserialized: Phase = serde_json::from_str(&serialized).unwrap();
        assert_eq!(phase, deserialized);
    }

    #[test]
    fn phase_stop_deserializes_bare_string_as_all_turns() {
        let stop: PhaseStop = serde_json::from_str("\"PreCombatMain\"").unwrap();
        assert_eq!(
            stop,
            PhaseStop {
                phase: Phase::PreCombatMain,
                scope: PhaseStopScope::AllTurns,
            }
        );
    }

    #[test]
    fn phase_stop_scoped_roundtrips() {
        let stop = PhaseStop {
            phase: Phase::DeclareBlockers,
            scope: PhaseStopScope::OpponentsTurns,
        };
        let serialized = serde_json::to_string(&stop).unwrap();
        // Serialize always emits the scoped object form, never a bare string.
        assert!(serialized.contains("\"scope\""));
        let deserialized: PhaseStop = serde_json::from_str(&serialized).unwrap();
        assert_eq!(stop, deserialized);
    }

    #[test]
    fn phase_stop_scope_field_missing_defaults_all_turns() {
        let stop: PhaseStop = serde_json::from_str("{\"phase\":\"Upkeep\"}").unwrap();
        assert_eq!(
            stop,
            PhaseStop {
                phase: Phase::Upkeep,
                scope: PhaseStopScope::AllTurns,
            }
        );
    }

    #[test]
    fn phase_stop_applies_matrix() {
        let owner = PlayerId(0);
        let same = PlayerId(0);
        let other = PlayerId(1);

        let all = PhaseStop {
            phase: Phase::Upkeep,
            scope: PhaseStopScope::AllTurns,
        };
        assert!(all.applies(owner, same));
        assert!(all.applies(owner, other));

        let own = PhaseStop {
            phase: Phase::Upkeep,
            scope: PhaseStopScope::OwnTurn,
        };
        assert!(own.applies(owner, same));
        assert!(!own.applies(owner, other));

        let opp = PhaseStop {
            phase: Phase::Upkeep,
            scope: PhaseStopScope::OpponentsTurns,
        };
        assert!(!opp.applies(owner, same));
        assert!(opp.applies(owner, other));
    }
}
