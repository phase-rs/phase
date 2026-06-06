use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine::ai_support::{auto_pass_recommended, legal_actions_full as engine_legal_actions_full};
use engine::database::legality::{validate_cedh_bracket, CedhBracketError};
use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckPayload, PlayerDeckPayload};
use engine::game::engine::{apply, start_game};
use engine::game::finalize_public_state;
use engine::game::{load_and_hydrate_decks, rehydrate_game_from_card_db};
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

/// Result of handling a game action: raw state snapshot, events, legal actions, log entries,
/// auto-pass flag, spell costs, and per-object action grouping.
/// The caller is responsible for filtering the state per-player before sending.
pub type ActionResult = (
    GameState,
    Vec<GameEvent>,
    Vec<GameAction>,
    Vec<GameLogEntry>,
    bool, // auto_pass_recommended
    HashMap<ObjectId, ManaCost>,
    // Per-object grouping of legal actions, keyed by `GameAction::source_object()`.
    // Required by the frontend's `collectObjectActions(...)` lookup for card clicks;
    // dropping this field leaves guests unable to play lands or cast spells.
    HashMap<ObjectId, Vec<GameAction>>,
);

pub const PUBLIC_SEAT_RESERVATION_MS: u64 = 120_000;

#[derive(Debug, Clone)]
pub struct SeatReservation {
    pub token: String,
    pub display_name: String,
    pub seat_index: usize,
    pub expires_at_ms: Option<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Returns the player who must act for the given WaitingFor, or None if the game is over.
pub fn acting_player(state: &GameState) -> Option<PlayerId> {
    engine::game::turn_control::authorized_submitter(state)
}

/// CR 103.5: Set of players who may act in the current WaitingFor — full
/// pending set for simultaneous-decision states, single-element for everything
/// else. Used by multiplayer transports to broadcast legal actions to every
/// pending player concurrently.
pub fn acting_players(state: &GameState) -> Vec<PlayerId> {
    engine::game::turn_control::authorized_submitters(state)
}

/// CR 103.5: True iff `player` is one of the actors permitted to submit an
/// action for the current WaitingFor. Replaces the
/// `acting_player(state) == Some(player)` idiom at multiplayer routing sites
/// so the simultaneous-decision states (MulliganDecision, MulliganBottomCards,
/// OpeningHandBottomCards)
/// route legal actions to every pending player, not just the first.
pub fn is_acting(state: &GameState, player: PlayerId) -> bool {
    engine::game::turn_control::is_authorized_submitter(state, player)
}

pub struct GameSession {
    pub game_code: String,
    pub state: GameState,
    /// Player tokens indexed by seat (0..player_count). Empty string = seat not yet claimed.
    pub player_tokens: Vec<String>,
    pub connected: Vec<bool>,
    pub decks: Vec<Option<PlayerDeckPayload>>,
    pub display_names: Vec<String>,
    /// Pre-deck seat reservations keyed by reservation token. Reservations
    /// are in-memory only; stale public reservations expire, and private-room
    /// reservations are released by socket cleanup.
    pub reservations: HashMap<String, SeatReservation>,
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
    /// Host preference: start automatically when every configured seat is
    /// occupied by a joined human or AI.
    pub start_when_full: bool,
    /// Ranked rooms apply rating updates when a match completes.
    pub ranked: bool,
    /// Engine events produced by `start_game` (the d20 first-player contest's
    /// `StartingPlayerContest` event). Captured here so the INITIAL post-start
    /// broadcast can surface them to clients; cleared after that broadcast so
    /// late joiners and reconnects do not re-receive the contest. Empty when the
    /// game has not started or the events have already been broadcast.
    pub start_events: Vec<GameEvent>,
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
        self.player_tokens.iter().enumerate().position(|(i, t)| {
            t.is_empty()
                && !self.ai_seats.contains(&PlayerId(i as u8))
                && !self.reservations.values().any(|r| r.seat_index == i)
        })
    }

    /// Returns true if all seats are actually occupied by joined humans or AI.
    /// Reservations hold capacity for lobby UX but do not make a game ready
    /// to start because the reserved player has not submitted a deck yet.
    pub fn is_full(&self) -> bool {
        self.player_tokens
            .iter()
            .enumerate()
            .all(|(i, t)| !t.is_empty() || self.ai_seats.contains(&PlayerId(i as u8)))
    }

    /// Count of occupied seats — humans who have joined plus configured AI
    /// seats and active reservations. Published on the public `LobbyGame`
    /// entry so browsers can see held seats as unavailable.
    pub fn current_player_count(&self) -> u32 {
        (0..self.player_count as usize)
            .filter(|i| {
                !self.player_tokens[*i].is_empty()
                    || self.ai_seats.contains(&PlayerId(*i as u8))
                    || self.reservations.values().any(|r| r.seat_index == *i)
            })
            .count() as u32
    }

