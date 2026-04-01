use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::player::PlayerId;

// ─── Dungeon Identity ────────────────────────────────────────────────────────

/// CR 309: The five dungeon cards across D&D crossover sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum DungeonId {
    LostMineOfPhandelver,
    DungeonOfTheMadMage,
    TombOfAnnihilation,
    Undercity,
    BaldursGateWilderness,
}

impl fmt::Display for DungeonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LostMineOfPhandelver => write!(f, "Lost Mine of Phandelver"),
            Self::DungeonOfTheMadMage => write!(f, "Dungeon of the Mad Mage"),
            Self::TombOfAnnihilation => write!(f, "Tomb of Annihilation"),
            Self::Undercity => write!(f, "Undercity"),
            Self::BaldursGateWilderness => write!(f, "Baldur's Gate Wilderness"),
        }
    }
}

impl FromStr for DungeonId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "LostMineOfPhandelver" => Ok(Self::LostMineOfPhandelver),
            "DungeonOfTheMadMage" => Ok(Self::DungeonOfTheMadMage),
            "TombOfAnnihilation" => Ok(Self::TombOfAnnihilation),
            "Undercity" => Ok(Self::Undercity),
            "BaldursGateWilderness" => Ok(Self::BaldursGateWilderness),
            _ => Err(format!("Unknown dungeon: {s}")),
        }
    }
}

// ─── Venture Source ──────────────────────────────────────────────────────────

/// Distinguishes normal venture from initiative-sourced venture.
/// Typed enum per CLAUDE.md: "never a raw bool."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VentureSource {
    /// CR 701.49a: Normal "venture into the dungeon" — offers AFR trio
    /// (Lost Mine, Tomb, Mad Mage). Also used for 701.49c re-entry.
    Normal,
    /// CR 701.49d: "venture into [quality]" — constrained to a specific dungeon.
    /// Currently only Undercity (via initiative), but general per CR 701.49d.
    Specific(DungeonId),
}

// ─── Per-Player Progress ─────────────────────────────────────────────────────

/// CR 309 / CR 701.49: Per-player dungeon venture progress.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DungeonProgress {
    /// Which dungeon is currently active (None = no dungeon in command zone).
    pub current_dungeon: Option<DungeonId>,
    /// The room index the venture marker is on (0 = topmost room).
    pub current_room: u8,
    /// Single source of truth for completed dungeons. Derived checks:
    /// - "completed a dungeon" → `!completed.is_empty()`
    /// - "completed Tomb of Annihilation" → `completed.contains(&TombOfAnnihilation)`
    /// - dungeon count quantity → `completed.len()`
    pub completed: HashSet<DungeonId>,
}

// ─── Static Dungeon Definitions ──────────────────────────────────────────────

/// Static room definition within a dungeon graph.
pub struct RoomDefinition {
    pub name: &'static str,
    /// Indices of rooms this room leads to (empty = bottommost room).
    pub next_rooms: &'static [u8],
}

/// Static dungeon definition — one per dungeon card.
pub struct DungeonDefinition {
    pub id: DungeonId,
    pub name: &'static str,
    pub rooms: &'static [RoomDefinition],
}

/// CR 309: Look up a dungeon's static definition.
pub fn get_definition(id: DungeonId) -> &'static DungeonDefinition {
    match id {
        DungeonId::LostMineOfPhandelver => &LOST_MINE_OF_PHANDELVER,
        DungeonId::DungeonOfTheMadMage => &DUNGEON_OF_THE_MAD_MAGE,
        DungeonId::TombOfAnnihilation => &TOMB_OF_ANNIHILATION,
        DungeonId::Undercity => &UNDERCITY,
        DungeonId::BaldursGateWilderness => &BALDURS_GATE_WILDERNESS,
    }
}

/// CR 309.5: Check if a room is the bottommost room of its dungeon.
pub fn is_bottommost(id: DungeonId, room: u8) -> bool {
    let def = get_definition(id);
    def.rooms
        .get(room as usize)
        .is_some_and(|r| r.next_rooms.is_empty())
}

/// CR 309.5a: Get the rooms a player can advance to from a given room.
pub fn next_rooms(id: DungeonId, room: u8) -> &'static [u8] {
    let def = get_definition(id);
    def.rooms.get(room as usize).map_or(&[], |r| r.next_rooms)
}

