use std::collections::{HashMap, HashSet};
use std::time::Duration;

use engine::ai_support::{
    auto_pass_recommended, legal_actions_with_costs as engine_legal_actions_with_costs,
};
use engine::game::deck_loading::{load_deck_into_state, DeckPayload, PlayerDeckPayload};
use engine::game::engine::{apply, start_game};
use engine::game::finalize_public_state;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::player::PlayerId;
use phase_ai::config::{AiConfig, AiDifficulty, Platform};
use rand::{Rng, SeedableRng};
use seat_reducer::types::{DeckChoice, SeatDelta, SeatKind, SeatState};
use tracing::{debug, info, warn};

use crate::filter::filter_state_for_player;
use crate::persist::{PersistedLobbyMeta, PersistedSession};
use crate::protocol::PlayerSlotInfo;
use crate::reconnect::ReconnectManager;

/// Result of handling a game action: raw state snapshot, events, legal actions, log entries, and spell costs.
/// The caller is responsible for filtering the state per-player before sending.
pub type ActionResult = (
    GameState,
    Vec<GameEvent>,
    Vec<GameAction>,
    Vec<GameLogEntry>,
    bool, // auto_pass_recommended
    HashMap<ObjectId, ManaCost>,
);

/// Returns the player who must act for the given WaitingFor, or None if the game is over.
pub fn acting_player(state: &GameState) -> Option<PlayerId> {
    engine::game::turn_control::authorized_submitter(state)
}

pub struct GameSession {
    pub game_code: String,
    pub state: GameState,
    /// Player tokens indexed by seat (0..player_count). Empty string = seat not yet claimed.
    pub player_tokens: Vec<String>,
    pub connected: Vec<bool>,
    pub decks: Vec<Option<PlayerDeckPayload>>,
    pub display_names: Vec<String>,
    pub timer_seconds: Option<u32>,
    /// Number of human player seats in this game.
    pub player_count: u8,
    /// Seats controlled by AI (not occupied by a human player).
    pub ai_seats: HashSet<PlayerId>,
    /// Per-AI-player configuration (difficulty, search params, etc.).
    pub ai_configs: HashMap<PlayerId, AiConfig>,
    /// Lobby metadata for games waiting for players. Set at creation, cleared when game fills.
    /// Stored here so it's available during shutdown flush without querying the LobbyManager.
    pub lobby_meta: Option<PersistedLobbyMeta>,
    /// True once the game has started (decks loaded, `start_game` called).
    /// A room can be full (`is_full()`) but not yet started — the host must
    /// send `SeatMutation::Start` to begin. Set by the existing auto-start
    /// paths in `join_game_with_name` and `create_game_with_ai`.
    pub game_started: bool,
}

impl GameSession {
    /// Returns the player index for the given token, if valid.
    pub fn player_for_token(&self, token: &str) -> Option<PlayerId> {
        self.player_tokens
            .iter()
            .position(|t| !t.is_empty() && t == token)
            .map(|i| PlayerId(i as u8))
    }

    /// Returns the first unclaimed human seat index, if any.
    /// AI seats are skipped — humans cannot join an AI-controlled seat.
    pub fn first_open_seat(&self) -> Option<usize> {
        self.player_tokens
            .iter()
            .enumerate()
            .position(|(i, t)| t.is_empty() && !self.ai_seats.contains(&PlayerId(i as u8)))
    }

    /// Returns true if all seats are claimed (by humans or AI).
    pub fn is_full(&self) -> bool {
        self.player_tokens
            .iter()
            .enumerate()
            .all(|(i, t)| !t.is_empty() || self.ai_seats.contains(&PlayerId(i as u8)))
    }

    /// Count of occupied seats — humans who have joined plus configured AI
    /// seats. Matches `is_full()` semantics: a full game has
    /// `current_player_count() == player_count`. Published on the public
    /// `LobbyGame` entry so browsers can see how close a game is to starting.
    pub fn current_player_count(&self) -> u32 {
        (0..self.player_count as usize)
            .filter(|i| {
                !self.player_tokens[*i].is_empty() || self.ai_seats.contains(&PlayerId(*i as u8))
            })
            .count() as u32
    }

    /// Returns true if the game hasn't started yet (mutations are still legal).
    pub fn is_pregame(&self) -> bool {
        !self.game_started
    }