    pub fn cleanup_expired_reservations(&mut self) -> bool {
        let before = self.reservations.len();
        let now = now_ms();
        self.reservations.retain(|_, reservation| {
            reservation
                .expires_at_ms
                .is_none_or(|expires| expires > now)
        });
        before != self.reservations.len()
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
                let reservation = self
                    .reservations
                    .values()
                    .find(|reservation| reservation.seat_index == i);

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
                    } else if let Some(reservation) = reservation {
                        reservation.display_name.clone()
                    } else {
                        String::new()
                    },
                    kind,
                    reserved: reservation.is_some(),
                    reservation_expires_at_ms: reservation.and_then(|r| r.expires_at_ms),
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
        // Preserve sandbox seeding through rematch — the format flag is
        // immutable, so debug capability survives the new game. Every seat
        // is permitted by default (see initial create site for rationale);
        // explicit revocations from the previous game are dropped at rematch
        // since the new game is a fresh debug context.
        if self.state.format_config.allow_debug_actions {
            self.state.debug_mode = true;
            for i in 0..player_count {
                self.state.debug_permitted.insert(PlayerId(i));
            }
        }
    }

    pub fn apply_seat_delta(&mut self, new_state: SeatState, delta: &SeatDelta, db: &CardDatabase) {
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
        let mut next_reservations = HashMap::new();

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

        for reservation in self.reservations.values() {
            let Some(new_idx) = old_to_new
                .get(reservation.seat_index)
                .and_then(|maybe| *maybe)
            else {
                continue;
            };
            let mut reservation = reservation.clone();
            reservation.seat_index = new_idx;
            next_reservations.insert(reservation.token.clone(), reservation);
        }

        self.player_count = new_player_count;
        self.player_tokens = next_tokens;
        self.connected = next_connected;
        self.decks = next_decks;
        self.display_names = next_names;
        self.reservations = next_reservations;
        self.ai_seats.clear();

        let mut next_ai_configs = HashMap::new();
        for (seat_idx, kind) in new_state.seats.iter().enumerate() {
            match kind {
                SeatKind::HostHuman | SeatKind::JoinedHuman => {}
                SeatKind::WaitingHuman => {
                    self.player_tokens[seat_idx].clear();
                    self.connected[seat_idx] = false;
                    self.decks[seat_idx] = None;
                    self.reservations
                        .retain(|_, reservation| reservation.seat_index != seat_idx);
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
                    self.reservations
                        .retain(|_, reservation| reservation.seat_index != seat_idx);
                    let config = phase_ai::config::create_config_for_players(
                        *difficulty,
                        Platform::Native,
                        new_player_count,
                    );
                    next_ai_configs.insert(pid, config);
                }
            }
        }

        // SeatDelta carries name-only `PlayerDeckList` (see DeckResolver docs);
        // server-core's `self.decks` stores the fully-resolved `PlayerDeckPayload`
        // because `start_game` and the broadcast paths consume that shape.
        // Resolve at the boundary using the live `CardDatabase`. `resolve_deck`
        // takes a `DeckData` which has the same shape as `PlayerDeckList`.
        for (seat_idx, _, ref deck) in &delta.new_ai {
            let deck_data = crate::starter_decks::DeckData {
                main_deck: deck.main_deck.clone(),
                sideboard: deck.sideboard.clone(),
                commander: deck.commander.clone(),
                attraction_deck: deck.attraction_deck.clone(),
                signature_spell: deck.signature_spell.clone(),
                bracket_tier: deck.bracket_tier,
            };
            // The resolver (`ServerDeckResolver::resolve` in phase-server)
            // has already validated these names against the same `db`, so
            // this should never error in practice. The `Err` arm exists as
            // defense-in-depth — if it does fire, log loudly: `start_game`
            // below would otherwise substitute an empty deck and silently
            // eliminate the player on their first draw step (CR 704.5b).
            self.decks[*seat_idx as usize] = match crate::resolve_deck(db, &deck_data) {
                Ok(payload) => Some(payload),
                Err(err) => {
                    warn!(
                        seat = *seat_idx,
                        error = %err,
                        "AI deck failed re-resolution at apply_seat_delta despite \
                         passing the resolver gate; seat will start with an empty \
                         library — investigate the resolver/DB mismatch",
                    );
                    None
                }
            };
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

    pub fn start_game(&mut self, db: &CardDatabase) -> Result<(), CedhBracketError> {
        // Gate: if any AI seat is configured for cEDH difficulty, validate that
        // every submitted deck is declared at the cEDH bracket tier before
        // mutating any session state.
        let is_cedh = self
            .ai_configs
            .values()
            .any(|c| c.difficulty == AiDifficulty::CEDH);

        if is_cedh {
            let deck_refs = self
                .decks
                .iter()
                .filter_map(|slot| slot.as_ref())
                .collect::<Vec<_>>();
            validate_cedh_bracket(&deck_refs)?;
        }

        let player_deck = self.decks[0].clone().unwrap_or_default();
        let opponent_deck = self.decks[1].clone().unwrap_or_default();
        let ai_decks: Vec<PlayerDeckPayload> = self.decks[2..]
            .iter()
            .map(|deck| deck.clone().unwrap_or_default())
            .collect();

        self.rebuild_pregame_state(self.player_count);
        // Canonical init sequence — see `engine::game::load_and_hydrate_decks`.
        // Replaces the prior `load_deck_into_state` + `rehydrate_game_from_card_db`
        // pairing that each transport layer (WASM, server-core, Tauri) used to
        // duplicate. Consolidating here is what prevents dual-faced cards
        // (Adventure, Omen, MDFC, Transform, Meld) from silently regressing
        // again when the init contract evolves.
        load_and_hydrate_decks(
            &mut self.state,
            &DeckPayload {
                player: player_deck,
                opponent: opponent_deck,
                ai_decks,
                // Multiplayer server does not enforce the cEDH gate at the
                // session layer (it plumbs bracket tier through separately).
                // Default to empty so old clients without ai_difficulties
                // deserialize safely.
                ai_difficulties: vec![],
            },
            Some(db),
        );
        self.state.log_player_names = self.display_names.clone();
        // Capture the d20 first-player contest events so the initial broadcast
        // can surface them; the broadcaster clears `start_events` afterward so
        // joiners/reconnects do not re-see the dice.
        let result = start_game(&mut self.state);
        self.start_events = result.events;
        self.game_started = true;
        self.lobby_meta = None;
        Ok(())
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
                let (legal, spell_costs, by_object) = engine_legal_actions_full(&r.state);
                let auto_pass = auto_pass_recommended(&r.state, &legal);
                (
                    r.state,
                    r.events,
                    legal,
                    r.log_entries,
                    auto_pass,
                    spell_costs,
                    by_object,
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
            start_when_full: self.start_when_full,
            ranked: self.ranked,
            lobby_meta: self.lobby_meta.clone(),
        }
    }

    /// Reconstruct a GameSession from a persisted snapshot.
    ///
    /// Restores fields that are `#[serde(skip)]` in GameState:
    /// - `all_card_names` from the card database
    /// - card characteristics from the card database
    /// - `log_player_names` from the persisted display names
    /// - `rng` re-seeded with fresh randomness
    pub fn from_persisted(ps: PersistedSession, db: &CardDatabase) -> Self {
        let mut state = ps.state;

        // Restore #[serde(skip)] fields
        state.all_card_names = db.card_names().into();
        state.log_player_names = ps.display_names.clone();
        rehydrate_game_from_card_db(&mut state, db);

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
            reservations: HashMap::new(),
            timer_seconds: ps.timer_seconds,
            player_count: ps.player_count,
            ai_seats,
            ai_configs,
            lobby_meta: ps.lobby_meta,
            game_started: ps.game_started,
            start_when_full: ps.start_when_full,
            ranked: ps.ranked,
            start_events: Vec::new(),
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
        // Sandbox capability: the engine-level `debug_mode` gate must agree
        // with the transport-level `allow_debug_actions` flag, otherwise a
        // sandbox-permitted action would pass the server gate only to be
        // rejected inside `apply`. Every seat is permitted by default — a
        // sandbox is a shared playground, not an admin console. The host's
        // grant/revoke flow remains (for the rare "kick this seat out of
        // debug" case) but is no longer the gate for normal sandbox use.
        if state.format_config.allow_debug_actions {
            state.debug_mode = true;
            for i in 0..player_count {
                state.debug_permitted.insert(PlayerId(i));
            }
        }

        let session = GameSession {
            game_code: game_code.clone(),
            state,
            player_tokens,
            connected,
            decks,
            display_names,
            reservations: HashMap::new(),
            timer_seconds,
            player_count,
            ai_seats: HashSet::new(),
            ai_configs: HashMap::new(),
            lobby_meta: None,
            game_started: false,
            start_when_full: true,
            ranked: false,
            start_events: Vec::new(),
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
        self.join_game_with_name_and_reservation(game_code, deck, display_name, None)
    }

    pub fn reserve_seat(
        &mut self,
        game_code: &str,
        display_name: String,
    ) -> Result<SeatReservation, String> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {}", game_code))?;
        session.cleanup_expired_reservations();
        if session.game_started {
            return Err("Game has already started".to_string());
        }

        let seat = session
            .first_open_seat()
            .ok_or_else(|| "Game is already full".to_string())?;
        let token = generate_player_token();
        let expires_at_ms = Some(now_ms() + PUBLIC_SEAT_RESERVATION_MS);
        let reservation = SeatReservation {
            token: token.clone(),
            display_name,
            seat_index: seat,
            expires_at_ms,
        };
        session.reservations.insert(token, reservation.clone());
        Ok(reservation)
    }

    pub fn release_reservation(&mut self, game_code: &str, reservation_token: &str) -> bool {
        self.sessions
            .get_mut(game_code)
            .and_then(|session| session.reservations.remove(reservation_token))
            .is_some()
    }

    pub fn has_active_reservation(&mut self, game_code: &str, reservation_token: &str) -> bool {
        let Some(session) = self.sessions.get_mut(game_code) else {
            return false;
        };
        session.cleanup_expired_reservations();
        session.reservations.contains_key(reservation_token)
    }

    pub fn release_reservations(&mut self, reservations: &[(String, String)]) -> bool {
        let mut changed = false;
        for (game_code, token) in reservations {
            changed |= self.release_reservation(game_code, token);
        }
        changed
    }

    pub fn join_game_with_name_and_reservation(
        &mut self,
        game_code: &str,
        deck: PlayerDeckPayload,
        display_name: String,
        reservation_token: Option<String>,
    ) -> Result<(String, GameState), String> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {}", game_code))?;

        session.cleanup_expired_reservations();
        let reservation = match reservation_token.as_deref() {
            Some(token) => Some(
                session
                    .reservations
                    .remove(token)
                    .ok_or_else(|| "Seat reservation expired or was released".to_string())?,
            ),
            None => None,
        };
        let seat = if let Some(reservation) = &reservation {
            reservation.seat_index
        } else {
            session
                .first_open_seat()
                .ok_or_else(|| "Game is already full".to_string())?
        };

        let player_token = generate_player_token();
        let player_id = PlayerId(seat as u8);
        session.player_tokens[seat] = player_token.clone();
        session.connected[seat] = true;
        session.decks[seat] = Some(deck);
        session.display_names[seat] = if display_name.is_empty() {
            reservation
                .as_ref()
                .map(|reservation| reservation.display_name.clone())
                .unwrap_or_default()
        } else {
            display_name
        };

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
        db: &CardDatabase,
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
        session
            .start_game(db)
            .expect("start_game in tests should not hit cEDH validation");

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

        // Sandbox capability gate. A `Debug(_)` is accepted only when the
        // session was created in sandbox mode AND the submitting player is in
        // the `debug_permitted` set. The set is host-managed via
        // `GrantDebugPermission` / `RevokeDebugPermission` and is initialized
        // to `{host}` when the game is sandbox-flagged.
        if matches!(action, GameAction::Debug(_))
            && !session.state.debug_permitted.contains(&player)
        {
            return Err(
                "Debug actions are not permitted (Sandbox mode disabled or no permission)"
                    .to_string(),
            );
        }

        // Grant/Revoke debug permission: host-only, and only meaningful in a
        // sandbox session. The host is always PlayerId(0). The host cannot
        // revoke their own permission (would leave nobody able to debug).
        const HOST_PLAYER: PlayerId = PlayerId(0);
        match &action {
            GameAction::GrantDebugPermission { .. } | GameAction::RevokeDebugPermission { .. } => {
                if !session.state.format_config.allow_debug_actions {
                    return Err("Sandbox mode is not enabled for this game".to_string());
                }
                if player != HOST_PLAYER {
                    return Err("Only the host can grant or revoke debug permission".to_string());
                }
                if let GameAction::RevokeDebugPermission {
                    player_id: target, ..
                } = &action
                {
                    if *target == HOST_PLAYER {
                        return Err("The host cannot revoke their own debug permission".to_string());
                    }
                }
            }
            _ => {}
        }

        // CancelAutoPass: any valid player can cancel their own flag regardless of whose turn it is.
        // This allows canceling UntilEndOfTurn while the opponent has priority.
        if matches!(action, GameAction::CancelAutoPass) {
            session.state.auto_pass.remove(&player);
            let (new_legal_actions, spell_costs, by_object) =
                engine_legal_actions_full(&session.state);
            let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);
            return Ok((
                session.state.clone(),
                vec![],
                new_legal_actions,
                vec![],
                auto_pass,
                spell_costs,
                by_object,
            ));
        }

        // SetPhaseStops: preference propagation keyed to the authenticated player,
        // not whoever currently holds priority. Mirrors CancelAutoPass — the engine's
        // own handler would key by `authorized_submitter`, which is the priority
        // holder in multiplayer, so we must intercept here to write to the correct
        // player's entry.
        if let GameAction::SetPhaseStops { stops } = &action {
            if stops.is_empty() {
                session.state.phase_stops.remove(&player);
            } else {
                session.state.phase_stops.insert(player, stops.clone());
            }
            let (new_legal_actions, spell_costs, by_object) =
                engine_legal_actions_full(&session.state);
            let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);
            return Ok((
                session.state.clone(),
                vec![],
                new_legal_actions,
                vec![],
                auto_pass,
                spell_costs,
                by_object,
            ));
        }

        // ReorderHand: per-player display-preference update keyed to the
        // authenticated player, not the priority holder. Mirrors
        // CancelAutoPass / SetPhaseStops by bypassing the turn/legal-action
        // prechecks, but still delegates validation and mutation to the engine
        // so all adapters share one authoritative contract.
        //
        // CR 402.3: The order of cards in a player's hand is not defined by
        // the rules; players may arrange them as they choose. Hand reordering
        // has no game-rules consequence.
        if matches!(action, GameAction::ReorderHand { .. }) {
            let result = apply(&mut session.state, player, action).map_err(|e| {
                warn!(game = %game_code, player = ?player, error = %e, reason = "engine_error", "action rejected");
                format!("Engine error: {}", e)
            })?;
            let (new_legal_actions, spell_costs, by_object) =
                engine_legal_actions_full(&session.state);
            let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);
            return Ok((
                session.state.clone(),
                result.events,
                new_legal_actions,
                result.log_entries,
                auto_pass,
                spell_costs,
                by_object,
            ));
        }

        // Debug / Grant / Revoke bypass the priority-holder and legal-action
        // gates entirely — they're out-of-band sandbox controls already gated
        // by `debug_permitted` (Debug) and host-only checks (Grant/Revoke)
        // above. Mirror the ReorderHand path: delegate to engine, broadcast
        // the audit event.
        if matches!(
            action,
            GameAction::Debug(_)
                | GameAction::GrantDebugPermission { .. }
                | GameAction::RevokeDebugPermission { .. }
        ) {
            let result = apply(&mut session.state, player, action).map_err(|e| {
                warn!(game = %game_code, player = ?player, error = %e, reason = "engine_error", "action rejected");
                format!("Engine error: {}", e)
            })?;
            let (new_legal_actions, spell_costs, by_object) =
                engine_legal_actions_full(&session.state);
            let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);
            return Ok((
                session.state.clone(),
                result.events,
                new_legal_actions,
                result.log_entries,
                auto_pass,
                spell_costs,
                by_object,
            ));
        }

        // Validate it's this player's turn to act.
        // CR 103.5: For simultaneous mulligan states, every pending player is
        // an authorized actor — use set membership rather than equality with
        // the (None-returning) representative.
        let authorized = acting_players(&session.state);
        if authorized.is_empty() {
            warn!(game = %game_code, player = ?player, reason = "game_over", "action rejected");
            return Err("Game is over".to_string());
        }
        if !authorized.contains(&player) {
            warn!(game = %game_code, player = ?player, reason = "not_your_turn", "action rejected");
            return Err("Not your turn to act".to_string());
        }

        // Mana abilities skip the legal_actions pre-check — they are excluded from
        // legal_actions() for auto-pass purposes but validated by apply() directly.
        // SetAutoPass also skips (always legal when you have priority).
        // Scry/surveil keep-on-top selections (CR 701.22a / CR 701.25a) also skip:
        // their legal set is every duplicate-free subset in any order, which cannot
        // be enumerated as candidate actions — apply() validates the submitted
        // selection structurally instead (see handle_resolution_choice). The
        // engine owns this classification via accepts_freeform_card_selection.
        // Combat-damage assignment (CR 510.1c/d, CR 702.19b) likewise has too many
        // legal divisions to enumerate (candidates.rs lists only the greedy
        // trample-through split) — apply() validates conservation and the
        // lethal-before-excess precondition, so the gate is bypassed here too.
        let skip_legality = action.is_mana_ability()
            || matches!(action, GameAction::SetAutoPass { .. })
            || (matches!(action, GameAction::SelectCards { .. })
                && session.state.waiting_for.accepts_freeform_card_selection())
            || (matches!(action, GameAction::ChooseCounterMoveDistribution { .. })
                && session
                    .state
                    .waiting_for
                    .accepts_freeform_counter_move_distribution())
            || (matches!(action, GameAction::AssignCombatDamage { .. })
                && session
                    .state
                    .waiting_for
                    .accepts_freeform_combat_damage_assignment())
            // CR 510.1d + CR 702.22k: a banded blocker's free damage division
            // has too many legal splits to enumerate as candidates, so the
            // server bypasses its legality gate and the engine handler
            // (handle_assign_blocker_damage) validates the submission.
            || (matches!(action, GameAction::AssignBlockerDamage { .. })
                && session
                    .state
                    .waiting_for
                    .accepts_freeform_blocker_damage_assignment());
        if !skip_legality {
            let (legal_actions, _, _) = engine_legal_actions_full(&session.state);
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

        let (new_legal_actions, spell_costs, by_object) = engine_legal_actions_full(&session.state);
        let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);

        Ok((
            session.state.clone(),
            result.events,
            new_legal_actions,
            result.log_entries,
            auto_pass,
            spell_costs,
            by_object,
        ))
    }

    /// Mark a player as disconnected.
    pub fn handle_disconnect(&mut self, game_code: &str, player: PlayerId) {
        if let Some(session) = self.sessions.get_mut(game_code) {
            session.connected[player.0 as usize] = false;
            let default_grace = self.reconnect.grace_period;
            self.reconnect
                .record_disconnect(game_code, player, default_grace);
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
            .filter(|s| s.first_open_seat().is_some())
            .map(|s| s.game_code.clone())
            .collect()
    }

    /// Look up game_code by player_token.
    pub fn game_for_token(&self, token: &str) -> Option<&str> {
        self.token_to_game.get(token).map(|s| s.as_str())
    }

    /// Drop the given tokens from the token-to-game index.
    ///
    /// A seat mutation (kick, replace-with-AI, remove) invalidates the affected
    /// seats' player tokens. `GameSession::apply_seat_delta` clears the per-seat
    /// token arrays, but it cannot reach this index (which lives on the
    /// manager), so without this the invalidated tokens keep resolving to the
    /// game via [`game_for_token`] — a stale mapping that lets a kicked client's
    /// token still point at a game it is no longer part of, and that is never
    /// reclaimed. Callers pass `SeatDelta::invalidated_tokens` here right after
    /// applying the delta. Empty strings (vacant seats) are skipped, never a
    /// real index key. Mirrors the index cleanup done when a whole game is
    /// removed from the manager.
    pub fn unindex_tokens(&mut self, tokens: &[String]) {
        for token in tokens {
            self.unindex_token(token);
        }
    }

    /// Remove a game session entirely, cleaning up the token-to-game index.
    /// Returns the removed session if it existed.
    pub fn remove_game(&mut self, game_code: &str) -> Option<GameSession> {
        let session = self.sessions.remove(game_code)?;
        for token in &session.player_tokens {
            self.unindex_token(token);
        }
        Some(session)
    }

    fn unindex_token(&mut self, token: &str) {
        if !token.is_empty() {
            self.token_to_game.remove(token);
        }
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
    use seat_reducer::types::SeatMutation;

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
                    cleave_variant: None,
                    color_override: None,
                    color_identity: vec![],
                    scryfall_oracle_id: None,
                    modal: None,
                    additional_cost: None,
                    strive_cost: None,
                    casting_restrictions: vec![],
                    casting_options: vec![],
                    solve_condition: None,
                    parse_warnings: vec![],
                    brawl_commander: false,
                    is_commander: false,
                    is_oathbreaker: false,
                    deck_copy_limit: None,
                    metadata: Default::default(),
                    rarities: Default::default(),
                    attraction_lights: vec![],
                },
                count: 10,
            }],
            sideboard: Vec::new(),
            commander: Vec::new(),
            ..Default::default()
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
    fn unindex_tokens_removes_only_named_tokens() {
        let mut mgr = SessionManager::new();
        let (code, token1) = mgr.create_game(make_deck());
        let (token2, _) = mgr.join_game(&code, make_deck()).unwrap();

        assert_eq!(mgr.game_for_token(&token1), Some(code.as_str()));
        assert_eq!(mgr.game_for_token(&token2), Some(code.as_str()));

        // Simulate a seat mutation invalidating player 2's token (kick / replace
        // / remove). An empty entry (vacant seat) in the list is ignored.
        mgr.unindex_tokens(&[token2.clone(), String::new()]);

        // The invalidated token no longer resolves; the surviving seat is intact.
        assert_eq!(mgr.game_for_token(&token2), None);
        assert_eq!(mgr.game_for_token(&token1), Some(code.as_str()));
    }

    #[test]
    fn seat_mutation_unindexes_invalidated_human_token() {
        struct UnusedResolver;

        impl seat_reducer::types::DeckResolver for UnusedResolver {
            fn resolve(
                &self,
                _choice: &DeckChoice,
            ) -> Result<engine::game::deck_loading::PlayerDeckList, String> {
                panic!("human seat removal must not resolve a deck")
            }
        }

        let mut mgr = SessionManager::new();
        let (code, token1) = mgr.create_game(make_deck());
        let (token2, _) = mgr.join_game(&code, make_deck()).unwrap();
        let db = engine::database::CardDatabase::default();
        let resolver = UnusedResolver;
        let ctx = seat_reducer::types::ReducerCtx {
            platform: Platform::Native,
            deck_resolver: &resolver,
        };

        let mut seat_state = mgr.sessions.get(&code).unwrap().seat_state();
        let delta = seat_reducer::apply(
            &mut seat_state,
            SeatMutation::SetKind {
                seat_index: 1,
                kind: SeatKind::WaitingHuman,
            },
            &ctx,
        )
        .unwrap();
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .apply_seat_delta(seat_state, &delta, &db);
        mgr.unindex_tokens(&delta.invalidated_tokens);

        assert_eq!(delta.invalidated_tokens, vec![token2.clone()]);
        assert_eq!(mgr.game_for_token(&token2), None);
        assert_eq!(mgr.game_for_token(&token1), Some(code.as_str()));
    }

    #[test]
    fn remove_game_clears_token_index() {
        let mut mgr = SessionManager::new();
        let (code, token1) = mgr.create_game(make_deck());
        let (token2, _state) = mgr.join_game(&code, make_deck()).unwrap();

        // While the game exists, both players' tokens resolve to it.
        assert_eq!(mgr.game_for_token(&token1), Some(code.as_str()));
        assert_eq!(mgr.game_for_token(&token2), Some(code.as_str()));

        let removed = mgr.remove_game(&code);
        assert!(removed.is_some());

        // After removal, the session and both token-index entries are gone —
        // no orphaned mappings linger in token_to_game.
        assert!(!mgr.sessions.contains_key(&code));
        assert_eq!(mgr.game_for_token(&token1), None);
        assert_eq!(mgr.game_for_token(&token2), None);
    }

    #[test]
    fn remove_nonexistent_game_returns_none() {
        let mut mgr = SessionManager::new();
        assert!(mgr.remove_game("NOPE00").is_none());
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
            // CR 103.5: simultaneous mulligan — pick the first pending player
            // as the "acting" target for the wrong-token test.
            WaitingFor::MulliganDecision { pending, .. } => pending[0].player,
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

    // Helper: create a two-player game and advance past mulligans so both players
    // have Priority-phase waiting state. Returns (mgr, code, token0, token1).
    fn setup_two_player_game() -> (SessionManager, String, String, String) {
        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();
        // Advance through mulligan decisions until both players have kept hands.
        // We loop at most 20 times to avoid infinite loops in unexpected states.
        for _ in 0..20 {
            let session = mgr.sessions.get(&code).unwrap();
            match &session.state.waiting_for.clone() {
                // CR 103.5: simultaneous mulligan — submit a Keep for each
                // pending player using their own token.
                WaitingFor::MulliganDecision { pending, .. } => {
                    for entry in pending {
                        let tok = if entry.player == PlayerId(0) {
                            token0.clone()
                        } else {
                            token1.clone()
                        };
                        let _ = mgr.handle_action(
                            &code,
                            &tok,
                            GameAction::MulliganDecision {
                                choice: engine::types::actions::MulliganChoice::Keep,
                            },
                        );
                    }
                }
                WaitingFor::Priority { .. } => break,
                _ => break,
            }
        }
        (mgr, code, token0, token1)
    }

    /// `ReorderHand` succeeds even when the sender is not the priority holder.
    /// The hand is reordered to the requested permutation.
    #[test]
    fn reorder_hand_succeeds_while_opponent_has_priority() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();

        // Determine which player has priority; inject two ObjectIds into the
        // *other* player's hand so we can test off-priority reordering.
        let (priority_player, off_priority_token, off_priority_id) = {
            let session = mgr.sessions.get(&code).unwrap();
            match &session.state.waiting_for {
                WaitingFor::Priority { player } if *player == PlayerId(0) => {
                    (PlayerId(0), token1.clone(), 1usize)
                }
                _ => (PlayerId(1), token0.clone(), 0usize),
            }
        };
        let _ = priority_player; // acknowledged

        // Inject two synthetic ObjectIds directly into the off-priority player's hand.
        let id_a = ObjectId(900);
        let id_b = ObjectId(901);
        {
            let session = mgr.sessions.get_mut(&code).unwrap();
            session.state.players[off_priority_id].hand = engine::im::vector![id_a, id_b];
        }

        // Request reverse order [b, a].
        let result = mgr.handle_action(
            &code,
            &off_priority_token,
            GameAction::ReorderHand {
                order: vec![id_b, id_a],
            },
        );
        assert!(
            result.is_ok(),
            "ReorderHand should succeed: {:?}",
            result.err()
        );

        let session = mgr.sessions.get(&code).unwrap();
        let hand: Vec<ObjectId> = session.state.players[off_priority_id]
            .hand
            .iter()
            .copied()
            .collect();
        assert_eq!(hand, vec![id_b, id_a]);
    }

    /// `ReorderHand` with a non-permutation (wrong element) is rejected by the
    /// engine-owned validation path and leaves the hand unchanged.
    #[test]
    fn reorder_hand_invalid_permutation_is_rejected() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();

        let (off_priority_token, off_priority_id) = {
            let session = mgr.sessions.get(&code).unwrap();
            match &session.state.waiting_for {
                WaitingFor::Priority { player } if *player == PlayerId(0) => {
                    (token1.clone(), 1usize)
                }
                _ => (token0.clone(), 0usize),
            }
        };

        let id_a = ObjectId(902);
        let id_b = ObjectId(903);
        let id_bogus = ObjectId(999);
        {
            let session = mgr.sessions.get_mut(&code).unwrap();
            session.state.players[off_priority_id].hand = engine::im::vector![id_a, id_b];
        }

        // Send [a, bogus] — not a permutation of [a, b].
        let result = mgr.handle_action(
            &code,
            &off_priority_token,
            GameAction::ReorderHand {
                order: vec![id_a, id_bogus],
            },
        );
        // Should return an error from the engine-owned permutation validator.
        assert!(result.is_err(), "Invalid ReorderHand should be rejected");
        let session = mgr.sessions.get(&code).unwrap();
        let hand: Vec<ObjectId> = session.state.players[off_priority_id]
            .hand
            .iter()
            .copied()
            .collect();
        assert_eq!(
            hand,
            vec![id_a, id_b],
            "Hand should be unchanged after invalid reorder"
        );
    }

    // ── Sandbox capability tests ─────────────────────────────────────────

    fn create_sandbox_game(mgr: &mut SessionManager) -> (String, String) {
        let sandbox_config = FormatConfig::commander().with_sandbox();
        mgr.create_game_n_players(
            make_deck(),
            "Host".to_string(),
            None,
            2,
            MatchConfig::default(),
            Some(sandbox_config),
        )
    }

    #[test]
    fn with_sandbox_sets_flag_and_is_idempotent() {
        let base = FormatConfig::standard();
        assert!(!base.allow_debug_actions);
        let sb = base.clone().with_sandbox();
        assert!(sb.allow_debug_actions);
        // Idempotent — applying twice yields the same config.
        let sb2 = sb.clone().with_sandbox();
        assert_eq!(sb, sb2);
        // Only the capability flag differs.
        let restored = FormatConfig {
            allow_debug_actions: false,
            ..sb
        };
        assert_eq!(restored, base);
    }

    #[test]
    fn sandbox_game_seeds_all_seats_in_debug_permitted() {
        // Sandbox is a shared playground: every seat is permitted by default
        // so any participant can drive debug tools without an admin gate.
        let mut mgr = SessionManager::new();
        let (code, _token) = create_sandbox_game(&mut mgr);
        let session = mgr.sessions.get(&code).unwrap();
        assert!(session.state.format_config.allow_debug_actions);
        assert!(session.state.debug_mode);
        assert!(session.state.debug_permitted.contains(&PlayerId(0)));
        assert!(session.state.debug_permitted.contains(&PlayerId(1)));
        assert_eq!(session.state.debug_permitted.len(), 2);
    }

    #[test]
    fn non_sandbox_game_has_empty_debug_permitted() {
        let mut mgr = SessionManager::new();
        let (code, _token) = mgr.create_game(make_deck());
        let session = mgr.sessions.get(&code).unwrap();
        assert!(!session.state.format_config.allow_debug_actions);
        assert!(!session.state.debug_mode);
        assert!(session.state.debug_permitted.is_empty());
    }

    #[test]
    fn non_sandbox_rejects_debug_action() {
        let mut mgr = SessionManager::new();
        let (code, token) = mgr.create_game(make_deck());
        let result = mgr.handle_action(
            &code,
            &token,
            GameAction::Debug(engine::types::actions::DebugAction::ShuffleLibrary {
                player_id: PlayerId(0),
            }),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("not permitted") || err.contains("permission"),
            "{err}"
        );
    }

    #[test]
    fn sandbox_accepts_debug_action_from_host() {
        let mut mgr = SessionManager::new();
        let (code, token) = create_sandbox_game(&mut mgr);
        // We can't fully start the game without a database, but ShuffleLibrary
        // only validates the player exists, so it works against a pregame
        // state too as long as the player is present. Confirm the gate at
        // least accepts the action — engine validation may still reject if
        // pregame state lacks the player, but the *server gate* must not be
        // the rejecter.
        let result = mgr.handle_action(
            &code,
            &token,
            GameAction::Debug(engine::types::actions::DebugAction::ShuffleLibrary {
                player_id: PlayerId(0),
            }),
        );
        // The gate must accept; if engine rejects for other reasons that's
        // beside the point of this test. We assert the gate-specific error
        // text is absent.
        if let Err(e) = &result {
            assert!(
                !e.contains("not permitted") && !e.contains("Sandbox"),
                "Gate rejected the host in a sandbox game: {e}"
            );
        }
        // When the action does succeed, an audit event must be emitted.
        if let Ok(action_result) = result {
            let used = action_result.1.iter().any(|e| {
                matches!(
                    e,
                    engine::types::events::GameEvent::DebugActionUsed { description, .. }
                        if !description.is_empty()
                )
            });
            assert!(used, "Sandbox debug action must emit DebugActionUsed event");
        }
    }

    #[test]
    fn sandbox_rejects_debug_from_revoked_seat() {
        // Default is "all seats permitted" — a guest is only rejected after
        // an explicit revoke. This exercises the revoke escape hatch.
        let mut mgr = SessionManager::new();
        let (code, host_token) = create_sandbox_game(&mut mgr);
        let (guest_token, _state) = mgr
            .join_game_with_name(&code, make_deck(), "Guest".to_string())
            .expect("guest joins");

        // Host revokes the guest's default permission.
        let revoke = mgr.handle_action(
            &code,
            &host_token,
            GameAction::RevokeDebugPermission {
                player_id: PlayerId(1),
            },
        );
        assert!(revoke.is_ok(), "revoke must succeed: {:?}", revoke.err());

        let result = mgr.handle_action(
            &code,
            &guest_token,
            GameAction::Debug(engine::types::actions::DebugAction::ShuffleLibrary {
                player_id: PlayerId(1),
            }),
        );
        assert!(result.is_err());
    }

    #[test]
    fn host_can_grant_debug_to_guest() {
        let mut mgr = SessionManager::new();
        let (code, host_token) = create_sandbox_game(&mut mgr);
        let (guest_token, _state) = mgr
            .join_game_with_name(&code, make_deck(), "Guest".to_string())
            .expect("guest joins");

        // Host grants debug permission to seat 1.
        let result = mgr.handle_action(
            &code,
            &host_token,
            GameAction::GrantDebugPermission {
                player_id: PlayerId(1),
            },
        );
        assert!(result.is_ok(), "grant must succeed: {:?}", result.err());
        let session = mgr.sessions.get(&code).unwrap();
        assert!(session.state.debug_permitted.contains(&PlayerId(1)));

        // Guest can now submit a debug action.
        let result = mgr.handle_action(
            &code,
            &guest_token,
            GameAction::Debug(engine::types::actions::DebugAction::ShuffleLibrary {
                player_id: PlayerId(1),
            }),
        );
        if let Err(e) = &result {
            assert!(
                !e.contains("not permitted") && !e.contains("Sandbox"),
                "Gate rejected the granted guest: {e}"
            );
        }

        // Host revokes — guest is no longer permitted.
        let _ = mgr.handle_action(
            &code,
            &host_token,
            GameAction::RevokeDebugPermission {
                player_id: PlayerId(1),
            },
        );
        let session = mgr.sessions.get(&code).unwrap();
        assert!(!session.state.debug_permitted.contains(&PlayerId(1)));
        assert!(session.state.debug_permitted.contains(&PlayerId(0)));
    }

    #[test]
    fn non_host_cannot_grant_debug() {
        let mut mgr = SessionManager::new();
        let (code, _host_token) = create_sandbox_game(&mut mgr);
        let (guest_token, _state) = mgr
            .join_game_with_name(&code, make_deck(), "Guest".to_string())
            .expect("guest joins");

        let result = mgr.handle_action(
            &code,
            &guest_token,
            GameAction::GrantDebugPermission {
                player_id: PlayerId(1),
            },
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("host"), "{err}");
    }

    #[test]
    fn host_cannot_self_revoke() {
        let mut mgr = SessionManager::new();
        let (code, host_token) = create_sandbox_game(&mut mgr);
        let result = mgr.handle_action(
            &code,
            &host_token,
            GameAction::RevokeDebugPermission {
                player_id: PlayerId(0),
            },
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("own"), "{err}");
    }

    #[test]
    fn grant_outside_sandbox_is_rejected() {
        let mut mgr = SessionManager::new();
        let (code, token) = mgr.create_game(make_deck());
        let result = mgr.handle_action(
            &code,
            &token,
            GameAction::GrantDebugPermission {
                player_id: PlayerId(1),
            },
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Sandbox"), "{err}");
    }

    #[test]
    fn start_game_rejects_non_cedh_deck_when_any_ai_seat_is_cedh() {
        use engine::database::legality::CedhBracketError;
        use engine::game::bracket_estimate::CommanderBracketTier;

        // Build an empty CardDatabase (no real card data needed — the cEDH
        // bracket gate fires before any deck loading).
        let db = engine::database::CardDatabase::default();

        // Construct a two-seat session manually: host (seat 0) + AI (seat 1).
        let pc = 2usize;
        let state = engine::types::game_state::GameState::new(
            engine::types::format::FormatConfig::commander(),
            pc as u8,
            0,
        );
        let ai_pid = PlayerId(1);
        let cedh_config = phase_ai::config::create_config_for_players(
            AiDifficulty::CEDH,
            Platform::Native,
            pc as u8,
        );

        let mut session = GameSession {
            game_code: "TEST01".to_string(),
            state,
            player_tokens: vec!["host_token".to_string(), String::new()],
            connected: vec![true, true],
            // Both decks present but with non-cEDH bracket tier (Core is the default).
            decks: vec![
                Some(PlayerDeckPayload {
                    bracket_tier: CommanderBracketTier::Core,
                    ..Default::default()
                }),
                Some(PlayerDeckPayload {
                    bracket_tier: CommanderBracketTier::Core,
                    ..Default::default()
                }),
            ],
            display_names: vec!["Host".to_string(), "AI (CEDH)".to_string()],
            reservations: HashMap::new(),
            timer_seconds: None,
            player_count: pc as u8,
            ai_seats: [ai_pid].into_iter().collect(),
            ai_configs: [(ai_pid, cedh_config)].into_iter().collect(),
            lobby_meta: None,
            game_started: false,
            start_when_full: true,
            ranked: false,
            start_events: Vec::new(),
        };

        let game_started_before = session.game_started;
        let result = session.start_game(&db);

        // The gate must reject with DeckNotCedh.
        assert!(
            matches!(result, Err(CedhBracketError::DeckNotCedh { .. })),
            "expected DeckNotCedh, got: {:?}",
            result
        );
        // No session state should have been mutated — game_started stays false.
        assert_eq!(session.game_started, game_started_before);
        assert!(!session.game_started);
    }

    /// CR 701.22a / CR 701.25a: a reordered (and partial-2+) scry keep-on-top
    /// selection is a legal freeform selection that `select_cards_variants` does
    /// not enumerate. Before the freeform-skip change it was rejected as
    /// "Illegal action"; now the server must bypass the candidate gate for these
    /// states and let `apply()` validate the selection structurally.
    #[test]
    fn reordered_scry_selection_is_accepted_not_rejected_as_illegal() {
        use engine::game::zones::create_object;
        use engine::types::identifiers::{CardId, ObjectId};
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        // Make the scry the responsibility of the NON-active player so that
        // `authorized_submitter_for_player` is the identity (no turn-decision
        // re-routing) and authorization is unambiguous.
        let scry_player = PlayerId(if session.state.active_player == PlayerId(0) {
            1
        } else {
            0
        });
        let token = if scry_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };

        // Give the scrying player a known library and put them in a ScryChoice
        // over its top three cards.
        let mut top_three = Vec::new();
        for i in 0..3 {
            let id = create_object(
                &mut session.state,
                CardId(1000 + i),
                scry_player,
                format!("Scry Card {i}"),
                Zone::Library,
            );
            top_three.push(id);
        }
        let (a, b, c): (ObjectId, ObjectId, ObjectId) = (top_three[0], top_three[1], top_three[2]);
        session.state.waiting_for = WaitingFor::ScryChoice {
            player: scry_player,
            cards: top_three.clone(),
        };
        // ScryChoice carries no PendingContinuation here; the resolution handler
        // tolerates a None continuation (finishes back to priority), so the
        // action's acceptance through the gate is what this test asserts.

        // Reordered, partial-2 keep: [c, a] (drop b to the bottom). This is NOT
        // an enumerated candidate, so it would be rejected by the legality gate.
        let token = token.to_string();
        let result =
            mgr.handle_action(&code, &token, GameAction::SelectCards { cards: vec![c, a] });
        assert!(
            result.is_ok(),
            "reordered scry selection should be accepted, got: {result:?}"
        );

        // The selection was applied: c then a rest on top.
        let session = mgr.sessions.get(&code).unwrap();
        let player_idx = scry_player.0 as usize;
        let library: Vec<ObjectId> = session.state.players[player_idx]
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(&library[..2], &[c, a]);
        assert!(!library[2..].contains(&b) || library.last() == Some(&b));
    }

    /// A Dig reorder-mode selection (keep all, library-destined, reordered) is a
    /// non-canonical permutation that the candidate enumerator does not list, so
    /// pre-fix the server rejected it as "Illegal action". The server must now
    /// bypass the gate for DigChoice and let `apply()` validate it structurally.
    #[test]
    fn reordered_dig_selection_is_accepted_not_rejected_as_illegal() {
        use engine::game::zones::create_object;
        use engine::types::identifiers::{CardId, ObjectId};
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        let dig_player = PlayerId(if session.state.active_player == PlayerId(0) {
            1
        } else {
            0
        });
        let token = if dig_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };

        let mut top_three = Vec::new();
        for i in 0..3 {
            let id = create_object(
                &mut session.state,
                CardId(2000 + i),
                dig_player,
                format!("Dig Card {i}"),
                Zone::Library,
            );
            top_three.push(id);
        }
        let (a, b, c): (ObjectId, ObjectId, ObjectId) = (top_three[0], top_three[1], top_three[2]);
        // Reorder mode: keep all three, library-destined — order matters.
        session.state.waiting_for = WaitingFor::DigChoice {
            player: dig_player,
            library_owner: dig_player,
            cards: top_three.clone(),
            keep_count: 3,
            up_to: false,
            selectable_cards: top_three.clone(),
            kept_destination: Some(Zone::Library),
            rest_destination: Some(Zone::Library),
            source_id: None,
        };

        // Non-canonical permutation [c, a, b] — not an enumerated candidate.
        let token = token.to_string();
        let result = mgr.handle_action(
            &code,
            &token,
            GameAction::SelectCards {
                cards: vec![c, a, b],
            },
        );
        assert!(
            result.is_ok(),
            "reordered dig selection should be accepted, got: {result:?}"
        );

        let session = mgr.sessions.get(&code).unwrap();
        let library: Vec<ObjectId> = session.state.players[dig_player.0 as usize]
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(&library[..3], &[c, a, b]);
    }

    /// CR 702.19b: a single-blocker trample attacker's controller may keep all
    /// damage on the blocker (trample_damage:0) instead of trampling the excess
    /// through. `candidates.rs` enumerates only the greedy trample-through split,
    /// so before the freeform-skip change the multiplayer gate rejected the
    /// keep-on-blocker division as "Illegal action". The server must now bypass
    /// the candidate gate for `AssignCombatDamage` and let `apply()` validate the
    /// submitted division (CR 510.1c/d), accepting every legal one. An illegal
    /// division (wrong total) must still be rejected — by `apply()`, not the gate.
    #[test]
    fn keep_on_blocker_combat_damage_is_accepted_and_wrong_total_rejected() {
        use engine::game::combat::{AttackerInfo, CombatState};
        use engine::game::zones::create_object;
        use engine::types::game_state::{CombatDamageAssignmentMode, DamageSlot};
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        // The attacker's controller assigns combat damage. Make that the
        // active player; route the action through whichever token owns them.
        let assigning_player = session.state.active_player;
        let defending_player = PlayerId(if assigning_player == PlayerId(0) {
            1
        } else {
            0
        });
        let token = if assigning_player == PlayerId(0) {
            token0.clone()
        } else {
            token1.clone()
        };

        // A 5/5 trample attacker blocked by a single 2/2.
        let attacker = create_object(
            &mut session.state,
            CardId(3000),
            assigning_player,
            "Fatty".to_string(),
            Zone::Battlefield,
        );
        let blocker = create_object(
            &mut session.state,
            CardId(3001),
            defending_player,
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, defending_player)],
            ..Default::default()
        };
        combat.attackers[0].blocked = true;
        combat
            .blocker_to_attacker
            .entry(blocker)
            .or_default()
            .push(attacker);
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        session.state.combat = Some(combat);

        // CR 702.19b: single-blocker trample-with-excess interactive prompt.
        session.state.waiting_for = WaitingFor::AssignCombatDamage {
            player: assigning_player,
            attacker_id: attacker,
            total_damage: 5,
            blockers: vec![DamageSlot {
                blocker_id: blocker,
                lethal_minimum: 2,
            }],
            assignment_modes: vec![CombatDamageAssignmentMode::Normal],
            trample: Some(engine::game::combat::TrampleKind::Standard),
            defending_player,
            attack_target: engine::game::combat::AttackTarget::Player(defending_player),
            pw_loyalty: None,
            pw_controller: None,
        };

        // Illegal division first: wrong total (4 != 5). The gate is bypassed, so
        // this reaches apply() and is rejected there as an engine error — proving
        // we did NOT weaken validation, only skipped candidate enumeration.
        let illegal = mgr.handle_action(
            &code,
            &token,
            GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 4)],
                trample_damage: 0,
                controller_damage: 0,
            },
        );
        match illegal {
            Err(e) => assert!(
                e.starts_with("Engine error:"),
                "wrong-total division must be rejected by apply(), not the gate, got: {e}"
            ),
            Ok(_) => panic!("wrong-total combat damage division must be rejected"),
        }

        // Legal-but-non-enumerated division: keep all 5 on the blocker, trample
        // nothing through (CR 702.19b). Pre-fix this was rejected as illegal.
        let legal = mgr.handle_action(
            &code,
            &token,
            GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 5)],
                trample_damage: 0,
                controller_damage: 0,
            },
        );
        assert!(
            legal.is_ok(),
            "keep-on-blocker combat damage division (CR 702.19b) should be accepted, got: {legal:?}"
        );

        // The defending player took no trample damage — proof the controller's
        // declined-excess division resolved as submitted (life unchanged at 20).
        let session = mgr.sessions.get(&code).unwrap();
        assert_eq!(session.state.players[defending_player.0 as usize].life, 20);
    }
}
