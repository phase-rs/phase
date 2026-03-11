use std::collections::{HashMap, HashSet};
use std::time::Duration;

use engine::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload};
use engine::game::engine::{apply, start_game};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::get_legal_actions;
use rand::Rng;

use crate::filter::filter_state_for_player;
use crate::reconnect::ReconnectManager;

/// Result of handling a game action: per-player filtered states, events, and legal actions.
type ActionResult = (Vec<(PlayerId, GameState)>, Vec<GameEvent>, Vec<GameAction>);

/// Returns the player who must act for the given WaitingFor, or None if the game is over.
pub fn acting_player(waiting_for: &WaitingFor) -> Option<PlayerId> {
    match waiting_for {
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
        | WaitingFor::TriggerTargetSelection { player, .. } => Some(*player),
        WaitingFor::GameOver { .. } => None,
    }
}

pub struct GameSession {
    pub game_code: String,
    pub state: GameState,
    /// Player tokens indexed by seat (0..player_count). Empty string = seat not yet claimed.
    pub player_tokens: Vec<String>,
    pub connected: Vec<bool>,
    pub decks: Vec<Option<Vec<DeckEntry>>>,
    pub display_names: Vec<String>,
    pub timer_seconds: Option<u32>,
    /// Number of human player seats in this game.
    pub player_count: u8,
    /// Seats controlled by AI (not occupied by a human player).
    pub ai_seats: HashSet<PlayerId>,
}

impl GameSession {
    /// Returns the player index for the given token, if valid.
    pub fn player_for_token(&self, token: &str) -> Option<PlayerId> {
        self.player_tokens
            .iter()
            .position(|t| !t.is_empty() && t == token)
            .map(|i| PlayerId(i as u8))
    }

    /// Returns the first unclaimed seat index, if any.
    pub fn first_open_seat(&self) -> Option<usize> {
        self.player_tokens.iter().position(|t| t.is_empty())
    }

    /// Returns true if all seats are claimed.
    pub fn is_full(&self) -> bool {
        self.player_tokens.iter().all(|t| !t.is_empty())
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
    pub fn create_game(&mut self, deck: Vec<DeckEntry>) -> (String, String) {
        self.create_game_n_players(deck, String::new(), None, 2)
    }

    /// Create a new game session with lobby settings (2-player default). Returns (game_code, player_token).
    pub fn create_game_with_settings(
        &mut self,
        deck: Vec<DeckEntry>,
        display_name: String,
        timer_seconds: Option<u32>,
    ) -> (String, String) {
        self.create_game_n_players(deck, display_name, timer_seconds, 2)
    }

    /// Create a new N-player game session. Returns (game_code, player_token).
    pub fn create_game_n_players(
        &mut self,
        deck: Vec<DeckEntry>,
        display_name: String,
        timer_seconds: Option<u32>,
        player_count: u8,
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

        let session = GameSession {
            game_code: game_code.clone(),
            state: GameState::new_two_player(rand::rng().random()),
            player_tokens,
            connected,
            decks,
            display_names,
            timer_seconds,
            player_count,
            ai_seats: HashSet::new(),
        };

        self.token_to_game
            .insert(player_token.clone(), game_code.clone());
        self.sessions.insert(game_code.clone(), session);

        (game_code, player_token)
    }

    /// Join an existing game. Returns (player_id, player_token, initial_state_for_joiner) on success.
    pub fn join_game(
        &mut self,
        game_code: &str,
        deck: Vec<DeckEntry>,
    ) -> Result<(String, GameState), String> {
        self.join_game_with_name(game_code, deck, String::new())
    }

    /// Join an existing game with a display name. Returns (player_token, initial_state_for_joiner) on success.
    /// Assigns the first open seat and starts the game when the last seat is filled.
    pub fn join_game_with_name(
        &mut self,
        game_code: &str,
        deck: Vec<DeckEntry>,
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

        // Start the game when the last human seat is filled
        if session.is_full() {
            // Load deck data into game state before starting
            let player_deck = session.decks[0].clone().unwrap_or_default();
            let opponent_deck = session.decks[1].clone().unwrap_or_default();
            let payload = DeckPayload {
                player_deck,
                opponent_deck,
            };
            load_deck_into_state(&mut session.state, &payload);

            // Initialize the game via engine
            let _result = start_game(&mut session.state);
        }

        let filtered = filter_state_for_player(&session.state, player_id);
        Ok((player_token, filtered))
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

        // Validate it's this player's turn to act
        let current_actor = acting_player(&session.state.waiting_for);
        match current_actor {
            None => return Err("Game is over".to_string()),
            Some(actor) if actor != player => return Err("Not your turn to act".to_string()),
            _ => {}
        }

        // Validate action is legal
        let legal_actions = get_legal_actions(&session.state);
        if !legal_actions.contains(&action) {
            return Err(format!("Illegal action: {:?}", action));
        }

        // Apply action
        let result =
            apply(&mut session.state, action).map_err(|e| format!("Engine error: {}", e))?;

        // Filter state for each player
        let filtered_states: Vec<(PlayerId, GameState)> = (0..session.player_count)
            .map(|i| {
                let pid = PlayerId(i);
                (pid, filter_state_for_player(&session.state, pid))
            })
            .collect();
        let new_legal_actions = get_legal_actions(&session.state);

        Ok((filtered_states, result.events, new_legal_actions))
    }

    /// Mark a player as disconnected.
    pub fn handle_disconnect(&mut self, game_code: &str, player: PlayerId) {
        if let Some(session) = self.sessions.get_mut(game_code) {
            session.connected[player.0 as usize] = false;
            self.reconnect.record_disconnect(game_code, player);
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
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

fn generate_game_code() -> String {
    let mut rng = rand::rng();
    let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect();
    (0..6)
        .map(|_| chars[rng.random_range(0..chars.len())])
        .collect()
}

fn generate_player_token() -> String {
    let mut rng = rand::rng();
    (0..32)
        .map(|_| format!("{:x}", rng.random_range(0u8..16)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::mana::ManaCost;

    fn make_deck() -> Vec<DeckEntry> {
        vec![DeckEntry {
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
            },
            count: 10,
        }]
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
