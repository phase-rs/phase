//! Coverage plan: CR 500–514 Turn Structure.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "500.1",
        section: 500,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "A turn consists of five phases.",
    },
    CoveragePlanEntry {
        rule: "501.1",
        section: 501,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Beginning phase: untap, upkeep, draw.",
    },
    CoveragePlanEntry {
        rule: "502.1",
        section: 502,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Untap step — no priority (CR 502.4).",
    },
    CoveragePlanEntry {
        rule: "503.1",
        section: 503,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Upkeep step — AP gets priority.",
    },
    CoveragePlanEntry {
        rule: "504.1",
        section: 504,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is", "hand_count_at_least"],
        notes: "Draw step — AP draws a card as TBA.",
    },
    CoveragePlanEntry {
        rule: "505.1",
        section: 505,
        priority: CoveragePriority::High,
        suggested_assertions: &["phase_is"],
        notes: "Two main phases separated by combat.",
    },
    CoveragePlanEntry {
        rule: "506.1",
        section: 506,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Combat phase has five steps.",
    },
    CoveragePlanEntry {
        rule: "512.1",
        section: 512,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Ending phase: end step + cleanup.",
    },
    CoveragePlanEntry {
        rule: "514.1",
        section: 514,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is", "creature_damage"],
        notes: "Cleanup removes damage and discards to hand size.",
    },
];
