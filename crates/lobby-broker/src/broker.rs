//! The functional broker core: `Broker::handle(conn, msg, env) -> Vec<Outbound>`.
//!
//! Mirrors the engine's `apply(state, action) -> events` reducer. No I/O, no
//! locking, no tokio: the only impurity is `env` (time/rng), `&mut self` (the
//! lobby map), and `&mut conn` (this connection's lobby state). The shell
//! interprets the returned [`Outbound`]s over its transport. **Output order is
//! significant** — the shell MUST perform them in returned order.
//!
//! This is the always-P2P path: there is no `is_p2p` / `mode` branch in the
//! core. The native shell only calls into the broker for the LobbyOnly-mode
//! dispatch (and the mode-agnostic Subscribe/Ping arms, whose behavior is
//! identical across modes), so every entry the core sees is a P2P entry.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::env::BrokerEnv;
use crate::lobby::{LobbyManager, RegisterGameRequest};
use crate::protocol::{LobbyClientMessage, LobbyServerMessage, ServerMode};

/// Capacity cap for the broker path. `LobbyManager` is otherwise unbounded —
/// without this gate an abusive client could pin arbitrary entries in memory
/// until the staleness reaper fires. Mirrors `MAX_LOBBY_ENTRIES` in the
/// pre-extraction phase-server shell.
pub const MAX_LOBBY_ENTRIES: usize = 200;

/// The client's self-reported identity from `ClientHello`. `build_commit` is
/// the join-compatibility gate; `client_version` is the display-only string
/// stamped into a registered entry's `host_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHelloInfo {
    pub client_version: String,
    pub build_commit: String,
}

/// Per-connection broker state — the lobby subset of the shell's per-socket
/// identity. The core owns ALL mutation of these fields (plan §3.1 review C2);
/// the shell never writes them, it only reads them back to wire up transport
/// (e.g. mapping `subscribed` to its subscriber registry is implied by the
/// `AddSubscriber`/`RemoveSubscriber` outbounds).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnState {
    /// Client identity from `ClientHello`. `build_commit` gates joins;
    /// `client_version` is stamped into a registered entry's `host_version`.
    pub client_hello: Option<ClientHelloInfo>,
    /// Whether this connection is subscribed to the lobby feed.
    pub subscribed: bool,
    /// The game code this connection registered as host, if any (ownership
    /// stamp). Disconnect / re-registration teardown keys off this.
    pub host_game: Option<String>,
    /// `(game_code, token)` reservations this connection holds, released on
    /// disconnect or explicit release/consume.
    pub reservations: Vec<(String, String)>,
}

/// A side effect the shell must perform after a broker call. **Order within a
/// returned `Vec<Outbound>` is significant** and must be preserved.
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    /// Point reply to the originating connection.
    ToSelf(LobbyServerMessage),
    /// Fan out to all lobby subscribers.
    ToSubscribers(LobbyServerMessage),
    /// Register this connection's sender in the shell's subscriber set.
    AddSubscriber,
    /// Deregister (prune closed senders); the shell owns the mechanism.
    RemoveSubscriber,
    /// Send a `PlayerCount` to this connection. The shell owns the count
    /// (AtomicU32 natively / `getWebSockets().length` in a DO), so the core
    /// cannot fill the value — it just asks the shell to emit it.
    SendPlayerCountToSelf,
}

/// Result of the build-commit compatibility check. `pub` so the native shell's
/// inline join/lookup arms reuse this single authority rather than duplicating it.
#[derive(Debug, PartialEq, Eq)]
pub enum BuildCommitCheck {
    Allow,
    Reject { host: String, guest: String },
}

/// Host and guest commits must either both be populated and equal, or at least
/// one must be empty (restored session / legacy client) for a join to proceed.
pub fn check_build_commit(host_commit: &str, guest_commit: &str) -> BuildCommitCheck {
    if !guest_commit.is_empty() && !host_commit.is_empty() && host_commit != guest_commit {
        BuildCommitCheck::Reject {
            host: host_commit.to_owned(),
            guest: guest_commit.to_owned(),
        }
    } else {
        BuildCommitCheck::Allow
    }
}

