use std::collections::HashMap;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use super::ability::{ResolvedAbility, TargetRef};
use super::events::GameEvent;
use super::identifiers::{CardId, ObjectId};
use super::mana::ManaCost;
use super::phase::Phase;
use super::player::{Player, PlayerId};
use super::proposed_event::{ProposedEvent, ReplacementId};

use crate::game::combat::CombatState;

use crate::game::game_object::GameObject;

fn default_rng() -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCast {
    pub object_id: ObjectId,
    pub card_id: CardId,
    pub ability: ResolvedAbility,
    pub cost: ManaCost,
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
        pending_cast: PendingCast,
        legal_targets: Vec<TargetRef>,
    },
    DeclareAttackers {
        player: PlayerId,
    },
    DeclareBlockers {
        player: PlayerId,
    },
    GameOver {
        winner: Option<PlayerId>,
    },
    ReplacementChoice {
        player: PlayerId,
        candidate_count: usize,
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

    // Layer system
    pub layers_dirty: bool,
    pub next_timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingReplacement {
    pub proposed: ProposedEvent,
    pub candidates: Vec<ReplacementId>,
    pub depth: u16,
}

impl GameState {
    pub fn new_two_player(seed: u64) -> Self {
        GameState {
            turn_number: 0,
            active_player: PlayerId(0),
            phase: Phase::Untap,
            players: vec![
                Player {
                    id: PlayerId(0),
                    life: 20,
                    ..Player::default()
                },
                Player {
                    id: PlayerId(1),
                    life: 20,
                    ..Player::default()
                },
            ],
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
            layers_dirty: true,
            next_timestamp: 1,
        }
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
            WaitingFor::GameOver {
                winner: Some(PlayerId(0)),
            },
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                candidate_count: 2,
            },
        ];
        assert_eq!(variants.len(), 6);
    }

    #[test]
    fn stack_entry_kind_spell() {
        use crate::types::ability::ResolvedAbility;
        use std::collections::HashMap;
        let entry = StackEntry {
            id: ObjectId(1),
            source_id: ObjectId(2),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: ResolvedAbility {
                    api_type: String::new(),
                    params: HashMap::new(),
                    targets: vec![],
                    source_id: ObjectId(2),
                    controller: PlayerId(0),
                    sub_ability: None,
                    svars: HashMap::new(),
                },
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
}
