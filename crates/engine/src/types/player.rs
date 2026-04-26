use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::events::BendingType;
use super::identifiers::ObjectId;
use super::mana::{ManaColor, ManaPool};

use crate::game::deck_loading::DeckEntry;

/// Status of a player in the game. Mirrors `PhaseStatus` for permanents — a
/// phased-out player is treated as though they don't exist for targeting,
/// damage, attacking, and SBA loss-from-life purposes, but they remain in the
/// game state (never removed from `state.players`). Their phased-out turn
/// proceeds with the player still as the active player; the status is cleared
/// at the next time a `Duration::UntilYourNextTurn` effect that phased them
/// out would expire (the active player's untap step).
///
/// CR 702.26 governs *permanent* phasing only; the player-phasing semantics
/// are derived from card Oracle text on the small set of cards (e.g.,
/// historical Teferi's Protection wording) that say "you phase out". The
/// invariants below mirror the permanent-phasing invariants:
///
/// - Status is the sole encoding of phased-in vs phased-out — the player
///   never leaves `state.players`.
/// - While phased out, the player can't be targeted, attacked, dealt damage,
///   or lose the game from 0-or-less life. These exclusions live at single
///   choke points (`game/targeting.rs::add_players`,
///   `game/combat.rs::get_valid_attack_targets`,
///   `game/effects/deal_damage.rs::apply_damage_after_replacement`,
///   `game/sba.rs::check_player_life`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
pub enum PlayerStatus {
    #[default]
    Active,
    PhasedOut,
}

impl PlayerStatus {
    pub fn is_phased_in(&self) -> bool {
        matches!(self, PlayerStatus::Active)
    }

    pub fn is_phased_out(&self) -> bool {
        matches!(self, PlayerStatus::PhasedOut)
    }
}

/// CR 122.1b: Named player counter types tracked by the engine.
/// Poison counters route to the dedicated `poison_counters` field due to SBA rules (CR 704.5c).
/// Energy counters are excluded — they use the dedicated `energy` field and `GainEnergy` effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlayerCounterKind {
    Poison,
    Experience,
    Rad,
    Ticket,
}

impl fmt::Display for PlayerCounterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Poison => write!(f, "poison"),
            Self::Experience => write!(f, "experience"),
            Self::Rad => write!(f, "rad"),
            Self::Ticket => write!(f, "ticket"),
        }
    }
}