    /// Build slot info for all seats in this game session.
    pub fn player_slot_info(&self) -> Vec<PlayerSlotInfo> {
        (0..self.player_count as usize)
            .map(|i| {
                let pid = PlayerId(i as u8);
                let is_ai = self.ai_seats.contains(&pid);
                let claimed = !self.player_tokens[i].is_empty();

                let kind = if i == 0 {
                    SeatKind::HostHuman
                } else if is_ai {
                    let difficulty = self
                        .ai_configs
                        .get(&pid)
                        .map(|c| c.difficulty)
                        .unwrap_or(AiDifficulty::Medium);
                    SeatKind::Ai {
                        difficulty,
                        deck: DeckChoice::Random,
                    }
                } else if claimed {
                    SeatKind::JoinedHuman
                } else {
                    SeatKind::WaitingHuman
                };

                PlayerSlotInfo {
                    player_id: pid.0,
                    name: if claimed || is_ai {
                        self.display_names[i].clone()
                    } else {
                        String::new()
                    },
                    kind,
                }
            })
            .collect()
    }

    pub fn seat_state(&self) -> SeatState {
        SeatState {
            seats: (0..self.player_count as usize)
                .map(|i| {
                    let pid = PlayerId(i as u8);
                    if i == 0 {
                        SeatKind::HostHuman
                    } else if self.ai_seats.contains(&pid) {
                        let difficulty = self
                            .ai_configs
                            .get(&pid)
                            .map(|c| c.difficulty)
                            .unwrap_or(AiDifficulty::Medium);
                        SeatKind::Ai {
                            difficulty,
                            deck: DeckChoice::Random,
                        }
                    } else if !self.player_tokens[i].is_empty() {
                        SeatKind::JoinedHuman
                    } else {
                        SeatKind::WaitingHuman
                    }
                })
                .collect(),
            tokens: self.player_tokens.clone(),
            format: self.state.format_config.clone(),
            game_started: self.game_started,
        }
    }

    fn rebuild_pregame_state(&mut self, player_count: u8) {
        let format_config = self.state.format_config.clone();
        let match_config = self.state.match_config;
        self.state = GameState::new(format_config, player_count, rand::rng().random());
        self.state.match_config = if player_count == 2 {
            match_config
        } else {
            MatchConfig::default()
        };
    }

    pub fn apply_seat_delta(&mut self, new_state: SeatState, delta: &SeatDelta) {
        let old_player_count = self.player_count;
        let new_player_count = new_state.seats.len() as u8;

        let mut old_to_new: Vec<Option<usize>> = (0..old_player_count as usize).map(Some).collect();
        if let Some(renumbering) = &delta.renumbering {
            old_to_new[renumbering.removed_index as usize] = None;
            for &(old_idx, new_idx) in &renumbering.remapping {
                old_to_new[old_idx as usize] = Some(new_idx as usize);
            }
        }

        let mut next_tokens = vec![String::new(); new_player_count as usize];
        let mut next_connected = vec![false; new_player_count as usize];
        let mut next_decks = vec![None; new_player_count as usize];
        let mut next_names = vec![String::new(); new_player_count as usize];

        for (old_idx, maybe_new_idx) in old_to_new
            .iter()
            .enumerate()
            .take(old_player_count as usize)
        {
            let Some(new_idx) = *maybe_new_idx else {
                continue;
            };
            next_tokens[new_idx] = self.player_tokens[old_idx].clone();
            next_connected[new_idx] = self.connected[old_idx];
            next_decks[new_idx] = self.decks[old_idx].clone();
            next_names[new_idx] = self.display_names[old_idx].clone();
        }

        self.player_count = new_player_count;
        self.player_tokens = next_tokens;
        self.connected = next_connected;
        self.decks = next_decks;
        self.display_names = next_names;
        self.ai_seats.clear();

        let mut next_ai_configs = HashMap::new();
        for (seat_idx, kind) in new_state.seats.iter().enumerate() {
            match kind {
                SeatKind::HostHuman | SeatKind::JoinedHuman => {}
                SeatKind::WaitingHuman => {
                    self.player_tokens[seat_idx].clear();
                    self.connected[seat_idx] = false;
                    self.decks[seat_idx] = None;
                    if seat_idx != 0 {
                        self.display_names[seat_idx].clear();
                    }
                }
                SeatKind::Ai { difficulty, .. } => {
                    let pid = PlayerId(seat_idx as u8);
                    self.ai_seats.insert(pid);
                    self.player_tokens[seat_idx].clear();
                    self.connected[seat_idx] = true;
                    self.display_names[seat_idx] = format!("AI ({difficulty:?})");
                    let config = phase_ai::config::create_config_for_players(
                        *difficulty,
                        Platform::Native,
                        new_player_count,
                    );
                    next_ai_configs.insert(pid, config);
                }
            }
        }

        for &(seat_idx, _, ref deck) in &delta.new_ai {
            self.decks[seat_idx as usize] = Some(deck.clone());
        }
        for &seat_idx in &delta.removed_ai {
            if seat_idx as usize >= self.decks.len() {
                continue;
            }
            if !delta
                .new_ai
                .iter()
                .any(|(new_idx, _, _)| *new_idx == seat_idx)
            {
                self.decks[seat_idx as usize] = None;
            }
        }

        self.ai_configs = next_ai_configs;
        self.game_started = new_state.game_started;

        if old_player_count != new_player_count {
            self.rebuild_pregame_state(new_player_count);
        }
    }

