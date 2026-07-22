//! Coverage plan: CR 506–511 Combat.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "507.1",
        section: 507,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Beginning of combat step.",
    },
    CoveragePlanEntry {
        rule: "508.1",
        section: 508,
        priority: CoveragePriority::High,
        suggested_assertions: &["phase_is", "creature_on_battlefield"],
        notes: "Declare attackers — needs combat runner helpers (future step kinds).",
    },
    CoveragePlanEntry {
        rule: "509.1",
        section: 509,
        priority: CoveragePriority::High,
        suggested_assertions: &["phase_is"],
        notes: "Declare blockers.",
    },
    CoveragePlanEntry {
        rule: "510.1",
        section: 510,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_damage", "player_life"],
        notes: "Combat damage step.",
    },
    CoveragePlanEntry {
        rule: "511.1",
        section: 511,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "End of combat step.",
    },
];
