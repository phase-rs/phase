use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::types::player::PlayerId;

// ─── Dungeon Identity ────────────────────────────────────────────────────────

/// CR 309: The five dungeon cards across D&D crossover sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    pub completed: BTreeSet<DungeonId>,
}

// ─── Static Dungeon Definitions ──────────────────────────────────────────────

/// Static room definition within a dungeon graph.
pub struct RoomDefinition {
    /// CR 309.4b: The room's printed name. Flavor text — it never affects play,
    /// but it is how players and the UI identify a room.
    pub name: &'static str,
    /// CR 309.4c: The room ability's printed effect — the "[effect]" half of
    /// "When you move your venture marker into this room, [effect]". Verbatim
    /// Oracle text minus the "(Leads to: ...)" reminder, which `next_rooms`
    /// already encodes.
    pub text: &'static str,
    /// Indices of rooms this room leads to (empty = bottommost room).
    pub next_rooms: &'static [u8],
}

/// Static dungeon definition — one per dungeon card.
pub struct DungeonDefinition {
    pub id: DungeonId,
    pub name: &'static str,
    pub rooms: &'static [RoomDefinition],
}

/// CR 309.4: Where a room sits on the printed dungeon card, as a fraction of
/// the card image (0.0-1.0, origin top-left).
///
/// The dungeon cards are the rare case where the "art" IS the game state: each
/// card is a labeled floor plan whose rooms are drawn as rectangles, not a
/// framed illustration. That is what makes overlaying a venture marker on the
/// image meaningful rather than decorative — the marker lands in the drawn room
/// it represents.
///
/// Fractions, not pixels, because the client renders the card at whatever width
/// the panel gives it and picks an image rung by viewport. Fractions also
/// transfer across printings: the token-set and oversized printings of a
/// dungeon are the same layout at the same proportions (verified by comparing
/// the `tafr` and `oafr` Lost Mine scans, identical at every rung).
///
/// `x`/`y` are the room rectangle's CENTER, which is where the marker is drawn.
///
/// Stored as PERMILLE (0-1000) rather than a float. `DerivedViews` derives `Eq`
/// so that state snapshots can be compared for equality, and `f32` is not `Eq`
/// — a float here would force the whole projection down to `PartialEq`. Permille
/// is far finer than the overlay needs: one part in a thousand of a 672px card
/// is under a pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMarkerPoint {
    /// Distance from the card's left edge, in thousandths of its width.
    pub x_permille: u16,
    /// Distance from the card's top edge, in thousandths of its height.
    pub y_permille: u16,
}

/// Authoring helper: the coordinates are read off the card as fractions, so
/// they are written as fractions and converted once, here.
const fn pt(x_permille: u16, y_permille: u16) -> RoomMarkerPoint {
    RoomMarkerPoint {
        x_permille,
        y_permille,
    }
}

/// CR 309.4: Centers of each room's drawn rectangle on the dungeon card,
/// indexed in the same order as the dungeon's `rooms` table.
///
/// Authored from the printed card faces; there is no coordinate data on
/// Scryfall (its `oracle_text` gives only ordered "Name — Effect. (Leads to:)"
/// lines), so these are the single place the geometry exists. The
/// `dungeon_marker_points_cover_every_room` test pins each list's length to its
/// room table so a dungeon can never carry a partial or stale list.
pub fn marker_points(id: DungeonId) -> &'static [RoomMarkerPoint] {
    match id {
        DungeonId::LostMineOfPhandelver => &LOST_MINE_MARKERS,
        DungeonId::DungeonOfTheMadMage => &MAD_MAGE_MARKERS,
        DungeonId::TombOfAnnihilation => &TOMB_MARKERS,
        DungeonId::Undercity => &UNDERCITY_MARKERS,
        DungeonId::BaldursGateWilderness => &BALDURS_GATE_MARKERS,
    }
}

/// Lost Mine of Phandelver — 4 rows: full / halves / thirds / full.
/// Read off the printed card face (AFR token #021).
static LOST_MINE_MARKERS: [RoomMarkerPoint; 7] = [
    pt(500, 215), // 0 Cave Entrance   (full width)
    pt(310, 390), // 1 Goblin Lair     (left half)
    pt(690, 390), // 2 Mine Tunnels    (right half)
    pt(180, 610), // 3 Storeroom       (left third)
    pt(500, 610), // 4 Dark Pool       (centre third)
    pt(810, 610), // 5 Fungi Cavern    (right third)
    pt(500, 800), // 6 Temple of Dumathoin (full width)
];

/// Dungeon of the Mad Mage — alternating full-width corridors and half-width
/// branch pairs. The branch rows are drawn roughly twice as tall as the
/// single-room corridors, so the y values are not evenly spaced.
/// Read off the printed card face (AFR token #020).
static MAD_MAGE_MARKERS: [RoomMarkerPoint; 9] = [
    pt(500, 184), // 0 Yawning Portal      (full width)
    pt(500, 259), // 1 Dungeon Level       (full width)
    pt(290, 376), // 2 Goblin Bazaar       (left half, tall)
    pt(715, 376), // 3 Twisted Caverns     (right half, tall)
    pt(500, 494), // 4 Lost Level          (full width)
    pt(290, 606), // 5 Runestone Caverns   (left half, tall)
    pt(715, 606), // 6 Muiral's Graveyard  (right half, tall)
    pt(500, 723), // 7 Deep Mines          (full width)
    pt(500, 814), // 8 Mad Wizard's Lair   (full width, bottommost)
];

/// Tomb of Annihilation — irregular: Oubliette is a single TALL room on the
/// right spanning the vertical extent of both Veils of Fear and Sandfall Cell,
/// which are stacked on the left. Not a 2x2 grid.
/// Read off the printed card face (AFR token #022).
static TOMB_MARKERS: [RoomMarkerPoint; 5] = [
    pt(500, 220), // 0 Trapped Entry           (full width)
    pt(290, 400), // 1 Veils of Fear           (left, upper)
    pt(720, 480), // 2 Oubliette               (right, spans both left rows)
    pt(290, 580), // 3 Sandfall Cell           (left, lower)
    pt(500, 775), // 4 Cradle of the Death God (full width, bottommost)
];

/// Undercity — the "You can't enter this dungeon unless you 'venture into
/// Undercity.'" line above the grid pushes every row down relative to the AFR
/// dungeons, and the Trap!/Arena/Stash row is asymmetric (Arena is a narrow
/// centre room). Archives/Catacombs below it are unequal halves.
/// Read off the printed card face (CLB token #020).
static UNDERCITY_MARKERS: [RoomMarkerPoint; 9] = [
    pt(500, 248), // 0 Secret Entrance
    pt(310, 370), // 1 Forge            (left half)
    pt(710, 370), // 2 Lost Well        (right half)
    pt(200, 508), // 3 Trap!            (left)
    pt(490, 508), // 4 Arena            (narrow centre)
    pt(780, 508), // 5 Stash            (right)
    pt(280, 654), // 6 Archives         (left)
    pt(700, 660), // 7 Catacombs        (right, taller)
    pt(500, 818), // 8 Throne of the Dead Three (bottommost)
];

/// Baldur's Gate Wilderness — 19 rooms in an alternating lattice, NOT a
/// diamond: one full-width entry, then rows of 3 and 2 alternating down the
/// card (3/2/3/2/3/2/3). Rooms in the 3-rows sit at the thirds; rooms in the
/// 2-rows sit offset between them, which is what makes the diagonal corridors
/// line up on the printed card.
/// Read off the printed card face (CLB token, 2024 printing).
static BALDURS_GATE_MARKERS: [RoomMarkerPoint; 19] = [
    // Row 1 — full width
    pt(500, 170), // 0  Crash Landing
    // Row 2 — three
    pt(210, 253), // 1  Goblin Camp
    pt(500, 253), // 2  Emerald Grove
    pt(800, 253), // 3  Auntie's Teahouse
    // Row 3 — two
    pt(295, 336), // 4  Defiled Temple
    pt(730, 336), // 5  Mountain Pass
    // Row 4 — three
    pt(210, 440), // 6  Ebonlake Grotto
    pt(500, 440), // 7  Grymforge
    pt(800, 440), // 8  Githyanki Crèche
    // Row 5 — two
    pt(285, 545), // 9  Last Light Inn
    pt(700, 545), // 10 Reithwin Tollhouse
    // Row 6 — three
    pt(210, 622), // 11 Moonrise Towers
    pt(500, 622), // 12 Gauntlet of Shar
    pt(800, 622), // 13 Balthazar's Lab
    // Row 7 — two
    pt(300, 710), // 14 Circus of the Last Days
    pt(715, 710), // 15 Undercity Ruins
    // Row 8 — three (bottommost row)
    pt(190, 822), // 16 Steel Watch Foundry
    pt(500, 822), // 17 Ansur's Sanctum
    pt(810, 822), // 18 Temple of Bhaal
];

