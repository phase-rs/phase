//! Coverage plan: CR 117 Timing and Priority.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "117.1",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is", "game_not_over"],
        notes: "A player may cast spells / activate abilities when they have priority.",
    },
    CoveragePlanEntry {
        rule: "117.3a",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Active player receives priority after a spell/ability resolves.",
    },
    CoveragePlanEntry {
        rule: "117.3b",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Active player receives priority after a TBA that doesn't use stack.",
    },
    CoveragePlanEntry {
        rule: "117.3c",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Active player receives priority when a phase/step begins.",
    },
    CoveragePlanEntry {
        rule: "117.4",
        section: 117,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "If all players pass in succession, top of stack resolves / step ends.",
    },
];
