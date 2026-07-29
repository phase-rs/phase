use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine::ai_support::{auto_pass_recommended, legal_actions_full as engine_legal_actions_full};
use engine::database::legality::{validate_cedh_bracket, CedhBracketError};
use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckPayload, PlayerDeckPayload};
use engine::game::engine::{apply, start_game};
use engine::game::finalize_public_state;
use engine::game::interaction::bind_interaction_authority;
use engine::game::preview::preview_auto_payment_sources;
use engine::game::{load_and_hydrate_decks, rehydrate_game_from_card_db};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, PersistedGameState};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::InteractionSessionId;
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::player::PlayerId;
use phase_ai::config::{AiConfig, AiDifficulty, Platform};
use phase_ai::session::AiSession;
use rand::{Rng, SeedableRng};
use seat_reducer::types::{seat_team_info, DeckChoice, SeatDelta, SeatKind, SeatState};
use tracing::{debug, info, warn};

use crate::filter::filter_state_for_player;
use crate::persist::{PersistedLobbyMeta, PersistedSession};
use crate::protocol::PlayerSlotInfo;
use crate::reconnect::ReconnectManager;
use crate::takeback::PendingTakeback;

/// Bind the engine's interaction authority to a freshly created or restored state.
///
/// Every server-side `GameState` must pass through here. `GameState::new` leaves
/// `interaction_session_id` as `None`, and while it is unset
/// `derive_viewer_interaction` short-circuits to `AuthorityUnbound` with zero
/// opportunities — so every interaction-driven client surface silently goes dark.
/// `ensure_interaction_authority` cannot repair this: it only *maintains* slots for
/// an already-bound session, and clears them when the session is absent.
///
/// The game code is a sound session id: non-empty and far under the 128-byte limit.
/// Uniqueness only has to hold *within* a state — the id namespaces that state's
/// interaction ids and detects stale ones — so `generate_game_code`'s lack of a
/// collision check (6 random chars) is not a concern here. Nor is guessability:
/// `slot_for_submission` authorizes against the authenticated actor, never this id.
///
/// Always re-bind on restore rather than trusting an id carried in a persisted blob,
/// matching how this module re-stamps `hosting` and revokes unentitled debug capability.
fn bind_interaction_session(state: &mut GameState, game_code: &str) {
    if let Err(error) =
        bind_interaction_authority(state, InteractionSessionId(game_code.to_string()))
    {
        // Reachable only on decimal-serial exhaustion, so failing the game would be
        // disproportionate — but degrading silently is the very defect this fixes.
        warn!(
            game = %game_code,
            code = ?error.code,
            "failed to bind interaction authority; interaction surfaces unavailable for this session"
        );
    }
}

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

/// One completed authoritative transition paired with the server revision
/// allocated while the session lock was held. The transport must keep this
/// pairing intact when it fans a snapshot out to multiple viewers.
pub type RevisionedActionResult = (u64, ActionResult);

/// Broadcast-ready fields for a state snapshot taken outside the normal
/// `handle_action` flow (e.g. an approved takeback rollback): the raw state,
/// legal actions, auto-pass flag, spell costs, and per-object action grouping.
pub type BroadcastSnapshot = (
    GameState,
    Vec<GameAction>,
    bool,
    HashMap<ObjectId, ManaCost>,
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
/// so the simultaneous-decision states (MulliganDecision,
/// OpeningHandBottomCards)
/// route legal actions to every pending player, not just the first.
pub fn is_acting(state: &GameState, player: PlayerId) -> bool {
    engine::game::turn_control::is_authorized_submitter(state, player)
}

/// Deployment shape of this `phase-server` instance. Selected once at startup
/// from `--single-user`; never per-session, because a session cannot prove it
/// is unobserved — `SpectatorJoin` admits any started game by code, with no
/// `public` / password / `ai_seats` check. Deriving capability from the seat
/// mix would therefore open sandbox on a *shared* server to any client that
/// created an all-AI game.
///
/// Shape precedent: `phase_server::persistence::SessionRetention`, which is
/// likewise a two-variant `Copy` enum selected from the same CLI flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostingMode {
    /// A shared instance: other humans may join or spectate any session.
    Shared,
    /// The desktop shell's sidecar. `SingleUser` asserts that no other client
    /// can reach this instance at all, and that assertion rests on the spawn
    /// arguments — `--bind 127.0.0.1` plus `--allowed-origin`
    /// (`client/src-tauri/src/native_engine.rs`), NOT on the seat mix. If the
    /// sidecar is ever bound beyond loopback, this variant stops being true
    /// and the capability it grants must be re-derived.
    SingleUser,
}

pub struct GameSession {
    pub game_code: String,
    /// Monotonic server-authored revision of the current authoritative state.
    /// Read-only snapshots reuse this value; mutators advance it before their
    /// per-viewer views are captured for transport.
    pub state_revision: u64,
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
    /// Deployment shape of the process hosting this session, copied from the
    /// owning `SessionManager` at creation and **re-stamped, never
    /// deserialized**, at `SessionManager::restore_session`. Deliberately
    /// absent from `PersistedSession`.
    ///
    /// The stamp blocks re-*derivation* only: it stops a restored blob from
    /// claiming this process is a sidecar when it is not. The capability the
    /// derivation produces (`GameState::debug_mode` / `debug_permitted`) is
    /// a plain `#[serde(default)]` pair that rides inside the blob, so the
    /// stamp alone does not keep a sidecar's capability out of a shared
    /// server — `GameSession::revoke_unentitled_debug_capability`, called
    /// immediately after the stamp, is what does that.
    ///
    /// **Read fence.** This field is read by exactly one function,
    /// `seed_debug_capability`, which is reachable only via
    /// `rebuild_pregame_state`, which is called only from `start_game` and
    /// from `apply_seat_delta`. Neither runs during the lobby re-registration
    /// window in which a restored session still holds the `from_persisted`
    /// placeholder. **Any new read must either sit behind
    /// `rebuild_pregame_state` or be added after the restore stamp.**
    pub hosting: HostingMode,
    /// Number of human player seats in this game.
    pub player_count: u8,
    /// Seats controlled by AI (not occupied by a human player).
    pub ai_seats: HashSet<PlayerId>,
    /// Per-AI-player configuration (difficulty, search params, etc.).
    pub ai_configs: HashMap<PlayerId, AiConfig>,
    /// Runtime-only per-game AI cache. Rebuilt from `state.deck_pools` on
    /// start/restore, not persisted.
    pub ai_session: Option<Arc<AiSession>>,
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
    /// A "request takeback" awaiting unanimous human approval, if any. See
    /// `crate::takeback` for the GH #1507 multiplayer-safe undo flow.
    pub pending_takeback: Option<PendingTakeback>,
    /// Rolling buffer of (actor, state) pairs — the authoritative state
    /// immediately preceding each state-mutating action, tagged with which
    /// player took that action. Tagged by actor so a takeback request can
    /// find the requester's *own* last action even when other players have
    /// acted since (see `crate::takeback::GameSession::request_takeback`).
    pub takeback_history: VecDeque<(PlayerId, GameState)>,
}

