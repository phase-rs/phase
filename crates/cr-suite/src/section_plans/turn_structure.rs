//! Coverage plan: CR 500–514 Turn Structure.

use super::{CoveragePlanEntry, CoveragePriority};

// Most of these rules can only assert the phase the setup placed the game in,
// which is tautological until the runner drives real turn/phase advancement
// (CR 500–514 TBAs). They are marked Low priority with honest deferral notes;
// each names the transition that would make it a discriminating executable.
pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "500.1",
        section: 500,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "A turn consists of five phases. Deferred: only asserts the setup-chosen phase \
                until the runner advances through a full turn.",
    },
    CoveragePlanEntry {
        rule: "501.1",
        section: 501,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "Beginning phase: untap, upkeep, draw. Deferred: needs real phase advancement.",
    },
    CoveragePlanEntry {
        rule: "502.1",
        section: 502,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "Untap step — no priority (CR 502.4). Deferred: needs untap-step advancement.",
    },
    CoveragePlanEntry {
        rule: "503.1",
        section: 503,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is", "priority_player"],
        notes: "Upkeep step — AP gets priority. Deferred: needs advance into the upkeep step.",
    },
    CoveragePlanEntry {
        rule: "504.1",
        section: 504,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["hand_count_at_least"],
        notes: "Draw step — AP draws a card as a TBA. Would become executable via a real advance \
                into the draw step asserting hand_count increased by one.",
    },
    CoveragePlanEntry {
        rule: "505.1",
        section: 505,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "Two main phases separated by combat. Deferred: only asserts the setup-chosen \
                phase; needs a real advance between the two main phases.",
    },
    CoveragePlanEntry {
        rule: "506.1",
        section: 506,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "Combat phase has five steps. Deferred: needs combat-step advancement.",
    },
    CoveragePlanEntry {
        rule: "512.1",
        section: 512,
        priority: CoveragePriority::Low,
        suggested_assertions: &["phase_is"],
        notes: "Ending phase: end step + cleanup. Deferred: needs advance into the ending phase.",
    },
    CoveragePlanEntry {
        rule: "514.1",
        section: 514,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_damage"],
        notes: "Cleanup removes marked damage and discards to hand size (CR 514.1–514.2). Would \
                become executable by advancing into cleanup and asserting damage cleared to 0.",
    },
];