    pub fn start_game(&mut self) {
        let player_deck = self.decks[0].clone().unwrap_or(PlayerDeckPayload {
            main_deck: Vec::new(),
            sideboard: Vec::new(),
        });
        let opponent_deck = self.decks[1].clone().unwrap_or(PlayerDeckPayload {
            main_deck: Vec::new(),
            sideboard: Vec::new(),
        });
        let ai_decks: Vec<PlayerDeckPayload> = self.decks[2..]
            .iter()
            .map(|deck| {
                deck.clone().unwrap_or(PlayerDeckPayload {
                    main_deck: Vec::new(),
                    sideboard: Vec::new(),
                })
            })
            .collect();

        self.rebuild_pregame_state(self.player_count);
        load_deck_into_state(
            &mut self.state,
            &DeckPayload {
                player: player_deck,
                opponent: opponent_deck,
                ai_decks,
            },
        );
        self.state.log_player_names = self.display_names.clone();
        let _ = start_game(&mut self.state);
        self.game_started = true;
        self.lobby_meta = None;
    }

    /// Run AI actions and return per-action broadcast data.
    ///
    /// Each entry contains: raw state snapshot, events, legal actions, and log entries.
    /// The caller is responsible for filtering the state per-player before sending.
    /// Returns an empty vec if the session has no AI seats.
    pub fn run_ai(&mut self) -> Vec<ActionResult> {
        if self.ai_seats.is_empty() {
            return vec![];
        }

        let ai_results =
            phase_ai::auto_play::run_ai_actions(&mut self.state, &self.ai_seats, &self.ai_configs);

        if !ai_results.is_empty() {
            debug!(game = %self.game_code, ai_actions = ai_results.len(), "AI actions computed");
        }

        ai_results
            .into_iter()
            .map(|r| {
                let (legal, spell_costs) = engine_legal_actions_with_costs(&self.state);
                let auto_pass = auto_pass_recommended(&self.state, &legal);
                (
                    self.state.clone(),
                    r.events,
                    legal,
                    r.log_entries,
                    auto_pass,
                    spell_costs,
                )
            })
            .collect()
    }

    /// Create a serializable snapshot of this session for disk persistence.
    pub fn to_persisted(&self) -> PersistedSession {
        let ai_difficulties = self
            .ai_configs
            .iter()
            .map(|(pid, config)| (pid.0, config.difficulty))
            .collect();

        PersistedSession {
            game_code: self.game_code.clone(),
            state: self.state.clone(),
            player_tokens: self.player_tokens.clone(),
            display_names: self.display_names.clone(),
            timer_seconds: self.timer_seconds,
            player_count: self.player_count,
            ai_seats: self.ai_seats.iter().map(|pid| pid.0).collect(),
            ai_difficulties,
            game_started: self.game_started,
            lobby_meta: self.lobby_meta.clone(),
        }
    }

