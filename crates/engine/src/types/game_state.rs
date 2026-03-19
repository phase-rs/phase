use std::collections::{BTreeSet, HashMap, HashSet};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityCost, AbilityDefinition, AdditionalCost, ChoiceType, ChoiceValue,
    ContinuousModification, DelayedTriggerCondition, Duration, GameRestriction, ModalChoice,
    ResolvedAbility, StaticCondition, TargetFilter, TargetRef, TriggerCondition,
};
use super::events::GameEvent;
use super::format::FormatConfig;
use super::identifiers::{CardId, ObjectId, TrackedSetId};
use super::mana::ManaCost;
use super::match_config::{MatchConfig, MatchPhase, MatchScore};
use super::phase::Phase;
use super::player::{Player, PlayerId};
use super::proposed_event::{ProposedEvent, ReplacementId};
use super::zones::Zone;

use crate::game::combat::CombatState;
use crate::game::deck_loading::DeckEntry;

use crate::game::game_object::GameObject;

fn default_rng() -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(0)
}

fn default_game_number() -> u8 {
    1
}

/// Tracks whether the game is in day or night state (CR 727).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DayNight {
    Day,
    Night,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExileLink {
    pub exiled_id: ObjectId,
    pub source_id: ObjectId,
    /// CR 610.3a: The zone the exiled object occupied before being exiled.
    pub return_zone: Zone,
}

/// Tracks commander damage dealt to a specific player by a specific commander.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommanderDamageEntry {
    pub player: PlayerId,
    pub commander: ObjectId,
    pub damage: u32,
}

