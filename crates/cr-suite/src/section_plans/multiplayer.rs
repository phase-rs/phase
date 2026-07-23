//! Coverage plan: CR 800 Multiplayer + CR 903 Commander (deferred entries).

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        // CR 800.1: a multiplayer game begins with more than two players.
        rule: "800.1",
        section: 800,
        priority: CoveragePriority::Low,
        suggested_assertions: &["game_not_over"],
        notes: "Multiplayer overview. Deferred: the runner builds two-player games; needs an \
                N-player setup with N>2.",
    },
    CoveragePlanEntry {
        // CR 800.4: when a player leaves, their objects leave and SBAs are checked.
        rule: "800.4",
        section: 800,
        priority: CoveragePriority::Low,
        suggested_assertions: &["game_over"],
        notes:
            "A player leaving removes their objects (CR 800.4a). Deferred: needs a leave/concede \
                action in a multiplayer game.",
    },
    CoveragePlanEntry {
        // CR 903.1: Commander variant overview.
        rule: "903.1",
        section: 903,
        priority: CoveragePriority::Low,
        suggested_assertions: &["in_command_zone"],
        notes: "Commander overview. Deferred: needs Commander format setup (with_commander) and \
                command-zone assertions.",
    },
    CoveragePlanEntry {
        // CR 903.10: commander damage — 21 combat damage from one commander loses.
        rule: "903.10",
        section: 903,
        priority: CoveragePriority::Low,
        suggested_assertions: &["game_over", "player_life"],
        notes: "Commander damage (21 from one commander → lose). Deferred: needs commander combat \
                damage tracking + a dedicated assertion.",
    },
];
