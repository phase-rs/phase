use std::collections::HashMap;
use std::time::Duration;

use draft_core::pack_source::PackSource;
use draft_core::types::{
    DraftAction, DraftConfig, DraftDeckSubmission, DraftPairing, DraftSeat, DraftStatus,
    PairingStatus,
};
use draft_core::view::DraftPlayerView;
use engine::types::player::PlayerId;
use rand::Rng;
use tracing::{info, warn};

use crate::deck_resolve;
use crate::persist::{PersistedDraftSession, PersistedLobbyMeta};
use crate::protocol::DeckData;
use crate::reconnect::ReconnectManager;
use crate::session::{generate_player_token, SessionManager};

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
    /// Server-side remaining pick timer in ms. Injected into DraftPlayerView before send.
    pub timer_remaining_ms: Option<u32>,
    /// JoinHandle for the active pick timer task (prevents double-fire).
    pub timer_task: Option<tokio::task::JoinHandle<()>>,
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

    /// Create a serializable snapshot for disk persistence.
    pub fn to_persisted(&self) -> PersistedDraftSession {
        PersistedDraftSession {
            draft_code: self.draft_code.clone(),
            session: self.session.clone(),
            player_tokens: self.player_tokens.clone(),
            display_names: self.display_names.clone(),
            config: self.config.clone(),
            active_matches: self.active_matches.clone(),
            lobby_meta: self.lobby_meta.clone(),
            timer_remaining_ms: self.timer_remaining_ms,
        }
    }

    /// Restore a draft session from a persisted snapshot.
    ///
    /// All players start disconnected. `timer_task` is None — the caller
    /// should re-arm from `timer_remaining_ms` if needed.
    pub fn from_persisted(ps: PersistedDraftSession) -> Self {
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
            timer_remaining_ms: ps.timer_remaining_ms,
            timer_task: None,
        }
    }

    /// Inject server-side timer into the filtered view before serializing.
    pub fn view_for_seat(&self, seat: usize) -> DraftPlayerView {
        let mut view = draft_core::view::filter_for_player(&self.session, seat as u8);
        view.timer_remaining_ms = self.timer_remaining_ms;
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
    (0..pod_size)
        .filter(|&seat_idx| {
            if session.seats_picked_this_round.get(seat_idx as u8) {
                return false;
            }
            session.current_pack[seat_idx]
                .as_ref()
                .is_some_and(|pack| !pack.0.is_empty())
        })
        .collect()
}

pub struct DraftSessionManager {
    pub sessions: HashMap<String, DraftSession>,
    pub reconnect: ReconnectManager,
    /// Maps player_token -> draft_code for O(1) token-based lookups.
    token_to_draft: HashMap<String, String>,
}

