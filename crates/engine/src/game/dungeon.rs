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

// ─── Room Effects ───────────────────────────────────────────────────────────

use crate::types::ability::{
    AbilityCondition, CastingPermission, ContinuousModification, ControllerRef, Duration, Effect,
    FilterProp, PlayerFilter, PtValue, QuantityExpr, ResolvedAbility, SearchSelectionConstraint,
    StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::Supertype;
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
        // 3: Storeroom — "Put a +1/+1 counter on target creature you control"
        (DungeonId::LostMineOfPhandelver, 3) => (
            simple(
                Effect::AddCounter {
                    counter_type: "+1/+1".to_string(),
                    count: fixed(1),
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
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
                    player: crate::types::ability::GainLifePlayer::Controller,
                },
                source_id,
                controller,
            )));
            (lose, vec![])
        }
        // 5: Fungi Cavern — "Target creature gets -4/-0 until end of turn"
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
            .duration(Duration::UntilEndOfTurn),
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
                    player: crate::types::ability::GainLifePlayer::Controller,
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
                },
                source_id,
                controller,
            )
            .duration(Duration::UntilYourNextTurn);
            (ability, vec![])
        }
        // 4: Lost Level — "Destroy target creature of an opponent's choice"
        (DungeonId::DungeonOfTheMadMage, 4) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Lost Level".to_string(),
                    description: Some(
                        "Destroy target creature of an opponent's choice".to_string(),
                    ),
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
                },
                source_id,
                controller,
            );
            let grant = simple(
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        duration: Duration::UntilEndOfTurn,
                        // Placeholder — rewritten to ability.controller at
                        // grant time by `grant_permission::resolve`.
                        granted_to: crate::types::player::PlayerId(0),
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
            simple(
                Effect::Unimplemented {
                    name: "Room: Mad Wizard's Lair".to_string(),
                    description: Some(
                        "Draw three cards and reveal them. You may cast one without paying its mana cost.".to_string(),
                    ),
                },
                source_id,
                controller,
            ),
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
        // 1: Veils of Fear — "Each player sacrifices a creature"
        (DungeonId::TombOfAnnihilation, 1) => {
            let mut ability = simple(
                Effect::Sacrifice {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    count: QuantityExpr::Fixed { value: 1 },
                },
                source_id,
                controller,
            );
            ability.player_scope = Some(PlayerFilter::All);
            (ability, vec![])
        }
        // 2: Oubliette — "Discard a card"
        (DungeonId::TombOfAnnihilation, 2) => (
            simple(
                Effect::DiscardCard {
                    count: 1,
                    target: TargetFilter::Any,
                },
                source_id,
                controller,
            ),
            vec![],
        ),
        // 3: Sandfall Cell — "You lose 2 life and create a 2/2 black Zombie creature token"
        (DungeonId::TombOfAnnihilation, 3) => {
            let mut lose = simple(
                Effect::LoseLife { amount: fixed(2), target: None },
                source_id,
                controller,
            );
            lose.sub_ability = Some(Box::new(simple(
                creature_token("Zombie", 2, 2, &["Zombie"], &[ManaColor::Black], &[], 1),
                source_id,
                controller,
            )));
            (lose, vec![])
        }
        // 4: Cradle of the Death God — "Create The Atropal, a legendary 4/4 black God Horror
        //    creature token"
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
                    keywords: vec![],
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
                Effect::AddCounter {
                    counter_type: "+1/+1".to_string(),
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
        // 8: Throne of the Dead Three — complex multi-step effect, deferred
        (DungeonId::Undercity, 8) => (
            simple(
                Effect::Unimplemented {
                    name: "Room: Throne of the Dead Three".to_string(),
                    description: Some(
                        "Reveal the top ten cards of your library. Put a creature card from among them onto the battlefield with three +1/+1 counters on it. It gains hexproof until your next turn. Then shuffle.".to_string(),
                    ),
                },
                source_id,
                controller,
            ),
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
            draw.condition = Some(AbilityCondition::IfYouDo);
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
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
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
                        affected_zone: None,
                        effect_zone: None,
                        active_zones: vec![],
                        characteristic_defining: false,
                        description: Some(
                            "Creatures you control get +2/+2 and have trample.".to_string(),
                        ),
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
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
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
}