/// CR 309.4: Get the name of a room.
pub fn room_name(id: DungeonId, room: u8) -> &'static str {
    let def = get_definition(id);
    def.rooms.get(room as usize).map_or("Unknown", |r| r.name)
}

/// CR 701.49a / CR 701.49d: Get available dungeons for a new venture.
/// Normal venture offers the AFR trio; Specific constrains to one dungeon.
pub fn available_dungeons(source: VentureSource) -> Vec<DungeonId> {
    match source {
        VentureSource::Normal => vec![
            DungeonId::LostMineOfPhandelver,
            DungeonId::DungeonOfTheMadMage,
            DungeonId::TombOfAnnihilation,
        ],
        VentureSource::Specific(id) => vec![id],
    }
}

/// Sentinel base for synthetic dungeon ObjectIds used by room triggers.
/// Each player gets `DUNGEON_SENTINEL_BASE + player.0 as u64`.
pub const DUNGEON_SENTINEL_BASE: u64 = 0xD0_0000_0000;

/// Get the synthetic ObjectId for a player's dungeon room triggers.
/// Used by the SBA (CR 704.5t) to identify pending room abilities on the stack.
pub fn dungeon_sentinel_id(player: PlayerId) -> crate::types::identifiers::ObjectId {
    crate::types::identifiers::ObjectId(DUNGEON_SENTINEL_BASE + player.0 as u64)
}

// ─── Dungeon Graph Data ──────────────────────────────────────────────────────
//
// Each dungeon is a directed acyclic graph of rooms. Room 0 is always the
// topmost room. Rooms with empty `next_rooms` are bottommost rooms.
// Room effects are handled separately in effects/venture.rs.

/// Lost Mine of Phandelver (Adventures in the Forgotten Realms)
/// 7 rooms, 2 branch points.
static LOST_MINE_OF_PHANDELVER: DungeonDefinition = DungeonDefinition {
    id: DungeonId::LostMineOfPhandelver,
    name: "Lost Mine of Phandelver",
    rooms: &[
        // 0: Cave Entrance → {Goblin Lair, Mine Tunnels}
        RoomDefinition {
            name: "Cave Entrance",
            next_rooms: &[1, 2],
        },
        // 1: Goblin Lair → {Storeroom, Dark Pool}
        RoomDefinition {
            name: "Goblin Lair",
            next_rooms: &[3, 4],
        },
        // 2: Mine Tunnels → {Dark Pool, Fungi Cavern}
        RoomDefinition {
            name: "Mine Tunnels",
            next_rooms: &[4, 5],
        },
        // 3: Storeroom → Temple of Dumathoin
        RoomDefinition {
            name: "Storeroom",
            next_rooms: &[6],
        },
        // 4: Dark Pool → Temple of Dumathoin
        RoomDefinition {
            name: "Dark Pool",
            next_rooms: &[6],
        },
        // 5: Fungi Cavern → Temple of Dumathoin
        RoomDefinition {
            name: "Fungi Cavern",
            next_rooms: &[6],
        },
        // 6: Temple of Dumathoin (bottommost)
        RoomDefinition {
            name: "Temple of Dumathoin",
            next_rooms: &[],
        },
    ],
};

/// Dungeon of the Mad Mage (Adventures in the Forgotten Realms)
/// 9 rooms, 2 branch points.
static DUNGEON_OF_THE_MAD_MAGE: DungeonDefinition = DungeonDefinition {
    id: DungeonId::DungeonOfTheMadMage,
    name: "Dungeon of the Mad Mage",
    rooms: &[
        // 0: Yawning Portal → Dungeon Level
        RoomDefinition {
            name: "Yawning Portal",
            next_rooms: &[1],
        },
        // 1: Dungeon Level → {Goblin Bazaar, Twisted Caverns}
        RoomDefinition {
            name: "Dungeon Level",
            next_rooms: &[2, 3],
        },
        // 2: Goblin Bazaar → Lost Level
        RoomDefinition {
            name: "Goblin Bazaar",
            next_rooms: &[4],
        },
        // 3: Twisted Caverns → Lost Level
        RoomDefinition {
            name: "Twisted Caverns",
            next_rooms: &[4],
        },
        // 4: Lost Level → {Runestone Caverns, Muiral's Graveyard}
        RoomDefinition {
            name: "Lost Level",
            next_rooms: &[5, 6],
        },
        // 5: Runestone Caverns → Deep Mines
        RoomDefinition {
            name: "Runestone Caverns",
            next_rooms: &[7],
        },
        // 6: Muiral's Graveyard → Deep Mines
        RoomDefinition {
            name: "Muiral's Graveyard",
            next_rooms: &[7],
        },
        // 7: Deep Mines → Mad Wizard's Lair
        RoomDefinition {
            name: "Deep Mines",
            next_rooms: &[8],
        },
        // 8: Mad Wizard's Lair (bottommost)
        RoomDefinition {
            name: "Mad Wizard's Lair",
            next_rooms: &[],
        },
    ],
};

