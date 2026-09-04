use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine::ai_support::{auto_pass_recommended, legal_actions_full as engine_legal_actions_full};
use engine::database::legality::{validate_cedh_bracket, CedhBracketError};
use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckPayload, PlayerDeckPayload};
use engine::game::engine::{
    apply_with_rejection,
    resume_restored_stack_automation as resume_engine_restored_stack_automation, start_game,
    RestoredStackAutomationOutcome, RestoredStackAutomationPresentation,
};
use engine::game::interaction::{bind_interaction_authority, submit_interaction_with_rejection};
use engine::game::match_flow::apply_trusted_match_forfeit;
use engine::game::public_state::{
    bump_state_revision, finalize_public_state, mark_public_state_all_dirty,
};
use engine::game::{
    create_debug_cards_with_rejection, debug_card_entry_source, load_and_hydrate_decks,
    rehydrate_game_from_card_db_with_finalization, CardDbRehydrationFinalization,
    DebugCardCreateRequest,
};
use engine::types::action_rejection::ActionRejection;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, PersistedGameState};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{InteractionSessionId, InteractionSubmission};
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::match_config::MatchForfeitCause;
use engine::types::player::PlayerId;
use phase_ai::auto_play::AiActionsStop;
use phase_ai::config::{AiConfig, AiDifficulty, Platform};
use phase_ai::session::AiSession;
use rand::{Rng, SeedableRng};
use seat_reducer::types::{seat_team_info, DeckChoice, SeatDelta, SeatKind, SeatState};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::filter::filter_state_for_player;
use crate::game_log::GameFileCache;
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

/// Distinguishes an engine-owned, viewer-safe action refusal from an
/// operational session failure. Only the former is eligible for a structured
/// `ActionRejected` frame at the WebSocket boundary.
#[derive(Debug)]
pub enum SessionActionError {
    Operational(String),
    /// A request-only lifecycle operation cannot proceed. Game-action-shaped
    /// attempts use [`Self::Rejected`] so their pending client promises settle
    /// on correlated action-response frames.
    RequestRejected(String),
    Rejected(ActionRejection),
}

impl From<String> for SessionActionError {
    fn from(value: String) -> Self {
        Self::Operational(value)
    }
}

impl SessionActionError {
    fn into_legacy_reason(self) -> String {
        match self {
            Self::Operational(reason) | Self::RequestRejected(reason) => reason,
            Self::Rejected(rejection) => rejection.message,
        }
    }
}

/// The server-visible result of driving the native AI after an authoritative
/// transition. A non-empty `failure` is terminal for the AI driver, but does
/// not discard transitions that were already committed before that failure.
#[derive(Debug)]
pub struct AiRunOutcome {
    pub transitions: Vec<RevisionedActionResult>,
    pub failure: Option<AiDriverFailure>,
    /// The durable terminal record, when this invocation observed or created
    /// an AI driver failure. Transports must send the final state frames before
    /// publishing this record.
    pub fault: Option<AiDriverFault>,
}

/// Internal result of one uninterrupted AI decision batch. Keeping the exact
/// stop reason until [`GameSession::run_ai`] has finished the engine-owned
/// stack-resolution session is necessary: a safety cap is terminal only when
/// an AI submitter is still eligible in the resulting state.
struct AiActionBatch {
    transitions: Vec<RevisionedActionResult>,
    stop: AiActionsStop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiDriverFailure {
    MissingAiConfig { player: PlayerId },
    ChooseActionNone { player: PlayerId },
    ApplyFailed { player: PlayerId, error: String },
    ActionSafetyCapReached { limit: usize },
}

/// Durable, server-authored terminal state for the native AI driver.
///
/// This is deliberately separate from `GameState`: it is a transport/runtime
/// failure, not a Magic game-rule result. The revision is allocated once when
/// the fault is recorded, so a reconnect can prove it has received the final
/// state snapshot before showing the fault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AiDriverFault {
    pub id: u64,
    /// The last state revision a client must have applied before rendering
    /// this out-of-band driver failure. The session revision is then bumped
    /// separately as a durable persistence fence.
    pub after_state_revision: u64,
    pub cause: AiDriverFailure,
}

impl AiDriverFailure {
    pub fn message(&self) -> String {
        match self {
            Self::MissingAiConfig { player } => {
                format!(
                    "Native AI driver is missing configuration for player {}.",
                    player.0
                )
            }
            Self::ChooseActionNone { player } => {
                format!(
                    "Native AI could not choose an action for player {}.",
                    player.0
                )
            }
            Self::ApplyFailed { player, error } => {
                format!(
                    "Native AI action for player {} was rejected: {error}",
                    player.0
                )
            }
            Self::ActionSafetyCapReached { limit } => {
                format!("Native AI stopped after its {limit}-action safety limit.")
            }
        }
    }
}

/// Stable identity for one lifetime of a Full authoritative session.
///
/// A game code can be retired and later reused. Persisting the generation
/// alongside it prevents delayed saves from an earlier lifetime from
/// overwriting the newer session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FullSessionKey {
    pub game_code: String,
    pub generation: u64,
}

/// A durable Full-server snapshot fenced by both session generation and the
/// session-local mutation revision. `activation_epoch` is populated only by a
/// single-user activation; shared-server sessions intentionally have none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullPersistSnapshot {
    pub key: FullSessionKey,
    pub mutation_revision: u64,
    pub activation_epoch: Option<u64>,
    pub persisted: PersistedSession,
}

/// Runtime-only persistence authority for one lifetime of a Full session.
///
/// The key fences delayed writes from a recycled game code. The optional
/// activation epoch is the single-user singleton fence and is stamped only by
/// the persistence owner; neither value is trusted from a game snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullRuntime {
    pub key: FullSessionKey,
    pub activation_epoch: Option<u64>,
}

/// Outcome of a generation-fenced Full-server persistence operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FullPersistDisposition {
    Applied,
    SupersededOrRetired,
    NotCurrentActivation,
}

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

/// Server-owned result of explicitly resuming persisted stack automation.
///
/// Persistence reconstruction remains pure: callers invoke this only after the
/// restored session has been hydrated, finalized, bound to fresh interaction
/// authority, and stamped with this process's hosting policy. The engine keeps
/// the complete event batch for server bookkeeping; the transport receives
/// only its bounded presentation plus a freshly-derived snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredStackAutomationResume {
    /// Allocated exactly once when the engine progressed or repaired a saved
    /// automation session. A no-op leaves the restored revision unchanged.
    pub state_revision: Option<u64>,
    /// Engine-authored, bounded transport presentation of the resume outcome.
    pub presentation: RestoredStackAutomationPresentation,
    /// Recomputed only for a state-changing resume. Its state and legal-action
    /// projections are the authoritative payload for the subsequent broadcast.
    pub broadcast: Option<BroadcastSnapshot>,
}

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
    /// Per-game JSON debug log sink (issue #7978), copied from the owning
    /// `SessionManager` at creation and re-stamped, never deserialized, at
    /// `SessionManager::restore_session` — same lifecycle as `hosting`
    /// below, for the same reason: a runtime handle, not session state.
    pub game_log: Arc<GameFileCache>,
    /// Server-issued durable identity for a Full session lifetime. This is
    /// stamped by phase-server after persistence binds the newly-created
    /// session; it is not trusted from the serialized game blob.
    pub full_runtime: Option<FullRuntime>,
    /// Monotonic server-authored revision of the current authoritative state.
    /// Read-only snapshots reuse this value; mutators advance it before their
    /// per-viewer views are captured for transport.
    pub state_revision: u64,
    /// A native AI-driver failure permanently closes this session to further
    /// game mutations. Persist it so restart/reconnect cannot turn an
    /// actionable AI priority into a silent freeze again.
    pub ai_driver_fault: Option<AiDriverFault>,
    /// Monotonic per-session identifier for durable driver faults.
    pub next_ai_driver_fault_id: u64,
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
    /// **Read fence.** This field has exactly three readers:
    ///
    /// 1. `seed_debug_capability`, reachable only via `rebuild_pregame_state`,
    ///    which is called only from `start_game` and from `apply_seat_delta`.
    /// 2. `takeback::record_turn_rewind_point`, via
    ///    [`GameSession::observe_transition`] — reached only from the
    ///    authoritative transition handlers, all of which require a started
    ///    game. Deliberately not a count: the tally here has been wrong before,
    ///    and the property that matters is "every caller is post-start", which
    ///    no number states. `resume_restored_stack_automation` reaches this
    ///    reader only after `SessionManager::restore_session` has re-stamped
    ///    this field for the current process.
    /// 3. `takeback::offers_turn_rewind`, via `GameSession::rewind_options`
    ///    and `GameSession::request_takeback` — likewise post-start.
    ///
    /// None of them runs during the lobby re-registration window in which a
    /// restored session still holds the `from_persisted` placeholder
    /// (`HostingMode::Shared`), and that placeholder can only *under*-grant:
    /// readers 2 and 3 would decline to capture or publish, never over-offer.
    /// **Any new read must either sit behind `rebuild_pregame_state` or be
    /// reachable only after the restore stamp.**
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
    /// Rolling buffer of authoritative states, each one the post-state of a
    /// transition that came to rest at the start of the turn it announced.
    /// Strictly increasing in `turn_number` **within a game**, capped at
    /// `MAX_TURN_REWIND_HISTORY`, and only ever populated on a
    /// `HostingMode::SingleUser` sidecar (`takeback::offers_turn_rewind`).
    /// Deliberately not persisted, matching `takeback_history`.
    pub turn_rewind_history: VecDeque<GameState>,
    /// The `game_number` the rewind rings above were last observed at. A Bo3
    /// match assigns `turn_number = 1` again at each game's start, so both
    /// rings are retired when this stops matching the post-state's own
    /// `game_number` — see `crate::takeback::GameSession::observe_transition`.
    pub rewind_game_number: u8,
}

impl GameSession {
    pub fn ai_driver_fault(&self) -> Option<&AiDriverFault> {
        self.ai_driver_fault.as_ref()
    }

    pub(crate) fn reject_if_ai_driver_faulted(&self) -> Result<(), String> {
        self.ai_driver_fault
            .as_ref()
            .map(|fault| {
                format!(
                    "Native AI driver fault {}: {}",
                    fault.id,
                    fault.cause.message()
                )
            })
            .map_or(Ok(()), Err)
    }

    fn record_ai_driver_fault(&mut self, cause: AiDriverFailure) -> AiDriverFault {
        if let Some(fault) = &self.ai_driver_fault {
            return fault.clone();
        }
        let fault = AiDriverFault {
            id: self.next_ai_driver_fault_id,
            after_state_revision: self.state_revision,
            cause,
        };
        // Allocate a fresh persistence revision without inventing a state
        // transition for clients to render. This lets the database fence the
        // durable fault while delivery remains ordered after the last actual
        // state frame.
        self.advance_state_revision();
        self.next_ai_driver_fault_id = self.next_ai_driver_fault_id.saturating_add(1);
        self.ai_driver_fault = Some(fault.clone());
        fault
    }
    /// Allocates the revision for one completed authoritative state transition.
    pub fn advance_state_revision(&mut self) -> u64 {
        self.state_revision = self.state_revision.saturating_add(1);
        self.state_revision
    }

    /// Builds the only persistence payload for an active Full runtime.
    /// Snapshot generation and revision therefore cannot be supplied by a
    /// transport caller independently of the authoritative session.
    pub fn full_persist_snapshot(&self) -> Option<FullPersistSnapshot> {
        let runtime = self.full_runtime.as_ref()?;
        Some(FullPersistSnapshot {
            key: runtime.key.clone(),
            mutation_revision: self.state_revision,
            activation_epoch: runtime.activation_epoch,
            persisted: self.to_persisted(),
        })
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
            // (rejected above as ActionNotAllowed) remain
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
        if self.ai_driver_fault.is_some() {
            return;
        }
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
        // A faulted game is never restarted in place: doing so would create a
        // new playable state behind the durable terminal fault record.
        if self.ai_driver_fault.is_some() {
            return Ok(());
        }
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
        // Per-game JSON debug log (issue #7978): "Game started"/"Turn 1" rows
        // — otherwise every game's events.jsonl would start mid-game.
        self.game_log
            .write_game_log_entries(&self.game_code, &result.log_entries);
        // Deliberately NOT a turn-rewind capture site, unlike the four
        // transition handlers. These events never reach a transition handler,
        // and turn 1's opening state sits adjacent to the mulligan flow —
        // rewinding to it would land a player somewhere the rollback machine
        // was never designed to resume from. An exclusion, not an omission.
        self.start_events = result.events;
        self.game_started = true;
        self.advance_state_revision();
        self.ai_session = Some(AiSession::arc_from_game(&self.state));
        self.lobby_meta = None;
        Ok(())
    }

    /// Run AI actions and return per-action broadcast data.
    ///
    /// Each entry is one authoritative transition: raw state snapshot, events,
    /// legal actions, and log entries. Stack automation remains in that ordinary
    /// engine transition stream rather than using a separate Resolve All frame.
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
    pub fn run_ai(&mut self) -> AiRunOutcome {
        if let Some(fault) = self.ai_driver_fault.clone() {
            return AiRunOutcome {
                transitions: Vec::new(),
                failure: None,
                fault: Some(fault),
            };
        }
        if self.ai_seats.is_empty() || self.pending_takeback.is_some() {
            return AiRunOutcome {
                transitions: Vec::new(),
                failure: None,
                fault: None,
            };
        }

        let batch = self.run_ai_action_batch();
        let transitions = batch.transitions;
        let failure = match batch.stop {
            AiActionsStop::NoEligibleAiActor | AiActionsStop::ActionBudgetReached { .. } => None,
            AiActionsStop::MissingAiConfig { player } => {
                Some(AiDriverFailure::MissingAiConfig { player })
            }
            AiActionsStop::ChooseActionNone { player } => {
                Some(AiDriverFailure::ChooseActionNone { player })
            }
            AiActionsStop::ApplyFailed { player, error, .. } => {
                Some(AiDriverFailure::ApplyFailed {
                    player,
                    error: error.to_string(),
                })
            }
            AiActionsStop::ActionSafetyCapReached { limit } if self.ai_seat_can_act() => {
                Some(AiDriverFailure::ActionSafetyCapReached { limit })
            }
            AiActionsStop::ActionSafetyCapReached { .. } => None,
        };
        let fault = failure
            .clone()
            .map(|failure| self.record_ai_driver_fault(failure));
        AiRunOutcome {
            transitions,
            failure,
            fault,
        }
    }

