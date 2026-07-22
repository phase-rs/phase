//! Coverage plan: CR 400–408 Zones.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "400.1",
        section: 400,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_zone"],
        notes: "Definitional zone overview — mostly not-applicable.",
    },
    CoveragePlanEntry {
        rule: "401.1",
        section: 401,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["hand_count_equals"],
        notes: "Library is a zone; order matters.",
    },
    CoveragePlanEntry {
        rule: "402.1",
        section: 402,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["hand_count_equals", "hand_count_at_least"],
        notes: "Hand is a hidden zone for that player.",
    },
    CoveragePlanEntry {
        rule: "403.1",
        section: 403,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_on_battlefield", "creature_zone"],
        notes: "Battlefield is the shared zone for permanents.",
    },
    CoveragePlanEntry {
        rule: "404.1",
        section: 404,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_in_graveyard"],
        notes: "Graveyard is a public zone.",
    },
    CoveragePlanEntry {
        rule: "405.1",
        section: 405,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["phase_is"],
        notes: "Stack holds spells and abilities waiting to resolve.",
    },
    CoveragePlanEntry {
        rule: "406.1",
        section: 406,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_zone"],
        notes: "Exile is a public zone.",
    },
    CoveragePlanEntry {
        rule: "408.1",
        section: 408,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_zone"],
        notes: "Command zone — Commander / emblems / etc.",
    },
];