/// The matchmaking broker. Wraps the pure [`LobbyManager`]; all broker dispatch
/// rules (ownership, subscription, reservation, gating) live here, written once.
///
/// `Serialize`/`Deserialize` exist for the Cloudflare Durable Object shell,
/// which snapshots the whole broker to DO storage after each mutating message
/// (a hibernated DO loses in-memory state). The native phase-server shell keeps
/// it in an `Arc<Mutex>` and never serializes it. The snapshot format is an
/// internal implementation detail, versioned by the broker code — not a wire
/// contract; the shell falls back to `Broker::new()` if a snapshot fails to load.
#[derive(Serialize, Deserialize)]
pub struct Broker {
    lobby: LobbyManager,
}

impl Broker {
    pub fn new() -> Self {
        Self {
            lobby: LobbyManager::new(),
        }
    }

    /// Borrow the underlying registry. The shell uses this for the non-broker
    /// lobby operations it still owns (Full-mode lobby listing for server-run
    /// games, draft registration) and for the staleness reaper's Full-mode
    /// session/db deletion, which needs the expired codes.
    pub fn lobby(&self) -> &LobbyManager {
        &self.lobby
    }

    /// Mutable access to the underlying registry for the shell's non-broker
    /// lobby operations (see [`Broker::lobby`]).
    pub fn lobby_mut(&mut self) -> &mut LobbyManager {
        &mut self.lobby
    }

    /// Single entry for client frames. Returns the ordered side effects the
    /// shell must perform. The error helper keeps the many reject paths terse.
    pub fn handle(
        &mut self,
        conn: &mut ConnState,
        msg: LobbyClientMessage,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        match msg {
            LobbyClientMessage::ClientHello {
                client_version,
                build_commit,
                protocol_version: _,
            } => {
                // The handshake gate (protocol-version check, first-frame
                // enforcement) stays in the shell; by the time a ClientHello
                // reaches the broker it has been accepted. Record the commit so
                // join gates can compare it. No outbound.
                info!(version = %client_version, commit = %build_commit, "ClientHello accepted");
                conn.client_hello = Some(ClientHelloInfo {
                    client_version,
                    build_commit,
                });
                vec![]
            }

            LobbyClientMessage::SubscribeLobby => {
                debug!("lobby subscription");
                conn.subscribed = true;
                let games = self.lobby.public_games();
                debug!(games = games.len(), "sending lobby state");
                vec![
                    Outbound::AddSubscriber,
                    Outbound::ToSelf(LobbyServerMessage::LobbyUpdate { games }),
                    Outbound::SendPlayerCountToSelf,
                ]
            }

            LobbyClientMessage::UnsubscribeLobby => {
                debug!("lobby unsubscribe");
                conn.subscribed = false;
                vec![Outbound::RemoveSubscriber]
            }

            LobbyClientMessage::CreateGameWithSettings {
                deck: _,
                display_name,
                public,
                password,
                timer_seconds,
                player_count: requested_player_count,
                match_config,
                format_config,
                room_name,
                host_peer_id,
                draft_metadata,
                start_when_full: _,
            } => self.handle_create_game(
                conn,
                display_name,
                public,
                password,
                timer_seconds,
                requested_player_count,
                match_config,
                format_config,
                room_name,
                host_peer_id,
                draft_metadata,
                env,
            ),

            LobbyClientMessage::JoinGameWithPassword {
                game_code,
                deck: _,
                display_name,
                password,
                reservation_token,
            } => self.handle_join(conn, game_code, display_name, password, reservation_token),

            LobbyClientMessage::LookupJoinTarget {
                game_code,
                password,
                reserve,
                display_name,
                release_reservation_token,
            } => self.handle_lookup(
                conn,
                game_code,
                password,
                reserve,
                display_name,
                release_reservation_token,
                env,
            ),

            LobbyClientMessage::Ping { timestamp } => {
                vec![Outbound::ToSelf(LobbyServerMessage::Pong { timestamp })]
            }

            LobbyClientMessage::UpdateLobbyMetadata {
                game_code,
                current_players,
                max_players,
                consumed_reservation_tokens,
            } => self.handle_update_metadata(
                conn,
                game_code,
                current_players,
                max_players,
                consumed_reservation_tokens,
                env,
            ),

            LobbyClientMessage::UnregisterLobby { game_code } => {
                self.handle_unregister(conn, game_code)
            }
        }
    }