    /// One uninterrupted run of AI decisions from the current state.
    fn run_ai_action_batch(&mut self) -> AiActionBatch {
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

        let stop = ai_results.stop;

        let transitions = ai_results
            .results
            .into_iter()
            .map(|r| {
                let (legal, spell_costs, by_object) = engine_legal_actions_full(&r.state);
                let auto_pass = auto_pass_recommended(&r.state, &legal);
                let revision = self.advance_state_revision();
                // Per AI result, which is exactly the granularity
                // `run_ai_actions` reports. This is what makes a turn that
                // elapses entirely inside AI play still produce a rewind
                // point; a rule that promoted entries out of
                // `takeback_history` could not, because `run_ai` pushes none.
                self.observe_transition(&r.events, &r.state);
                // Per-game JSON debug log (issue #7978): this is the single
                // AI-transition mint point every caller of `run_ai` funnels
                // through (takeback approve/reject, reconnect, game start,
                // Play-vs-AI start, and the AI follow-up after a human
                // action) — one hook here covers all of them, matching the
                // human-action hooks in `handle_action`/
                // `handle_interaction_with_rejection`.
                self.game_log
                    .write_game_log_entries(&self.game_code, &r.log_entries);
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
            .collect();

        AiActionBatch { transitions, stop }
    }

    fn ai_seat_can_act(&self) -> bool {
        acting_players(&self.state)
            .into_iter()
            .any(|player| self.ai_seats.contains(&player))
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
            ai_driver_fault: self.ai_driver_fault.clone(),
            next_ai_driver_fault_id: self.next_ai_driver_fault_id,
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
    ///
    /// The fresh seed also resets `rng_word_pos` — a `#[serde(default)]` field, not a skipped
    /// one. It is the saved high-water of the stream the old seed generated, and has no
    /// meaning against the new one.
    pub fn from_persisted(ps: PersistedSession, db: &CardDatabase) -> Result<Self, String> {
        let mut state = ps
            .state
            .prepare_for_restore(
                engine::types::game_state::PersistedRestoreFinalization::DeferUntilRehydrated,
            )
            .map_err(|error| error.to_string())?
            .finalize_after_rehydration(|state| {
                state
                    .format_config
                    .validate_for_player_count(ps.player_count)?;
                state
                    .format_config
                    .reject_unimplemented_range_of_influence()?;

                // Restore #[serde(skip)] fields before the engine computes the
                // first externally visible state from this snapshot.
                state.all_card_names = db.card_names().into();
                state.log_player_names = ps.display_names.clone();
                rehydrate_game_from_card_db_with_finalization(
                    state,
                    db,
                    CardDbRehydrationFinalization::Defer,
                );

                // Re-seed RNG with fresh randomness (stale rng_seed would produce
                // deterministic sequences identical across all restored games)
                let fresh_seed: u64 = rand::rng().random();
                state.rng_seed = fresh_seed;
                state.rng = rand_chacha::ChaCha20Rng::seed_from_u64(fresh_seed);
                // A fresh stream starts at word 0, so the saved high-water — which indexes into the
                // OLD keystream and is meaningless against this one — has to go with the old seed.
                state.rng_word_pos = 0;
                Ok(())
            })
            .map_err(|error| error.to_string())?;
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

        let rewind_game_number = state.game_number;

        Ok(GameSession {
            game_code: ps.game_code,
            full_runtime: None,
            state_revision: ps.state_revision,
            ai_driver_fault: ps.ai_driver_fault,
            next_ai_driver_fault_id: ps.next_ai_driver_fault_id.max(1),
            state,
            player_tokens: ps.player_tokens,
            connected: vec![false; pc],
            decks: vec![None; pc],
            display_names: ps.display_names,
            reservations: HashMap::new(),
            timer_seconds: ps.timer_seconds,
            // Placeholder, same rationale as `hosting` below: a runtime
            // handle, not persisted state, re-stamped by
            // `SessionManager::restore_session`. `Arc::default()` is a
            // disabled (no-op) sink, so nothing is under- or over-logged
            // between here and that stamp.
            game_log: Arc::default(),
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
            // Neither ring is persisted: a rollback offer is a live-session
            // affordance, not durable state.
            takeback_history: VecDeque::new(),
            turn_rewind_history: VecDeque::new(),
            rewind_game_number,
        })
    }

    /// Explicitly resumes a persisted stack automation session after the
    /// restore owner has finished attaching runtime authority.
    ///
    /// Generic [`Self::from_persisted`] intentionally does not call this: a
    /// decode is not an implicit priority pass. The restore owner invokes this
    /// once after it has supplied process-owned hosting policy, then broadcasts
    /// the returned bounded presentation with the recomputed snapshot.
    pub fn resume_restored_stack_automation(&mut self) -> RestoredStackAutomationResume {
        let resumed = resume_engine_restored_stack_automation(&mut self.state);
        let presentation = resumed.presentation.clone();
        if presentation.outcome == RestoredStackAutomationOutcome::Noop {
            return RestoredStackAutomationResume {
                state_revision: None,
                presentation,
                broadcast: None,
            };
        }

        // The engine's complete event batch stays server-internal. In
        // particular, a collapsed session can contain far more lifecycle
        // events than a transport frame may carry, while rewind bookkeeping
        // still needs to observe every one of them.
        let post_state = self.state.clone();
        self.observe_transition(&resumed.action_result().events, &post_state);
        // Per-game JSON debug log (issue #7978): stack automation replayed
        // after a server restart is exactly the post-crash scenario this log
        // exists to make debuggable — it must not be invisible here.
        self.game_log
            .write_game_log_entries(&self.game_code, &resumed.action_result().log_entries);
        let revision = self.advance_state_revision();
        let (legal_actions, spell_costs, by_object) = engine_legal_actions_full(&self.state);
        let auto_pass = auto_pass_recommended(&self.state, &legal_actions);

        RestoredStackAutomationResume {
            state_revision: Some(revision),
            presentation,
            broadcast: Some((post_state, legal_actions, auto_pass, spell_costs, by_object)),
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
    /// Per-game JSON debug log sink (issue #7978), stamped onto every session
    /// this manager creates or restores — same lifecycle as `hosting`.
    /// Disabled (no-op writes) unless the transport wires a real one in
    /// (`phase-server` does this once at startup, from `PHASE_LOG_DIR`).
    pub game_log: Arc<GameFileCache>,
}

impl SessionManager {
    /// A shared instance: other humans may join or spectate.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::default(),
            hosting: HostingMode::Shared,
            token_to_game: HashMap::new(),
            game_log: Arc::default(),
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
            game_log: Arc::default(),
        }
    }

    /// Create a new game session (2-player default). Returns (game_code, player_token).
    pub fn create_game(&mut self, deck: PlayerDeckPayload) -> (String, String) {
        self.create_game_n_players(deck, String::new(), None, 2, MatchConfig::default(), None)
            .expect("the standard format must be supported")
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
            .expect("the standard format must be supported")
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
    ) -> Result<(String, String), String> {
        let format_config = format_config.unwrap_or_else(FormatConfig::standard);
        format_config.validate_for_player_count(player_count)?;
        format_config.reject_unimplemented_range_of_influence()?;

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

        let mut state = GameState::new(format_config, player_count, rand::rng().random());
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

        let rewind_game_number = state.game_number;
        let session = GameSession {
            game_code: game_code.clone(),
            full_runtime: None,
            state_revision: 0,
            ai_driver_fault: None,
            next_ai_driver_fault_id: 1,
            state,
            player_tokens,
            connected,
            decks,
            display_names,
            reservations: HashMap::new(),
            timer_seconds,
            hosting: self.hosting,
            game_log: Arc::clone(&self.game_log),
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
            turn_rewind_history: VecDeque::new(),
            rewind_game_number,
        };

        self.token_to_game
            .insert(player_token.clone(), game_code.clone());
        self.sessions.insert(game_code.clone(), session);

        info!(game = %game_code, player_count, "game session created");

        Ok((game_code, player_token))
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

        session.reject_if_ai_driver_faulted()?;
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
    ) -> Result<(String, String), String> {
        let total_players = 1 + ai_requests.len() as u8;
        let (game_code, player_token) = self.create_game_n_players(
            host_deck,
            display_name,
            timer_seconds,
            total_players,
            match_config,
            format_config,
        )?;

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

        Ok((game_code, player_token))
    }

    /// Returns the exact mana sources automatic payment would use without
    /// changing the authenticated game session.
    pub fn preview_mana_payment(
        &self,
        game_code: &str,
        player_token: &str,
        action: &GameAction,
    ) -> Result<Vec<ObjectId>, String> {
        self.preview_mana_payment_with_rejection(game_code, player_token, action)
            .map_err(SessionActionError::into_legacy_reason)
    }

    /// Viewer-safe preview form for the Full transport.
    pub fn preview_mana_payment_with_rejection(
        &self,
        game_code: &str,
        player_token: &str,
        action: &GameAction,
    ) -> Result<Vec<ObjectId>, SessionActionError> {
        let session = self
            .sessions
            .get(game_code)
            .ok_or_else(|| format!("Game not found: {game_code}"))?;
        session.reject_if_ai_driver_faulted()?;
        let player = session
            .player_for_token(player_token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        engine::game::preview::preview_auto_payment_sources_with_rejection(
            &session.state,
            player,
            action,
        )
        .map_err(SessionActionError::Rejected)
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
        self.handle_action_with_card_db(game_code, player_token, action, None)
    }

    /// Handle a game action whose transport can resolve debug card names through
    /// its live card database. The engine owns materialization and entry; this
    /// boundary only resolves the player-entered card name into a card face.
    #[allow(clippy::type_complexity)]
    pub fn handle_action_with_card_db(
        &mut self,
        game_code: &str,
        player_token: &str,
        action: GameAction,
        card_db: Option<&CardDatabase>,
    ) -> Result<ActionResult, String> {
        self.handle_action_with_card_db_outcome(game_code, player_token, action, card_db)
            .map_err(SessionActionError::into_legacy_reason)
    }

    /// Rich form used by the Full WebSocket transport. Engine rejection DTOs
    /// remain distinct from session/database failures so the transport never
    /// has to infer meaning from a diagnostic string.
    #[allow(clippy::type_complexity)]
    pub fn handle_action_with_card_db_outcome(
        &mut self,
        game_code: &str,
        player_token: &str,
        action: GameAction,
        card_db: Option<&CardDatabase>,
    ) -> Result<ActionResult, SessionActionError> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {}", game_code))?;

        session.reject_if_ai_driver_faulted()?;

        let player = session
            .player_for_token(player_token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        // GH #1507: while a takeback request is awaiting approval, the
        // authoritative state must not move out from under it — a new
        // action here would either invalidate the snapshot the table is
        // voting on or silently discard the action once the rollback lands.
        // Require the table to resolve (approve/decline/cancel) first.
        if session.pending_takeback.is_some() {
            return Err(SessionActionError::Rejected(ActionRejection::new(
                engine::types::action_rejection::ActionRejectionCode::ActionNotAllowed,
            )));
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
        if let GameAction::Debug(debug_action) = &action {
            engine::game::preflight_debug_action_with_rejection(
                &session.state,
                player,
                debug_action,
            )
            .map_err(SessionActionError::Rejected)?;
        }

        // Grant/Revoke debug permission: host-only, and only meaningful in a
        // sandbox session. The host is always PlayerId(0). The host cannot
        // revoke their own permission (would leave nobody able to debug).
        const HOST_PLAYER: PlayerId = PlayerId(0);
        match &action {
            GameAction::GrantDebugPermission { .. } | GameAction::RevokeDebugPermission { .. } => {
                if !session.state.format_config.allow_debug_actions {
                    return Err(SessionActionError::Rejected(ActionRejection::new(
                        engine::types::action_rejection::ActionRejectionCode::ActionNotAllowed,
                    )));
                }
                if player != HOST_PLAYER {
                    return Err(SessionActionError::Rejected(ActionRejection::new(
                        engine::types::action_rejection::ActionRejectionCode::ActionNotAllowed,
                    )));
                }
                if let GameAction::RevokeDebugPermission {
                    player_id: target, ..
                } = &action
                {
                    if *target == HOST_PLAYER {
                        return Err(SessionActionError::Rejected(ActionRejection::new(
                            engine::types::action_rejection::ActionRejectionCode::ActionNotAllowed,
                        )));
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
        if matches!(&action, GameAction::Debug(debug_action) if debug_action.is_zero_count_create())
        {
            let (legal_actions, spell_costs, by_object) = engine_legal_actions_full(&session.state);
            let auto_pass = auto_pass_recommended(&session.state, &legal_actions);
            return Ok((
                session.state.clone(),
                Vec::new(),
                legal_actions,
                Vec::new(),
                auto_pass,
                spell_costs,
                by_object,
            ));
        }
        let debug_card_source = match &action {
            GameAction::Debug(DebugAction::CreateCard { card_name, .. }) => {
                let card_db = card_db.ok_or_else(|| {
                    "Debug::CreateCard requires a card database at the transport boundary"
                        .to_string()
                })?;
                let face = card_db
                    .get_face_by_name(card_name)
                    .ok_or_else(|| "Engine error: card not found in database".to_string())?;
                Some(debug_card_entry_source(card_db, face))
            }
            _ => None,
        };

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
        let result = match action {
            GameAction::Debug(DebugAction::CreateCard {
                owner,
                zone,
                count,
                attach_to,
                run_etb,
                nonlegendary,
                ..
            }) => {
                let result = create_debug_cards_with_rejection(
                    &mut session.state,
                    DebugCardCreateRequest {
                        actor: player,
                        source: debug_card_source
                            .expect("nonzero debug CreateCard source was bound before mutation"),
                        owner,
                        zone,
                        count,
                        attach_to,
                        run_etb,
                        nonlegendary,
                    },
                )
                .map_err(SessionActionError::Rejected)?;
                bump_state_revision(&mut session.state);
                mark_public_state_all_dirty(&mut session.state);
                finalize_public_state(&mut session.state);
                result
            }
            action => apply_with_rejection(&mut session.state, player, action)
                .map_err(SessionActionError::Rejected)?,
        };
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

        // Turn-rewind bookkeeping, deliberately OUTSIDE the `pre_action_state`
        // block above: an `is_actor_scoped_preference` action records no
        // takeback snapshot but still goes through `apply` and can auto-advance
        // into a new turn. The two rings are independent; coupling them would
        // lose exactly those boundaries. The post-state clone the broadcast
        // already needs is hoisted here so capture reads a local and cannot be
        // reordered into a use-after-move.
        let post_state = session.state.clone();
        session.observe_transition(&result.events, &post_state);

        // Per-game JSON debug log (issue #7978): written here, at the point
        // the engine's `GameLogEntry` rows are minted, not by each transport
        // call site — so every path that reaches this function (and
        // `handle_interaction_with_rejection`, and AI transitions via
        // `run_ai_action_batch`) is covered without remembering a hook per
        // caller.
        session
            .game_log
            .write_game_log_entries(&session.game_code, &result.log_entries);

        Ok((
            post_state,
            result.events,
            new_legal_actions,
            result.log_entries,
            auto_pass,
            spell_costs,
            by_object,
        ))
    }

    /// Apply one engine-authored interaction submission from a player.
    ///
    /// Deliberately shaped as the exact sibling of [`Self::handle_action`]: it
    /// returns the same [`ActionResult`], so the transport's broadcast path is
    /// shared rather than duplicated.
    ///
    /// The acting `PlayerId` is resolved from the join-token-authenticated
    /// session, never from the payload — the wire frame carries no actor field
    /// at all (`protocol.rs`: "The authenticated session, rather than the
    /// client, determines the actor"). `submit_interaction` then re-authorizes
    /// the actor against the interaction slot inside the engine, so a forged
    /// `interaction_id` belonging to another seat is rejected twice.
    ///
    /// There is deliberately no debug-capability gate here, unlike
    /// `handle_action`. `materialize_response` dispatches on
    /// `human_response_model`, and its `Choose` fallthrough sources actions from
    /// `actor_candidates` ->
    /// `ai_support::validated_candidate_actions_for_semantic_owner`. An
    /// engine-wide grep shows no production constructor of
    /// `GameAction::Debug`, `GrantDebugPermission`, or `RevokeDebugPermission`
    /// anywhere in that chain — only classification and ordering matches — so
    /// guarding here would be validation for a case that cannot occur. The
    /// engine test
    /// `published_interaction_choices_never_offer_a_debug_action_in_a_sandbox_game`
    /// (`crates/engine/tests/integration/interaction_contract.rs`) fails the day
    /// that stops being true.
    pub fn handle_interaction(
        &mut self,
        game_code: &str,
        player_token: &str,
        submission: InteractionSubmission,
    ) -> Result<ActionResult, String> {
        self.handle_interaction_with_rejection(game_code, player_token, submission)
            .map_err(SessionActionError::into_legacy_reason)
    }

    /// Rich interaction form for the authenticated Full transport.
    #[allow(clippy::type_complexity)]
    pub fn handle_interaction_with_rejection(
        &mut self,
        game_code: &str,
        player_token: &str,
        submission: InteractionSubmission,
    ) -> Result<ActionResult, SessionActionError> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {game_code}"))?;

        session.reject_if_ai_driver_faulted()?;

        let player = session
            .player_for_token(player_token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        // GH #1507: the authoritative state must not move while the table is
        // voting on a rollback. Same interlock, same reason, as `handle_action`.
        if session.pending_takeback.is_some() {
            return Err(SessionActionError::Rejected(ActionRejection::new(
                engine::types::action_rejection::ActionRejectionCode::ActionNotAllowed,
            )));
        }

        // Snapshot BEFORE `log_player_names` is written, matching
        // `handle_action`'s order exactly. The two handlers are siblings; the
        // ordering is part of that.
        //
        // The clone is unconditional here where `handle_action`'s is
        // conditional on `!action.is_actor_scoped_preference()`, because the
        // action is not known until `submit_interaction` returns. That is
        // equivalent, not a regression: the seven actions that predicate
        // matches are UI preferences that no candidate enumerator or
        // `materialize_*` function ever produces, so the predicate is always
        // false on this path. The guard below is kept anyway so the two
        // handlers stay structurally identical if that ever changes.
        let pre_action_state = session.state.clone();

        // Set player names for log resolution.
        session.state.log_player_names = session.display_names.clone();

        let applied = submit_interaction_with_rejection(&mut session.state, player, submission)
            .map_err(SessionActionError::Rejected)?;

        if !applied.action.is_actor_scoped_preference() {
            session.push_takeback_state(player, pre_action_state);
        }

        info!(
            game = %game_code,
            player = ?player,
            action_type = applied.action.variant_name(),
            event_count = applied.result.events.len(),
            "interaction applied"
        );

        let (new_legal_actions, spell_costs, by_object) = engine_legal_actions_full(&session.state);
        let auto_pass = auto_pass_recommended(&session.state, &new_legal_actions);

        // Same capture, same placement rationale, as `handle_action`.
        let post_state = session.state.clone();
        session.observe_transition(&applied.result.events, &post_state);

        // Per-game JSON debug log (issue #7978) — see `handle_action`'s
        // sibling comment above; this is the interaction-path mint point.
        session
            .game_log
            .write_game_log_entries(&session.game_code, &applied.result.log_entries);

        Ok((
            post_state,
            applied.result.events,
            new_legal_actions,
            applied.result.log_entries,
            auto_pass,
            spell_costs,
            by_object,
        ))
    }

    /// Applies the payload-free match-concede intent after binding its
    /// requester to an authenticated player token. The closed cause is chosen
    /// here, never by the wire payload or a game action.
    pub fn handle_match_concede(
        &mut self,
        game_code: &str,
        player_token: &str,
    ) -> Result<RevisionedActionResult, String> {
        self.handle_match_concede_outcome(game_code, player_token)
            .map_err(SessionActionError::into_legacy_reason)
    }

    /// Three-way result for the transport-owned match-concede request.
    ///
    /// Authentication/session availability remains operational. A request that
    /// is validly authenticated but cannot forfeit the current match is a
    /// requester-visible lifecycle refusal instead.
    pub fn handle_match_concede_outcome(
        &mut self,
        game_code: &str,
        player_token: &str,
    ) -> Result<RevisionedActionResult, SessionActionError> {
        let session = self
            .sessions
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found: {game_code}"))?;
        let player = session
            .player_for_token(player_token)
            .ok_or_else(|| "Invalid player token".to_string())?;
        session.reject_if_ai_driver_faulted()?;
        if session.pending_takeback.is_some() {
            return Err(SessionActionError::RequestRejected(
                "A takeback request is pending — resolve it before conceding the match".to_string(),
            ));
        }

        let events = apply_trusted_match_forfeit(
            &mut session.state,
            player,
            MatchForfeitCause::MatchConcede,
        )
        .map_err(SessionActionError::RequestRejected)?;
        let (legal_actions, spell_costs, by_object) = engine_legal_actions_full(&session.state);
        let auto_pass = auto_pass_recommended(&session.state, &legal_actions);
        let revision = session.advance_state_revision();
        // Included rather than skipped: the guard is the event scan itself, so
        // "can conceding a match start a turn?" never has to be proved.
        let post_state = session.state.clone();
        session.observe_transition(&events, &post_state);
        Ok((
            revision,
            (
                post_state,
                events,
                legal_actions,
                Vec::new(),
                auto_pass,
                spell_costs,
                by_object,
            ),
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
    ///
    /// Also releases this game's cached per-game log writers (issue #7978
    /// follow-up): every removal path — normal game-over cleanup, restored-
    /// terminal cleanup, and reconnect-grace expiry — funnels through here,
    /// and some of those (restored-terminal, expiry) run before any
    /// `game_session` tracing span exists, so `GameFileLayer::on_close`
    /// never fires for them. Closing here, once, covers all of them instead
    /// of requiring every removal call site to remember it.
    pub fn remove_game(&mut self, game_code: &str) -> Option<GameSession> {
        let session = self.sessions.remove(game_code)?;
        for token in &session.player_tokens {
            self.unindex_token(token);
        }
        self.game_log.close(game_code);
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
        session.game_log = Arc::clone(&self.game_log);
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
    use std::collections::{BTreeMap, BTreeSet};

    use engine::database::card_db::CardDatabase;
    use engine::game::deck_loading::DeckEntry;
    use engine::game::engine::apply;
    use engine::game::interaction::derive_viewer_interaction;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::{Effect, ResolvedAbility, TargetRef};
    use engine::types::actions::{
        PrecastCopyShortcutResponse, ResolveAllConsentDecision, ResolveAllScope,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::game_state::{
        AutoPassMode, CastPaymentMode, PersistedGameState, StackEntry, StackEntryKind,
        StackResolutionAutoPassOverlay, StackResolutionBudget, StackResolutionEntryFence,
        StackResolutionPolicy, StackResolutionSession, WaitingFor,
    };
    use engine::types::interaction::{
        InteractionAvailability, InteractionChoiceId, InteractionReasonCode, InteractionResponse,
        MAX_INTERACTION_LIST_LEN,
    };
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
    fn full_persist_snapshot_roundtrips_exact_generation_and_revision() {
        let mut manager = SessionManager::new();
        let (game_code, _) = manager.create_game(make_deck());
        let snapshot = FullPersistSnapshot {
            key: FullSessionKey {
                game_code,
                generation: 7,
            },
            mutation_revision: 11,
            activation_epoch: Some(3),
            persisted: manager.sessions.values().next().unwrap().to_persisted(),
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: FullPersistSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.key, snapshot.key);
        assert_eq!(restored.mutation_revision, 11);
        assert_eq!(restored.activation_epoch, Some(3));
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
        let restored =
            GameSession::from_persisted(persisted, &db).expect("supported persisted format config");

        assert_eq!(
            restored.state.interaction_session_id,
            Some(InteractionSessionId(code)),
            "the restored session's authority must come from the game code, not \
             from whatever id the persisted blob happened to carry"
        );
    }

    #[test]
    fn persisted_session_with_limited_range_is_rejected() {
        let mut mgr = SessionManager::new();
        let (code, _token) = mgr.create_game(make_deck());
        let mut persisted = mgr.sessions.get(&code).unwrap().to_persisted();
        let mut state = persisted
            .state
            .into_game_state()
            .expect("test snapshot satisfies the checked restore contract");
        state.format_config.range_of_influence =
            Some(Box::new(engine::types::format::RangeOfInfluenceConfig {
                default_range: 0,
                player_overrides: std::collections::BTreeMap::new(),
            }));
        persisted.state = PersistedGameState::capture(state);

        let error = GameSession::from_persisted(persisted, &CardDatabase::default())
            .err()
            .expect("limited range must remain disabled at the restore boundary");

        assert!(error.contains("not supported"));
    }

    #[test]
    fn player_slot_info_omits_team_metadata_for_individual_formats() {
        for format in [FormatConfig::standard(), FormatConfig::commander()] {
            let mut mgr = SessionManager::new();
            let (code, _) = mgr
                .create_game_n_players(
                    make_deck(),
                    "Host".to_string(),
                    None,
                    2,
                    MatchConfig::default(),
                    Some(format),
                )
                .expect("supported format config");

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
        let (code, _) = mgr
            .create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                4,
                MatchConfig::default(),
                Some(FormatConfig::two_headed_giant()),
            )
            .expect("supported format config");

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

    #[test]
    fn create_game_rejects_limited_range_until_supported() {
        let mut mgr = SessionManager::new();
        let mut format_config = FormatConfig::standard();
        format_config.range_of_influence =
            Some(Box::new(engine::types::format::RangeOfInfluenceConfig {
                default_range: 0,
                player_overrides: std::collections::BTreeMap::new(),
            }));

        assert!(mgr
            .create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                2,
                MatchConfig::default(),
                Some(format_config),
            )
            .expect_err("limited range must remain disabled at the session boundary")
            .contains("not supported"));
        assert!(mgr.sessions.is_empty());
    }

    /// CR 103.4: each player begins with a starting life total of 20, and some
    /// variant games use a different one — so a host-configured `starting_life`
    /// is the authority, not the format's default.
    ///
    /// This is the server-side half of the desktop (Tauri) solo/host route: the
    /// native engine is this phase-server running as a sidecar, and the client
    /// hands it the edited `FormatConfig` in `CreateGameWithSettings`. Dropping
    /// it here would silently seat every player at the format default.
    ///
    /// REVERT-FAIL: make `create_game_n_players` ignore its `format_config`
    /// argument (or re-derive one from `format_config.format`) ⇒ life is 40.
    #[test]
    fn create_game_honors_a_configured_starting_life() {
        let mut mgr = SessionManager::new();
        let mut format_config = FormatConfig::commander();
        format_config.starting_life = 25;

        let (code, _) = mgr
            .create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                2,
                MatchConfig::default(),
                Some(format_config),
            )
            .expect("a custom starting life is a supported configuration");

        let session = mgr.sessions.get(&code).unwrap();
        assert_eq!(session.state.format_config.starting_life, 25);
        for player in &session.state.players {
            assert_eq!(
                player.life, 25,
                "every seat must start on the configured life total, not Commander's 40"
            );
        }

        // The between-games rebuild re-reads the session's own format config, so
        // game 2 of a match must not silently revert to the format default.
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .rebuild_pregame_state(2);
        for player in &mgr.sessions.get(&code).unwrap().state.players {
            assert_eq!(
                player.life, 25,
                "the rebuild must preserve the configured life total"
            );
        }
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
        let (code, _) = mgr
            .create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                2,
                MatchConfig {
                    match_type: MatchType::Bo3,
                    loop_detection: LoopDetectionMode::On,
                },
                None,
            )
            .expect("supported format config");

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

    /// Proves a production mint point (`handle_action` ->
    /// `handle_action_with_card_db_outcome`'s write hook) actually reaches a
    /// real `GameFileCache`, not just that `write_game_log_entries` works in
    /// isolation. Attaching the cache post-hoc on `setup_two_player_game`'s
    /// manager would not do this: `GameSession.game_log` is copied from
    /// `SessionManager.game_log` at session-creation time, so the cache must
    /// be installed before `create_game`. This is also the exact wiring gap
    /// (`GameCodeVisitor::record_debug` never captured the span field) that
    /// the second review round caught — a test reaching the real hook is what
    /// would have caught it directly.
    #[test]
    fn handle_action_hook_writes_real_events_stream_file() {
        let dir = tempfile::tempdir().unwrap();
        let games_dir = dir.path().join("games");
        std::fs::create_dir_all(&games_dir).unwrap();

        let mut mgr = SessionManager::new();
        mgr.game_log = std::sync::Arc::new(GameFileCache::new(games_dir.clone()));
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr.join_game(&code, make_deck()).unwrap();
        for _ in 0..20 {
            let session = mgr.sessions.get(&code).unwrap();
            match &session.state.waiting_for.clone() {
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

        let priority_player = match &mgr.sessions.get(&code).unwrap().state.waiting_for {
            WaitingFor::Priority { player } => *player,
            other => panic!("expected Priority, got {other:?}"),
        };
        let acting_token = if priority_player == PlayerId(0) {
            &token0
        } else {
            &token1
        };
        let result = mgr.handle_action(&code, acting_token, GameAction::PassPriority);
        assert!(result.is_ok(), "PassPriority should succeed: {result:?}");

        let events_path = games_dir.join(format!("{code}.events.jsonl"));
        let content = std::fs::read_to_string(&events_path)
            .expect("a real production action hook must have written the events stream file");
        assert!(
            !content.trim().is_empty(),
            "events stream file exists but is empty"
        );
        let row: serde_json::Value = serde_json::from_str(content.lines().next().unwrap())
            .expect("each row must be valid JSON");
        assert!(
            row.get("category").is_some(),
            "row must carry the engine's LogCategory field: {row}"
        );
        assert!(row.get("ts").is_some(), "row must carry a ts field: {row}");
    }

    /// Proves `GameSession::start_game`'s write hook is reachable, not just
    /// present in source — `create_game_n_players`/`join_game` never call
    /// `start_game` (see `handle_action_hook_writes_real_events_stream_file`'s
    /// doc comment), so `create_game_with_ai` is the only `SessionManager`
    /// path that reaches it, mirroring `takeback_auto_approves_for_sole_human_seat`'s setup.
    #[test]
    fn start_game_hook_writes_real_events_stream_file() {
        let dir = tempfile::tempdir().unwrap();
        let games_dir = dir.path().join("games");
        std::fs::create_dir_all(&games_dir).unwrap();

        let mut mgr = SessionManager::new();
        mgr.game_log = std::sync::Arc::new(GameFileCache::new(games_dir.clone()));
        let db = engine::database::CardDatabase::default();
        let (code, _token) = mgr
            .create_game_with_ai(
                make_deck(),
                "Host".to_string(),
                None,
                MatchConfig::default(),
                vec![(1, AiDifficulty::Easy, make_deck())],
                Vec::new(),
                None,
                &db,
            )
            .expect("supported format config");

        let events_path = games_dir.join(format!("{code}.events.jsonl"));
        let content = std::fs::read_to_string(&events_path)
            .expect("start_game's write hook must have written the events stream file");
        assert!(
            !content.trim().is_empty(),
            "events stream file exists but is empty"
        );
    }

    /// Maintainer review finding on PR #8245: `GameFileCache` cached a
    /// writer for every game it ever logged and never released it —
    /// `GameFileCache::close` existed, but nothing called it from
    /// `SessionManager::remove_game`, so every removal path (game-over,
    /// restored-terminal cleanup, reconnect-grace expiry) leaked the cached
    /// `BufWriter`/file handle for the process lifetime. This setup has no
    /// tracing span at all (server-core doesn't do tracing) — deliberately
    /// exercising the "no active `game_session` span" removal shape the
    /// finding named, where `GameFileLayer::on_close` (the OTHER eviction
    /// path, span-teardown-triggered) never fires.
    #[test]
    fn remove_game_evicts_cached_log_writers() {
        let dir = tempfile::tempdir().unwrap();
        let games_dir = dir.path().join("games");
        std::fs::create_dir_all(&games_dir).unwrap();

        let mut mgr = SessionManager::new();
        mgr.game_log = std::sync::Arc::new(GameFileCache::new(games_dir));
        let db = engine::database::CardDatabase::default();
        let (code, _token) = mgr
            .create_game_with_ai(
                make_deck(),
                "Host".to_string(),
                None,
                MatchConfig::default(),
                vec![(1, AiDifficulty::Easy, make_deck())],
                Vec::new(),
                None,
                &db,
            )
            .expect("supported format config");

        assert!(
            mgr.game_log.cached_entry_count() > 0,
            "start_game's write hook should have opened and cached a writer"
        );

        mgr.remove_game(&code);

        assert_eq!(
            mgr.game_log.cached_entry_count(),
            0,
            "remove_game must evict this game's cached writers, not just the session"
        );
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

    use crate::takeback::{RewindOption, RewindTarget, TakebackOutcome, MAX_TAKEBACK_HISTORY};

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
        let outcome = session
            .request_takeback(priority_player, RewindTarget::LastAction)
            .unwrap();
        assert_eq!(outcome, TakebackOutcome::Pending);

        // A second concurrent request is rejected — only one in flight at a time.
        assert!(session
            .request_takeback(other_player, RewindTarget::LastAction)
            .is_err());

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

        assert_eq!(
            session.request_takeback(P0, RewindTarget::LastAction),
            Ok(TakebackOutcome::Pending)
        );
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
        let legacy_restored =
            GameSession::from_persisted(legacy, &db).expect("supported persisted format config");
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
        let trusted_restored =
            GameSession::from_persisted(trusted, &db).expect("supported persisted format config");
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
        let outcome = session
            .request_takeback(player_a, RewindTarget::LastAction)
            .unwrap();
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
        session
            .request_takeback(priority_player, RewindTarget::LastAction)
            .unwrap();
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
        session
            .request_takeback(priority_player, RewindTarget::LastAction)
            .unwrap();

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
        assert!(session
            .request_takeback(player, RewindTarget::LastAction)
            .is_err());
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
        session
            .request_takeback(priority_player, RewindTarget::LastAction)
            .unwrap();

        let result = mgr
            .handle_action_with_card_db_outcome(&code, &other_token, GameAction::PassPriority, None)
            .expect_err("action should be rejected while a takeback is pending");
        assert!(matches!(
            result,
            SessionActionError::Rejected(rejection)
                if rejection.code
                    == engine::types::action_rejection::ActionRejectionCode::ActionNotAllowed
        ));
    }

    /// A solo human vs. AI seats auto-resolves their own takeback request —
    /// there's nobody else at the table to ask.
    #[test]
    fn takeback_auto_approves_for_sole_human_seat() {
        let mut mgr = SessionManager::new();
        let db = engine::database::CardDatabase::default();
        let (code, _token) = mgr
            .create_game_with_ai(
                make_deck(),
                "Host".to_string(),
                None,
                MatchConfig::default(),
                vec![(1, AiDifficulty::Easy, make_deck())],
                Vec::new(),
                None,
                &db,
            )
            .expect("supported format config");

        let session = mgr.sessions.get_mut(&code).unwrap();
        // Force a known checkpoint to take back to, since the AI may have
        // already acted past mulligans by the time the game starts.
        session.push_takeback_snapshot(PlayerId(0));
        let outcome = session
            .request_takeback(PlayerId(0), RewindTarget::LastAction)
            .unwrap();
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
        session
            .request_takeback(priority_player, RewindTarget::LastAction)
            .unwrap();

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
        session
            .request_takeback(priority_player, RewindTarget::LastAction)
            .unwrap();

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
        let (code, _token0) = mgr
            .create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                3,
                MatchConfig::default(),
                None,
            )
            .expect("supported format config");
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
        let outcome = session
            .request_takeback(PlayerId(0), RewindTarget::LastAction)
            .unwrap();
        assert_eq!(outcome, TakebackOutcome::Pending);

        let state_before = session.state.clone();
        let ai_results = session.run_ai();
        assert!(
            ai_results.transitions.is_empty(),
            "run_ai must no-op while a takeback vote is pending, even though the AI seat has a legal action"
        );
        assert_eq!(
            session.state.waiting_for, state_before.waiting_for,
            "authoritative state must not move while a takeback vote is pending"
        );
    }

    // ── Turn-boundary rewind tests ───────────────────────────────────────

    /// `make_deck` is ten cards, so a fixture that drives several turns decks a
    /// player out and ends the game before it reaches the boundary under test.
    /// Padding the libraries keeps the *rewind* behaviour the thing being
    /// measured rather than CR 104.3c.
    fn padded_game(hosting: HostingMode) -> (SessionManager, String, String, String) {
        let (mut mgr, code, token0, token1) = setup_two_player_game();
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.hosting = hosting;
        for seat in 0..session.player_count {
            for _ in 0..80 {
                engine::game::zones::create_object(
                    &mut session.state,
                    engine::types::identifiers::CardId(9000),
                    PlayerId(seat),
                    "Library Filler".to_string(),
                    Zone::Library,
                );
            }
        }
        (mgr, code, token0, token1)
    }

    /// The desktop sidecar. `hosting` is a property of the process, so stamping
    /// it directly is exactly what `SessionManager::restore_session` does.
    fn single_user_game() -> (SessionManager, String, String, String) {
        padded_game(HostingMode::SingleUser)
    }

    fn priority_token(mgr: &SessionManager, code: &str, token0: &str, token1: &str) -> String {
        match &mgr.sessions[code].state.waiting_for {
            WaitingFor::Priority { player } if *player == PlayerId(0) => token0.to_string(),
            WaitingFor::Priority { .. } => token1.to_string(),
            other => panic!("expected Priority, got {other:?}"),
        }
    }

    /// Passes priority (through the real `handle_action` path, so every capture
    /// site runs) until `turn_number` advances. Returns the new turn number.
    fn advance_one_turn(mgr: &mut SessionManager, code: &str, token0: &str, token1: &str) -> u32 {
        let start = mgr.sessions[code].state.turn_number;
        for _ in 0..400 {
            if mgr.sessions[code].state.turn_number > start {
                return mgr.sessions[code].state.turn_number;
            }
            if !matches!(
                mgr.sessions[code].state.waiting_for,
                WaitingFor::Priority { .. }
            ) {
                panic!(
                    "fixture stalled outside Priority: {:?}",
                    mgr.sessions[code].state.waiting_for
                );
            }
            let token = priority_token(mgr, code, token0, token1);
            mgr.handle_action(code, &token, GameAction::PassPriority)
                .expect("PassPriority through the real transition handler");
        }
        panic!("fixture never reached the next turn");
    }

    /// **R6 — BLOCKER 1.** `turn_number` is *assigned* 1 at each game's start of
    /// a Bo3 match, not carried across the match, and `ChoosePlayDraw` runs
    /// through `handle_action` — a capture site. Without retiring both rings at
    /// the game boundary, `rewind_options()` republishes a *finished game's*
    /// turn numbers and `TurnStart { n }` resolves by first match to that
    /// finished game's board.
    ///
    /// This fixture SPANS the game boundary deliberately: a single-game version
    /// of this test goes green with the defect live.
    #[test]
    fn a_bo3_game_boundary_retires_both_rewind_rings() {
        let (mut mgr, code, token0, token1) = single_user_game();
        advance_one_turn(&mut mgr, &code, &token0, &token1);
        advance_one_turn(&mut mgr, &code, &token0, &token1);

        let game_one_options = mgr.sessions[&code].rewind_options();
        assert!(
            game_one_options.iter().any(|o| o.turn_number == 2),
            "reach guard: game 1 must actually have published turn 2 — got {game_one_options:?}"
        );
        assert!(
            !mgr.sessions[&code].takeback_history.is_empty(),
            "reach guard: the action ring must be non-empty before the boundary"
        );

        // Hand the session the between-games pause the engine itself produces
        // after game 1 ends, then take the real `ChoosePlayDraw` action through
        // `handle_action`.
        let chooser = PlayerId(1);
        {
            let session = mgr.sessions.get_mut(&code).unwrap();
            // The between-games rebuild sources game 2's decks from
            // `state.deck_pools`, which the production `GameSession::start_game`
            // path fills via `load_and_hydrate_decks`. This fixture reaches a
            // started game without a `CardDatabase`, so seed them here.
            session.state.deck_pools = (0..2)
                .map(|seat| engine::types::game_state::PlayerDeckPool {
                    player: PlayerId(seat),
                    registered_main: std::sync::Arc::new(make_deck().main_deck),
                    current_main: std::sync::Arc::new(make_deck().main_deck),
                    ..Default::default()
                })
                .collect();
            session.state.match_phase = engine::types::match_config::MatchPhase::BetweenGames;
            session.state.match_score = engine::types::match_config::MatchScore {
                p0_wins: 1,
                p1_wins: 0,
                draws: 0,
            };
            session.state.game_number = 2;
            session.state.next_game_chooser = Some(chooser);
            session.state.sideboard_submitted.clear();
            session.state.waiting_for = WaitingFor::BetweenGamesChoosePlayDraw {
                player: chooser,
                game_number: 2,
                score: session.state.match_score,
            };
        }
        mgr.handle_action(
            &code,
            &token1,
            GameAction::ChoosePlayDraw { play_first: true },
        )
        .expect("the between-games choice is a normal authoritative transition");

        let session = &mgr.sessions[&code];
        assert_eq!(session.state.game_number, 2, "sanity: game 2 is live");
        assert!(
            session.takeback_history.is_empty(),
            "the action ring belongs to the finished game and must be retired"
        );

        let options = session.rewind_options();
        assert_eq!(
            options,
            vec![RewindOption {
                turn_number: 1,
                active_player: session.state.active_player,
            }],
            "game 2 must publish only its own opening boundary — got {options:?}"
        );

        let mut seen: Vec<u32> = options.iter().map(|o| o.turn_number).collect();
        let before_dedup = seen.len();
        seen.dedup();
        assert_eq!(before_dedup, seen.len(), "no two options may share a turn");

        // The severest consequence, asserted directly: a turn number that
        // belonged to the FINISHED game must no longer be resolvable at all.
        let mut mgr = mgr;
        let session = mgr.sessions.get_mut(&code).unwrap();
        let refusal = session
            .request_takeback(PlayerId(0), RewindTarget::TurnStart { turn_number: 2 })
            .expect_err("game 1's turn 2 must not be reachable from game 2");
        assert!(
            refusal.contains("no longer available"),
            "unexpected refusal: {refusal}"
        );
        assert_eq!(
            session.state.game_number, 2,
            "the refused request must not have installed a previous game's board"
        );
    }

    /// R7. On the sidecar a sole human seat auto-approves, and the restored
    /// turn stays selectable — `retain(<=)`, not `retain(<)` — which is what
    /// makes turn rewind repeatable rather than self-consuming.
    #[test]
    fn single_user_turn_rewind_auto_approves_and_stays_reselectable() {
        let (mut mgr, code, token0, token1) = single_user_game();
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .ai_seats
            .insert(PlayerId(1));
        let turn_two = advance_one_turn(&mut mgr, &code, &token0, &token1);
        let turn_three = advance_one_turn(&mut mgr, &code, &token0, &token1);

        let session = mgr.sessions.get_mut(&code).unwrap();
        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two
                }
            ),
            Ok(TakebackOutcome::Approved),
            "a sole human seat has nobody to ask"
        );
        assert_eq!(session.state.turn_number, turn_two);
        assert!(session.pending_takeback.is_none(), "interlock released");

        let options = session.rewind_options();
        assert!(
            options.iter().any(|o| o.turn_number == turn_two),
            "`retain(<=)`: the restored turn must remain selectable — got {options:?}"
        );
        assert!(
            !options.iter().any(|o| o.turn_number == turn_three),
            "boundaries after the restored one belong to the discarded branch"
        );

        // Repeatable: the same target again, with no intervening action.
        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two
                }
            ),
            Ok(TakebackOutcome::Approved)
        );
        assert_eq!(session.state.turn_number, turn_two);
    }

    /// R12. An approved rewind prunes only the discarded branch. Fails under
    /// both `clear()` and `retain(<)`.
    #[test]
    fn approved_rewind_prunes_only_the_discarded_branch() {
        let (mut mgr, code, token0, token1) = single_user_game();
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .ai_seats
            .insert(PlayerId(1));
        let turn_two = advance_one_turn(&mut mgr, &code, &token0, &token1);
        let turn_three = advance_one_turn(&mut mgr, &code, &token0, &token1);
        let turn_four = advance_one_turn(&mut mgr, &code, &token0, &token1);

        let session = mgr.sessions.get_mut(&code).unwrap();
        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_three
                }
            ),
            Ok(TakebackOutcome::Approved)
        );
        let turns: Vec<u32> = session
            .rewind_options()
            .iter()
            .map(|o| o.turn_number)
            .collect();
        assert!(
            turns.contains(&turn_two),
            "an earlier ancestor must survive — this is not a disguised clear(): {turns:?}"
        );
        assert!(
            turns.contains(&turn_three),
            "`retain(<=)` keeps the restored turn: {turns:?}"
        );
        assert!(
            !turns.contains(&turn_four),
            "the discarded branch must be gone: {turns:?}"
        );
    }

    /// **R9 — M5.** A shared server publishes nothing and refuses a turn rewind
    /// with the EXPLICIT gate message, not a vacuous empty-ring miss. The
    /// over-scoping guard is the third assertion: `LastAction` on the same
    /// session must still succeed, proving the gate scoped only `TurnStart` and
    /// did not regress shipped GH #1507 takeback.
    #[test]
    fn an_online_session_publishes_no_rewind_targets_and_refuses_a_turn_start() {
        let (mut mgr, code, token0, token1) = padded_game(HostingMode::Shared);
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .ai_seats
            .insert(PlayerId(1));
        let turn_two = advance_one_turn(&mut mgr, &code, &token0, &token1);

        let session = mgr.sessions.get_mut(&code).unwrap();
        assert_eq!(session.hosting, HostingMode::Shared, "sanity");
        assert!(session.rewind_options().is_empty());
        assert!(
            session.turn_rewind_history.is_empty(),
            "capture is gated too"
        );

        let turn_before = session.state.turn_number;
        let refusal = session
            .request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two,
                },
            )
            .expect_err("a shared host must refuse turn rewind");
        assert!(
            refusal.contains("not available in this game"),
            "must be the explicit policy refusal, not the empty-ring miss: {refusal}"
        );
        assert_eq!(session.state.turn_number, turn_before, "state unchanged");

        // Over-scoping guard.
        assert_eq!(
            session.request_takeback(PlayerId(0), RewindTarget::LastAction),
            Ok(TakebackOutcome::Approved),
            "the shipped action-granular takeback must be untouched"
        );

        // Paired positive: the identical fixture on a sidecar DOES publish.
        let (mut mgr, code, token0, token1) = single_user_game();
        advance_one_turn(&mut mgr, &code, &token0, &token1);
        assert!(!mgr.sessions[&code].rewind_options().is_empty());
    }

    /// **R10 — G3/M6.** Undo is repeatable with nothing in between. At BASE_SHA
    /// `try_resolve_pending_takeback` did `clear()`, so the second request
    /// returned `Err`.
    #[test]
    fn undo_is_repeatable_without_an_intervening_action() {
        let (mut mgr, code, token0, token1) = padded_game(HostingMode::Shared);
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .ai_seats
            .insert(PlayerId(1));

        for _ in 0..3 {
            let token = priority_token(&mgr, &code, &token0, &token1);
            if token != token0 {
                // Only the human seat's own actions are takeback-able.
                mgr.handle_action(&code, &token, GameAction::PassPriority)
                    .expect("advance to the human seat's priority");
                continue;
            }
            mgr.handle_action(&code, &token0, GameAction::PassPriority)
                .expect("human action");
        }

        let session = mgr.sessions.get_mut(&code).unwrap();
        let depth_before = session.takeback_history.len();
        assert!(
            depth_before >= 2,
            "reach guard: need at least two ancestors"
        );

        assert_eq!(
            session.request_takeback(PlayerId(0), RewindTarget::LastAction),
            Ok(TakebackOutcome::Approved)
        );
        assert_eq!(
            session.request_takeback(PlayerId(0), RewindTarget::LastAction),
            Ok(TakebackOutcome::Approved),
            "the second consecutive undo is what fails at BASE_SHA"
        );

        // Hostile: once the ring is exhausted, further requests must refuse —
        // `truncate` bounds correctly rather than growing.
        for _ in 0..depth_before + 2 {
            if session
                .request_takeback(PlayerId(0), RewindTarget::LastAction)
                .is_err()
            {
                return;
            }
        }
        panic!("an exhausted takeback ring must eventually refuse");
    }

    /// **R11 — M6.** `truncate` must not convert a voted rollback into a
    /// unilateral one: at a multi-human table every step of a repeated
    /// walk-back is still its own unanimity vote.
    #[test]
    fn repeated_walk_back_still_requires_unanimity_at_each_step() {
        let (mut mgr, code, token0, token1) = padded_game(HostingMode::Shared);
        for _ in 0..4 {
            let token = priority_token(&mgr, &code, &token0, &token1);
            mgr.handle_action(&code, &token, GameAction::PassPriority)
                .expect("drive a few actions from both seats");
        }

        let session = mgr.sessions.get_mut(&code).unwrap();
        assert_eq!(
            session.request_takeback(PlayerId(0), RewindTarget::LastAction),
            Ok(TakebackOutcome::Pending),
            "two humans: the first step is a vote"
        );
        assert_eq!(
            session.respond_takeback(PlayerId(1), true),
            Ok(TakebackOutcome::Approved)
        );

        // Immediately again — `truncate` left ancestors reachable, but that must
        // NOT make the second step unilateral.
        assert_eq!(
            session.request_takeback(PlayerId(0), RewindTarget::LastAction),
            Ok(TakebackOutcome::Pending),
            "the second step is still a vote, not an auto-approval"
        );
        let state_before = session.state.clone();
        assert_eq!(
            session.respond_takeback(PlayerId(1), false),
            Ok(TakebackOutcome::Rejected)
        );
        assert_eq!(
            session.state.waiting_for, state_before.waiting_for,
            "a decline must leave the authoritative state untouched"
        );
    }

    /// **R8.** Turn rewind is a mechanism, not a privilege: on a sidecar with
    /// two human seats it still requires unanimity, and a decline changes
    /// nothing — including the ring.
    #[test]
    fn turn_rewind_requires_unanimity_at_a_multi_human_table() {
        let (mut mgr, code, token0, token1) = single_user_game();
        let turn_two = advance_one_turn(&mut mgr, &code, &token0, &token1);
        advance_one_turn(&mut mgr, &code, &token0, &token1);

        let session = mgr.sessions.get_mut(&code).unwrap();
        let turn_before = session.state.turn_number;
        let objects_before = session.state.objects.len();
        let options_before = session.rewind_options();

        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two
                }
            ),
            Ok(TakebackOutcome::Pending)
        );
        assert_eq!(
            session.state.turn_number, turn_before,
            "state must not move"
        );
        assert_eq!(session.state.objects.len(), objects_before);

        // Multi-authority hostile: a decline must not prune.
        assert_eq!(
            session.respond_takeback(PlayerId(1), false),
            Ok(TakebackOutcome::Rejected)
        );
        assert_eq!(session.state.turn_number, turn_before);
        assert_eq!(
            session.rewind_options(),
            options_before,
            "a declined request must leave the ring intact"
        );

        // Paired positive: approval does roll back.
        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two
                }
            ),
            Ok(TakebackOutcome::Pending)
        );
        assert_eq!(
            session.respond_takeback(PlayerId(1), true),
            Ok(TakebackOutcome::Approved)
        );
        assert_eq!(session.state.turn_number, turn_two);
    }

    /// **R4 — G2.** The turn ring reaches a boundary the action ring
    /// structurally cannot: every wire `PassPriority` burns one of
    /// `MAX_TAKEBACK_HISTORY`'s twelve slots, so a full turn never fits.
    #[test]
    fn turn_rewind_reaches_a_boundary_the_action_ring_cannot() {
        let (mut mgr, code, token0, token1) = single_user_game();
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .ai_seats
            .insert(PlayerId(1));
        let target_turn = advance_one_turn(&mut mgr, &code, &token0, &token1);
        // Keep playing until the twelve-slot action ring has scrolled entirely
        // past the target boundary. That is the whole point of the second ring:
        // every wire `PassPriority` burns a slot, so the action ring cannot
        // reach back across a turn (CR 500.1).
        let mut turns_played = 0;
        while turns_played < 8 {
            let saturated = {
                let s = &mgr.sessions[&code];
                s.takeback_history.len() == MAX_TAKEBACK_HISTORY
                    && s.takeback_history
                        .iter()
                        .all(|(_, st)| st.turn_number > target_turn)
            };
            if saturated {
                break;
            }
            advance_one_turn(&mut mgr, &code, &token0, &token1);
            turns_played += 1;
        }

        let session = mgr.sessions.get_mut(&code).unwrap();
        // Reach guard: the action ring is full AND can no longer see the target.
        assert_eq!(session.takeback_history.len(), MAX_TAKEBACK_HISTORY);
        assert!(
            session
                .takeback_history
                .iter()
                .all(|(_, s)| s.turn_number > target_turn),
            "reach guard: no action-ring entry may still sit in turn {target_turn}"
        );
        let turn_two = target_turn;

        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two
                }
            ),
            Ok(TakebackOutcome::Approved)
        );
        assert_eq!(session.state.turn_number, turn_two);

        // Hostile: a turn that never existed must refuse and change nothing.
        let objects_before = session.state.objects.len();
        assert!(session
            .request_takeback(PlayerId(0), RewindTarget::TurnStart { turn_number: 9999 })
            .is_err());
        assert_eq!(session.state.turn_number, turn_two);
        assert_eq!(session.state.objects.len(), objects_before);
    }

    /// **R13 — M7/G6.** The takeback path was the only state-install path that
    /// did not rebuild `ai_session`. `rekey_after_trusted_restore` rewrites
    /// object identity, so every id-keyed cache from the discarded branch is
    /// stale by construction — a rebuild, not a selective invalidation.
    #[test]
    fn an_approved_rollback_rebuilds_the_ai_session() {
        let (mut mgr, code, token0, token1) = single_user_game();
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .ai_seats
            .insert(PlayerId(1));
        advance_one_turn(&mut mgr, &code, &token0, &token1);
        let turn_two = mgr.sessions[&code].state.turn_number;

        let session = mgr.sessions.get_mut(&code).unwrap();
        session.ai_session = Some(AiSession::arc_from_game(&session.state));
        let before = session
            .ai_session
            .clone()
            .expect("reach guard: the fixture installed a session");

        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two
                }
            ),
            Ok(TakebackOutcome::Approved)
        );
        let after = session
            .ai_session
            .clone()
            .expect("an AI session must still be present after the rollback");
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the rolled-back session must be a fresh build, not the pre-rollback Arc"
        );

        // Paired negative: a session with no AI must not gain one.
        let (mut mgr, code, token0, token1) = single_user_game();
        advance_one_turn(&mut mgr, &code, &token0, &token1);
        let turn_two = mgr.sessions[&code].state.turn_number;
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.ai_session = None;
        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: turn_two
                }
            ),
            Ok(TakebackOutcome::Pending)
        );
        assert_eq!(
            session.respond_takeback(PlayerId(1), true),
            Ok(TakebackOutcome::Approved)
        );
        assert!(session.ai_session.is_none(), "`None` must stay `None`");
    }

    /// A **driven-AI** sidecar fixture: seat 1 is not merely listed in
    /// `ai_seats`, it carries a real `AiConfig`, which is what makes `run_ai`
    /// actually choose and apply actions rather than bail on
    /// `MissingAiConfig`. The AI bookkeeping mirrors
    /// `run_ai_is_noop_while_takeback_is_pending` above;
    /// `single_user_game` supplies the `HostingMode::SingleUser` stamp and the
    /// padded libraries.
    fn single_user_game_vs_ai() -> (SessionManager, String, String, PlayerId) {
        let (mut mgr, code, token0, _token1) = single_user_game();
        let ai_seat = PlayerId(1);
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.ai_seats.insert(ai_seat);
        session.ai_configs.insert(
            ai_seat,
            phase_ai::config::create_config_for_players(AiDifficulty::Easy, Platform::Native, 2),
        );
        (mgr, code, token0, ai_seat)
    }

    #[test]
    fn ai_driver_failure_gate_requires_an_authorized_ai_submitter() {
        let (mut mgr, code, _token0, ai_seat) = single_user_game_vs_ai();
        let session = mgr.sessions.get_mut(&code).unwrap();

        session.state.waiting_for = WaitingFor::Priority { player: ai_seat };
        assert!(
            session.ai_seat_can_act(),
            "an AI priority holder must keep a capped driver diagnosable"
        );

        session.state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert!(
            !session.ai_seat_can_act(),
            "a human priority holder is a normal hand-off, not an AI driver failure"
        );

        session.state.waiting_for = WaitingFor::GameOver {
            winner: Some(ai_seat),
        };
        assert!(
            !session.ai_seat_can_act(),
            "a terminal state cannot be reported as an AI driver stall"
        );
    }

    #[test]
    fn ai_driver_fault_fences_persistence_without_synthetic_state_transition() {
        let (mut mgr, code, _token0, ai_seat) = single_user_game_vs_ai();
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.ai_configs.remove(&ai_seat);
        session.state.waiting_for = WaitingFor::Priority { player: ai_seat };
        let before = session.state_revision;

        let outcome = session.run_ai();

        assert!(outcome.transitions.is_empty());
        assert!(matches!(
            outcome.failure,
            Some(AiDriverFailure::MissingAiConfig { player }) if player == ai_seat
        ));
        let fault = outcome
            .fault
            .expect("missing configuration records a fault");
        assert_eq!(fault.after_state_revision, before);
        assert_eq!(session.state_revision, before + 1);
        assert_eq!(
            session.to_persisted().state_revision,
            before + 1,
            "the durable snapshot must be fenced beyond the last delivered state"
        );
    }

    #[test]
    fn ai_driver_fault_blocks_takebacks_and_match_concede() {
        let (mut mgr, code, token0, _ai_seat) = single_user_game_vs_ai();
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.record_ai_driver_fault(AiDriverFailure::ActionSafetyCapReached { limit: 1 });

        for result in [
            session
                .request_takeback(PlayerId(0), RewindTarget::LastAction)
                .map(|_| ()),
            session.respond_takeback(PlayerId(0), true).map(|_| ()),
            session.cancel_takeback(PlayerId(0)),
        ] {
            assert!(result
                .expect_err("a terminal AI driver fault blocks takeback mutation")
                .contains("Native AI driver fault"));
        }

        let err = mgr
            .handle_match_concede(&code, &token0)
            .expect_err("a terminal AI driver fault blocks match concede");
        assert!(err.contains("Native AI driver fault"));
    }

    #[test]
    fn match_concede_distinguishes_lifecycle_refusal_from_operational_failure() {
        let (mut mgr, code, token0, _token1) = setup_two_player_game();

        let lifecycle = mgr
            .handle_match_concede_outcome(&code, &token0)
            .expect_err("a non-Bo3 match cannot be conceded as a match");
        assert!(matches!(
            lifecycle,
            SessionActionError::RequestRejected(reason)
                if reason == "Match forfeits require a best-of-three match"
        ));

        let operational = mgr
            .handle_match_concede_outcome("missing", &token0)
            .expect_err("an absent session is operational");
        assert!(matches!(operational, SessionActionError::Operational(_)));
    }

    /// Drives the fixture until **`run_ai` itself** publishes a turn boundary
    /// whose active player is the AI seat, and returns that turn number.
    ///
    /// The split is the whole point: the human seat goes through the real
    /// `handle_action` path and the AI seat goes through `session.run_ai()` —
    /// never by hand. On a wire session that is exactly the production shape,
    /// and it is what puts the crossing into the AI's turn inside `run_ai`: in
    /// a two-player game the last player to pass in the active player's end
    /// step is the *non*-active one, so the human's own turn is ended by the
    /// AI's pass, which only `run_ai` submits.
    ///
    /// The before/after diff around the `run_ai` call is the attribution: an
    /// option that was absent before it and present after it can only have come
    /// from `run_ai`'s own results.
    fn drive_until_run_ai_opens_an_ai_turn(
        mgr: &mut SessionManager,
        code: &str,
        token0: &str,
        ai_seat: PlayerId,
    ) -> u32 {
        for _ in 0..40 {
            let session = mgr.sessions.get_mut(code).unwrap();
            let before: Vec<u32> = session
                .rewind_options()
                .iter()
                .map(|option| option.turn_number)
                .collect();
            let ai_results = session.run_ai();
            let gained = session.rewind_options().into_iter().find(|option| {
                option.active_player == ai_seat && !before.contains(&option.turn_number)
            });
            if let Some(option) = gained {
                assert!(
                    !ai_results.transitions.is_empty(),
                    "a boundary appeared across a `run_ai` call that returned nothing — \
                     the fixture is not measuring what it claims to"
                );
                return option.turn_number;
            }
            let waiting = session.state.waiting_for.clone();
            match waiting {
                WaitingFor::Priority { player } if player == ai_seat => panic!(
                    "the AI seat holds priority but `run_ai` produced nothing — \
                     the fixture cannot drive the AI at all"
                ),
                WaitingFor::Priority { .. } => {}
                other => panic!("fixture stalled outside Priority: {other:?}"),
            }
            mgr.handle_action(code, token0, GameAction::PassPriority)
                .expect("the human seat passes through the real transition handler");
        }
        panic!("`run_ai` never opened an AI-active turn boundary");
    }

    /// **R5 + R14 — the `run_ai` capture site and the G5 freeze fix.**
    ///
    /// Every other capture test in this module drives *both* seats by hand
    /// through `handle_action` and never calls `run_ai()`. On a wire session the
    /// AI's priority passes come from `run_ai`, so in production the transition
    /// that crosses into the AI's turn happens inside it — and the headline
    /// affordance ("rewind to the start of the AI's turn") rests entirely on the
    /// `observe_transition` call in `run_ai`'s per-result map. This test is the
    /// only coverage of that line.
    ///
    /// It then carries the G5 half: if a rewind onto an AI-active boundary did
    /// not resume the AI, a user who rewound there would get a stalled game.
    ///
    /// **Revert probe (run, not asserted from memory):** deleting
    /// `self.observe_transition(&r.events, &r.state);` from `run_ai` makes
    /// `drive_until_run_ai_opens_an_ai_turn` exhaust its budget and panic with
    /// "`run_ai` never opened an AI-active turn boundary" — the boundary is
    /// never published at all.
    #[test]
    fn a_turn_opened_inside_run_ai_is_rewindable_and_resumes_instead_of_freezing() {
        let (mut mgr, code, token0, ai_seat) = single_user_game_vs_ai();
        let ai_turn = drive_until_run_ai_opens_an_ai_turn(&mut mgr, &code, &token0, ai_seat);

        let session = mgr.sessions.get_mut(&code).unwrap();
        // **The discriminating guard.** `run_ai` pushes no `takeback_history`
        // entries — only `handle_action` does. So if this turn's boundary had
        // come from the hand-driven path rather than from `run_ai`'s own
        // results, the action ring would already carry an entry sitting in it.
        // Read here, before the human acts again: once the human passes inside
        // the AI's turn, `handle_action` pushes one legitimately.
        assert!(
            session
                .takeback_history
                .iter()
                .all(|(_, snapshot)| snapshot.turn_number != ai_turn),
            "no action-ring entry may sit in turn {ai_turn} — the boundary must have come \
             from `run_ai`'s own results, not from the `handle_action` capture site"
        );
        assert!(
            !session.takeback_history.is_empty(),
            "reach guard: the action ring is populated, so the assertion above is a \
             statement about this turn and not about an empty collection"
        );
        // Q3's headline: the boundary is offered, and labelled with the AI seat.
        assert!(
            session.rewind_options().contains(&RewindOption {
                turn_number: ai_turn,
                active_player: ai_seat,
            }),
            "the AI-active boundary must be published: {:?}",
            session.rewind_options()
        );

        assert_eq!(
            session.request_takeback(
                PlayerId(0),
                RewindTarget::TurnStart {
                    turn_number: ai_turn
                }
            ),
            Ok(TakebackOutcome::Approved),
            "seat 0 is the sole human — nobody to ask"
        );
        assert_eq!(session.state.turn_number, ai_turn);
        assert_eq!(
            session.state.active_player, ai_seat,
            "the rewind must land on the AI's turn — otherwise the G5 half below is vacuous"
        );

        // **G5.** Nothing else on the approved path drives the AI, so if this
        // returns empty the desktop table is frozen: the AI holds priority and
        // no client action will ever arrive to move it.
        let waiting_before = session.state.waiting_for.clone();
        let resumed = session.run_ai();
        assert!(
            !resumed.transitions.is_empty(),
            "a rewind onto an AI-active boundary must resume the AI, not freeze the game"
        );
        assert_ne!(
            session.state.waiting_for, waiting_before,
            "the AI must have actually advanced the state past its own priority"
        );

        // **Paired negative.** A rewind landing on *human* priority must leave
        // `run_ai` a no-op — the resume above is a consequence of where the
        // rewind landed, not something the approved path does unconditionally.
        let (mut mgr, code, token0, ai_seat) = single_user_game_vs_ai();
        drive_until_run_ai_opens_an_ai_turn(&mut mgr, &code, &token0, ai_seat);
        let session = mgr.sessions.get_mut(&code).unwrap();
        assert_eq!(
            session.request_takeback(PlayerId(0), RewindTarget::LastAction),
            Ok(TakebackOutcome::Approved)
        );
        assert!(
            matches!(session.state.waiting_for, WaitingFor::Priority { player } if player == PlayerId(0)),
            "reach guard: this rewind must land on the human's priority, or the \
             negative below proves nothing — got {:?}",
            session.state.waiting_for
        );
        let waiting_before = session.state.waiting_for.clone();
        let turn_before = session.state.turn_number;
        assert!(
            session.run_ai().transitions.is_empty(),
            "with the human on priority the AI has nothing to do"
        );
        assert_eq!(session.state.waiting_for, waiting_before, "state unchanged");
        assert_eq!(session.state.turn_number, turn_before);
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
        .expect("supported sandbox config")
    }

    #[test]
    fn server_card_database_resolves_debug_card_batches() {
        let mut mgr = SessionManager::new();
        let (code, token) = create_sandbox_game(&mut mgr);
        let db = CardDatabase::from_json_str(
            r#"{
                "server debug creature": {
                    "name": "Server Debug Creature",
                    "mana_cost": { "type": "NoCost" },
                    "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                    "power": "1",
                    "toughness": "1",
                    "loyalty": null,
                    "defense": null,
                    "oracle_text": null,
                    "abilities": [],
                    "triggers": [],
                    "static_abilities": [],
                    "replacements": [],
                    "keywords": []
                }
            }"#,
        )
        .expect("debug-card fixture database parses");

        let result = mgr
            .handle_action_with_card_db(
                &code,
                &token,
                GameAction::Debug(engine::types::actions::DebugAction::CreateCard {
                    card_name: "Server Debug Creature".into(),
                    owner: PlayerId(1),
                    zone: Zone::Battlefield,
                    count: 2,
                    attach_to: None,
                    run_etb: true,
                    nonlegendary: false,
                }),
                Some(&db),
            )
            .expect("server transport resolves a debug CreateCard batch through its card database");

        assert_eq!(
            result
                .1
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        GameEvent::DebugActionUsed {
                            player_id: PlayerId(0),
                            ..
                        }
                    )
                })
                .count(),
            1,
            "source-bound server debug actions retain exactly the engine audit event"
        );
        assert!(
            !result.3.is_empty(),
            "the audit event resolves to a player-visible game-log entry"
        );

        assert_eq!(
            mgr.sessions[&code]
                .state
                .objects
                .values()
                .filter(|object| {
                    object.name == "Server Debug Creature"
                        && object.owner == PlayerId(1)
                        && object.zone == Zone::Battlefield
                })
                .count(),
            2
        );
    }

    #[test]
    fn server_debug_create_zeroes_skip_lifecycle_and_takeback_side_effects() {
        let mut mgr = SessionManager::new();
        let (code, token) = create_sandbox_game(&mut mgr);
        let session = &mgr.sessions[&code];
        let history_depth = session.takeback_history.len();
        let turn_history_depth = session.turn_rewind_history.len();
        let rewind_game_number = session.rewind_game_number;
        let session_revision = session.state_revision;
        let revision = session.state.state_revision;
        let object_count = session.state.objects.len();
        let log_player_names = session.state.log_player_names.clone();

        let actions = [
            (
                "card",
                GameAction::Debug(DebugAction::CreateCard {
                    card_name: "database deliberately absent".into(),
                    owner: PlayerId(0),
                    zone: Zone::Battlefield,
                    count: 0,
                    attach_to: None,
                    run_etb: true,
                    nonlegendary: false,
                }),
            ),
            (
                "token",
                GameAction::Debug(DebugAction::CreateToken {
                    request: engine::types::actions::DebugTokenRequest::Preset {
                        preset_id: "not resolved for zero".into(),
                        owner: PlayerId(0),
                        power_override: None,
                        toughness_override: None,
                        enter_with_counters: Vec::new(),
                    },
                    count: 0,
                    run_etb: true,
                }),
            ),
            (
                "token copy",
                GameAction::Debug(DebugAction::CreateTokenCopy {
                    source_id: ObjectId(u64::MAX),
                    owner: PlayerId(0),
                    count: 0,
                    nonlegendary: false,
                }),
            ),
        ];

        for (label, action) in actions {
            let result = mgr
                .handle_action(&code, &token, action)
                .unwrap_or_else(|error| panic!("authorized zero {label} must be a no-op: {error}"));

            assert!(result.1.is_empty(), "zero {label} emitted events");
            assert!(result.3.is_empty(), "zero {label} emitted log entries");
        }
        let session = &mgr.sessions[&code];
        assert_eq!(session.takeback_history.len(), history_depth);
        assert_eq!(session.turn_rewind_history.len(), turn_history_depth);
        assert_eq!(session.rewind_game_number, rewind_game_number);
        assert_eq!(session.state_revision, session_revision);
        assert_eq!(session.state.state_revision, revision);
        assert_eq!(session.state.objects.len(), object_count);
        assert_eq!(session.state.log_player_names, log_player_names);
    }

    #[test]
    fn server_debug_create_preflight_runs_before_database_lookup() {
        let mut mgr = SessionManager::new();
        let (code, token) = create_sandbox_game(&mut mgr);

        let owner_error = mgr
            .handle_action(
                &code,
                &token,
                GameAction::Debug(DebugAction::CreateCard {
                    card_name: "database deliberately absent".into(),
                    owner: PlayerId(9),
                    zone: Zone::Hand,
                    count: 1,
                    attach_to: None,
                    run_etb: true,
                    nonlegendary: false,
                }),
            )
            .expect_err("an invalid owner must fail before database lookup");
        assert_eq!(
            owner_error,
            "That action is not valid in the current game state."
        );
        assert!(!owner_error.contains("card database"));

        let session = &mgr.sessions[&code];
        let history_depth = session.takeback_history.len();
        let revision = session.state.state_revision;
        let public_state_dirty = session.state.public_state_dirty.clone();
        let log_player_names = session.state.log_player_names.clone();
        let lookup_error = mgr
            .handle_action(
                &code,
                &token,
                GameAction::Debug(DebugAction::CreateCard {
                    card_name: "database deliberately absent".into(),
                    owner: PlayerId(0),
                    zone: Zone::Hand,
                    count: 1,
                    attach_to: None,
                    run_etb: true,
                    nonlegendary: false,
                }),
            )
            .expect_err("a valid nonzero request requires a database");
        assert!(lookup_error.contains("requires a card database"));
        let session = &mgr.sessions[&code];
        assert_eq!(session.takeback_history.len(), history_depth);
        assert_eq!(session.state.state_revision, revision);
        assert_eq!(session.state.public_state_dirty, public_state_dirty);
        assert_eq!(session.state.log_player_names, log_player_names);

        mgr.sessions
            .get_mut(&code)
            .expect("sandbox session exists")
            .state
            .waiting_for = WaitingFor::GameOver { winner: None };
        let priority_error = mgr
            .handle_action(
                &code,
                &token,
                GameAction::Debug(DebugAction::CreateCard {
                    card_name: "database deliberately absent".into(),
                    owner: PlayerId(0),
                    zone: Zone::Battlefield,
                    count: 1,
                    attach_to: None,
                    run_etb: true,
                    nonlegendary: false,
                }),
            )
            .expect_err("a real entry off Priority must fail before database lookup");
        assert_eq!(
            priority_error,
            "That action is not valid in the current game state."
        );
        assert!(!priority_error.contains("card database"));
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
        assert_eq!(err, "That action is not valid in the current game state.");
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
        let (code, _token) = mgr
            .create_game_with_ai(
                make_deck(),
                "Host".to_string(),
                None,
                MatchConfig::default(),
                vec![(1, AiDifficulty::Easy, make_deck())],
                Vec::new(),
                None,
                &db,
            )
            .expect("supported format config");
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
        let (code, host_token) = mgr
            .create_game_with_ai(
                make_deck(),
                "Host".to_string(),
                None,
                MatchConfig::default(),
                vec![(1, AiDifficulty::Easy, make_deck())],
                Vec::new(),
                None,
                &db,
            )
            .expect("supported format config");

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
        let (code, _host) = mgr
            .create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                3,
                MatchConfig::default(),
                None,
            )
            .expect("supported format config");
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
        let restored =
            GameSession::from_persisted(persisted, &db).expect("supported persisted format config");
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
        GameSession::from_persisted(persisted, db).expect("supported persisted format config")
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

    /// The high-water this row plants before persisting. Deliberately NOT block-aligned
    /// (ChaCha20 block 18, word 3), so a fast-forward that only lands on block boundaries
    /// cannot pass by accident. `set_word_pos`/`get_word_pos` round-trip any position exactly.
    const SAVED_WORD_POS: u128 = 291;

    /// `from_persisted` re-seeds `rng` from fresh entropy — deliberately, so restored games do
    /// not all share one deterministic sequence — and must drop `rng_word_pos` in the same step.
    /// A fresh stream starts at word 0, so a surviving high-water leaves `advance_rng_high_water`
    /// guarding a position the live cursor is BEHIND, and the next `capture_rng_word_pos`
    /// `.expect`-panics `HighWaterRegression`. `resolve_and_apply_library_shuffle` performs that
    /// capture before every shuffle, so this bit every restored server game that had shuffled.
    ///
    /// Non-vacuity: the premise assertions measure that the blob really carried a NON-ZERO
    /// position AND that the engine chokepoint really resumed it, so the `== 0` below can only be
    /// this function discarding it — not serde dropping a field that was never there.
    /// Discrimination: deleting `state.rng_word_pos = 0` reds the high-water assertion with
    /// `291 != 0` and panics the shuffle underneath it.
    #[test]
    fn restore_reseeds_the_rng_and_drops_the_saved_stream_position() {
        let db = engine::database::CardDatabase::default();
        let mut mgr = SessionManager::new();
        let code = single_ai_opponent_game(&mut mgr);

        let session = mgr.sessions.get_mut(&code).unwrap();
        // Guard, not an assumption: if setup ever consumes past the planted position the
        // capture below would panic in the fixture rather than in the code under test.
        assert!(
            session.state.rng_word_pos < SAVED_WORD_POS,
            "fixture premise: setup must leave the high-water below the planted position",
        );
        // Plant it the way a shuffle does — advance the live cursor, then promote it through
        // the engine's own monotonic primitive. Never by writing the field.
        session.state.rng.set_word_pos(SAVED_WORD_POS);
        session.state.capture_rng_word_pos();
        let saved_seed = session.state.rng_seed;
        assert_eq!(
            session.state.rng_word_pos, SAVED_WORD_POS,
            "fixture premise: a NON-ZERO saved high-water, or this row measures nothing",
        );

        let json = serde_json::to_string(&mgr.sessions.get(&code).unwrap().to_persisted()).unwrap();
        let blob: crate::persist::PersistedSession = serde_json::from_str(&json).unwrap();

        // Premise 2, measured: the position survives disk AND the chokepoint resumes it.
        let chokepoint_only = blob
            .state
            .clone()
            .into_game_state()
            .expect("test snapshot satisfies the checked restore contract");
        assert_eq!(
            chokepoint_only.rng_word_pos, SAVED_WORD_POS,
            "premise: the persisted blob carries the position across disk",
        );
        assert_eq!(
            chokepoint_only.rng.get_word_pos(),
            chokepoint_only.rng_word_pos,
            "premise: `into_game_state` resumes it, so a zero below is this session's own \
             policy and not a lost field",
        );

        let mut restored =
            GameSession::from_persisted(blob, &db).expect("supported persisted format config");

        assert_ne!(
            restored.state.rng_seed, saved_seed,
            "the fresh-seed policy must survive this fix",
        );
        assert_eq!(
            restored.state.rng_word_pos, 0,
            "a freshly seeded stream must not inherit the old stream's high-water",
        );
        assert_eq!(
            restored.state.rng.get_word_pos(),
            restored.state.rng_word_pos,
            "live cursor and persisted high-water must agree after restore",
        );

        assert!(
            !restored.state.players[0].library.is_empty(),
            "reach-guard: the shuffle below must have a library to act on",
        );
        engine::game::library::resolve_and_apply_library_shuffle(
            &mut restored.state,
            PlayerId(0),
            &mut Vec::new(),
        )
        .expect("a restored session must be able to shuffle");
    }

    /// Paired reach-guard. It does NOT red when the fix is reverted, and is not meant to: its job
    /// is to prove the panic is REACHABLE through the production shuffle seam on a restored
    /// session, so the row above's "the shuffle succeeds" is evidence rather than a statement
    /// about a call that could never have failed.
    #[test]
    #[should_panic(expected = "HighWaterRegression")]
    fn a_restored_session_that_kept_the_saved_stream_position_panics_on_its_next_shuffle() {
        let db = engine::database::CardDatabase::default();
        let mut mgr = SessionManager::new();
        let code = single_ai_opponent_game(&mut mgr);
        let mut restored = round_trip_through_disk(mgr.sessions.get(&code).unwrap(), &db);

        // Re-create the pre-fix pairing on a RESTORED state: a fresh word-0 stream under a
        // surviving high-water. Exactly what `from_persisted` used to hand back.
        restored.state.rng_word_pos = SAVED_WORD_POS;
        engine::game::library::resolve_and_apply_library_shuffle(
            &mut restored.state,
            PlayerId(0),
            &mut Vec::new(),
        )
        .expect("unreachable: the capture panics first");
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
    fn revoked_seat_cannot_create_debug_cards_through_server_card_database() {
        let mut mgr = SessionManager::new();
        let (code, host_token) = create_sandbox_game(&mut mgr);
        let (guest_token, _state) = mgr
            .join_game_with_name(&code, make_deck(), "Guest".to_string())
            .expect("guest joins");

        mgr.handle_action(
            &code,
            &host_token,
            GameAction::RevokeDebugPermission {
                player_id: PlayerId(1),
            },
        )
        .expect("host revokes the guest's debug permission");

        // The server's source-bound CreateCard path must not skip the shared
        // Debug(_) admission gate before attempting card-database resolution.
        let err = mgr
            .handle_action_with_card_db(
                &code,
                &guest_token,
                GameAction::Debug(DebugAction::CreateCard {
                    card_name: "Any Card".to_string(),
                    owner: PlayerId(1),
                    zone: Zone::Battlefield,
                    count: 1,
                    attach_to: None,
                    run_etb: true,
                    nonlegendary: false,
                }),
                None,
            )
            .expect_err("revoked guests cannot reach the server CreateCard path");
        assert_eq!(err, "You are not authorized to use debug actions.");
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
        assert_eq!(err, "That action is not allowed right now.");
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
        assert_eq!(err, "That action is not allowed right now.");
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
        assert_eq!(err, "That action is not allowed right now.");
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
            game_log: Arc::default(),
            full_runtime: None,
            state_revision: 0,
            ai_driver_fault: None,
            next_ai_driver_fault_id: 1,
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
            turn_rewind_history: VecDeque::new(),
            rewind_game_number: 1,
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
            rest_order: engine::types::ability::DigRestOrder::Preserve,
            source_id: None,
            enter_tapped: false,
            enters_attacking: false,
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
        let illegal = mgr.handle_action_with_card_db_outcome(
            &code,
            &token,
            GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 4)],
                trample_damage: 0,
                controller_damage: 0,
            },
            None,
        );
        match illegal {
            Err(SessionActionError::Rejected(rejection)) => assert_eq!(
                rejection.code,
                engine::types::action_rejection::ActionRejectionCode::InvalidAction,
                "wrong-total division must be rejected by apply(), not the gate"
            ),
            Err(error) => panic!(
                "wrong-total division must be rejected by apply(), not the gate, got: {error:?}"
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
        let (code, token0) = mgr
            .create_game_n_players(
                make_deck(),
                "Host".to_string(),
                None,
                3,
                MatchConfig::default(),
                Some(FormatConfig::standard()),
            )
            .expect("supported format config");
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
        let duplicate = mgr.handle_action_with_card_db_outcome(
            &code,
            &token0,
            GameAction::DeclareAttackers {
                attacks: vec![
                    attacks[0],
                    (attacks[0].0, AttackTarget::Player(PlayerId(2))),
                ],
                bands: vec![],
            },
            None,
        );
        assert!(
            matches!(
                duplicate,
                Err(SessionActionError::Rejected(ref rejection))
                    if rejection.code
                        == engine::types::action_rejection::ActionRejectionCode::InvalidAction
            ),
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

    /// A live, engine-published submission for whichever seat currently holds a
    /// complete progress witness, paired with that seat's authenticated token.
    ///
    /// Deriving the witness from `derive_viewer_interaction` rather than
    /// hand-building one is the point: these tests must exercise a capability
    /// the engine actually minted, or a `StaleInteraction` would be
    /// indistinguishable from a fabricated id.
    fn live_witness<'a>(
        mgr: &SessionManager,
        code: &str,
        token0: &'a str,
        token1: &'a str,
    ) -> (PlayerId, &'a str, &'a str, InteractionSubmission) {
        let state = &mgr.sessions.get(code).expect("session").state;
        for (player, token, other) in [(PlayerId(0), token0, token1), (PlayerId(1), token1, token0)]
        {
            let filtered = filter_state_for_player(state, player);
            let view = derive_viewer_interaction(state, &filtered, player);
            if let InteractionAvailability::ProgressAvailable { witness } = view.availability {
                return (player, token, other, witness);
            }
        }
        panic!("a started session must publish a progress witness for some seat");
    }

    fn started_two_seat_game() -> (SessionManager, String, String, String) {
        let mut mgr = SessionManager::new();
        let (code, token0) = mgr.create_game(make_deck());
        let (token1, _) = mgr
            .join_game(&code, make_deck())
            .expect("second seat joins");
        (mgr, code, token0, token1)
    }

    /// The multi-authority hostile fixture: two seats, both legitimately
    /// authenticated, one capability. Seat B holding a *valid* token for the
    /// same game must not be able to spend seat A's `interaction_id`.
    #[test]
    fn handle_interaction_binds_the_actor_to_the_authenticated_token() {
        let (mut mgr, code, token0, token1) = started_two_seat_game();
        let (_acting, acting_token, other_token, witness) =
            live_witness(&mgr, &code, &token0, &token1);

        let before = mgr.sessions[&code].state.active_interaction_slots.clone();

        let forged = mgr.handle_interaction(&code, other_token, witness.clone());
        let error = forged.expect_err("a valid token for another seat must not spend this slot");
        assert_eq!(error, "You are not authorized to answer that interaction.");
        assert_eq!(
            mgr.sessions[&code].state.active_interaction_slots, before,
            "a refused submission must not consume the capability"
        );

        // The success half is the reach guard: without it the `Err` above could
        // have come from staleness rather than from authorization.
        mgr.handle_interaction(&code, acting_token, witness)
            .expect("the authenticated owner of the slot may spend it");
        assert_ne!(
            mgr.sessions[&code].state.active_interaction_slots, before,
            "an accepted submission re-mints the interaction slots"
        );
    }

    #[test]
    fn handle_interaction_rejects_an_unknown_token() {
        let (mut mgr, code, token0, token1) = started_two_seat_game();
        let (_acting, _acting_token, _other, witness) = live_witness(&mgr, &code, &token0, &token1);

        let before_waiting = mgr.sessions[&code].state.waiting_for.clone();
        let before_slots = mgr.sessions[&code].state.active_interaction_slots.clone();

        let error = mgr
            .handle_interaction(&code, "not-a-real-token", witness)
            .expect_err("an unknown token is not a seat");
        assert_eq!(error, "Invalid player token");

        assert_eq!(mgr.sessions[&code].state.waiting_for, before_waiting);
        assert_eq!(
            mgr.sessions[&code].state.active_interaction_slots,
            before_slots
        );
    }

    /// Capability consumption. This is the routine condition the benign
    /// rejection channel exists for: a double-click produces exactly this.
    #[test]
    fn handle_interaction_rejects_a_stale_submission_benignly() {
        let (mut mgr, code, token0, token1) = started_two_seat_game();
        let (_acting, acting_token, _other, witness) = live_witness(&mgr, &code, &token0, &token1);

        mgr.handle_interaction(&code, acting_token, witness.clone())
            .expect("the first submission spends a live capability");

        let error = mgr
            .handle_interaction(&code, acting_token, witness)
            .expect_err("an interaction id is single-use");
        assert_eq!(error, "That interaction has already changed.");
    }

    #[test]
    fn handle_interaction_rejects_an_oversized_response_before_the_reducer() {
        let (mut mgr, code, token0, token1) = started_two_seat_game();
        let (_acting, acting_token, _other, witness) = live_witness(&mgr, &code, &token0, &token1);

        let choice = InteractionChoiceId("a".to_string());
        let oversized = InteractionSubmission {
            interaction_id: witness.interaction_id.clone(),
            response: InteractionResponse::Select {
                choice_ids: vec![choice.clone(); MAX_INTERACTION_LIST_LEN + 1],
            },
        };

        let error = mgr
            .handle_interaction(&code, acting_token, oversized)
            .expect_err("an oversized response is refused");
        assert_eq!(error, "That interaction response is too large.");

        // Negative sibling: the boundary sits at the constant, not at "any
        // large list". The same shape at exactly the limit is still refused,
        // but for a different reason and further downstream.
        let at_limit = InteractionSubmission {
            interaction_id: witness.interaction_id.clone(),
            response: InteractionResponse::Select {
                choice_ids: vec![choice; MAX_INTERACTION_LIST_LEN],
            },
        };
        let error = mgr
            .handle_interaction(&code, acting_token, at_limit)
            .expect_err("unknown choices are still refused");
        assert!(
            error != "That interaction response is too large.",
            "the bound must be the constant, not list size in general: {error}"
        );
    }

    #[test]
    fn handle_interaction_defers_to_a_pending_takeback() {
        let (mut mgr, code, token0, token1) = started_two_seat_game();
        let (acting, acting_token, _other, witness) = live_witness(&mgr, &code, &token0, &token1);

        let target_state = mgr.sessions[&code].state.clone();
        mgr.sessions.get_mut(&code).unwrap().pending_takeback = Some(PendingTakeback {
            requested_by: acting,
            target_state,
            approvals: HashSet::new(),
            history_truncate_len: 0,
        });

        let before_slots = mgr.sessions[&code].state.active_interaction_slots.clone();
        let error = mgr
            .handle_interaction(&code, acting_token, witness.clone())
            .expect_err("the table is voting; the state must not move");
        assert_eq!(error, "That action is not allowed right now.");
        assert_eq!(
            mgr.sessions[&code].state.active_interaction_slots,
            before_slots
        );

        // Reach guard: the very same submission succeeds once the interlock is
        // cleared, so the refusal above is attributable to the takeback.
        mgr.sessions.get_mut(&code).unwrap().pending_takeback = None;
        mgr.handle_interaction(&code, acting_token, witness)
            .expect("with no pending takeback the same submission applies");
    }

    /// Parks a one-entry stack in front of a human seat that has already been
    /// passed to, with the AI seat's pass for this cycle already recorded, so
    /// the human's Resolve All has exactly one representative left to ask.
    fn ai_table_awaiting_one_consent() -> (SessionManager, String, PlayerId) {
        let mut mgr = SessionManager::new();
        let (game_code, _token) = mgr.create_game(make_deck());
        let ai_player = PlayerId(1);
        let session = mgr
            .sessions
            .get_mut(&game_code)
            .expect("a new game retains its session");
        session.ai_seats.insert(ai_player);
        session.ai_configs.insert(
            ai_player,
            phase_ai::config::create_config_for_players(AiDifficulty::Easy, Platform::Native, 2),
        );
        let stack_object = ObjectId(1);
        session.state.active_player = ai_player;
        session.state.priority_player = PlayerId(0);
        session.state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        session.state.priority_passes.insert(ai_player);
        session.state.stack.push_back(StackEntry {
            id: stack_object,
            source_id: stack_object,
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: stack_object,
                ability: Box::new(ResolvedAbility::new(
                    Effect::NoOp,
                    Vec::new(),
                    stack_object,
                    PlayerId(0),
                )),
            },
        });
        (mgr, game_code, ai_player)
    }

    /// CR 117.3d: a human Resolve All at a table with AI seats enters the same
    /// fenced session used by direct `UntilStackEmpty` without asking the AI for
    /// anything. The AI seat owing a response is exactly what used to park the
    /// human on a consent prompt they could not clear.
    #[test]
    fn resolve_all_at_an_ai_table_needs_no_ai_consent() {
        let (mut mgr, game_code, _ai_player) = ai_table_awaiting_one_consent();
        let session = mgr
            .sessions
            .get_mut(&game_code)
            .expect("the game retains its session");

        apply(
            &mut session.state,
            PlayerId(0),
            GameAction::BeginResolveAll {
                max_resolutions: 1,
                scope: ResolveAllScope::Own,
            },
        )
        .expect("the priority holder may start Resolve All");
        assert!(
            !matches!(
                session.state.waiting_for,
                WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. }
            ),
            "the human must never be parked on an AI's consent, got {:?}",
            session.state.waiting_for
        );
        assert!(
            session.state.resolve_all_consent_run.is_none(),
            "the requester's single-participant run materializes immediately"
        );

        session.run_ai();

        assert!(session.state.stack.is_empty());
        assert!(session.state.resolve_all_consent_run.is_none());
        assert!(session.state.stack_resolution_session.is_none());
        assert!(!matches!(
            session.state.waiting_for,
            WaitingFor::ResolveAllReady { .. }
        ));
    }

    /// The public server driver observes the final state after the engine-owned
    /// runner, with no extra Resolve All transport action.
    #[test]
    fn run_ai_publishes_the_completed_shared_session() {
        let (mut mgr, game_code, _ai_player) = ai_table_awaiting_one_consent();
        let session = mgr
            .sessions
            .get_mut(&game_code)
            .expect("the game retains its session");

        apply(
            &mut session.state,
            PlayerId(0),
            GameAction::BeginResolveAll {
                max_resolutions: 1,
                scope: ResolveAllScope::Own,
            },
        )
        .expect("the priority holder may start Resolve All");

        let transitions = session.run_ai();

        assert!(
            session.state.stack.is_empty(),
            "the consented entry must have resolved, stack: {:?}",
            session.state.stack
        );
        assert!(
            session.state.resolve_all_consent_run.is_none(),
            "one run authorizes one batch and must not outlive it"
        );
        // CR 117.3d: an `Own` run asks nobody, so the AI-grant round-trip that
        // used to supply a second transition here no longer happens -- that
        // round-trip is the defect this test's fixture reproduces. What must
        // still hold is that the engine-owned runner published, and that no
        // frame it published ever parked a player on a consent decision.
        assert!(
            !transitions.transitions.is_empty(),
            "the engine-owned runner must publish the post-collapse state"
        );
        assert!(
            transitions
                .transitions
                .iter()
                .all(|(_, (broadcast_state, ..))| !matches!(
                    broadcast_state.waiting_for,
                    WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. }
                )),
            "a published frame parked a player on a Resolve All consent decision"
        );
        assert!(
            transitions
                .transitions
                .iter()
                .any(|(_, (broadcast_state, ..))| broadcast_state.stack.is_empty()),
            "no broadcast frame carried the post-collapse state"
        );
    }

    /// The same rule read from the other side: an AI seat's Resolve All is that
    /// seat's own pre-commitment too, so it never interrupts the human for a
    /// consent decision and never leaves a Ready latch behind.
    #[test]
    fn an_ai_resolve_all_does_not_prompt_the_human() {
        let (mut mgr, game_code, ai_player) = ai_table_awaiting_one_consent();
        let session = mgr
            .sessions
            .get_mut(&game_code)
            .expect("the game retains its session");
        // Flip the roles: the AI seat is the one starting Resolve All.
        session.state.priority_player = ai_player;
        session.state.waiting_for = WaitingFor::Priority { player: ai_player };
        session.state.priority_passes.clear();
        session.state.priority_passes.insert(PlayerId(0));

        apply(
            &mut session.state,
            ai_player,
            GameAction::BeginResolveAll {
                max_resolutions: 1,
                scope: ResolveAllScope::Own,
            },
        )
        .expect("the priority holder may start Resolve All");

        assert!(
            !matches!(
                session.state.waiting_for,
                WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. }
            ),
            "the human is never asked to approve another seat's Resolve All, got {:?}",
            session.state.waiting_for
        );
        assert!(
            session.state.resolve_all_consent_run.is_none(),
            "the requester's single-participant run materializes immediately"
        );
        assert!(session.state.stack.is_empty());
        assert!(session.state.stack_resolution_session.is_none());
    }

    /// Generic reconstruction deliberately preserves a coherent automation
    /// session. The restore owner must opt into the one explicit resume after
    /// it finishes attaching runtime authority.
    #[test]
    fn restored_session_resumes_a_coherent_automation_once_on_explicit_request() {
        let (mut mgr, game_code, ai_player) = ai_table_awaiting_one_consent();
        let session = mgr
            .sessions
            .get_mut(&game_code)
            .expect("the game retains its session");
        let initial_stack_len = session.state.stack.len();
        let entry = session
            .state
            .stack
            .back()
            .expect("fixture retains a stack entry");
        session.state.stack_resolution_session = Some(StackResolutionSession {
            entries: vec![StackResolutionEntryFence::capture(entry)],
            cursor: 0,
            representatives: BTreeSet::from([PlayerId(0), ai_player]),
            verified_pass_representatives: BTreeSet::new(),
            budget: StackResolutionBudget::Unlimited,
            policy: StackResolutionPolicy::Committed,
            auto_pass_overlay: StackResolutionAutoPassOverlay {
                baseline: BTreeMap::new(),
            },
        });
        for representative in [PlayerId(0), ai_player] {
            session.state.auto_pass.insert(
                representative,
                AutoPassMode::UntilStackEmpty {
                    initial_stack_len,
                    policy: StackResolutionPolicy::Committed,
                },
            );
        }
        assert!(session.state.stack_resolution_session.is_some());
        let revision_before = session.state_revision;
        let persisted = session.to_persisted();

        let mut restored = GameSession::from_persisted(persisted, &CardDatabase::default())
            .expect("the snapshot restores");

        assert!(
            restored.state.stack_resolution_session.is_some(),
            "generic restore must preserve the active shared session"
        );
        assert!(
            !restored.state.stack.is_empty(),
            "generic restore must not resolve a stack entry"
        );
        assert_eq!(restored.state_revision, revision_before);

        let resumed = restored.resume_restored_stack_automation();
        assert_eq!(
            resumed.presentation.outcome,
            RestoredStackAutomationOutcome::Progressed,
            "the coherent authorization enters the ordinary runner"
        );
        assert_eq!(resumed.state_revision, Some(revision_before + 1));
        let broadcast = resumed
            .broadcast
            .as_ref()
            .expect("resume recomputes broadcast");
        let (expected_legal, expected_costs, expected_by_object) =
            engine_legal_actions_full(&restored.state);
        assert_eq!(&broadcast.0, &restored.state);
        assert_eq!(&broadcast.1, &expected_legal);
        assert_eq!(
            broadcast.2,
            auto_pass_recommended(&restored.state, &broadcast.1)
        );
        assert_eq!(&broadcast.3, &expected_costs);
        assert_eq!(&broadcast.4, &expected_by_object);
        assert!(
            resumed.presentation.omitted_event_count > 0,
            "the runner's complete internal event batch must be accounted for"
        );
        let presentation_wire =
            serde_json::to_string(&resumed.presentation).expect("bounded presentation serializes");
        assert!(
            !presentation_wire.contains("events"),
            "the transport presentation must not serialize the internal event batch"
        );
        assert!(restored.state.stack.is_empty());
        assert!(restored.state.stack_resolution_session.is_none());

        let repeated = restored.resume_restored_stack_automation();
        assert_eq!(
            repeated.presentation.outcome,
            RestoredStackAutomationOutcome::Noop
        );
        assert_eq!(repeated.state_revision, None);
        assert!(repeated.broadcast.is_none());
        assert_eq!(
            restored.state_revision,
            revision_before + 1,
            "the explicit resume must be one-shot"
        );
    }

    /// Legacy Ready persistence is identified exclusively by its missing
    /// baseline; its explicit resume repair remains readable during migration.
    #[test]
    fn explicit_restore_resume_repairs_a_legacy_latch_whose_run_is_gone() {
        let (mut mgr, game_code, ai_player) = ai_table_awaiting_one_consent();
        let session = mgr
            .sessions
            .get_mut(&game_code)
            .expect("the game retains its session");
        // `Shared` is the scope that still opens a consent queue; a missing
        // baseline is then what identifies the run as the legacy encoding this
        // migration path repairs.
        apply(
            &mut session.state,
            PlayerId(0),
            GameAction::BeginResolveAll {
                max_resolutions: 1,
                scope: ResolveAllScope::Shared,
            },
        )
        .expect("the priority holder may start a shared Resolve All");
        let WaitingFor::ResolveAllConsent { epoch, .. } = session.state.waiting_for else {
            panic!("expected a pending consent prompt");
        };
        session
            .state
            .resolve_all_consent_run
            .as_mut()
            .expect("fresh consent run exists")
            .auto_pass_baseline = None;
        apply(
            &mut session.state,
            ai_player,
            GameAction::RespondResolveAllConsent {
                epoch,
                decision: ResolveAllConsentDecision::Grant,
            },
        )
        .expect("the AI representative may grant");
        session.state.resolve_all_consent_run = None;
        let persisted = session.to_persisted();

        let mut restored = GameSession::from_persisted(persisted, &CardDatabase::default())
            .expect("the snapshot restores");

        assert!(matches!(
            restored.state.waiting_for,
            WaitingFor::ResolveAllReady { .. }
        ));
        let revision_before = restored.state_revision;
        let resumed = restored.resume_restored_stack_automation();

        assert!(
            matches!(restored.state.waiting_for, WaitingFor::Priority { .. }),
            "a run-less latch must repair to ordinary priority, got {:?}",
            restored.state.waiting_for
        );
        assert_eq!(
            restored.state.stack.len(),
            1,
            "nothing may resolve without a run to authorize it"
        );
        assert_eq!(
            resumed.presentation.outcome,
            RestoredStackAutomationOutcome::ZeroResolutionRepair
        );
        assert_eq!(resumed.state_revision, Some(revision_before + 1));
        assert!(resumed.broadcast.is_some());
    }

    #[test]
    fn explicit_restore_resume_is_a_revision_preserving_noop_for_ordinary_priority() {
        let (mgr, game_code, _) = ai_table_awaiting_one_consent();
        let session = mgr.sessions.get(&game_code).expect("session exists");
        let persisted = session.to_persisted();
        let mut restored = GameSession::from_persisted(persisted, &CardDatabase::default())
            .expect("ordinary state restores");
        let revision_before = restored.state_revision;

        let resumed = restored.resume_restored_stack_automation();

        assert_eq!(
            resumed.presentation.outcome,
            RestoredStackAutomationOutcome::Noop
        );
        assert_eq!(resumed.state_revision, None);
        assert!(resumed.broadcast.is_none());
        assert_eq!(restored.state_revision, revision_before);
    }
}
