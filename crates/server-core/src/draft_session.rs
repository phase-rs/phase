use std::collections::HashMap;

use draft_core::pack_source::PackSource;
use draft_core::types::{
    DraftAction, DraftConfig, DraftDeckSubmission, DraftPairing, DraftSeat, DraftStatus,
    PackDistribution, PairingStatus,
};
use draft_core::view::DraftPlayerView;
use engine::database::ComboTable;
use engine::types::player::PlayerId;
use rand::Rng;
use tracing::{info, warn};

use seat_reducer::types::DeckChoice;

use crate::deck_resolve;
use crate::deck_resolve::deck_data_from_payload;
use crate::persist::{PersistedDraftSession, PersistedLobbyMeta};
use crate::protocol::DeckData;
use crate::reconnect::ReconnectManager;
use crate::session::{generate_player_token, SessionManager, HOST_AWAY_REFUSAL};

/// Server-side draft session, mirroring `GameSession` for game play.
/// Wraps `draft_core::types::DraftSession` (the pure reducer state) with
/// server-specific concerns: player tokens, connection tracking, reconnect
/// management, timer state, and active match tracking.
pub struct DraftSession {
    pub draft_code: String,
    pub session: draft_core::types::DraftSession,
    /// Per-seat player tokens (seat_index -> token). Empty string = seat not claimed.
    pub player_tokens: Vec<String>,
    pub connected: Vec<bool>,
    pub display_names: Vec<String>,
    pub config: DraftConfig,
    /// Active game sessions spawned at GeneratePairings. match_id -> game_code.
    pub active_matches: HashMap<String, String>,
    /// Lobby metadata -- set at creation, cleared when draft starts.
    pub lobby_meta: Option<PersistedLobbyMeta>,
    /// What drives this session between operations. Chosen once, at
    /// construction, from [`native_flow`]; never serialized.
    pub schedule: DraftSchedule,
}

/// What drives a native draft session between operations. Process-local: a
/// restore builds a fresh schedule, so nothing here outlives its process.
pub enum DraftSchedule {
    /// The pod-wide pick timer of a legacy pick-and-pass or shared-stack pod.
    Legacy(LegacySchedule),
    /// A private FIFO session: its owner identity, its registered wake and any
    /// staged result not yet saved.
    Fifo(FifoSchedule),
}

/// The legacy pick timer's process-local state.
#[derive(Default)]
pub struct LegacySchedule {
    /// Server-side remaining pick timer in ms. Injected into DraftPlayerView before send.
    pub timer_remaining_ms: Option<u32>,
    /// JoinHandle for the active pick timer task (prevents double-fire).
    pub timer_task: Option<tokio::task::JoinHandle<()>>,
}

/// A FIFO session's process-local schedule. Its fields are private: only the
/// manager moves them.
pub struct FifoSchedule {
    owner: DraftOwnerId,
    wake: Option<FifoWake>,
    /// Boxed: a whole core session, which would otherwise dwarf the legacy arm.
    pending: Option<Box<PendingFifoResult>>,
}

impl FifoSchedule {
    /// A fresh owner with no registration and no unsaved result.
    fn new(owner: DraftOwnerId) -> Self {
        Self {
            owner,
            wake: None,
            pending: None,
        }
    }

    /// Clears the registration and aborts its task, if any.
    fn abort_wake(&mut self) {
        if let Some(task) = self.wake.take().and_then(|wake| wake.task) {
            task.abort();
        }
    }
}

/// The identity of one installed FIFO session. A later owner of the same code
/// gets a different identity, so nothing bound to this one moves to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftOwnerId(u64);

/// The authority a FIFO wake carries: which owner registered it, which
/// registration of that owner it is, and the core deadline it waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FifoWakeRegistration {
    owner: DraftOwnerId,
    serial: u64,
    due_ms: u64,
}

impl FifoWakeRegistration {
    /// The core deadline, in epoch milliseconds, this wake waits for.
    pub fn due_ms(&self) -> u64 {
        self.due_ms
    }
}

struct FifoWake {
    registration: FifoWakeRegistration,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// A staged FIFO result and the row it is saved as, held until the save lands.
struct PendingFifoResult {
    candidate: draft_core::types::DraftSession,
    row: String,
}

/// Which native runtime flow a core session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRowFlow {
    Legacy,
    Fifo,
}

/// The one classification of a core session: a core the legacy admission
/// accepts runs the legacy flow; every other core (a FIFO core, or a
/// legacy-flow core carrying FIFO state) is FIFO.
pub fn native_flow(core: &draft_core::types::DraftSession) -> NativeRowFlow {
    match core.validate_legacy_admission() {
        Ok(()) => NativeRowFlow::Legacy,
        Err(_) => NativeRowFlow::Fifo,
    }
}

/// Who submits a FIFO operation: a seat, resolved from its token, or the
/// server itself (start, control, clock expiry).
#[derive(Debug, Clone, Copy)]
pub enum DraftActor<'a> {
    Seat { token: &'a str },
    Server,
}

/// What staging a FIFO operation produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FifoStaged {
    /// The operation left the committed core as it was; nothing to save.
    Unchanged,
    /// A result is staged and must be saved before it is committed.
    Pending,
}

/// A validated private FIFO session and the row it will be saved as, not yet
/// installed in the manager.
pub struct StagedFifoSession {
    session: DraftSession,
    row: String,
}

impl StagedFifoSession {
    pub fn draft_code(&self) -> &str {
        &self.session.draft_code
    }

    pub fn row(&self) -> &str {
        &self.row
    }

    pub fn player_tokens(&self) -> &[String] {
        &self.session.player_tokens
    }
}

/// A FIFO operation is refused while an earlier result is unsaved.
pub const FIFO_RESULT_UNSAVED: &str = "the previous draft result has not been saved";
/// A FIFO operation names a session that runs the legacy flow.
pub const NOT_A_FIFO_SESSION: &str = "this draft session does not run the FIFO flow";
/// The internal FIFO restore was handed a legacy row.
pub const LEGACY_ROW_NOT_PRIVATE_FIFO: &str =
    "a legacy draft row restores only through the native restore";
/// A FIFO install names a code a session already holds.
pub const DRAFT_CODE_RESIDENT: &str = "a draft session already holds this code";

/// Refuses a FIFO core with the shared runtime text.
fn refuse_fifo_core(core: &draft_core::types::DraftSession) -> Result<(), String> {
    match native_flow(core) {
        NativeRowFlow::Legacy => Ok(()),
        NativeRowFlow::Fifo => Err(draft_core::types::FIFO_UNSUPPORTED_BY_RUNTIME.to_string()),
    }
}

/// Refuses a resident FIFO session on a public entry, before any other check.
fn refuse_fifo_resident(session: &DraftSession) -> Result<(), String> {
    refuse_fifo_core(&session.session)
}

impl DraftSession {
    /// Returns the seat index for the given token, if valid.
    pub fn seat_for_token(&self, token: &str) -> Option<usize> {
        self.player_tokens
            .iter()
            .position(|t| !t.is_empty() && t == token)
    }

    /// Returns the first unclaimed seat index, if any.
    pub fn first_open_seat(&self) -> Option<usize> {
        self.player_tokens.iter().position(|t| t.is_empty())
    }

    /// Returns true if all seats are claimed.
    pub fn is_full(&self) -> bool {
        self.player_tokens.iter().all(|t| !t.is_empty())
    }

    /// Resolve a draft seat to its current spawned game and corresponding
    /// player seat in that two-player game.
    pub fn active_match_for_seat(&self, seat: usize) -> Option<(&DraftPairing, &str, PlayerId)> {
        if self.session.status != DraftStatus::MatchInProgress {
            return None;
        }

        let draft_player = PlayerId(u8::try_from(seat).ok()?);
        let pairing = self.session.pairings.iter().find(|pairing| {
            pairing.round == self.session.current_round
                && pairing.status != PairingStatus::Complete
                && pairing.players.contains(&draft_player)
        })?;
        let game_player = pairing
            .players
            .iter()
            .position(|player| *player == draft_player)?;
        let game_code = self.active_matches.get(&pairing.match_id)?;

        Some((pairing, game_code, PlayerId(game_player as u8)))
    }

    /// Create a serializable snapshot for disk persistence.
    pub fn to_persisted(&self) -> PersistedDraftSession {
        self.persisted_with_core(&self.session)
    }

    /// This wrapper's row with `core` as its core session. A FIFO session
    /// carries no legacy timer, so its row writes none.
    fn persisted_with_core(&self, core: &draft_core::types::DraftSession) -> PersistedDraftSession {
        PersistedDraftSession {
            draft_code: self.draft_code.clone(),
            session: core.clone(),
            player_tokens: self.player_tokens.clone(),
            display_names: self.display_names.clone(),
            config: self.config.clone(),
            active_matches: self.active_matches.clone(),
            lobby_meta: self.lobby_meta.clone(),
            timer_remaining_ms: self
                .legacy_schedule()
                .and_then(|legacy| legacy.timer_remaining_ms),
        }
    }

    /// The legacy pick timer, or `None` for a FIFO session.
    pub fn legacy_schedule(&self) -> Option<&LegacySchedule> {
        match &self.schedule {
            DraftSchedule::Legacy(legacy) => Some(legacy),
            DraftSchedule::Fifo(_) => None,
        }
    }

    /// The legacy pick timer, or `None` for a FIFO session.
    pub fn legacy_schedule_mut(&mut self) -> Option<&mut LegacySchedule> {
        match &mut self.schedule {
            DraftSchedule::Legacy(legacy) => Some(legacy),
            DraftSchedule::Fifo(_) => None,
        }
    }

    fn fifo_schedule(&self) -> Option<&FifoSchedule> {
        match &self.schedule {
            DraftSchedule::Legacy(_) => None,
            DraftSchedule::Fifo(fifo) => Some(fifo),
        }
    }

    fn fifo_schedule_mut(&mut self) -> Option<&mut FifoSchedule> {
        match &mut self.schedule {
            DraftSchedule::Legacy(_) => None,
            DraftSchedule::Fifo(fifo) => Some(fifo),
        }
    }

    /// Restore a draft session from a persisted snapshot.
    ///
    /// All players start disconnected. The legacy schedule has no timer task —
    /// the caller should re-arm from `timer_remaining_ms` if needed.
    pub fn from_persisted(ps: PersistedDraftSession) -> Result<Self, String> {
        Self::try_from_persisted(ps)
    }

    fn from_validated_persisted(ps: PersistedDraftSession, schedule: DraftSchedule) -> Self {
        let pod_size = ps.player_tokens.len();
        Self {
            draft_code: ps.draft_code,
            session: ps.session,
            player_tokens: ps.player_tokens,
            connected: vec![false; pod_size],
            display_names: ps.display_names,
            config: ps.config,
            active_matches: ps.active_matches,
            lobby_meta: ps.lobby_meta,
            schedule,
        }
    }

    /// Validate a disk snapshot before it becomes a live server session.
    ///
    /// The public restore: a FIFO row is refused with the shared runtime text
    /// before its snapshot is validated.
    fn try_from_persisted(ps: PersistedDraftSession) -> Result<Self, String> {
        Self::check_persisted_wrapper(&ps)?;
        match native_flow(&ps.session) {
            NativeRowFlow::Fifo => Err(draft_core::types::FIFO_UNSUPPORTED_BY_RUNTIME.to_string()),
            NativeRowFlow::Legacy => {
                ps.session
                    .validate_persisted_snapshot()
                    .map_err(|error| format!("invalid persisted draft snapshot: {error}"))?;
                let schedule = DraftSchedule::Legacy(LegacySchedule {
                    timer_remaining_ms: ps.timer_remaining_ms,
                    timer_task: None,
                });
                Ok(Self::from_validated_persisted(ps, schedule))
            }
        }
    }

    /// The wrapper checks every restore shares: the row's code, configuration
    /// and seat vectors agree with its core session, and its tokens are unique.
    fn check_persisted_wrapper(ps: &PersistedDraftSession) -> Result<(), String> {
        let core = &ps.session;
        let seat_count = core.seats.len();
        if ps.draft_code != core.draft_code {
            return Err("persisted draft code does not match core session".to_string());
        }
        if ps.config != core.config {
            return Err("persisted draft configuration does not match core session".to_string());
        }
        if ps.config.pod_size as usize != seat_count
            || ps.player_tokens.len() != seat_count
            || ps.display_names.len() != seat_count
        {
            return Err("persisted draft seat vectors do not match core seats".to_string());
        }
        let mut tokens = std::collections::HashSet::new();
        if ps
            .player_tokens
            .iter()
            .filter(|token| !token.is_empty())
            .any(|token| !tokens.insert(token))
        {
            return Err("persisted draft player tokens must be unique".to_string());
        }
        Ok(())
    }

    /// Inject server-side timer into the filtered view before serializing. A
    /// FIFO session has no legacy timer to inject.
    pub fn view_for_seat(&self, seat: usize) -> DraftPlayerView {
        let mut view = draft_core::view::filter_for_player(&self.session, seat as u8);
        if let Some(legacy) = self.legacy_schedule() {
            view.timer_remaining_ms = legacy.timer_remaining_ms;
        }
        view
    }
}

/// Seats that still owe a pick this round and have not yet submitted one.
/// Skips seats already recorded in `seats_picked_this_round` so auto-pick
/// sweeps do not hit `SeatAlreadyPickedThisRound` (issue #1193).
pub fn draft_seats_needing_auto_pick(
    session: &mut draft_core::types::DraftSession,
    pod_size: usize,
) -> Vec<usize> {
    let pod_size_u8 = pod_size as u8;
    session
        .seats_picked_this_round
        .ensure_len(pod_size_u8, false);
    // Dispatched on the distribution rather than filtered by a pick-and-pass
    // conjunct. The `current_pack` filter below DOMINATES: a shared-stack
    // session leaves `current_pack` all-`None`, so it returns an empty sweep
    // for every such session and any arm placed below it would be dead code.
    match session.kind.procedure().distribution {
        PackDistribution::PickAndPass => (0..pod_size)
            .filter(|&seat_idx| {
                if session.seats_picked_this_round.get(seat_idx as u8) {
                    return false;
                }
                session.current_pack[seat_idx]
                    .as_ref()
                    .is_some_and(|pack| !pack.0.is_empty())
            })
            .collect(),
        // Exactly one seat owes a decision at a time, and it is the engine's
        // `active_seat`. No stack means no session-owed decision at all.
        PackDistribution::SharedStackPiles { .. } => session
            .shared_stack
            .as_ref()
            .map(|state| usize::from(state.active_seat))
            .filter(|seat| *seat < pod_size)
            .into_iter()
            .collect(),
        // An all-at-once kind opens directly in deckbuilding: it has no
        // decision window for a timer to expire on.
        PackDistribution::AllAtOnce => Vec::new(),
    }
}

pub struct DraftSessionManager {
    pub sessions: HashMap<String, DraftSession>,
    pub reconnect: ReconnectManager,
    /// Maps player_token -> draft_code for O(1) token-based lookups.
    token_to_draft: HashMap<String, String>,
    /// The card data this server hosts the engine with. A deck submission's
    /// companion designation is checked against it (CR 702.139a/b); without it
    /// a designation fails closed.
    card_db: Option<std::sync::Arc<engine::database::CardDatabase>>,
    /// The last identity issued to a FIFO owner or wake registration. Never
    /// persisted: nothing that holds an identity outlives its process.
    next_serial: u64,
}

