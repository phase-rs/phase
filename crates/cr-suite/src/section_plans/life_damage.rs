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
        // CR 104.2a: a player wins if all their opponents have left the game.
        // Conceding is CR 104.3a, NOT 104.2a.
        rule: "104.2a",
        section: 104,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["game_over"],
        notes: "A player wins when all opponents have left the game (overrides win-preclusion).",
    },
    CoveragePlanEntry {
        // CR 104.3a: a player can concede at any time and immediately loses.
        rule: "104.3a",
        section: 104,
        priority: CoveragePriority::Low,
        suggested_assertions: &["game_over"],
        notes: "Concede → that player loses immediately. Deferred: needs an engine concede action.",
    },
    CoveragePlanEntry {
        rule: "119.1",
        section: 119,
        priority: CoveragePriority::High,
        suggested_assertions: &["player_life"],
        notes: "Default starting life total is 20.",
    },
    CoveragePlanEntry {
        // CR 119.2: damage dealt to a player normally causes that player to
        // lose that much life (see CR 120.3a).
        rule: "119.2",
        section: 119,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life"],
        notes: "Damage dealt to a player normally causes that player to lose that much life.",
    },
    CoveragePlanEntry {
        // CR 119.3: gaining or losing life adjusts the life total accordingly.
        rule: "119.3",
        section: 119,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life"],
        notes: "Gaining/losing life adjusts the player's life total accordingly.",
    },
    CoveragePlanEntry {
        // CR 119.7: if an effect says a player can't gain life, life-gain is skipped.
        rule: "119.7",
        section: 119,
        priority: CoveragePriority::Low,
        suggested_assertions: &["player_life"],
        notes: "Can't-gain-life: life-gain events do nothing. Deferred: needs a life-gain path.",
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
        // CR 120.3a: damage to a player by a source without infect = that much life loss.
        rule: "120.3a",
        section: 120,
        priority: CoveragePriority::Medium,
        suggested_assertions: &["player_life"],
        notes: "Damage to a player by a non-infect source causes loss of that much life.",
    },
    CoveragePlanEntry {
        // CR 120.4: damage is processed in a four-part sequence (ordering rule),
        // NOT the "damage to player = life loss" rule (that is CR 120.3a).
        rule: "120.4",
        section: 120,
        priority: CoveragePriority::Low,
        suggested_assertions: &["creature_damage", "player_life"],
        notes: "Damage processing order (four-part sequence). Deferred: ordering not observable \
                via post-condition state alone.",
    },
];
