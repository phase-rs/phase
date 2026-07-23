//! Coverage plan: CR 704 State-Based Actions.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 704.1: SBAs are game actions that happen automatically when their
        // conditions are met; they don't use the stack. (Definition of the set.)
        rule: "704.1",
        section: 704,
        priority: CoveragePriority::Low,
        suggested_assertions: &["game_not_over"],
        notes: "Definition: SBAs happen automatically when conditions are met and don't use the \
                stack. Deferred: definitional — no single transition to assert.",
    },
    CoveragePlanEntry {
        // CR 704.3: whenever a player WOULD get priority, the game checks and
        // performs all applicable SBAs first, repeating until none apply.
        rule: "704.3",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["game_over", "creature_in_graveyard"],
        notes: "The check/perform loop that runs before a player gets priority (the timing rule).",
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
        // CR 704.5j: legend rule — two+ legendary permanents with the SAME NAME
        // controlled by the SAME PLAYER; controller keeps one, rest to graveyard.
        rule: "704.5j",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_zone", "creature_in_graveyard"],
        notes: "Legend rule — same-name legendary permanents under one controller; keep one, rest \
                to graveyard. Deferred: needs two legendary permanents + a choice.",
    },
    CoveragePlanEntry {
        // CR 704.5n: an Equipment/Fortification attached to an illegal permanent
        // or a player becomes unattached (it stays on the battlefield).
        rule: "704.5n",
        section: 704,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "Illegally-attached Equipment/Fortification becomes unattached (stays on \
                battlefield). Deferred: needs Equipment attach primitives.",
    },
    CoveragePlanEntry {
        // CR 704.5q: a permanent with both +1/+1 and -1/-1 counters removes N of
        // each, where N is the smaller count (counter annihilation).
        rule: "704.5q",
        section: 704,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["creature_on_battlefield"],
        notes: "+1/+1 and -1/-1 counters annihilate in equal numbers. Deferred: needs counter \
                setup + a counter-count read assertion.",
    },
];