impl DraftSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::default(),
            token_to_draft: HashMap::new(),
        }
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
            timer_remaining_ms: None,
            timer_task: None,
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
    pub fn handle_disconnect(&mut self, draft_code: &str, seat: usize) {
        if let Some(session) = self.sessions.get_mut(draft_code) {
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
                .record_disconnect(draft_code, fake_pid, default_grace);
            info!(draft = %draft_code, seat, "player disconnected");
        }
    }

    /// Attempt to reconnect a player. Returns their filtered view on success.
    pub fn handle_reconnect(
        &mut self,
        draft_code: &str,
        token: &str,
    ) -> Result<DraftPlayerView, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;

        let seat = session
            .seat_for_token(token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        let fake_pid = PlayerId(seat as u8);
        match self.reconnect.attempt_reconnect(draft_code, fake_pid) {
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
    pub fn restore_session(&mut self, ps: PersistedDraftSession) {
        let session = DraftSession::from_persisted(ps);
        let draft_code = session.draft_code.clone();
        for token in &session.player_tokens {
            if !token.is_empty() {
                self.token_to_draft
                    .insert(token.clone(), draft_code.clone());
            }
        }
        self.sessions.insert(draft_code, session);
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

        let view = draft_core::view::filter_for_player(&session.session, seat);
        let pack = view
            .current_pack
            .ok_or_else(|| format!("seat {seat} has no pending pack"))?;

        if pack.is_empty() {
            return Err(format!("seat {seat} pack is empty"));
        }

        let idx = rand::rng().random_range(0..pack.len());
        let card_instance_id = pack[idx].instance_id.clone();

        let action = DraftAction::Pick {
            seat,
            card_instance_id,
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
        let round = session.session.current_round.max(1);
        draft_core::session::apply(
            &mut session.session,
            DraftAction::GeneratePairings { round },
            None,
        )
        .map_err(|e| format!("GeneratePairings failed: {e}"))?;
        Ok(())
    }

    /// Spawn 2-player game sessions for pending pairings in `round` using submitted decks.
    pub fn spawn_match_games_for_round(
        &mut self,
        draft_code: &str,
        game_mgr: &mut SessionManager,
        db: &engine::database::CardDatabase,
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
                    deck_payload_from_submission(db, submission)
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

            let (game_code, token0) = game_mgr.create_game_n_players(
                decks[0].clone(),
                name0,
                None,
                2,
                match_config,
                Some(format_config.clone()),
            );
            let (token1, _) = game_mgr.join_game_with_name(&game_code, decks[1].clone(), name1)?;

            game_mgr
                .sessions
                .get_mut(&game_code)
                .ok_or_else(|| format!("spawned game missing: {game_code}"))?
                .start_game(db)
                .map_err(|e| format!("start_game failed for {game_code}: {e:?}"))?;

            session
                .active_matches
                .insert(pairing.match_id.clone(), game_code.clone());

            spawns.push(DraftMatchSpawn {
                match_id: pairing.match_id,
                round,
                game_code,
                player_a: DraftMatchPlayer {
                    draft_seat: pairing.players[0].0,
                    game_token: token0,
                    game_player: PlayerId(0),
                },
                player_b: DraftMatchPlayer {
                    draft_seat: pairing.players[1].0,
                    game_token: token1,
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
    pub fn remove_draft(&mut self, draft_code: &str) -> Option<DraftSession> {
        let session = self.sessions.remove(draft_code)?;
        for token in &session.player_tokens {
            if !token.is_empty() {
                self.token_to_draft.remove(token);
            }
        }
        Some(session)
    }
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
    pub game_token: String,
    pub game_player: PlayerId,
}

fn deck_payload_from_submission(
    db: &engine::database::CardDatabase,
    submission: &DraftDeckSubmission,
) -> Result<engine::game::deck_loading::PlayerDeckPayload, String> {
    let deck = DeckData {
        main_deck: submission.main_deck.clone(),
        sideboard: Vec::new(),
        commander: Vec::new(),
        attraction_deck: Vec::new(),
        signature_spell: Vec::new(),
        ..Default::default()
    };
    deck_resolve::resolve_deck(db, &deck)
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

/// Returns the appropriate reconnect grace period for the given draft phase.
///
/// Longer than the 10s game reconnect because tournaments span hours.
/// - Lobby: 30 min (gathering players)
/// - Drafting: 5 min (picks in progress, auto-pick kicks in after)
/// - Deckbuilding: 15 min (building takes time)
/// - MatchInProgress / BetweenRounds: 10 min
/// - Complete / Abandoned: 1 min (draft is over)
/// - Paused / Pairing / RoundComplete: 10 min (transient states)
pub fn draft_grace_period(status: &DraftStatus) -> Duration {
    match status {
        DraftStatus::Lobby => Duration::from_secs(1800),
        DraftStatus::Drafting => Duration::from_secs(300),
        DraftStatus::Deckbuilding => Duration::from_secs(900),
        DraftStatus::MatchInProgress => Duration::from_secs(600),
        DraftStatus::RoundComplete => Duration::from_secs(600),
        DraftStatus::Paused => Duration::from_secs(600),
        DraftStatus::Pairing => Duration::from_secs(600),
        DraftStatus::Complete | DraftStatus::Abandoned => Duration::from_secs(60),
    }
}

/// The draft host occupies seat 0 (`create_draft` assigns the creator seat 0).
const DRAFT_HOST_SEAT: usize = 0;

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
        // Seat-scoped: overwrite the client-supplied seat with the authenticated
        // seat so a player cannot pick from or submit a deck for another seat.
        DraftAction::Pick {
            card_instance_id, ..
        } => Ok(DraftAction::Pick {
            seat: seat as u8,
            card_instance_id,
        }),
        DraftAction::SubmitDeck { main_deck, .. } => Ok(DraftAction::SubmitDeck {
            seat: seat as u8,
            main_deck,
        }),
        // Table authority: only the host may start the draft, advance rounds,
        // generate pairings, report results, or replace a seat with a bot.
        DraftAction::StartDraft
        | DraftAction::AdvanceRound
        | DraftAction::GeneratePairings { .. }
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
            source: DraftSource::Set {
                code: "TST".to_string(),
            },
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

        mgr.handle_disconnect(&code, 0);
        assert!(!mgr.sessions[&code].connected[0]);

        let result = mgr.handle_reconnect(&code, &token);
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
            .spawn_match_games_for_round(&code, &mut game_mgr, &CardDatabase::default(), 1)
            .expect("missing deck submissions should skip only the incomplete pairing");

        assert!(spawns.is_empty());
        assert!(game_mgr.sessions.is_empty());
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
        let restored = DraftSession::from_persisted(persisted);

        assert!(restored.connected.iter().all(|&c| !c));
        assert!(restored.timer_task.is_none());
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
        mgr2.restore_session(persisted);

        // All tokens should resolve to the same draft code
        for t in &tokens {
            if !t.is_empty() {
                assert_eq!(mgr2.draft_for_token(t), Some(code.as_str()));
            }
        }

        // Original host token should still work
        assert_eq!(mgr2.draft_for_token(&token), Some(code.as_str()));
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
    fn draft_grace_period_returns_correct_durations() {
        assert_eq!(
            draft_grace_period(&DraftStatus::Lobby),
            Duration::from_secs(1800)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::Drafting),
            Duration::from_secs(300)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::Deckbuilding),
            Duration::from_secs(900)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::MatchInProgress),
            Duration::from_secs(600)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::Complete),
            Duration::from_secs(60)
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
                card_instance_id: "abc".to_string(),
            },
        )
        .expect("seat-scoped action is allowed for any seat");
        assert_eq!(
            action,
            DraftAction::Pick {
                seat: 2,
                card_instance_id: "abc".to_string()
            }
        );
    }

    #[test]
    fn authorize_rebinds_submit_deck_seat() {
        let action = authorize_client_draft_action(
            3,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec!["x".to_string()],
            },
        )
        .expect("submit deck is allowed for any seat");
        assert_eq!(
            action,
            DraftAction::SubmitDeck {
                seat: 3,
                main_deck: vec!["x".to_string()]
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
            source: DraftSource::Set {
                code: "TST".to_string(),
            },
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
                card_instance_id: card_id,
            },
            None,
        )
        .unwrap();

        let seats = draft_seats_needing_auto_pick(&mut session, 2);
        assert_eq!(seats, vec![1]);
        assert!(!seats.contains(&0));
    }
}
