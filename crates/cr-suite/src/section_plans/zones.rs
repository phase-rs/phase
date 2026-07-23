//! Coverage plan: CR 400–408 Zones.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "400.1",
        section: 400,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_zone"],
        notes: "Definitional zone overview. Deferred: definitional — no transition to assert.",
    },
    CoveragePlanEntry {
        // CR 401.1: a player's deck becomes their library at game start.
        rule: "401.1",
        section: 401,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["library_count_equals"],
        notes: "Library is an ordered zone. Executable via library_count_equals on a \
                setup-seeded library; a draw/mill transition would make it discriminating.",
    },
    CoveragePlanEntry {
        rule: "402.1",
        section: 402,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["hand_count_equals", "hand_count_at_least"],
        notes: "Hand is a hidden zone for its owner. Executable via hand_count on a seeded hand.",
    },
    CoveragePlanEntry {
        rule: "403.1",
        section: 403,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_on_battlefield", "creature_zone"],
        notes: "Battlefield is the shared permanent zone. Deferred: only asserts a setup-placed \
                creature until an ETB / ChangeZone transition is exercised.",
    },
    CoveragePlanEntry {
        rule: "404.1",
        section: 404,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_in_graveyard"],
        notes: "Graveyard is a public zone; reached via SBA death (see 704.5f/g fixtures).",
    },
    CoveragePlanEntry {
        // CR 405.1: the stack holds spells/abilities waiting to resolve.
        rule: "405.1",
        section: 405,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["stack_is_empty"],
        notes: "Stack holds spells/abilities. Executable: cast a bolt (stack non-empty), resolve, \
                then assert stack_is_empty.",
    },
    CoveragePlanEntry {
        rule: "406.1",
        section: 406,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_zone"],
        notes: "Exile is a public zone. Deferred: needs an exile transition (e.g. exile effect).",
    },
    CoveragePlanEntry {
        rule: "408.1",
        section: 408,
        priority: CoveragePriority::Low,
        suggested_assertions: &["in_command_zone", "creature_zone"],
        notes: "Command zone (Commander / emblems). Deferred: needs command-zone setup.",
    },
];
