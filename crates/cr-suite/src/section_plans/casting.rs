//! Coverage plan: CR 601 Casting Spells.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 601.2: casting a spell moves the card to the stack and proceeds
        // through announcement, targeting, costs, and payment.
        rule: "601.2",
        section: 601,
        priority: CoveragePriority::High,
        suggested_assertions: &["stack_is_empty", "player_life"],
        notes:
            "Cast a spell → it goes on the stack. Executable: cast Lightning Bolt, assert stack \
                is non-empty pre-resolve (via a resolve step then stack_is_empty + life change).",
    },
    CoveragePlanEntry {
        // CR 601.2c: the player announces the spell's targets.
        rule: "601.2c",
        section: 601,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life", "creature_in_graveyard"],
        notes: "Target selection during casting — exercised by the SelectTargets step of the bolt \
                cast (a target player or creature is chosen).",
    },
    CoveragePlanEntry {
        // CR 601.2h/601.2i: costs are paid; the spell becomes cast.
        rule: "601.2i",
        section: 601,
        priority: CoveragePriority::Low,
        suggested_assertions: &["stack_is_empty"],
        notes: "Costs paid → spell is cast and becomes an object on the stack. Deferred: mana \
                payment is auto-mode in the runner; a mana-count assertion would discriminate.",
    },
];