/// CR 702.139: Tracks a declared companion outside the game.
/// The companion is not a `GameObject` until it moves to hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionInfo {
    /// The companion card's face data for creating a GameObject when moved to hand.
    pub card: DeckEntry,
    /// CR 702.139c: Whether the companion has been put into hand this game (once per game).
    pub used: bool,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PlayerId(pub u8);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub life: i32,
    pub mana_pool: ManaPool,

    // Per-player zones
    pub library: im::Vector<ObjectId>,
    pub hand: im::Vector<ObjectId>,
    pub graveyard: im::Vector<ObjectId>,

    // Tracking
    pub has_drawn_this_turn: bool,
    pub lands_played_this_turn: u8,
    pub poison_counters: u32,
    /// CR 122.1: Energy counters are a kind of counter that a player may have.
    #[serde(default)]
    pub energy: u32,
    #[serde(default)]
    pub life_gained_this_turn: u32,
    #[serde(default)]
    pub life_lost_this_turn: u32,
    /// CR 603.4: Amount of life lost during the previous turn, snapshotted at turn start.
    /// Used by "if an opponent lost life during their last turn" intervening-if conditions.
    #[serde(default)]
    pub life_lost_last_turn: u32,
    #[serde(default)]
    pub descended_this_turn: bool,
    #[serde(default)]
    pub cards_drawn_this_turn: u32,
    /// CR 121.1 + CR 504.1: Number of cards this player has drawn during the
    /// current step. Reset on every step transition (`turns.rs::advance_phase`)
    /// and at turn start (`start_next_turn`). Used by
    /// `ReplacementCondition::ExceptFirstDrawInDrawStep` and
    /// `TriggerCondition::ExceptFirstDrawInDrawStep` to identify the first
    /// card-draw of each draw step (CR 504.1's turn-based action).
    #[serde(default)]
    pub cards_drawn_this_step: u32,
    /// CR 702.179b: Players have no speed until a rule or effect sets it.
    /// `None` means the player currently has no speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<u8>,
    /// CR 702.179d: The inherent speed trigger can increase speed only once each turn.
    #[serde(default)]
    pub speed_trigger_used_this_turn: bool,
    /// CR 710.2: Number of crimes committed this turn.
    #[serde(default)]
    pub crimes_committed_this_turn: u32,
    /// CR 704.5b: Set when this player attempted to draw from an empty library.
    /// Checked by SBAs — the player loses the game.
    #[serde(default)]
    pub drew_from_empty_library: bool,

    /// Number of turns this player has taken (cumulative, never reset).
    /// Used by "your Nth turn of the game" Oracle conditions (QuantityRef::TurnsTaken).
    #[serde(default)]
    pub turns_taken: u32,

    // Elimination tracking (N-player support)
    #[serde(default)]
    pub is_eliminated: bool,

    /// Avatar crossover: which bending types this player has performed this turn.
    #[serde(default)]
    pub bending_types_this_turn: HashSet<BendingType>,

    /// CR 122.1: Player counters (experience, rad, ticket, etc.).
    /// Poison counters route to the dedicated `poison_counters` field via method accessors.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub player_counters: HashMap<PlayerCounterKind, u32>,

    /// Phasing status. Default `Active`. While `PhasedOut`, the player is
    /// excluded from targeting/attack/damage/SBA-loss filter choke points.
    /// See `PlayerStatus` for the full invariant list.
    #[serde(default)]
    pub status: PlayerStatus,

    /// CR 702.139: The player's declared companion (if any). Lives outside the game.
    /// Stored as card data (not a GameObject) until moved to hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<CompanionInfo>,

    // Derived fields (computed in WASM bridge, not persisted)
    #[serde(skip_deserializing, default)]
    pub can_look_at_top_of_library: bool,

    /// CR 903.4: Combined color identity of this player's commander(s).
    /// Derived per-tick by `derive_display_state` from `state.deck_pools` /
    /// command-zone objects so the frontend can render
    /// `ManaPip::AnyInCommandersIdentity` (Command Tower, Path of Ancestry)
    /// without recomputing identity client-side. Empty when the player has
    /// no commander or has only a colorless commander (CR 903.4f).
    #[serde(skip_deserializing, default)]
    pub commander_color_identity: Vec<ManaColor>,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            id: PlayerId(0),
            life: 20,
            mana_pool: ManaPool::default(),
            library: im::Vector::new(),
            hand: im::Vector::new(),
            graveyard: im::Vector::new(),
            has_drawn_this_turn: false,
            lands_played_this_turn: 0,
            poison_counters: 0,
            energy: 0,
            life_gained_this_turn: 0,
            life_lost_this_turn: 0,
            life_lost_last_turn: 0,
            descended_this_turn: false,
            cards_drawn_this_turn: 0,
            cards_drawn_this_step: 0,
            speed: None,
            speed_trigger_used_this_turn: false,
            crimes_committed_this_turn: 0,
            drew_from_empty_library: false,
            turns_taken: 0,
            is_eliminated: false,
            bending_types_this_turn: HashSet::new(),
            player_counters: HashMap::new(),
            companion: None,
            can_look_at_top_of_library: false,
            commander_color_identity: Vec::new(),
            status: PlayerStatus::Active,
        }
    }
}

impl Player {
    /// CR 122.1: Get the current count of a player counter.
    /// Poison counters route to the dedicated field (SBA at CR 704.5c).
    pub fn player_counter(&self, kind: &PlayerCounterKind) -> u32 {
        match kind {
            PlayerCounterKind::Poison => self.poison_counters,
            _ => self.player_counters.get(kind).copied().unwrap_or(0),
        }
    }

    /// CR 122.1: Add counters of a given type to this player.
    /// Poison counters route to the dedicated field (SBA at CR 104.3d).
    pub fn add_player_counters(&mut self, kind: &PlayerCounterKind, count: u32) {
        match kind {
            PlayerCounterKind::Poison => self.poison_counters += count,
            _ => *self.player_counters.entry(*kind).or_insert(0) += count,
        }
    }

    /// True when this player is phased in (the normal state).
    pub fn is_phased_in(&self) -> bool {
        self.status.is_phased_in()
    }

    /// True when this player is phased out (excluded from targeting/damage/
    /// attack/SBA per the player-phasing exclusion choke points).
    pub fn is_phased_out(&self) -> bool {
        self.status.is_phased_out()
    }
}