/// Tomb of Annihilation (Adventures in the Forgotten Realms)
/// 5 rooms, 1 branch point.
static TOMB_OF_ANNIHILATION: DungeonDefinition = DungeonDefinition {
    id: DungeonId::TombOfAnnihilation,
    name: "Tomb of Annihilation",
    rooms: &[
        // 0: Trapped Entry → {Veils of Fear, Oubliette}
        RoomDefinition {
            name: "Trapped Entry",
            next_rooms: &[1, 2],
        },
        // 1: Veils of Fear → Sandfall Cell
        RoomDefinition {
            name: "Veils of Fear",
            next_rooms: &[3],
        },
        // 2: Oubliette → Cradle of the Death God
        RoomDefinition {
            name: "Oubliette",
            next_rooms: &[4],
        },
        // 3: Sandfall Cell → Cradle of the Death God
        RoomDefinition {
            name: "Sandfall Cell",
            next_rooms: &[4],
        },
        // 4: Cradle of the Death God (bottommost)
        RoomDefinition {
            name: "Cradle of the Death God",
            next_rooms: &[],
        },
    ],
};

/// Undercity (Commander Legends: Battle for Baldur's Gate)
/// 9 rooms, 3 branch points. Only reachable via "venture into the Undercity" (initiative).
static UNDERCITY: DungeonDefinition = DungeonDefinition {
    id: DungeonId::Undercity,
    name: "Undercity",
    rooms: &[
        // 0: Secret Entrance → {Forge, Lost Well}
        RoomDefinition {
            name: "Secret Entrance",
            next_rooms: &[1, 2],
        },
        // 1: Forge → {Trap!, Arena}
        RoomDefinition {
            name: "Forge",
            next_rooms: &[3, 4],
        },
        // 2: Lost Well → {Arena, Stash}
        RoomDefinition {
            name: "Lost Well",
            next_rooms: &[4, 5],
        },
        // 3: Trap! → Archives
        RoomDefinition {
            name: "Trap!",
            next_rooms: &[6],
        },
        // 4: Arena → {Archives, Catacombs}
        RoomDefinition {
            name: "Arena",
            next_rooms: &[6, 7],
        },
        // 5: Stash → Catacombs
        RoomDefinition {
            name: "Stash",
            next_rooms: &[7],
        },
        // 6: Archives → Throne of the Dead Three
        RoomDefinition {
            name: "Archives",
            next_rooms: &[8],
        },
        // 7: Catacombs → Throne of the Dead Three
        RoomDefinition {
            name: "Catacombs",
            next_rooms: &[8],
        },
        // 8: Throne of the Dead Three (bottommost)
        RoomDefinition {
            name: "Throne of the Dead Three",
            next_rooms: &[],
        },
    ],
};

