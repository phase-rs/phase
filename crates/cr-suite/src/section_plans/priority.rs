//! Coverage plan: CR 117 Timing and Priority.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 117.1: the player with priority may cast spells / activate abilities.
        rule: "117.1",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["priority_player", "game_not_over"],
        notes:
            "The player with priority may cast spells, activate abilities, take special actions.",
    },
    CoveragePlanEntry {
        // CR 117.3a: the ACTIVE player receives priority at the beginning of most
        // steps/phases (after TBAs and begin-of-step triggers). No priority in untap.
        rule: "117.3a",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["priority_player", "phase_is"],
        notes: "Active player receives priority at the start of most steps/phases (none in untap).",
    },
    CoveragePlanEntry {
        // CR 117.3b: the ACTIVE player receives priority after a spell/ability
        // (other than a mana ability) resolves.
        rule: "117.3b",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["priority_player", "stack_is_empty"],
        notes: "Active player receives priority after a (non-mana) spell or ability resolves.",
    },
    CoveragePlanEntry {
        // CR 117.3c: a player who casts/activates/takes a special action while
        // holding priority receives priority again afterward.
        rule: "117.3c",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["priority_player"],
        notes: "A player who acts while holding priority receives priority again afterward.",
    },
    CoveragePlanEntry {
        // CR 117.4: if all players pass in succession, the top of the stack
        // resolves, or if the stack is empty, the step/phase ends.
        rule: "117.4",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["stack_is_empty", "phase_is"],
        notes: "All players passing in succession resolves the top of stack, or ends the step.",
    },
];
