//! Coverage plan: CR 702 Keyword Abilities (sample evergreen set).

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "702.2",
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "Deathtouch — interacts with CR 704.5h.",
    },
    CoveragePlanEntry {
        rule: "702.7",
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "First strike — combat damage ordering.",
    },
    CoveragePlanEntry {
        rule: "702.9",
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "Flying — blocker legality.",
    },
    CoveragePlanEntry {
        rule: "702.19",
        section: 702,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life", "creature_damage"],
        notes: "Trample — excess combat damage to player/planeswalker.",
    },
    CoveragePlanEntry {
        rule: "702.23",
        section: 702,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "Haste — summoning sickness exemption.",
    },
];