impl GameSession {
    /// Allocates the revision for one completed authoritative state transition.
    pub fn advance_state_revision(&mut self) -> u64 {
        self.state_revision = self.state_revision.saturating_add(1);
        self.state_revision
    }

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
                    team_info: seat_team_info(&self.state.format_config, pid.0),
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
        // CR 732.2a: re-read the immutable match config (incl. the combo-detector
        // opt-in) so a Bo3 rematch keeps a consistent detector across games. Bo3 is
        // 2-player-only; `loop_detection` is player-count-agnostic.
        let match_config = if player_count == 2 {
            match_config
        } else {
            MatchConfig {
                loop_detection: match_config.loop_detection,
                ..MatchConfig::default()
            }
        };
        self.state.set_match_config(match_config);
        self.seed_debug_capability();
        // `GameState::new` above reset the authority to `None`, exactly as it reset
        // the debug capability that `seed_debug_capability` restores.
        bind_interaction_session(&mut self.state, &self.game_code);
    }

    /// Seeds `debug_mode` / `debug_permitted` for the state just built by
    /// `rebuild_pregame_state`.
    ///
    /// That function is the only SESSION-LAYER seeding site whose result
    /// survives `start_game`: it replaces `self.state` wholesale, discarding
    /// anything seeded during construction (`create_game_n_players` seeds
    /// before `start_game` runs, so its seeding is unobservable to a client —
    /// `create_game_with_ai` starts the game before returning the game code).
    ///
    /// It is *not* the only seeding site in the codebase. The engine's
    /// between-games rebuild (`engine::game::match_flow`'s
    /// `restart_between_games_with_starting_player`) builds a fresh
    /// `GameState` that never touches `GameSession`, so it carries these two
    /// fields forward itself. A `session.rs` authority structurally cannot
    /// reach it.
    ///
    /// Note `GameState::debug_mode`'s own doc comment still reads "Always
    /// false for multiplayer games" — stale as of this commit, since the
    /// client classifies `native-ai` as multiplayer. A rider is filed against
    /// `crates/engine/src/types/game_state.rs` to correct it. That file is
    /// clean in the working tree; it is out of this change's scope because it
    /// belongs to the sibling Item 1 workstream, not because another agent
    /// holds it. (The earlier wording here claimed the latter and was wrong —
    /// an unverified ownership claim is the same failure mode as the stale
    /// comment it defers.)
    ///
    /// Both branches below read `self.player_count` as the single seat-count
    /// authority: `human_seats()` derives from it, and `apply_seat_delta`
    /// assigns it before rebuilding, so a parameter would only reintroduce
    /// the chance of the two disagreeing.
    fn seed_debug_capability(&mut self) {
        if self.state.format_config.allow_debug_actions {
            // Sandbox is a shared playground, not an admin console: every seat
            // is permitted by default. Explicit revocations from a previous
            // game are dropped, since a rebuilt pregame state is a fresh debug
            // context. (The old comment here claimed this preserved seeding
            // "through rematch" — it does not: a Bo3 rematch rebuilds in the
            // engine, never through this function.)
            self.state.debug_mode = true;
            for i in 0..self.player_count {
                self.state.debug_permitted.insert(PlayerId(i));
            }
        } else if self.hosting == HostingMode::SingleUser {
            // Desktop-solo parity with the browser engine, which enables
            // `debug_mode` unconditionally for single-player. The sandbox
            // *format* flag stays false, so own-library name exposure
            // (`engine::game::visibility`) and the host grant/revoke flow
            // (rejected above with "Sandbox mode is not enabled") remain
            // closed — this grants the debug panel, not a shared sandbox.
            //
            // Human seats only. An AI seat has no client and never submits a
            // Debug action; excluding it keeps the strict wire gate in
            // `handle_action` a meaningful per-seat check rather than a global
            // on-switch. `human_seats()` is correct here and only here: both
            // callers of `rebuild_pregame_state` have already populated
            // `ai_seats` (`start_game` <- `create_game_with_ai`;
            // `apply_seat_delta` <- its own rebuild).
            self.state.debug_mode = true;
            for seat in self.human_seats() {
                self.state.debug_permitted.insert(seat);
            }
        }
    }

    /// Drop debug capability this process is not entitled to grant.
    ///
    /// The inverse of `seed_debug_capability`, applied on the restore path.
    /// `debug_mode` / `debug_permitted` are plain `#[serde(default)]` fields
    /// on `GameState` and `to_persisted` captures the whole state, so both
    /// ride inside `PersistedGameState`; `rebuild_pregame_state` never runs
    /// on a restore, so nothing else re-examines them. Two things can entitle
    /// a session to the capability, and they belong to different owners:
    ///
    /// - `format_config.allow_debug_actions` — a property of the GAME, which
    ///   legitimately travels with the blob. A sandbox game is a sandbox game
    ///   wherever it is resumed.
    /// - `HostingMode::SingleUser` — a property of THIS PROCESS, which does
    ///   not travel. This is the one the restore stamp establishes.
    ///
    /// If neither holds, `seed_debug_capability` would take neither branch,
    /// so the capability is one this process would never have granted: a blob
    /// written by a `--single-user` sidecar (`debug_mode: true`,
    /// `debug_permitted: {0}`) must not arrive on a shared server with seat 0
    /// still past the `handle_action` debug gate. Only deployment
    /// configuration (the sidecar's separate `PHASE_GAMES_DB`) keeps that
    /// unreachable today, which is not a mechanism.
    ///
    /// An entitled session keeps its persisted values **verbatim** rather
    /// than being re-seeded. An explicit `RevokeDebugPermission` taken before
    /// the save is game state, and re-deriving would silently reinstate the
    /// revoked seat.
    fn revoke_unentitled_debug_capability(&mut self) {
        if self.state.format_config.allow_debug_actions || self.hosting == HostingMode::SingleUser {
            return;
        }
        self.state.debug_mode = false;
        self.state.debug_permitted.clear();
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
                companion: deck.companion.clone(),
                planar_deck: deck.planar_deck.clone(),
                scheme_deck: deck.scheme_deck.clone(),
                attraction_deck: deck.attraction_deck.clone(),
                contraption_deck: deck.contraption_deck.clone(),
                sticker_sheets: deck.sticker_sheets.clone(),
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
        self.advance_state_revision();
        self.ai_session = Some(AiSession::arc_from_game(&self.state));
        self.lobby_meta = None;
        Ok(())
    }

    /// Run AI actions and return per-action broadcast data.
    ///
    /// Each entry contains: raw state snapshot, events, legal actions, and log entries.
    /// The caller is responsible for filtering the state per-player before sending.
    /// Returns an empty vec if the session has no AI seats.
    ///
    /// GH #1507: also a no-op while a takeback request is pending. AI moves
    /// mutate `self.state` directly (not through `handle_action`'s pending-
    /// takeback guard), so without this check an AI seat could advance the
    /// authoritative state — e.g. via a reconnecting human's follow-up AI
    /// turn — out from under the snapshot the table is voting to roll back
    /// to. Every call site (join-fills-the-room, reconnect, fresh AI-game
    /// creation) is gated here once rather than at each caller.
    pub fn run_ai(&mut self) -> Vec<RevisionedActionResult> {
        if self.ai_seats.is_empty() || self.pending_takeback.is_some() {
            return vec![];
        }

        let mut rng = rand::rng();
        let ai_session = self
            .ai_session
            .get_or_insert_with(|| AiSession::arc_from_game(&self.state));
        let ai_results = phase_ai::auto_play::run_ai_actions(
            &mut self.state,
            &self.ai_seats,
            &self.ai_configs,
            &mut rng,
            ai_session,
        );

        if !ai_results.is_empty() {
            debug!(game = %self.game_code, ai_actions = ai_results.len(), "AI actions computed");
        }

        ai_results
            .into_iter()
            .map(|r| {
                let (legal, spell_costs, by_object) = engine_legal_actions_full(&r.state);
                let auto_pass = auto_pass_recommended(&r.state, &legal);
                let revision = self.advance_state_revision();
                (
                    revision,
                    (
                        r.state,
                        r.events,
                        legal,
                        r.log_entries,
                        auto_pass,
                        spell_costs,
                        by_object,
                    ),
                )
            })
            .collect()
    }

    /// Recomputes the broadcast-ready fields (legal actions, auto-pass,
    /// spell costs, per-object grouping) for the session's *current* state.
    /// Used after any out-of-band mutation that doesn't go through
    /// `handle_action` — e.g. an approved takeback rollback — so the
    /// resulting `StateUpdate` carries data consistent with a normal action.
    pub fn current_broadcast_snapshot(&self) -> BroadcastSnapshot {
        let (legal_actions, spell_costs, by_object) = engine_legal_actions_full(&self.state);
        let auto_pass = auto_pass_recommended(&self.state, &legal_actions);
        (
            self.state.clone(),
            legal_actions,
            auto_pass,
            spell_costs,
            by_object,
        )
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
            state_revision: self.state_revision,
            state: PersistedGameState::capture(self.state.clone()),
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
        let mut state = ps.state.into_game_state();

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
        // Re-bind rather than trusting any id the blob carries, on the same
        // principle that `restore_session` re-stamps `hosting` and revokes an
        // unentitled debug capability: a persisted blob never drives authority.
        bind_interaction_session(&mut state, &ps.game_code);

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
        let ai_session = if ps.game_started {
            Some(AiSession::arc_from_game(&state))
        } else {
            None
        };

        GameSession {
            game_code: ps.game_code,
            state_revision: ps.state_revision,
            state,
            player_tokens: ps.player_tokens,
            connected: vec![false; pc],
            decks: vec![None; pc],
            display_names: ps.display_names,
            reservations: HashMap::new(),
            timer_seconds: ps.timer_seconds,
            // Least-privilege placeholder. `hosting` is a property of THIS
            // process, not of the persisted blob, and is re-stamped by
            // `SessionManager::restore_session`. Between here and that stamp
            // the value can only *under*-grant, never over-grant.
            hosting: HostingMode::Shared,
            player_count: ps.player_count,
            ai_seats,
            ai_configs,
            ai_session,
            lobby_meta: ps.lobby_meta,
            game_started: ps.game_started,
            start_when_full: ps.start_when_full,
            ranked: ps.ranked,
            start_events: Vec::new(),
            pending_takeback: None,
            takeback_history: VecDeque::new(),
        }
    }
}

pub struct SessionManager {
    pub sessions: HashMap<String, GameSession>,
    pub reconnect: ReconnectManager,
    /// Deployment shape of this process, stamped onto every session this
    /// manager creates or restores. See `HostingMode`.
    pub hosting: HostingMode,
    /// Maps player_token -> game_code for token-based lookups.
    token_to_game: HashMap<String, String>,
}

