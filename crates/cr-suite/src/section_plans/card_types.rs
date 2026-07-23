//! Coverage plan: CR 300–310 Card Types (sample).

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 300.1: enumerates the card types.
        rule: "300.1",
        section: 300,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "Card-type enumeration (definitional). Deferred: no single transition to assert.",
    },
    CoveragePlanEntry {
        // CR 302.1: a creature spell can be cast during a main phase with an empty stack.
        rule: "302.1",
        section: 302,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_on_battlefield", "stack_is_empty"],
        notes: "Creatures — cast a creature spell → resolves onto the battlefield. Deferred: the \
                runner's only production cast step is Lightning Bolt; needs a creature-cast step.",
    },
    CoveragePlanEntry {
        // CR 304.1: an instant can be cast any time its controller has priority.
        rule: "304.1",
        section: 304,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["stack_is_empty", "player_life"],
        notes: "Instants — Lightning Bolt is an instant; its cast/resolve is the model for this \
                type (Effect::DealDamage). Executable via existing bolt fixtures.",
    },
    CoveragePlanEntry {
        // CR 305.1: playing a land is a special action, not a spell.
        rule: "305.1",
        section: 305,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "Lands — played as a special action (no stack). Deferred: needs a play-land step.",
    },
];