/// CR 309.1: The dungeon card's identity on Scryfall, so the client can show
/// the printed card the venture marker moves across.
///
/// A dungeon is a real printed card, not a synthesized object, so it has no
/// `printed_ref` on any battlefield object to borrow — a dungeon never enters
/// the battlefield (CR 309.3: it goes to the command zone). This is the only
/// place its printing identity exists, which is why it lives beside the room
/// tables rather than in the client.
///
/// Two lookup paths, because the five dungeons are not indexed uniformly by the
/// client's Scryfall sidecars (verified against the Scryfall API):
///
///   * Four dungeons are `layout: "normal"`, so `scryfall-data.json` keys them
///     by `oracle_id` like any other card.
///   * Undercity is `layout: "double_faced_token"` — it is printed as
///     `Undercity // The Initiative`, whose front face is the dungeon. That
///     layout is in `gen-scryfall-images.sh`'s NON_PLAYABLE exclusion list, so
///     it is absent from `scryfall-data.json` entirely and resolves ONLY
///     through `scryfall-token-images.json`, which is keyed by printing id.
///
/// Emitting both ids lets the client try the card table and fall back to the
/// token table without knowing which dungeon is which — the asymmetry stays
/// here, in the engine, instead of becoming a special case in the display layer.
pub struct DungeonCardRef {
    /// Scryfall `oracle_id` — the `scryfall-data.json` key.
    pub oracle_id: &'static str,
    /// Scryfall printing id of the token-set printing — the
    /// `scryfall-token-images.json` key. This is the `tafr`/`tclb` printing,
    /// whose art is identical at every image rung to its oversized `oafr`/`oclb`
    /// twin (both scan to 488x680 `normal` / 672x936 `large`), so nothing is
    /// lost by pinning one.
    pub scryfall_id: &'static str,
    /// The dungeon's own face name. Meaningful only for the double-faced
    /// Undercity, where it selects the dungeon face over `The Initiative`;
    /// for the single-faced dungeons it equals the card name.
    pub face_name: &'static str,
}

/// CR 309.1: The printed card behind each dungeon. Ids verified against the
/// Scryfall API rather than transcribed from memory.
pub fn card_ref(id: DungeonId) -> DungeonCardRef {
    match id {
        DungeonId::LostMineOfPhandelver => DungeonCardRef {
            oracle_id: "5c446a7f-0301-4343-b0df-146cf2db605b",
            scryfall_id: "59b11ff8-f118-4978-87dd-509dc0c8c932",
            face_name: "Lost Mine of Phandelver",
        },
        DungeonId::DungeonOfTheMadMage => DungeonCardRef {
            oracle_id: "30d0398c-bf5a-4b30-936f-afdb5a109b4b",
            scryfall_id: "6f509dbe-6ec7-4438-ab36-e20be46c9922",
            face_name: "Dungeon of the Mad Mage",
        },
        DungeonId::TombOfAnnihilation => DungeonCardRef {
            oracle_id: "d2ea2605-0ca0-4782-851d-e706bd0114e4",
            scryfall_id: "70b284bd-7a8f-4b60-8238-f746bdc5b236",
            face_name: "Tomb of Annihilation",
        },
        // CR 309.1: the only double-faced dungeon. `face_name` is what picks the
        // dungeon face out of `Undercity // The Initiative`.
        DungeonId::Undercity => DungeonCardRef {
            oracle_id: "36b61021-72f0-4c22-a41d-9b2f093d7ca8",
            scryfall_id: "2c65185b-6cf0-451d-985e-56aa45d9a57d",
            face_name: "Undercity",
        },
        DungeonId::BaldursGateWilderness => DungeonCardRef {
            oracle_id: "06b9590d-01bf-4fae-9837-352c9e04267a",
            scryfall_id: "a9d56324-8293-4500-a9ad-fed351ccf966",
            face_name: "Baldur's Gate Wilderness",
        },
    }
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

/// CR 309.4c: Get the printed effect text of a room's room ability.
pub fn room_text(id: DungeonId, room: u8) -> &'static str {
    let def = get_definition(id);
    def.rooms.get(room as usize).map_or("", |r| r.text)
}

/// CR 309.4: How many rooms the dungeon card has, so a display can place the
/// venture marker's room within the whole dungeon ("room 3 of 7").
pub fn room_count(id: DungeonId) -> u8 {
    get_definition(id).rooms.len() as u8
}

/// CR 309.4b + CR 309.4c: A room as the UI presents it — index, printed name,
/// and printed effect.
///
/// SINGLE AUTHORITY for "what does this room do" at the client boundary. Every
/// surface that names a room to a player (the dungeon prompt, the branch-point
/// room prompt, the dungeon HUD badge, the game log) reads this, so a room's
/// description can never disagree between surfaces. Per the display-layer rule,
/// the frontend renders these strings rather than carrying its own room table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPreview {
    /// Index of this room within its dungeon — the value a
    /// `GameAction::ChooseDungeonRoom` carries.
    pub index: u8,
    pub name: String,
    pub text: String,
}

/// CR 309.4b + CR 309.4c: Build the preview for one room of a dungeon.
pub fn room_preview(id: DungeonId, room: u8) -> RoomPreview {
    RoomPreview {
        index: room,
        name: room_name(id, room).to_string(),
        text: room_text(id, room).to_string(),
    }
}

/// CR 309.2a + CR 309.4a: A dungeon as the UI presents it at the point of
/// choosing one — its name plus the topmost room, which the venturing player
/// enters immediately and unavoidably on making this choice. Without
/// `entry_room` the choice is between five bare names with no visible
/// consequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DungeonPreview {
    pub dungeon: DungeonId,
    pub name: String,
    /// CR 309.4a: The topmost room — where the venture marker lands.
    pub entry_room: RoomPreview,
}