    /// Socket-close teardown. Emits, in order: for each held reservation a
    /// `LobbyGameUpdated` (released seat frees capacity); then, if this conn
    /// owned a host entry, a `LobbyGameRemoved`. Does NOT emit player-count —
    /// that decrement+broadcast is shell-owned (unconditional on close).
    pub fn on_disconnect(&mut self, conn: &mut ConnState) -> Vec<Outbound> {
        let mut out = Vec::new();

        if !conn.reservations.is_empty() {
            let reservations = std::mem::take(&mut conn.reservations);
            let changed = self.lobby.release_reservations(&reservations);
            if changed {
                for (game_code, _) in &reservations {
                    if let Some(game) = self.lobby.public_game(game_code) {
                        out.push(Outbound::ToSubscribers(
                            LobbyServerMessage::LobbyGameUpdated { game },
                        ));
                    }
                }
            }
        }

        if let Some(game_code) = conn.host_game.take() {
            let existed = self.lobby.has_game(&game_code);
            self.lobby.unregister_game(&game_code);
            if existed {
                info!(game = %game_code, "lobby host disconnected — lobby entry removed");
                out.push(Outbound::ToSubscribers(
                    LobbyServerMessage::LobbyGameRemoved { game_code },
                ));
            }
        }

        // Subscriber pruning on close is shell-owned (it drops the closed
        // sender). The core only signals it if the conn was subscribed.
        if conn.subscribed {
            conn.subscribed = false;
            out.push(Outbound::RemoveSubscriber);
        }

        out
    }