/// CR 603.7: A delayed triggered ability created during resolution of a spell or ability.
/// Fires once at the specified condition, then is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelayedTrigger {
    /// When this trigger fires.
    pub condition: DelayedTriggerCondition,
    /// The ability to execute when it fires.
    pub ability: ResolvedAbility,
    /// CR 603.7d: Controller (the player who created it).
    pub controller: PlayerId,
    /// Source permanent that created this delayed trigger.
    pub source_id: ObjectId,
    /// Whether this trigger fires once and is removed (most delayed triggers).
    /// CR 603.7c.
    pub one_shot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCast {
    pub object_id: ObjectId,
    pub card_id: CardId,
    pub ability: ResolvedAbility,
    pub cost: ManaCost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_cost: Option<AbilityCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_ability_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSelectionSlot {
    pub legal_targets: Vec<TargetRef>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TargetSelectionProgress {
    #[serde(default)]
    pub current_slot: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_slots: Vec<Option<TargetRef>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_legal_targets: Vec<TargetRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TargetSelectionConstraint {
    DifferentTargetPlayers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlayerDeckPool {
    pub player: PlayerId,
    pub registered_main: Vec<DeckEntry>,
    pub registered_sideboard: Vec<DeckEntry>,
    pub current_main: Vec<DeckEntry>,
    pub current_sideboard: Vec<DeckEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WaitingFor {
    Priority {
        player: PlayerId,
    },
    MulliganDecision {
        player: PlayerId,
        mulligan_count: u8,
    },
    MulliganBottomCards {
        player: PlayerId,
        count: u8,
    },
    ManaPayment {
        player: PlayerId,
    },
    TargetSelection {
        player: PlayerId,
        pending_cast: Box<PendingCast>,
        target_slots: Vec<TargetSelectionSlot>,
        #[serde(default)]
        selection: TargetSelectionProgress,
    },
    DeclareAttackers {
        player: PlayerId,
        valid_attacker_ids: Vec<ObjectId>,
        #[serde(default)]
        valid_attack_targets: Vec<crate::game::combat::AttackTarget>,
    },
    DeclareBlockers {
        player: PlayerId,
        valid_blocker_ids: Vec<ObjectId>,
        #[serde(default)]
        valid_block_targets: HashMap<ObjectId, Vec<ObjectId>>,
    },
    GameOver {
        winner: Option<PlayerId>,
    },
    ReplacementChoice {
        player: PlayerId,
        candidate_count: usize,
        #[serde(default)]
        candidate_descriptions: Vec<String>,
    },
    EquipTarget {
        player: PlayerId,
        equipment_id: ObjectId,
        valid_targets: Vec<ObjectId>,
    },
    ScryChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
    },
    DigChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        keep_count: usize,
    },
    SurveilChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
    },
    RevealChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        #[serde(default = "super::ability::default_target_filter_any")]
        filter: TargetFilter,
    },
    /// Player is choosing card(s) from a filtered library search.
    SearchChoice {
        player: PlayerId,
        /// Object IDs of legal choices (pre-filtered from library).
        cards: Vec<ObjectId>,
        /// How many cards to select.
        count: usize,
    },
    TriggerTargetSelection {
        player: PlayerId,
        target_slots: Vec<TargetSelectionSlot>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        target_constraints: Vec<TargetSelectionConstraint>,
        #[serde(default)]
        selection: TargetSelectionProgress,
    },
    BetweenGamesSideboard {
        player: PlayerId,
        game_number: u8,
        score: MatchScore,
    },
    BetweenGamesChoosePlayDraw {
        player: PlayerId,
        game_number: u8,
        score: MatchScore,
    },
    /// Player must choose from a named set of options (creature type, color, etc.).
    NamedChoice {
        player: PlayerId,
        choice_type: ChoiceType,
        options: Vec<String>,
        /// The object that originated this choice (for persisting to chosen_attributes).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
    },
    /// Player must choose modes for a modal spell (e.g. "Choose one —").
    ModeChoice {
        player: PlayerId,
        modal: ModalChoice,
        pending_cast: Box<PendingCast>,
    },
    /// Player must choose which cards to discard down to maximum hand size (cleanup step).
    DiscardToHandSize {
        player: PlayerId,
        /// How many cards must be discarded.
        count: usize,
        /// The ObjectIds of all cards in the player's hand (the chooseable set).
        cards: Vec<ObjectId>,
    },
    /// Player must decide on an additional casting cost (e.g. kicker, blight, "or pay").
    OptionalCostChoice {
        player: PlayerId,
        cost: AdditionalCost,
        pending_cast: Box<PendingCast>,
    },
    /// CR 715.3a: Player chooses creature face vs Adventure half when casting
    /// an Adventure card from hand (or exile with permission).
    AdventureCastChoice {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
    },
    /// CR 601.2c: Player chooses any number of legal targets from a set.
    /// Used for "exile any number of" and similar variable-count targeting.
    MultiTargetSelection {
        player: PlayerId,
        legal_targets: Vec<ObjectId>,
        min_targets: usize,
        max_targets: usize,
        /// The pending ability to execute with selected targets injected.
        pending_ability: Box<ResolvedAbility>,
    },
    /// Player must choose modes for a modal activated or triggered ability.
    /// Unlike ModeChoice (which is casting-specific via PendingCast), this variant
    /// is decoupled from PendingCast and carries the mode ability definitions directly.
    AbilityModeChoice {
        player: PlayerId,
        modal: ModalChoice,
        /// The source object that owns this ability.
        source_id: ObjectId,
        /// The individual mode abilities the player can choose from.
        mode_abilities: Vec<AbilityDefinition>,
        /// Whether this is an activated ability (needs stack push) or triggered
        /// (already on stack, needs effect replacement).
        #[serde(default)]
        is_activated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ability_index: Option<usize>,
        /// For activated abilities: the cost to pay after mode selection.
        /// CR 602.2a: Announce → choose modes → choose targets → pay costs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ability_cost: Option<AbilityCost>,
        /// Mode indices unavailable due to NoRepeatThisTurn/NoRepeatThisGame constraints.
        /// CR 700.2: Engine computes which modes have been previously chosen; frontend uses this to disable them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unavailable_modes: Vec<usize>,
    },
    /// CR 702.49a: Player may activate Ninjutsu from hand during declare blockers step.
    /// Presented when the attacking player has Ninjutsu cards in hand and unblocked attackers.
    NinjutsuActivation {
        player: PlayerId,
        /// Cards in hand with the Ninjutsu keyword.
        ninjutsu_cards: Vec<CardId>,
        /// Unblocked attackers that can be returned to hand.
        unblocked_attackers: Vec<ObjectId>,
    },
    /// CR 609.3: Player must choose whether to perform an optional effect ("You may X").
    OptionalEffectChoice {
        player: PlayerId,
        source_id: ObjectId,
    },
    /// CR 118.12: Opponent must decide whether to pay a cost to prevent a counter effect.
    /// Used by "counter unless its controller pays {X}" spells (Mana Leak, No More Lies).
    UnlessPayment {
        player: PlayerId,
        cost: ManaCost,
        /// The counter ability to execute if the opponent declines to pay.
        pending_counter: Box<ResolvedAbility>,
    },
    /// CR 601.2b: Player must choose a card to discard as part of an additional casting cost.
    /// After selection, the card is discarded and casting continues via `pay_and_push`.
    DiscardForCost {
        player: PlayerId,
        /// How many cards to discard.
        count: usize,
        /// Eligible cards in hand (excludes the spell being cast).
        cards: Vec<ObjectId>,
        /// The pending cast to resume after the discard is complete.
        pending_cast: Box<PendingCast>,
    },
    /// CR 118.3 / CR 601.2b: Player must choose permanent(s) to sacrifice as cost.
    SacrificeForCost {
        player: PlayerId,
        /// How many permanents to sacrifice (usually 1; covers "sacrifice two creatures").
        count: usize,
        /// Pre-filtered eligible permanents on the battlefield.
        permanents: Vec<ObjectId>,
        /// The pending cast to resume after the sacrifice is complete.
        pending_cast: Box<PendingCast>,
    },
}

impl WaitingFor {
    /// Extract the player who must act, if any.
    pub fn acting_player(&self) -> Option<PlayerId> {
        match self {
            WaitingFor::Priority { player }
            | WaitingFor::MulliganDecision { player, .. }
            | WaitingFor::MulliganBottomCards { player, .. }
            | WaitingFor::ManaPayment { player }
            | WaitingFor::TargetSelection { player, .. }
            | WaitingFor::DeclareAttackers { player, .. }
            | WaitingFor::DeclareBlockers { player, .. }
            | WaitingFor::ReplacementChoice { player, .. }
            | WaitingFor::EquipTarget { player, .. }
            | WaitingFor::ScryChoice { player, .. }
            | WaitingFor::DigChoice { player, .. }
            | WaitingFor::SurveilChoice { player, .. }
            | WaitingFor::RevealChoice { player, .. }
            | WaitingFor::SearchChoice { player, .. }
            | WaitingFor::TriggerTargetSelection { player, .. }
            | WaitingFor::BetweenGamesSideboard { player, .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { player, .. }
            | WaitingFor::NamedChoice { player, .. }
            | WaitingFor::ModeChoice { player, .. }
            | WaitingFor::DiscardToHandSize { player, .. }
            | WaitingFor::OptionalCostChoice { player, .. }
            | WaitingFor::AbilityModeChoice { player, .. }
            | WaitingFor::MultiTargetSelection { player, .. }
            | WaitingFor::AdventureCastChoice { player, .. }
            | WaitingFor::NinjutsuActivation { player, .. }
            | WaitingFor::DiscardForCost { player, .. }
            | WaitingFor::SacrificeForCost { player, .. }
            | WaitingFor::OptionalEffectChoice { player, .. }
            | WaitingFor::UnlessPayment { player, .. } => Some(*player),
            WaitingFor::GameOver { .. } => None,
        }
    }
}

/// What the frontend requests for auto-pass (no internal state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoPassRequest {
    UntilStackEmpty,
    UntilEndOfTurn,
}

/// What the engine stores for auto-pass (includes captured state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoPassMode {
    /// Auto-pass while stack is non-empty. Clears when stack empties or grows
    /// beyond `initial_stack_len` (the stack size when the flag was set).
    UntilStackEmpty { initial_stack_len: usize },
    /// Auto-pass through all priority/combat stops until the flagged player's turn starts.
    UntilEndOfTurn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionResult {
    pub events: Vec<GameEvent>,
    pub waiting_for: WaitingFor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackEntry {
    pub id: ObjectId,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub kind: StackEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum StackEntryKind {
    Spell {
        card_id: CardId,
        ability: ResolvedAbility,
        /// CR 715.4: True when this spell was cast as the Adventure half.
        /// On resolution, exiled with AdventureCreature permission instead of going to graveyard.
        #[serde(default)]
        cast_as_adventure: bool,
    },
    ActivatedAbility {
        source_id: ObjectId,
        ability: ResolvedAbility,
    },
    TriggeredAbility {
        source_id: ObjectId,
        ability: ResolvedAbility,
        #[serde(default)]
        condition: Option<TriggerCondition>,
        /// CR 603.7c: The event that caused this trigger, for event-context resolution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<GameEvent>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameState {
    pub turn_number: u32,
    pub active_player: PlayerId,
    pub phase: Phase,
    pub players: Vec<Player>,
    pub priority_player: PlayerId,

    // Central object store
    pub objects: HashMap<ObjectId, GameObject>,
    pub next_object_id: u64,

    // Shared zones
    pub battlefield: Vec<ObjectId>,
    pub stack: Vec<StackEntry>,
    pub exile: Vec<ObjectId>,

    /// Objects in the command zone (commanders, emblems).
    #[serde(default)]
    pub command_zone: Vec<ObjectId>,

    // RNG
    pub rng_seed: u64,
    #[serde(skip, default = "default_rng")]
    pub rng: ChaCha20Rng,

    // Combat
    pub combat: Option<CombatState>,

    // Game flow
    pub waiting_for: WaitingFor,
    pub lands_played_this_turn: u8,
    pub max_lands_per_turn: u8,
    pub priority_pass_count: u8,

    // Replacement effects
    pub pending_replacement: Option<PendingReplacement>,
    /// Transient: effect to resolve after a replacement choice's zone change completes.
    /// Set by `continue_replacement` for Optional replacements, consumed by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_effect: Option<Box<crate::types::ability::AbilityDefinition>>,

    // Layer system
    pub layers_dirty: bool,
    pub next_timestamp: u64,

    // Runtime continuous effects (from resolved spells/abilities, not printed card text)
    #[serde(default)]
    pub transient_continuous_effects: Vec<TransientContinuousEffect>,
    #[serde(default)]
    pub next_continuous_effect_id: u64,

    // Day/night tracking
    #[serde(default)]
    pub day_night: Option<DayNight>,
    #[serde(default)]
    pub spells_cast_this_turn: u8,

    // Triggered ability targeting
    #[serde(default)]
    pub pending_trigger: Option<crate::game::triggers::PendingTrigger>,

    // Exile tracking for "until leaves" effects
    #[serde(default)]
    pub exile_links: Vec<ExileLink>,

    /// CR 603.7: Delayed triggered abilities waiting to fire.
    #[serde(default)]
    pub delayed_triggers: Vec<DelayedTrigger>,

    /// CR 603.7: Object sets tracked for delayed triggers ("those cards", "that creature").
    #[serde(default)]
    pub tracked_object_sets: HashMap<TrackedSetId, Vec<ObjectId>>,

    #[serde(default)]
    pub next_tracked_set_id: u64,

    // Commander support
    #[serde(default)]
    pub commander_cast_count: HashMap<ObjectId, u32>,

    // N-player support
    #[serde(default)]
    pub seat_order: Vec<PlayerId>,
    #[serde(default = "FormatConfig::standard")]
    pub format_config: FormatConfig,
    #[serde(default)]
    pub eliminated_players: Vec<PlayerId>,
    #[serde(default)]
    pub commander_damage: Vec<CommanderDamageEntry>,
    #[serde(default)]
    pub priority_passes: BTreeSet<PlayerId>,
    /// Per-player auto-pass flags. When set, the engine auto-passes for this player.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub auto_pass: HashMap<PlayerId, AutoPassMode>,

    /// CR 605.3: Lands manually tapped for mana via TapLandForMana this priority window.
    /// Per-player map enables multiplayer correctness (e.g., UnlessPayment opponent tapping).
    /// Cleared on priority pass, cast, non-mana action, or phase transition.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lands_tapped_for_mana: HashMap<PlayerId, Vec<ObjectId>>,

    #[serde(default)]
    pub match_config: MatchConfig,
    #[serde(default)]
    pub match_phase: MatchPhase,
    #[serde(default)]
    pub match_score: MatchScore,
    #[serde(default = "default_game_number")]
    pub game_number: u8,
    #[serde(default)]
    pub current_starting_player: PlayerId,
    #[serde(default)]
    pub next_game_chooser: Option<PlayerId>,
    #[serde(default)]
    pub deck_pools: Vec<PlayerDeckPool>,
    #[serde(default)]
    pub sideboard_submitted: Vec<PlayerId>,

    // Trigger constraint tracking: (object_id, trigger_index) pairs that have fired
    #[serde(default)]
    pub triggers_fired_this_turn: HashSet<(ObjectId, usize)>,
    #[serde(default)]
    pub triggers_fired_this_game: HashSet<(ObjectId, usize)>,
    #[serde(default)]
    pub activated_abilities_this_turn: HashMap<(ObjectId, usize), u32>,
    #[serde(default)]
    pub activated_abilities_this_game: HashMap<(ObjectId, usize), u32>,
    #[serde(default)]
    pub spells_cast_this_game: HashMap<PlayerId, u32>,
    #[serde(default)]
    pub spells_cast_this_turn_by_player: HashMap<PlayerId, u32>,
    #[serde(default)]
    pub players_who_cast_noncreature_spell_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_searched_library_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_attacked_this_step: HashSet<PlayerId>,
    #[serde(default)]
    pub players_attacked_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub attacking_creatures_this_turn: HashMap<PlayerId, u32>,
    #[serde(default)]
    pub players_who_created_token_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_discarded_card_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_sacrificed_artifact_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_had_creature_etb_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_had_angel_or_berserker_etb_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_had_artifact_etb_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub cards_left_graveyard_this_turn: HashMap<PlayerId, u32>,
    #[serde(default)]
    pub creature_died_this_turn: bool,

    /// Modal modes chosen this turn per source: (ObjectId, mode_index).
    /// CR 700.2: "choose one that hasn't been chosen this turn"
    /// Note: ObjectId-keyed — zone changes create new ObjectId per CR 400.7, naturally resetting tracking.
    #[serde(default)]
    pub modal_modes_chosen_this_turn: HashSet<(ObjectId, usize)>,
    /// Modal modes chosen this game per source: (ObjectId, mode_index).
    /// CR 700.2: "choose one that hasn't been chosen" (game-scoped)
    /// Note: ObjectId-keyed — zone changes create new ObjectId per CR 400.7, naturally resetting tracking.
    #[serde(default)]
    pub modal_modes_chosen_this_game: HashSet<(ObjectId, usize)>,

    /// Cards currently revealed to all players (e.g. during a RevealHand effect).
    /// `filter_state_for_player` skips hiding these cards.
    #[serde(default)]
    pub revealed_cards: HashSet<ObjectId>,

    // Pending ability continuation after a player choice (Scry/Dig/Surveil).
    // When resolve_ability_chain pauses mid-chain for a choice state, the remaining
    // sub-ability is stored here and executed after the player responds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continuation: Option<Box<crate::types::ability::ResolvedAbility>>,

    /// Pending optional effect ability chain, awaiting player accept/decline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_optional_effect: Option<Box<crate::types::ability::ResolvedAbility>>,

    /// The most recently chosen named value (creature type, color, etc.).
    /// Set by the NamedChoice handler, consumed by continuation effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_named_choice: Option<ChoiceValue>,

    /// All creature subtypes seen across loaded cards. Used by Changeling CDA
    /// to grant every creature type at runtime.
    #[serde(default)]
    pub all_creature_types: Vec<String>,

    /// All card names from the loaded card database, used to validate
    /// "name a card" choices. Skipped in serialization to avoid sending 30k+ names.
    #[serde(skip)]
    pub all_card_names: Vec<String>,

    /// Object IDs from the most recently resolved Effect::Token.
    /// Consumed by sub_abilities referencing "it"/"them" via TargetFilter::LastCreated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_created_token_ids: Vec<ObjectId>,

    /// CR 722: The current monarch, if any. At the beginning of the monarch's end step,
    /// the monarch draws a card. When a creature deals combat damage to the monarch,
    /// the creature's controller becomes the monarch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monarch: Option<PlayerId>,

    /// Active game-level restrictions (e.g., damage prevention disabled).
    /// Checked by relevant game systems; expired entries cleaned up at phase transitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restrictions: Vec<GameRestriction>,

    /// Transient: set by stack.rs before resolving a triggered ability, cleared after.
    /// Used by event-context TargetFilter variants to resolve trigger event data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_trigger_event: Option<GameEvent>,

    /// Transient: set by PayCost resolver when payment fails.
    /// Gates IfYouDo sub-abilities. Reset in DecideOptionalEffect handler.
    #[serde(skip)]
    pub cost_payment_failed_flag: bool,
}

/// A runtime-generated continuous effect stored at state level.
///
/// Unlike `StaticDefinition` (which represents intrinsic/printed card text),
/// transient effects are created by resolving spells and abilities at runtime
/// (e.g., "target creature gets +3/+3 until end of turn"). They participate
/// in layer evaluation alongside intrinsic statics but have explicit lifetimes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransientContinuousEffect {
    pub id: u64,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub timestamp: u64,
    pub duration: Duration,
    pub affected: TargetFilter,
    pub modifications: Vec<ContinuousModification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<StaticCondition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingReplacement {
    pub proposed: ProposedEvent,
    pub candidates: Vec<ReplacementId>,
    pub depth: u16,
    /// When true, the replacement is Optional — index 0 = accept, index 1 = decline.
    /// `candidates` has exactly one entry (the real replacement); decline is synthetic.
    #[serde(default)]
    pub is_optional: bool,
}

impl GameState {
    /// Create a new game with the given format configuration and player count.
    pub fn new(config: FormatConfig, player_count: u8, seed: u64) -> Self {
        let players: Vec<Player> = (0..player_count)
            .map(|i| Player {
                id: PlayerId(i),
                life: config.starting_life,
                ..Player::default()
            })
            .collect();
        let seat_order: Vec<PlayerId> = (0..player_count).map(PlayerId).collect();

        GameState {
            turn_number: 0,
            active_player: PlayerId(0),
            phase: Phase::Untap,
            players,
            priority_player: PlayerId(0),
            objects: HashMap::new(),
            next_object_id: 1,
            battlefield: Vec::new(),
            stack: Vec::new(),
            exile: Vec::new(),
            command_zone: Vec::new(),
            rng_seed: seed,
            rng: ChaCha20Rng::seed_from_u64(seed),
            combat: None,
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            lands_played_this_turn: 0,
            max_lands_per_turn: 1,
            priority_pass_count: 0,
            pending_replacement: None,
            post_replacement_effect: None,
            layers_dirty: true,
            next_timestamp: 1,
            transient_continuous_effects: Vec::new(),
            next_continuous_effect_id: 1,
            day_night: None,
            spells_cast_this_turn: 0,
            pending_trigger: None,
            exile_links: Vec::new(),
            delayed_triggers: Vec::new(),
            tracked_object_sets: HashMap::new(),
            next_tracked_set_id: 1,
            commander_cast_count: HashMap::new(),
            seat_order,
            format_config: config,
            eliminated_players: Vec::new(),
            commander_damage: Vec::new(),
            priority_passes: BTreeSet::new(),
            auto_pass: HashMap::new(),
            lands_tapped_for_mana: HashMap::new(),
            match_config: MatchConfig::default(),
            match_phase: MatchPhase::InGame,
            match_score: MatchScore::default(),
            game_number: default_game_number(),
            current_starting_player: PlayerId(0),
            next_game_chooser: None,
            deck_pools: Vec::new(),
            sideboard_submitted: Vec::new(),
            triggers_fired_this_turn: HashSet::new(),
            triggers_fired_this_game: HashSet::new(),
            activated_abilities_this_turn: HashMap::new(),
            activated_abilities_this_game: HashMap::new(),
            spells_cast_this_game: HashMap::new(),
            spells_cast_this_turn_by_player: HashMap::new(),
            players_who_cast_noncreature_spell_this_turn: HashSet::new(),
            players_who_searched_library_this_turn: HashSet::new(),
            players_attacked_this_step: HashSet::new(),
            players_attacked_this_turn: HashSet::new(),
            attacking_creatures_this_turn: HashMap::new(),
            players_who_created_token_this_turn: HashSet::new(),
            players_who_discarded_card_this_turn: HashSet::new(),
            players_who_sacrificed_artifact_this_turn: HashSet::new(),
            players_who_had_creature_etb_this_turn: HashSet::new(),
            players_who_had_angel_or_berserker_etb_this_turn: HashSet::new(),
            players_who_had_artifact_etb_this_turn: HashSet::new(),
            cards_left_graveyard_this_turn: HashMap::new(),
            creature_died_this_turn: false,
            modal_modes_chosen_this_turn: HashSet::new(),
            modal_modes_chosen_this_game: HashSet::new(),
            revealed_cards: HashSet::new(),
            pending_continuation: None,
            pending_optional_effect: None,
            last_named_choice: None,
            all_creature_types: Vec::new(),
            all_card_names: Vec::new(),
            last_created_token_ids: Vec::new(),
            monarch: None,
            restrictions: Vec::new(),
            current_trigger_event: None,
            cost_payment_failed_flag: false,
        }
    }

    /// Create a standard 2-player game (backward-compatible).
    pub fn new_two_player(seed: u64) -> Self {
        Self::new(FormatConfig::standard(), 2, seed)
    }

    /// Returns the current timestamp and increments for next use.
    pub fn next_timestamp(&mut self) -> u64 {
        let ts = self.next_timestamp;
        self.next_timestamp += 1;
        ts
    }

    /// Register a transient continuous effect and mark layers dirty.
    pub fn add_transient_continuous_effect(
        &mut self,
        source_id: ObjectId,
        controller: PlayerId,
        duration: Duration,
        affected: TargetFilter,
        modifications: Vec<ContinuousModification>,
        condition: Option<StaticCondition>,
    ) -> u64 {
        let id = self.next_continuous_effect_id;
        self.next_continuous_effect_id += 1;
        let timestamp = self.next_timestamp();
        self.transient_continuous_effects
            .push(TransientContinuousEffect {
                id,
                source_id,
                controller,
                timestamp,
                duration,
                affected,
                modifications,
                condition,
            });
        self.layers_dirty = true;
        id
    }
}

impl Default for GameState {
    fn default() -> Self {
        Self::new_two_player(0)
    }
}

// Reconstruct RNG from seed on deserialization
impl PartialEq for GameState {
    fn eq(&self, other: &Self) -> bool {
        self.turn_number == other.turn_number
            && self.active_player == other.active_player
            && self.phase == other.phase
            && self.players == other.players
            && self.priority_player == other.priority_player
            && self.objects.len() == other.objects.len()
            && self.next_object_id == other.next_object_id
            && self.battlefield == other.battlefield
            && self.stack == other.stack
            && self.exile == other.exile
            && self.command_zone == other.command_zone
            && self.rng_seed == other.rng_seed
            && self.combat == other.combat
            && self.waiting_for == other.waiting_for
            && self.lands_played_this_turn == other.lands_played_this_turn
            && self.max_lands_per_turn == other.max_lands_per_turn
            && self.priority_pass_count == other.priority_pass_count
            && self.pending_replacement == other.pending_replacement
            && self.layers_dirty == other.layers_dirty
            && self.next_timestamp == other.next_timestamp
            && self.day_night == other.day_night
            && self.spells_cast_this_turn == other.spells_cast_this_turn
            && self.pending_trigger == other.pending_trigger
            && self.exile_links == other.exile_links
            && self.delayed_triggers == other.delayed_triggers
            && self.tracked_object_sets == other.tracked_object_sets
            && self.next_tracked_set_id == other.next_tracked_set_id
            && self.commander_cast_count == other.commander_cast_count
            && self.seat_order == other.seat_order
            && self.format_config == other.format_config
            && self.eliminated_players == other.eliminated_players
            && self.commander_damage == other.commander_damage
            && self.priority_passes == other.priority_passes
            && self.auto_pass == other.auto_pass
            && self.lands_tapped_for_mana == other.lands_tapped_for_mana
            && self.match_config == other.match_config
            && self.match_phase == other.match_phase
            && self.match_score == other.match_score
            && self.game_number == other.game_number
            && self.current_starting_player == other.current_starting_player
            && self.next_game_chooser == other.next_game_chooser
            && self.deck_pools == other.deck_pools
            && self.sideboard_submitted == other.sideboard_submitted
            && self.triggers_fired_this_turn == other.triggers_fired_this_turn
            && self.triggers_fired_this_game == other.triggers_fired_this_game
            && self.activated_abilities_this_turn == other.activated_abilities_this_turn
            && self.activated_abilities_this_game == other.activated_abilities_this_game
            && self.spells_cast_this_game == other.spells_cast_this_game
            && self.spells_cast_this_turn_by_player == other.spells_cast_this_turn_by_player
            && self.players_who_cast_noncreature_spell_this_turn
                == other.players_who_cast_noncreature_spell_this_turn
            && self.players_who_searched_library_this_turn
                == other.players_who_searched_library_this_turn
            && self.players_attacked_this_step == other.players_attacked_this_step
            && self.players_attacked_this_turn == other.players_attacked_this_turn
            && self.attacking_creatures_this_turn == other.attacking_creatures_this_turn
            && self.players_who_created_token_this_turn == other.players_who_created_token_this_turn
            && self.players_who_discarded_card_this_turn
                == other.players_who_discarded_card_this_turn
            && self.players_who_sacrificed_artifact_this_turn
                == other.players_who_sacrificed_artifact_this_turn
            && self.players_who_had_creature_etb_this_turn
                == other.players_who_had_creature_etb_this_turn
            && self.players_who_had_angel_or_berserker_etb_this_turn
                == other.players_who_had_angel_or_berserker_etb_this_turn
            && self.players_who_had_artifact_etb_this_turn
                == other.players_who_had_artifact_etb_this_turn
            && self.cards_left_graveyard_this_turn == other.cards_left_graveyard_this_turn
            && self.creature_died_this_turn == other.creature_died_this_turn
            && self.modal_modes_chosen_this_turn == other.modal_modes_chosen_this_turn
            && self.modal_modes_chosen_this_game == other.modal_modes_chosen_this_game
            && self.pending_continuation == other.pending_continuation
            && self.last_named_choice == other.last_named_choice
    }
}

impl Eq for GameState {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_creates_two_player_game() {
        let state = GameState::default();
        assert_eq!(state.players.len(), 2);
    }

    #[test]
    fn default_starts_at_turn_zero() {
        let state = GameState::default();
        assert_eq!(state.turn_number, 0);
    }

    #[test]
    fn default_starts_in_untap_phase() {
        let state = GameState::default();
        assert_eq!(state.phase, Phase::Untap);
    }

    #[test]
    fn default_players_have_20_life() {
        let state = GameState::default();
        for player in &state.players {
            assert_eq!(player.life, 20);
        }
    }

    #[test]
    fn default_players_have_distinct_ids() {
        let state = GameState::default();
        assert_ne!(state.players[0].id, state.players[1].id);
    }

    #[test]
    fn game_state_has_central_object_store() {
        let state = GameState::default();
        assert!(state.objects.is_empty());
        assert_eq!(state.next_object_id, 1);
    }

    #[test]
    fn game_state_has_shared_zone_collections() {
        let state = GameState::default();
        assert!(state.battlefield.is_empty());
        assert!(state.stack.is_empty());
        assert!(state.exile.is_empty());
    }

    #[test]
    fn game_state_has_seeded_rng() {
        let state1 = GameState::new_two_player(42);
        let state2 = GameState::new_two_player(42);
        assert_eq!(state1.rng_seed, state2.rng_seed);
        assert_eq!(state1.rng_seed, 42);
    }

    #[test]
    fn game_state_has_waiting_for() {
        let state = GameState::default();
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        );
    }

    #[test]
    fn game_state_has_land_tracking() {
        let state = GameState::default();
        assert_eq!(state.lands_played_this_turn, 0);
        assert_eq!(state.max_lands_per_turn, 1);
    }

    #[test]
    fn new_two_player_creates_game_with_seed() {
        let state = GameState::new_two_player(12345);
        assert_eq!(state.rng_seed, 12345);
        assert_eq!(state.players.len(), 2);
    }

    #[test]
    fn game_state_serializes_and_roundtrips() {
        let state = GameState::default();
        let serialized = serde_json::to_string(&state).unwrap();
        let mut deserialized: GameState = serde_json::from_str(&serialized).unwrap();
        // Reconstruct RNG from seed since it's skipped in serde
        deserialized.rng = ChaCha20Rng::seed_from_u64(deserialized.rng_seed);
        assert_eq!(state, deserialized);
    }

    #[test]
    fn waiting_for_variants_exist() {
        let variants = [
            WaitingFor::Priority {
                player: PlayerId(0),
            },
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 1,
            },
            WaitingFor::MulliganBottomCards {
                player: PlayerId(0),
                count: 2,
            },
            WaitingFor::ManaPayment {
                player: PlayerId(0),
            },
            WaitingFor::DeclareAttackers {
                player: PlayerId(0),
                valid_attacker_ids: vec![],
                valid_attack_targets: vec![],
            },
            WaitingFor::DeclareBlockers {
                player: PlayerId(0),
                valid_blocker_ids: vec![],
                valid_block_targets: HashMap::new(),
            },
            WaitingFor::GameOver {
                winner: Some(PlayerId(0)),
            },
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                candidate_count: 2,
                candidate_descriptions: vec![],
            },
            WaitingFor::EquipTarget {
                player: PlayerId(0),
                equipment_id: ObjectId(1),
                valid_targets: vec![],
            },
            WaitingFor::ScryChoice {
                player: PlayerId(0),
                cards: vec![ObjectId(1)],
            },
            WaitingFor::DigChoice {
                player: PlayerId(0),
                cards: vec![ObjectId(1)],
                keep_count: 1,
            },
            WaitingFor::SurveilChoice {
                player: PlayerId(0),
                cards: vec![ObjectId(1)],
            },
            WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(ObjectId(1))],
                    optional: false,
                }],
                target_constraints: vec![],
                selection: TargetSelectionProgress::default(),
            },
            WaitingFor::ModeChoice {
                player: PlayerId(0),
                modal: ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 3,
                    ..Default::default()
                },
                pending_cast: Box::new(PendingCast {
                    object_id: ObjectId(1),
                    card_id: CardId(1),
                    ability: ResolvedAbility::new(
                        crate::types::ability::Effect::Unimplemented {
                            name: "placeholder".to_string(),
                            description: None,
                        },
                        vec![],
                        ObjectId(1),
                        PlayerId(0),
                    ),
                    cost: crate::types::mana::ManaCost::NoCost,
                    activation_cost: None,
                    activation_ability_index: None,
                    target_constraints: vec![],
                }),
            },
            WaitingFor::DiscardToHandSize {
                player: PlayerId(0),
                count: 2,
                cards: vec![ObjectId(1), ObjectId(2)],
            },
            WaitingFor::OptionalCostChoice {
                player: PlayerId(0),
                cost: AdditionalCost::Optional(crate::types::ability::AbilityCost::Blight {
                    count: 1,
                }),
                pending_cast: Box::new(PendingCast {
                    object_id: ObjectId(1),
                    card_id: CardId(1),
                    ability: ResolvedAbility::new(
                        crate::types::ability::Effect::Unimplemented {
                            name: "placeholder".to_string(),
                            description: None,
                        },
                        vec![],
                        ObjectId(1),
                        PlayerId(0),
                    ),
                    cost: crate::types::mana::ManaCost::NoCost,
                    activation_cost: None,
                    activation_ability_index: None,
                    target_constraints: vec![],
                }),
            },
            WaitingFor::AbilityModeChoice {
                player: PlayerId(0),
                modal: ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 2,
                    ..Default::default()
                },
                source_id: ObjectId(1),
                mode_abilities: vec![],
                is_activated: true,
                ability_index: Some(0),
                ability_cost: None,
                unavailable_modes: vec![],
            },
            WaitingFor::DiscardForCost {
                player: PlayerId(0),
                count: 1,
                cards: vec![ObjectId(1)],
                pending_cast: Box::new(PendingCast {
                    object_id: ObjectId(1),
                    card_id: CardId(1),
                    ability: ResolvedAbility::new(
                        crate::types::ability::Effect::Unimplemented {
                            name: "Dummy".to_string(),
                            description: None,
                        },
                        vec![],
                        ObjectId(1),
                        PlayerId(0),
                    ),
                    cost: ManaCost::NoCost,
                    activation_cost: None,
                    activation_ability_index: None,
                    target_constraints: vec![],
                }),
            },
        ];
        assert_eq!(variants.len(), 18);
    }

    #[test]
    fn stack_entry_kind_spell() {
        use crate::types::ability::ResolvedAbility;
        let entry = StackEntry {
            id: ObjectId(1),
            source_id: ObjectId(2),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    ObjectId(2),
                    PlayerId(0),
                ),
                cast_as_adventure: false,
            },
        };
        assert_eq!(entry.id, ObjectId(1));
        assert_eq!(entry.source_id, ObjectId(2));
    }

    #[test]
    fn action_result_contains_events_and_waiting_for() {
        let result = ActionResult {
            events: vec![GameEvent::GameStarted],
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
        };
        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn players_have_per_player_zones() {
        let state = GameState::default();
        for player in &state.players {
            assert!(player.library.is_empty());
            assert!(player.hand.is_empty());
            assert!(player.graveyard.is_empty());
        }
    }

    #[test]
    fn day_night_starts_none() {
        let state = GameState::default();
        assert_eq!(state.day_night, None);
    }

    #[test]
    fn spells_cast_this_turn_starts_zero() {
        let state = GameState::default();
        assert_eq!(state.spells_cast_this_turn, 0);
    }

    #[test]
    fn day_night_enum_variants() {
        assert_ne!(DayNight::Day, DayNight::Night);
    }

    #[test]
    fn day_night_changed_event_roundtrips() {
        let event = GameEvent::DayNightChanged {
            new_state: "Night".to_string(),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn exile_link_roundtrips() {
        let link = ExileLink {
            exiled_id: ObjectId(10),
            source_id: ObjectId(5),
            return_zone: Zone::Battlefield,
        };
        let json = serde_json::to_string(&link).unwrap();
        let deserialized: ExileLink = serde_json::from_str(&json).unwrap();
        assert_eq!(link, deserialized);
    }

    #[test]
    fn trigger_target_selection_roundtrips() {
        use crate::types::ability::TargetRef;
        let wf = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Object(ObjectId(1)),
                    TargetRef::Object(ObjectId(2)),
                ],
                optional: false,
            }],
            target_constraints: vec![],
            selection: TargetSelectionProgress::default(),
        };
        let json = serde_json::to_string(&wf).unwrap();
        let deserialized: WaitingFor = serde_json::from_str(&json).unwrap();
        assert_eq!(wf, deserialized);
        // Verify tag format
        assert!(json.contains("\"TriggerTargetSelection\""));
    }

    #[test]
    fn pending_trigger_roundtrips() {
        use crate::game::triggers::PendingTrigger;
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility};

        let trigger = PendingTrigger {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 42,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
        };
        let json = serde_json::to_string(&trigger).unwrap();
        let deserialized: PendingTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(trigger, deserialized);
    }

    #[test]
    fn game_state_with_pending_trigger_and_exile_links() {
        use crate::game::triggers::PendingTrigger;
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility};

        let mut state = GameState::new_two_player(42);
        state.exile_links.push(ExileLink {
            exiled_id: ObjectId(10),
            source_id: ObjectId(5),
            return_zone: Zone::Battlefield,
        });
        state.pending_trigger = Some(PendingTrigger {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
        });

        let json = serde_json::to_string(&state).unwrap();
        let mut deserialized: GameState = serde_json::from_str(&json).unwrap();
        deserialized.rng = rand_chacha::ChaCha20Rng::seed_from_u64(deserialized.rng_seed);
        assert_eq!(state, deserialized);
    }

    #[test]
    fn new_two_player_initializes_pending_trigger_and_exile_links() {
        let state = GameState::new_two_player(0);
        assert!(state.pending_trigger.is_none());
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn new_with_standard_config_matches_new_two_player() {
        let from_new = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let from_legacy = GameState::new_two_player(42);
        assert_eq!(from_new.players.len(), from_legacy.players.len());
        assert_eq!(from_new.players[0].life, from_legacy.players[0].life);
        assert_eq!(from_new.players[1].life, from_legacy.players[1].life);
        assert_eq!(from_new.rng_seed, from_legacy.rng_seed);
        assert_eq!(from_new, from_legacy);
    }

    #[test]
    fn new_with_commander_config_creates_four_players_with_40_life() {
        let state = GameState::new(crate::types::format::FormatConfig::commander(), 4, 0);
        assert_eq!(state.players.len(), 4);
        for player in &state.players {
            assert_eq!(player.life, 40);
        }
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2), PlayerId(3)]
        );
    }

    #[test]
    fn new_initializes_seat_order() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        assert_eq!(state.seat_order, vec![PlayerId(0), PlayerId(1)]);
    }

    #[test]
    fn new_initializes_eliminated_players_empty() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        assert!(state.eliminated_players.is_empty());
    }

    #[test]
    fn new_initializes_commander_damage_empty() {
        let state = GameState::new(crate::types::format::FormatConfig::commander(), 4, 0);
        assert!(state.commander_damage.is_empty());
    }

    #[test]
    fn new_initializes_priority_passes_empty() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        assert!(state.priority_passes.is_empty());
    }

    #[test]
    fn player_is_eliminated_defaults_to_false() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        for player in &state.players {
            assert!(!player.is_eliminated);
        }
    }

    #[test]
    fn new_two_player_has_seat_order_and_format_config() {
        let state = GameState::new_two_player(0);
        assert_eq!(state.seat_order, vec![PlayerId(0), PlayerId(1)]);
        assert_eq!(
            state.format_config,
            crate::types::format::FormatConfig::standard()
        );
    }

    #[test]
    fn game_state_with_new_fields_serializes_and_roundtrips() {
        let state = GameState::new(crate::types::format::FormatConfig::commander(), 4, 42);
        let serialized = serde_json::to_string(&state).unwrap();
        let mut deserialized: GameState = serde_json::from_str(&serialized).unwrap();
        deserialized.rng = ChaCha20Rng::seed_from_u64(deserialized.rng_seed);
        assert_eq!(state, deserialized);
    }
}
