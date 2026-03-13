use std::collections::{BTreeSet, HashMap, HashSet};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use super::ability::{ChoiceType, ResolvedAbility, TargetFilter, TargetRef, TriggerCondition};
use super::events::GameEvent;
use super::format::FormatConfig;
use super::identifiers::{CardId, ObjectId};
use super::mana::ManaCost;
use super::match_config::{MatchConfig, MatchPhase, MatchScore};
use super::phase::Phase;
use super::player::{Player, PlayerId};
use super::proposed_event::{ProposedEvent, ReplacementId};

use crate::game::combat::CombatState;
use crate::game::deck_loading::DeckEntry;

use crate::game::game_object::GameObject;

fn default_rng() -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(0)
}

fn default_game_number() -> u8 {
    1
}

/// Tracks whether the game is in day or night state (MTG Rule 727).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DayNight {
    Day,
    Night,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExileLink {
    pub exiled_id: ObjectId,
    pub source_id: ObjectId,
}

/// Tracks commander damage dealt to a specific player by a specific commander.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommanderDamageEntry {
    pub player: PlayerId,
    pub commander: ObjectId,
    pub damage: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCast {
    pub object_id: ObjectId,
    pub card_id: CardId,
    pub ability: ResolvedAbility,
    pub cost: ManaCost,
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
        legal_targets: Vec<TargetRef>,
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
        legal_targets: Vec<TargetRef>,
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
    },
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

    /// Cards currently revealed to all players (e.g. during a RevealHand effect).
    /// `filter_state_for_player` skips hiding these cards.
    #[serde(default)]
    pub revealed_cards: HashSet<ObjectId>,

    // Pending ability continuation after a player choice (Scry/Dig/Surveil).
    // When resolve_ability_chain pauses mid-chain for a choice state, the remaining
    // sub-ability is stored here and executed after the player responds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continuation: Option<Box<crate::types::ability::ResolvedAbility>>,

    /// The most recently chosen named value (creature type, color, etc.).
    /// Set by the NamedChoice handler, consumed by continuation effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_named_choice: Option<String>,

    /// All creature subtypes seen across loaded cards. Used by Changeling CDA
    /// to grant every creature type at runtime.
    #[serde(default)]
    pub all_creature_types: Vec<String>,
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
            day_night: None,
            spells_cast_this_turn: 0,
            pending_trigger: None,
            exile_links: Vec::new(),
            commander_cast_count: HashMap::new(),
            seat_order,
            format_config: config,
            eliminated_players: Vec::new(),
            commander_damage: Vec::new(),
            priority_passes: BTreeSet::new(),
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
            revealed_cards: HashSet::new(),
            pending_continuation: None,
            last_named_choice: None,
            all_creature_types: Vec::new(),
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
            && self.commander_cast_count == other.commander_cast_count
            && self.seat_order == other.seat_order
            && self.format_config == other.format_config
            && self.eliminated_players == other.eliminated_players
            && self.commander_damage == other.commander_damage
            && self.priority_passes == other.priority_passes
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
            && self.pending_continuation == other.pending_continuation
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
                legal_targets: vec![TargetRef::Object(ObjectId(1))],
            },
        ];
        assert_eq!(variants.len(), 13);
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
            legal_targets: vec![
                TargetRef::Object(ObjectId(1)),
                TargetRef::Object(ObjectId(2)),
            ],
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
        use crate::types::ability::{Effect, ResolvedAbility, TargetFilter};
        use crate::types::triggers::TriggerMode;

        let trigger = PendingTrigger {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            trigger_def: crate::types::ability::TriggerDefinition {
                mode: TriggerMode::ChangesZone,
                execute: None,
                valid_card: Some(TargetFilter::SelfRef),
                origin: Some(crate::types::zones::Zone::Battlefield),
                destination: Some(crate::types::zones::Zone::Graveyard),
                trigger_zones: vec![],
                phase: None,
                optional: false,
                combat_damage: false,
                secondary: false,
                valid_target: None,
                valid_source: None,
                description: None,
                constraint: None,
                condition: None,
            },
            ability: ResolvedAbility::new(
                Effect::Draw { count: 1 },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 42,
        };
        let json = serde_json::to_string(&trigger).unwrap();
        let deserialized: PendingTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(trigger, deserialized);
    }

    #[test]
    fn game_state_with_pending_trigger_and_exile_links() {
        use crate::game::triggers::PendingTrigger;
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        state.exile_links.push(ExileLink {
            exiled_id: ObjectId(10),
            source_id: ObjectId(5),
        });
        state.pending_trigger = Some(PendingTrigger {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            trigger_def: crate::types::ability::TriggerDefinition {
                mode: TriggerMode::ChangesZone,
                execute: None,
                valid_card: None,
                origin: None,
                destination: None,
                trigger_zones: vec![],
                phase: None,
                optional: false,
                combat_damage: false,
                secondary: false,
                valid_target: None,
                valid_source: None,
                description: None,
                constraint: None,
                condition: None,
            },
            ability: ResolvedAbility::new(
                Effect::Draw { count: 1 },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 1,
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
