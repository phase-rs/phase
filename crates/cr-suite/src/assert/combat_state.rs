//! Combat-state assertions (CR 508 / CR 509).
//!
//! These read the engine's live `CombatState` (populated by the production
//! `DeclareAttackers` / `DeclareBlockers` turn-based actions, CR 508.1 / 509.1).
//! When no combat is in progress the assertions fail with a clear diagnostic
//! rather than silently passing — the runner must have driven a real combat
//! declaration for these to hold.

use engine::game::scenario::GameRunner;

use super::{AssertionFailure, HandleMap};

/// Assert a named creature was declared as an attacker this combat (CR 508.1).
pub fn assert_attacker_declared(
    runner: &GameRunner,
    handles: &HandleMap,
    creature: &str,
) -> Result<(), AssertionFailure> {
    let id = handles.get(creature).ok_or_else(|| AssertionFailure {
        kind: "attacker_declared".into(),
        detail: format!("unknown creature handle {creature:?}"),
    })?;
    let combat = runner
        .state()
        .combat
        .as_ref()
        .ok_or_else(|| AssertionFailure {
            kind: "attacker_declared".into(),
            detail: "no combat in progress (attackers not declared)".into(),
        })?;
    if combat.attackers.iter().any(|a| a.object_id == *id) {
        Ok(())
    } else {
        Err(AssertionFailure {
            kind: "attacker_declared".into(),
            detail: format!("{creature} ({id:?}) is not a declared attacker"),
        })
    }
}

/// Assert a named creature was declared as a blocker this combat (CR 509.1).
pub fn assert_blocker_declared(
    runner: &GameRunner,
    handles: &HandleMap,
    creature: &str,
) -> Result<(), AssertionFailure> {
    let id = handles.get(creature).ok_or_else(|| AssertionFailure {
        kind: "blocker_declared".into(),
        detail: format!("unknown creature handle {creature:?}"),
    })?;
    let combat = runner
        .state()
        .combat
        .as_ref()
        .ok_or_else(|| AssertionFailure {
            kind: "blocker_declared".into(),
            detail: "no combat in progress (blockers not declared)".into(),
        })?;
    if combat.blocker_to_attacker.contains_key(id) {
        Ok(())
    } else {
        Err(AssertionFailure {
            kind: "blocker_declared".into(),
            detail: format!("{creature} ({id:?}) is not a declared blocker"),
        })
    }
}
