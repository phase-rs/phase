//! Coverage plan: CR 614 Replacement Effects + CR 615 Prevention Effects.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 614.1: replacement effects watch for an event and replace it.
        rule: "614.1",
        section: 614,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_on_battlefield", "creature_zone"],
        notes: "Replacement effects modify an event before it happens (e.g. enters-tapped). \
                Deferred: needs a replacement source and an as-enters transition.",
    },
    CoveragePlanEntry {
        // CR 614.13: some replacement effects are one-shot 'shields' that are consumed.
        rule: "614.13",
        section: 614,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "Shield/regeneration one-shot replacement is consumed on use. Deferred: needs a \
                shield-counter setup and a consumed-shield read.",
    },
    CoveragePlanEntry {
        // CR 615.1: prevention effects watch for a damage event and prevent it.
        rule: "615.1",
        section: 615,
        priority: CoveragePriority::Low,
        suggested_assertions: &["player_life", "creature_damage"],
        notes: "Prevention effects prevent some/all of a damage event. Deferred: needs a \
                prevention source (e.g. Fog / protection) plus a bolt damage event.",
    },
];
