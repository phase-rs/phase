//! Coverage plan: CR 613 Interaction of Continuous Effects (layer system).

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 613.1: characteristics are computed by applying continuous effects
        // in a fixed series of layers.
        rule: "613.1",
        section: 613,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_has_keyword"],
        notes: "Layer system overview. Deferred: the existing keyword/P-T assertions already read \
                post-layer values; a layer-ordering discriminator needs conflicting effects.",
    },
    CoveragePlanEntry {
        // CR 613.4 (7c): P/T-setting vs P/T-modifying sublayers.
        rule: "613.4",
        section: 613,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_damage"],
        notes: "Power/toughness sublayers (set → counters → modify → switch). Deferred: needs two \
                conflicting P/T effects and a P/T read assertion.",
    },
];
