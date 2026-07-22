//! Coverage plan: CR 119 Life + CR 120 Damage + CR 104 Ending the Game.

use super::{CoveragePlanEntry, CoveragePriority};

pub const PLAN: &[CoveragePlanEntry] = &[
    CoveragePlanEntry {
        rule: "104.1",
        section: 104,
        priority: CoveragePriority::High,
        suggested_assertions: &["game_over", "player_life"],
        notes: "Game ends when a player wins via SBAs after life loss.",
    },
    CoveragePlanEntry {
        rule: "104.2a",
        section: 104,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["game_over"],
        notes: "A player can concede; model via forced GameOver if engine exposes concede.",
    },
    CoveragePlanEntry {
        rule: "119.1",
        section: 119,
        priority: CoveragePriority::High,
        suggested_assertions: &["player_life"],
        notes: "Default starting life total is 20.",
    },
    CoveragePlanEntry {
        rule: "119.2",
        section: 119,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life"],
        notes: "Life totals can be modified by effects / costs.",
    },
    CoveragePlanEntry {
        rule: "119.3",
        section: 119,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life"],
        notes: "If an effect causes a player to gain life and that player can't, ignore.",
    },
    CoveragePlanEntry {
        rule: "120.1",
        section: 120,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_damage", "player_life"],
        notes: "Damage is dealt; results depend on what is damaged.",
    },
    CoveragePlanEntry {
        rule: "120.3",
        section: 120,
        priority: CoveragePriority::High,
        suggested_assertions: &["creature_damage", "creature_on_battlefield"],
        notes: "Marked damage persists until removed / leaves battlefield.",
    },
    CoveragePlanEntry {
        rule: "120.4",
        section: 120,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life"],
        notes: "Damage to a player causes loss of that much life.",
    },
];
