//! Command-zone assertions (CR 408) — presence stubs.
//!
//! CR 408 defines the command zone, which holds objects with special game
//! effects: commanders (CR 903), emblems (CR 114), plane/scheme/phenomenon
//! cards, and the like. Membership is observable through the generic
//! `creature_zone` assertion with `zone = "Command"`; this helper documents the
//! command-zone-specific predicates a future family would expose.

use engine::game::scenario::GameRunner;
use engine::types::zones::Zone;

use super::{AssertionFailure, HandleMap};

/// Assert a named object handle is currently in the command zone (CR 408.1).
pub fn assert_in_command_zone(
    runner: &GameRunner,
    handles: &HandleMap,
    handle: &str,
) -> Result<(), AssertionFailure> {
    let id = handles.get(handle).ok_or_else(|| AssertionFailure {
        kind: "command_zone".into(),
        detail: format!("unknown handle {handle:?}"),
    })?;
    let obj = runner
        .state()
        .objects
        .get(id)
        .ok_or_else(|| AssertionFailure {
            kind: "command_zone".into(),
            detail: format!("object {id:?} ({handle}) missing"),
        })?;
    if obj.zone == Zone::Command {
        Ok(())
    } else {
        Err(AssertionFailure {
            kind: "command_zone".into(),
            detail: format!("{handle}: expected Command zone, got {:?}", obj.zone),
        })
    }
}

/// CR 408 predicate vocabulary for future command-zone assertions.
pub const COMMAND_NOTES: &[&str] = &[
    "in_command_zone: object is in the command zone (CR 408.1).",
    "commander_tax: assert the additional {2} cost per prior cast (CR 903.8).",
    "emblem_present: assert a player owns an emblem (CR 114.1).",
];
