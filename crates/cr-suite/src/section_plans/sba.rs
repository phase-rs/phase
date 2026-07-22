//! Coverage plan: CR 704 State-Based Actions.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "704.1",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["game_not_over"],
        notes: "SBAs are checked throughout the game continuously whenever a player would receive priority.",
    },
    CoveragePlanEntry {
        rule: "704.3",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["game_over", "creature_in_graveyard"],
        notes: "Whenever a player would get priority, the game checks SBAs first.",
    },
    CoveragePlanEntry {
        rule: "704.5a",
        section: 704,
        priority: CoveragePriority::High,
        suggested_assertions: &["game_over", "player_life"],
        notes: "0 or less life → lose the game.",
    },
    CoveragePlanEntry {
        rule: "704.5b",
        section: 704,
        priority: CoveragePriority::High,
        suggested_assertions: &["game_over"],
        notes: "Attempt to draw from empty library → lose (needs library setup).",
    },
    CoveragePlanEntry {
        rule: "704.5c",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["game_over"],
        notes: "Poison counters ≥ 10 → lose.",
    },
    CoveragePlanEntry {
        rule: "704.5f",
        section: 704,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_in_graveyard"],
        notes: "Toughness ≤ 0 → graveyard.",
    },
    CoveragePlanEntry {
        rule: "704.5g",
        section: 704,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_in_graveyard"],
        notes: "Lethal damage → destroyed.",
    },
    CoveragePlanEntry {
        rule: "704.5h",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_in_graveyard"],
        notes: "Deathtouch damage → destroyed.",
    },
    CoveragePlanEntry {
        rule: "704.5j",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_zone"],
        notes: "Legend rule — needs two legendary permanents.",
    },
    CoveragePlanEntry {
        rule: "704.5n",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_zone"],
        notes: "+1/+1 and -1/-1 counters annihilate.",
    },
];