    /// Reconstruct a GameSession from a persisted snapshot.
    ///
    /// Restores fields that are `#[serde(skip)]` in GameState:
    /// - `all_card_names` from the provided card name list
    /// - `log_player_names` from the persisted display names
    /// - `rng` re-seeded with fresh randomness
    pub fn from_persisted(ps: PersistedSession, card_names: &[String]) -> Self {
        let mut state = ps.state;

        // Restore #[serde(skip)] fields
        state.all_card_names = card_names.to_vec().into();
        state.log_player_names = ps.display_names.clone();

        // Re-seed RNG with fresh randomness (stale rng_seed would produce
        // deterministic sequences identical across all restored games)
        let fresh_seed: u64 = rand::rng().random();
        state.rng_seed = fresh_seed;
        state.rng = rand_chacha::ChaCha20Rng::seed_from_u64(fresh_seed);
        finalize_public_state(&mut state);

        let ai_seats: HashSet<PlayerId> = ps.ai_seats.iter().map(|&s| PlayerId(s)).collect();

        let ai_configs: HashMap<PlayerId, AiConfig> = ps
            .ai_difficulties
            .iter()
            .map(|(&seat, &difficulty)| {
                let pid = PlayerId(seat);
                let config = phase_ai::config::create_config_for_players(
                    difficulty,
                    Platform::Native,
                    ps.player_count,
                );
                (pid, config)
            })
            .collect();

        let pc = ps.player_count as usize;

        GameSession {
            game_code: ps.game_code,
            state,
            player_tokens: ps.player_tokens,
            connected: vec![false; pc],
            decks: vec![None; pc],
            display_names: ps.display_names,
            timer_seconds: ps.timer_seconds,
            player_count: ps.player_count,
            ai_seats,
            ai_configs,
            lobby_meta: ps.lobby_meta,
            game_started: ps.game_started,
        }
    }
}