impl DraftSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::default(),
            token_to_draft: HashMap::new(),
            card_db: None,
            next_serial: 0,
        }
    }

    /// Issues a fresh identity for a FIFO owner or a wake registration.
    fn mint(&mut self) -> u64 {
        self.next_serial += 1;
        self.next_serial
    }

    /// Supply the server's card database so deck submissions can be checked
    /// against the engine's companion eligibility authority.
    pub fn with_card_db(mut self, db: std::sync::Arc<engine::database::CardDatabase>) -> Self {
        self.card_db = Some(db);
        self
    }

    /// Create a new draft session. Returns (draft_code, player_token, seat_index).
    ///
    /// The creator occupies seat 0. Remaining seats are empty (awaiting joins).
    pub fn create_draft(
        &mut self,
        config: DraftConfig,
        display_name: String,
    ) -> (String, String, u8) {
        let draft_code = generate_draft_code();
        let player_token = generate_player_token();
        let pod_size = config.pod_size as usize;

        let mut player_tokens = vec![String::new(); pod_size];
        player_tokens[0] = player_token.clone();

        let mut connected = vec![false; pod_size];
        connected[0] = true;

        let mut display_names = vec![String::new(); pod_size];
        display_names[0] = display_name.clone();

        // Build draft-core seats -- creator is seat 0, rest are empty humans.
        // Runtime connection state lives in `inner.connected_seats` (post-init
        // mutation below); the wrapper `connected: Vec<bool>` retains its
        // legacy operational role as the local truth.
        let seats: Vec<DraftSeat> = (0..pod_size)
            .map(|i| DraftSeat::Human {
                player_id: PlayerId(i as u8),
                display_name: if i == 0 {
                    display_name.clone()
                } else {
                    String::new()
                },
            })
            .collect();

        let mut inner =
            draft_core::types::DraftSession::new(config.clone(), seats, draft_code.clone());
        // Mirror the initial "only the creator is connected" state into the
        // engine bitmap so `DraftPlayerView.seats[*].connected` reflects it.
        for i in 0..pod_size {
            let _ = draft_core::session::apply(
                &mut inner,
                draft_core::types::DraftAction::SetSeatConnected {
                    seat: i as u8,
                    connected: i == 0,
                },
                None,
            );
        }

        let session = DraftSession {
            draft_code: draft_code.clone(),
            session: inner,
            player_tokens,
            connected,
            display_names,
            config,
            active_matches: HashMap::new(),
            lobby_meta: None,
            schedule: DraftSchedule::Legacy(LegacySchedule::default()),
        };

        self.token_to_draft
            .insert(player_token.clone(), draft_code.clone());
        self.sessions.insert(draft_code.clone(), session);

        info!(draft = %draft_code, "draft session created");

        (draft_code, player_token, 0)
    }

    /// Join an existing draft. Returns (player_token, seat_index, initial_view).
    pub fn join_draft(
        &mut self,
        draft_code: &str,
        display_name: String,
        password: Option<&str>,
    ) -> Result<(String, u8, DraftPlayerView), String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;
        refuse_fifo_resident(session)?;

        if let Some(meta) = &session.lobby_meta {
            match (&meta.password, password) {
                (None, _) => {}
                (Some(_), None) => return Err("password_required".to_string()),
                (Some(expected), Some(provided)) if expected == provided => {}
                (Some(_), Some(_)) => return Err("Wrong password".to_string()),
            }
        }

        if session.session.status != DraftStatus::Lobby {
            return Err("Draft has already started".to_string());
        }
        if !session.connected[DRAFT_HOST_SEAT] {
            return Err(HOST_AWAY_REFUSAL.to_string());
        }

        let seat = session
            .first_open_seat()
            .ok_or_else(|| "Draft is already full".to_string())?;

        let player_token = generate_player_token();
        session.player_tokens[seat] = player_token.clone();
        session.connected[seat] = true;
        session.display_names[seat] = display_name.clone();

        // Update the draft-core seat
        session.session.seats[seat] = DraftSeat::Human {
            player_id: PlayerId(seat as u8),
            display_name,
        };
        // Mirror the connection state into the engine bitmap so the view
        // layer reflects it. Best-effort; the wrapper `connected` is the
        // local operational source.
        let _ = draft_core::session::apply(
            &mut session.session,
            draft_core::types::DraftAction::SetSeatConnected {
                seat: seat as u8,
                connected: true,
            },
            None,
        );

        self.token_to_draft
            .insert(player_token.clone(), draft_code.to_string());

        info!(draft = %draft_code, seat, "player joined draft");

        let view = session.view_for_seat(seat);
        Ok((player_token, seat as u8, view))
    }

    /// Handle a draft action from a player. Validates token -> seat mapping
    /// before delegating to draft-core. Returns views for all seats.
    pub fn handle_draft_action(
        &mut self,
        draft_code: &str,
        token: &str,
        action: DraftAction,
        pack_source: Option<&dyn PackSource>,
    ) -> Result<Vec<DraftPlayerView>, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;

        let seat = session
            .seat_for_token(token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        // The WebSocket wire surface is untrusted: the client supplies the
        // action's `seat` field, but the only authority for "who is acting" is
        // the token → seat mapping resolved above. Bind seat-scoped actions to
        // that seat and gate table-authority actions to the host (seat 0).
        // `apply_system_action` deliberately bypasses this for trusted
        // server-internal actions (auto-pick on timer expiry, GameOver
        // auto-report), so bot/auto picks are unaffected.
        let action = authorize_client_draft_action(seat, action)?;
        admit_dormant_action(&session.session, &action)?;
        check_submit_companion(&session.session, self.card_db.as_deref(), &action)?;

        let clears_lobby = matches!(action, DraftAction::StartDraft);

        let _deltas = draft_core::session::apply(&mut session.session, action, pack_source)
            .map_err(|e| {
                warn!(draft = %draft_code, error = %e, "draft action rejected");
                format!("Draft error: {}", e)
            })?;

        if clears_lobby {
            session.lobby_meta = None;
        }

        // Broadcast updated view to all connected seats
        let views: Vec<_> = (0..session.player_tokens.len())
            .map(|i| session.view_for_seat(i))
            .collect();
        Ok(views)
    }

    /// Apply an action without token validation (for server-internal use,
    /// e.g. GameOver auto-report). Lock ordering: always acquire draft_sessions
    /// before sessions (game sessions) to prevent deadlock.
    pub fn apply_system_action(
        &mut self,
        draft_code: &str,
        action: DraftAction,
        pack_source: Option<&dyn PackSource>,
    ) -> Result<Vec<DraftPlayerView>, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;
        admit_dormant_action(&session.session, &action)?;
        check_submit_companion(&session.session, self.card_db.as_deref(), &action)?;

        let clears_lobby = matches!(action, DraftAction::StartDraft);

        let _deltas = draft_core::session::apply(&mut session.session, action, pack_source)
            .map_err(|e| {
                warn!(draft = %draft_code, error = %e, "system draft action rejected");
                format!("Draft error: {}", e)
            })?;

        if clears_lobby {
            session.lobby_meta = None;
        }

        let views: Vec<_> = (0..session.player_tokens.len())
            .map(|i| session.view_for_seat(i))
            .collect();
        Ok(views)
    }

    /// Mark a player as disconnected.
    pub fn handle_disconnect(
        &mut self,
        draft_code: &str,
        seat: usize,
        disconnected_at: std::time::Instant,
    ) {
        if let Some(session) = self.sessions.get_mut(draft_code) {
            // A FIFO session's seats record no reconnect lapse here; its
            // reconnect grace lands with native admission.
            if refuse_fifo_resident(session).is_err() {
                return;
            }
            session.connected[seat] = false;
            // Mirror into the engine bitmap so the view reflects it.
            let _ = draft_core::session::apply(
                &mut session.session,
                draft_core::types::DraftAction::SetSeatConnected {
                    seat: seat as u8,
                    connected: false,
                },
                None,
            );
            let fake_pid = PlayerId(seat as u8);
            let default_grace = self.reconnect.grace_period;
            self.reconnect
                .record_disconnect(draft_code, fake_pid, default_grace, disconnected_at);
            info!(draft = %draft_code, seat, "player disconnected");
        }
    }

    /// Attempt to reconnect a player. Returns their filtered view on success.
    pub fn handle_reconnect(
        &mut self,
        draft_code: &str,
        token: &str,
        received_at: std::time::Instant,
    ) -> Result<DraftPlayerView, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;
        refuse_fifo_resident(session)?;

        let seat = session
            .seat_for_token(token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        let fake_pid = PlayerId(seat as u8);
        match self
            .reconnect
            .attempt_reconnect(draft_code, fake_pid, received_at)
        {
            crate::reconnect::ReconnectResult::Ok { .. }
            | crate::reconnect::ReconnectResult::NotFound => {
                session.connected[seat] = true;
                // Mirror into the engine bitmap so the view reflects it.
                let _ = draft_core::session::apply(
                    &mut session.session,
                    draft_core::types::DraftAction::SetSeatConnected {
                        seat: seat as u8,
                        connected: true,
                    },
                    None,
                );
                Ok(session.view_for_seat(seat))
            }
            crate::reconnect::ReconnectResult::Expired => {
                Err("Reconnect grace period expired".to_string())
            }
        }
    }

    /// O(1) lookup: player_token -> draft_code.
    pub fn draft_for_token(&self, token: &str) -> Option<&str> {
        self.token_to_draft.get(token).map(|s| s.as_str())
    }

    /// Restore a previously persisted draft session, rebuilding the token_to_draft index.
    pub fn restore_session(&mut self, ps: PersistedDraftSession) -> Result<(), String> {
        self.restore_persisted_session(ps)
    }

    /// Restore an untrusted persisted snapshot only after its wrapper and core
    /// session agree. The manager remains unchanged when validation fails.
    pub fn restore_persisted_session(&mut self, ps: PersistedDraftSession) -> Result<(), String> {
        let session = DraftSession::try_from_persisted(ps)?;
        let draft_code = session.draft_code.clone();
        let mut claimed = Vec::new();
        for (seat, token) in session.player_tokens.iter().enumerate() {
            if !token.is_empty() {
                self.token_to_draft
                    .insert(token.clone(), draft_code.clone());
                claimed.push(PlayerId(seat as u8));
            }
        }
        self.sessions.insert(draft_code.clone(), session);
        // A restored seat starts disconnected, so its grace starts now.
        let grace = self.reconnect.grace_period;
        for player in claimed {
            self.reconnect
                .record_disconnect(&draft_code, player, grace, std::time::Instant::now());
        }
        Ok(())
    }

    /// Whether an unstarted pod's host is connected or inside reconnect grace.
    pub fn is_live_lobby_pod(&self, draft_code: &str) -> bool {
        self.sessions.get(draft_code).is_some_and(|session| {
            session.session.status == DraftStatus::Lobby
                && (session.connected[DRAFT_HOST_SEAT]
                    || self
                        .reconnect
                        .is_disconnected(draft_code, PlayerId(DRAFT_HOST_SEAT as u8)))
        })
    }

    /// Acts on every lapsed seat: a `Lobby` host's pod is removed, a `Lobby`
    /// guest's seat is freed, a `Drafting` seat is auto-picked. Consumes each
    /// lapse in the critical section that acts on it.
    pub fn sweep_expired_seats(&mut self, as_of: std::time::Instant) -> DraftSeatSweep {
        let mut sweep = DraftSeatSweep::default();
        for (draft_code, player) in self.reconnect.check_expired_with_players(as_of) {
            let seat = usize::from(player.0);
            if !sweep.affected.contains(&draft_code) {
                sweep.affected.push(draft_code.clone());
            }
            let Some(session) = self.sessions.get(&draft_code) else {
                continue;
            };
            if session.connected[seat] {
                continue;
            }
            match session.session.status {
                DraftStatus::Drafting => {
                    match self.pick_random_for_seat(&draft_code, player.0, None) {
                        Ok(()) => {
                            info!(draft = %draft_code, seat, "auto-picked for disconnected seat (grace expired)");
                        }
                        Err(error) => {
                            warn!(draft = %draft_code, seat, %error, "auto-pick on grace expiry failed");
                        }
                    }
                }
                DraftStatus::Lobby if seat == DRAFT_HOST_SEAT => {
                    if let Some(removed) = self.remove_draft(&draft_code) {
                        info!(draft = %draft_code, "reaped a lobby pod whose host's grace expired");
                        sweep.reaped.push((draft_code, removed));
                    }
                }
                DraftStatus::Lobby => {
                    self.free_seat(&draft_code, seat);
                    if !sweep.freed.contains(&draft_code) {
                        sweep.freed.push(draft_code);
                    }
                }
                DraftStatus::Deckbuilding
                | DraftStatus::Pairing
                | DraftStatus::MatchInProgress
                | DraftStatus::RoundComplete
                | DraftStatus::Paused
                | DraftStatus::Review
                | DraftStatus::Complete
                | DraftStatus::Abandoned => {}
            }
        }
        sweep
            .affected
            .retain(|code| self.sessions.contains_key(code));
        sweep.freed.retain(|code| self.sessions.contains_key(code));
        sweep
    }

    /// Releases an unstarted pod's guest seat so its token no longer resolves.
    fn free_seat(&mut self, draft_code: &str, seat: usize) {
        let Some(session) = self.sessions.get_mut(draft_code) else {
            return;
        };
        let token = std::mem::take(&mut session.player_tokens[seat]);
        self.token_to_draft.remove(&token);
        session.display_names[seat].clear();
        session.session.seats[seat] = DraftSeat::Human {
            player_id: PlayerId(seat as u8),
            display_name: String::new(),
        };
        info!(draft = %draft_code, seat, "freed a lobby seat whose grace expired");
    }

    /// Auto-pick a random card for a disconnected seat whose grace period expired.
    ///
    /// Returns `Ok(())` if a pick was made. Only fires during the Drafting phase (D-02).
    /// Called from the draft disconnect expiry path in phase-server.
    pub fn pick_random_for_seat(
        &mut self,
        draft_code: &str,
        seat: u8,
        pack_source: Option<&dyn PackSource>,
    ) -> Result<(), String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("draft {draft_code} not found"))?;

        if session.session.status != DraftStatus::Drafting {
            return Err("draft not in Drafting status".into());
        }

        // THE DISPATCH SITS ABOVE the `view.current_pack` read below, which is
        // this function's own dominating conjunct: a shared-stack session
        // leaves `current_pack` all-`None`, so an arm placed BELOW it would be
        // dead and would silently return "no pending pack" instead of driving
        // the turn.
        match session.session.kind.procedure().distribution {
            PackDistribution::SharedStackPiles { .. } => {
                // `forced_decision` is asked of the ONE legality authority; it
                // is not an AI and not a pile evaluation. `None` here is the
                // REACHABLE case, not an impossibility: the disconnect-expiry
                // caller passes the DISCONNECTED seat, which for a shared-stack
                // pod need not be the active one. Refusing is the only right
                // answer -- driving the ACTIVE seat's turn because a DIFFERENT
                // seat disconnected would be a rules violation dressed as
                // robustness -- and it must mutate nothing.
                let (decision, pile) = {
                    let state = session
                        .session
                        .shared_stack
                        .as_ref()
                        .ok_or_else(|| format!("draft {draft_code} has no shared stack"))?;
                    let decision = draft_core::shared_stack::forced_decision(state, seat)
                        .ok_or_else(|| {
                            format!("seat {seat} is not the active shared-stack seat")
                        })?;
                    // The cursor is read from the SAME state `forced_decision`
                    // was asked about. Any other value is refused
                    // `SharedStackRefusal::PileNotActive` -- correct behaviour,
                    // but a silently no-op timeout path -- so the cursor is the
                    // contract here and not an implementation detail.
                    (decision, state.cursor)
                };
                let action = DraftAction::SharedStackDecision {
                    seat,
                    pile,
                    decision,
                };
                draft_core::session::apply(&mut session.session, action, pack_source)
                    .map_err(|e| format!("auto-pick failed: {e}"))?;
                info!(draft = %draft_code, seat, ?decision, "drove a shared-stack turn for a stalled seat");
                return Ok(());
            }
            // `PickAndPass` keeps today's body verbatim below. `AllAtOnce`
            // reaches the same body and lands on its existing "no pending pack"
            // refusal, which is the right answer: it has no pick step.
            PackDistribution::PickAndPass | PackDistribution::AllAtOnce => {}
        }

        let view = draft_core::view::filter_for_player(&session.session, seat);
        let pack = view
            .current_pack
            .ok_or_else(|| format!("seat {seat} has no pending pack"))?;

        if pack.is_empty() {
            return Err(format!("seat {seat} pack is empty"));
        }

        // CR 903.13b: a seat's pick step takes its kind's whole card count —
        // one for the four CR 905.1a kinds, two for CommanderDraft — dropping
        // to the remainder on an odd final pick. `Winston` reports `1` on that
        // axis but takes no pick step at all and never reaches this line: the
        // `SharedStackPiles` arm above returned before it. Reading the count
        // from the
        // procedure is what keeps the disconnected-seat auto-pick and the
        // reducer's `expected` in agreement by construction rather than by
        // coincidence. Was a hardcoded single id, which stalled a Commander pod
        // at `WrongPickCardCount`.
        //
        // The other half of the discharged marker, decided here so it is not
        // re-asked: `guard_create_draft_with_settings` does NOT gain a kind
        // allowlist. `DraftKind`'s deserialization already refuses every
        // non-variant, so an allowlist would re-express the type system as a
        // runtime string check; the guard's job is bounding client-supplied
        // strings and sizes before clone-heavy work, not policy; every kind is
        // now genuinely creatable server-side, so there is nothing to deny; and
        // an allowlist would become a second authority over which kinds exist,
        // competing with `DraftKind::ALL`.
        let cards_per_pick =
            usize::from(session.session.config.kind.procedure().cards_per_pick).min(pack.len());
        let mut rng = rand::rng();
        // `swap_remove` off a scratch Vec draws DISTINCT ids; drawing twice by
        // index into the pack could pick the same card twice, which the reducer
        // rejects.
        let mut remaining: Vec<String> = pack.iter().map(|c| c.instance_id.clone()).collect();
        let card_instance_ids: Vec<String> = (0..cards_per_pick)
            .map(|_| remaining.swap_remove(rng.random_range(0..remaining.len())))
            .collect();

        let action = DraftAction::Pick {
            seat,
            card_instance_ids,
        };
        draft_core::session::apply(&mut session.session, action, pack_source)
            .map_err(|e| format!("auto-pick failed: {e}"))?;

        info!(draft = %draft_code, seat, "auto-picked random card for disconnected seat");
        Ok(())
    }

    /// Server-internal: generate Swiss/SE pairings when the pod reaches `Pairing`.
    pub fn ensure_pairings_generated(&mut self, draft_code: &str) -> Result<(), String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {draft_code}"))?;
        if session.session.status != DraftStatus::Pairing {
            return Ok(());
        }
        // The reducer derives the round (`DraftSession::next_pairing_round`);
        // deriving it here is what made round 2 unreachable.
        draft_core::session::apply(&mut session.session, DraftAction::GeneratePairings, None)
            .map_err(|e| format!("GeneratePairings failed: {e}"))?;
        Ok(())
    }

    /// Spawn 2-player game sessions for pending pairings in `round` using submitted decks.
    pub fn spawn_match_games_for_round(
        &mut self,
        draft_code: &str,
        game_mgr: &mut SessionManager,
        db: &std::sync::Arc<engine::database::CardDatabase>,
        combos: &ComboTable,
        round: u8,
    ) -> Result<Vec<DraftMatchSpawn>, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {draft_code}"))?;

        let match_config = session.config.kind.match_config();
        let format_config = engine::types::format::FormatConfig::limited();
        let mut spawns = Vec::new();

        let pairings: Vec<DraftPairing> = session
            .session
            .pairings
            .iter()
            .filter(|p| p.round == round && p.status == PairingStatus::Pending)
            .cloned()
            .collect();

        for pairing in pairings {
            if session.active_matches.contains_key(&pairing.match_id) {
                continue;
            }

            let deck_payloads: Result<Vec<_>, String> = pairing
                .players
                .iter()
                .map(|pid| {
                    let seat = pid.0 as usize;
                    let submission = session.session.submitted_decks.get(pid).ok_or_else(|| {
                        format!(
                            "seat {} has not submitted a deck for match {}",
                            seat, pairing.match_id
                        )
                    })?;
                    deck_payload_from_submission(db, combos, submission)
                })
                .collect();
            let decks = match deck_payloads {
                Ok(decks) => decks,
                Err(error) => {
                    warn!(
                        draft = %draft_code,
                        match_id = %pairing.match_id,
                        error = %error,
                        "draft match spawn skipped for pairing"
                    );
                    continue;
                }
            };

            let seat0 = pairing.players[0].0 as usize;
            let seat1 = pairing.players[1].0 as usize;
            let name0 = session
                .display_names
                .get(seat0)
                .cloned()
                .unwrap_or_else(|| format!("Player {}", seat0));
            let name1 = session
                .display_names
                .get(seat1)
                .cloned()
                .unwrap_or_else(|| format!("Player {}", seat1));

            // A draft deck arrives already resolved, so each seat's provenance
            // is recovered from its payload against the same database.
            let choice0 = DeckChoice::DeckList(Box::new(deck_data_from_payload(db, &decks[0])));
            let choice1 = DeckChoice::DeckList(Box::new(deck_data_from_payload(db, &decks[1])));
            let (game_code, _token0) = game_mgr.create_game_n_players(
                decks[0].clone(),
                Some(choice0),
                name0,
                None,
                2,
                match_config,
                Some(format_config.clone()),
            )?;
            // The manager created this game two statements ago and nothing has
            // handed out a handle, so the synchronous exclusive accessor is the
            // right one — `server-core` stays free of `async`.
            let token1 = {
                let session = game_mgr
                    .session_exclusive(&game_code)
                    .ok_or_else(|| format!("spawned game missing: {game_code}"))?;
                let (token, _) =
                    session.join_with_reservation(decks[1].clone(), Some(choice1), name1, None)?;
                session
                    .start_game(db)
                    .map_err(|e| format!("start_game failed for {game_code}: {e:?}"))?;
                token
            };
            game_mgr.index_token(token1, &game_code);

            session
                .active_matches
                .insert(pairing.match_id.clone(), game_code.clone());

            spawns.push(DraftMatchSpawn {
                match_id: pairing.match_id,
                round,
                game_code,
                player_a: DraftMatchPlayer {
                    draft_seat: pairing.players[0].0,
                    game_player: PlayerId(0),
                },
                player_b: DraftMatchPlayer {
                    draft_seat: pairing.players[1].0,
                    game_player: PlayerId(1),
                },
                opponent_names: [
                    session
                        .display_names
                        .get(seat1)
                        .cloned()
                        .unwrap_or_default(),
                    session
                        .display_names
                        .get(seat0)
                        .cloned()
                        .unwrap_or_default(),
                ],
            });
        }

        Ok(spawns)
    }

    /// Scan active_matches across all sessions to find the draft owning a game.
    pub fn draft_for_game_code(&self, game_code: &str) -> Option<String> {
        self.sessions
            .values()
            .find(|s| s.active_matches.values().any(|gc| gc == game_code))
            .map(|s| s.draft_code.clone())
    }

    /// Remove a draft session entirely, cleaning up the token_to_draft index.
    /// Returns the removed session if it existed.
    /// A removed FIFO owner's wake task is aborted.
    pub fn remove_draft(&mut self, draft_code: &str) -> Option<DraftSession> {
        let mut session = self.sessions.remove(draft_code)?;
        if let Some(fifo) = session.fifo_schedule_mut() {
            fifo.abort_wake();
        }
        for token in &session.player_tokens {
            if !token.is_empty() {
                self.token_to_draft.remove(token);
            }
        }
        self.reconnect.remove_game(draft_code);
        Some(session)
    }

    /// A FIFO schedule for a fresh owner.
    fn new_fifo_schedule(&mut self) -> DraftSchedule {
        DraftSchedule::Fifo(FifoSchedule::new(DraftOwnerId(self.mint())))
    }

    /// Pairs a validated FIFO session with the row it will be saved as.
    fn staged(session: DraftSession) -> Result<StagedFifoSession, String> {
        let row = serde_json::to_string(&session.to_persisted())
            .map_err(|error| format!("failed to serialize draft session: {error}"))?;
        Ok(StagedFifoSession { session, row })
    }

    /// The internal FIFO restore. Shares the public restore's wrapper step,
    /// then admits only a valid FIFO row: the classification runs before the
    /// snapshot validation, as in the public restore. Builds a fresh owner with
    /// no registration and no unsaved result; the manager's maps are untouched.
    pub fn stage_restored_fifo(
        &mut self,
        ps: PersistedDraftSession,
    ) -> Result<StagedFifoSession, String> {
        DraftSession::check_persisted_wrapper(&ps)?;
        match native_flow(&ps.session) {
            NativeRowFlow::Legacy => return Err(LEGACY_ROW_NOT_PRIVATE_FIFO.to_string()),
            NativeRowFlow::Fifo => {}
        }
        ps.session
            .validate_persisted_snapshot()
            .map_err(|error| format!("invalid persisted FIFO snapshot: {error}"))?;
        if ps.timer_remaining_ms.is_some() {
            return Err("a FIFO draft row carries no legacy pick timer".to_string());
        }
        if ps.lobby_meta.is_some() {
            return Err("a private FIFO draft row carries no lobby listing".to_string());
        }
        let schedule = self.new_fifo_schedule();
        Self::staged(DraftSession::from_validated_persisted(ps, schedule))
    }

    /// A private FIFO pod with one human seat per name, under a code no
    /// resident holds. Core validates the FIFO configuration and snapshot.
    pub fn stage_private_fifo(
        &mut self,
        config: DraftConfig,
        display_names: Vec<String>,
    ) -> Result<StagedFifoSession, String> {
        if display_names.len() != usize::from(config.pod_size) {
            return Err("a private FIFO pod names one player per seat".to_string());
        }
        let draft_code = loop {
            let code = generate_draft_code();
            if !self.sessions.contains_key(&code) {
                break code;
            }
        };
        let seats = display_names
            .iter()
            .enumerate()
            .map(|(seat, name)| {
                Ok(DraftSeat::Human {
                    player_id: PlayerId(
                        u8::try_from(seat).map_err(|_| "Invalid seat index".to_string())?,
                    ),
                    display_name: name.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let core = draft_core::types::DraftSession::new_fifo(
            config.clone(),
            seats,
            draft_code.clone(),
            draft_core::types::DraftSessionId(generate_player_token()),
        )
        .map_err(|error| format!("Draft error: {error}"))?;
        let pod_size = display_names.len();
        let session = DraftSession {
            draft_code,
            session: core,
            player_tokens: (0..pod_size).map(|_| generate_player_token()).collect(),
            connected: vec![false; pod_size],
            display_names,
            config,
            active_matches: HashMap::new(),
            lobby_meta: None,
            schedule: self.new_fifo_schedule(),
        };
        Self::staged(session)
    }

    /// Installs a staged FIFO session. Never replaces a resident: a code any
    /// session holds is refused. Its tokens are not indexed and no reconnect
    /// lapse is recorded, so no token lookup and no seat sweep reaches it.
    pub fn install_fifo(&mut self, staged: StagedFifoSession) -> Result<(), String> {
        let code = staged.session.draft_code.clone();
        if self.sessions.contains_key(&code) {
            return Err(DRAFT_CODE_RESIDENT.to_string());
        }
        self.sessions.insert(code, staged.session);
        Ok(())
    }

    /// The earliest running core deadline of a FIFO session, as draft-core's
    /// clock reports it. `None` when the code holds no FIFO session, a result
    /// is unsaved, or no clock runs. Decides nothing about expiry: core's
    /// `AdvanceClock` is the one expiry transaction.
    pub fn fifo_next_due(&self, draft_code: &str) -> Option<u64> {
        let session = self.sessions.get(draft_code)?;
        if session.fifo_schedule()?.pending.is_some() {
            return None;
        }
        draft_core::clock::next_running_deadline(&session.session)
    }

    /// Registers a new wake for a FIFO session's owner, replacing (and
    /// aborting) its previous one. The task is attached separately.
    pub fn register_fifo_wake(
        &mut self,
        draft_code: &str,
        due_ms: u64,
    ) -> Option<FifoWakeRegistration> {
        self.sessions
            .get(draft_code)
            .and_then(DraftSession::fifo_schedule)?;
        let serial = self.mint();
        let fifo = self
            .sessions
            .get_mut(draft_code)
            .and_then(DraftSession::fifo_schedule_mut)?;
        fifo.abort_wake();
        let registration = FifoWakeRegistration {
            owner: fifo.owner,
            serial,
            due_ms,
        };
        fifo.wake = Some(FifoWake {
            registration: registration.clone(),
            task: None,
        });
        Some(registration)
    }

    /// Stores a wake's task while its registration is current and has none;
    /// otherwise aborts the task.
    pub fn attach_fifo_wake_task(
        &mut self,
        draft_code: &str,
        registration: &FifoWakeRegistration,
        task: tokio::task::JoinHandle<()>,
    ) {
        let wake = self
            .sessions
            .get_mut(draft_code)
            .and_then(DraftSession::fifo_schedule_mut)
            .and_then(|fifo| fifo.wake.as_mut())
            .filter(|wake| wake.registration == *registration && wake.task.is_none());
        match wake {
            Some(wake) => wake.task = Some(task),
            None => task.abort(),
        }
    }

    /// Clears a FIFO session's wake and aborts its task.
    pub fn cancel_fifo_wake(&mut self, draft_code: &str) {
        if let Some(fifo) = self
            .sessions
            .get_mut(draft_code)
            .and_then(DraftSession::fifo_schedule_mut)
        {
            fifo.abort_wake();
        }
    }

    /// The FIFO session's current wake registration, if any.
    pub fn current_fifo_wake(&self, draft_code: &str) -> Option<FifoWakeRegistration> {
        self.sessions
            .get(draft_code)
            .and_then(DraftSession::fifo_schedule)?
            .wake
            .as_ref()
            .map(|wake| wake.registration.clone())
    }

    /// The wake's recheck under the manager lock. True only when the resident
    /// under `draft_code` is a FIFO session, its owner is the registration's,
    /// its current registration is the one the wake carries, and no result is
    /// unsaved. A claimed wake is cleared and its task handle dropped, not
    /// aborted: the caller is that task, and aborting it would cancel it at its
    /// next await, mid-operation.
    pub fn claim_fifo_wake(
        &mut self,
        draft_code: &str,
        registration: &FifoWakeRegistration,
    ) -> bool {
        let Some(fifo) = self
            .sessions
            .get_mut(draft_code)
            .and_then(DraftSession::fifo_schedule_mut)
        else {
            return false;
        };
        let current = fifo.owner == registration.owner
            && fifo.pending.is_none()
            && fifo
                .wake
                .as_ref()
                .is_some_and(|wake| wake.registration == *registration);
        if current {
            // Dropping the handle detaches the task; it keeps running.
            fifo.wake = None;
        }
        current
    }

    /// Stages one FIFO operation on a clone of the committed core. Refuses a
    /// legacy session and refuses while a result is unsaved. A seat's action is
    /// bound to its token's seat through the client authority. Core's own
    /// staged transaction validates and bumps the revision; a result that
    /// changes the core is held unsaved with its row, and the owner's wake is
    /// cleared, until [`Self::commit_fifo_result`].
    pub fn stage_fifo_operation(
        &mut self,
        draft_code: &str,
        actor: DraftActor<'_>,
        action: DraftAction,
        pack_source: Option<&dyn PackSource>,
        now_ms: u64,
    ) -> Result<FifoStaged, String> {
        let card_db = self.card_db.clone();
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {draft_code}"))?;
        let fifo = session
            .fifo_schedule()
            .ok_or_else(|| NOT_A_FIFO_SESSION.to_string())?;
        if fifo.pending.is_some() {
            return Err(FIFO_RESULT_UNSAVED.to_string());
        }
        let action = match actor {
            DraftActor::Seat { token } => {
                let seat = session
                    .seat_for_token(token)
                    .ok_or_else(|| "Invalid player token".to_string())?;
                authorize_client_draft_action(seat, action)?
            }
            DraftActor::Server => action,
        };
        check_submit_companion(&session.session, card_db.as_deref(), &action)?;
        let mut candidate = session.session.clone();
        draft_core::session::apply_at(&mut candidate, action, pack_source, now_ms).map_err(
            |error| {
                warn!(draft = %draft_code, %error, "FIFO draft operation rejected");
                format!("Draft error: {error}")
            },
        )?;
        if candidate == session.session {
            return Ok(FifoStaged::Unchanged);
        }
        let row = serde_json::to_string(&session.persisted_with_core(&candidate))
            .map_err(|error| format!("failed to serialize draft session: {error}"))?;
        let fifo = session
            .fifo_schedule_mut()
            .ok_or_else(|| NOT_A_FIFO_SESSION.to_string())?;
        fifo.pending = Some(Box::new(PendingFifoResult { candidate, row }));
        fifo.abort_wake();
        Ok(FifoStaged::Pending)
    }

    /// The row of the FIFO session's unsaved result, if any.
    pub fn pending_fifo_row(&self, draft_code: &str) -> Option<&str> {
        self.sessions
            .get(draft_code)
            .and_then(DraftSession::fifo_schedule)?
            .pending
            .as_deref()
            .map(|pending| pending.row.as_str())
    }

    /// Makes the saved result the committed core.
    pub fn commit_fifo_result(&mut self, draft_code: &str) -> Result<(), String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {draft_code}"))?;
        let pending = session
            .fifo_schedule_mut()
            .ok_or_else(|| NOT_A_FIFO_SESSION.to_string())?
            .pending
            .take()
            .ok_or_else(|| "no draft result is staged".to_string())?;
        session.session = pending.candidate;
        Ok(())
    }
}

/// What one [`DraftSessionManager::sweep_expired_seats`] call did, by pod.
#[derive(Default)]
pub struct DraftSeatSweep {
    /// Resident pods with a lapsed seat, whose views and rows are now stale.
    pub affected: Vec<String>,
    /// Resident pods that freed a guest seat, whose listed count is now stale.
    pub freed: Vec<String>,
    /// Pods removed because their host lapsed, owed the shell's teardown.
    pub reaped: Vec<(String, DraftSession)>,
}

/// A spawned draft match game session.
#[derive(Debug, Clone)]
pub struct DraftMatchSpawn {
    pub match_id: String,
    pub round: u8,
    pub game_code: String,
    pub player_a: DraftMatchPlayer,
    pub player_b: DraftMatchPlayer,
    /// Opponent display name indexed by draft seat (0 = player_a.seat, 1 = player_b.seat).
    pub opponent_names: [String; 2],
}

#[derive(Debug, Clone)]
pub struct DraftMatchPlayer {
    pub draft_seat: u8,
    pub game_player: PlayerId,
}

fn deck_payload_from_submission(
    db: &engine::database::CardDatabase,
    combos: &ComboTable,
    submission: &DraftDeckSubmission,
) -> Result<engine::game::deck_loading::PlayerDeckPayload, String> {
    let deck = DeckData {
        main_deck: submission.main_deck.clone(),
        sideboard: Vec::new(),
        // DEFERRED(out of scope -- the server-hosted draft launches 1v1
        // pairings only). Two independent reasons this slot stays empty rather
        // than being filled from `submission.commanders`.
        //
        // (1) No Commander session reaches this code:
        // `PostDraftPlay::CompleteImmediately` maps CommanderDraft to
        // `DraftStatus::Complete`, `apply_generate_pairings` is the only writer
        // of `session.pairings`, and `ensure_pairings_generated` refuses to run
        // outside `DraftStatus::Pairing` -- so the pairing loop that calls this
        // function never has a pairing to iterate.
        //
        // (2) Filling it unconditionally would change the four existing kinds:
        // `apply_submit_deck` has no per-kind gate on the designation list and
        // `load_and_hydrate_decks` places commanders unconditionally, so a
        // Premier/Traditional/Sealed submission carrying designations would
        // start a `FormatConfig::limited()` game with cards in the command
        // zone.
        //
        // A Commander pod launches client-side; see the pod launch path.
        commander: Vec::new(),
        companion: submission.companion.clone().into_iter().collect(),
        attraction_deck: Vec::new(),
        signature_spell: Vec::new(),
        printings: submission.printings.clone(),
        ..Default::default()
    };
    deck_resolve::resolve_deck(db, combos, &deck)
}

impl Default for DraftSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate a 6-character uppercase alphanumeric draft code.
pub fn generate_draft_code() -> String {
    let mut rng = rand::rng();
    let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect();
    (0..6)
        .map(|_| chars[rng.random_range(0..chars.len())])
        .collect()
}

/// The draft host occupies seat 0 (`create_draft` assigns the creator seat 0).
pub const DRAFT_HOST_SEAT: usize = 0;

fn admit_dormant_action(
    session: &draft_core::types::DraftSession,
    action: &DraftAction,
) -> Result<(), String> {
    refuse_fifo_core(session)?;
    if action.is_fifo_only() {
        return Err(draft_core::types::FIFO_UNSUPPORTED_BY_RUNTIME.to_string());
    }
    Ok(())
}

/// CR 702.139a/b: a native deck submission's companion designation is checked
/// by the engine's Limited eligibility authority BEFORE the reducer mutates, so
/// the server never acknowledges a deck that match admission would refuse. Every
/// other action passes through untouched.
fn check_submit_companion(
    session: &draft_core::types::DraftSession,
    db: Option<&engine::database::CardDatabase>,
    action: &DraftAction,
) -> Result<(), String> {
    let DraftAction::SubmitDeck {
        main_deck,
        commanders,
        companion,
        ..
    } = action
    else {
        return Ok(());
    };
    draft_core::session::check_submission_companion(
        session,
        db,
        main_deck,
        commanders,
        companion.as_deref(),
    )
    .map_err(|e| match e {
        // Carry the refusal's own reason, as the wasm submit path does.
        draft_core::types::DraftError::ValidationFailed { errors } => format!(
            "Draft error: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ),
        other => format!("Draft error: {other}"),
    })
}

/// Authorize a client-originated draft action against the authenticated seat.
///
/// The client controls the action's payload `seat`, but a player may only act
/// for the seat their token maps to, and only the host may drive table-wide
/// state. This is the single authority for that check; it runs in
/// `handle_draft_action` (the client path) and is intentionally NOT applied by
/// `apply_system_action` (trusted server-internal actions). The transport-layer
/// `client_forbidden_draft_action_reason` in phase-server is a complementary
/// earlier gate that blocks actions never permitted from any client.
fn authorize_client_draft_action(seat: usize, action: DraftAction) -> Result<DraftAction, String> {
    match action {
        DraftAction::Select {
            prompt_id,
            picked_card_instance_ids,
            burned_card_instance_ids,
            effect_card_instance_ids,
            ..
        } => Ok(DraftAction::Select {
            seat: u8::try_from(seat).map_err(|_| "Invalid authenticated seat".to_string())?,
            prompt_id,
            picked_card_instance_ids,
            burned_card_instance_ids,
            effect_card_instance_ids,
        }),
        DraftAction::AdvanceClock => Err("AdvanceClock is server-internal".to_string()),
        // Seat-scoped: overwrite the client-supplied seat with the authenticated
        // seat so a player cannot pick from or submit a deck for another seat.
        DraftAction::Pick {
            card_instance_ids, ..
        } => Ok(DraftAction::Pick {
            seat: seat as u8,
            card_instance_ids,
        }),
        DraftAction::PickWithDraftEffect {
            effect_card_instance_id,
            card_instance_ids,
            ..
        } => Ok(DraftAction::PickWithDraftEffect {
            seat: seat as u8,
            effect_card_instance_id,
            card_instance_ids,
        }),
        // The seat is table authority and is overwritten with the authenticated
        // one; the designation, companion and owner printings are player data and are carried
        // through untouched.
        DraftAction::SubmitDeck {
            main_deck,
            commanders,
            companion,
            printings,
            ..
        } => Ok(DraftAction::SubmitDeck {
            seat: seat as u8,
            main_deck,
            commanders,
            companion,
            printings,
        }),
        // The seat is table authority and is overwritten with the
        // authenticated one; the pile and the decision are player data and are
        // carried through untouched. EXACTLY `Pick`'s contract above. The
        // host-only arm below would let ONLY seat 0 ever decide, and it
        // compiles -- which is why this arm is named rather than left to the
        // compiler.
        DraftAction::SharedStackDecision { pile, decision, .. } => {
            Ok(DraftAction::SharedStackDecision {
                seat: seat as u8,
                pile,
                decision,
            })
        }
        // Table authority: only the host may start the draft, advance rounds,
        // generate pairings, report results, or replace a seat with a bot.
        DraftAction::Control { .. }
        | DraftAction::StartDraft
        | DraftAction::AdvanceRound
        | DraftAction::GeneratePairings
        | DraftAction::ReportMatchResult { .. }
        | DraftAction::ReplaceSeatWithBot { .. } => {
            if seat == DRAFT_HOST_SEAT {
                Ok(action)
            } else {
                Err("Only the draft host can perform this action".to_string())
            }
        }
        // Server-internal only — never accepted from a client wire surface.
        DraftAction::SetSeatConnected { .. } => {
            Err("SetSeatConnected is server-internal".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use draft_core::types::{
        DeckAddableCards, DraftKind, DraftSource, PodPolicy, SpectatorVisibility, TournamentFormat,
    };
    use engine::database::CardDatabase;

    fn test_config() -> DraftConfig {
        DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        }
    }

    fn printing_row(name: &str, set: &str, count: u32) -> serde_json::Value {
        serde_json::json!({
            "section": "main_deck", "name": name, "count": count,
            "printing": { "set_code": set, "collector_number": "001" }
        })
    }

    fn printing_pod(kind: DraftKind) -> (DraftSessionManager, String, Vec<String>) {
        let mut mgr = DraftSessionManager::new();
        let config = DraftConfig {
            kind,
            pack_count: kind.procedure().packs_per_player,
            ..test_config()
        };
        let (code, _, _) = mgr.create_draft(config, "Alice".into());
        for seat in 1..8 {
            mgr.join_draft(&code, format!("Player {seat}"), None)
                .unwrap();
        }
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".into(),
            cards_per_pack: 14,
        };
        mgr.apply_system_action(&code, DraftAction::StartDraft, Some(&source))
            .unwrap();
        while mgr.sessions[&code].session.status == DraftStatus::Drafting {
            for seat in 0..8 {
                let card = mgr.sessions[&code].session.current_pack[seat]
                    .as_ref()
                    .unwrap()
                    .0[0]
                    .instance_id
                    .clone();
                mgr.apply_system_action(
                    &code,
                    DraftAction::Pick {
                        seat: seat as u8,
                        card_instance_ids: vec![card],
                    },
                    Some(&source),
                )
                .unwrap();
            }
        }
        assert_eq!(
            mgr.sessions[&code].session.status,
            DraftStatus::Deckbuilding
        );
        let tokens = mgr.sessions[&code].player_tokens.clone();
        (mgr, code, tokens)
    }

    fn submit_printing_frame(
        mgr: &mut DraftSessionManager,
        code: &str,
        token: &str,
        main: Vec<String>,
        printings: Option<serde_json::Value>,
    ) -> Result<Vec<DraftPlayerView>, String> {
        use crate::client_message_wire_guard::guard_client_message_before_dispatch;
        use crate::protocol::ClientMessage;
        let mut data = serde_json::json!({
            "seat": 0, "main_deck": main, "commanders": []
        });
        if let Some(rows) = printings {
            data["printings"] = rows;
        }
        let wire = serde_json::json!({
            "type": "DraftAction", "data": {
                "draft_code": code,
                "action": { "type": "SubmitDeck", "data": data },
                "request_id": 1
            }
        });
        let message: ClientMessage =
            serde_json::from_str(&wire.to_string()).map_err(|error| error.to_string())?;
        guard_client_message_before_dispatch(&message, crate::protocol::ServerMode::Full)?;
        let ClientMessage::DraftAction {
            draft_code, action, ..
        } = message
        else {
            unreachable!()
        };
        mgr.handle_draft_action(&draft_code, token, action, None)
    }

    /// CR 702.139a/b: the native submission path runs the engine's companion
    /// check before the reducer mutates, so a Gyruda-class designation whose
    /// starting deck holds an odd-mana-value card is refused (and nothing is
    /// recorded), while the corrected submission proceeds. With no card
    /// database the designation fails closed.
    #[test]
    fn native_submission_refuses_a_companion_the_starting_deck_makes_ineligible() {
        use engine::database::CardDatabase;
        use engine::types::card::CardFace;
        use engine::types::keywords::{CompanionCondition, Keyword};
        use engine::types::mana::ManaCost;

        let (mgr, code, tokens) = printing_pod(DraftKind::Premier);
        let pool: Vec<String> = mgr.sessions[&code].session.pools[1]
            .iter()
            .map(|card| card.name.clone())
            .collect();
        let (companion, odd) = (pool[0].clone(), pool[1].clone());
        let mut faces = std::collections::BTreeMap::new();
        for name in &pool {
            let face = CardFace {
                name: name.clone(),
                mana_cost: ManaCost::generic(if *name == odd { 3 } else { 2 }),
                keywords: if *name == companion {
                    vec![Keyword::Companion(CompanionCondition::EvenManaValues)]
                } else {
                    Vec::new()
                },
                ..CardFace::default()
            };
            faces.insert(name.to_lowercase(), serde_json::to_value(&face).unwrap());
        }
        let db = std::sync::Arc::new(
            CardDatabase::from_json_str(&serde_json::to_string(&faces).unwrap()).unwrap(),
        );
        let submit = |mgr: &mut DraftSessionManager, main: Vec<String>| {
            mgr.handle_draft_action(
                &code,
                &tokens[1],
                DraftAction::SubmitDeck {
                    seat: 1,
                    main_deck: main,
                    commanders: Vec::new(),
                    companion: Some(companion.clone()),
                    printings: Vec::new(),
                },
                None,
            )
        };
        let mut with_odd: Vec<String> = vec!["Plains".to_string(); 39];
        with_odd.push(odd.clone());
        let fixed: Vec<String> = vec!["Plains".to_string(); 40];

        // No card database: a designation cannot be verified.
        let (mut bare, bare_code, bare_tokens) = printing_pod(DraftKind::Premier);
        let bare_companion = bare.sessions[&bare_code].session.pools[1][0].name.clone();
        let error = bare
            .handle_draft_action(
                &bare_code,
                &bare_tokens[1],
                DraftAction::SubmitDeck {
                    seat: 1,
                    main_deck: fixed.clone(),
                    commanders: Vec::new(),
                    companion: Some(bare_companion),
                    printings: Vec::new(),
                },
                None,
            )
            .unwrap_err();
        assert!(error.contains("cannot be verified"), "{error}");

        let mut mgr = mgr.with_card_db(db);
        let error = submit(&mut mgr, with_odd).unwrap_err();
        assert!(error.contains("not a legal companion"), "{error}");
        assert!(mgr.sessions[&code].session.submitted_decks.is_empty());

        let views = submit(&mut mgr, fixed).expect("the corrected submission proceeds");
        assert!(views[1].seats[1].has_submitted_deck);
    }

    #[test]
    fn submitted_printings_survive_native_ingress_and_storage() {
        let (mut mgr, code, tokens) = printing_pod(DraftKind::Premier);
        let rows = serde_json::json!([printing_row("Plains", "lea", 2)]);
        let views = submit_printing_frame(
            &mut mgr,
            &code,
            &tokens[1],
            vec!["Plains".into(); 40],
            Some(rows.clone()),
        )
        .unwrap();
        assert!(views[0].seats[1].has_submitted_deck);
        let stored =
            serde_json::to_value(&mgr.sessions[&code].session.submitted_decks[&PlayerId(1)])
                .unwrap();
        assert_eq!(stored["seat"], 1);
        assert_eq!(
            stored["printings"], rows,
            "authenticated submission retains owner printings"
        );
        assert!(!mgr.sessions[&code]
            .session
            .submitted_decks
            .contains_key(&PlayerId(0)));
    }

    #[test]
    fn submitted_printings_outer_wire_bytes() {
        use crate::protocol::ClientMessage;
        for data in [
            r#"{"seat":0,"main_deck":["Plains"],"commanders":[]}"#,
            r#"{"seat":0,"main_deck":["Plains"],"commanders":[],"printings":[{"section":"main_deck","name":"Plains","count":1,"printing":{"set_code":"lea","collector_number":"001"}}]}"#,
        ] {
            let expected = format!(
                r#"{{"type":"DraftAction","data":{{"draft_code":"ABCD12","action":{{"type":"SubmitDeck","data":{data}}},"request_id":1}}}}"#
            );
            for wire in [
                expected.clone(),
                format!(
                    r#"{{"data":{{"request_id":1,"draft_code":"ABCD12","action":{{"data":{data},"type":"SubmitDeck"}}}},"type":"DraftAction"}}"#
                ),
            ] {
                let message: ClientMessage = serde_json::from_str(&wire).unwrap();
                assert_eq!(
                    serde_json::to_string(&message).unwrap(),
                    expected,
                    "native frame bytes"
                );
            }
        }
    }

    #[test]
    fn submitted_printings_rejected_ingress_preserves_prior_owner_data() {
        use engine::types::card_printing::{MAX_DECK_PRINTING_ENTRIES, MAX_PRINTING_SET_CODE_LEN};
        let (mut mgr, code, tokens) = printing_pod(DraftKind::Premier);
        let valid = serde_json::json!([printing_row("Plains", "lea", 1)]);
        submit_printing_frame(
            &mut mgr,
            &code,
            &tokens[0],
            vec!["Plains".into(); 40],
            Some(valid),
        )
        .unwrap();
        let before = mgr.sessions[&code].session.submitted_decks.clone();
        let mut oversized = printing_row("Plains", "lea", 1);
        oversized["printing"]["set_code"] =
            serde_json::json!("x".repeat(MAX_PRINTING_SET_CODE_LEN + 1));
        for rejected in [
            serde_json::json!([oversized]),
            serde_json::json!(vec![serde_json::Value::Null; MAX_DECK_PRINTING_ENTRIES + 1]),
        ] {
            assert!(
                submit_printing_frame(
                    &mut mgr,
                    &code,
                    &tokens[0],
                    vec!["Island".into(); 40],
                    Some(rejected)
                )
                .is_err(),
                "abusive printing frame must be refused"
            );
            assert_eq!(mgr.sessions[&code].session.submitted_decks, before);
        }
        let rows = serde_json::json!([null, printing_row("Island", "leb", 1), {}]);
        submit_printing_frame(
            &mut mgr,
            &code,
            &tokens[0],
            vec!["Island".into(); 40],
            Some(rows),
        )
        .unwrap();
        let stored = &mgr.sessions[&code].session.submitted_decks[&PlayerId(0)];
        assert_eq!(stored.main_deck, vec!["Island"; 40]);
        assert_eq!(
            serde_json::to_value(&stored.printings).unwrap(),
            serde_json::json!([printing_row("Island", "leb", 1)])
        );
    }

    #[test]
    fn submitted_printings_persist_reconnect_and_remain_private() {
        let (mut mgr, code, tokens) = printing_pod(DraftKind::Premier);
        let owner_a = serde_json::json!([printing_row("Plains", "secret-a", 1)]);
        let owner_b = serde_json::json!([printing_row("Plains", "secret-b", 2)]);
        for (token, rows) in [(&tokens[0], &owner_a), (&tokens[1], &owner_b)] {
            submit_printing_frame(
                &mut mgr,
                &code,
                token,
                vec!["Plains".into(); 40],
                Some(rows.clone()),
            )
            .unwrap();
        }
        let persisted = serde_json::to_string(&mgr.sessions[&code].to_persisted()).unwrap();
        let mut restored = DraftSessionManager::new();
        restored
            .restore_persisted_session(serde_json::from_str(&persisted).unwrap())
            .unwrap();
        for (seat, rows) in [(0, &owner_a), (1, &owner_b)] {
            let stored = &restored.sessions[&code].session.submitted_decks[&PlayerId(seat)];
            assert_eq!(
                serde_json::to_value(&stored.printings).unwrap(),
                *rows,
                "persisted owner data is isolated by authenticated seat"
            );
        }
        let reconnect = restored
            .handle_reconnect(&code, &tokens[1], std::time::Instant::now())
            .unwrap();
        assert!(reconnect.seats[0].has_submitted_deck);
        assert!(reconnect.seats[1].has_submitted_deck);
        assert!(reconnect.seats[1].connected);
        let core = &restored.sessions[&code].session;
        let views = [
            serde_json::to_string(&reconnect).unwrap(),
            serde_json::to_string(&restored.sessions[&code].view_for_seat(2)).unwrap(),
            serde_json::to_string(&draft_core::view::filter_for_spectator(
                core,
                SpectatorVisibility::Public,
            ))
            .unwrap(),
        ];
        for view in views {
            assert!(
                !view.contains("secret-a"),
                "other seat's sidecar must not leak into views"
            );
            assert!(
                !view.contains("secret-b"),
                "own submission is not a public carrier either"
            );
            assert!(!view.contains("\"printings\""));
        }
        submit_printing_frame(
            &mut restored,
            &code,
            &tokens[1],
            vec!["Island".into(); 40],
            None,
        )
        .unwrap();
        assert!(
            restored.sessions[&code].session.submitted_decks[&PlayerId(1)]
                .printings
                .is_empty(),
            "name-only resubmission clears persisted owner art"
        );
        assert_eq!(
            serde_json::to_value(
                &restored.sessions[&code].session.submitted_decks[&PlayerId(0)].printings
            )
            .unwrap(),
            owner_a
        );
        restored
            .apply_system_action(
                &code,
                DraftAction::ReplaceSeatWithBot {
                    seat: 0,
                    name: None,
                },
                None,
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(
                &restored.sessions[&code].session.submitted_decks[&PlayerId(0)].printings
            )
            .unwrap(),
            owner_a,
            "bot takeover does not replace an accepted human deck"
        );
    }

    fn printing_database() -> std::sync::Arc<CardDatabase> {
        let cards: serde_json::Map<String, serde_json::Value> = [
            "Plains", "Island", "Forest", "Mountain",
        ]
        .into_iter()
        .map(|name| {
            (
                name.to_lowercase(),
                serde_json::json!({
                    "name":name,
                    "mana_cost":{"type":"Cost","shards":[],"generic":0},
                    "card_type":{"supertypes":["Basic"],"core_types":["Land"],"subtypes":[name]},
                    "power":null,"toughness":null,"loyalty":null,"defense":null,
                    "oracle_text":null,"non_ability_text":null,"flavor_name":null,
                    "keywords":[],"abilities":[],"triggers":[],"static_abilities":[],
                    "replacements":[],"color_override":null,"color_identity":[],
                    "scryfall_oracle_id":format!("oracle-{name}")
                }),
            )
        })
        .collect();
        std::sync::Arc::new(
            CardDatabase::from_json_str(&serde_json::Value::Object(cards).to_string()).unwrap(),
        )
    }

    /// The names of the cards draft seat 0 brings to its spawned game in `round`.
    fn seat_zero_card_names(
        mgr: &mut DraftSessionManager,
        code: &str,
        db: &std::sync::Arc<CardDatabase>,
        round: u8,
    ) -> Vec<String> {
        let mut games = SessionManager::new();
        let spawns = mgr
            .spawn_match_games_for_round(code, &mut games, db, &ComboTable::default(), round)
            .unwrap();
        let (spawn, player) = spawns
            .iter()
            .find_map(|spawn| {
                [&spawn.player_a, &spawn.player_b]
                    .into_iter()
                    .find(|player| player.draft_seat == 0)
                    .map(|player| (spawn, player))
            })
            .expect("seat 0 is paired this round");
        let game = games.try_session(&spawn.game_code).unwrap();
        game.state
            .objects
            .values()
            .filter(|object| object.owner == player.game_player)
            .map(|object| object.name.clone())
            .collect()
    }

    #[test]
    fn casual_between_rounds_revision_reaches_the_next_rounds_spawned_game() {
        let db = printing_database();
        let (mut mgr, code, tokens) = printing_pod(DraftKind::Premier);
        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .session
            .config
            .pod_policy = PodPolicy::Casual;
        for token in &tokens {
            submit_printing_frame(&mut mgr, &code, token, vec!["Plains".into(); 40], None).unwrap();
        }
        mgr.ensure_pairings_generated(&code).unwrap();

        // Reach guard: round 1 plays the original submission.
        let round_one = seat_zero_card_names(&mut mgr, &code, &db, 1);
        assert_eq!(round_one, vec!["Plains".to_string(); 40]);

        let pairings = mgr.sessions[&code].session.pairings.clone();
        for pairing in pairings {
            mgr.apply_system_action(
                &code,
                DraftAction::ReportMatchResult {
                    match_id: pairing.match_id,
                    winner_seat: Some(pairing.players[0].0),
                },
                None,
            )
            .unwrap();
        }
        assert_eq!(
            mgr.sessions[&code].session.status,
            DraftStatus::RoundComplete
        );

        // MTR 7.3: the revision arrives through the client wire between rounds.
        submit_printing_frame(&mut mgr, &code, &tokens[0], vec!["Island".into(); 40], None)
            .unwrap();
        assert_eq!(
            mgr.sessions[&code].session.status,
            DraftStatus::RoundComplete,
            "a revision never advances the round"
        );
        mgr.handle_draft_action(&code, &tokens[0], DraftAction::AdvanceRound, None)
            .unwrap();
        mgr.ensure_pairings_generated(&code).unwrap();

        let round_two = seat_zero_card_names(&mut mgr, &code, &db, 2);
        assert_eq!(
            round_two,
            vec!["Island".to_string(); 40],
            "the next round's game is built from the revised submission"
        );
    }

    #[test]
    fn submitted_printings_reach_actual_draft_and_sealed_pairing_objects() {
        use engine::types::card_printing::CardPrinting;
        let db = printing_database();
        for kind in [DraftKind::Premier, DraftKind::Sealed] {
            let (mut mgr, code, tokens) = printing_pod(kind);
            let mut hostile = vec![
                printing_row("Island", "opponent-only", 1),
                printing_row("Mountain", "pool-only", 1),
                printing_row("Unresolvable", "missing", 1),
                printing_row("Plains", "", 1),
                printing_row("Plains", "bad\n", 1),
                printing_row("Plains", "zero", 0),
            ];
            for section in ["sideboard", "commander", "unknown"] {
                let mut row = printing_row("Plains", "wrong-section", 1);
                row["section"] = serde_json::json!(section);
                hostile.push(row);
            }
            hostile.extend([
                printing_row("Plains", "owner-a", 1),
                printing_row("Plains", "owner-b", u32::MAX),
                printing_row("Plains", "exhausted", 1),
            ]);
            // Pool membership cannot give an annotation a submitted-deck budget.
            mgr.sessions.get_mut(&code).unwrap().session.pools[0][0].name = "Mountain".into();
            for (seat, token) in tokens.iter().enumerate().take(7) {
                let mut main = vec!["Plains".into(); 2];
                main.extend(vec![
                    if seat == 1 {
                        "Island".into()
                    } else {
                        "Forest".into()
                    };
                    38
                ]);
                let rows = match seat {
                    0 => Some(serde_json::json!(hostile)),
                    1 => Some(serde_json::json!([printing_row("Plains", "owner-c", 1)])),
                    _ => None,
                };
                submit_printing_frame(&mut mgr, &code, token, main, rows).unwrap();
            }
            // The trusted server seam supplies an empty sidecar; no bot generator is invented.
            mgr.apply_system_action(
                &code,
                DraftAction::SubmitDeck {
                    seat: 7,
                    main_deck: vec!["Plains".into(); 40],
                    commanders: vec![],
                    companion: None,
                    printings: vec![],
                },
                None,
            )
            .unwrap();
            assert_eq!(mgr.sessions[&code].session.status, DraftStatus::Pairing);
            let persisted = serde_json::to_string(&mgr.sessions[&code].to_persisted()).unwrap();
            let mut restored = DraftSessionManager::new();
            restored
                .restore_persisted_session(serde_json::from_str(&persisted).unwrap())
                .unwrap();
            restored
                .handle_reconnect(&code, &tokens[0], std::time::Instant::now())
                .unwrap();
            restored.ensure_pairings_generated(&code).unwrap();
            let core = &mut restored.sessions.get_mut(&code).unwrap().session;
            assert_eq!(core.pairings.len(), 4);
            // Valid first-round fixture, with the annotated pod seats explicitly reversed.
            for (pairing, players) in core
                .pairings
                .iter_mut()
                .zip([[1, 0], [3, 2], [5, 4], [7, 6]])
            {
                pairing.players = players.map(PlayerId);
            }
            let mut games = SessionManager::new();
            let spawns = restored
                .spawn_match_games_for_round(&code, &mut games, &db, &ComboTable::default(), 1)
                .unwrap();
            assert_eq!(spawns.len(), 4, "every complete pairing starts a real game");
            for spawn in spawns {
                let game = games.try_session(&spawn.game_code).unwrap();
                assert!(game.game_started);
                for player in [&spawn.player_a, &spawn.player_b] {
                    let objects: Vec<_> = game
                        .state
                        .objects
                        .values()
                        .filter(|object| object.owner == player.game_player)
                        .collect();
                    assert_eq!(objects.len(), 40, "submitted card counts are unchanged");
                    let mut actual: Vec<_> =
                        objects
                            .iter()
                            .filter_map(|object| {
                                assert_eq!(object.printing.get(), object.base_printing.get(),
                            "both physical-object printing fields carry the same owner choice");
                                object
                                    .printing
                                    .get()
                                    .as_deref()
                                    .map(|printing| (object.name.clone(), printing.clone()))
                            })
                            .collect();
                    actual.sort_by(|left, right| left.1.set_code.cmp(&right.1.set_code));
                    let sets: &[&str] = match player.draft_seat {
                        0 => &["owner-a", "owner-b"],
                        1 => &["owner-c"],
                        _ => &[],
                    };
                    let expected: Vec<_> = sets
                        .iter()
                        .map(|set| ("Plains".to_string(), CardPrinting::owner(*set, "001")))
                        .collect();
                    assert_eq!(actual, expected,
                        "pairing objects retain the authenticated owner's exact printing budget: pod seat {}",
                        player.draft_seat);
                    let plains = objects
                        .iter()
                        .filter(|object| object.name == "Plains")
                        .count();
                    assert_eq!(plains, if player.draft_seat == 7 { 40 } else { 2 });
                }
            }
        }
    }

    /// A four-seat Commander pod (CR 903.13a; the 4-player pod is the product
    /// default, and `min_pod_size` 3 is the CR 800.1 floor below it).
    fn commander_test_config() -> DraftConfig {
        let procedure = DraftKind::CommanderDraft.procedure();
        DraftConfig {
            kind: DraftKind::CommanderDraft,
            pod_size: procedure.pod_size,
            pack_count: procedure.packs_per_player,
            min_deck_size: procedure.min_deck_size,
            ..test_config()
        }
    }

    fn start_pod(mgr: &mut DraftSessionManager, config: DraftConfig) -> String {
        let pod_size = config.pod_size;
        let (code, _host_token, _) = mgr.create_draft(config, "Alice".to_string());
        for i in 1..pod_size {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        mgr.apply_system_action(&code, DraftAction::StartDraft, Some(&source))
            .unwrap();
        code
    }

    /// A 2-seat Winston pod. Every seat here is human because `start_pod`
    /// joins human players, not because the engine requires it -- a
    /// shared-stack pod admits bot seats. `server-core` deliberately seats no
    /// Winston bot of its own: `pick_random_for_seat` is the timeout/disconnect
    /// default and stays exactly that.
    fn winston_test_config() -> DraftConfig {
        let procedure = DraftKind::Winston.procedure();
        DraftConfig {
            kind: DraftKind::Winston,
            pod_size: procedure.pod_size,
            pack_count: procedure.packs_per_player,
            min_deck_size: procedure.min_deck_size,
            ..test_config()
        }
    }

    /// V17(a). REACH, asserted AT `draft_seats_needing_auto_pick` — above the
    /// `current_pack` conjunct that used to dominate every arm below it.
    ///
    /// The mutation this catches: leaving the pick-and-pass filter in place.
    /// A Winston session leaves `current_pack` all-`None`, so the unfiltered
    /// function returns an EMPTY sweep and every timeout arm below it is dead.
    #[test]
    fn draft_seats_needing_auto_pick_reaches_the_active_winston_seat() {
        let mut mgr = DraftSessionManager::new();
        let code = start_pod(&mut mgr, winston_test_config());
        let session = &mut mgr.sessions.get_mut(&code).unwrap().session;
        assert_eq!(session.status, DraftStatus::Drafting);
        // The dominating conjunct is genuinely empty here, which is what makes
        // this a measurement and not a coincidence.
        assert!(session.current_pack.iter().all(Option::is_none));
        let active = usize::from(session.shared_stack().unwrap().active_seat);

        assert_eq!(draft_seats_needing_auto_pick(session, 2), vec![active]);

        // Paired positive: a pick-and-pass pod still yields its seats.
        let mut premier_mgr = DraftSessionManager::new();
        let premier_code = start_pod(&mut premier_mgr, test_config());
        let premier = &mut premier_mgr.sessions.get_mut(&premier_code).unwrap().session;
        assert_eq!(
            draft_seats_needing_auto_pick(premier, 8),
            (0..8).collect::<Vec<_>>()
        );
    }

    /// V17(b). The timeout path drives the ACTIVE seat's turn legally, from the
    /// hostile turn-start state where `Take` is refused `PileEmpty` and
    /// `Decline` is the only legal move -- `stack == 0, cursor == 0,
    /// piles[0].is_empty()` with a later pile non-empty.
    ///
    /// This is the leg that discriminates WHERE the distribution `match` sits
    /// in `pick_random_for_seat`: below the `view.current_pack` read the arm is
    /// dead, and this test reds.
    ///
    /// It is also the leg that would fail if the forced decision were derived
    /// from stack emptiness instead of from `refusal_for`.
    #[test]
    fn timeout_advances_a_winston_turn() {
        let mut mgr = DraftSessionManager::new();
        let code = start_pod(&mut mgr, winston_test_config());
        let session = &mut mgr.sessions.get_mut(&code).unwrap().session;

        // Build the hostile fixture, conserving every card into a pool rather
        // than dropping it.
        let (active, expected_pool_before) = {
            let state = session.shared_stack.as_mut().unwrap();
            let mut orphaned: Vec<_> = std::mem::take(&mut state.main_stack);
            orphaned.append(&mut state.piles[0]);
            state.inspected[0] = 0;
            assert!(state.piles[0].is_empty());
            assert!(
                state.piles.iter().skip(1).any(|pile| !pile.is_empty()),
                "a later pile is still non-empty"
            );
            let active = state.active_seat;
            session.pools[0].extend(orphaned);
            (active, session.pools[usize::from(active)].len())
        };

        mgr.pick_random_for_seat(&code, active, None)
            .expect("the timeout path drives the active seat");

        let session = &mgr.sessions[&code].session;
        let state = session.shared_stack().unwrap();
        // At the fixture's turn-start state `Take` is refused `PileEmpty`, so
        // the only legal move is `Decline` on pile 0 — which ADVANCES THE
        // CURSOR rather than ending the turn. Assert that something moved
        // before driving the rest of the turn below.
        assert!(
            state.cursor == 1 || state.active_seat != active,
            "the turn advanced in some direction"
        );

        // Drive the rest of this seat's turn through the same entry point.
        let mut guard = 0;
        while mgr.sessions[&code]
            .session
            .shared_stack()
            .unwrap()
            .active_seat
            == active
            && mgr.sessions[&code].session.status == DraftStatus::Drafting
        {
            mgr.pick_random_for_seat(&code, active, None)
                .expect("the timeout path keeps driving the active seat");
            guard += 1;
            assert!(guard < 8, "the turn did not end");
        }
        let session = &mgr.sessions[&code].session;
        assert_ne!(
            session.shared_stack().unwrap().active_seat,
            active,
            "the turn passed to the other seat"
        );
        assert!(
            session.pools[usize::from(active)].len() > expected_pool_before,
            "the timed-out seat's pool grew by at least one card"
        );
    }

    /// V17(c). MULTI-AUTHORITY HOSTILE FIXTURE: the disconnect-expiry caller
    /// passes the DISCONNECTED seat, which need not be the active one.
    ///
    /// The mutation it catches is an arm that falls back to "drive the active
    /// seat instead" -- so the assertion is that NOTHING moved.
    #[test]
    fn disconnect_expiry_refuses_a_non_active_winston_seat() {
        let mut mgr = DraftSessionManager::new();
        let code = start_pod(&mut mgr, winston_test_config());
        let active = mgr.sessions[&code]
            .session
            .shared_stack()
            .unwrap()
            .active_seat;
        let idle = (active + 1) % 2;
        let before = mgr.sessions[&code].session.shared_stack.clone();
        let pools_before = mgr.sessions[&code].session.pools.clone();

        let error = mgr
            .pick_random_for_seat(&code, idle, None)
            .expect_err("a non-active seat has no turn to advance");
        assert!(
            error.contains("not the active shared-stack seat"),
            "{error}"
        );
        let session = &mgr.sessions[&code].session;
        assert_eq!(session.shared_stack, before, "nothing moved");
        assert_eq!(session.pools, pools_before, "no pool moved");

        // Paired positive IN THE SAME TEST: the identical call for the ACTIVE
        // seat succeeds, so the refusal is about the seat and not about the
        // kind.
        mgr.pick_random_for_seat(&code, active, None)
            .expect("the active seat's turn is driveable");
        assert_ne!(mgr.sessions[&code].session.shared_stack, before);
    }

    /// V29. The payload seat is TABLE authority and is overwritten with the
    /// authenticated one; `pile` and `decision` are player data and survive
    /// untouched -- exactly `Pick`'s contract.
    #[test]
    fn shared_stack_decision_is_rescoped_to_the_authenticated_seat() {
        use draft_core::types::SharedStackPileDecision;

        let action = authorize_client_draft_action(
            2,
            DraftAction::SharedStackDecision {
                seat: 1,
                pile: 2,
                decision: SharedStackPileDecision::Decline,
            },
        )
        .expect("a shared-stack decision is allowed for any seat");
        assert_eq!(
            action,
            DraftAction::SharedStackDecision {
                seat: 2,
                pile: 2,
                decision: SharedStackPileDecision::Decline,
            }
        );
        // Paired positive: a host-only action from seat 1 is still refused, so
        // this arm did not land in the host-only group.
        assert!(authorize_client_draft_action(1, DraftAction::StartDraft).is_err());
    }

    /// The native Winston decision route: the WebSocket `SharedStackDecision`
    /// frame reaches `handle_draft_action`, whose dormant-flow admission gate
    /// runs before `session::apply`. A legacy Winston decision must pass that
    /// gate; refusing it would refuse every native Winston turn while the
    /// shared-stack reducer and timeout tests, which bypass the gate, stay green.
    #[test]
    fn native_winston_decision_is_admitted_through_handle_draft_action() {
        let mut mgr = DraftSessionManager::new();
        let code = start_pod(&mut mgr, winston_test_config());
        let (token, action, before) = {
            let managed = &mgr.sessions[&code];
            let state = managed
                .session
                .shared_stack()
                .expect("a started Winston pod deals a shared stack");
            let seat = state.active_seat;
            // The same legal (decision, cursor) pair the timeout path submits.
            let decision = draft_core::shared_stack::forced_decision(state, seat)
                .expect("the active seat has a legal decision");
            let action = DraftAction::SharedStackDecision {
                seat,
                pile: state.cursor,
                decision,
            };
            (
                managed.player_tokens[usize::from(seat)].clone(),
                action,
                managed.session.shared_stack.clone(),
            )
        };

        mgr.handle_draft_action(&code, &token, action, None)
            .expect("the active seat's Winston decision is admitted and applied");

        assert_ne!(
            mgr.sessions[&code].session.shared_stack, before,
            "the decision moved the shared stack"
        );
    }

    #[test]
    fn fifo_action_token_binding_preserves_nested_prompt_authority() {
        use draft_core::types::{DraftControl, DraftPauseReason, DraftPromptId, DraftSessionId};
        let action = DraftAction::Select {
            seat: 1,
            prompt_id: DraftPromptId {
                session_id: DraftSessionId("authority".into()),
                seat: 1,
                generation: 19,
            },
            picked_card_instance_ids: vec!["z".into(), "a".into()],
            burned_card_instance_ids: vec!["b".into(), "y".into()],
            effect_card_instance_ids: vec!["effect-b".into(), "effect-a".into()],
        };
        let mut expected = action.clone();
        if let DraftAction::Select { seat, .. } = &mut expected {
            *seat = 2;
        }
        assert_eq!(
            authorize_client_draft_action(2, action.clone()).unwrap(),
            expected
        );
        assert_eq!(
            authorize_client_draft_action(256, action),
            Err("Invalid authenticated seat".into())
        );
        for control in [
            DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
            DraftControl::Resume,
        ] {
            let action = DraftAction::Control { control };
            assert_eq!(
                authorize_client_draft_action(0, action.clone()),
                Ok(action.clone())
            );
            assert_eq!(
                authorize_client_draft_action(1, action),
                Err("Only the draft host can perform this action".into())
            );
        }
        assert_eq!(
            authorize_client_draft_action(0, DraftAction::AdvanceClock),
            Err("AdvanceClock is server-internal".into())
        );
        assert_eq!(
            authorize_client_draft_action(2, DraftAction::AdvanceClock),
            Err("AdvanceClock is server-internal".into())
        );
        let mut manager = DraftSessionManager::new();
        let code = start_pod(&mut manager, test_config());
        let token = manager.sessions[&code].player_tokens[2].clone();
        let before = serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap();
        assert_eq!(
            manager
                .handle_draft_action(&code, "wrong-token", expected.clone(), None)
                .unwrap_err(),
            "Invalid player token"
        );
        assert_eq!(
            manager
                .handle_draft_action(&code, &token, expected, None)
                .unwrap_err(),
            "FIFO drafting is not supported by this runtime"
        );
        assert_eq!(
            serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap(),
            before
        );
        manager
            .handle_draft_action(
                &code,
                &token,
                DraftAction::Pick {
                    seat: 7,
                    card_instance_ids: vec!["TST-2-0-0".into()],
                },
                None,
            )
            .unwrap();
        assert_eq!(manager.sessions[&code].session.pools[2].len(), 1);
        assert!(manager.sessions[&code].session.pools[7].is_empty());
    }

    #[test]
    fn dormant_native_restore_rejects_valid_fifo_before_any_install() {
        use draft_core::types::{
            DraftClockPolicy, DraftControl, DraftFlow, DraftPauseReason, DraftSessionId,
            PickClockPolicy,
        };
        let mut manager = DraftSessionManager::new();
        let code = start_pod(&mut manager, test_config());
        let legacy = manager.sessions[&code].to_persisted();
        let mut config = legacy.config.clone();
        config.flow = DraftFlow::FifoAsync;
        config.cards_per_pack = 2;
        config.clock_policy = DraftClockPolicy {
            pick: PickClockPolicy::Fixed { duration_ms: 1000 },
            review_duration_ms: 1000,
        };
        let mut fifo = draft_core::types::DraftSession::new_fifo(
            config.clone(),
            legacy.session.seats.clone(),
            code.clone(),
            DraftSessionId("native-restore".into()),
        )
        .unwrap();
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".into(),
            cards_per_pack: 2,
        };
        draft_core::session::apply_at(&mut fifo, DraftAction::StartDraft, Some(&source), 1000)
            .unwrap();
        let active = fifo.clone();
        let legal = draft_core::pick_pass::legal_auto_selection(&fifo, 0).unwrap();
        let mut positive = fifo.clone();
        draft_core::session::apply_at(&mut positive, legal, None, 1100).unwrap();
        assert_eq!(positive.pools[0].len(), 1);
        for _ in 0..2 {
            for seat in 0..8 {
                let action = draft_core::pick_pass::legal_auto_selection(&fifo, seat).unwrap();
                draft_core::session::apply_at(&mut fifo, action, None, 1100).unwrap();
            }
        }
        assert_eq!(fifo.status, DraftStatus::Review);
        let review = fifo.clone();
        draft_core::session::apply_at(
            &mut fifo,
            DraftAction::Control {
                control: DraftControl::Pause {
                    reason: DraftPauseReason::PausedByHost,
                },
            },
            None,
            1500,
        )
        .unwrap();
        for core in [active, review, fifo] {
            core.validate_persisted_snapshot().unwrap();
            let mut payload = legacy.clone();
            payload.config = config.clone();
            payload.session = core;
            let before = serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap();
            let tokens = manager.token_to_draft.clone();
            assert_eq!(
                DraftSession::from_persisted(payload.clone()).err().unwrap(),
                "FIFO drafting is not supported by this runtime"
            );
            assert_eq!(
                manager.restore_session(payload.clone()),
                Err("FIFO drafting is not supported by this runtime".into())
            );
            assert_eq!(
                manager.restore_persisted_session(payload),
                Err("FIFO drafting is not supported by this runtime".into())
            );
            assert_eq!(manager.sessions.len(), 1);
            assert_eq!(manager.token_to_draft, tokens);
            assert_eq!(
                serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap(),
                before
            );
        }
        let restored = DraftSession::from_persisted(legacy.clone()).unwrap();
        assert_eq!(restored.session, legacy.session);
        let mut fresh = DraftSessionManager::new();
        fresh.restore_session(legacy).unwrap();
        let token = fresh.sessions[&code].player_tokens[0].clone();
        fresh
            .handle_draft_action(
                &code,
                &token,
                DraftAction::Pick {
                    seat: 0,
                    card_instance_ids: vec!["TST-0-0-0".into()],
                },
                None,
            )
            .unwrap();
        assert_eq!(fresh.sessions[&code].session.pools[0].len(), 1);
    }

    /// A started FIFO core over a legacy pod's seats and draft code, and the
    /// FIFO configuration it runs under.
    fn started_fifo_core(
        legacy: &PersistedDraftSession,
    ) -> (DraftConfig, draft_core::types::DraftSession) {
        use draft_core::types::{DraftClockPolicy, DraftFlow, DraftSessionId, PickClockPolicy};
        let mut config = legacy.config.clone();
        config.flow = DraftFlow::FifoAsync;
        config.cards_per_pack = 2;
        config.clock_policy = DraftClockPolicy {
            pick: PickClockPolicy::Fixed { duration_ms: 1000 },
            review_duration_ms: 1000,
        };
        let mut core = draft_core::types::DraftSession::new_fifo(
            config.clone(),
            legacy.session.seats.clone(),
            legacy.draft_code.clone(),
            DraftSessionId("native-restore".into()),
        )
        .unwrap();
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".into(),
            cards_per_pack: 2,
        };
        draft_core::session::apply_at(&mut core, DraftAction::StartDraft, Some(&source), 1000)
            .unwrap();
        (config, core)
    }

    /// Restoring `payload` fails with `expected` through both native entry
    /// points, and the manager is unchanged.
    fn assert_restore_refused(
        manager: &mut DraftSessionManager,
        code: &str,
        payload: PersistedDraftSession,
        expected: &str,
    ) {
        let before = serde_json::to_value(manager.sessions[code].to_persisted()).unwrap();
        let tokens = manager.token_to_draft.clone();
        let sessions = manager.sessions.len();
        assert_eq!(
            DraftSession::from_persisted(payload.clone()).err().unwrap(),
            expected
        );
        assert_eq!(
            manager.restore_persisted_session(payload),
            Err(expected.to_string())
        );
        assert_eq!(manager.sessions.len(), sessions);
        assert_eq!(manager.token_to_draft, tokens);
        assert_eq!(
            serde_json::to_value(manager.sessions[code].to_persisted()).unwrap(),
            before
        );
    }

    #[test]
    fn dormant_native_restore_refuses_malformed_fifo_rows_with_the_shared_text() {
        let mut manager = DraftSessionManager::new();
        let code = start_pod(&mut manager, test_config());
        let legacy = manager.sessions[&code].to_persisted();
        let (config, fifo) = started_fifo_core(&legacy);
        // A valid FIFO row is refused with the shared text.
        fifo.validate_persisted_snapshot().unwrap();
        let mut payload = legacy.clone();
        payload.config = config.clone();
        payload.session = fifo.clone();
        assert_restore_refused(
            &mut manager,
            &code,
            payload,
            draft_core::types::FIFO_UNSUPPORTED_BY_RUNTIME,
        );

        // The shared stack of a started legacy Winston pod, which no FIFO
        // session keeps, and a FIFO row that lost its FIFO state.
        let mut winston_manager = DraftSessionManager::new();
        let winston = start_pod(&mut winston_manager, winston_test_config());
        let mut with_stack = fifo.clone();
        with_stack.shared_stack = winston_manager.sessions[&winston]
            .session
            .shared_stack
            .clone();
        assert!(with_stack.shared_stack.is_some());
        let mut without_fifo_state = fifo;
        without_fifo_state.fifo = None;
        for malformed in [with_stack, without_fifo_state] {
            // Reach: the snapshot validator refuses this row by itself.
            assert!(malformed.validate_persisted_snapshot().is_err());
            let mut payload = legacy.clone();
            payload.config = config.clone();
            payload.session = malformed;
            assert_restore_refused(
                &mut manager,
                &code,
                payload,
                draft_core::types::FIFO_UNSUPPORTED_BY_RUNTIME,
            );
        }
    }

    #[test]
    fn dormant_native_restore_refuses_legacy_rows_carrying_fifo_state_with_the_shared_text() {
        let mut manager = DraftSessionManager::new();
        let code = start_pod(&mut manager, test_config());
        let legacy = manager.sessions[&code].to_persisted();
        let (_, fifo) = started_fifo_core(&legacy);
        let mut review = legacy.clone();
        review.session.status = DraftStatus::Review;
        let mut residual = legacy.clone();
        residual.session.fifo = fifo.fifo;
        let mut burning = legacy.clone();
        burning.config.selection_policy.burns_per_pick = 1;
        burning.session.config = burning.config.clone();
        for payload in [review, residual, burning] {
            // Reach: a legacy-flow row that the snapshot validator refuses
            // through the same runtime admission.
            assert_eq!(
                payload.session.config.flow,
                draft_core::types::DraftFlow::LegacyBarrier
            );
            assert_eq!(
                payload.session.validate_persisted_snapshot(),
                Err(draft_core::types::DraftError::IncompatibleFlow)
            );
            assert_restore_refused(
                &mut manager,
                &code,
                payload,
                draft_core::types::FIFO_UNSUPPORTED_BY_RUNTIME,
            );
        }
    }

    #[test]
    fn native_restore_still_validates_legacy_rows_after_the_runtime_admission() {
        let mut manager = DraftSessionManager::new();
        let code = start_pod(&mut manager, test_config());
        let legacy = manager.sessions[&code].to_persisted();
        // A legacy row whose session kind disagrees with its configuration
        // passes the runtime admission and keeps the validator's refusal.
        let mut mislabelled = legacy.clone();
        mislabelled.session.kind = DraftKind::Traditional;
        mislabelled.session.validate_legacy_admission().unwrap();
        let refusal = mislabelled
            .session
            .validate_persisted_snapshot()
            .unwrap_err();
        assert_restore_refused(
            &mut manager,
            &code,
            mislabelled,
            &format!("invalid persisted draft snapshot: {refusal}"),
        );

        // A valid legacy row restores, and its seats can pick.
        let restored = DraftSession::from_persisted(legacy.clone()).unwrap();
        assert_eq!(restored.session, legacy.session);
        let mut fresh = DraftSessionManager::new();
        fresh.restore_persisted_session(legacy).unwrap();
        let token = fresh.sessions[&code].player_tokens[0].clone();
        fresh
            .handle_draft_action(
                &code,
                &token,
                DraftAction::Pick {
                    seat: 0,
                    card_instance_ids: vec!["TST-0-0-0".into()],
                },
                None,
            )
            .unwrap();
        assert_eq!(fresh.sessions[&code].session.pools[0].len(), 1);
    }

    /// CR 903.13b: the disconnected-seat auto-pick takes the kind's WHOLE pick
    /// step — two cards for a Commander pod.
    ///
    /// REVERT-PROBE: restore the hardcoded `vec![card_instance_id]` and
    /// `pick_random_for_seat` returns `Err` (the reducer's `WrongPickCardCount`),
    /// so the `unwrap` below panics AND the pool-length assertion fails.
    #[test]
    fn commander_auto_pick_takes_two_cards() {
        let mut mgr = DraftSessionManager::new();
        let code = start_pod(&mut mgr, commander_test_config());

        mgr.pick_random_for_seat(&code, 1, None).unwrap();

        let view = draft_core::view::filter_for_player(&mgr.sessions[&code].session, 1);
        assert_eq!(
            view.pool.len(),
            2,
            "CR 903.13b: a Commander pick step drafts two cards"
        );
        // Distinct ids: `swap_remove` must not have drawn the same card twice.
        assert_ne!(view.pool[0].instance_id, view.pool[1].instance_id);
    }

    /// The paired control: a CR 905.1a kind still takes exactly one card, so
    /// the change reads the procedure rather than hardcoding two.
    #[test]
    fn premier_auto_pick_still_takes_one_card() {
        let mut mgr = DraftSessionManager::new();
        let code = start_pod(&mut mgr, test_config());

        mgr.pick_random_for_seat(&code, 1, None).unwrap();

        let view = draft_core::view::filter_for_player(&mgr.sessions[&code].session, 1);
        assert_eq!(view.pool.len(), 1, "CR 905.1a: one card per pick step");
    }

    #[test]
    fn create_draft_returns_code_and_token() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, seat) = mgr.create_draft(test_config(), "Alice".to_string());

        assert_eq!(code.len(), 6);
        assert_eq!(token.len(), 32);
        assert_eq!(seat, 0);
        assert!(mgr.sessions.contains_key(&code));
    }

    #[test]
    fn join_draft_enforces_lobby_password() {
        let mut mgr = DraftSessionManager::new();
        let (code, _host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        mgr.sessions.get_mut(&code).unwrap().lobby_meta = Some(PersistedLobbyMeta {
            host_name: "Alice".to_string(),
            public: false,
            password: Some("secret".to_string()),
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        });

        assert_eq!(
            mgr.join_draft(&code, "Bob".to_string(), None).unwrap_err(),
            "password_required"
        );
        assert_eq!(
            mgr.join_draft(&code, "Bob".to_string(), Some("wrong"))
                .unwrap_err(),
            "Wrong password"
        );
        assert!(mgr
            .join_draft(&code, "Bob".to_string(), Some("secret"))
            .is_ok());
    }

    #[test]
    fn join_draft_rejects_after_draft_started() {
        let mut mgr = DraftSessionManager::new();
        let (code, _host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        mgr.sessions.get_mut(&code).unwrap().lobby_meta = Some(PersistedLobbyMeta {
            host_name: "Alice".to_string(),
            public: false,
            password: Some("secret".to_string()),
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        });

        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), Some("secret"))
                .unwrap();
        }

        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        mgr.apply_system_action(&code, DraftAction::StartDraft, Some(&source))
            .unwrap();

        assert!(mgr.sessions[&code].lobby_meta.is_none());
        assert_eq!(mgr.sessions[&code].session.status, DraftStatus::Drafting);

        let result = mgr.join_draft(&code, "Late".to_string(), Some("secret"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already started"));
    }

    #[test]
    fn started_persisted_draft_does_not_register_in_lobby() {
        use crate::persist::restored_draft_lobby_register_request;

        let mut mgr = DraftSessionManager::new();
        let (code, _host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        mgr.sessions.get_mut(&code).unwrap().lobby_meta = Some(PersistedLobbyMeta {
            host_name: "Alice".to_string(),
            public: true,
            password: None,
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        });

        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }

        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        mgr.apply_system_action(&code, DraftAction::StartDraft, Some(&source))
            .unwrap();

        // Simulate legacy persistence that still carried lobby_meta after start.
        let mut persisted = mgr.sessions[&code].to_persisted();
        persisted.lobby_meta = Some(PersistedLobbyMeta {
            host_name: "Alice".to_string(),
            public: true,
            password: None,
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        });

        assert!(restored_draft_lobby_register_request(&persisted).is_none());
    }

    #[test]
    fn restore_registration_path_registers_only_lobby_status_drafts() {
        use std::cell::Cell;

        use lobby_broker::{BrokerEnv, LobbyManager};

        use crate::persist::restored_draft_lobby_register_request;

        struct TestEnv {
            now: Cell<u64>,
        }

        impl BrokerEnv for TestEnv {
            fn now_ms(&self) -> u64 {
                self.now.get()
            }

            fn new_token(&self) -> String {
                "tok".to_string()
            }

            fn new_game_code(&self) -> String {
                "CODE".to_string()
            }
        }

        fn register_restored_draft(
            lob: &mut LobbyManager,
            draft_code: &str,
            ps: &crate::persist::PersistedDraftSession,
            env: &TestEnv,
        ) {
            if let Some(req) = restored_draft_lobby_register_request(ps) {
                lob.register_game(draft_code, req, env);
            }
        }

        let env = TestEnv {
            now: Cell::new(1_000_000),
        };
        let mut lob = LobbyManager::new();
        let meta = PersistedLobbyMeta {
            host_name: "Alice".to_string(),
            public: true,
            password: Some("secret".to_string()),
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        };

        // Still in lobby — should register on restore.
        let mut lobby_mgr = DraftSessionManager::new();
        let (lobby_code, _host_token, _) =
            lobby_mgr.create_draft(test_config(), "Alice".to_string());
        lobby_mgr.sessions.get_mut(&lobby_code).unwrap().lobby_meta = Some(meta.clone());
        let lobby_ps = lobby_mgr.sessions[&lobby_code].to_persisted();

        // Started with open seats and stale lobby_meta — must not register.
        let mut drafting_mgr = DraftSessionManager::new();
        let (draft_code, _host_token, _) =
            drafting_mgr.create_draft(test_config(), "Alice".to_string());
        drafting_mgr
            .sessions
            .get_mut(&draft_code)
            .unwrap()
            .lobby_meta = Some(meta);
        drafting_mgr
            .join_draft(&draft_code, "Bob".to_string(), Some("secret"))
            .unwrap();
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        drafting_mgr
            .apply_system_action(&draft_code, DraftAction::StartDraft, Some(&source))
            .unwrap();
        let mut drafting_ps = drafting_mgr.sessions[&draft_code].to_persisted();
        drafting_ps.lobby_meta = Some(PersistedLobbyMeta {
            host_name: "Alice".to_string(),
            public: true,
            password: Some("secret".to_string()),
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        });
        assert_eq!(drafting_ps.session.status, DraftStatus::Drafting);
        assert!(
            drafting_ps
                .player_tokens
                .iter()
                .filter(|t| !t.is_empty())
                .count()
                < 8,
            "started draft should still have open seats"
        );

        register_restored_draft(&mut lob, &lobby_code, &lobby_ps, &env);
        register_restored_draft(&mut lob, &draft_code, &drafting_ps, &env);

        assert!(
            lob.has_game(&lobby_code),
            "lobby-status draft must re-register on restore"
        );
        assert!(
            !lob.has_game(&draft_code),
            "started draft with stale lobby_meta must not re-register"
        );
    }

    #[test]
    fn join_draft_assigns_seat() {
        let mut mgr = DraftSessionManager::new();
        let (code, _host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        let result = mgr.join_draft(&code, "Bob".to_string(), None);
        assert!(result.is_ok());
        let (token, seat, _view) = result.unwrap();
        assert_eq!(token.len(), 32);
        assert_eq!(seat, 1);
    }

    #[test]
    fn join_full_draft_fails() {
        let mut mgr = DraftSessionManager::new();
        let (code, _host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        // Fill all 8 seats (seat 0 is the host)
        for i in 1..8 {
            let result = mgr.join_draft(&code, format!("Player {i}"), None);
            assert!(result.is_ok(), "Failed to join seat {i}");
        }

        // 9th join should fail
        let result = mgr.join_draft(&code, "TooMany".to_string(), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("full"));
    }

    #[test]
    fn draft_for_token_lookup_works() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        assert_eq!(mgr.draft_for_token(&token), Some(code.as_str()));
        assert_eq!(mgr.draft_for_token("nonexistent"), None);
    }

    #[test]
    fn disconnect_and_reconnect_works() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        mgr.handle_disconnect(&code, 0, std::time::Instant::now());
        assert!(!mgr.sessions[&code].connected[0]);

        let result = mgr.handle_reconnect(&code, &token, std::time::Instant::now());
        assert!(result.is_ok());
        assert!(mgr.sessions[&code].connected[0]);
    }

    #[test]
    fn handle_draft_action_validates_token() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        let result = mgr.handle_draft_action(&code, "invalid-token", DraftAction::StartDraft, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid player token"));
    }

    #[test]
    fn apply_system_action_bypasses_token() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        // Fill the pod so we can start
        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }

        // System action bypasses token validation
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let result = mgr.apply_system_action(&code, DraftAction::StartDraft, Some(&source));
        assert!(result.is_ok());

        // Verify session transitioned to Drafting
        assert_eq!(mgr.sessions[&code].session.status, DraftStatus::Drafting);
    }

    #[test]
    fn draft_code_is_uppercase_alphanumeric() {
        let code = generate_draft_code();
        assert_eq!(code.len(), 6);
        assert!(code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
    }

    #[test]
    fn draft_for_game_code_finds_match() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .active_matches
            .insert("r1-t0".to_string(), "GAME01".to_string());

        assert_eq!(mgr.draft_for_game_code("GAME01"), Some(code));
        assert_eq!(mgr.draft_for_game_code("NONEXIST"), None);
    }

    #[test]
    fn active_match_for_seat_uses_the_current_round_pairing_order() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        let session = mgr.sessions.get_mut(&code).unwrap();
        session.session.status = DraftStatus::MatchInProgress;
        session.session.current_round = 2;
        session.session.pairings.extend([
            DraftPairing {
                round: 1,
                table: 0,
                players: [PlayerId(0), PlayerId(4)],
                match_id: "r1-t0".to_string(),
                status: PairingStatus::Pending,
                winner: None,
            },
            DraftPairing {
                round: 2,
                table: 0,
                players: [PlayerId(4), PlayerId(0)],
                match_id: "r2-t0".to_string(),
                status: PairingStatus::Pending,
                winner: None,
            },
        ]);
        session
            .active_matches
            .insert("r1-t0".to_string(), "OLD001".to_string());
        session
            .active_matches
            .insert("r2-t0".to_string(), "GAME02".to_string());

        let (pairing, game_code, game_player) = session
            .active_match_for_seat(0)
            .expect("seat zero has a current match");
        assert_eq!(pairing.match_id, "r2-t0");
        assert_eq!(game_code, "GAME02");
        assert_eq!(game_player, PlayerId(1));
    }

    #[test]
    fn spawn_match_games_skips_pairing_without_submitted_decks() {
        let mut draft_mgr = DraftSessionManager::new();
        let (code, _host_token, _) = draft_mgr.create_draft(test_config(), "Alice".to_string());
        draft_mgr
            .join_draft(&code, "Bob".to_string(), None)
            .unwrap();

        draft_mgr
            .sessions
            .get_mut(&code)
            .unwrap()
            .session
            .pairings
            .push(DraftPairing {
                round: 1,
                table: 0,
                players: [PlayerId(0), PlayerId(1)],
                match_id: "r1-t0".to_string(),
                status: PairingStatus::Pending,
                winner: None,
            });

        let mut game_mgr = SessionManager::new();
        let spawns = draft_mgr
            .spawn_match_games_for_round(
                &code,
                &mut game_mgr,
                &std::sync::Arc::new(CardDatabase::default()),
                &ComboTable::default(),
                1,
            )
            .expect("missing deck submissions should skip only the incomplete pairing");

        assert!(spawns.is_empty());
        assert_eq!(game_mgr.game_count(), 0);
    }

    #[test]
    fn to_persisted_roundtrips_through_serde_json() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }

        let session = &mgr.sessions[&code];
        let persisted = session.to_persisted();
        let json = serde_json::to_string(&persisted).expect("serialize");
        let restored: PersistedDraftSession = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.draft_code, persisted.draft_code);
        assert_eq!(restored.player_tokens, persisted.player_tokens);
        assert_eq!(restored.display_names, persisted.display_names);
        assert_eq!(restored.timer_remaining_ms, persisted.timer_remaining_ms);
    }

    #[test]
    fn from_persisted_sets_connected_false_and_timer_task_none() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        let persisted = mgr.sessions[&code].to_persisted();
        let restored = DraftSession::from_persisted(persisted).unwrap();

        assert!(restored.connected.iter().all(|&c| !c));
        assert!(restored
            .legacy_schedule()
            .expect("a restored legacy pod runs the legacy schedule")
            .timer_task
            .is_none());
    }

    #[test]
    fn restore_session_rebuilds_token_to_draft_index() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }

        let persisted = mgr.sessions[&code].to_persisted();
        let tokens = persisted.player_tokens.clone();

        // Create a fresh manager and restore into it
        let mut mgr2 = DraftSessionManager::new();
        mgr2.restore_session(persisted).unwrap();

        // All tokens should resolve to the same draft code
        for t in &tokens {
            if !t.is_empty() {
                assert_eq!(mgr2.draft_for_token(t), Some(code.as_str()));
            }
        }

        // Original host token should still work
        assert_eq!(mgr2.draft_for_token(&token), Some(code.as_str()));
    }

    #[test]
    fn restore_persisted_session_rejects_mismatched_wrapper_without_mutation() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        let mut persisted = mgr.sessions[&code].to_persisted();
        persisted.display_names.pop();

        let mut restored = DraftSessionManager::new();
        assert!(restored.restore_persisted_session(persisted).is_err());
        assert!(restored.sessions.is_empty());
    }

    #[test]
    fn restore_persisted_session_rejects_duplicate_player_tokens() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        let mut persisted = mgr.sessions[&code].to_persisted();
        persisted.player_tokens[1] = token;

        let mut restored = DraftSessionManager::new();
        assert!(restored.restore_persisted_session(persisted).is_err());
        assert!(restored.sessions.is_empty());
    }

    #[test]
    fn restore_persisted_session_accepts_chaos_through_redacted_player_views() {
        let mut source = DraftSessionManager::new();
        let (code, token, _) = source.create_draft(test_config(), "Alice".to_string());
        let mut persisted = source.sessions[&code].to_persisted();
        let chaos_source = DraftSource::Set {
            layout: draft_core::types::SetLayout::Chaos {
                candidate_codes: vec!["TST".to_string()],
                assignments: vec![vec!["TST".to_string(); 3]; 8],
            },
        };
        persisted.config.source = chaos_source.clone();
        persisted.session.config.source = chaos_source;

        let mut restored = DraftSessionManager::new();
        restored
            .restore_persisted_session(persisted)
            .expect("redacted core views make persisted Chaos sessions safe to restore");

        let view = restored.sessions[&code].view_for_seat(0);
        let serialized = serde_json::to_string(&view).expect("serialize player view");
        assert!(
            !serialized.contains("assignments"),
            "a server player view must not serialize Chaos assignments: {serialized}"
        );
        assert!(restored.sessions.contains_key(&code));
        assert_eq!(restored.draft_for_token(&token), Some(code.as_str()));
    }

    fn fill_and_start(mgr: &mut DraftSessionManager, code: &str) {
        for i in 1..8 {
            mgr.join_draft(code, format!("Player {i}"), None).unwrap();
        }
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        mgr.apply_system_action(code, DraftAction::StartDraft, Some(&source))
            .unwrap();
    }

    #[test]
    fn pick_random_for_seat_picks_from_available_pack() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        fill_and_start(&mut mgr, &code);

        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };

        // Get seat 0's pool size before auto-pick
        let pool_before = mgr.sessions[&code].session.pools[0].len();

        // Auto-pick for seat 0
        let result = mgr.pick_random_for_seat(&code, 0, Some(&source));
        assert!(result.is_ok(), "auto-pick failed: {:?}", result.err());

        // Pool should grow by 1
        let pool_after = mgr.sessions[&code].session.pools[0].len();
        assert_eq!(pool_after, pool_before + 1);
    }

    #[test]
    fn pick_random_for_seat_fails_when_not_drafting() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        // Session is in Lobby status, not Drafting
        let result = mgr.pick_random_for_seat(&code, 0, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not in Drafting status"));
    }

    #[test]
    fn join_draft_is_refused_while_the_host_is_away() {
        let mut mgr = DraftSessionManager::new();
        let (code, host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        mgr.handle_disconnect(&code, DRAFT_HOST_SEAT, std::time::Instant::now());
        let tokens = mgr.sessions[&code].player_tokens.clone();
        let connected = mgr.sessions[&code].connected.clone();
        let indexed = mgr.token_to_draft.len();

        assert_eq!(
            mgr.join_draft(&code, "Bob".to_string(), None).map(|_| ()),
            Err(HOST_AWAY_REFUSAL.to_string())
        );
        assert_eq!(mgr.sessions[&code].player_tokens, tokens);
        assert_eq!(mgr.sessions[&code].connected, connected);
        assert_eq!(mgr.token_to_draft.len(), indexed);

        mgr.handle_reconnect(&code, &host_token, std::time::Instant::now())
            .expect("host returns");
        let (_, seat, _) = mgr
            .join_draft(&code, "Bob".to_string(), None)
            .expect("admitted once the host is back");
        assert_eq!(seat, 1);
    }

    #[test]
    fn a_lobby_pod_is_live_while_its_host_is_connected_or_in_grace() {
        let mut mgr = DraftSessionManager::new();
        let (code, host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        assert!(mgr.is_live_lobby_pod(&code));
        mgr.handle_disconnect(&code, DRAFT_HOST_SEAT, std::time::Instant::now());
        assert!(mgr.is_live_lobby_pod(&code));

        mgr.handle_reconnect(&code, &host_token, std::time::Instant::now())
            .expect("host returns");
        mgr.reconnect.grace_period = std::time::Duration::ZERO;
        mgr.handle_disconnect(&code, DRAFT_HOST_SEAT, std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(
            mgr.reconnect
                .check_expired_with_players(std::time::Instant::now()),
            vec![(code.clone(), PlayerId(0))]
        );
        assert!(!mgr.is_live_lobby_pod(&code));

        let drafting = start_pod(&mut mgr, test_config());
        assert!(!mgr.is_live_lobby_pod(&drafting));
    }

    #[test]
    fn sweep_expired_seats_reaps_frees_and_auto_picks_by_status() {
        let mut mgr = DraftSessionManager::new();
        let (reaped, _, _) = mgr.create_draft(test_config(), "Alice".to_string());
        mgr.join_draft(&reaped, "Bob".to_string(), None).unwrap();
        let (freed, _, _) = mgr.create_draft(test_config(), "Carol".to_string());
        let (lapsed_guest, _, _) = mgr.join_draft(&freed, "Dan".to_string(), None).unwrap();
        let (waiting, _, _) = mgr.create_draft(test_config(), "Erin".to_string());
        let (waiting_guest, _, _) = mgr.join_draft(&waiting, "Finn".to_string(), None).unwrap();
        let drafting = start_pod(&mut mgr, test_config());

        mgr.handle_disconnect(&reaped, 1, std::time::Instant::now());
        mgr.handle_disconnect(&waiting, 1, std::time::Instant::now());
        mgr.reconnect.grace_period = std::time::Duration::ZERO;
        mgr.handle_disconnect(&reaped, DRAFT_HOST_SEAT, std::time::Instant::now());
        mgr.handle_disconnect(&freed, 1, std::time::Instant::now());
        mgr.handle_disconnect(&drafting, DRAFT_HOST_SEAT, std::time::Instant::now());
        let pool_before = mgr.sessions[&drafting].session.pools[0].len();
        std::thread::sleep(std::time::Duration::from_millis(2));

        let sweep = mgr.sweep_expired_seats(std::time::Instant::now());

        let reaped_codes: Vec<&str> = sweep.reaped.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(reaped_codes, [reaped.as_str()]);
        assert!(!mgr.sessions.contains_key(&reaped));
        assert!(!mgr.reconnect.is_disconnected(&reaped, PlayerId(1)));

        assert_eq!(sweep.freed, [freed.as_str()]);
        let pod = &mgr.sessions[&freed];
        assert_eq!(pod.first_open_seat(), Some(1));
        assert!(pod.display_names[1].is_empty());
        assert_eq!(mgr.draft_for_token(&lapsed_guest), None);

        assert_eq!(
            mgr.sessions[&waiting].seat_for_token(&waiting_guest),
            Some(1)
        );
        assert!(mgr.reconnect.is_disconnected(&waiting, PlayerId(1)));

        assert!(mgr.sessions.contains_key(&drafting));
        assert_eq!(
            mgr.sessions[&drafting].session.pools[0].len(),
            pool_before + 1
        );
        assert!(sweep.affected.contains(&drafting));
        assert!(sweep.affected.contains(&freed));
        assert!(!sweep.affected.contains(&reaped));
    }

    #[test]
    fn restore_persisted_session_records_every_claimed_seat() {
        let mut mgr = DraftSessionManager::new();
        let (code, _, _) = mgr.create_draft(test_config(), "Alice".to_string());
        for i in 1..4 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }
        let persisted = mgr.sessions[&code].to_persisted();

        let mut restored = DraftSessionManager::new();
        restored.restore_persisted_session(persisted).unwrap();

        let recorded: Vec<bool> = (0..8u8)
            .map(|seat| restored.reconnect.is_disconnected(&code, PlayerId(seat)))
            .collect();
        assert_eq!(
            recorded,
            [true, true, true, true, false, false, false, false]
        );
    }

    #[test]
    fn authorize_rebinds_pick_seat_to_authenticated_seat() {
        // A non-host (seat 2) picks but spoofs seat 0's pack: the seat is
        // overwritten with the authenticated seat, never the payload's value.
        let action = authorize_client_draft_action(
            2,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec!["abc".to_string()],
            },
        )
        .expect("seat-scoped action is allowed for any seat");
        assert_eq!(
            action,
            DraftAction::Pick {
                seat: 2,
                card_instance_ids: vec!["abc".to_string()]
            }
        );
    }

    #[test]
    fn authorize_rebinds_draft_effect_pick_seat_to_authenticated_seat() {
        let action = authorize_client_draft_action(
            2,
            DraftAction::PickWithDraftEffect {
                seat: 0,
                effect_card_instance_id: "cogwork-1".to_string(),
                card_instance_ids: vec!["card-1".to_string(), "card-2".to_string()],
            },
        )
        .expect("seat-scoped action is allowed for any seat");
        assert_eq!(
            action,
            DraftAction::PickWithDraftEffect {
                seat: 2,
                effect_card_instance_id: "cogwork-1".to_string(),
                card_instance_ids: vec!["card-1".to_string(), "card-2".to_string()],
            }
        );
    }

    /// The seat is TABLE authority and is overwritten with the authenticated
    /// one; the CR 903.3 designation is PLAYER data and must survive untouched.
    ///
    /// The second half is the discriminating assertion for this seam: a
    /// reconstruction that dropped `commanders` (or reset it to `Vec::new()`)
    /// would leave the server path silently unable to designate a commander at
    /// all, while every draft-core test still passed.
    #[test]
    fn authorize_rebinds_submit_deck_seat_and_carries_the_designation() {
        let action = authorize_client_draft_action(
            3,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec!["x".to_string()],
                commanders: vec!["x".to_string()],
                companion: Some("Yorion, Sky Nomad".to_string()),
                printings: Vec::new(),
            },
        )
        .expect("submit deck is allowed for any seat");
        assert_eq!(
            action,
            DraftAction::SubmitDeck {
                seat: 3,
                main_deck: vec!["x".to_string()],
                commanders: vec!["x".to_string()],
                companion: Some("Yorion, Sky Nomad".to_string()),
                printings: Vec::new(),
            }
        );
    }

    #[test]
    fn authorize_rejects_host_actions_from_non_host() {
        for action in [
            DraftAction::StartDraft,
            DraftAction::AdvanceRound,
            DraftAction::ReportMatchResult {
                match_id: "m".to_string(),
                winner_seat: Some(1),
            },
            DraftAction::ReplaceSeatWithBot {
                seat: 1,
                name: None,
            },
        ] {
            assert!(
                authorize_client_draft_action(1, action).is_err(),
                "non-host (seat 1) must not drive table-authority actions"
            );
        }
    }

    #[test]
    fn authorize_allows_host_actions_from_host() {
        assert!(authorize_client_draft_action(DRAFT_HOST_SEAT, DraftAction::StartDraft).is_ok());
        assert!(authorize_client_draft_action(DRAFT_HOST_SEAT, DraftAction::AdvanceRound).is_ok());
    }

    #[test]
    fn authorize_rejects_set_seat_connected_from_client() {
        assert!(authorize_client_draft_action(
            DRAFT_HOST_SEAT,
            DraftAction::SetSeatConnected {
                seat: 1,
                connected: false,
            },
        )
        .is_err());
    }

    #[test]
    fn draft_seats_needing_auto_pick_skips_already_picked() {
        use draft_core::pack_source::FixturePackSource;
        use draft_core::session;
        use draft_core::types::{
            DeckAddableCards, DraftAction, DraftKind, DraftSeat, DraftSource, PodPolicy,
            SpectatorVisibility, TournamentFormat,
        };
        use engine::types::player::PlayerId;

        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 2,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..2)
            .map(|i| DraftSeat::Human {
                player_id: PlayerId(i),
                display_name: format!("Player {i}"),
            })
            .collect();
        let source = FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let mut session =
            draft_core::types::DraftSession::new(config, seats, "TEST-001".to_string());
        session::apply(&mut session, DraftAction::StartDraft, Some(&source)).unwrap();

        let card_id = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session::apply(
            &mut session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![card_id],
            },
            None,
        )
        .unwrap();

        let seats = draft_seats_needing_auto_pick(&mut session, 2);
        assert_eq!(seats, vec![1]);
        assert!(!seats.contains(&0));
    }

    /// The private FIFO fixture: 8 seats, `TST`, 2-card packs, 3 packs, a
    /// fixed 1000 ms pick clock and a 1000 ms Review.
    fn private_fifo_config() -> DraftConfig {
        use draft_core::types::{DraftClockPolicy, DraftFlow, PickClockPolicy};
        DraftConfig {
            flow: DraftFlow::FifoAsync,
            cards_per_pack: 2,
            clock_policy: DraftClockPolicy {
                pick: PickClockPolicy::Fixed { duration_ms: 1000 },
                review_duration_ms: 1000,
            },
            ..test_config()
        }
    }

    fn fifo_source() -> draft_core::pack_source::FixturePackSource {
        draft_core::pack_source::FixturePackSource {
            set_code: "TST".into(),
            cards_per_pack: 2,
        }
    }

    /// Stages and installs a private FIFO pod; returns its code and tokens.
    fn install_private_pod(mgr: &mut DraftSessionManager, config: DraftConfig) -> String {
        let names = (0..config.pod_size)
            .map(|seat| format!("Player {seat}"))
            .collect();
        let staged = mgr.stage_private_fifo(config, names).unwrap();
        let code = staged.draft_code().to_string();
        mgr.install_fifo(staged).unwrap();
        code
    }

    /// Stages one FIFO operation and commits it as a saved result would be.
    fn fifo_op(
        mgr: &mut DraftSessionManager,
        code: &str,
        actor: DraftActor<'_>,
        action: DraftAction,
        now_ms: u64,
    ) -> FifoStaged {
        let staged = mgr
            .stage_fifo_operation(code, actor, action, Some(&fifo_source()), now_ms)
            .unwrap();
        if staged == FifoStaged::Pending {
            mgr.commit_fifo_result(code).unwrap();
        }
        staged
    }

    /// The seat's head selection, submitted through its own token.
    fn fifo_select_head(mgr: &mut DraftSessionManager, code: &str, seat: u8, now_ms: u64) {
        let token = mgr.sessions[code].player_tokens[usize::from(seat)].clone();
        let action = draft_core::pick_pass::legal_auto_selection(&mgr.sessions[code].session, seat)
            .expect("the seat has a head");
        assert_eq!(
            fifo_op(
                mgr,
                code,
                DraftActor::Seat { token: &token },
                action,
                now_ms
            ),
            FifoStaged::Pending
        );
    }

    /// A seat prompt's running deadline, read from the committed core.
    fn prompt_deadline(mgr: &DraftSessionManager, code: &str, seat: usize) -> Option<u64> {
        let clock = mgr.sessions[code].session.fifo.as_ref()?.seats[seat]
            .prompt
            .as_ref()?
            .clock
            .as_ref()?;
        match clock.state {
            draft_core::types::DraftClockState::Running { deadline_ms } => Some(deadline_ms),
            draft_core::types::DraftClockState::Frozen { .. } => None,
        }
    }

    #[tokio::test]
    async fn removing_a_fifo_owner_aborts_its_wake_task() {
        use tokio::sync::oneshot::error::TryRecvError;
        let mut mgr = DraftSessionManager::new();
        let code = install_private_pod(&mut mgr, private_fifo_config());
        let registration = mgr
            .register_fifo_wake(&code, 2000)
            .expect("a FIFO owner registers a wake");
        // A wake task that never finishes on its own; `ended` reports
        // `Closed` once the task is dropped.
        let (alive, mut ended) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _alive = alive;
            std::future::pending::<()>().await;
        });
        mgr.attach_fifo_wake_task(&code, &registration, task);
        tokio::task::yield_now().await;
        assert_eq!(mgr.current_fifo_wake(&code), Some(registration));
        assert_eq!(ended.try_recv(), Err(TryRecvError::Empty));

        let removed = mgr.remove_draft(&code).expect("the FIFO owner");
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(ended.try_recv(), Err(TryRecvError::Closed));
        drop(removed);
    }

    #[test]
    fn schedule_arm_follows_the_native_classification() {
        let is_legacy =
            |session: &DraftSession| matches!(session.schedule, DraftSchedule::Legacy(_));
        // A created legacy pod.
        let mut mgr = DraftSessionManager::new();
        let created = start_pod(&mut mgr, test_config());
        assert_eq!(
            native_flow(&mgr.sessions[&created].session),
            NativeRowFlow::Legacy
        );
        assert!(is_legacy(&mgr.sessions[&created]));
        // A restored legacy pod and a restored Winston row.
        let mut winston_mgr = DraftSessionManager::new();
        let winston = start_pod(&mut winston_mgr, winston_test_config());
        let mut fresh = DraftSessionManager::new();
        fresh
            .restore_persisted_session(mgr.sessions[&created].to_persisted())
            .unwrap();
        fresh
            .restore_persisted_session(winston_mgr.sessions[&winston].to_persisted())
            .unwrap();
        for code in [&created, &winston] {
            assert_eq!(
                native_flow(&fresh.sessions[code].session),
                NativeRowFlow::Legacy
            );
            assert!(is_legacy(&fresh.sessions[code]));
        }
        // A private stage and an internal restore.
        let private = install_private_pod(&mut fresh, private_fifo_config());
        assert_eq!(
            native_flow(&fresh.sessions[&private].session),
            NativeRowFlow::Fifo
        );
        assert!(!is_legacy(&fresh.sessions[&private]));
        let row = fresh.sessions[&private].to_persisted();
        let mut restored = DraftSessionManager::new();
        let staged = restored.stage_restored_fifo(row).unwrap();
        restored.install_fifo(staged).unwrap();
        assert!(!is_legacy(&restored.sessions[&private]));
        assert_eq!(restored.sessions.len(), 1);
    }

    #[test]
    fn fifo_next_due_reads_the_earliest_running_core_deadline() {
        use draft_core::types::{DraftControl, DraftPauseReason};
        let mut mgr = DraftSessionManager::new();
        let code = install_private_pod(&mut mgr, private_fifo_config());
        // Unsaved: a staged start is not yet committed.
        assert_eq!(
            mgr.stage_fifo_operation(
                &code,
                DraftActor::Server,
                DraftAction::StartDraft,
                Some(&fifo_source()),
                1000
            ),
            Ok(FifoStaged::Pending)
        );
        assert!(mgr.pending_fifo_row(&code).is_some());
        assert_eq!(mgr.fifo_next_due(&code), None);
        mgr.commit_fifo_result(&code).unwrap();
        assert_eq!(prompt_deadline(&mgr, &code, 0), Some(2000));
        assert_eq!(mgr.fifo_next_due(&code), Some(2000));

        fifo_select_head(&mut mgr, &code, 0, 1500);
        assert_eq!(mgr.fifo_next_due(&code), Some(2000));
        // Seat 1 takes its own head; seat 0's remainder becomes its next head.
        fifo_select_head(&mut mgr, &code, 1, 1500);
        assert_eq!(prompt_deadline(&mgr, &code, 1), Some(2500));
        for seat in 2..8 {
            assert_eq!(prompt_deadline(&mgr, &code, seat), Some(2000));
        }
        assert_eq!(mgr.fifo_next_due(&code), Some(2000));

        let pause = DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        };
        fifo_op(&mut mgr, &code, DraftActor::Server, pause, 1600);
        assert_eq!(mgr.fifo_next_due(&code), None);
        let resume = DraftAction::Control {
            control: DraftControl::Resume,
        };
        fifo_op(&mut mgr, &code, DraftActor::Server, resume, 3000);
        assert_eq!(prompt_deadline(&mgr, &code, 1), Some(3900));
        assert_eq!(prompt_deadline(&mgr, &code, 2), Some(3400));
        assert_eq!(mgr.fifo_next_due(&code), Some(3400));

        // A session with no pick clock.
        let off = install_private_pod(
            &mut mgr,
            DraftConfig {
                clock_policy: draft_core::types::DraftClockPolicy {
                    pick: draft_core::types::PickClockPolicy::Off,
                    review_duration_ms: 0,
                },
                ..private_fifo_config()
            },
        );
        fifo_op(
            &mut mgr,
            &off,
            DraftActor::Server,
            DraftAction::StartDraft,
            1000,
        );
        assert_eq!(mgr.sessions[&off].session.status, DraftStatus::Drafting);
        assert_eq!(mgr.fifo_next_due(&off), None);

        // In Review, the Review deadline.
        let review = install_private_pod(&mut mgr, private_fifo_config());
        fifo_op(
            &mut mgr,
            &review,
            DraftActor::Server,
            DraftAction::StartDraft,
            1000,
        );
        for _ in 0..2 {
            for seat in 0..8 {
                fifo_select_head(&mut mgr, &review, seat, 1100);
            }
        }
        assert_eq!(mgr.sessions[&review].session.status, DraftStatus::Review);
        let review_deadline = match mgr.sessions[&review]
            .session
            .fifo
            .as_ref()
            .unwrap()
            .review
            .as_ref()
            .unwrap()
            .clock
            .state
        {
            draft_core::types::DraftClockState::Running { deadline_ms } => deadline_ms,
            draft_core::types::DraftClockState::Frozen { .. } => panic!("Review runs"),
        };
        assert_eq!(review_deadline, 2100);
        assert_eq!(mgr.fifo_next_due(&review), Some(review_deadline));
        // A legacy pod has no FIFO deadline.
        let legacy = start_pod(&mut mgr, test_config());
        assert_eq!(mgr.fifo_next_due(&legacy), None);
    }

    #[test]
    fn internal_fifo_restore_validates_after_the_classification() {
        use draft_core::types::{DraftControl, DraftPauseReason};
        let mut manager = DraftSessionManager::new();
        let code = start_pod(&mut manager, test_config());
        let legacy = manager.sessions[&code].to_persisted();
        let (config, fifo) = started_fifo_core(&legacy);
        let mut valid = legacy.clone();
        valid.config = config.clone();
        valid.session = fifo.clone();
        valid.session.validate_persisted_snapshot().unwrap();

        // A legacy row the validator also refuses keeps the legacy-row text.
        let mut mislabelled = legacy.clone();
        mislabelled.session.kind = DraftKind::Traditional;
        assert!(mislabelled.session.validate_persisted_snapshot().is_err());
        let mut without_fifo_state = valid.clone();
        without_fifo_state.session.fifo = None;
        let snapshot_refusal = without_fifo_state
            .session
            .validate_persisted_snapshot()
            .unwrap_err();
        let mut with_timer = valid.clone();
        with_timer.timer_remaining_ms = Some(5000);
        let mut with_listing = valid.clone();
        with_listing.lobby_meta = Some(PersistedLobbyMeta {
            host_name: "Alice".into(),
            public: true,
            password: None,
            timer_seconds: None,
            start_when_full: true,
            ranked: false,
        });
        let mut seat_mismatch = valid.clone();
        seat_mismatch.display_names.pop();
        for (row, expected) in [
            (legacy.clone(), LEGACY_ROW_NOT_PRIVATE_FIFO.to_string()),
            (mislabelled, LEGACY_ROW_NOT_PRIVATE_FIFO.to_string()),
            (
                without_fifo_state,
                format!("invalid persisted FIFO snapshot: {snapshot_refusal}"),
            ),
            (
                with_timer,
                "a FIFO draft row carries no legacy pick timer".to_string(),
            ),
            (
                with_listing,
                "a private FIFO draft row carries no lobby listing".to_string(),
            ),
            (
                seat_mismatch,
                "persisted draft seat vectors do not match core seats".to_string(),
            ),
        ] {
            let before = serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap();
            let tokens = manager.token_to_draft.clone();
            assert_eq!(manager.stage_restored_fifo(row).err(), Some(expected));
            assert_eq!(manager.sessions.len(), 1);
            assert_eq!(manager.token_to_draft, tokens);
            assert_eq!(
                serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap(),
                before
            );
        }

        // A valid FIFO row under a resident legacy pod's code is refused
        // and the legacy pod is unchanged.
        let before = serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap();
        let staged = manager.stage_restored_fifo(valid.clone()).unwrap();
        assert_eq!(
            manager.install_fifo(staged),
            Err(DRAFT_CODE_RESIDENT.to_string())
        );
        assert!(matches!(
            manager.sessions[&code].schedule,
            DraftSchedule::Legacy(_)
        ));
        assert_eq!(
            serde_json::to_value(manager.sessions[&code].to_persisted()).unwrap(),
            before
        );

        // The valid row installs in a fresh manager, as do its Review and
        // paused cores.
        let mut review = fifo.clone();
        for _ in 0..2 {
            for seat in 0..8 {
                let action = draft_core::pick_pass::legal_auto_selection(&review, seat).unwrap();
                draft_core::session::apply_at(&mut review, action, None, 1100).unwrap();
            }
        }
        assert_eq!(review.status, DraftStatus::Review);
        let mut paused = review.clone();
        draft_core::session::apply_at(
            &mut paused,
            DraftAction::Control {
                control: DraftControl::Pause {
                    reason: DraftPauseReason::PausedByHost,
                },
            },
            None,
            1500,
        )
        .unwrap();
        for (core, due) in [(fifo, Some(2000)), (review, Some(2100)), (paused, None)] {
            let mut row = valid.clone();
            row.session = core.clone();
            let mut fresh = DraftSessionManager::new();
            let staged = fresh.stage_restored_fifo(row).unwrap();
            fresh.install_fifo(staged).unwrap();
            assert_eq!(fresh.sessions[&code].session, core);
            assert!(matches!(
                fresh.sessions[&code].schedule,
                DraftSchedule::Fifo(_)
            ));
            assert_eq!(fresh.fifo_next_due(&code), due);
            assert_eq!(fresh.current_fifo_wake(&code), None);
        }
    }

    #[test]
    fn restored_winston_pod_keeps_its_shared_stack_turn() {
        let mut started = DraftSessionManager::new();
        let code = start_pod(&mut started, winston_test_config());
        let mut mgr = DraftSessionManager::new();
        mgr.restore_persisted_session(started.sessions[&code].to_persisted())
            .unwrap();
        assert!(matches!(
            mgr.sessions[&code].schedule,
            DraftSchedule::Legacy(_)
        ));
        let decide = |mgr: &DraftSessionManager, seat: u8| {
            let state = mgr.sessions[&code].session.shared_stack().unwrap();
            let decision = draft_core::shared_stack::forced_decision(state, state.active_seat)
                .expect("the active seat has a legal decision");
            (
                mgr.sessions[&code].player_tokens[usize::from(seat)].clone(),
                DraftAction::SharedStackDecision {
                    seat,
                    pile: state.cursor,
                    decision,
                },
            )
        };
        let active = mgr.sessions[&code]
            .session
            .shared_stack()
            .unwrap()
            .active_seat;
        let other = 1 - active;
        // A non-active seat's decision is refused, as before.
        let before = mgr.sessions[&code].session.shared_stack.clone();
        let (token, action) = decide(&mgr, other);
        assert!(mgr
            .handle_draft_action(&code, &token, action, None)
            .is_err());
        assert_eq!(mgr.sessions[&code].session.shared_stack, before);
        // The active seat's decision is accepted.
        let (token, action) = decide(&mgr, active);
        mgr.handle_draft_action(&code, &token, action, None)
            .expect("the restored pod accepts the active seat's decision");
        assert_ne!(mgr.sessions[&code].session.shared_stack, before);
        // The timeout path drives the seat now active.
        let after_decision = mgr.sessions[&code].session.shared_stack.clone();
        let now_active = mgr.sessions[&code]
            .session
            .shared_stack()
            .unwrap()
            .active_seat;
        mgr.pick_random_for_seat(&code, now_active, None).unwrap();
        assert_ne!(mgr.sessions[&code].session.shared_stack, after_decision);
    }

    #[test]
    fn fifo_resident_refuses_public_manager_entries() {
        let fifo_text = draft_core::types::FIFO_UNSUPPORTED_BY_RUNTIME.to_string();
        let mut mgr = DraftSessionManager::new();
        let (legacy, _, _) = mgr.create_draft(test_config(), "Host".to_string());
        let lobby_pod = install_private_pod(&mut mgr, private_fifo_config());
        let drafting_pod = install_private_pod(&mut mgr, private_fifo_config());
        fifo_op(
            &mut mgr,
            &drafting_pod,
            DraftActor::Server,
            DraftAction::StartDraft,
            1000,
        );
        assert_eq!(mgr.sessions[&lobby_pod].session.status, DraftStatus::Lobby);
        assert_eq!(
            mgr.sessions[&drafting_pod].session.status,
            DraftStatus::Drafting
        );
        let now = std::time::Instant::now();
        for code in [&lobby_pod, &drafting_pod] {
            let before = serde_json::to_value(mgr.sessions[code].to_persisted()).unwrap();
            let tokens = mgr.token_to_draft.clone();
            let host = mgr.sessions[code].player_tokens[0].clone();
            assert_eq!(
                mgr.join_draft(code, "Guest".into(), None).err(),
                Some(fifo_text.clone())
            );
            assert_eq!(
                mgr.handle_reconnect(code, &host, now).err(),
                Some(fifo_text.clone())
            );
            assert_eq!(
                mgr.handle_draft_action(code, &host, DraftAction::StartDraft, None)
                    .err(),
                Some(fifo_text.clone())
            );
            assert_eq!(
                mgr.apply_system_action(code, DraftAction::StartDraft, None)
                    .err(),
                Some(fifo_text.clone())
            );
            // Disconnect bookkeeping records no lapse.
            mgr.handle_disconnect(code, 0, now);
            assert!(!mgr.reconnect.is_disconnected(code, PlayerId(0)));
            assert!(!mgr.sessions[code].connected[0]);
            assert_eq!(
                serde_json::to_value(mgr.sessions[code].to_persisted()).unwrap(),
                before
            );
            assert_eq!(mgr.token_to_draft, tokens);
        }
        // The same manager's legacy pod accepts a join and records a lapse.
        let (guest, seat, _) = mgr.join_draft(&legacy, "Guest".into(), None).unwrap();
        assert_eq!(mgr.draft_for_token(&guest), Some(legacy.as_str()));
        mgr.handle_disconnect(&legacy, usize::from(seat), now);
        assert!(mgr.reconnect.is_disconnected(&legacy, PlayerId(seat)));
        // A sweep with zero grace changes nothing on either FIFO pod.
        let rows: Vec<_> = [&lobby_pod, &drafting_pod]
            .map(|code| serde_json::to_value(mgr.sessions[code].to_persisted()).unwrap())
            .into();
        mgr.reconnect.grace_period = std::time::Duration::ZERO;
        let sweep = mgr.sweep_expired_seats(now + std::time::Duration::from_secs(1));
        assert!(!sweep.affected.contains(&lobby_pod));
        assert!(!sweep.affected.contains(&drafting_pod));
        for (code, row) in [&lobby_pod, &drafting_pod].into_iter().zip(rows) {
            assert_eq!(
                serde_json::to_value(mgr.sessions[code].to_persisted()).unwrap(),
                row
            );
        }
    }
}