impl SessionManager {
    /// A shared instance: other humans may join or spectate.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::default(),
            hosting: HostingMode::Shared,
            token_to_game: HashMap::new(),
        }
    }

    /// The desktop shell's sidecar: one user, loopback-bound, no other client
    /// can reach it. `grace_period` sets the reconnect window, which a
    /// single-user instance makes effectively unbounded.
    pub fn single_user(grace_period: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::new(grace_period),
            hosting: HostingMode::SingleUser,
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
        // CR 732.2a: Bo3 is inherently 2-player, but the combo-detector opt-in is
        // player-count-agnostic (infinite loops are a Commander staple), so carry
        // `loop_detection` through for any table size while resetting `match_type`.
        // `set_match_config` is the single authority that projects the opt-in onto
        // the runtime `GameState::loop_detection` gate.
        let match_config = if player_count == 2 {
            match_config
        } else {
            MatchConfig {
                loop_detection: match_config.loop_detection,
                ..MatchConfig::default()
            }
        };
        state.set_match_config(match_config);
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

        bind_interaction_session(&mut state, &game_code);

        let session = GameSession {
            game_code: game_code.clone(),
            state_revision: 0,
            state,
            player_tokens,
            connected,
            decks,
            display_names,
            reservations: HashMap::new(),
            timer_seconds,
            hosting: self.hosting,
            player_count,
            ai_seats: HashSet::new(),
            ai_configs: HashMap::new(),
            ai_session: None,
            lobby_meta: None,
            game_started: false,
            start_when_full: true,
            ranked: false,
            start_events: Vec::new(),
            pending_takeback: None,
            takeback_history: VecDeque::new(),
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

    /// Returns the exact mana sources automatic payment would use without
    /// changing the authenticated game session.
    pub fn preview_mana_payment(
        &self,
        game_code: &str,
        player_token: &str,
        action: &GameAction,
    ) -> Result<Vec<ObjectId>, String> {
        let session = self
            .sessions
            .get(game_code)
            .ok_or_else(|| format!("Game not found: {game_code}"))?;
        let player = session
            .player_for_token(player_token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        preview_auto_payment_sources(&session.state, player, action)
            .map_err(|error| format!("Engine error: {error}"))
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

        // GH #1507: while a takeback request is awaiting approval, the
        // authoritative state must not move out from under it — a new
        // action here would either invalidate the snapshot the table is
        // voting on or silently discard the action once the rollback lands.
        // Require the table to resolve (approve/decline/cancel) first.
        if session.pending_takeback.is_some() {
            return Err(
                "A takeback request is pending — resolve it before taking further actions"
                    .to_string(),
            );
        }

        // Debug capability gate. `debug_permitted` is the single authority:
        // a `Debug(_)` is accepted iff the submitting player is in the set.
        // `seed_debug_capability` fills it from one of two sources — every
        // seat when the game is sandbox-flagged, or the human seats when this
        // process is a `HostingMode::SingleUser` desktop instance — and the
        // host adjusts it afterwards via `GrantDebugPermission` /
        // `RevokeDebugPermission` (sandbox games only). Naming a mode in the
        // refusal would be wrong for the single-user source, so the message
        // stays on the seat, which is what was actually checked.
        if matches!(action, GameAction::Debug(_))
            && !session.state.debug_permitted.contains(&player)
        {
            return Err("Debug actions are not permitted for this seat".to_string());
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

        // The engine is the sole admission authority for game actions. Session
        // policy above authenticates the actor and controls takeback/debug
        // access; the engine validates actor authorization and action shape.
        // Candidate enumeration is advisory for clients and AI, not a second
        // legality gate: several legal action classes are combinatorial.
        let records_takeback = !action.is_actor_scoped_preference();

        let pre_action_state = records_takeback.then(|| session.state.clone());

        // Set player names for log resolution.
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
        if let Some(snapshot) = pre_action_state {
            session.push_takeback_state(player, snapshot);
        }

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
    pub fn restore_session(&mut self, mut session: GameSession) {
        // Deployment shape is a property of THIS process, never of the
        // persisted blob. Re-stamped here, never deserialized. This is the
        // sole entry for a restored session into a manager (`sessions.insert`
        // has exactly two call sites: create, and this one).
        session.hosting = self.hosting;
        // The stamp above only stops the blob from driving a future
        // *derivation*. `debug_mode` / `debug_permitted` are serialized state
        // and arrive already set, so a sidecar's capability would otherwise
        // walk straight into a shared server. Order matters: this reads the
        // `hosting` just stamped.
        session.revoke_unentitled_debug_capability();
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
    use std::collections::BTreeSet;

    use engine::database::card_db::CardDatabase;
    use engine::game::deck_loading::DeckEntry;
    use engine::game::engine::apply;
    use engine::game::interaction::derive_viewer_interaction;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::TargetRef;
    use engine::types::actions::PrecastCopyShortcutResponse;
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::game_state::{CastPaymentMode, PersistedGameState, WaitingFor};
    use engine::types::interaction::{InteractionAvailability, InteractionReasonCode};
    use engine::types::mana::ManaCost;
    use engine::types::phase::{Phase, PhaseStop, PhaseStopScope};
    use engine::types::zones::Zone;
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

    /// `GameState::new` leaves `interaction_session_id` as `None`, and while it is
    /// unset every `derive_viewer_interaction` call returns `AuthorityUnbound` with
    /// zero opportunities. Nothing self-heals it: `ensure_interaction_authority`
    /// only maintains slots for an already-bound session.
    #[test]
    fn created_session_binds_interaction_authority() {
        let mut mgr = SessionManager::new();
        let (code, _token) = mgr.create_game(make_deck());

        assert_eq!(
            mgr.sessions
                .get(&code)
                .unwrap()
                .state
                .interaction_session_id,
            Some(InteractionSessionId(code.clone())),
            "a created session must carry interaction authority"
        );
    }

    /// The behavioural half: authority actually reaches `derive_viewer_interaction`.
    ///
    /// Guarded against vacuity in-test. `derive_viewer_interaction` checks terminal
    /// and `can_submit` *before* the authority check, so a state that never reaches
    /// the third guard would pass this trivially. Clearing the binding on a copy and
    /// asserting the unbound verdict *does* appear proves the assertion discriminates.
    #[test]
    fn bound_session_does_not_report_authority_unbound() {
        let mut mgr = SessionManager::new();
        let (code, _token) = mgr.create_game(make_deck());
        mgr.join_game(&code, make_deck())
            .expect("second seat joins");

        let state = mgr.sessions.get(&code).unwrap().state.clone();
        let filtered = filter_state_for_player(&state, PlayerId(0));

        let unbound_reason = InteractionAvailability::Unsupported {
            reason: InteractionReasonCode::AuthorityUnbound,
        };

        let bound = derive_viewer_interaction(&state, &filtered, PlayerId(0));
        assert_ne!(
            bound.availability, unbound_reason,
            "a bound session must not report AuthorityUnbound"
        );

        // Clearing the slots alongside the session is not cosmetic: the engine
        // asserts that an unbound state holds no slots
        // (`debug_assert_interaction_consistency`), and clearing them is exactly
        // what the unbound production path did before this fix
        // (`ensure_interaction_authority`). Dropping only the session id builds a
        // state the engine considers impossible.
        let mut stripped = state.clone();
        stripped.interaction_session_id = None;
        stripped.active_interaction_slots.clear();
        let unbound = derive_viewer_interaction(&stripped, &filtered, PlayerId(0));
        assert_eq!(
            unbound.availability, unbound_reason,
            "probe is vacuous: this state does not reach the authority check at all, \
             so the assertion above proves nothing"
        );
    }

    /// A restored blob must be re-bound, not trusted. Mirrors how `restore_session`
    /// re-stamps `hosting` and revokes an unentitled debug capability.
    ///
    /// The blob is deliberately poisoned with a foreign session id first. Without
    /// that step the test would be vacuous: `interaction_session_id` is an ordinary
    /// serialized field, so a snapshot of an already-bound session round-trips its
    /// id and the assertion would hold whether or not `from_persisted` re-binds.
    #[test]
    fn restored_session_rebinds_interaction_authority() {
        let mut mgr = SessionManager::new();
        let (code, _token) = mgr.create_game(make_deck());
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .state
            .interaction_session_id = Some(InteractionSessionId("some-other-game".to_string()));
        let persisted = mgr.sessions.get(&code).unwrap().to_persisted();

        let db = CardDatabase::default();
        let restored = GameSession::from_persisted(persisted, &db);

        assert_eq!(
            restored.state.interaction_session_id,
            Some(InteractionSessionId(code)),
            "the restored session's authority must come from the game code, not \
             from whatever id the persisted blob happened to carry"
        );
    }

    #[test]
    fn player_slot_info_omits_team_metadata_for_individual_formats() {
        for format in [FormatConfig::standard(), FormatConfig::commander()] {
            let mut mgr = SessionManager::new();
            let (code, _) = mgr.create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                2,
                MatchConfig::default(),
                Some(format),
            );

            let slots = mgr.sessions.get(&code).unwrap().player_slot_info();
            assert_eq!(slots.len(), 2);
            assert!(slots.iter().all(|slot| slot.team_info.is_none()));

            let json = serde_json::to_value(&slots[0]).unwrap();
            assert!(json.get("teamInfo").is_none());
        }
    }

    #[test]
    fn player_slot_info_includes_two_headed_giant_team_metadata() {
        let mut mgr = SessionManager::new();
        let (code, _) = mgr.create_game_n_players(
            make_deck(),
            "Host".to_string(),
            None,
            4,
            MatchConfig::default(),
            Some(FormatConfig::two_headed_giant()),
        );

        let slots = mgr.sessions.get(&code).unwrap().player_slot_info();
        let team_indices: Vec<u8> = slots
            .iter()
            .map(|slot| slot.team_info.unwrap().team_index)
            .collect();
        let positions: Vec<u8> = slots
            .iter()
            .map(|slot| slot.team_info.unwrap().position_in_team)
            .collect();

        assert_eq!(team_indices, vec![0, 0, 1, 1]);
        assert_eq!(positions, vec![0, 1, 0, 1]);
    }

    /// CR 732.2a (#4603 opt-in, Best-of-N): the combo-detector opt-in lives on the
    /// immutable `MatchConfig` and is projected onto `GameState::loop_detection` by
    /// `set_match_config` at BOTH game creation AND the between-games rebuild, so a Bo3
    /// match keeps a consistent detector across every game. No mid-game action can flip
    /// it (removed as the security fix) — the config is the sole provenance.
    ///
    /// REVERT-FAIL: change either provenance site (create_game_n_players or
    /// rebuild_pregame_state) back to a raw `state.match_config = …` assignment that
    /// drops the `loop_detection` projection ⇒ the corresponding `is_on()` flips.
    #[test]
    fn loop_detection_config_persists_across_bo3_rebuild() {
        use engine::types::game_state::LoopDetectionMode;
        use engine::types::match_config::MatchType;

        let mut mgr = SessionManager::new();
        let (code, _) = mgr.create_game_n_players(
            make_deck(),
            "Host".to_string(),
            None,
            2,
            MatchConfig {
                match_type: MatchType::Bo3,
                loop_detection: LoopDetectionMode::On,
            },
            None,
        );

        // Game 1: the creation site projects the opt-in onto the runtime flag.
        assert!(
            mgr.sessions
                .get(&code)
                .unwrap()
                .state
                .loop_detection
                .is_on(),
            "the MatchConfig opt-in must enable the detector at game-1 creation"
        );

        // Between-games rebuild (game 2): the immutable config is re-read, so the
        // detector stays consistent across the whole Bo3 match.
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .rebuild_pregame_state(2);
        assert!(
            mgr.sessions.get(&code).unwrap().state.loop_detection.is_on(),
            "the Bo3 between-games rebuild must re-derive the detector from the immutable MatchConfig"
        );
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
            min_main_deck_size: 0,
            max_sideboard_size: None,
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

    /// `SetPhaseStops` is keyed to the authenticated player and delegated to the
    /// engine write-handler (keyed by `actor`), so a non-priority player's stops
    /// land on their OWN entry — not the priority holder's. Mirrors the
    /// `SetPriorityYield` delegate precedent.
    #[test]
    fn set_phase_stops_lands_on_authenticated_players_entry() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();

        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        // Dispatch from the player who does NOT hold priority.
        let (non_priority_player, non_priority_token) = if priority_player == PlayerId(0) {
            (PlayerId(1), token1)
        } else {
            (PlayerId(0), token0)
        };

        let stops = vec![PhaseStop {
            phase: Phase::DeclareBlockers,
            scope: PhaseStopScope::OpponentsTurns,
        }];
        let result = mgr.handle_action(
            &code,
            &non_priority_token,
            GameAction::SetPhaseStops {
                stops: stops.clone(),
            },
        );
        assert!(
            result.is_ok(),
            "SetPhaseStops from a non-priority player should succeed: {:?}",
            result.err()
        );

        let state = &mgr.sessions.get(&code).unwrap().state;
        assert_eq!(
            state.phase_stops.get(&non_priority_player),
            Some(&stops),
            "the write must land on the authenticated (non-priority) player's entry"
        );
        assert!(
            !state.phase_stops.contains_key(&priority_player),
            "the priority holder's entry must remain untouched"
        );
    }

    #[test]
    fn priority_passing_mode_route_updates_recommendation_without_history_or_advance() {
        use std::sync::Arc;

        use engine::game::zones::create_object;
        use engine::types::ability::{
            AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
        };
        use engine::types::game_state::PriorityPassingMode;
        use engine::types::identifiers::CardId;

        let (mut mgr, code, token0, _token1) = setup_two_player_game();
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.state.active_player = PlayerId(0);
        session.state.priority_player = PlayerId(0);
        session.state.phase = Phase::End;
        session.state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let source = create_object(
            &mut session.state,
            CardId(900),
            PlayerId(0),
            "Priority Test Source".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut session.state.objects.get_mut(&source).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ),
        );
        let waiting_before = session.state.waiting_for.clone();
        let stack_before = session.state.stack.clone();
        let pass_before = session.state.priority_passes.clone();
        let history_before = session.takeback_history.len();

        let (_, events, _, logs, standard, _, _) = mgr
            .handle_action(
                &code,
                &token0,
                GameAction::SetPriorityPassingMode {
                    mode: PriorityPassingMode::Standard,
                },
            )
            .expect("Standard preference route");
        assert!(!standard, "meaningful End-step action holds in Standard");
        assert!(events.is_empty() && logs.is_empty());

        let (_, events, _, logs, skips_low_use_window, _, _) = mgr
            .handle_action(
                &code,
                &token0,
                GameAction::SetPriorityPassingMode {
                    mode: PriorityPassingMode::SkipLowUseWindows,
                },
            )
            .expect("low-use-window preference route");
        assert!(
            skips_low_use_window,
            "the opt-in mode passes the player's own empty End window"
        );
        assert!(events.is_empty() && logs.is_empty());
        assert_eq!(
            mgr.sessions
                .get(&code)
                .unwrap()
                .state
                .priority_passing_modes,
            HashMap::from([(PlayerId(0), PriorityPassingMode::SkipLowUseWindows)])
        );

        let (_, _, _, _, standard_again, _, _) = mgr
            .handle_action(
                &code,
                &token0,
                GameAction::SetPriorityPassingMode {
                    mode: PriorityPassingMode::Standard,
                },
            )
            .expect("return to sparse Standard");
        assert!(!standard_again);
        let session = mgr.sessions.get(&code).unwrap();
        assert!(session.state.priority_passing_modes.is_empty());
        assert_eq!(session.state.waiting_for, waiting_before);
        assert_eq!(session.state.stack, stack_before);
        assert_eq!(session.state.priority_passes, pass_before);
        assert_eq!(session.takeback_history.len(), history_before);
    }

    /// `ReorderHand` succeeds even when the sender is not the priority holder.
    /// The hand is reordered to the requested permutation.
    #[test]
    fn reorder_hand_succeeds_while_opponent_has_priority() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();
        let history_before = mgr.sessions.get(&code).unwrap().takeback_history.len();

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
        assert_eq!(
            session.takeback_history.len(),
            history_before,
            "a cosmetic hand reorder must not create a takeback checkpoint"
        );
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
        let history_before = mgr.sessions.get(&code).unwrap().takeback_history.len();

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
        assert_eq!(
            session.takeback_history.len(),
            history_before,
            "a rejected engine action must not create a takeback checkpoint"
        );
    }

    /// Manual payment remains accepted and reaches the engine unchanged.
    #[test]
    fn engine_apply_accepts_manual_payment_mode_casts() {
        let (mut mgr, code, token0, _token1) = setup_two_player_game();

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Gate Probe", false, "Draw a card.")
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;

        let session = mgr.sessions.get_mut(&code).unwrap();
        session.state = runner.state().clone();

        let result = mgr.handle_action(
            &code,
            &token0,
            GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Manual,
            },
        );
        assert!(
            result.is_ok(),
            "Manual-mode cast must be validated by the engine: {:?}",
            result.err()
        );
        assert_eq!(
            mgr.sessions
                .get(&code)
                .unwrap()
                .state
                .pending_cast
                .as_ref()
                .map(|cast| cast.payment_mode),
            Some(CastPaymentMode::Manual),
            "the engine must receive the submitted manual payment mode unchanged"
        );
    }

    // ── Takeback tests (GH #1507) ────────────────────────────────────────

    use crate::takeback::TakebackOutcome;

    fn precast_offer_runner() -> (GameRunner, u64) {
        const CHAIN_OF_SMOG: &str = "Target player discards two cards. That player may copy this spell and may choose a new target for that copy.";
        let db = CardDatabase::from_mtgjson(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../data/mtgjson/test_fixture.json"),
        )
        .expect("parser fixture must contain Witherbloom Apprentice");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Witherbloom Apprentice", Zone::Battlefield, &db);
        let chain = scenario
            .add_spell_to_hand_from_oracle(P0, "Chain of Smog", false, CHAIN_OF_SMOG)
            .id();
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&chain].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: chain,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast Chain through the engine reducer");
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P0)),
            })
            .expect("target Chain at P0");
        let epoch = match &runner.state().waiting_for {
            WaitingFor::PrecastCopyShortcutOffer { epoch, .. } => *epoch,
            ref other => panic!("expected live pre-cast offer, got {other:?}"),
        };
        (runner, epoch)
    }

    /// Two human players: a request stays `Pending` until the other player
    /// approves, then the rolled-back state matches the pre-action snapshot.
    #[test]
    fn takeback_requires_unanimous_human_approval() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();

        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        let (acting_token, other_player) = if priority_player == PlayerId(0) {
            (token0.clone(), PlayerId(1))
        } else {
            (token1.clone(), PlayerId(0))
        };

        let state_before = mgr.sessions.get(&code).unwrap().state.clone();
        let result = mgr.handle_action(&code, &acting_token, GameAction::PassPriority);
        assert!(
            result.is_ok(),
            "PassPriority should succeed: {:?}",
            result.err()
        );
        assert_ne!(
            mgr.sessions.get(&code).unwrap().state.waiting_for,
            state_before.waiting_for,
            "sanity: the action should have actually changed turn state"
        );

        let session = mgr.sessions.get_mut(&code).unwrap();
        let outcome = session.request_takeback(priority_player).unwrap();
        assert_eq!(outcome, TakebackOutcome::Pending);

        // A second concurrent request is rejected — only one in flight at a time.
        assert!(session.request_takeback(other_player).is_err());

        let outcome = session.respond_takeback(other_player, true).unwrap();
        assert_eq!(outcome, TakebackOutcome::Approved);
        assert_eq!(
            session.state.waiting_for, state_before.waiting_for,
            "approved takeback should restore the pre-action waiting_for"
        );
        assert!(session.pending_takeback.is_none());
    }

    /// A takeback restores a live shortcut offer through the same rekeying
    /// boundary as persisted sessions. The old offer capability must not be
    /// usable after the approved rollback.
    #[test]
    fn approved_takeback_rekeys_precast_shortcut_capabilities() {
        let (mut mgr, code, _token0, _token1) = setup_two_player_game();
        let (runner, stale_epoch) = precast_offer_runner();
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.state = runner.state().clone();
        session.push_takeback_snapshot(P0);
        apply(
            &mut session.state,
            P0,
            GameAction::PrecastCopyShortcut {
                epoch: stale_epoch,
                response: PrecastCopyShortcutResponse::Decline,
            },
        )
        .expect("mutate away from the offer before requesting takeback");

        assert_eq!(session.request_takeback(P0), Ok(TakebackOutcome::Pending));
        assert_eq!(
            session.respond_takeback(P1, true),
            Ok(TakebackOutcome::Approved)
        );
        let fresh_epoch = match &session.state.waiting_for {
            WaitingFor::PrecastCopyShortcutOffer { epoch, .. } => *epoch,
            ref other => panic!("approved takeback must reissue the offer, got {other:?}"),
        };
        assert_ne!(fresh_epoch, stale_epoch);
        assert!(apply(
            &mut session.state,
            P0,
            GameAction::PrecastCopyShortcut {
                epoch: stale_epoch,
                response: PrecastCopyShortcutResponse::Decline,
            },
        )
        .is_err());
    }

    /// Existing server snapshots wrote a raw `GameState` at `state`. That
    /// untrusted representation has no private shortcut transcript, so it
    /// must deserialize and resume at ordinary priority rather than preserving
    /// a protocol wait it cannot validate.
    #[test]
    fn persisted_session_restores_legacy_raw_state_and_current_trusted_envelope() {
        let (mgr, code, _token0, _token1) = setup_two_player_game();
        let (runner, stale_epoch) = precast_offer_runner();
        let mut persisted = mgr.sessions[&code].to_persisted();

        let mut legacy_json = serde_json::to_value(&persisted)
            .expect("current persisted session serializes before compatibility rewrite");
        legacy_json["state"] = serde_json::to_value(runner.state())
            .expect("historical raw GameState representation serializes");
        let legacy: PersistedSession = serde_json::from_value(legacy_json)
            .expect("legacy raw persisted session remains decodable");
        assert!(matches!(&legacy.state, PersistedGameState::Raw(_)));

        let db = CardDatabase::from_mtgjson(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../data/mtgjson/test_fixture.json"),
        )
        .expect("parser fixture must contain Witherbloom Apprentice");
        let legacy_restored = GameSession::from_persisted(legacy, &db);
        assert!(matches!(
            legacy_restored.state.waiting_for,
            WaitingFor::Priority { player } if player == P0
        ));

        persisted.state = PersistedGameState::capture(runner.state().clone());
        let trusted: PersistedSession = serde_json::from_value(
            serde_json::to_value(persisted).expect("trusted session serializes"),
        )
        .expect("trusted persisted session remains decodable");
        assert!(matches!(&trusted.state, PersistedGameState::Trusted(_)));
        let trusted_restored = GameSession::from_persisted(trusted, &db);
        let fresh_epoch = match trusted_restored.state.waiting_for {
            WaitingFor::PrecastCopyShortcutOffer { epoch, .. } => epoch,
            ref other => panic!("trusted restore must reissue its offer, got {other:?}"),
        };
        assert_ne!(fresh_epoch, stale_epoch);
    }

    /// Player A acts, then player B acts. A's takeback request must restore
    /// the state to right before A's *own* action — not merely undo B's more
    /// recent action while leaving A's action intact. Regression test for a
    /// bug where `takeback_history` ignored which player produced each
    /// checkpoint and always restored the single most recent global entry.
    #[test]
    fn takeback_restores_requesters_own_action_not_latest_global_action() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();

        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        let (player_a, token_a, player_b, token_b) = if priority_player == PlayerId(0) {
            (PlayerId(0), token0.clone(), PlayerId(1), token1.clone())
        } else {
            (PlayerId(1), token1.clone(), PlayerId(0), token0.clone())
        };

        let state_before_a = mgr.sessions.get(&code).unwrap().state.clone();

        let result = mgr.handle_action(&code, &token_a, GameAction::PassPriority);
        assert!(
            result.is_ok(),
            "A's PassPriority should succeed: {:?}",
            result.err()
        );
        let state_after_a = mgr.sessions.get(&code).unwrap().state.clone();
        assert_ne!(
            state_after_a.waiting_for, state_before_a.waiting_for,
            "sanity: A's action should have changed turn state"
        );

        let result = mgr.handle_action(&code, &token_b, GameAction::PassPriority);
        assert!(
            result.is_ok(),
            "B's PassPriority should succeed: {:?}",
            result.err()
        );
        let state_after_b = mgr.sessions.get(&code).unwrap().state.clone();
        assert_ne!(
            state_after_b.waiting_for, state_after_a.waiting_for,
            "sanity: B's action should have changed turn state further"
        );

        // A requests a takeback — must target the checkpoint before A's own
        // action, not the checkpoint before B's (more recent) action.
        let session = mgr.sessions.get_mut(&code).unwrap();
        let outcome = session.request_takeback(player_a).unwrap();
        assert_eq!(outcome, TakebackOutcome::Pending);
        let outcome = session.respond_takeback(player_b, true).unwrap();
        assert_eq!(outcome, TakebackOutcome::Approved);

        assert_eq!(
            session.state.waiting_for, state_before_a.waiting_for,
            "takeback must restore to before the REQUESTER's own action"
        );
        assert_ne!(
            session.state.waiting_for, state_after_a.waiting_for,
            "must not merely undo only the other player's later action"
        );
    }

    /// A single decline withdraws the request and leaves state untouched.
    #[test]
    fn takeback_decline_leaves_state_untouched() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();
        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        let (acting_token, other_player) = if priority_player == PlayerId(0) {
            (token0.clone(), PlayerId(1))
        } else {
            (token1.clone(), PlayerId(0))
        };

        let _ = mgr.handle_action(&code, &acting_token, GameAction::PassPriority);
        let state_after_pass = mgr.sessions.get(&code).unwrap().state.clone();

        let session = mgr.sessions.get_mut(&code).unwrap();
        session.request_takeback(priority_player).unwrap();
        let outcome = session.respond_takeback(other_player, false).unwrap();
        assert_eq!(outcome, TakebackOutcome::Rejected);
        assert!(session.pending_takeback.is_none());
        assert_eq!(session.state.waiting_for, state_after_pass.waiting_for);
    }

    /// The requester can withdraw their own request before anyone responds;
    /// nobody else may cancel it.
    #[test]
    fn takeback_cancel_is_requester_only() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();
        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        let (acting_token, other_player) = if priority_player == PlayerId(0) {
            (token0.clone(), PlayerId(1))
        } else {
            (token1.clone(), PlayerId(0))
        };

        let _ = mgr.handle_action(&code, &acting_token, GameAction::PassPriority);
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.request_takeback(priority_player).unwrap();

        assert!(session.cancel_takeback(other_player).is_err());
        assert!(session.pending_takeback.is_some());

        assert!(session.cancel_takeback(priority_player).is_ok());
        assert!(session.pending_takeback.is_none());
    }

    /// With no prior action, there is nothing to take back.
    #[test]
    fn takeback_with_no_history_is_rejected() {
        let (mut mgr, code, token0, _token1) = setup_two_player_game();
        let _ = token0;
        let session = mgr.sessions.get_mut(&code).unwrap();
        let player = match &session.state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        assert!(session.request_takeback(player).is_err());
    }

    /// While a takeback request is pending, new actions are rejected so the
    /// table can't race the vote.
    #[test]
    fn action_rejected_while_takeback_pending() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();
        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        let (acting_token, other_token) = if priority_player == PlayerId(0) {
            (token0.clone(), token1.clone())
        } else {
            (token1.clone(), token0.clone())
        };

        let _ = mgr.handle_action(&code, &acting_token, GameAction::PassPriority);
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.request_takeback(priority_player).unwrap();

        let result = mgr.handle_action(&code, &other_token, GameAction::PassPriority);
        assert!(
            result.is_err(),
            "action should be rejected while a takeback is pending"
        );
    }

    /// A solo human vs. AI seats auto-resolves their own takeback request —
    /// there's nobody else at the table to ask.
    #[test]
    fn takeback_auto_approves_for_sole_human_seat() {
        let mut mgr = SessionManager::new();
        let db = engine::database::CardDatabase::default();
        let (code, _token) = mgr.create_game_with_ai(
            make_deck(),
            "Host".to_string(),
            None,
            MatchConfig::default(),
            vec![(1, AiDifficulty::Easy, make_deck())],
            Vec::new(),
            None,
            &db,
        );

        let session = mgr.sessions.get_mut(&code).unwrap();
        // Force a known checkpoint to take back to, since the AI may have
        // already acted past mulligans by the time the game starts.
        session.push_takeback_snapshot(PlayerId(0));
        let outcome = session.request_takeback(PlayerId(0)).unwrap();
        assert_eq!(outcome, TakebackOutcome::Approved);
    }

    /// `pending_takeback_message` is what a reconnecting socket replays to
    /// learn about an in-flight vote — `None` when nothing is pending, and
    /// the requester's identity once a request exists.
    #[test]
    fn pending_takeback_message_reflects_request_state() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();
        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        let acting_token = if priority_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };

        assert!(mgr
            .sessions
            .get(&code)
            .unwrap()
            .pending_takeback_message()
            .is_none());

        let _ = mgr.handle_action(&code, acting_token, GameAction::PassPriority);
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.request_takeback(priority_player).unwrap();

        match session.pending_takeback_message() {
            Some(crate::protocol::ServerMessage::TakebackRequested { requester, .. }) => {
                assert_eq!(requester, priority_player);
            }
            other => panic!("expected TakebackRequested, got {:?}", other),
        }
    }

    /// GH #1507 regression guard: a pending takeback request must survive a
    /// disconnect/reconnect cycle so the reconnecting socket can still be
    /// told about it (see `pending_takeback_message` and the phase-server
    /// Reconnect handler that replays it).
    #[test]
    fn pending_takeback_survives_disconnect_and_reconnect() {
        let (mut mgr, code, token0, token1) = setup_two_player_game();
        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {:?}", other),
        };
        let (acting_token, approver, approver_token) = if priority_player == PlayerId(0) {
            (token0.clone(), PlayerId(1), token1.clone())
        } else {
            (token1.clone(), PlayerId(0), token0.clone())
        };

        let _ = mgr.handle_action(&code, &acting_token, GameAction::PassPriority);
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.request_takeback(priority_player).unwrap();

        mgr.handle_disconnect(&code, approver);
        let reconnected_state = mgr.handle_reconnect(&code, &approver_token);
        assert!(
            reconnected_state.is_ok(),
            "reconnect should succeed: {:?}",
            reconnected_state.err()
        );

        let session = mgr.sessions.get(&code).unwrap();
        assert!(
            session.pending_takeback.is_some(),
            "the pending takeback must still be there for the reconnecting socket to be told about"
        );
        assert!(session.pending_takeback_message().is_some());
    }

    /// GH #1507 follow-up: `run_ai` must not advance the authoritative state
    /// while a takeback vote is pending — AI moves bypass `handle_action`'s
    /// pending-takeback guard entirely (they mutate `self.state` directly),
    /// so without an explicit check here a reconnecting human's follow-up AI
    /// turn (or any other `run_ai` call site) could move the state out from
    /// under the snapshot the table is voting to roll back to.
    #[test]
    fn run_ai_is_noop_while_takeback_is_pending() {
        let mut mgr = SessionManager::new();
        let (code, _token0) = mgr.create_game_n_players(
            make_deck(),
            "Host".to_string(),
            None,
            3,
            MatchConfig::default(),
            None,
        );
        let (_token1, _) = mgr.join_game(&code, make_deck()).unwrap();
        let (_token2, _) = mgr.join_game(&code, make_deck()).unwrap();

        // Retroactively mark seat 2 as AI-controlled (server-side bookkeeping
        // only — the engine state itself has no notion of AI seats), and
        // hand it a known legal action: an undecided mulligan. This is
        // exactly the kind of action `run_ai` would otherwise resolve.
        let ai_pid = PlayerId(2);
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.ai_seats.insert(ai_pid);
        session.ai_configs.insert(
            ai_pid,
            phase_ai::config::create_config_for_players(AiDifficulty::Easy, Platform::Native, 3),
        );
        session.state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![engine::types::game_state::MulliganDecisionEntry {
                player: ai_pid,
                mulligan_count: 0,
                phase: engine::types::game_state::MulliganDecisionPhase::Declare,
            }],
            free_first_mulligan: true,
        };

        // Player 0 requests a takeback; with two human seats (0 and 1) it
        // stays Pending until player 1 also approves.
        session.push_takeback_snapshot(PlayerId(0));
        let outcome = session.request_takeback(PlayerId(0)).unwrap();
        assert_eq!(outcome, TakebackOutcome::Pending);

        let state_before = session.state.clone();
        let ai_results = session.run_ai();
        assert!(
            ai_results.is_empty(),
            "run_ai must no-op while a takeback vote is pending, even though the AI seat has a legal action"
        );
        assert_eq!(
            session.state.waiting_for, state_before.waiting_for,
            "authoritative state must not move while a takeback vote is pending"
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

    /// Fixture shape for the `HostingMode` tests below, matching the existing
    /// `takeback_auto_approves_for_sole_human_seat` precedent: a default
    /// `CardDatabase` and `format_config: None` are enough to run
    /// `create_game_with_ai` all the way through `start_game`. (The
    /// "can't fully start the game without a database" comment elsewhere in
    /// this module does not apply to this path.) `format_config: None`
    /// yields `FormatConfig::standard`, so `allow_debug_actions` is false —
    /// which is what makes the capability assertions load-bearing rather
    /// than a restatement of the sandbox flag.
    fn single_ai_opponent_game(mgr: &mut SessionManager) -> String {
        let db = engine::database::CardDatabase::default();
        let (code, _token) = mgr.create_game_with_ai(
            make_deck(),
            "Host".to_string(),
            None,
            MatchConfig::default(),
            vec![(1, AiDifficulty::Easy, make_deck())],
            Vec::new(),
            None,
            &db,
        );
        code
    }

    #[test]
    fn single_user_instance_seeds_human_seats_through_start_game() {
        // The surviving-seam probe. `create_game_with_ai` calls `start_game`,
        // which calls `rebuild_pregame_state`, which replaces `self.state`
        // wholesale. Seeding placed anywhere earlier — `create_game_n_players`,
        // or the tail of `create_game_with_ai` — is wiped, and this test fails.
        // A test built on `create_game_n_players` alone (the shape both
        // pre-existing debug tests use) would pass against the wrong seam.
        let mut mgr = SessionManager::single_user(Duration::from_secs(60));
        let code = single_ai_opponent_game(&mut mgr);
        let session = mgr.sessions.get(&code).unwrap();

        // The capability came from the hosting mode, NOT from a sandbox
        // format flag: this assertion fails if someone reaches for
        // `FormatConfig::with_sandbox()` instead.
        assert!(
            !session.state.format_config.allow_debug_actions,
            "sandbox format flag must stay off — it changes hidden-info exposure"
        );
        assert!(session.state.debug_mode);
        assert_eq!(
            session.state.debug_permitted,
            BTreeSet::from([PlayerId(0)]),
            "human seats only: the AI seat has no client and never submits Debug"
        );
    }

    #[test]
    fn shared_instance_does_not_seed_debug_capability() {
        // Discriminates "seeded from the hosting mode" from "seeded always".
        // Without this, the test above passes for an implementation that
        // seeds unconditionally — which would open the debug panel on the
        // online server.
        let mut mgr = SessionManager::new();
        let code = single_ai_opponent_game(&mut mgr);
        let session = mgr.sessions.get(&code).unwrap();

        assert!(!session.state.debug_mode);
        assert!(session.state.debug_permitted.is_empty());
    }

    #[test]
    fn single_user_human_seat_may_submit_debug_action_but_ai_seat_may_not() {
        // Two authorities in one fixture, positive then negative, against the
        // SAME `session.state`.
        let mut mgr = SessionManager::single_user(Duration::from_secs(60));
        let db = engine::database::CardDatabase::default();
        let (code, host_token) = mgr.create_game_with_ai(
            make_deck(),
            "Host".to_string(),
            None,
            MatchConfig::default(),
            vec![(1, AiDifficulty::Easy, make_deck())],
            Vec::new(),
            None,
            &db,
        );

        // POSITIVE, through the real wire gate. `ShuffleLibrary` (not
        // `CreateCard`) is the reach-guard: `CreateCard` is resolved at the
        // WASM layer and the engine returns `InvalidAction` if it reaches
        // `apply()`, so a positive built on it passes identically against a
        // totally broken gate. `ShuffleLibrary` is state-only and actually
        // applied, so `Ok` is reachable only past the strict wire gate in
        // `handle_action` AND both engine gates.
        let ok = mgr.handle_action(
            &code,
            &host_token,
            GameAction::Debug(engine::types::actions::DebugAction::ShuffleLibrary {
                player_id: PlayerId(0),
            }),
        );
        assert!(ok.is_ok(), "seat 0 must be permitted: {:?}", ok.err());

        // NEGATIVE — and it deliberately does NOT go through `handle_action`.
        // That function authenticates by TOKEN, never by `PlayerId`:
        // `player_for_token` scans `player_tokens`, `create_game_n_players`
        // builds `vec![String::new(); pc]` and assigns only index 0, and
        // `create_game_with_ai` never assigns a token to an AI seat. So any
        // AI-seat attempt returns `Err("Invalid player token")`, satisfying a
        // bare `assert!(result.is_err())` IDENTICALLY against a totally broken
        // debug gate. Drive the per-seat gate where an AI seat is addressable:
        // the engine.
        let session = mgr.sessions.get_mut(&code).unwrap();
        assert_eq!(
            session.state.debug_permitted,
            BTreeSet::from([PlayerId(0)]),
            "fixture premise: only the human seat is permitted. An empty set \
             would make the engine's lenient gate ALLOW, flipping this test red"
        );
        let err = engine::game::apply(
            &mut session.state,
            PlayerId(1),
            GameAction::Debug(engine::types::actions::DebugAction::ShuffleLibrary {
                player_id: PlayerId(1),
            }),
        )
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("Debug actions require debug permission"),
            "must fail the per-seat gate specifically, not the debug_mode gate \
             ('Debug actions require debug_mode to be enabled') and not a token \
             check: {err:?}"
        );
    }

    #[test]
    fn seat_delta_rebuild_rederives_debug_capability() {
        // The SECOND caller of `rebuild_pregame_state`. `start_game` is the
        // first (covered above); `apply_seat_delta` reaches the same seam,
        // gated on `old_player_count != new_player_count`. A seeding
        // implementation wired only into `start_game` passes every test above
        // and fails this one.
        struct UnusedResolver;
        impl seat_reducer::types::DeckResolver for UnusedResolver {
            fn resolve(
                &self,
                _choice: &DeckChoice,
            ) -> Result<engine::game::deck_loading::PlayerDeckList, String> {
                panic!("human seat removal must not resolve a deck")
            }
        }

        let db = engine::database::CardDatabase::default();
        let mut mgr = SessionManager::single_user(Duration::from_secs(60));
        let (code, _host) = mgr.create_game_n_players(
            make_deck(),
            "Host".to_string(),
            None,
            3,
            MatchConfig::default(),
            None,
        );
        // Seat 1 joins; seat 2 is left waiting, because the reducer rejects
        // removing a claimed seat (`SeatClaimed`).
        mgr.join_game(&code, make_deck()).unwrap();

        // Premise: nothing is seeded before the rebuild, so the assertion
        // below cannot be satisfied by leftovers from game creation.
        assert!(
            mgr.sessions
                .get(&code)
                .unwrap()
                .state
                .debug_permitted
                .is_empty(),
            "fixture premise: pregame state carries no capability yet"
        );

        let resolver = UnusedResolver;
        let ctx = seat_reducer::types::ReducerCtx {
            platform: Platform::Native,
            deck_resolver: &resolver,
        };
        let mut seat_state = mgr.sessions.get(&code).unwrap().seat_state();
        let delta = seat_reducer::apply(
            &mut seat_state,
            SeatMutation::Remove { seat_index: 2 },
            &ctx,
        )
        .unwrap();
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.apply_seat_delta(seat_state, &delta, &db);

        // Re-derived at the NEW seat count, not carried stale from 3 seats.
        assert_eq!(session.player_count, 2);
        assert!(session.state.debug_mode);
        assert_eq!(
            session.state.debug_permitted,
            BTreeSet::from([PlayerId(0), PlayerId(1)]),
            "both remaining seats are human, and seat 2 must not survive"
        );
    }

    #[test]
    fn restore_session_stamps_this_instances_hosting() {
        // Direction matters, and only this one discriminates. `hosting` is
        // deliberately absent from `PersistedSession`, so `from_persisted`
        // ALWAYS yields the least-privilege `Shared` placeholder. Restoring
        // into a `Shared` manager therefore has the placeholder and the
        // manager agreeing, and would pass with the stamp deleted — verified,
        // not assumed. Restoring into a `SingleUser` manager makes the two
        // sources disagree, so only the manager's value winning can produce
        // `SingleUser` here.
        //
        // It is also the production direction: the desktop sidecar restores
        // its own suspended game and must regain the capability.
        let db = engine::database::CardDatabase::default();
        let mut origin = SessionManager::new();
        let code = single_ai_opponent_game(&mut origin);
        let persisted = origin.sessions.get(&code).unwrap().to_persisted();
        let restored = GameSession::from_persisted(persisted, &db);
        assert_eq!(
            restored.hosting,
            HostingMode::Shared,
            "premise: the un-stamped placeholder is Shared, so a SingleUser \
             result below can only have come from the manager"
        );

        let mut sidecar = SessionManager::single_user(Duration::from_secs(60));
        sidecar.restore_session(restored);
        let session = sidecar.sessions.get_mut(&code).unwrap();
        assert_eq!(session.hosting, HostingMode::SingleUser);

        // And the stamp is load-bearing, not decorative: a rebuild on the
        // sidecar manager re-derives the capability the placeholder would
        // have denied. Cleared first so the assertion cannot be satisfied by
        // values riding along inside `PersistedGameState` (both fields are
        // `#[serde(default)]`, not `skip`).
        let pc = session.player_count;
        session.state.debug_mode = false;
        session.state.debug_permitted.clear();
        session.rebuild_pregame_state(pc);
        assert!(session.state.debug_mode);
        assert_eq!(session.state.debug_permitted, BTreeSet::from([PlayerId(0)]));
    }

    /// Serialize through JSON exactly as `persist.rs` writes to disk, so the
    /// "the capability rides in the blob" premise is measured rather than
    /// asserted from the `#[serde(default)]` attributes.
    fn round_trip_through_disk(session: &GameSession, db: &CardDatabase) -> GameSession {
        let json = serde_json::to_string(&session.to_persisted()).unwrap();
        let persisted: crate::persist::PersistedSession = serde_json::from_str(&json).unwrap();
        GameSession::from_persisted(persisted, db)
    }

    #[test]
    fn restore_drops_a_sidecar_blobs_debug_capability_on_a_shared_server() {
        // The `hosting` re-stamp blocks re-DERIVATION only. `debug_mode` and
        // `debug_permitted` are `#[serde(default)]` (not `skip`) and
        // `to_persisted` captures the whole `GameState`, so they arrive
        // already set and `rebuild_pregame_state` never runs on the restore
        // path to re-examine them.
        let db = engine::database::CardDatabase::default();
        let mut sidecar = SessionManager::single_user(Duration::from_secs(60));
        let code = single_ai_opponent_game(&mut sidecar);

        let origin = sidecar.sessions.get(&code).unwrap();
        // Premise 1: the sidecar really granted it, so a cleared result below
        // cannot come from the blob never having had the capability.
        assert!(origin.state.debug_mode);
        assert_eq!(origin.state.debug_permitted, BTreeSet::from([PlayerId(0)]));
        // Premise 2: the grant came from the hosting mode, not the sandbox
        // format flag — the flag is what legitimately travels with the blob,
        // and if it were set here the session would stay entitled.
        assert!(!origin.state.format_config.allow_debug_actions);

        let restored = round_trip_through_disk(origin, &db);
        // Premise 3: both fields genuinely survive a disk round trip. If this
        // ever goes false the assertions below become vacuous.
        assert!(restored.state.debug_mode);
        assert_eq!(
            restored.state.debug_permitted,
            BTreeSet::from([PlayerId(0)])
        );

        let mut shared = SessionManager::new();
        shared.restore_session(restored);
        let session = shared.sessions.get(&code).unwrap();
        assert_eq!(session.hosting, HostingMode::Shared);
        assert!(
            !session.state.debug_mode,
            "a --single-user sidecar's capability must not survive into a shared server"
        );
        assert!(
            session.state.debug_permitted.is_empty(),
            "seat 0 would otherwise stay past the handle_action debug gate"
        );
    }

    #[test]
    fn restore_keeps_a_sandbox_games_capability_including_its_revocations() {
        // The paired positive, and the guard against clearing unconditionally:
        // `allow_debug_actions` is a property of the GAME and travels with the
        // blob, so a sandbox game is still a sandbox game after a restart.
        // Values are kept VERBATIM rather than re-seeded — an explicit
        // `RevokeDebugPermission` is game state, and re-deriving would
        // silently reinstate the revoked seat.
        let db = engine::database::CardDatabase::default();
        let mut origin_mgr = SessionManager::new();
        let (code, _token) = create_sandbox_game(&mut origin_mgr);
        let origin = origin_mgr.sessions.get_mut(&code).unwrap();
        assert_eq!(
            origin.state.debug_permitted,
            BTreeSet::from([PlayerId(0), PlayerId(1)]),
            "premise: sandbox seeds every seat"
        );
        origin.state.debug_permitted.remove(&PlayerId(1));

        let restored = round_trip_through_disk(origin, &db);
        let mut shared = SessionManager::new();
        shared.restore_session(restored);
        let session = shared.sessions.get(&code).unwrap();
        assert!(session.state.debug_mode);
        assert_eq!(
            session.state.debug_permitted,
            BTreeSet::from([PlayerId(0)]),
            "seat 0 keeps its grant and seat 1 stays revoked"
        );
    }

    #[test]
    fn restore_keeps_the_sidecars_capability_when_it_resumes_its_own_game() {
        // The production restore direction: the desktop sidecar resuming a
        // suspended game. `HostingMode::SingleUser` is the second entitlement,
        // so nothing is dropped here. Fails against a restore that clears
        // whenever `allow_debug_actions` is false.
        let db = engine::database::CardDatabase::default();
        let mut origin_mgr = SessionManager::single_user(Duration::from_secs(60));
        let code = single_ai_opponent_game(&mut origin_mgr);
        let restored = round_trip_through_disk(origin_mgr.sessions.get(&code).unwrap(), &db);

        let mut sidecar = SessionManager::single_user(Duration::from_secs(60));
        sidecar.restore_session(restored);
        let session = sidecar.sessions.get(&code).unwrap();
        assert!(session.state.debug_mode);
        assert_eq!(session.state.debug_permitted, BTreeSet::from([PlayerId(0)]));
    }

    // CR 107.1c: "remove any number of counters" — a human's intermediate submit
    // ("remove 2 of 3") is not one of the coarse AI candidates (remove-none /
    // remove-all), but the engine validates the full legal space directly.
    #[test]
    fn remove_counters_intermediate_submit_is_validated_by_engine() {
        use engine::types::ability::{
            Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
        };
        use engine::types::counter::CounterType;
        use engine::types::game_state::{CounterRemoveChoice, GameState, WaitingFor};
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token) = mgr.create_game(make_deck());

        let mut state = GameState::new_two_player(7);
        let bearer = engine::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bearer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bearer)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 3);
        let pending = ResolvedAbility::new(
            Effect::RemoveCounter {
                counter_type: None,
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: -1 }),
                target: TargetFilter::SelfRef,
            },
            vec![TargetRef::Object(bearer)],
            bearer,
            PlayerId(0),
        );
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::RemoveCountersChoice {
            player: PlayerId(0),
            source_id: bearer,
            counter_type: None,
            available: vec![(CounterType::Plus1Plus1, 3)],
            pending_effect: Box::new(pending),
        };
        mgr.sessions.get_mut(&code).unwrap().state = state;

        // Discriminating: this "remove 2 of 3" submit is absent from the coarse
        // candidate set ({[], remove-all}) but is legal under the engine's
        // structural validation.
        let intermediate_removal = GameAction::ChooseCountersToRemove {
            selections: vec![CounterRemoveChoice {
                counter_type: CounterType::Plus1Plus1,
                count: 2,
            }],
        };
        let session = mgr.sessions.get(&code).unwrap();
        let (candidates, _, _) = engine_legal_actions_full(&session.state);
        assert!(
            !candidates.contains(&intermediate_removal),
            "the intermediate removal must be absent from coarse candidates"
        );
        let result = mgr.handle_action(&code, &token, intermediate_removal);

        assert!(
            result.is_ok(),
            "intermediate removal must be accepted, not rejected as illegal: {result:?}"
        );
        let removed_to = mgr.sessions.get(&code).unwrap().state.objects[&bearer]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(removed_to, 1, "exactly 2 of 3 +1/+1 counters removed");
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
            state_revision: 0,
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
            // This test asserts cEDH bracket validation, not capability.
            hosting: HostingMode::Shared,
            player_count: pc as u8,
            ai_seats: [ai_pid].into_iter().collect(),
            ai_configs: [(ai_pid, cedh_config)].into_iter().collect(),
            ai_session: None,
            lobby_meta: None,
            game_started: false,
            start_when_full: true,
            ranked: false,
            start_events: Vec::new(),
            pending_takeback: None,
            takeback_history: VecDeque::new(),
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
            enter_tapped: false,
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

    /// CR 103.5: mulligan bottoming allows the selected hand cards in any
    /// order. The server forwards that order to the engine-owned action
    /// admission and `handle_mulligan_bottom` preserves it at library bottom.
    #[test]
    fn reordered_mulligan_bottom_selection_is_accepted_not_rejected_as_illegal() {
        use engine::game::zones::create_object;
        use engine::types::game_state::{
            MulliganDecisionEntry, MulliganDecisionPhase, PendingMulliganAction,
        };
        use engine::types::identifiers::{CardId, ObjectId};
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        let bottom_player = PlayerId(if session.state.active_player == PlayerId(0) {
            1
        } else {
            0
        });
        let token = if bottom_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };

        let mut hand = Vec::new();
        for i in 0..2 {
            let id = create_object(
                &mut session.state,
                CardId(3000 + i),
                bottom_player,
                format!("Bottom Card {i}"),
                Zone::Hand,
            );
            hand.push(id);
        }
        let (a, b): (ObjectId, ObjectId) = (hand[0], hand[1]);
        session.state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: bottom_player,
                mulligan_count: 1,
                phase: MulliganDecisionPhase::BottomCards {
                    count: 2,
                    then: PendingMulliganAction::Keep,
                },
            }],
            free_first_mulligan: false,
        };

        let token = token.to_string();
        let result =
            mgr.handle_action(&code, &token, GameAction::SelectCards { cards: vec![b, a] });
        assert!(
            result.is_ok(),
            "reordered mulligan bottom selection should be accepted, got: {result:?}"
        );

        let session = mgr.sessions.get(&code).unwrap();
        let player_idx = bottom_player.0 as usize;
        let hand_after: Vec<ObjectId> = session.state.players[player_idx]
            .hand
            .iter()
            .copied()
            .collect();
        assert!(!hand_after.contains(&a));
        assert!(!hand_after.contains(&b));
        let library_after: Vec<ObjectId> = session.state.players[player_idx]
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(&library_after[library_after.len() - 2..], &[b, a]);
    }

    /// A duplicate card id must not be able to satisfy a multi-card bottoming
    /// obligation — `[b, b]` for a 2-card obligation has the right length but
    /// only ever moves one physical card, leaving the other owed card
    /// stranded in hand. `validate_bottom_selection` (mulligan.rs) must reject
    /// it before anything is moved; hand, library, and the pending obligation
    /// must all be left exactly as they were.
    #[test]
    fn duplicate_mulligan_bottom_selection_is_rejected() {
        use engine::game::zones::create_object;
        use engine::types::game_state::{
            MulliganDecisionEntry, MulliganDecisionPhase, PendingMulliganAction,
        };
        use engine::types::identifiers::{CardId, ObjectId};
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        let bottom_player = PlayerId(if session.state.active_player == PlayerId(0) {
            1
        } else {
            0
        });
        let token = if bottom_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };

        let mut hand = Vec::new();
        for i in 0..2 {
            let id = create_object(
                &mut session.state,
                CardId(3100 + i),
                bottom_player,
                format!("Dup Bottom Card {i}"),
                Zone::Hand,
            );
            hand.push(id);
        }
        let (a, b): (ObjectId, ObjectId) = (hand[0], hand[1]);
        let pending_before = vec![MulliganDecisionEntry {
            player: bottom_player,
            mulligan_count: 1,
            phase: MulliganDecisionPhase::BottomCards {
                count: 2,
                then: PendingMulliganAction::Keep,
            },
        }];
        session.state.waiting_for = WaitingFor::MulliganDecision {
            pending: pending_before.clone(),
            free_first_mulligan: false,
        };

        let token = token.to_string();
        let result =
            mgr.handle_action(&code, &token, GameAction::SelectCards { cards: vec![b, b] });
        assert!(
            result.is_err(),
            "duplicate mulligan bottom selection must be rejected, got: {result:?}"
        );

        let session = mgr.sessions.get(&code).unwrap();
        assert_eq!(
            session.state.waiting_for,
            WaitingFor::MulliganDecision {
                pending: pending_before,
                free_first_mulligan: false,
            },
            "pending obligation must be unchanged after a rejected selection"
        );
        let player_idx = bottom_player.0 as usize;
        let hand_after: Vec<ObjectId> = session.state.players[player_idx]
            .hand
            .iter()
            .copied()
            .collect();
        assert!(hand_after.contains(&a));
        assert!(hand_after.contains(&b));
        let library_after: Vec<ObjectId> = session.state.players[player_idx]
            .library
            .iter()
            .copied()
            .collect();
        assert!(!library_after.contains(&a));
        assert!(!library_after.contains(&b));
    }

    /// The Tiny Leaders format extension's forced opening-hand bottoming (for
    /// example, extra commanders) is likewise an order-preserving selection;
    /// `handle_opening_hand_bottom` validates and places the submitted order.
    #[test]
    fn reordered_opening_hand_bottom_selection_is_accepted_not_rejected_as_illegal() {
        use engine::game::zones::create_object;
        use engine::types::game_state::{MulliganBottomEntry, OpeningHandBottomReason};
        use engine::types::identifiers::{CardId, ObjectId};
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        let bottom_player = PlayerId(if session.state.active_player == PlayerId(0) {
            1
        } else {
            0
        });
        let token = if bottom_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };

        let mut hand = Vec::new();
        for i in 0..2 {
            let id = create_object(
                &mut session.state,
                CardId(4000 + i),
                bottom_player,
                format!("Opening Bottom Card {i}"),
                Zone::Hand,
            );
            hand.push(id);
        }
        let (a, b): (ObjectId, ObjectId) = (hand[0], hand[1]);
        session.state.waiting_for = WaitingFor::OpeningHandBottomCards {
            pending: vec![MulliganBottomEntry {
                player: bottom_player,
                count: 2,
            }],
            reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
        };

        let token = token.to_string();
        let result =
            mgr.handle_action(&code, &token, GameAction::SelectCards { cards: vec![b, a] });
        assert!(
            result.is_ok(),
            "reordered opening-hand bottom selection should be accepted, got: {result:?}"
        );

        let session = mgr.sessions.get(&code).unwrap();
        let player_idx = bottom_player.0 as usize;
        let hand_after: Vec<ObjectId> = session.state.players[player_idx]
            .hand
            .iter()
            .copied()
            .collect();
        assert!(!hand_after.contains(&a));
        assert!(!hand_after.contains(&b));
        let library_after: Vec<ObjectId> = session.state.players[player_idx]
            .library
            .iter()
            .copied()
            .collect();
        assert_eq!(&library_after[library_after.len() - 2..], &[b, a]);
    }

    /// Same duplicate-rejection guarantee as
    /// `duplicate_mulligan_bottom_selection_is_rejected`, for the
    /// `OpeningHandBottomCards` sibling state (e.g. Tiny Leaders forced
    /// pregame bottoming) — `[b, b]` must not satisfy a 2-card obligation.
    #[test]
    fn duplicate_opening_hand_bottom_selection_is_rejected() {
        use engine::game::zones::create_object;
        use engine::types::game_state::{MulliganBottomEntry, OpeningHandBottomReason};
        use engine::types::identifiers::{CardId, ObjectId};
        use engine::types::zones::Zone;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();

        let session = mgr.sessions.get_mut(&code).unwrap();
        let bottom_player = PlayerId(if session.state.active_player == PlayerId(0) {
            1
        } else {
            0
        });
        let token = if bottom_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };

        let mut hand = Vec::new();
        for i in 0..2 {
            let id = create_object(
                &mut session.state,
                CardId(4100 + i),
                bottom_player,
                format!("Dup Opening Bottom Card {i}"),
                Zone::Hand,
            );
            hand.push(id);
        }
        let (a, b): (ObjectId, ObjectId) = (hand[0], hand[1]);
        let pending_before = vec![MulliganBottomEntry {
            player: bottom_player,
            count: 2,
        }];
        session.state.waiting_for = WaitingFor::OpeningHandBottomCards {
            pending: pending_before.clone(),
            reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
        };

        let token = token.to_string();
        let result =
            mgr.handle_action(&code, &token, GameAction::SelectCards { cards: vec![b, b] });
        assert!(
            result.is_err(),
            "duplicate opening-hand bottom selection must be rejected, got: {result:?}"
        );

        let session = mgr.sessions.get(&code).unwrap();
        assert_eq!(
            session.state.waiting_for,
            WaitingFor::OpeningHandBottomCards {
                pending: pending_before,
                reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
            },
            "pending obligation must be unchanged after a rejected selection"
        );
        let player_idx = bottom_player.0 as usize;
        let hand_after: Vec<ObjectId> = session.state.players[player_idx]
            .hand
            .iter()
            .copied()
            .collect();
        assert!(hand_after.contains(&a));
        assert!(hand_after.contains(&b));
        let library_after: Vec<ObjectId> = session.state.players[player_idx]
            .library
            .iter()
            .copied()
            .collect();
        assert!(!library_after.contains(&a));
        assert!(!library_after.contains(&b));
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

        let legal_non_candidate = GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: vec![(blocker, 5)],
            trample_damage: 0,
            controller_damage: 0,
        };
        let (candidates, _, _) = engine_legal_actions_full(&session.state);
        assert!(
            !candidates.contains(&legal_non_candidate),
            "the legal keep-on-blocker split must be absent from the greedy candidates"
        );
        let history_before_illegal = session.takeback_history.len();

        // Illegal division first: wrong total (4 != 5). It reaches apply() and
        // is rejected by the engine, proving candidate removal does not weaken
        // structural validation.
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
        assert_eq!(
            mgr.sessions.get(&code).unwrap().takeback_history.len(),
            history_before_illegal,
            "rejected engine action must not add a takeback snapshot"
        );

        // Legal-but-non-enumerated division: keep all 5 on the blocker, trample
        // nothing through (CR 702.19b). Pre-fix this was rejected as illegal.
        let legal = mgr.handle_action(&code, &token, legal_non_candidate);
        assert!(
            legal.is_ok(),
            "keep-on-blocker combat damage division (CR 702.19b) should be accepted, got: {legal:?}"
        );

        // The defending player took no trample damage — proof the controller's
        // declined-excess division resolved as submitted (life unchanged at 20).
        let session = mgr.sessions.get(&code).unwrap();
        assert_eq!(session.state.players[defending_player.0 as usize].life, 20);
    }

    /// CR 508.1a–e: each attacker independently chooses a defender. The
    /// candidate enumerator only samples that combinatorial space (it lists
    /// single-target alpha strikes), so a split declaration is legal but absent
    /// from it — the session must admit it and let `apply()` validate. Guards
    /// against reintroducing a candidate-membership legality gate at the
    /// session boundary, while CR 508.1a duplicate rejection stays enforced by
    /// `validate_attackers`.
    #[test]
    fn split_attack_declaration_is_admitted_and_validated_by_the_engine() {
        use engine::game::combat::AttackTarget;
        use engine::game::zones::create_object;
        use engine::types::card_type::CoreType;
        use engine::types::identifiers::CardId;

        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game_n_players(
            make_deck(),
            "Host".to_string(),
            None,
            3,
            MatchConfig::default(),
            Some(FormatConfig::standard()),
        );
        let _ = mgr.join_game(&code, make_deck()).unwrap();
        let _ = mgr.join_game(&code, make_deck()).unwrap();

        let attacks = {
            let session = mgr.sessions.get_mut(&code).unwrap();
            let first = create_object(
                &mut session.state,
                CardId(4000),
                PlayerId(0),
                "First attacker".to_string(),
                Zone::Battlefield,
            );
            let second = create_object(
                &mut session.state,
                CardId(4001),
                PlayerId(0),
                "Second attacker".to_string(),
                Zone::Battlefield,
            );
            for id in [first, second] {
                session
                    .state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Creature);
            }

            session.state.active_player = PlayerId(0);
            session.state.priority_player = PlayerId(0);
            session.state.phase = Phase::DeclareAttackers;
            session.state.waiting_for = WaitingFor::DeclareAttackers {
                player: PlayerId(0),
                valid_attacker_ids: vec![first, second],
                valid_attack_targets: vec![
                    AttackTarget::Player(PlayerId(1)),
                    AttackTarget::Player(PlayerId(2)),
                ],
                valid_attack_targets_by_attacker: None,
                attacker_constraints: Default::default(),
            };

            vec![
                (first, AttackTarget::Player(PlayerId(1))),
                (second, AttackTarget::Player(PlayerId(2))),
            ]
        };
        let action = GameAction::DeclareAttackers {
            attacks: attacks.clone(),
            bands: vec![],
        };

        let enumerated = engine::ai_support::legal_actions(&mgr.sessions[&code].state);
        assert!(
            !enumerated.contains(&action),
            "the finite candidate set intentionally omits this split attack: {enumerated:?}"
        );

        // The bypass must not weaken validation: a malformed freeform declaration
        // reaches the engine and is rejected there rather than by candidate lookup.
        let duplicate = mgr.handle_action(
            &code,
            &token0,
            GameAction::DeclareAttackers {
                attacks: vec![
                    attacks[0],
                    (attacks[0].0, AttackTarget::Player(PlayerId(2))),
                ],
                bands: vec![],
            },
        );
        assert!(
            matches!(duplicate, Err(ref error) if error.starts_with("Engine error:")),
            "a duplicate attacker must be rejected by the engine, got: {duplicate:?}"
        );

        let result = mgr.handle_action(&code, &token0, action);
        assert!(
            result.is_ok(),
            "a legal split attack must be validated by apply(), got: {result:?}"
        );
        let attackers = &mgr.sessions[&code].state.combat.as_ref().unwrap().attackers;
        assert_eq!(attackers.len(), 2);
        assert!(attackers.iter().any(|attacker| {
            attacker.object_id == attacks[0].0
                && attacker.attack_target == AttackTarget::Player(PlayerId(1))
        }));
        assert!(attackers.iter().any(|attacker| {
            attacker.object_id == attacks[1].0
                && attacker.attack_target == AttackTarget::Player(PlayerId(2))
        }));
    }
}