    /// Reaper for a tokio interval (native) or DO alarm (WASM). Returns a
    /// `LobbyGameRemoved` per reaped game. The Full-mode session/db deletion
    /// stays in the shell — it pulls the expired codes from
    /// [`Broker::lobby_mut`]`.check_expired` directly.
    pub fn reap_expired(&mut self, timeout_secs: u64, env: &impl BrokerEnv) -> Vec<Outbound> {
        self.lobby
            .check_expired(timeout_secs, env)
            .into_iter()
            .map(|game_code| {
                Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved { game_code })
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_create_game(
        &mut self,
        conn: &mut ConnState,
        display_name: String,
        public: bool,
        password: Option<String>,
        timer_seconds: Option<u32>,
        requested_player_count: u8,
        match_config: engine::types::match_config::MatchConfig,
        format_config: Option<engine::types::format::FormatConfig>,
        room_name: Option<String>,
        host_peer_id: Option<String>,
        draft_metadata: Option<crate::protocol::DraftLobbyMetadata>,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        let peer_id = match host_peer_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(id) => id.to_string(),
            None => {
                warn!("lobby-only CreateGameWithSettings missing host_peer_id");
                return vec![error("host_peer_id is required on lobby-only servers")];
            }
        };

        if conn.client_hello.is_none() {
            return vec![error("ClientHello required before any other message")];
        }

        let mut out = Vec::new();

        // Re-registration cleanup: drop a previously-owned entry first so a
        // double CreateGameWithSettings doesn't orphan the first. Emits
        // LobbyGameRemoved BEFORE the new LobbyGameAdded (order-significant).
        if let Some(previous) = conn.host_game.take() {
            let existed = self.lobby.has_game(&previous);
            self.lobby.unregister_game(&previous);
            if existed {
                info!(game = %previous, "replacing previous lobby entry from same socket");
                out.push(Outbound::ToSubscribers(
                    LobbyServerMessage::LobbyGameRemoved {
                        game_code: previous,
                    },
                ));
            }
        }

        if self.lobby.len() >= MAX_LOBBY_ENTRIES {
            warn!(
                entries = self.lobby.len(),
                limit = MAX_LOBBY_ENTRIES,
                "lobby full, rejecting CreateGameWithSettings"
            );
            out.push(error("Server lobby is full, please try again shortly"));
            return out;
        }

        let game_code = env.new_game_code();
        let player_token = env.new_token();
        let pc = requested_player_count.clamp(2, 6);
        let (host_version, host_build_commit) = conn
            .client_hello
            .as_ref()
            .map(|h| (h.client_version.clone(), h.build_commit.clone()))
            .unwrap_or_default();

        self.lobby.register_game(
            &game_code,
            RegisterGameRequest {
                host_name: display_name.clone(),
                public,
                password,
                timer_seconds,
                host_version,
                host_build_commit,
                current_players: 1,
                max_players: pc as u32,
                format_config,
                match_config,
                room_name: room_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                host_peer_id: peer_id,
                draft_metadata,
            },
            env,
        );

        conn.host_game = Some(game_code.clone());

        out.push(Outbound::ToSelf(LobbyServerMessage::GameCreated {
            game_code: game_code.clone(),
            player_token,
        }));

        if public {
            if let Some(game) = self.lobby.public_game(&game_code) {
                out.push(Outbound::ToSubscribers(
                    LobbyServerMessage::LobbyGameAdded { game },
                ));
            }
        }

        info!(game = %game_code, host = %display_name, "lobby-only game registered");
        out
    }

    fn handle_join(
        &mut self,
        conn: &mut ConnState,
        game_code: String,
        display_name: String,
        password: Option<String>,
        reservation_token: Option<String>,
    ) -> Vec<Outbound> {
        if conn
            .host_game
            .as_deref()
            .is_some_and(|owned| owned == game_code)
        {
            return vec![error("You are already hosting this game")];
        }

        let guest_commit = conn
            .client_hello
            .as_ref()
            .map(|h| h.build_commit.as_str())
            .unwrap_or("");
        let host_commit = self.lobby.host_build_commit(&game_code).unwrap_or("");
        if let BuildCommitCheck::Reject { host, guest } =
            check_build_commit(host_commit, guest_commit)
        {
            warn!(game = %game_code, %host, %guest, "build mismatch — refusing join (lobby-only)");
            return vec![error(&format!(
                "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
            ))];
        }

        match self.lobby.verify_password(&game_code, password.as_deref()) {
            Ok(()) => {}
            Err(e) if e == "password_required" => {
                return vec![Outbound::ToSelf(LobbyServerMessage::PasswordRequired {
                    game_code,
                })];
            }
            Err(e) => {
                warn!(game = %game_code, error = %e, "password verification failed (lobby-only)");
                return vec![error(&e)];
            }
        }

        let info = match self.lobby.join_target_info(&game_code) {
            Some(info) => info,
            None => return vec![error(&format!("Game not found in lobby: {game_code}"))],
        };
        if !info.is_p2p {
            return vec![error(&format!(
                "Game {game_code} is hosted on a Full-mode server and cannot be brokered"
            ))];
        }

        let consumed_reservation_token = if let Some(token) = reservation_token.as_deref() {
            if self.lobby.consume_reservation(&game_code, token) {
                conn.reservations
                    .retain(|(code, t)| code != &game_code || t != token);
                reservation_token
            } else {
                None
            }
        } else {
            None
        };

        if info.max_players > 0
            && info.current_players >= info.max_players
            && consumed_reservation_token.is_none()
        {
            return vec![error(&format!("Game {game_code} is full"))];
        }

        info!(game = %game_code, joiner = %display_name, "sent PeerInfo to guest");
        vec![Outbound::ToSelf(LobbyServerMessage::PeerInfo {
            game_code,
            host_peer_id: info.host_peer_id,
            format_config: info.format_config,
            match_config: info.match_config,
            player_count: info.max_players as u8,
            filled_seats: info.current_players as u8,
            reservation_token: consumed_reservation_token,
        })]
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_lookup(
        &mut self,
        conn: &mut ConnState,
        game_code: String,
        password: Option<String>,
        reserve: bool,
        display_name: Option<String>,
        release_reservation_token: Option<String>,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        let mut out = Vec::new();
        let mut reservation_token = None;
        let mut reservation_expires_at_ms = None;
        let mut reservation_counted_in_info = false;

        if conn
            .host_game
            .as_deref()
            .is_some_and(|owned| owned == game_code)
        {
            return vec![error("You are already hosting this game")];
        }

        // --- build-commit + password gates, then snapshot ---
        let guest_commit = conn
            .client_hello
            .as_ref()
            .map(|h| h.build_commit.as_str())
            .unwrap_or("");
        let host_commit = self.lobby.host_build_commit(&game_code).unwrap_or("");
        if let BuildCommitCheck::Reject { host, guest } =
            check_build_commit(host_commit, guest_commit)
        {
            warn!(game = %game_code, %host, %guest, "build mismatch — refusing lookup");
            return vec![error(&format!(
                "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
            ))];
        }

        let mut info = match self.lobby.verify_password(&game_code, password.as_deref()) {
            Ok(()) => match self.lobby.join_target_info(&game_code) {
                Some(info) => info,
                None => return vec![error(&format!("Game not found in lobby: {game_code}"))],
            },
            Err(e) if e == "password_required" => {
                return vec![Outbound::ToSelf(LobbyServerMessage::PasswordRequired {
                    game_code,
                })];
            }
            Err(e) => {
                warn!(game = %game_code, error = %e, "lookup password verification failed");
                return vec![error(&e)];
            }
        };

        // --- optional reservation release ---
        if let Some(token) = release_reservation_token.as_deref() {
            // Always the P2P path here (core is always-P2P).
            if self.lobby.release_reservation(&game_code, token) {
                conn.reservations
                    .retain(|(code, t)| code != &game_code || t != token);
                if let Some(game) = self.lobby.public_game(&game_code) {
                    out.push(Outbound::ToSubscribers(
                        LobbyServerMessage::LobbyGameUpdated { game },
                    ));
                }
            }
        }

        // --- optional reservation ---
        if reserve {
            let mut has_active_reservation = false;
            conn.reservations.retain(|(code, token)| {
                if code != &game_code {
                    return true;
                }
                let active = self.lobby.has_active_reservation(code, token, env);
                has_active_reservation |= active;
                active
            });
            if has_active_reservation {
                out.push(error("You already hold a reservation for this game"));
                return out;
            }
            match self.lobby.reserve_seat(
                &game_code,
                display_name.unwrap_or_else(|| "Player".to_string()),
                env,
            ) {
                Ok(reservation) => {
                    reservation_token = Some(reservation.token.clone());
                    reservation_expires_at_ms = reservation.expires_at_ms;
                    conn.reservations
                        .push((game_code.clone(), reservation.token));
                    if let Some(game) = self.lobby.public_game(&game_code) {
                        out.push(Outbound::ToSubscribers(
                            LobbyServerMessage::LobbyGameUpdated { game },
                        ));
                    }
                }
                Err(e) => {
                    out.push(error(&e));
                    return out;
                }
            }
            if let Some(latest_info) = self.lobby.join_target_info(&game_code) {
                info = latest_info;
                reservation_counted_in_info = true;
            }
        } else if info.max_players > 0 && info.current_players >= info.max_players {
            // --- seat-full short-circuit ---
            out.push(error(&format!("Game {game_code} is full")));
            return out;
        }

        let filled_seats = (info.current_players
            + u32::from(reservation_token.is_some() && !reservation_counted_in_info))
        .min(info.max_players) as u8;
        out.push(Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo {
            game_code: game_code.clone(),
            is_p2p: info.is_p2p,
            format_config: info.format_config,
            match_config: info.match_config,
            player_count: info.max_players as u8,
            filled_seats,
            reservation_token,
            reservation_expires_at_ms,
        }));
        info!(game = %game_code, is_p2p = info.is_p2p, "sent JoinTargetInfo");
        out
    }

    fn handle_update_metadata(
        &mut self,
        conn: &mut ConnState,
        game_code: String,
        current_players: u8,
        max_players: u8,
        consumed_reservation_tokens: Vec<String>,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        let is_owner = conn.host_game.as_deref().is_some_and(|g| g == game_code);
        if !is_owner {
            return vec![error("Only the lobby host can update metadata")];
        }

        let mut consumed_reservation = false;
        for token in &consumed_reservation_tokens {
            consumed_reservation |= self.lobby.consume_reservation(&game_code, token);
        }

        let max_players = max_players.max(1);
        let floor = if consumed_reservation {
            self.lobby.seated_player_count(&game_code).unwrap_or(0)
        } else {
            0
        };
        let target = (current_players as u32).max(floor).min(max_players as u32);
        self.lobby.set_current_players(&game_code, target, env);
        self.lobby.set_max_players(&game_code, max_players);
        match self.lobby.public_game(&game_code) {
            Some(game) => vec![Outbound::ToSubscribers(
                LobbyServerMessage::LobbyGameUpdated { game },
            )],
            None => vec![],
        }
    }

    fn handle_unregister(&mut self, conn: &mut ConnState, game_code: String) -> Vec<Outbound> {
        let is_owner = conn.host_game.as_deref().is_some_and(|g| g == game_code);
        if !is_owner {
            warn!(game = %game_code, "UnregisterLobby rejected — socket is not the registered host");
            return vec![error(
                "UnregisterLobby only allowed for the host that registered the game",
            )];
        }

        let existed = self.lobby.has_game(&game_code);
        self.lobby.unregister_game(&game_code);
        // Clear so disconnect cleanup doesn't try to unregister again.
        conn.host_game = None;
        if existed {
            info!(game = %game_code, "lobby entry removed by host (UnregisterLobby)");
            vec![Outbound::ToSubscribers(
                LobbyServerMessage::LobbyGameRemoved { game_code },
            )]
        } else {
            vec![]
        }
    }
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

/// Construct the `ServerHello` greeting frame. Lives in the broker so the
/// greeting wire shape has a single owner shared by both shells; the shell
/// supplies its own version/commit/mode.
pub fn server_hello(
    server_version: String,
    build_commit: String,
    protocol_version: u32,
    mode: ServerMode,
) -> LobbyServerMessage {
    LobbyServerMessage::ServerHello {
        server_version,
        build_commit,
        protocol_version,
        mode,
    }
}

fn error(message: &str) -> Outbound {
    Outbound::ToSelf(LobbyServerMessage::Error {
        message: message.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::LobbyClientMessage;
    use std::cell::Cell;

    /// Deterministic env: monotonic codes/tokens so sequence assertions are
    /// stable; settable clock for reservation-expiry behavior.
    struct FakeEnv {
        now: Cell<u64>,
        token: Cell<u64>,
        code: Cell<u64>,
    }
    impl FakeEnv {
        fn new() -> Self {
            Self {
                now: Cell::new(1_000_000),
                token: Cell::new(0),
                code: Cell::new(0),
            }
        }
    }
    impl BrokerEnv for FakeEnv {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn new_token(&self) -> String {
            let n = self.token.get();
            self.token.set(n + 1);
            format!("token-{n}")
        }
        fn new_game_code(&self) -> String {
            let n = self.code.get();
            self.code.set(n + 1);
            format!("CODE{n:02}")
        }
    }

    /// The broker ignores deck contents (decks are host-validated over P2P), so
    /// tests use an empty deck. `DeckData` has no `Default`, so build it inline.
    fn test_deck() -> engine::starter_decks::DeckData {
        engine::starter_decks::DeckData {
            main_deck: vec![],
            sideboard: vec![],
            commander: vec![],
            bracket_tier: Default::default(),
        }
    }

    fn hello(conn: &mut ConnState, broker: &mut Broker, env: &FakeEnv) {
        broker.handle(
            conn,
            LobbyClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: "abc".into(),
                protocol_version: 7,
            },
            env,
        );
    }

    fn create(conn: &mut ConnState, broker: &mut Broker, env: &FakeEnv) -> Vec<Outbound> {
        broker.handle(
            conn,
            LobbyClientMessage::CreateGameWithSettings {
                deck: test_deck(),
                display_name: "Host".into(),
                public: true,
                password: None,
                timer_seconds: None,
                player_count: 4,
                match_config: Default::default(),
                format_config: None,
                room_name: None,
                host_peer_id: Some("peer-1".into()),
                draft_metadata: None,
                start_when_full: true,
            },
            env,
        )
    }

    fn game_code_of(out: &[Outbound]) -> String {
        out.iter()
            .find_map(|o| match o {
                Outbound::ToSelf(LobbyServerMessage::GameCreated { game_code, .. }) => {
                    Some(game_code.clone())
                }
                _ => None,
            })
            .expect("GameCreated present")
    }

    #[test]
    fn create_emits_game_created_then_lobby_game_added() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let out = create(&mut conn, &mut broker, &env);
        // GameCreated (point reply) precedes the public LobbyGameAdded fan-out.
        assert!(matches!(
            out[0],
            Outbound::ToSelf(LobbyServerMessage::GameCreated { .. })
        ));
        assert!(matches!(
            out[1],
            Outbound::ToSubscribers(LobbyServerMessage::LobbyGameAdded { .. })
        ));
        assert_eq!(out.len(), 2);
        assert_eq!(conn.host_game.as_deref(), Some("CODE00"));
    }

    #[test]
    fn re_registration_emits_removed_before_added() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let first = create(&mut conn, &mut broker, &env);
        let first_code = game_code_of(&first);

        // Second CreateGameWithSettings from the SAME conn.
        let out = create(&mut conn, &mut broker, &env);

        // Order-significant: the old entry's Removed must precede the new
        // entry's GameCreated + Added.
        assert_eq!(
            out[0],
            Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved {
                game_code: first_code.clone(),
            }),
            "re-registration must broadcast LobbyGameRemoved first"
        );
        assert!(
            matches!(
                out[1],
                Outbound::ToSelf(LobbyServerMessage::GameCreated { .. })
            ),
            "GameCreated follows the removal"
        );
        assert!(
            matches!(
                out[2],
                Outbound::ToSubscribers(LobbyServerMessage::LobbyGameAdded { .. })
            ),
            "LobbyGameAdded is last"
        );
        // The new entry replaced the old ownership stamp.
        assert_ne!(conn.host_game.as_deref(), Some(first_code.as_str()));
    }

