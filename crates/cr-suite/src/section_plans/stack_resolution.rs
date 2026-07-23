//! Coverage plan: CR 608 Resolving Spells and Abilities.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 608.1: each time all players pass in succession, the top object resolves.
        rule: "608.1",
        section: 608,
        priority: CoveragePriority::High,
        suggested_assertions: &["stack_is_empty", "player_life"],
        notes: "Top of stack resolves after all players pass. Executable: cast bolt → resolve_top \
                → assert stack_is_empty and the target's life dropped by 3.",
    },
    CoveragePlanEntry {
        // CR 608.2b: on resolution, targets are re-checked for legality.
        rule: "608.2b",
        section: 608,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_in_graveyard", "stack_is_empty"],
        notes: "Illegal-on-resolution targets are ignored (CR 608.2b). Deferred: needs a fizzle \
                setup (target leaves before resolution).",
    },
    CoveragePlanEntry {
        // CR 608.2m: after resolution the object leaves the stack.
        rule: "608.2m",
        section: 608,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["stack_is_empty"],
        notes: "A fully-resolved instant/sorcery goes to its owner's graveyard, leaving the stack \
                empty. Executable via cast → resolve_top → stack_is_empty.",
    },
];