/// Baldur's Gate Wilderness (Commander Legends: Battle for Baldur's Gate)
/// 19 rooms in a diamond pattern. Room effects are complex — most deferred as Unimplemented.
/// Graph structure: each room in a row leads to the adjacent rooms in the next row.
static BALDURS_GATE_WILDERNESS: DungeonDefinition = DungeonDefinition {
    id: DungeonId::BaldursGateWilderness,
    name: "Baldur's Gate Wilderness",
    rooms: &[
        // Row 1 (top)
        // 0: Crash Landing → {Goblin Camp, Emerald Grove}
        RoomDefinition {
            name: "Crash Landing",
            next_rooms: &[1, 2],
        },
        // Row 2
        // 1: Goblin Camp → {Auntie's Teahouse, Defiled Temple}
        RoomDefinition {
            name: "Goblin Camp",
            next_rooms: &[3, 4],
        },
        // 2: Emerald Grove → {Defiled Temple, Mountain Pass}
        RoomDefinition {
            name: "Emerald Grove",
            next_rooms: &[4, 5],
        },
        // Row 3
        // 3: Auntie's Teahouse → {Ebonlake Grotto, Grymforge}
        RoomDefinition {
            name: "Auntie's Teahouse",
            next_rooms: &[6, 7],
        },
        // 4: Defiled Temple → {Grymforge, Githyanki Crèche}
        RoomDefinition {
            name: "Defiled Temple",
            next_rooms: &[7, 8],
        },
        // 5: Mountain Pass → {Githyanki Crèche, Last Light Inn}
        RoomDefinition {
            name: "Mountain Pass",
            next_rooms: &[8, 9],
        },
        // Row 4 (widest)
        // 6: Ebonlake Grotto → {Reithwin Tollhouse, Moonrise Towers}
        RoomDefinition {
            name: "Ebonlake Grotto",
            next_rooms: &[10, 11],
        },
        // 7: Grymforge → {Reithwin Tollhouse, Moonrise Towers}
        RoomDefinition {
            name: "Grymforge",
            next_rooms: &[10, 11],
        },
        // 8: Githyanki Crèche → {Moonrise Towers, Gauntlet of Shar}
        RoomDefinition {
            name: "Githyanki Crèche",
            next_rooms: &[11, 12],
        },
        // 9: Last Light Inn → {Moonrise Towers, Gauntlet of Shar}
        RoomDefinition {
            name: "Last Light Inn",
            next_rooms: &[11, 12],
        },
        // Row 5
        // 10: Reithwin Tollhouse → {Balthazar's Lab, Circus of the Last Days}
        RoomDefinition {
            name: "Reithwin Tollhouse",
            next_rooms: &[13, 14],
        },
        // 11: Moonrise Towers → {Circus of the Last Days, Undercity Ruins}
        RoomDefinition {
            name: "Moonrise Towers",
            next_rooms: &[14, 15],
        },
        // 12: Gauntlet of Shar → {Undercity Ruins}
        RoomDefinition {
            name: "Gauntlet of Shar",
            next_rooms: &[15],
        },
        // Row 6
        // 13: Balthazar's Lab → {Steel Watch Foundry}
        RoomDefinition {
            name: "Balthazar's Lab",
            next_rooms: &[16],
        },
        // 14: Circus of the Last Days → {Steel Watch Foundry, Ansur's Sanctum}
        RoomDefinition {
            name: "Circus of the Last Days",
            next_rooms: &[16, 17],
        },
        // 15: Undercity Ruins → {Ansur's Sanctum}
        RoomDefinition {
            name: "Undercity Ruins",
            next_rooms: &[17],
        },
        // Row 7
        // 16: Steel Watch Foundry → Temple of Bhaal
        RoomDefinition {
            name: "Steel Watch Foundry",
            next_rooms: &[18],
        },
        // 17: Ansur's Sanctum → Temple of Bhaal
        RoomDefinition {
            name: "Ansur's Sanctum",
            next_rooms: &[18],
        },
        // Row 8 (bottom)
        // 18: Temple of Bhaal (bottommost)
        RoomDefinition {
            name: "Temple of Bhaal",
            next_rooms: &[],
        },
    ],
};

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify all dungeons have valid graph structure:
    /// - Every room index in `next_rooms` is a valid room index
    /// - At least one bottommost room
    /// - All next_rooms point forward (no cycles)
    #[test]
    fn all_dungeons_have_valid_graph_structure() {
        let dungeons = [
            DungeonId::LostMineOfPhandelver,
            DungeonId::DungeonOfTheMadMage,
            DungeonId::TombOfAnnihilation,
            DungeonId::Undercity,
            DungeonId::BaldursGateWilderness,
        ];

        for id in dungeons {
            let def = get_definition(id);
            let room_count = def.rooms.len();
            assert!(room_count > 0, "{id}: no rooms");

            let mut bottommost_count = 0;
            for (i, room) in def.rooms.iter().enumerate() {
                for &next in room.next_rooms {
                    assert!(
                        (next as usize) < room_count,
                        "{id}: room {i} ({}) points to invalid room {next}",
                        room.name
                    );
                    assert!(
                        next as usize > i,
                        "{id}: room {i} ({}) points backward to room {next}",
                        room.name
                    );
                }
                if room.next_rooms.is_empty() {
                    bottommost_count += 1;
                }
            }
            assert!(bottommost_count >= 1, "{id}: no bottommost room found");
        }
    }

    #[test]
    fn is_bottommost_correct() {
        assert!(!is_bottommost(DungeonId::LostMineOfPhandelver, 0));
        assert!(is_bottommost(DungeonId::LostMineOfPhandelver, 6));
        assert!(is_bottommost(DungeonId::TombOfAnnihilation, 4));
        assert!(!is_bottommost(DungeonId::TombOfAnnihilation, 0));
        assert!(is_bottommost(DungeonId::Undercity, 8));
        assert!(is_bottommost(DungeonId::BaldursGateWilderness, 18));
    }

    #[test]
    fn next_rooms_at_branch_points() {
        // Lost Mine: Cave Entrance has 2 exits
        assert_eq!(next_rooms(DungeonId::LostMineOfPhandelver, 0), &[1, 2]);
        // Storeroom has 1 exit
        assert_eq!(next_rooms(DungeonId::LostMineOfPhandelver, 3), &[6]);
        // Temple of Dumathoin has 0 exits (bottommost)
        assert!(next_rooms(DungeonId::LostMineOfPhandelver, 6).is_empty());
    }

    #[test]
    fn available_dungeons_normal_vs_specific() {
        let normal = available_dungeons(VentureSource::Normal);
        assert_eq!(normal.len(), 3);
        assert!(!normal.contains(&DungeonId::Undercity));
        assert!(!normal.contains(&DungeonId::BaldursGateWilderness));

        let specific = available_dungeons(VentureSource::Specific(DungeonId::Undercity));
        assert_eq!(specific, vec![DungeonId::Undercity]);
    }

    #[test]
    fn dungeon_progress_default_is_empty() {
        let progress = DungeonProgress::default();
        assert_eq!(progress.current_dungeon, None);
        assert_eq!(progress.current_room, 0);
        assert!(progress.completed.is_empty());
    }

    #[test]
    fn dungeon_id_display_and_from_str() {
        let id = DungeonId::LostMineOfPhandelver;
        assert_eq!(id.to_string(), "Lost Mine of Phandelver");
        assert_eq!(
            DungeonId::from_str("LostMineOfPhandelver").unwrap(),
            DungeonId::LostMineOfPhandelver
        );
        assert!(DungeonId::from_str("InvalidDungeon").is_err());
    }

    #[test]
    fn room_names_match_oracle_text() {
        assert_eq!(
            room_name(DungeonId::LostMineOfPhandelver, 0),
            "Cave Entrance"
        );
        assert_eq!(
            room_name(DungeonId::TombOfAnnihilation, 4),
            "Cradle of the Death God"
        );
        assert_eq!(room_name(DungeonId::Undercity, 0), "Secret Entrance");
        assert_eq!(
            room_name(DungeonId::Undercity, 8),
            "Throne of the Dead Three"
        );
        assert_eq!(
            room_name(DungeonId::DungeonOfTheMadMage, 8),
            "Mad Wizard's Lair"
        );
    }

    #[test]
    fn room_count_per_dungeon() {
        assert_eq!(
            get_definition(DungeonId::LostMineOfPhandelver).rooms.len(),
            7
        );
        assert_eq!(
            get_definition(DungeonId::DungeonOfTheMadMage).rooms.len(),
            9
        );
        assert_eq!(get_definition(DungeonId::TombOfAnnihilation).rooms.len(), 5);
        assert_eq!(get_definition(DungeonId::Undercity).rooms.len(), 9);
        assert_eq!(
            get_definition(DungeonId::BaldursGateWilderness).rooms.len(),
            19
        );
    }
}