    #[test]
    fn subscribe_emits_add_then_update_then_count() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let out = broker.handle(&mut conn, LobbyClientMessage::SubscribeLobby, &env);
        assert_eq!(out[0], Outbound::AddSubscriber);
        assert!(matches!(
            out[1],
            Outbound::ToSelf(LobbyServerMessage::LobbyUpdate { .. })
        ));
        assert_eq!(out[2], Outbound::SendPlayerCountToSelf);
        assert_eq!(out.len(), 3);
        assert!(conn.subscribed);
    }

    #[test]
    fn update_metadata_cannot_reset_players_after_consuming_reservations() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let mut guest = ConnState::default();
        hello(&mut guest, &mut broker, &env);
        let reserve_out = broker.handle(
            &mut guest,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Guest".into()),
                release_reservation_token: None,
            },
            &env,
        );
        let token = reserve_out
            .iter()
            .find_map(|o| match o {
                Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo {
                    reservation_token, ..
                }) => reservation_token.clone(),
                _ => None,
            })
            .expect("reservation token");

        broker.handle(
            &mut host,
            LobbyClientMessage::UpdateLobbyMetadata {
                game_code: code.clone(),
                current_players: 0,
                max_players: 4,
                consumed_reservation_tokens: vec![token],
            },
            &env,
        );

        let info = broker.lobby().join_target_info(&code).expect("game exists");
        assert_eq!(info.current_players, 2);
    }

    #[test]
    fn update_metadata_with_stale_consumed_reservation_can_lower_players() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        broker.handle(
            &mut host,
            LobbyClientMessage::UpdateLobbyMetadata {
                game_code: code.clone(),
                current_players: 2,
                max_players: 4,
                consumed_reservation_tokens: vec![],
            },
            &env,
        );
        broker.handle(
            &mut host,
            LobbyClientMessage::UpdateLobbyMetadata {
                game_code: code.clone(),
                current_players: 1,
                max_players: 4,
                consumed_reservation_tokens: vec!["stale-token".into()],
            },
            &env,
        );

        let info = broker.lobby().join_target_info(&code).expect("game exists");
        assert_eq!(info.current_players, 1);
    }

    #[test]
    fn second_reserve_on_same_game_from_same_conn_is_rejected() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let mut guest = ConnState::default();
        hello(&mut guest, &mut broker, &env);

        let first = broker.handle(
            &mut guest,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Squatter".into()),
                release_reservation_token: None,
            },
            &env,
        );
        assert!(matches!(
            first.last(),
            Some(Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo { .. }))
        ));

        let second = broker.handle(
            &mut guest,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Squatter".into()),
                release_reservation_token: None,
            },
            &env,
        );
        assert!(matches!(
            second.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        assert_eq!(guest.reservations.len(), 1);
    }

    #[test]
    fn expired_reservation_on_conn_does_not_block_new_reserve() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let mut guest = ConnState::default();
        hello(&mut guest, &mut broker, &env);

        let first = broker.handle(
            &mut guest,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Guest".into()),
                release_reservation_token: None,
            },
            &env,
        );
        assert!(matches!(
            first.last(),
            Some(Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo { .. }))
        ));
        assert_eq!(guest.reservations.len(), 1);

        env.now
            .set(env.now.get() + crate::lobby::PUBLIC_SEAT_RESERVATION_MS + 1);

        let second = broker.handle(
            &mut guest,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Guest".into()),
                release_reservation_token: None,
            },
            &env,
        );
        assert!(matches!(
            second.last(),
            Some(Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo { .. }))
        ));
        assert_eq!(guest.reservations.len(), 1);
    }

    #[test]
    fn on_disconnect_emits_reservation_updates_then_host_removed() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        // Host conn registers a game.
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        // Guest conn reserves a seat (via LookupJoinTarget reserve=true).
        let mut guest = ConnState::default();
        hello(&mut guest, &mut broker, &env);
        let _ = broker.handle(
            &mut guest,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Guest".into()),
                release_reservation_token: None,
            },
            &env,
        );
        assert_eq!(guest.reservations.len(), 1);

        // Guest disconnects with a held reservation → LobbyGameUpdated.
        let guest_out = broker.on_disconnect(&mut guest);
        assert_eq!(
            guest_out
                .iter()
                .filter(|o| matches!(
                    o,
                    Outbound::ToSubscribers(LobbyServerMessage::LobbyGameUpdated { .. })
                ))
                .count(),
            1,
            "released reservation broadcasts one LobbyGameUpdated"
        );
        assert!(guest.reservations.is_empty());

        // Host disconnects → LobbyGameRemoved for its owned entry.
        let host_out = broker.on_disconnect(&mut host);
        assert!(
            host_out.contains(&Outbound::ToSubscribers(
                LobbyServerMessage::LobbyGameRemoved {
                    game_code: code.clone(),
                }
            )),
            "host disconnect removes its lobby entry"
        );
        assert!(host.host_game.is_none());
    }

    #[test]
    fn on_disconnect_orders_reservation_updates_before_host_removed() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        // A single conn that BOTH hosts a game AND holds a reservation on
        // another game — verifies the per-reservation Updated precedes the
        // host Removed in a single teardown.
        let mut other_host = ConnState::default();
        hello(&mut other_host, &mut broker, &env);
        let other = create(&mut other_host, &mut broker, &env);
        let other_code = game_code_of(&other);

        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let mine = create(&mut conn, &mut broker, &env);
        let my_code = game_code_of(&mine);
        // Reserve a seat on the OTHER host's game.
        let _ = broker.handle(
            &mut conn,
            LobbyClientMessage::LookupJoinTarget {
                game_code: other_code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Me".into()),
                release_reservation_token: None,
            },
            &env,
        );

        let out = broker.on_disconnect(&mut conn);
        // First: the reservation-release LobbyGameUpdated(s).
        assert!(
            matches!(
                out[0],
                Outbound::ToSubscribers(LobbyServerMessage::LobbyGameUpdated { .. })
            ),
            "reservation updates come first"
        );
        // Then: the host LobbyGameRemoved.
        assert!(
            out.iter().any(|o| o
                == &Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved {
                    game_code: my_code.clone(),
                })),
            "host removed after reservation updates"
        );
        let updated_pos = out
            .iter()
            .position(|o| {
                matches!(
                    o,
                    Outbound::ToSubscribers(LobbyServerMessage::LobbyGameUpdated { .. })
                )
            })
            .unwrap();
        let removed_pos = out
            .iter()
            .position(|o| {
                matches!(
                    o,
                    Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved { .. })
                )
            })
            .unwrap();
        assert!(updated_pos < removed_pos, "Updated must precede Removed");
    }

    #[test]
    fn create_without_client_hello_is_rejected() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let out = create(&mut conn, &mut broker, &env);
        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        assert!(conn.host_game.is_none());
    }

    #[test]
    fn create_without_peer_id_is_rejected() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::CreateGameWithSettings {
                deck: test_deck(),
                display_name: "Host".into(),
                public: true,
                password: None,
                timer_seconds: None,
                player_count: 4,
                match_config: Default::default(),
                format_config: None,
                room_name: None,
                host_peer_id: None,
                draft_metadata: None,
                start_when_full: true,
            },
            &env,
        );
        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        assert!(conn.host_game.is_none());
    }

    #[test]
    fn unregister_by_non_owner_is_rejected() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let mut other = ConnState::default();
        let out = broker.handle(
            &mut other,
            LobbyClientMessage::UnregisterLobby {
                game_code: code.clone(),
            },
            &env,
        );
        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        // Entry survives.
        assert!(broker.lobby().has_game(&code));
    }

    #[test]
    fn join_returns_peer_info_after_gates() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let mut guest = ConnState::default();
        hello(&mut guest, &mut broker, &env);
        let out = broker.handle(
            &mut guest,
            LobbyClientMessage::JoinGameWithPassword {
                game_code: code.clone(),
                deck: test_deck(),
                display_name: "Guest".into(),
                password: None,
                reservation_token: None,
            },
            &env,
        );
        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::PeerInfo { .. })]
        ));
    }

    #[test]
    fn host_cannot_join_own_game() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let out = broker.handle(
            &mut host,
            LobbyClientMessage::JoinGameWithPassword {
                game_code: code,
                deck: test_deck(),
                display_name: "Host".into(),
                password: None,
                reservation_token: None,
            },
            &env,
        );

        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
    }

    #[test]
    fn host_cannot_lookup_own_game() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let out = broker.handle(
            &mut host,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code,
                password: None,
                reserve: false,
                display_name: Some("Host".into()),
                release_reservation_token: None,
            },
            &env,
        );

        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
    }

    #[test]
    fn ping_returns_pong() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let out = broker.handle(&mut conn, LobbyClientMessage::Ping { timestamp: 7 }, &env);
        assert_eq!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Pong { timestamp: 7 })]
        );
    }
}