/// CR 309.2a + CR 309.4a: Build the preview for a choosable dungeon.
pub fn dungeon_preview(id: DungeonId) -> DungeonPreview {
    DungeonPreview {
        dungeon: id,
        name: get_definition(id).name.to_string(),
        entry_room: room_preview(id, 0),
    }
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

// ─── Room Effects ───────────────────────────────────────────────────────────

use crate::game::ability_utils::build_resolved_from_def;
use crate::parser::oracle_effect::parse_effect_chain;
use crate::types::ability::{
    AbilityCondition, AbilityKind, CardPlayMode, CastFromZoneDriver, CastingPermission,
    ContinuousModification, ControllerRef, Duration, Effect, FilterProp, PlayerFilter, PlayerScope,
    PtValue, QuantityExpr, ResolvedAbility, SearchSelectionConstraint, StaticDefinition,
    TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::Supertype;
use crate::types::counter::CounterType;
use crate::types::game_state::TargetSelectionConstraint;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// CR 309.4c: Build a room's triggered ability as a `ResolvedAbility`.
///
/// Room abilities are triggered abilities that fire when a player moves their
/// venture marker into a room. Returns the ability plus any target constraints
/// needed for the trigger system to handle target selection.
pub fn room_effects(
    dungeon: DungeonId,
    room: u8,
    source_id: ObjectId,
    controller: PlayerId,
) -> (ResolvedAbility, Vec<TargetSelectionConstraint>) {
    let (ability, constraints) = match (dungeon, room) {
        // ── Lost Mine of Phandelver ─────────────────────────────────────
        // 0: Cave Entrance — "Scry 1"
        (DungeonId::LostMineOfPhandelver, 0) => (
            simple(
                Effect::Scry {
                    count: fixed(1),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 1: Goblin Lair — "Create a 1/1 red Goblin creature token"
        (DungeonId::LostMineOfPhandelver, 1) => (
            simple(
                creature_token("Goblin", 1, 1, &["Goblin"], &[ManaColor::Red], &[], 1),
                source_id,
                controller,
            ),
            vec![],
        ),
        // 2: Mine Tunnels — "Create a Treasure token"
        (DungeonId::LostMineOfPhandelver, 2) => (
            simple(treasure_token(), source_id, controller),
            vec![],
        ),
        // 3: Storeroom — "Put a +1/+1 counter on target creature"
        // (any creature; the Oracle text is unrestricted, matching the sibling
        // counter-rooms Undercity "Forge" / "Throne of the Dead Three").
        (DungeonId::LostMineOfPhandelver, 3) => (
            simple(
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: fixed(1),
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 4: Dark Pool — "Each opponent loses 1 life. You gain 1 life."
        (DungeonId::LostMineOfPhandelver, 4) => {
            let mut lose = ResolvedAbility::new(
                Effect::LoseLife { amount: fixed(1), target: None },
                vec![],
                source_id,
                controller,
            );
            lose.player_scope = Some(PlayerFilter::Opponent);
            lose.sub_ability = Some(Box::new(simple(
                Effect::GainLife {
                    amount: fixed(1),
                    player: crate::types::ability::TargetFilter::Controller,
                },
                source_id,
                controller,
            )));
            (lose, vec![])
        }
        // 5: Fungi Cavern — "Target creature gets -4/-0 until your next turn"
        // CR 514.2 + CR 611.2a: the debuff persists until the *beginning* of the
        // controller's next turn, not just end of turn.
        (DungeonId::LostMineOfPhandelver, 5) => (
            ResolvedAbility::new(
                Effect::Pump {
                    power: PtValue::Fixed(-4),
                    toughness: PtValue::Fixed(0),
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                vec![],
                source_id,
                controller,
            )
            .duration(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }),
            vec![],
        ),
        // 6: Temple of Dumathoin — "Draw a card"
        (DungeonId::LostMineOfPhandelver, 6) => (
            simple(
                Effect::Draw {
                    count: fixed(1),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),

        // ── Dungeon of the Mad Mage ─────────────────────────────────────
        // 0: Yawning Portal — "You gain 1 life"
        (DungeonId::DungeonOfTheMadMage, 0) => (
            simple(
                Effect::GainLife {
                    amount: fixed(1),
                    player: crate::types::ability::TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 1: Dungeon Level — "Scry 1"
        (DungeonId::DungeonOfTheMadMage, 1) => (
            simple(
                Effect::Scry {
                    count: fixed(1),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 2: Goblin Bazaar — "Create a Treasure token"
        (DungeonId::DungeonOfTheMadMage, 2) => (
            simple(treasure_token(), source_id, controller),
            vec![],
        ),
        // 3: Twisted Caverns — "Target creature can't attack until your next turn"
        // CR 309.4c + CR 508.1c: Room ability applying a combat restriction.
        (DungeonId::DungeonOfTheMadMage, 3) => {
            let ability = simple(
                Effect::GenericEffect {
                    static_abilities: vec![StaticDefinition::continuous().modifications(vec![
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::CantAttack,
                        },
                    ])],
                    duration: None,
                    target: Some(TargetFilter::Typed(TypedFilter::creature())),
                    end_cost: None,
                },
                source_id,
                controller,
            )
            .duration(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            });
            (ability, vec![])
        }
        // 4: Lost Level — "Scry 2"
        (DungeonId::DungeonOfTheMadMage, 4) => (
            simple(
                Effect::Scry {
                    count: fixed(2),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 5: Runestone Caverns — "Exile the top two cards of your library. You may play them."
        // CR 309.4c + CR 406.3a: Room ability granting impulse draw (play from exile).
        (DungeonId::DungeonOfTheMadMage, 5) => {
            let mut exile = simple(
                Effect::ExileTop {
                    player: TargetFilter::Controller,
                    count: fixed(2),
                    position: crate::types::ability::LibraryPosition::Top,
                    face_down: false,
                },
                source_id,
                controller,
            );
            let grant = simple(
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        provenance: crate::types::ability::PlayFromExileProvenance::Impulse,
                        mode: crate::types::ability::CardPlayMode::Play,
                        duration: Duration::UntilEndOfTurn,
                        // Placeholder — rewritten to ability.controller at
                        // grant time by `grant_permission::resolve`.
                        granted_to: crate::types::player::PlayerId(0),
                        frequency: crate::types::statics::CastFrequency::Unlimited,
                        source_id: None,
                        invalidation: None,
                        exiled_by_ability_controller: None,
                        mana_spend_permission: None,
                        card_filter: None,
                        single_use_group: None,
                        single_use: false,
                        cast_cost_raise: None,
                        alt_ability_cost: None,
                        land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    },
                    target: TargetFilter::Any,
                    grantee: Default::default(),
                },
                source_id,
                controller,
            );
            exile.sub_ability = Some(Box::new(grant));
            (exile, vec![])
        }
        // 6: Muiral's Graveyard — "Create two 1/1 black Skeleton creature tokens"
        (DungeonId::DungeonOfTheMadMage, 6) => (
            simple(
                creature_token(
                    "Skeleton",
                    1,
                    1,
                    &["Skeleton"],
                    &[ManaColor::Black],
                    &[],
                    2,
                ),
                source_id,
                controller,
            ),
            vec![],
        ),
        // 7: Deep Mines — "Scry 3"
        (DungeonId::DungeonOfTheMadMage, 7) => (
            simple(
                Effect::Scry {
                    count: fixed(3),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 8: Mad Wizard's Lair — "Draw three cards and reveal them. You may cast one of them
        //    without paying its mana cost."
        (DungeonId::DungeonOfTheMadMage, 8) => (
            mad_wizards_lair(source_id, controller),
            vec![],
        ),

        // ── Tomb of Annihilation ────────────────────────────────────────
        // 0: Trapped Entry — "Each player loses 1 life"
        (DungeonId::TombOfAnnihilation, 0) => {
            let mut ability = simple(
                Effect::LoseLife { amount: fixed(1), target: None },
                source_id,
                controller,
            );
            ability.player_scope = Some(PlayerFilter::All);
            (ability, vec![])
        }
        // 1: Veils of Fear — "Each player loses 2 life unless they discard a card."
        // CR 118.12a: a punisher — each player either discards a card or loses
        // 2 life. Parsed from the Oracle text so the base life loss, the
        // each-player scope, and the unless-discard payment all come from the
        // shared punisher parser (the payer binds to the per-iteration
        // ScopedPlayer). The room was wrongly implemented as "each player
        // sacrifices a creature".
        (DungeonId::TombOfAnnihilation, 1) => {
            const ORACLE: &str = "Each player loses 2 life unless they discard a card.";
            let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
            (build_resolved_from_def(&def, source_id, controller), vec![])
        }
        // 2: Oubliette — "Discard a card and sacrifice a creature, an artifact,
        //    and a land."
        // CR 701.9a (discard) + CR 701.21a (sacrifice: the controller sacrifices
        // permanents they control). The controller discards a card, then
        // sacrifices one creature, one artifact, and one land, chained in
        // printed order via sub_ability. Each sacrifice is a count of one with
        // min_count 0 so it resolves to zero when the controller has no
        // permanent of that type (CR 701.21a: you can only sacrifice what you
        // control).
        (DungeonId::TombOfAnnihilation, 2) => {
            let sac = |filter: TypedFilter| Effect::Sacrifice {
                target: TargetFilter::Typed(filter),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            };
            let sac_land = simple(sac(TypedFilter::land()), source_id, controller);
            let sac_artifact =
                simple(sac(TypedFilter::new(TypeFilter::Artifact)), source_id, controller)
                    .sub_ability(sac_land);
            let sac_creature = simple(sac(TypedFilter::creature()), source_id, controller)
                .sub_ability(sac_artifact);
            let discard = simple(
                Effect::DiscardCard {
                    count: 1,
                    target: TargetFilter::Any,
                },
                source_id,
                controller,
            )
            .sub_ability(sac_creature);
            (discard, vec![])
        }
        // 3: Sandfall Cell — "Each player loses 2 life unless they sacrifice a
        //    creature, artifact, or land of their choice."
        // CR 118.12a: a punisher — each player either sacrifices a creature,
        // artifact, or land, or loses 2 life. Parsed from the Oracle text so the
        // base life loss, the each-player scope, and the unless-sacrifice
        // payment all come from the shared punisher parser (the payer binds to
        // the per-iteration ScopedPlayer). The room was wrongly implemented as
        // "you lose 2 life and create a 2/2 black Zombie token" — a fabricated
        // effect that appears nowhere on the card.
        (DungeonId::TombOfAnnihilation, 3) => {
            const ORACLE: &str =
                "Each player loses 2 life unless they sacrifice a creature, artifact, or land of their choice.";
            let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
            (build_resolved_from_def(&def, source_id, controller), vec![])
        }
        // 4: Cradle of the Death God — "Create The Atropal, a legendary 4/4 black God Horror
        //    creature token with deathtouch."
        // CR 702.2a: Deathtouch is a static ability granted to the token.
        (DungeonId::TombOfAnnihilation, 4) => (
            simple(
                Effect::Token {
                    name: "The Atropal".to_string(),
                    power: PtValue::Fixed(4),
                    toughness: PtValue::Fixed(4),
                    types: vec![
                        "Creature".to_string(),
                        "God".to_string(),
                        "Horror".to_string(),
                    ],
                    colors: vec![ManaColor::Black],
                    keywords: vec![Keyword::Deathtouch],
                    tapped: false,
                    count: fixed(1),
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    // CR 205.4a: The Atropal is legendary.
                    supertypes: vec![crate::types::card_type::Supertype::Legendary],
                    static_abilities: vec![],
                    enter_with_counters: vec![],
                },
                source_id,
                controller,
            ),
            vec![],
        ),

        // ── Undercity ───────────────────────────────────────────────────
        // 0: Secret Entrance — "Search your library for a basic land card, reveal it,
        //    put it into your hand, then shuffle."
        (DungeonId::Undercity, 0) => (
            search_basic_land(source_id, controller),
            vec![],
        ),
        // 1: Forge — "Put two +1/+1 counters on target creature"
        (DungeonId::Undercity, 1) => (
            simple(
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: fixed(2),
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 2: Lost Well — "Scry 2"
        (DungeonId::Undercity, 2) => (
            simple(
                Effect::Scry {
                    count: fixed(2),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 3: Trap! — "Target player loses 5 life"
        // CR 309.4c + CR 119.3: Room ability targeting a player for life loss.
        (DungeonId::Undercity, 3) => (
            simple(
                Effect::LoseLife {
                    amount: fixed(5),
                    target: Some(TargetFilter::Player),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 4: Arena — "Goad target creature"
        (DungeonId::Undercity, 4) => (
            simple(
                Effect::Goad {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 5: Stash — "Create a Treasure token"
        (DungeonId::Undercity, 5) => (
            simple(treasure_token(), source_id, controller),
            vec![],
        ),
        // 6: Archives — "Draw a card"
        (DungeonId::Undercity, 6) => (
            simple(
                Effect::Draw {
                    count: fixed(1),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 7: Catacombs — "Create a 4/1 black Skeleton creature token with menace"
        (DungeonId::Undercity, 7) => (
            simple(
                creature_token(
                    "Skeleton",
                    4,
                    1,
                    &["Skeleton"],
                    &[ManaColor::Black],
                    &[Keyword::Menace],
                    1,
                ),
                source_id,
                controller,
            ),
            vec![],
        ),
        // 8: Throne of the Dead Three — reveal 10, put creature with counters, hexproof, shuffle
        (DungeonId::Undercity, 8) => (
            throne_of_dead_three(source_id, controller),
            vec![],
        ),

        // ── Baldur's Gate Wilderness ────────────────────────────────────
        // 0: Crash Landing — "Search your library for a basic land card, reveal it,
        //    put it into your hand, then shuffle."
        (DungeonId::BaldursGateWilderness, 0) => (
            search_basic_land(source_id, controller),
            vec![],
        ),
        // 1: Goblin Camp — "Create a Treasure token"
        (DungeonId::BaldursGateWilderness, 1) => (
            simple(treasure_token(), source_id, controller),
            vec![],
        ),
        // 2: Emerald Grove — "Create a 2/2 white Knight creature token"
        (DungeonId::BaldursGateWilderness, 2) => (
            simple(
                creature_token("Knight", 2, 2, &["Knight"], &[ManaColor::White], &[], 1),
                source_id,
                controller,
            ),
            vec![],
        ),
        // 3: Auntie's Teahouse — "Scry 3"
        (DungeonId::BaldursGateWilderness, 3) => (
            simple(
                Effect::Scry {
                    count: fixed(3),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 4: Defiled Temple — "You may sacrifice a permanent. If you do, draw a card."
        (DungeonId::BaldursGateWilderness, 4) => {
            let mut ability = simple(
                Effect::Sacrifice {
                    target: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
                source_id,
                controller,
            );
            ability.optional = true;

            let mut draw = ResolvedAbility::new(
                Effect::Draw {
                    count: fixed(1),
                    target: TargetFilter::Controller,
                },
                vec![],
                source_id,
                controller,
            );
            draw.condition = Some(AbilityCondition::effect_performed());
            ability.sub_ability = Some(Box::new(draw));

            (ability, vec![])
        }
        // 5: Mountain Pass — "You may put a land card from your hand onto the battlefield."
        (DungeonId::BaldursGateWilderness, 5) => {
            let mut ability = simple(
                Effect::ChangeZone {
                    origin: Some(Zone::Hand),
                    destination: Zone::Battlefield,
                    target: TargetFilter::Typed(TypedFilter::land()),
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                 enters_modified_if: None },
                source_id,
                controller,
            );
            ability.optional = true;
            (ability, vec![])
        }
        // 6: Ebonlake Grotto — "Create two 1/1 blue Faerie Dragon creature tokens with flying"
        (DungeonId::BaldursGateWilderness, 6) => (
            simple(
                creature_token(
                    "Faerie Dragon",
                    1,
                    1,
                    &["Faerie", "Dragon"],
                    &[ManaColor::Blue],
                    &[Keyword::Flying],
                    2,
                ),
                source_id,
                controller,
            ),
            vec![],
        ),
        // 7: Grymforge — "For each opponent, goad up to one target creature that player controls."
        (DungeonId::BaldursGateWilderness, 7) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Grymforge".to_string(),
                    description: Some(
                        "For each opponent, goad up to one target creature that player controls."
                            .to_string(),
                    ),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 8: Githyanki Crèche — "Distribute three +1/+1 counters among up to three target
        //    creatures you control."
        (DungeonId::BaldursGateWilderness, 8) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Githyanki Crèche".to_string(),
                    description: Some(
                        "Distribute three +1/+1 counters among up to three target creatures you control."
                            .to_string(),
                    ),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 9: Last Light Inn — "Draw two cards"
        (DungeonId::BaldursGateWilderness, 9) => (
            simple(
                Effect::Draw {
                    count: fixed(2),
                    target: TargetFilter::Controller,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 10: Reithwin Tollhouse — "Roll 2d4 and create that many Treasure tokens."
        (DungeonId::BaldursGateWilderness, 10) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Reithwin Tollhouse".to_string(),
                    description: Some(
                        "Roll 2d4 and create that many Treasure tokens.".to_string(),
                    ),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 11: Moonrise Towers — "Instant and sorcery spells you cast this turn cost {3} less
        //     to cast."
        (DungeonId::BaldursGateWilderness, 11) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Moonrise Towers".to_string(),
                    description: Some(
                        "Instant and sorcery spells you cast this turn cost {3} less to cast."
                            .to_string(),
                    ),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 12: Gauntlet of Shar — "Each opponent loses 5 life"
        (DungeonId::BaldursGateWilderness, 12) => {
            let mut ability =
                simple(Effect::LoseLife { amount: fixed(5), target: None }, source_id, controller);
            ability.player_scope = Some(PlayerFilter::Opponent);
            (ability, vec![])
        }
        // 13: Balthazar's Lab — "Return up to two target creature cards from your graveyard
        //     to your hand."
        (DungeonId::BaldursGateWilderness, 13) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Balthazar's Lab".to_string(),
                    description: Some(
                        "Return up to two target creature cards from your graveyard to your hand."
                            .to_string(),
                    ),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 14: Circus of the Last Days — "Create a token that's a copy of one of your
        //     commanders, except it's not legendary."
        (DungeonId::BaldursGateWilderness, 14) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Circus of the Last Days".to_string(),
                    description: Some(
                        "Create a token that's a copy of one of your commanders, except it's not legendary."
                            .to_string(),
                    ),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 15: Undercity Ruins — "Create three 4/1 black Skeleton creature tokens with menace"
        (DungeonId::BaldursGateWilderness, 15) => (
            simple(
                creature_token(
                    "Skeleton",
                    4,
                    1,
                    &["Skeleton"],
                    &[ManaColor::Black],
                    &[Keyword::Menace],
                    3,
                ),
                source_id,
                controller,
            ),
            vec![],
        ),
        // 16: Steel Watch Foundry — "You get an emblem with 'Creatures you control get +2/+2
        //     and have trample.'"
        (DungeonId::BaldursGateWilderness, 16) => (
            simple(
                Effect::CreateEmblem {
                    statics: vec![StaticDefinition {
                        mode: StaticMode::Continuous,
                        affected: Some(TargetFilter::Typed(
                            TypedFilter::creature().controller(ControllerRef::You),
                        )),
                        modifications: vec![
                            ContinuousModification::AddPower { value: 2 },
                            ContinuousModification::AddToughness { value: 2 },
                            ContinuousModification::AddKeyword {
                                keyword: Keyword::Trample,
                            },
                        ],
                        condition: None,
                        per_player_condition: None,
                        affected_zone: None,
                        effect_zone: None,
                        active_zones: vec![],
                        characteristic_defining: false,
                        description: Some(
                            "Creatures you control get +2/+2 and have trample.".to_string(),
                        ),
                        attack_defended: None,
                        source_controller: None,
                        source_object: None,
                        bypass_beneficiary: None,
                        protection_does_not_remove: None,
                        room_door: None,
                    }],
                    triggers: Vec::new(),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 17: Ansur's Sanctum — "Reveal the top four cards of your library. Put those cards
        //     into your hand. Each opponent loses life equal to the total mana value of
        //     those cards."
        (DungeonId::BaldursGateWilderness, 17) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Ansur's Sanctum".to_string(),
                    description: Some(
                        "Reveal the top four cards of your library. Put those cards into your hand. Each opponent loses life equal to the total mana value of those cards."
                            .to_string(),
                    ),
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 18: Temple of Bhaal — "Creatures your opponents control get -5/-5 until end of turn"
        (DungeonId::BaldursGateWilderness, 18) => (
            ResolvedAbility::new(
                Effect::PumpAll {
                    power: PtValue::Fixed(-5),
                    toughness: PtValue::Fixed(-5),
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent),
                    ),
                },
                vec![],
                source_id,
                controller,
            )
            .duration(Duration::UntilEndOfTurn),
            vec![],
        ),

        // ── Fallback for out-of-bounds room indices ────────────────────
        _ => (
            simple(
                Effect::Unimplemented {
                    name: format!("Room: {}", room_name(dungeon, room)),
                    description: None,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
    };
    (ability, constraints)
}

/// Shorthand: build a simple `ResolvedAbility` with no sub-abilities.
fn simple(effect: Effect, source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(effect, vec![], source_id, controller)
}

/// Shorthand: `QuantityExpr::Fixed`.
fn fixed(value: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value }
}

/// Build a standard creature token Effect.
/// `subtypes` are creature subtypes (e.g., "Goblin", "Zombie").
/// The `types` field in `Effect::Token` combines core types + subtypes,
/// and the resolver separates them (see `build_token_attrs_from_effect`).
fn creature_token(
    name: &str,
    power: i32,
    toughness: i32,
    subtypes: &[&str],
    colors: &[ManaColor],
    keywords: &[Keyword],
    count: i32,
) -> Effect {
    let mut types = vec!["Creature".to_string()];
    for sub in subtypes {
        types.push((*sub).to_string());
    }
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(power),
        toughness: PtValue::Fixed(toughness),
        types,
        colors: colors.to_vec(),
        keywords: keywords.to_vec(),
        tapped: false,
        count: fixed(count),
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

/// Undercity room 8 (Throne of the Dead Three): reveal 10, put a creature from
/// among them onto the battlefield with three +1/+1 counters, grant hexproof
/// until your next turn, then shuffle.
fn throne_of_dead_three(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    const ORACLE: &str = "Reveal the top ten cards of your library. Put a creature card from among them onto the battlefield with three +1/+1 counters on it. It gains hexproof until your next turn. Then shuffle.";
    let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
    patch_throne_parsed_chain(build_resolved_from_def(&def, source_id, controller))
}

/// CR 309.4c: Dungeon of the Mad Mage room 8 (Mad Wizard's Lair) — draw three,
/// reveal them, optionally cast one without paying its mana cost.
fn mad_wizards_lair(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let mut draw = simple(
        Effect::Draw {
            count: fixed(3),
            target: TargetFilter::Controller,
        },
        source_id,
        controller,
    );

    let reveal = simple(
        Effect::Reveal {
            target: TargetFilter::Any,
        },
        source_id,
        controller,
    );

    let mut cast = simple(
        Effect::CastFromZone {
            target: TargetFilter::LastRevealed,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::DuringResolution,
            mana_spend_permission: None,
        },
        source_id,
        controller,
    );
    cast.optional = true;

    let mut reveal = reveal;
    reveal.sub_ability = Some(Box::new(cast));
    draw.sub_ability = Some(Box::new(reveal));
    draw
}

/// The oracle parser covers reveal / hexproof / shuffle but the dig-from-among form
/// does not lift "with N +1/+1 counters on it" onto `Effect::Dig`. Wire the counter
/// clause explicitly and retarget the hexproof grant at the dug creature (CR 608.2c).
fn patch_throne_parsed_chain(mut root: ResolvedAbility) -> ResolvedAbility {
    if !resolved_chain_contains(&root, |effect| {
        matches!(
            effect,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                ..
            }
        )
    }) {
        let tail = root.sub_ability.take();
        let mut counters = simple(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: fixed(3),
                target: TargetFilter::ParentTarget,
            },
            root.source_id,
            root.controller,
        );
        counters.sub_ability = tail;
        root.sub_ability = Some(Box::new(counters));
    }
    retarget_hexproof_to_parent(&mut root);
    root
}

fn resolved_chain_contains(
    ability: &ResolvedAbility,
    mut pred: impl FnMut(&Effect) -> bool,
) -> bool {
    if pred(&ability.effect) {
        return true;
    }
    ability
        .sub_ability
        .as_ref()
        .is_some_and(|sub| resolved_chain_contains(sub, pred))
}

fn retarget_hexproof_to_parent(ability: &mut ResolvedAbility) {
    if let Effect::GenericEffect {
        static_abilities, ..
    } = &mut ability.effect
    {
        for st in static_abilities.iter_mut() {
            if matches!(st.affected, Some(TargetFilter::SelfRef)) {
                st.affected = Some(TargetFilter::ParentTarget);
            }
        }
    }
    if let Some(sub) = ability.sub_ability.as_mut() {
        retarget_hexproof_to_parent(sub);
    }
}

/// Build a "search for a basic land, reveal, put into hand, shuffle" ability chain.
/// CR 701.18a: Search → ChangeZone(Library→Hand) → Shuffle.
/// Shared by Undercity room 0 (Secret Entrance) and BGW room 0 (Crash Landing).
fn search_basic_land(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    let mut search = simple(
        Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Land],
                controller: None,
                properties: vec![FilterProp::HasSupertype {
                    value: Supertype::Basic,
                }],
            }),
            count: fixed(1),
            reveal: true,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
            source_zones: vec![crate::types::zones::Zone::Library],
        },
        source_id,
        controller,
    );
    let mut change_zone = simple(
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        source_id,
        controller,
    );
    change_zone.sub_ability = Some(Box::new(simple(
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
        source_id,
        controller,
    )));
    search.sub_ability = Some(Box::new(change_zone));
    search
}

/// Build a Treasure artifact token Effect.
fn treasure_token() -> Effect {
    Effect::Token {
        name: "Treasure".to_string(),
        power: PtValue::Fixed(0),
        toughness: PtValue::Fixed(0),
        types: vec!["Artifact".to_string(), "Treasure".to_string()],
        colors: vec![],
        keywords: vec![],
        tapped: false,
        count: fixed(1),
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

// ─── Dungeon Graph Data ──────────────────────────────────────────────────────
//
// Each dungeon is a directed acyclic graph of rooms. Room 0 is always the
// topmost room. Rooms with empty `next_rooms` are bottommost rooms.

/// Lost Mine of Phandelver (Adventures in the Forgotten Realms)
/// 7 rooms, 2 branch points.
static LOST_MINE_OF_PHANDELVER: DungeonDefinition = DungeonDefinition {
    id: DungeonId::LostMineOfPhandelver,
    name: "Lost Mine of Phandelver",
    rooms: &[
        // 0: Cave Entrance → {Goblin Lair, Mine Tunnels}
        RoomDefinition {
            name: "Cave Entrance",
            text: "Scry 1.",
            next_rooms: &[1, 2],
        },
        // 1: Goblin Lair → {Storeroom, Dark Pool}
        RoomDefinition {
            name: "Goblin Lair",
            text: "Create a 1/1 red Goblin creature token.",
            next_rooms: &[3, 4],
        },
        // 2: Mine Tunnels → {Dark Pool, Fungi Cavern}
        RoomDefinition {
            name: "Mine Tunnels",
            text: "Create a Treasure token.",
            next_rooms: &[4, 5],
        },
        // 3: Storeroom → Temple of Dumathoin
        RoomDefinition {
            name: "Storeroom",
            text: "Put a +1/+1 counter on target creature.",
            next_rooms: &[6],
        },
        // 4: Dark Pool → Temple of Dumathoin
        RoomDefinition {
            name: "Dark Pool",
            text: "Each opponent loses 1 life and you gain 1 life.",
            next_rooms: &[6],
        },
        // 5: Fungi Cavern → Temple of Dumathoin
        RoomDefinition {
            name: "Fungi Cavern",
            text: "Target creature gets -4/-0 until your next turn.",
            next_rooms: &[6],
        },
        // 6: Temple of Dumathoin (bottommost)
        RoomDefinition {
            name: "Temple of Dumathoin",
            text: "Draw a card.",
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
            text: "You gain 1 life.",
            next_rooms: &[1],
        },
        // 1: Dungeon Level → {Goblin Bazaar, Twisted Caverns}
        RoomDefinition {
            name: "Dungeon Level",
            text: "Scry 1.",
            next_rooms: &[2, 3],
        },
        // 2: Goblin Bazaar → Lost Level
        RoomDefinition {
            name: "Goblin Bazaar",
            text: "Create a Treasure token.",
            next_rooms: &[4],
        },
        // 3: Twisted Caverns → Lost Level
        RoomDefinition {
            name: "Twisted Caverns",
            text: "Target creature can't attack until your next turn.",
            next_rooms: &[4],
        },
        // 4: Lost Level → {Runestone Caverns, Muiral's Graveyard}
        RoomDefinition {
            name: "Lost Level",
            text: "Scry 2.",
            next_rooms: &[5, 6],
        },
        // 5: Runestone Caverns → Deep Mines
        RoomDefinition {
            name: "Runestone Caverns",
            text: "Exile the top two cards of your library. You may play them.",
            next_rooms: &[7],
        },
        // 6: Muiral's Graveyard → Deep Mines
        RoomDefinition {
            name: "Muiral's Graveyard",
            text: "Create two 1/1 black Skeleton creature tokens.",
            next_rooms: &[7],
        },
        // 7: Deep Mines → Mad Wizard's Lair
        RoomDefinition {
            name: "Deep Mines",
            text: "Scry 3.",
            next_rooms: &[8],
        },
        // 8: Mad Wizard's Lair (bottommost)
        RoomDefinition {
            name: "Mad Wizard's Lair",
            text: "Draw three cards and reveal them. You may cast one of them without paying its mana cost.",
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
            text: "Each player loses 1 life.",
            next_rooms: &[1, 2],
        },
        // 1: Veils of Fear → Sandfall Cell
        RoomDefinition {
            name: "Veils of Fear",
            text: "Each player loses 2 life unless they discard a card.",
            next_rooms: &[3],
        },
        // 2: Oubliette → Cradle of the Death God
        RoomDefinition {
            name: "Oubliette",
            text: "Discard a card and sacrifice a creature, an artifact, and a land.",
            next_rooms: &[4],
        },
        // 3: Sandfall Cell → Cradle of the Death God
        RoomDefinition {
            name: "Sandfall Cell",
            text: "Each player loses 2 life unless they sacrifice a creature, artifact, or land of their choice.",
            next_rooms: &[4],
        },
        // 4: Cradle of the Death God (bottommost)
        RoomDefinition {
            name: "Cradle of the Death God",
            text: "Create The Atropal, a legendary 4/4 black God Horror creature token with deathtouch.",
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
            text: "Search your library for a basic land card, reveal it, put it into your hand, then shuffle.",
            next_rooms: &[1, 2],
        },
        // 1: Forge → {Trap!, Arena}
        RoomDefinition {
            name: "Forge",
            text: "Put two +1/+1 counters on target creature.",
            next_rooms: &[3, 4],
        },
        // 2: Lost Well → {Arena, Stash}
        RoomDefinition {
            name: "Lost Well",
            text: "Scry 2.",
            next_rooms: &[4, 5],
        },
        // 3: Trap! → Archives
        RoomDefinition {
            name: "Trap!",
            text: "Target player loses 5 life.",
            next_rooms: &[6],
        },
        // 4: Arena → {Archives, Catacombs}
        RoomDefinition {
            name: "Arena",
            text: "Goad target creature.",
            next_rooms: &[6, 7],
        },
        // 5: Stash → Catacombs
        RoomDefinition {
            name: "Stash",
            text: "Create a Treasure token.",
            next_rooms: &[7],
        },
        // 6: Archives → Throne of the Dead Three
        RoomDefinition {
            name: "Archives",
            text: "Draw a card.",
            next_rooms: &[8],
        },
        // 7: Catacombs → Throne of the Dead Three
        RoomDefinition {
            name: "Catacombs",
            text: "Create a 4/1 black Skeleton creature token with menace.",
            next_rooms: &[8],
        },
        // 8: Throne of the Dead Three (bottommost)
        RoomDefinition {
            name: "Throne of the Dead Three",
            text: "Reveal the top ten cards of your library. Put a creature card from among them onto the battlefield with three +1/+1 counters on it. It gains hexproof until your next turn. Then shuffle.",
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
            text: "Search your library for a basic land card, reveal it, put it into your hand, then shuffle.",
            next_rooms: &[1, 2],
        },
        // Row 2
        // 1: Goblin Camp → {Auntie's Teahouse, Defiled Temple}
        RoomDefinition {
            name: "Goblin Camp",
            text: "Create a Treasure token.",
            next_rooms: &[3, 4],
        },
        // 2: Emerald Grove → {Defiled Temple, Mountain Pass}
        RoomDefinition {
            name: "Emerald Grove",
            text: "Create a 2/2 white Knight creature token.",
            next_rooms: &[4, 5],
        },
        // Row 3
        // 3: Auntie's Teahouse → {Ebonlake Grotto, Grymforge}
        RoomDefinition {
            name: "Auntie's Teahouse",
            text: "Scry 3.",
            next_rooms: &[6, 7],
        },
        // 4: Defiled Temple → {Grymforge, Githyanki Crèche}
        RoomDefinition {
            name: "Defiled Temple",
            text: "You may sacrifice a permanent. If you do, draw a card.",
            next_rooms: &[7, 8],
        },
        // 5: Mountain Pass → {Githyanki Crèche, Last Light Inn}
        RoomDefinition {
            name: "Mountain Pass",
            text: "You may put a land card from your hand onto the battlefield.",
            next_rooms: &[8, 9],
        },
        // Row 4 (widest)
        // 6: Ebonlake Grotto → {Reithwin Tollhouse, Moonrise Towers}
        RoomDefinition {
            name: "Ebonlake Grotto",
            text: "Create two 1/1 blue Faerie Dragon creature tokens with flying.",
            next_rooms: &[10, 11],
        },
        // 7: Grymforge → {Reithwin Tollhouse, Moonrise Towers}
        RoomDefinition {
            name: "Grymforge",
            text: "For each opponent, goad up to one target creature that player controls.",
            next_rooms: &[10, 11],
        },
        // 8: Githyanki Crèche → {Moonrise Towers, Gauntlet of Shar}
        RoomDefinition {
            name: "Githyanki Crèche",
            text: "Distribute three +1/+1 counters among up to three target creatures you control.",
            next_rooms: &[11, 12],
        },
        // 9: Last Light Inn → {Moonrise Towers, Gauntlet of Shar}
        RoomDefinition {
            name: "Last Light Inn",
            text: "Draw two cards.",
            next_rooms: &[11, 12],
        },
        // Row 5
        // 10: Reithwin Tollhouse → {Balthazar's Lab, Circus of the Last Days}
        RoomDefinition {
            name: "Reithwin Tollhouse",
            text: "Roll 2d4 and create that many Treasure tokens.",
            next_rooms: &[13, 14],
        },
        // 11: Moonrise Towers → {Circus of the Last Days, Undercity Ruins}
        RoomDefinition {
            name: "Moonrise Towers",
            text: "Instant and sorcery spells you cast this turn cost {3} less to cast.",
            next_rooms: &[14, 15],
        },
        // 12: Gauntlet of Shar → {Undercity Ruins}
        RoomDefinition {
            name: "Gauntlet of Shar",
            text: "Each opponent loses 5 life.",
            next_rooms: &[15],
        },
        // Row 6
        // 13: Balthazar's Lab → {Steel Watch Foundry}
        RoomDefinition {
            name: "Balthazar's Lab",
            text: "Return up to two target creature cards from your graveyard to your hand.",
            next_rooms: &[16],
        },
        // 14: Circus of the Last Days → {Steel Watch Foundry, Ansur's Sanctum}
        RoomDefinition {
            name: "Circus of the Last Days",
            text: "Create a token that's a copy of one of your commanders, except it's not legendary.",
            next_rooms: &[16, 17],
        },
        // 15: Undercity Ruins → {Ansur's Sanctum}
        RoomDefinition {
            name: "Undercity Ruins",
            text: "Create three 4/1 black Skeleton creature tokens with menace.",
            next_rooms: &[17],
        },
        // Row 7
        // 16: Steel Watch Foundry → Temple of Bhaal
        RoomDefinition {
            name: "Steel Watch Foundry",
            text: "You get an emblem with \"Creatures you control get +2/+2 and have trample.\"",
            next_rooms: &[18],
        },
        // 17: Ansur's Sanctum → Temple of Bhaal
        RoomDefinition {
            name: "Ansur's Sanctum",
            text: "Reveal the top four cards of your library and put them into your hand. Each opponent loses life equal to those cards' total mana value.",
            next_rooms: &[18],
        },
        // Row 8 (bottom)
        // 18: Temple of Bhaal (bottommost)
        RoomDefinition {
            name: "Temple of Bhaal",
            text: "Creatures your opponents control get -5/-5 until end of turn.",
            next_rooms: &[],
        },
    ],
};

// ─── Tests ───────────────────────────────────────────────────────────────────

/// CR 309.7: "A player completes a dungeon as that dungeon card is removed from
/// the game." True when `player` has completed at least one dungeon
/// (`specific: None`) or the named dungeon (`specific: Some(d)`).
///
/// SINGLE AUTHORITY for the "have you completed a dungeon" predicate. The same
/// printed clause is read two ways — as a triggered ability's intervening-if
/// (`TriggerCondition::CompletedDungeon`) and at resolution
/// (`AbilityCondition::CompletedDungeon`, e.g. Tomb of Horrors Adventurer's "If
/// you've completed a dungeon, copy that spell twice instead"). Both delegate
/// here so the two readings can never disagree about what "completed" means.
pub fn has_completed_dungeon(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    specific: &Option<DungeonId>,
) -> bool {
    state
        .dungeon_progress
        .get(&player)
        .is_some_and(|progress| match specific {
            None => !progress.completed.is_empty(),
            Some(dungeon) => progress.completed.contains(dungeon),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_DUNGEONS: [DungeonId; 5] = [
        DungeonId::LostMineOfPhandelver,
        DungeonId::DungeonOfTheMadMage,
        DungeonId::TombOfAnnihilation,
        DungeonId::Undercity,
        DungeonId::BaldursGateWilderness,
    ];

    /// CR 309.4c: Every room has a room ability, so every room must carry its
    /// printed effect — the prompts, stack entry, and game log all render it.
    #[test]
    fn every_room_has_printed_effect_text() {
        for id in ALL_DUNGEONS {
            let def = get_definition(id);
            for (index, room) in def.rooms.iter().enumerate() {
                assert!(
                    !room.text.is_empty(),
                    "{id} room {index} ({}) has no printed effect text",
                    room.name
                );
                assert!(
                    !room.text.contains("Leads to"),
                    "{id} room {index} ({}) kept the (Leads to: ...) reminder; \
                     next_rooms already encodes it",
                    room.name
                );
            }
        }
    }

    /// The previews handed to the client must agree with the static definitions
    /// they are built from — the client has no room table of its own.
    #[test]
    fn previews_mirror_the_static_definitions() {
        for id in ALL_DUNGEONS {
            let def = get_definition(id);

            let dungeon = dungeon_preview(id);
            assert_eq!(dungeon.dungeon, id);
            assert_eq!(dungeon.name, def.name);
            // CR 309.4a: the entry room is always the topmost room.
            assert_eq!(dungeon.entry_room, room_preview(id, 0));

            for index in 0..def.rooms.len() as u8 {
                let preview = room_preview(id, index);
                assert_eq!(preview.index, index);
                assert_eq!(preview.name, room_name(id, index));
                assert_eq!(preview.text, room_text(id, index));
            }
        }
    }

    /// Spot-check the Oracle text of one room per dungeon against the printed
    /// card, so a bad bulk edit cannot silently shift every room's description.
    #[test]
    fn room_text_matches_printed_oracle_text() {
        assert_eq!(
            room_text(DungeonId::LostMineOfPhandelver, 4),
            "Each opponent loses 1 life and you gain 1 life.",
            "Lost Mine — Dark Pool"
        );
        assert_eq!(
            room_text(DungeonId::DungeonOfTheMadMage, 8),
            "Draw three cards and reveal them. You may cast one of them without paying its mana cost.",
            "Mad Mage — Mad Wizard's Lair"
        );
        assert_eq!(
            room_text(DungeonId::TombOfAnnihilation, 0),
            "Each player loses 1 life.",
            "Tomb — Trapped Entry"
        );
        assert_eq!(
            room_text(DungeonId::Undercity, 3),
            "Target player loses 5 life.",
            "Undercity — Trap!"
        );
        assert_eq!(
            room_text(DungeonId::BaldursGateWilderness, 12),
            "Each opponent loses 5 life.",
            "Baldur's Gate Wilderness — Gauntlet of Shar"
        );
    }

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

    /// Oracle fidelity: Lost Mine of Phandelver "Storeroom" is "Put a +1/+1
    /// counter on target creature" — ANY creature, no controller restriction
    /// (matching the sibling unrestricted counter-rooms). Revert-probe: the old
    /// `.controller(You)` sets `tf.controller = Some(You)`.
    #[test]
    fn lost_mine_storeroom_targets_any_creature() {
        let (ability, _) =
            room_effects(DungeonId::LostMineOfPhandelver, 3, ObjectId(1), PlayerId(0));
        match &ability.effect {
            Effect::PutCounter {
                target: TargetFilter::Typed(tf),
                counter_type,
                ..
            } => {
                assert_eq!(*counter_type, CounterType::Plus1Plus1);
                assert!(
                    tf.controller.is_none(),
                    "Storeroom targets ANY creature (Oracle: 'target creature'), got controller {:?}",
                    tf.controller
                );
            }
            other => panic!("expected PutCounter, got {other:?}"),
        }
    }

    /// Oracle fidelity: Lost Mine of Phandelver "Fungi Cavern" is "-4/-0 until
    /// your next turn" (CR 514.2 + CR 611.2a) — the debuff persists to the
    /// controller's next turn, not just end of turn. Revert-probe: the old
    /// `Duration::UntilEndOfTurn`.
    #[test]
    fn lost_mine_fungi_cavern_lasts_until_your_next_turn() {
        let (ability, _) =
            room_effects(DungeonId::LostMineOfPhandelver, 5, ObjectId(1), PlayerId(0));
        assert_eq!(
            ability.duration,
            Some(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }),
            "Fungi Cavern's -4/-0 lasts until your next turn, not end of turn"
        );
    }

    /// Oracle fidelity: Dungeon of the Mad Mage "Lost Level" is "Scry 2". It was
    /// stubbed as `Effect::Unimplemented` with fabricated text ("Destroy target
    /// creature of an opponent's choice"), so venturing into it did nothing.
    /// Revert-probe: the old stub is not `Effect::Scry`.
    #[test]
    fn mad_mage_lost_level_is_scry_two() {
        let (ability, _) =
            room_effects(DungeonId::DungeonOfTheMadMage, 4, ObjectId(1), PlayerId(0));
        match &ability.effect {
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            } => {}
            other => panic!("expected 'Scry 2' targeting the controller, got {other:?}"),
        }
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

    // ── Undercity room effect tests ─────────────────────────────────────

    fn undercity_effect(room: u8) -> ResolvedAbility {
        room_effects(DungeonId::Undercity, room, ObjectId(1), PlayerId(0)).0
    }

    fn assert_no_unimplemented_resolved(ability: &ResolvedAbility) {
        assert!(
            !matches!(ability.effect, Effect::Unimplemented { .. }),
            "unexpected Unimplemented effect: {:?}",
            ability.effect
        );
        if let Some(sub) = ability.sub_ability.as_ref() {
            assert_no_unimplemented_resolved(sub);
        }
    }

    fn assert_throne_room_chain(ability: &ResolvedAbility) {
        assert_no_unimplemented_resolved(ability);
        assert!(
            matches!(
                ability.effect,
                Effect::Dig {
                    reveal: true,
                    count: QuantityExpr::Fixed { value: 10 },
                    ..
                }
            ),
            "Throne must reveal top ten, got {:?}",
            ability.effect
        );
        assert!(
            super::resolved_chain_contains(ability, |effect| matches!(
                effect,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::ParentTarget,
                }
            )),
            "Throne must put three +1/+1 counters on the dug creature, got {:?}",
            ability
        );
        assert!(
            super::resolved_chain_contains(ability, |effect| {
                matches!(effect, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|st| {
                    matches!(st.affected, Some(TargetFilter::ParentTarget))
                        && st.modifications.iter().any(|m| matches!(
                            m,
                            ContinuousModification::AddKeyword {
                                keyword: Keyword::Hexproof
                            }
                        ))
                }))
            }),
            "Throne must grant hexproof to the dug creature, got {:?}",
            ability
        );
        assert!(
            super::resolved_chain_contains(ability, |effect| matches!(
                effect,
                Effect::Shuffle {
                    target: TargetFilter::Controller,
                    ..
                }
            )),
            "Throne must shuffle the controller's library, got {:?}",
            ability
        );
    }

    #[test]
    fn room_effects_undercity_throne_of_dead_three() {
        assert_throne_room_chain(&undercity_effect(8));
    }

    fn tomb_effect(room: u8) -> ResolvedAbility {
        room_effects(
            DungeonId::TombOfAnnihilation,
            room,
            ObjectId(1),
            PlayerId(0),
        )
        .0
    }

    /// Oracle fidelity: Tomb of Annihilation "Cradle of the Death God" creates
    /// The Atropal, a legendary 4/4 black God Horror creature token *with
    /// deathtouch* (CR 702.2a). The token was created with no keywords, dropping
    /// the deathtouch. Revert-probe: the old `keywords: vec![]` has no Deathtouch.
    #[test]
    fn tomb_cradle_atropal_has_deathtouch() {
        let ability = tomb_effect(4);
        match &ability.effect {
            Effect::Token {
                name,
                power: PtValue::Fixed(4),
                toughness: PtValue::Fixed(4),
                keywords,
                supertypes,
                ..
            } => {
                assert_eq!(name, "The Atropal");
                assert!(
                    keywords.contains(&Keyword::Deathtouch),
                    "The Atropal is created with deathtouch, got keywords {keywords:?}"
                );
                assert!(
                    supertypes.contains(&Supertype::Legendary),
                    "The Atropal is legendary"
                );
            }
            other => panic!("expected a 4/4 Atropal Token, got {other:?}"),
        }
    }

    fn mad_mage_effect(room: u8) -> ResolvedAbility {
        room_effects(
            DungeonId::DungeonOfTheMadMage,
            room,
            ObjectId(1),
            PlayerId(0),
        )
        .0
    }

    #[test]
    fn room_effects_mad_mage_deep_mines_scry() {
        let ability = mad_mage_effect(7);
        assert_no_unimplemented_resolved(&ability);
        assert!(
            matches!(
                ability.effect,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 3 },
                    ..
                }
            ),
            "Deep Mines must scry 3, got {:?}",
            ability.effect
        );
    }

    #[test]
    fn room_effects_mad_wizards_lair_draw_reveal_free_cast() {
        let ability = mad_mage_effect(8);
        assert_no_unimplemented_resolved(&ability);
        assert!(
            matches!(
                ability.effect,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 3 },
                    ..
                }
            ),
            "Mad Wizard's Lair must draw three, got {:?}",
            ability.effect
        );
        assert!(
            resolved_chain_contains(&ability, |effect| {
                matches!(effect, Effect::Reveal { .. })
            }),
            "Mad Wizard's Lair must reveal the drawn cards, got {:?}",
            ability
        );
        assert!(
            resolved_chain_contains(&ability, |effect| matches!(
                effect,
                Effect::CastFromZone {
                    without_paying_mana_cost: true,
                    ..
                }
            )),
            "Mad Wizard's Lair must offer a free cast, got {:?}",
            ability
        );
    }

    // ── Baldur's Gate Wilderness room effect tests ────────────────────

    fn bgw_effect(room: u8) -> ResolvedAbility {
        room_effects(
            DungeonId::BaldursGateWilderness,
            room,
            ObjectId(1),
            PlayerId(0),
        )
        .0
    }

    #[test]
    fn room_effects_bgw_implemented_rooms() {
        let implemented = [0, 1, 2, 3, 4, 5, 6, 9, 12, 15, 16, 18];
        for room in implemented {
            let ability = bgw_effect(room);
            assert!(
                !matches!(ability.effect, Effect::Unimplemented { .. }),
                "Room {room} ({}) should be implemented",
                room_name(DungeonId::BaldursGateWilderness, room),
            );
        }
    }

    #[test]
    fn room_effects_bgw_deferred_rooms() {
        let deferred = [7, 8, 10, 11, 13, 14, 17];
        for room in deferred {
            let ability = bgw_effect(room);
            assert!(
                matches!(ability.effect, Effect::Unimplemented { .. }),
                "Room {room} ({}) should be Unimplemented",
                room_name(DungeonId::BaldursGateWilderness, room),
            );
        }
    }

    #[test]
    fn room_effects_bgw_crash_landing_search() {
        let ability = bgw_effect(0);
        assert!(
            matches!(
                ability.effect,
                Effect::SearchLibrary {
                    count: QuantityExpr::Fixed { value: 1 },
                    reveal: true,
                    ..
                }
            ),
            "Room 0 should be SearchLibrary for basic land",
        );
        // CR 701.18a: Search → ChangeZone(Library→Hand) → Shuffle sub_ability chain.
        let cz = ability
            .sub_ability
            .as_ref()
            .expect("SearchLibrary should chain to ChangeZone(Hand)");
        assert!(
            matches!(
                cz.effect,
                Effect::ChangeZone {
                    destination: Zone::Hand,
                    ..
                }
            ),
            "sub_ability should be ChangeZone to Hand",
        );
        let shuffle = cz
            .sub_ability
            .as_ref()
            .expect("ChangeZone should chain to Shuffle");
        assert!(
            matches!(shuffle.effect, Effect::Shuffle { .. }),
            "final sub_ability should be Shuffle",
        );
    }

    #[test]
    fn room_effects_bgw_temple_of_bhaal_pump() {
        let ability = bgw_effect(18);
        match &ability.effect {
            Effect::PumpAll {
                power: PtValue::Fixed(-5),
                toughness: PtValue::Fixed(-5),
                ..
            } => {}
            other => panic!("Room 18 should be PumpAll -5/-5, got {other:?}"),
        }
        assert_eq!(ability.duration, Some(Duration::UntilEndOfTurn));
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

    /// Every dungeon must carry exactly one marker point per room.
    ///
    /// This is the guard that keeps the hand-authored geometry honest. The
    /// coordinates are read off the printed card faces and cannot be derived
    /// from anything the engine or Scryfall stores, so the failure mode they
    /// invite is a silent one: add a room, forget the point, and the client
    /// draws the venture marker on the wrong room — asserting something false
    /// about game state rather than merely looking wrong.
    #[test]
    fn dungeon_marker_points_cover_every_room() {
        for id in ALL_DUNGEONS {
            let rooms = get_definition(id).rooms.len();
            let points = marker_points(id).len();
            assert_eq!(
                points, rooms,
                "{id} has {rooms} rooms but {points} marker points",
            );
        }
    }

    /// Marker points are permille of the card image, so every coordinate has to
    /// land inside it. A point past 1000 would place the marker off the card
    /// entirely.
    #[test]
    fn dungeon_marker_points_are_normalized() {
        for id in ALL_DUNGEONS {
            for (index, point) in marker_points(id).iter().enumerate() {
                assert!(
                    point.x_permille <= 1000 && point.y_permille <= 1000,
                    "{id} room {index} marker {point:?} is outside the card",
                );
            }
        }
    }

    /// Rooms are drawn top-to-bottom on the card in the same order the room
    /// table lists them, so a room can never sit ABOVE one that precedes it.
    /// This catches a transposed or misordered coordinate list — the shape of
    /// error that `dungeon_marker_points_cover_every_room` cannot see because
    /// the count still matches.
    #[test]
    fn dungeon_marker_points_descend_the_card() {
        for id in ALL_DUNGEONS {
            let points = marker_points(id);
            for (index, window) in points.windows(2).enumerate() {
                assert!(
                    window[1].y_permille >= window[0].y_permille,
                    "{id}: room {} sits above room {index} ({:?} then {:?})",
                    index + 1,
                    window[0],
                    window[1],
                );
            }
        }
    }

    /// The Scryfall ids behind each dungeon must be distinct and well-formed.
    /// Undercity is the one that matters most: it is the only double-faced
    /// dungeon, and its `face_name` is what selects the dungeon half of
    /// `Undercity // The Initiative`.
    #[test]
    fn dungeon_card_refs_are_distinct_and_well_formed() {
        let mut oracle_ids = std::collections::BTreeSet::new();
        let mut scryfall_ids = std::collections::BTreeSet::new();
        for id in ALL_DUNGEONS {
            let card = card_ref(id);
            assert_eq!(card.oracle_id.len(), 36, "{id} oracle_id is not a uuid");
            assert_eq!(card.scryfall_id.len(), 36, "{id} scryfall_id is not a uuid");
            assert!(!card.face_name.is_empty(), "{id} has no face name");
            assert!(
                oracle_ids.insert(card.oracle_id),
                "{id} duplicate oracle_id"
            );
            assert!(
                scryfall_ids.insert(card.scryfall_id),
                "{id} duplicate scryfall_id",
            );
        }
    }
}