pub struct SessionManager {
    pub sessions: HashMap<String, GameSession>,
    pub reconnect: ReconnectManager,
    /// Maps player_token -> game_code for token-based lookups.
    token_to_game: HashMap<String, String>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::default(),
            token_to_game: HashMap::new(),
        }
    }

    pub fn with_grace_period(grace_period: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::new(grace_period),
            token_to_game: HashMap::new(),
        }
    }

    /// Create a new game session (2-player default). Returns (game_code, player_token).
    pub fn create_game(&mut self, deck: PlayerDeckPayload) -> (String, String) {
        self.create_game_n_players(deck, String::new(), None, 2, MatchConfig::default(), None)
    }

    /// Create a new game session with lobby settings (2-player default). Returns (game_code, player_token).
    pub fn create_game_with_settings(
        &mut self,
        deck: PlayerDeckPayload,
        display_name: String,
        timer_seconds: Option<u32>,
        match_config: MatchConfig,
    ) -> (String, String) {
        self.create_game_n_players(deck, display_name, timer_seconds, 2, match_config, None)
    }

    /// Create a new N-player game session. Returns (game_code, player_token).
    pub fn create_game_n_players(
        &mut self,
        deck: PlayerDeckPayload,
        display_name: String,
        timer_seconds: Option<u32>,
        player_count: u8,
        match_config: MatchConfig,
        format_config: Option<FormatConfig>,
    ) -> (String, String) {
        let game_code = generate_game_code();
        let player_token = generate_player_token();
        let pc = player_count as usize;

        let mut player_tokens = vec![String::new(); pc];
        player_tokens[0] = player_token.clone();
        let mut connected = vec![false; pc];
        connected[0] = true;
        let mut decks = vec![None; pc];
        decks[0] = Some(deck);
        let mut display_names = vec![String::new(); pc];
        display_names[0] = display_name;

        let mut state = GameState::new(
            format_config.unwrap_or_else(FormatConfig::standard),
            player_count,
            rand::rng().random(),
        );
        state.match_config = if player_count == 2 {
            match_config
        } else {
            MatchConfig::default()
        };

        let session = GameSession {
            game_code: game_code.clone(),
            state,
            player_tokens,
            connected,
            decks,
            display_names,
            timer_seconds,
            player_count,
            ai_seats: HashSet::new(),
            ai_configs: HashMap::new(),
            lobby_meta: None,
            game_started: false,
        };

        self.token_to_game
            .insert(player_token.clone(), game_code.clone());
        self.sessions.insert(game_code.clone(), session);

        info!(game = %game_code, player_count, "game session created");

        (game_code, player_token)
    }

    /// Join an existing game. Returns (player_id, player_token, initial_state_for_joiner) on success.
    pub fn join_game(
        &mut self,
        game_code: &str,
        deck: PlayerDeckPayload,
    ) -> Result<(String, GameState), String> {
        self.join_game_with_name(game_code, deck, String::new())
    }

    /// Join an existing game with a display name. Returns (player_token, initial_state_for_joiner) on success.
    /// Assigns the first open seat and starts the game when the last seat is filled.
    pub fn join_game_with_name(
        &mut self,
        game_code: &str,
        deck: PlayerDeckPayload,
        display_name: String,
    ) -> Result<(String, GameState), String> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {}", game_code))?;

        let seat = session
            .first_open_seat()
            .ok_or_else(|| "Game is already full".to_string())?;

        let player_token = generate_player_token();
        let player_id = PlayerId(seat as u8);
        session.player_tokens[seat] = player_token.clone();
        session.connected[seat] = true;
        session.decks[seat] = Some(deck);
        session.display_names[seat] = display_name;

        self.token_to_game
            .insert(player_token.clone(), game_code.to_string());

        info!(game = %game_code, player = ?player_id, seat, "player joined session");

        let filtered = filter_state_for_player(&session.state, player_id);
        Ok((player_token, filtered))
    }

    /// Set the full list of card names on a game session for "name a card" validation.
    pub fn set_card_names(&mut self, game_code: &str, names: Vec<String>) {
        if let Some(session) = self.sessions.get_mut(game_code) {
            session.state.all_card_names = names.into();
        }
    }

    /// Create a game with AI opponents. Returns (game_code, player_token) for the host.
    ///
    /// The host occupies seat 0. AI players are placed in the requested seats with
    /// their decks, configs, and display names. The game starts immediately.
    #[allow(clippy::too_many_arguments)]
    pub fn create_game_with_ai(
        &mut self,
        host_deck: PlayerDeckPayload,
        display_name: String,
        timer_seconds: Option<u32>,
        match_config: MatchConfig,
        ai_requests: Vec<(u8, AiDifficulty, PlayerDeckPayload)>,
        card_names: Vec<String>,
        format_config: Option<FormatConfig>,
    ) -> (String, String) {
        let total_players = 1 + ai_requests.len() as u8;
        let (game_code, player_token) = self.create_game_n_players(
            host_deck,
            display_name,
            timer_seconds,
            total_players,
            match_config,
            format_config,
        );

        let session = self.sessions.get_mut(&game_code).unwrap();
        for (seat_index, difficulty, deck) in &ai_requests {
            let seat = *seat_index as usize;
            session.display_names[seat] = format!("AI ({difficulty:?})");
            session.connected[seat] = true;
            session.decks[seat] = Some(deck.clone());
            let pid = PlayerId(*seat_index);
            session.ai_seats.insert(pid);
            let config = phase_ai::config::create_config_for_players(
                *difficulty,
                Platform::Native,
                total_players,
            );
            session.ai_configs.insert(pid, config);
        }

        session.state.all_card_names = card_names.into();
        session.start_game();

        (game_code, player_token)
    }

    /// Handle a game action from a player.
    /// Returns (filtered_states_per_player, events, legal_actions_for_next_actor) on success.
    #[allow(clippy::type_complexity)]
    pub fn handle_action(
        &mut self,
        game_code: &str,
        player_token: &str,
        action: GameAction,
    ) -> Result<ActionResult, String> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {}", game_code))?;

        let player = session
            .player_for_token(player_token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        // CancelAutoPass: any valid player can cancel their own flag regardless of whose turn it is.
        // This allows canceling UntilEndOfTurn while the opponent has priority.
        if matches!(action, GameAction::CancelAutoPass) {
            session.state.auto_pass.remove(&player);
            let (new_legal_actions, spell_costs) = engine_legal_actions_with_costs(&session.state);
            let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);
            return Ok((
                session.state.clone(),
                vec![],
                new_legal_actions,
                vec![],
                auto_pass,
                spell_costs,
            ));
        }

        // Validate it's this player's turn to act
        let current_actor = acting_player(&session.state);
        match current_actor {
            None => {
                warn!(game = %game_code, player = ?player, reason = "game_over", "action rejected");
                return Err("Game is over".to_string());
            }
            Some(actor) if actor != player => {
                warn!(game = %game_code, player = ?player, reason = "not_your_turn", "action rejected");
                return Err("Not your turn to act".to_string());
            }
            _ => {}
        }

        // Mana abilities skip the legal_actions pre-check — they are excluded from
        // legal_actions() for auto-pass purposes but validated by apply() directly.
        // SetAutoPass also skips (always legal when you have priority).
        let skip_legality =
            action.is_mana_ability() || matches!(action, GameAction::SetAutoPass { .. });
        if !skip_legality {
            let (legal_actions, _) = engine_legal_actions_with_costs(&session.state);
            if !legal_actions.contains(&action) {
                warn!(game = %game_code, player = ?player, reason = "illegal_action", "action rejected");
                return Err(format!("Illegal action: {:?}", action));
            }
        }

        // Set player names for log resolution
        session.state.log_player_names = session.display_names.clone();

        // Apply action. `player` is the PlayerId authenticated from the
        // WebSocket session (resolved from the join token) — never from the
        // action payload. The engine's guard in `apply` enforces
        // `player == authorized_submitter(state)`, so a spoofed action at the
        // wire is rejected inside the engine as well as here.
        let action_type = action.variant_name();
        let result = apply(&mut session.state, player, action).map_err(|e| {
            warn!(game = %game_code, player = ?player, error = %e, reason = "engine_error", "action rejected");
            format!("Engine error: {}", e)
        })?;

        info!(
            game = %game_code,
            player = ?player,
            action_type,
            event_count = result.events.len(),
            "action applied"
        );

        let (new_legal_actions, spell_costs) = engine_legal_actions_with_costs(&session.state);
        let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);

        Ok((
            session.state.clone(),
            result.events,
            new_legal_actions,
            result.log_entries,
            auto_pass,
            spell_costs,
        ))
    }

    /// Mark a player as disconnected.
    pub fn handle_disconnect(&mut self, game_code: &str, player: PlayerId) {
        if let Some(session) = self.sessions.get_mut(game_code) {
            session.connected[player.0 as usize] = false;
            self.reconnect.record_disconnect(game_code, player);
            info!(game = %game_code, player = ?player, "player disconnected");
        }
    }

    /// Attempt to reconnect a player. Returns their filtered state on success.
    pub fn handle_reconnect(
        &mut self,
        game_code: &str,
        player_token: &str,
    ) -> Result<GameState, String> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {}", game_code))?;

        let player = session
            .player_for_token(player_token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        // Check reconnect grace period
        let result = self.reconnect.attempt_reconnect(game_code, player);
        match result {
            crate::reconnect::ReconnectResult::Ok { .. } => {
                session.connected[player.0 as usize] = true;
                Ok(filter_state_for_player(&session.state, player))
            }
            crate::reconnect::ReconnectResult::Expired => {
                Err("Reconnect grace period expired".to_string())
            }
            crate::reconnect::ReconnectResult::NotFound => {
                // Player wasn't marked as disconnected -- allow reconnect anyway
                session.connected[player.0 as usize] = true;
                Ok(filter_state_for_player(&session.state, player))
            }
        }
    }

    /// Returns game codes waiting for more players (for lobby).
    pub fn open_games(&self) -> Vec<String> {
        self.sessions
            .values()
            .filter(|s| !s.is_full())
            .map(|s| s.game_code.clone())
            .collect()
    }

    /// Look up game_code by player_token.
    pub fn game_for_token(&self, token: &str) -> Option<&str> {
        self.token_to_game.get(token).map(|s| s.as_str())
    }

    /// Restore a pre-built session (e.g., from disk persistence).
    /// Registers all player tokens in the token-to-game index.
    pub fn restore_session(&mut self, session: GameSession) {
        let game_code = session.game_code.clone();
        for token in &session.player_tokens {
            if !token.is_empty() {
                self.token_to_game.insert(token.clone(), game_code.clone());
            }
        }
        self.sessions.insert(game_code, session);
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

pub fn generate_game_code() -> String {
    let mut rng = rand::rng();
    let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect();
    (0..6)
        .map(|_| chars[rng.random_range(0..chars.len())])
        .collect()
}

pub fn generate_player_token() -> String {
    let mut rng = rand::rng();
    (0..32)
        .map(|_| format!("{:x}", rng.random_range(0u8..16)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::deck_loading::DeckEntry;
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::game_state::WaitingFor;
    use engine::types::mana::ManaCost;

    fn make_deck() -> PlayerDeckPayload {
        PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: CardFace {
                    name: "Forest".to_string(),
                    mana_cost: ManaCost::NoCost,
                    card_type: CardType {
                        supertypes: vec![],
                        core_types: vec![engine::types::card_type::CoreType::Land],
                        subtypes: vec!["Forest".to_string()],
                    },
                    power: None,
                    toughness: None,
                    loyalty: None,
                    defense: None,
                    oracle_text: None,
                    non_ability_text: None,
                    flavor_name: None,
                    keywords: vec![],
                    abilities: vec![],
                    triggers: vec![],
                    static_abilities: vec![],
                    replacements: vec![],
                    color_override: None,
                    scryfall_oracle_id: None,
                    modal: None,
                    additional_cost: None,
                    strive_cost: None,
                    casting_restrictions: vec![],
                    casting_options: vec![],
                    solve_condition: None,
                    parse_warnings: vec![],
                    brawl_commander: false,
                    metadata: Default::default(),
                },
                count: 10,
            }],
            sideboard: Vec::new(),
        }
    }

    #[test]
    fn create_game_returns_code_and_token() {
        let mut mgr = SessionManager::new();
        let (code, token) = mgr.create_game(make_deck());
        assert_eq!(code.len(), 6);
        assert_eq!(token.len(), 32);
    }

    #[test]
    fn create_then_join_works() {
        let mut mgr = SessionManager::new();
        let (code, _token1) = mgr.create_game(make_deck());
        let result = mgr.join_game(&code, make_deck());
        assert!(result.is_ok());
        let (token2, _state) = result.unwrap();
        assert_eq!(token2.len(), 32);
    }

    #[test]
    fn join_nonexistent_game_fails() {
        let mut mgr = SessionManager::new();
        let result = mgr.join_game("NOPE00", make_deck());
        assert!(result.is_err());
    }

    #[test]
    fn join_full_game_fails() {
        let mut mgr = SessionManager::new();
        let (code, _) = mgr.create_game(make_deck());
        let _ = mgr.join_game(&code, make_deck());
        let result = mgr.join_game(&code, make_deck());
        assert!(result.is_err());
    }

    #[test]
    fn action_from_wrong_player_rejected() {
        let mut mgr = SessionManager::new();
        let (code, token1) = mgr.create_game(make_deck());
        let (token2, _) = mgr.join_game(&code, make_deck()).unwrap();

        // Determine which player has priority
        let session = mgr.sessions.get(&code).unwrap();
        let acting = match &session.state.waiting_for {
            WaitingFor::Priority { player } => *player,
            WaitingFor::MulliganDecision { player, .. } => *player,
            other => panic!("unexpected waiting_for: {:?}", other),
        };

        // Use the wrong player's token
        let wrong_token = if acting == PlayerId(0) {
            &token2
        } else {
            &token1
        };

        let result = mgr.handle_action(&code, wrong_token, GameAction::PassPriority);
        assert!(result.is_err());
    }

    #[test]
    fn open_games_lists_waiting_sessions() {
        let mut mgr = SessionManager::new();
        let (code1, _) = mgr.create_game(make_deck());
        let (code2, _) = mgr.create_game(make_deck());
        let _ = mgr.join_game(&code1, make_deck());

        let open = mgr.open_games();
        assert_eq!(open.len(), 1);
        assert!(open.contains(&code2));
    }

    #[test]
    fn disconnect_and_reconnect_works() {
        let mut mgr = SessionManager::new();
        let (code, token1) = mgr.create_game(make_deck());
        let _ = mgr.join_game(&code, make_deck()).unwrap();

        mgr.handle_disconnect(&code, PlayerId(0));
        let result = mgr.handle_reconnect(&code, &token1);
        assert!(result.is_ok());
    }

    #[test]
    fn reconnect_restores_between_games_waiting_state() {
        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let _ = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        session.state.match_phase = engine::types::match_config::MatchPhase::BetweenGames;
        session.state.waiting_for = WaitingFor::BetweenGamesSideboard {
            player: PlayerId(0),
            game_number: 2,
            score: engine::types::match_config::MatchScore {
                p0_wins: 1,
                p1_wins: 0,
                draws: 0,
            },
        };

        mgr.handle_disconnect(&code, PlayerId(0));
        let filtered = mgr.handle_reconnect(&code, &token0).unwrap();
        assert!(matches!(
            filtered.waiting_for,
            WaitingFor::BetweenGamesSideboard {
                player: PlayerId(0),
                game_number: 2,
                ..
            }
        ));
    }

    #[test]
    fn game_code_is_uppercase_alphanumeric() {
        let code = generate_game_code();
        assert!(code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
    }

    #[test]
    fn player_token_is_hex() {
        let token = generate_player_token();
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
