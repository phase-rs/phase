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
use crate::protocol::{
    LobbyClientMessage, LobbyServerMessage, ServerMode, TournamentRequestId, TournamentView,
};
use crate::reservation_auth::{
    consume_owned_reservation, release_owned_reservation, ReservationConsume, ReservationRelease,
    NOT_OWNED_RESERVATION,
};
use crate::tournament::{
    BracketShape, CreateTournamentRequest, MatchArity, PairingId, PodOutcome, ScoringPolicy,
    TournamentExpiryEvent, TournamentManager,
};

/// Capacity cap for the broker path. `LobbyManager` is otherwise unbounded —
/// without this gate an abusive client could pin arbitrary entries in memory
/// until the staleness reaper fires. Mirrors `MAX_LOBBY_ENTRIES` in the
/// pre-extraction phase-server shell.
pub const MAX_LOBBY_ENTRIES: usize = 200;

/// Capacity cap for the tournament registry — the same gate
/// [`MAX_LOBBY_ENTRIES`] applies to the lobby, for the other unbounded map the
/// broker owns. Without it a client can mint tournament records without limit:
/// a `Registration` record survives until the staleness reaper fires (300s),
/// and an `InProgress`/`Completed` one persists for its own much longer
/// retention window, while every successful creation also broadcasts a
/// `TournamentListUpdate` carrying *every* stored tournament — so both
/// resident memory and per-creation fan-out grow unbounded together.
///
/// A separate constant rather than a reuse of the lobby's, because the two
/// registries have genuinely different per-entry profiles — a tournament
/// carries its entrant list, pairing history and standings, and lives orders
/// of magnitude longer than a lobby row — and must be able to move
/// independently as either grows. The initial value matches the lobby's own
/// choice: far above any plausible count of concurrent events on one broker,
/// and still a hard bound.
pub const MAX_TOURNAMENT_ENTRIES: usize = 200;

/// Cap on the per-connection tournament bookkeeping lists
/// ([`ConnState::organized_tournaments`] and [`ConnState::joined_tournaments`]).
///
/// Those lists are appended to on every successful create/join and are
/// deliberately never pruned — not on disconnect, drop, expiry or completion —
/// because they are reconnect convenience that must outlive a socket. Never
/// pruned plus never bounded is unbounded per-connection growth, which
/// [`MAX_TOURNAMENT_ENTRIES`] does not cover: that bounds the registry, while
/// a client can join and re-join across many events on one long-lived socket.
///
/// 50 is far past any real "your events" list for a single connection while
/// keeping the worst case per socket trivial. See [`push_conn_tournament`] for
/// why reaching the cap evicts rather than refuses.
pub const MAX_CONN_TOURNAMENT_ENTRIES: usize = 50;

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
    /// Tournament codes this connection created, appended on every successful
    /// `CreateTournament`.
    ///
    /// **Not an authority.** Organizer permission is the `organizer_token`
    /// compared against `TournamentMeta::organizer_token`, never this list —
    /// which is why nothing in the broker reads it. It exists as local
    /// reconnect convenience for a future "your events" client flow, and is
    /// deliberately NOT cleared by [`Broker::on_disconnect`]: closing a socket
    /// must not cost an organizer their event, unlike `host_game`/
    /// `reservations`, which are socket-bound by design.
    ///
    /// Never pruned, but bounded: appends go through
    /// [`push_conn_tournament`], which holds the list at
    /// [`MAX_CONN_TOURNAMENT_ENTRIES`] most-recent codes.
    #[serde(default)]
    pub organized_tournaments: Vec<String>,
    /// Tournament codes this connection joined, appended on every successful
    /// `JoinTournament`.
    ///
    /// **Codes only — deliberately never the `player_token`.** Player
    /// permission is the token compared against the entrant record, never this
    /// list, so retaining the token here bought nothing and cost a great deal:
    /// the list is never pruned and is copied verbatim into the native shell's
    /// per-socket identity and into durable Durable Object attachments, which
    /// turned a non-authority convenience record into monotonic secret
    /// retention in storage that outlives the socket. A code carries no such
    /// risk — it is already broadcast to every subscriber in each
    /// `TournamentListUpdate`.
    ///
    /// Same non-authority framing and retention as
    /// [`Self::organized_tournaments`]: dropping from an event does not remove
    /// the record that this connection was in it, and the list is bounded to
    /// the [`MAX_CONN_TOURNAMENT_ENTRIES`] most-recent joins rather than
    /// growing without limit.
    #[serde(default)]
    pub joined_tournaments: Vec<String>,
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

/// Whether a gated tournament action moved any [`crate::protocol::TournamentSummary`]
/// field, and therefore whether the list broadcast is warranted.
///
/// A typed axis rather than a `bool`: the distinction is a real property of the
/// action — only a result report leaves every summary field untouched — and
/// until now it survived only as a comment each handler had to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListRowEffect {
    /// At least one summary field moved, so subscribers need the list row.
    Changed,
    /// Every summary field is untouched; broadcasting the list would be a
    /// no-op re-render for every subscriber.
    Unchanged,
}

/// What a successful gated tournament action produced. Handlers describe the
/// outcome; they never assemble outbounds themselves.
///
/// This is the type that makes an uncorrelated exit a **compile error**: the
/// four gated handlers return `Result<GatedEffect, String>`, so neither a
/// success tail nor a refusal can reach the shell without passing through
/// [`Broker::settle_gated`], which is the only place either is minted.
#[derive(Debug, Clone, PartialEq)]
struct GatedEffect {
    code: String,
    view: TournamentView,
    list_row: ListRowEffect,
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
    /// The tournament registry, alongside — not inside — [`Self::lobby`]: a
    /// tournament outlives any single lobby entry and has its own lifecycle
    /// and expiry rules ([`TournamentManager::check_expired`]).
    ///
    /// `#[serde(default)]` so a Durable Object snapshot taken before this
    /// field existed still restores its lobby entries instead of failing the
    /// whole parse and falling back to an empty broker (see
    /// [`WasmBroker::from_snapshot`]'s reset behavior in `broker-wasm`).
    #[serde(default)]
    tournaments: TournamentManager,
}

impl Broker {
    pub fn new() -> Self {
        Self {
            lobby: LobbyManager::new(),
            tournaments: TournamentManager::new(),
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

    /// Borrow the tournament registry, mirroring [`Broker::lobby`]. For shell
    /// operations the broker does not own (a Full-mode listing endpoint, a
    /// `/stats` gauge); dispatch itself never goes through this.
    pub fn tournaments(&self) -> &TournamentManager {
        &self.tournaments
    }

    /// Mutable access to the tournament registry, mirroring
    /// [`Broker::lobby_mut`] (see [`Broker::tournaments`]).
    pub fn tournaments_mut(&mut self) -> &mut TournamentManager {
        &mut self.tournaments
    }

    /// `true` when neither the lobby nor the tournament registry holds any
    /// entries.
    ///
    /// The sole authority for whether a shell may stop rescheduling its
    /// cleanup alarm/reaper — checking only one registry here is exactly the
    /// bug that let a tournament-only Durable Object stop its alarm while the
    /// tournament was still live and un-reaped: the last sweep to run landed
    /// inside the registration window, so the record was never removed and the
    /// seat it holds in the bounded registry
    /// ([`MAX_TOURNAMENT_ENTRIES`]) never came back.
    ///
    /// It lives here rather than in the WASM shim because "is this broker
    /// durably empty" is registry knowledge: the predicate must stay in
    /// lockstep with [`Broker::reap_expired`], which sweeps BOTH registries,
    /// so every registry that sweep can empty must also be able to keep the
    /// alarm alive. A boundary layer that recomputed this would drift the
    /// moment a third registry is added.
    pub fn is_empty(&self) -> bool {
        self.lobby.is_empty() && self.tournaments.is_empty()
    }

    /// Every tournament as a wire summary, ordered by code.
    ///
    /// [`TournamentManager::iter`] promises no order (it wraps
    /// `HashMap::values`), so the sort is this caller's job — and it must
    /// happen, because a `TournamentListUpdate` whose row order shuffled on
    /// every broadcast would make clients re-render the whole list for no
    /// state change. Sorting by `code` rather than `created_at` keeps the
    /// order total: codes are unique, creation timestamps are second-grained
    /// and collide freely.
    fn tournament_summaries(&self) -> Vec<crate::protocol::TournamentSummary> {
        let mut summaries: Vec<crate::protocol::TournamentSummary> = self
            .tournaments
            .iter()
            .map(crate::protocol::TournamentSummary::from)
            .collect();
        summaries.sort_by(|a, b| a.code.cmp(&b.code));
        summaries
    }

    /// The `TournamentListUpdate` broadcast that follows any list-affecting
    /// change. Assembled in one place so no call site can broadcast a list
    /// built a different way.
    fn tournament_list_update(&self) -> Outbound {
        Outbound::ToSubscribers(LobbyServerMessage::TournamentListUpdate {
            tournaments: self.tournament_summaries(),
        })
    }

    /// The correlated settlement for one gated tournament action, and the
    /// single authority for it. Success and refusal are two halves of one
    /// signal, so both are minted here and nowhere else: the four gated
    /// handlers hand back a [`GatedEffect`] or a reason and never build an
    /// outbound themselves.
    ///
    /// `request_id: None` reproduces the pre-correlation behavior **exactly** —
    /// the same `Vec<Outbound>`, in the same order, that these handlers
    /// returned before the correlator existed. A client that does not mint one
    /// (any build older than lobby protocol 5) is unaffected by this change.
    ///
    /// **Order within the returned `Vec` is significant** (see [`Outbound`]).
    /// On success this emits `[ack?, ToSubscribers(TournamentUpdate),
    /// list_update?]` — the `ToSelf` point reply ahead of the broadcast, which
    /// is the convention [`Broker::handle_join_tournament`] already follows for
    /// its own `TournamentJoined` + `TournamentUpdate` pair.
    ///
    /// The ack carries no token: the caller already holds the one that
    /// authorized the action, and the correlator identifies a *request*, never
    /// a *requester* — it must never be read as permission.
    fn settle_gated(
        &self,
        request_id: Option<TournamentRequestId>,
        outcome: Result<GatedEffect, String>,
    ) -> Vec<Outbound> {
        let GatedEffect {
            code,
            view,
            list_row,
        } = match outcome {
            Ok(effect) => effect,
            Err(reason) => return vec![settle_rejection(request_id, &reason)],
        };

        let mut out = Vec::new();
        if let Some(request_id) = request_id {
            out.push(Outbound::ToSelf(LobbyServerMessage::TournamentActionAck {
                request_id,
                code: code.clone(),
                view: view.clone(),
            }));
        }
        out.push(Outbound::ToSubscribers(
            LobbyServerMessage::TournamentUpdate { code, view },
        ));
        // Exhaustive rather than an `if`: a third effect added later must force
        // a decision here instead of silently taking the "no list row" path.
        match list_row {
            ListRowEffect::Changed => out.push(self.tournament_list_update()),
            ListRowEffect::Unchanged => {}
        }
        out
    }

    /// The detail view for `code`, or `None` if the tournament is gone.
    fn tournament_view(&self, code: &str) -> Option<crate::protocol::TournamentView> {
        self.tournaments
            .get(code)
            .map(crate::protocol::TournamentView::from)
    }

    /// Single entry for client frames. Returns the ordered side effects the
    /// shell must perform. The error helper keeps the many reject paths terse.
    pub fn handle(
        &mut self,
        conn: &mut ConnState,
        msg: LobbyClientMessage,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        // The correlator is read BEFORE the guard, so a gated frame refused at
        // the bounds check is refused *to that request* rather than by a bare
        // `Error` a correlated caller is designed to ignore — which would be a
        // hang until timeout. Exhaustive by construction on the message enum,
        // so a future gated variant that forgets its correlator is visible at
        // `LobbyClientMessage::tournament_request_id` rather than silently
        // uncorrelated here.
        let request_id = msg.tournament_request_id();
        if let Err(reason) = crate::inbound_guard::guard_inbound(&msg) {
            return vec![settle_rejection(request_id, &reason)];
        }
        match msg {
            LobbyClientMessage::ClientHello {
                client_version,
                build_commit,
                protocol_version: _,
                lobby_protocol_version: _,
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
                // The tournament list rides the same initial push as the game
                // list, so a freshly-subscribed client has both without a
                // second round-trip. Emitted unconditionally — an empty list
                // is a meaningful answer ("no events"), and omitting it would
                // leave a client unable to distinguish that from "not sent
                // yet". Order is significant and asserted by
                // `subscribe_emits_add_then_update_then_tournaments_then_count`.
                let tournaments = self.tournament_summaries();
                debug!(tournaments = tournaments.len(), "sending tournament state");
                vec![
                    Outbound::AddSubscriber,
                    Outbound::ToSelf(LobbyServerMessage::LobbyUpdate { games }),
                    Outbound::ToSelf(LobbyServerMessage::TournamentListUpdate { tournaments }),
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
                ranked,
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
                ranked,
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

            LobbyClientMessage::CreateTournament {
                name,
                arity,
                scoring,
                bracket,
                total_rounds,
            } => self.handle_create_tournament(
                conn,
                name,
                arity,
                scoring,
                bracket,
                total_rounds,
                env,
            ),

            LobbyClientMessage::JoinTournament {
                code,
                player_key,
                display_name,
            } => self.handle_join_tournament(conn, code, player_key, display_name, env),

            LobbyClientMessage::GetTournament { code } => self.handle_get_tournament(code),

            // The four gated actions destructure their correlator by name and
            // route the handler's outcome through [`Broker::settle_gated`], the
            // one place a gated success or refusal is minted. The handlers
            // return `Result<GatedEffect, String>` precisely so that no arm
            // here can answer a gated frame any other way. The binding shadows
            // the `request_id` read above the guard with the same value — one
            // read, one settlement.
            LobbyClientMessage::StartTournamentRound {
                code,
                organizer_token,
                request_id,
            } => {
                let outcome = self.handle_start_tournament_round(code, organizer_token, env);
                self.settle_gated(request_id, outcome)
            }

            LobbyClientMessage::ReportMatchResult {
                code,
                pairing_id,
                player_token,
                outcome: reported,
                request_id,
            } => {
                let outcome =
                    self.handle_report_match_result(code, pairing_id, player_token, reported, env);
                self.settle_gated(request_id, outcome)
            }

            LobbyClientMessage::DropFromTournament {
                code,
                player_token,
                request_id,
            } => {
                let outcome = self.handle_drop_from_tournament(code, player_token, env);
                self.settle_gated(request_id, outcome)
            }

            LobbyClientMessage::EndTournament {
                code,
                organizer_token,
                request_id,
            } => {
                let outcome = self.handle_end_tournament(code, organizer_token, env);
                self.settle_gated(request_id, outcome)
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

    /// Reaper for a tokio interval (native) or DO alarm (WASM). Sweeps BOTH
    /// registries in one call and returns their outbounds in one vector: a
    /// `LobbyGameRemoved` per reaped game, then the tournament lifecycle
    /// events, then at most one trailing `TournamentListUpdate`.
    ///
    /// The Full-mode session/db deletion stays in the shell — it pulls the
    /// expired codes from [`Broker::lobby_mut`]`.check_expired` directly, and
    /// tournaments have no equivalent server-run session to clean up.
    ///
    /// `timeout_secs` applies to lobby entries only. Tournament expiry runs on
    /// the three fixed lifecycle clocks
    /// ([`crate::tournament::REGISTRATION_TIMEOUT_SECS`] and siblings) that
    /// [`TournamentManager::check_expired`] owns, which is why it takes no
    /// timeout argument: a registration window, a 7-day abandonment and a
    /// 30-day retention are not the same duration as a lobby listing's, and
    /// threading one number through both would imply they are.
    pub fn reap_expired(&mut self, timeout_secs: u64, env: &impl BrokerEnv) -> Vec<Outbound> {
        let mut out: Vec<Outbound> = self
            .lobby
            .check_expired(timeout_secs, env)
            .into_iter()
            .map(|game_code| {
                Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved { game_code })
            })
            .collect();

        let events = self.tournaments.check_expired(env);
        let list_changed = !events.is_empty();
        for event in events {
            match event {
                // The record is gone, so there is no view left to send — the
                // code alone is the whole message.
                TournamentExpiryEvent::Deleted(code) => {
                    info!(tournament = %code, "tournament expired — record removed");
                    out.push(Outbound::ToSubscribers(
                        LobbyServerMessage::TournamentRemoved { code },
                    ));
                }
                // The record IS preserved (that is the point of `Abandoned`),
                // so subscribers get its updated view rather than a removal.
                TournamentExpiryEvent::Abandoned(code) => {
                    info!(tournament = %code, "tournament inactive — marked abandoned");
                    match self.tournament_view(&code) {
                        Some(view) => out.push(Outbound::ToSubscribers(
                            LobbyServerMessage::TournamentUpdate { code, view },
                        )),
                        // `check_expired` retains everything it abandons, so
                        // this is unreachable. Degrade to a removal rather
                        // than panic in a reaper: a broadcast a client
                        // over-prunes is recoverable, a shell that aborts its
                        // sweep loop is not.
                        None => {
                            warn!(tournament = %code, "abandoned tournament vanished mid-sweep");
                            out.push(Outbound::ToSubscribers(
                                LobbyServerMessage::TournamentRemoved { code },
                            ));
                        }
                    }
                }
            }
        }

        // Exactly ONE trailing list update per sweep, not one per expired
        // tournament: N simultaneous expiries change the list once, and N
        // identical broadcasts would just make every client re-render N times.
        if list_changed {
            out.push(self.tournament_list_update());
        }

        out
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
        ranked: bool,
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
                ranked,
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
            match consume_owned_reservation(
                &mut self.lobby,
                &mut conn.reservations,
                &game_code,
                token,
            ) {
                ReservationConsume::Consumed => reservation_token,
                ReservationConsume::NotHeld => {
                    return vec![error(NOT_OWNED_RESERVATION)];
                }
                ReservationConsume::NotFound => {
                    return vec![error("Seat reservation expired or was released")];
                }
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
            match release_owned_reservation(
                &mut self.lobby,
                &mut conn.reservations,
                &game_code,
                token,
            ) {
                ReservationRelease::Released => {
                    if let Some(game) = self.lobby.public_game(&game_code) {
                        out.push(Outbound::ToSubscribers(
                            LobbyServerMessage::LobbyGameUpdated { game },
                        ));
                    }
                }
                ReservationRelease::NotHeld => {
                    out.push(error(NOT_OWNED_RESERVATION));
                    return out;
                }
                ReservationRelease::NotFound => {}
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

    // -- Tournament organizer ----------------------------------------------
    //
    // Every gated helper below follows one shape: resolve the tournament,
    // check the presented token against the STORED one, call the single
    // `TournamentManager` method that does the real work, assemble outbounds.
    // The manager owns every rules decision (status gating, duplicate joins,
    // result legality, round ceilings); these helpers add authority and wire
    // assembly and nothing else, and they propagate the manager's own `Err`
    // string verbatim so a client sees why it was refused rather than a
    // generic broker message.

    /// Is `presented` the organizer token for `code`?
    ///
    /// `Err` carries the message to reply with, distinguishing "no such
    /// tournament" from "wrong token" for the caller while keeping both a
    /// single early return at the call site.
    fn authorize_organizer(&self, code: &str, presented: &str) -> Result<(), String> {
        let meta = self
            .tournaments
            .get(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        // An empty presented token must never authorize anything. Stored
        // tokens come from `BrokerEnv::new_token` and are never empty, so this
        // is belt-and-braces against a future env rather than a live hole —
        // but an authority check is the wrong place to rely on that.
        if presented.is_empty() || meta.organizer_token != presented {
            return Err(format!("Invalid organizer token for tournament {code}"));
        }
        Ok(())
    }

    /// The `player_key` owning `presented` in `code`, if any.
    ///
    /// Resolves the token to a player rather than merely testing it, because
    /// every player-gated action needs to know *which* entrant is acting —
    /// "some valid token exists" is exactly the check that would let player A
    /// drop player B.
    ///
    /// A dropped entrant is refused here, at the single authority, rather than
    /// per action. A drop is permanent in this engine — nothing ever clears
    /// [`crate::tournament::TournamentPlayer::dropped`], and
    /// [`crate::tournament::TournamentManager::drop_player`] settles the
    /// pairings it can on the way out — so a dropped player's token must stop
    /// authorizing *anything*, not merely the one action each downstream gate
    /// happened to think of. The narrower per-action checks are not
    /// sufficient: [`crate::tournament::validate_match_result`] rejects a
    /// dropped *winner*, but a dropped player reporting a still-active
    /// opponent as the winner — or a draw — passes it, and in a pod
    /// (`arity > 2`) with two or more active seats left the pairing is still
    /// `Pending` after the drop precisely so the remaining players can play it
    /// out, so there is a real, reachable window in which a dropped seat could
    /// settle a match it is no longer in.
    ///
    /// Three distinct `Err` shapes — missing tournament, unusable token,
    /// dropped entrant — because they are three different client-side
    /// situations and the caller replies with the message verbatim. The
    /// dropped case reveals nothing: it is told only to the holder of that
    /// player's own token.
    fn authorize_player(&self, code: &str, presented: &str) -> Result<String, String> {
        let meta = self
            .tournaments
            .get(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        if presented.is_empty() {
            return Err(format!("Invalid player token for tournament {code}"));
        }
        let player = meta
            .players
            .iter()
            .find(|p| p.player_token == presented)
            .ok_or_else(|| format!("Invalid player token for tournament {code}"))?;
        if player.dropped {
            return Err(format!("Player has dropped from tournament {code}"));
        }
        Ok(player.player_key.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_create_tournament(
        &mut self,
        conn: &mut ConnState,
        name: String,
        arity: MatchArity,
        scoring: ScoringPolicy,
        bracket: BracketShape,
        total_rounds: Option<u32>,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        // Registry capacity, checked before a code or a token is minted and
        // before the manager is asked to store anything — the same shape and
        // the same point in the flow as `handle_create_game`'s
        // `MAX_LOBBY_ENTRIES` gate.
        if self.tournaments.len() >= MAX_TOURNAMENT_ENTRIES {
            warn!(
                entries = self.tournaments.len(),
                limit = MAX_TOURNAMENT_ENTRIES,
                "tournament registry full, rejecting CreateTournament"
            );
            return vec![error(
                "Server tournament registry is full, please try again shortly",
            )];
        }

        // Broker-minted, exactly like a lobby game code:
        // `TournamentManager::create_tournament` takes a caller-supplied code
        // for the same reason `LobbyManager::register_game` does.
        let code = env.new_game_code();
        let organizer_token = match self.tournaments.create_tournament(
            &code,
            CreateTournamentRequest {
                name: name.clone(),
                arity,
                scoring,
                bracket,
                total_rounds,
            },
            env,
        ) {
            Ok(token) => token,
            Err(reason) => return vec![error(&reason)],
        };

        let Some(view) = self.tournament_view(&code) else {
            // Unreachable: `create_tournament` just returned `Ok`, so the
            // record exists.
            return vec![error("Tournament was created but could not be read back")];
        };

        push_conn_tournament(&mut conn.organized_tournaments, code.clone());
        info!(tournament = %code, name = %name, "tournament created");

        // No `TournamentUpdate` broadcast: nobody else holds this code yet, so
        // there is no detail view anyone could be watching. The list row is
        // the only thing that changed for subscribers.
        vec![
            Outbound::ToSelf(LobbyServerMessage::TournamentCreated {
                code,
                organizer_token,
                view,
            }),
            self.tournament_list_update(),
        ]
    }

    fn handle_join_tournament(
        &mut self,
        conn: &mut ConnState,
        code: String,
        player_key: String,
        display_name: String,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        let player_token =
            match self
                .tournaments
                .join_tournament(&code, &player_key, &display_name, env)
            {
                Ok(token) => token,
                Err(reason) => return vec![error(&reason)],
            };

        let Some(view) = self.tournament_view(&code) else {
            return vec![error("Tournament was joined but could not be read back")];
        };

        // The code alone: `player_token` is returned to this caller below and
        // never retained here (see `ConnState::joined_tournaments`).
        push_conn_tournament(&mut conn.joined_tournaments, code.clone());
        info!(tournament = %code, player = %player_key, "tournament joined");

        // A join changes both the detail view (a new entrant) and the list
        // row (`player_count`), so both broadcasts are warranted.
        vec![
            Outbound::ToSelf(LobbyServerMessage::TournamentJoined {
                code: code.clone(),
                player_token,
                view: view.clone(),
            }),
            Outbound::ToSubscribers(LobbyServerMessage::TournamentUpdate { code, view }),
            self.tournament_list_update(),
        ]
    }

    /// Read-only. Ungated: a tournament is public once its code is known,
    /// exactly like a lobby listing, and the view carries no token.
    fn handle_get_tournament(&mut self, code: String) -> Vec<Outbound> {
        match self.tournament_view(&code) {
            Some(view) => vec![Outbound::ToSelf(LobbyServerMessage::TournamentUpdate {
                code,
                view,
            })],
            None => vec![error(&format!("Tournament not found: {code}"))],
        }
    }

    /// Organizer-gated. Describes its outcome and leaves the outbounds to
    /// [`Broker::settle_gated`]; every refusal is an `Err` the type system
    /// forces through that one settlement.
    fn handle_start_tournament_round(
        &mut self,
        code: String,
        organizer_token: String,
        env: &impl BrokerEnv,
    ) -> Result<GatedEffect, String> {
        if let Err(reason) = self.authorize_organizer(&code, &organizer_token) {
            warn!(tournament = %code, "StartTournamentRound rejected — bad organizer token");
            return Err(reason);
        }
        self.tournaments.generate_pairings(&code, env)?;
        let Some(view) = self.tournament_view(&code) else {
            return Err(format!("Tournament not found: {code}"));
        };
        info!(tournament = %code, "tournament round paired");

        // Status may flip to `InProgress` and `current_round` advances — both
        // are list-row fields, so the list update is warranted here.
        Ok(GatedEffect {
            code,
            view,
            list_row: ListRowEffect::Changed,
        })
    }

    /// Player-gated, and the one gated action whose list row does not move —
    /// see [`ListRowEffect::Unchanged`] on the success tail below.
    fn handle_report_match_result(
        &mut self,
        code: String,
        pairing_id: PairingId,
        player_token: String,
        outcome: PodOutcome,
        env: &impl BrokerEnv,
    ) -> Result<GatedEffect, String> {
        let reporter = match self.authorize_player(&code, &player_token) {
            Ok(key) => key,
            Err(reason) => {
                // `reason` distinguishes an unusable token from an entrant who
                // has dropped, so it is logged rather than flattened into one
                // "bad token" message that would misreport the second case.
                warn!(tournament = %code, %reason, "ReportMatchResult rejected — player not authorized");
                return Err(reason);
            }
        };

        // Being *an* entrant is not enough: the reporter must be seated in
        // THIS pairing. Without this, any valid token in the event could
        // report any other pairing's result — two real tokens exist in that
        // fixture and only one is valid for this specific action.
        let seated = self
            .tournaments
            .get(&code)
            .and_then(|meta| meta.pairing(pairing_id))
            .map(|pairing| pairing.players.contains(&reporter));
        match seated {
            Some(true) => {}
            Some(false) => {
                warn!(tournament = %code, pairing = pairing_id, "ReportMatchResult rejected — reporter not seated in this pairing");
                return Err(format!(
                    "Player {reporter} is not seated in pairing {pairing_id}"
                ));
            }
            None => return Err(format!("Pairing {pairing_id} not found in {code}")),
        }

        self.tournaments
            .report_result(&code, pairing_id, outcome, env)?;
        let Some(view) = self.tournament_view(&code) else {
            return Err(format!("Tournament not found: {code}"));
        };
        info!(tournament = %code, pairing = pairing_id, "match result reported");

        // The one helper that does NOT pair its update with a list update:
        // a mid-event result changes standings and a pairing outcome, both of
        // which live only in the detail view. Status, round, and active-player
        // count — every field a `TournamentSummary` carries — are untouched,
        // so broadcasting the whole list here would be a no-op re-render for
        // every subscriber. That reasoning is now the justification for a typed
        // field rather than for an unwritten convention.
        Ok(GatedEffect {
            code,
            view,
            list_row: ListRowEffect::Unchanged,
        })
    }

    /// Player-gated.
    fn handle_drop_from_tournament(
        &mut self,
        code: String,
        player_token: String,
        env: &impl BrokerEnv,
    ) -> Result<GatedEffect, String> {
        // Resolving the token to its owner is what confines a drop to the
        // player who presented it: the key is never taken from the payload,
        // so there is no field a client could point at someone else.
        //
        // A second drop on an already-dropped token is refused by that same
        // gate, and deliberately so. It is not an idempotent no-op: `dropped`
        // is already `true`, so re-running [`TournamentManager::drop_player`]
        // changes no player state and can settle no further pairing (the
        // dropped set is unchanged), while still bumping `last_activity_at` —
        // i.e. its only observable effect is to push back the staleness reaper
        // for an event this caller has left. Refusing is both the honest
        // answer ("you are not a participant") and the one that does not hand
        // a departed entrant a liveness lever.
        let player_key = match self.authorize_player(&code, &player_token) {
            Ok(key) => key,
            Err(reason) => {
                warn!(tournament = %code, %reason, "DropFromTournament rejected — player not authorized");
                return Err(reason);
            }
        };
        self.tournaments.drop_player(&code, &player_key, env)?;
        let Some(view) = self.tournament_view(&code) else {
            return Err(format!("Tournament not found: {code}"));
        };
        info!(tournament = %code, player = %player_key, "player dropped from tournament");

        // A drop lowers `active_player_count`, which IS a summary field, so
        // the list row genuinely changed here (unlike a result report).
        Ok(GatedEffect {
            code,
            view,
            list_row: ListRowEffect::Changed,
        })
    }

    /// Organizer-gated.
    fn handle_end_tournament(
        &mut self,
        code: String,
        organizer_token: String,
        env: &impl BrokerEnv,
    ) -> Result<GatedEffect, String> {
        if let Err(reason) = self.authorize_organizer(&code, &organizer_token) {
            warn!(tournament = %code, "EndTournament rejected — bad organizer token");
            return Err(reason);
        }
        self.tournaments.complete_tournament(&code, env)?;
        let Some(view) = self.tournament_view(&code) else {
            return Err(format!("Tournament not found: {code}"));
        };
        info!(tournament = %code, "tournament completed");

        // Status → `Completed`, a summary field.
        Ok(GatedEffect {
            code,
            view,
            list_row: ListRowEffect::Changed,
        })
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
        // Always advertised by this build. `protocol_version` above stays on
        // the full-game constant for clients that predate the lobby-owned one.
        lobby_protocol_version: Some(crate::protocol::LOBBY_PROTOCOL_VERSION),
    }
}

fn error(message: &str) -> Outbound {
    Outbound::ToSelf(LobbyServerMessage::error(message))
}

/// The refusal for one gated tournament action: a `TournamentActionRejected`
/// carrying the caller's own correlator when it sent one, and the bare `Error`
/// this broker has always sent when it did not.
///
/// A free function rather than a [`Broker::settle_gated`] branch alone because
/// the inbound bounds guard refuses *before* dispatch — before any handler
/// could have produced a `Result<GatedEffect, String>` — and both refusals must
/// be shaped the same way. `settle_gated`'s `Err` arm delegates here, so a
/// gated refusal has exactly one construction site.
///
/// [`error`] itself is deliberately untouched: every non-tournament path keeps
/// today's exact bytes.
fn settle_rejection(request_id: Option<TournamentRequestId>, message: &str) -> Outbound {
    match request_id {
        Some(request_id) => Outbound::ToSelf(LobbyServerMessage::TournamentActionRejected {
            request_id,
            message: message.to_string(),
        }),
        None => error(message),
    }
}

/// Append to a per-connection tournament bookkeeping list, evicting oldest-first
/// to stay within [`MAX_CONN_TOURNAMENT_ENTRIES`].
///
/// Evicts rather than refuses, and never fails the create/join it accompanies.
/// These lists are explicitly *not* an authority — organizer permission is the
/// `organizer_token`, player permission the `player_token`, and nothing in the
/// broker reads either list — so failing a real operation because a
/// convenience record has nowhere to go would trade a correct outcome for a
/// bookkeeping one. Refusing only the *append* was the other option, but that
/// freezes the list on its oldest entries forever, which is exactly backwards
/// for a "your recent events" aid: recency is what makes it useful, so the
/// oldest record is the right one to lose.
///
/// The loop (rather than a single `remove`) also repairs an over-cap list
/// deserialized from a snapshot written before this bound existed.
fn push_conn_tournament<T>(list: &mut Vec<T>, entry: T) {
    while list.len() >= MAX_CONN_TOURNAMENT_ENTRIES {
        list.remove(0);
    }
    list.push(entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LobbyClientMessage, PROTOCOL_VERSION};
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
        /// Move the clock forward. Used by the expiry sweeps, which are the
        /// only broker behavior that depends on elapsed time.
        fn advance_secs(&self, secs: u64) {
            self.now.set(self.now.get() + secs * 1000);
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
            ..Default::default()
        }
    }

    fn hello(conn: &mut ConnState, broker: &mut Broker, env: &FakeEnv) {
        broker.handle(
            conn,
            LobbyClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: "abc".into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(crate::protocol::LOBBY_PROTOCOL_VERSION),
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
                ranked: false,
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
    fn rejected_limited_range_re_registration_preserves_the_existing_lobby() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let first = create(&mut conn, &mut broker, &env);
        let first_code = game_code_of(&first);
        let mut format_config = engine::types::format::FormatConfig::standard();
        format_config.range_of_influence =
            Some(Box::new(engine::types::format::RangeOfInfluenceConfig {
                default_range: 0,
                player_overrides: std::collections::BTreeMap::new(),
            }));

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
                format_config: Some(format_config),
                room_name: None,
                host_peer_id: Some("peer-1".into()),
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
            },
            &env,
        );

        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        assert_eq!(conn.host_game.as_deref(), Some(first_code.as_str()));
        assert!(broker.lobby_mut().public_game(&first_code).is_some());
    }

    #[test]
    fn subscribe_emits_add_then_update_then_tournaments_then_count() {
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
        // Emitted even though no tournament exists: an empty list is the
        // answer "no events", which a client cannot otherwise distinguish
        // from "not sent yet".
        match &out[2] {
            Outbound::ToSelf(LobbyServerMessage::TournamentListUpdate { tournaments }) => {
                assert!(tournaments.is_empty());
            }
            other => panic!("expected an empty TournamentListUpdate, got {other:?}"),
        }
        assert_eq!(out[3], Outbound::SendPlayerCountToSelf);
        assert_eq!(out.len(), 4);
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
                ranked: false,
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
    fn create_rejects_archenemy_seat_outside_player_count() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let mut format_config = engine::types::format::FormatConfig::archenemy();
        format_config.archenemy_player = Some(engine::types::player::PlayerId(4));

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
                format_config: Some(format_config),
                room_name: None,
                host_peer_id: Some("peer-host".into()),
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
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
    fn handle_rejects_oversized_display_name_without_parse() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::CreateGameWithSettings {
                deck: test_deck(),
                display_name: "a".repeat(21),
                public: true,
                password: None,
                timer_seconds: None,
                player_count: 4,
                match_config: Default::default(),
                format_config: None,
                room_name: None,
                host_peer_id: Some("peer-host".into()),
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
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
    fn foreign_release_reservation_token_is_rejected() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let mut guest_a = ConnState::default();
        hello(&mut guest_a, &mut broker, &env);
        let reserve_out = broker.handle(
            &mut guest_a,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Guest A".into()),
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

        let mut guest_b = ConnState::default();
        hello(&mut guest_b, &mut broker, &env);
        let release_out = broker.handle(
            &mut guest_b,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: false,
                display_name: Some("Guest B".into()),
                release_reservation_token: Some(token),
            },
            &env,
        );
        assert!(matches!(
            release_out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        assert!(guest_a.reservations.len() == 1);
        assert!(broker
            .lobby()
            .join_target_info(&code)
            .is_some_and(|info| info.current_players >= 2));
    }

    #[test]
    fn foreign_join_with_reservation_token_is_rejected() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let code = game_code_of(&created);

        let mut guest_a = ConnState::default();
        hello(&mut guest_a, &mut broker, &env);
        let reserve_out = broker.handle(
            &mut guest_a,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.clone(),
                password: None,
                reserve: true,
                display_name: Some("Guest A".into()),
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

        let mut guest_b = ConnState::default();
        hello(&mut guest_b, &mut broker, &env);
        let join_out = broker.handle(
            &mut guest_b,
            LobbyClientMessage::JoinGameWithPassword {
                game_code: code.clone(),
                deck: test_deck(),
                display_name: "Guest B".into(),
                password: None,
                reservation_token: Some(token),
            },
            &env,
        );
        assert!(matches!(
            join_out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        assert!(guest_a.reservations.len() == 1);
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

    // ======================================================================
    // Tournament organizer
    //
    // Every test below drives a real `Broker::handle` on a real `ConnState`,
    // not a bare `TournamentManager` call — the seam under test is the
    // dispatch layer's authority and outbound assembly, which a manager-level
    // test cannot reach.
    // ======================================================================

    use crate::protocol::{TournamentSummary, TournamentView};
    use crate::tournament::{
        BracketShape, MatchArity, PairingOutcome, PodOutcome, ScoringPolicy, TournamentStatus,
        IN_PROGRESS_ABANDON_SECS, REGISTRATION_TIMEOUT_SECS,
    };

    /// Creates a tournament through the real dispatch path and returns
    /// `(code, organizer_token)`.
    fn make_tournament(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
        bracket: BracketShape,
    ) -> (String, String) {
        let out = broker.handle(
            conn,
            LobbyClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: ScoringPolicy::default(),
                bracket,
                total_rounds: None,
            },
            env,
        );
        match out.first() {
            Some(Outbound::ToSelf(LobbyServerMessage::TournamentCreated {
                code,
                organizer_token,
                ..
            })) => (code.clone(), organizer_token.clone()),
            other => panic!("expected TournamentCreated, got {other:?}"),
        }
    }

    /// Joins through the real dispatch path and returns the minted
    /// `player_token`.
    fn join_tournament(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
        code: &str,
        player_key: &str,
        display_name: &str,
    ) -> String {
        let out = broker.handle(
            conn,
            LobbyClientMessage::JoinTournament {
                code: code.to_string(),
                player_key: player_key.to_string(),
                display_name: display_name.to_string(),
            },
            env,
        );
        match out.first() {
            Some(Outbound::ToSelf(LobbyServerMessage::TournamentJoined {
                player_token, ..
            })) => player_token.clone(),
            other => panic!("expected TournamentJoined, got {other:?}"),
        }
    }

    fn error_reason(out: &[Outbound]) -> &str {
        match out {
            [Outbound::ToSelf(LobbyServerMessage::Error { message, .. })] => message.as_str(),
            other => panic!("expected a single Error outbound, got {other:?}"),
        }
    }

    fn is_error(out: &[Outbound]) -> bool {
        matches!(out, [Outbound::ToSelf(LobbyServerMessage::Error { .. })])
    }

    /// Serialize every outbound and scan the bytes for a secret. Catches a
    /// leak wherever it is nested, rather than trusting a field-by-field walk
    /// to have visited every carrier.
    ///
    /// The match is deliberately exhaustive with no wildcard arm. Every
    /// [`Outbound`] variant must be classified explicitly as either "carries a
    /// payload, scan it" or "structurally cannot carry a secret, skip" — a
    /// `_ =>` arm would let a future variant that DOES carry a payload fall
    /// straight through every leak test in this module while they all stayed
    /// green. Adding a variant must instead break this compile and force the
    /// decision to be made.
    fn outbounds_contain(out: &[Outbound], needle: &str) -> bool {
        out.iter().any(|ob| {
            let msg = match ob {
                // Payload-carrying: these are the only variants that can hold
                // a token, and both must be scanned — `ToSelf` because a leak
                // test may be asserting the token is absent from a point reply
                // that is not its rightful recipient, `ToSubscribers` because
                // it is the broadcast path.
                Outbound::ToSelf(msg) | Outbound::ToSubscribers(msg) => msg,
                // Payload-free signals: fieldless unit variants that instruct
                // the shell to manipulate its own subscriber set or emit a
                // count it owns. They carry no data from the core at all, so
                // there is nothing here to leak.
                Outbound::AddSubscriber
                | Outbound::RemoveSubscriber
                | Outbound::SendPlayerCountToSelf => return false,
            };
            serde_json::to_string(msg)
                .expect("outbound serializes")
                .contains(needle)
        })
    }

    fn subscriber_msgs(out: &[Outbound]) -> Vec<&LobbyServerMessage> {
        out.iter()
            .filter_map(|ob| match ob {
                Outbound::ToSubscribers(msg) => Some(msg),
                _ => None,
            })
            .collect()
    }

    fn list_update(out: &[Outbound]) -> Vec<TournamentSummary> {
        out.iter()
            .find_map(|ob| match ob {
                Outbound::ToSubscribers(LobbyServerMessage::TournamentListUpdate {
                    tournaments,
                }) => Some(tournaments.clone()),
                _ => None,
            })
            .expect("a TournamentListUpdate is present")
    }

    fn view_of(out: &[Outbound]) -> TournamentView {
        out.iter()
            .find_map(|ob| match ob {
                Outbound::ToSelf(LobbyServerMessage::TournamentUpdate { view, .. })
                | Outbound::ToSubscribers(LobbyServerMessage::TournamentUpdate { view, .. }) => {
                    Some(view.clone())
                }
                _ => None,
            })
            .expect("a TournamentUpdate is present")
    }

    /// Runs a full head-to-head event up to "round 1 paired, one pairing
    /// pending", returning `(code, organizer_token, token_a, token_b)`.
    fn started_event(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
    ) -> (String, String, String, String) {
        let (code, organizer_token) = make_tournament(conn, broker, env, BracketShape::Swiss);
        let token_a = join_tournament(conn, broker, env, &code, "key-a", "Alice");
        let token_b = join_tournament(conn, broker, env, &code, "key-b", "Bob");
        let out = broker.handle(
            conn,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: None,
            },
            env,
        );
        assert!(!is_error(&out), "round 1 must pair: {out:?}");
        (code, organizer_token, token_a, token_b)
    }

    // -- Row 1: organizer token reaches only its creator --------------------

    #[test]
    fn create_tournament_returns_organizer_token_only_to_creator() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: ScoringPolicy::default(),
                bracket: BracketShape::Swiss,
                total_rounds: None,
            },
            &env,
        );

        let (code, organizer_token) = match &out[0] {
            Outbound::ToSelf(LobbyServerMessage::TournamentCreated {
                code,
                organizer_token,
                view,
            }) => {
                // The point reply's own view must not restate the token.
                let view_json = serde_json::to_string(view).expect("view serializes");
                assert!(!view_json.contains(organizer_token.as_str()));
                (code.clone(), organizer_token.clone())
            }
            other => panic!("expected TournamentCreated first, got {other:?}"),
        };
        assert!(!organizer_token.is_empty());

        // Structural: the token appears in NO broadcast, at any nesting depth.
        let broadcasts: Vec<Outbound> = out
            .iter()
            .filter(|ob| matches!(ob, Outbound::ToSubscribers(_)))
            .cloned()
            .collect();
        assert!(
            !outbounds_contain(&broadcasts, &organizer_token),
            "organizer_token leaked to subscribers"
        );

        // ...and the broadcast that DID go out is the list row, non-vacuously
        // carrying this tournament.
        let list = list_update(&out);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].code, code);
        assert_eq!(list[0].status, TournamentStatus::Registration);
        assert_eq!(list[0].player_count, 0);

        assert_eq!(conn.organized_tournaments, vec![code]);
    }

    /// The second connection's view of the same creation: a concurrently
    /// subscribed client receives no token field anywhere.
    #[test]
    fn a_second_connection_never_sees_another_organizers_token() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();
        let mut watcher = ConnState::default();

        let (code, organizer_token) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);

        // The watcher subscribes afterwards and asks for the event directly —
        // both of the paths by which a non-owner learns about a tournament.
        let sub = broker.handle(&mut watcher, LobbyClientMessage::SubscribeLobby, &env);
        assert!(!outbounds_contain(&sub, &organizer_token));

        let got = broker.handle(
            &mut watcher,
            LobbyClientMessage::GetTournament { code: code.clone() },
            &env,
        );
        assert!(!outbounds_contain(&got, &organizer_token));
        // Non-vacuity: the watcher really did receive this tournament.
        assert_eq!(view_of(&got).summary.code, code);
    }

    // -- Row 2: duplicate player_key ---------------------------------------

    #[test]
    fn join_tournament_rejects_duplicate_player_key() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        let (code, _) = make_tournament(&mut host, &mut broker, &env, BracketShape::Swiss);

        let mut alice = ConnState::default();
        let first = join_tournament(&mut alice, &mut broker, &env, &code, "key-a", "Alice");
        assert!(!first.is_empty());

        // A DIFFERENT connection claiming the same key is still refused — the
        // duplicate check keys on `player_key`, not on the socket.
        let mut impostor = ConnState::default();
        let out = broker.handle(
            &mut impostor,
            LobbyClientMessage::JoinTournament {
                code: code.clone(),
                player_key: "key-a".into(),
                display_name: "Not Alice".into(),
            },
            &env,
        );
        assert!(error_reason(&out).contains("already joined"));
        // No second token was minted for the same key.
        assert!(impostor.joined_tournaments.is_empty());
        assert_eq!(
            broker
                .tournaments()
                .get(&code)
                .expect("event")
                .players
                .len(),
            1
        );
    }

    #[test]
    fn join_after_registration_closes_surfaces_the_manager_error() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, ..) = started_event(&mut conn, &mut broker, &env);

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::JoinTournament {
                code: code.clone(),
                player_key: "key-late".into(),
                display_name: "Latecomer".into(),
            },
            &env,
        );
        // The manager's own message, verbatim — not a generic broker string.
        assert!(
            error_reason(&out).contains("no longer accepting entries"),
            "unexpected reason: {}",
            error_reason(&out)
        );
    }

    // -- Row 3: organizer-gated actions ------------------------------------

    #[test]
    fn organizer_gated_actions_reject_wrong_token() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, organizer_token) =
            make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        let player_token = join_tournament(&mut conn, &mut broker, &env, &code, "key-a", "Alice");
        join_tournament(&mut conn, &mut broker, &env, &code, "key-b", "Bob");

        // The multi-authority fixture: a REAL, live token for this very
        // tournament, just for the wrong authority tier.
        assert_ne!(player_token, organizer_token);
        for wrong in [player_token.as_str(), "not-a-token", ""] {
            for msg in [
                LobbyClientMessage::StartTournamentRound {
                    code: code.clone(),
                    organizer_token: wrong.to_string(),
                    request_id: None,
                },
                LobbyClientMessage::EndTournament {
                    code: code.clone(),
                    organizer_token: wrong.to_string(),
                    request_id: None,
                },
            ] {
                let out = broker.handle(&mut conn, msg, &env);
                assert!(
                    error_reason(&out).contains("Invalid organizer token"),
                    "token {wrong:?} was accepted"
                );
            }
        }
        // Nothing advanced.
        assert_eq!(
            broker.tournaments().get(&code).expect("event").status,
            TournamentStatus::Registration
        );

        // Positive control: the correct token calls through to the manager.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out));
        assert_eq!(
            broker.tournaments().get(&code).expect("event").status,
            TournamentStatus::InProgress
        );
    }

    /// The organizer token of a DIFFERENT tournament is not accepted here —
    /// proving the comparison is per-tournament, not "any known organizer".
    #[test]
    fn another_tournaments_organizer_token_is_rejected() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code_one, _) = make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        let (_, token_two) = make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::EndTournament {
                code: code_one,
                organizer_token: token_two,
                request_id: None,
            },
            &env,
        );
        assert!(error_reason(&out).contains("Invalid organizer token"));
    }

    // -- Row 4: player-gated actions ---------------------------------------

    #[test]
    fn player_gated_actions_reject_wrong_token() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, organizer_token, token_a, _token_b) =
            started_event(&mut conn, &mut broker, &env);

        for wrong in [organizer_token.as_str(), "not-a-token", ""] {
            let out = broker.handle(
                &mut conn,
                LobbyClientMessage::DropFromTournament {
                    code: code.clone(),
                    player_token: wrong.to_string(),
                    request_id: None,
                },
                &env,
            );
            assert!(
                error_reason(&out).contains("Invalid player token"),
                "token {wrong:?} was accepted for a drop"
            );
        }

        // Positive control: Alice's own token drops Alice, and nobody else.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::DropFromTournament {
                code: code.clone(),
                player_token: token_a,
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out));
        let meta = broker.tournaments().get(&code).expect("event");
        assert!(meta.player("key-a").expect("alice").dropped);
        assert!(!meta.player("key-b").expect("bob").dropped);
    }

    /// The sharp multi-authority case for reporting: two real, live player
    /// tokens exist, and a valid entrant who is NOT seated in the pairing must
    /// not be able to report it.
    #[test]
    fn a_player_cannot_report_a_pairing_they_are_not_seated_in() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        // Four entrants pair into TWO head-to-head pairings, so a real token
        // exists for a player genuinely absent from the pairing under test.
        let (code, organizer_token) =
            make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        let mut tokens = Vec::new();
        for i in 0..4 {
            tokens.push(join_tournament(
                &mut conn,
                &mut broker,
                &env,
                &code,
                &format!("key-{i}"),
                &format!("Player {i}"),
            ));
        }
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token,
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out));

        let meta = broker.tournaments().get(&code).expect("event");
        assert_eq!(meta.pairings.len(), 2, "fixture needs two pairings");
        let first = &meta.pairings[0];
        let pairing_id = first.id;
        let seated: Vec<String> = first.players.clone();
        let outsider_index = (0..4)
            .find(|i| !seated.contains(&format!("key-{i}")))
            .expect("some entrant is not in the first pairing");
        let outsider_token = tokens[outsider_index].clone();
        let seated_token = tokens[seated[0]
            .strip_prefix("key-")
            .and_then(|n| n.parse::<usize>().ok())
            .expect("key index")]
        .clone();

        // A valid token, for a real player, in this very tournament — and
        // still refused for THIS pairing.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: outsider_token,
                outcome: PodOutcome::Draw,
                request_id: None,
            },
            &env,
        );
        assert!(
            error_reason(&out).contains("not seated in pairing"),
            "unexpected reason: {}",
            error_reason(&out)
        );
        assert!(
            broker
                .tournaments()
                .get(&code)
                .expect("event")
                .pairing(pairing_id)
                .expect("pairing")
                .outcome
                .is_none(),
            "the pairing must still be pending"
        );

        // Positive control: a seated player's own token IS accepted, so the
        // rejection above is about the seat, not about reporting at all.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: seated_token,
                outcome: PodOutcome::Draw,
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out), "{out:?}");
        assert_eq!(
            broker
                .tournaments()
                .get(&code)
                .expect("event")
                .pairing(pairing_id)
                .expect("pairing")
                .outcome,
            Some(PairingOutcome::Reported(PodOutcome::Draw))
        );
    }

    /// Creates an `arity`-seat tournament through the real dispatch path.
    /// `make_tournament` is head-to-head only, and head-to-head cannot express
    /// the fixture below: a drop there leaves exactly one survivor, so the
    /// pairing auto-forfeits immediately and the hostile window never opens.
    fn make_pod_tournament(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
        arity: MatchArity,
    ) -> (String, String) {
        let out = broker.handle(
            conn,
            LobbyClientMessage::CreateTournament {
                name: "Commander Night".into(),
                arity,
                scoring: ScoringPolicy::default_for_arity(arity),
                bracket: BracketShape::Swiss,
                total_rounds: None,
            },
            env,
        );
        match out.first() {
            Some(Outbound::ToSelf(LobbyServerMessage::TournamentCreated {
                code,
                organizer_token,
                ..
            })) => (code.clone(), organizer_token.clone()),
            other => panic!("expected TournamentCreated, got {other:?}"),
        }
    }

    /// A dropped entrant's token must not report a result — the hostile case
    /// no *downstream* gate catches.
    ///
    /// Four-seat pod, one drop, three active seats left: `drop_player` awards a
    /// forfeit only when exactly one survivor remains, so the pairing is still
    /// `Pending`, and `pairing.players` still seats the dropped player, so the
    /// seated-in-this-pairing check passes. The reported outcome credits a
    /// *different, still-active* player, so `validate_match_result`'s
    /// dropped-winner rule does not fire either. `authorize_player` is the only
    /// thing standing between a departed seat and a settled match.
    #[test]
    fn a_dropped_player_cannot_report_a_pod_that_is_still_pending() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let arity = MatchArity::COMMANDER_POD;
        let (code, organizer_token) = make_pod_tournament(&mut conn, &mut broker, &env, arity);
        let tokens: Vec<String> = (0..4)
            .map(|i| {
                join_tournament(
                    &mut conn,
                    &mut broker,
                    &env,
                    &code,
                    &format!("key-{i}"),
                    &format!("Player {i}"),
                )
            })
            .collect();
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token,
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out), "round 1 must pair: {out:?}");

        let meta = broker.tournaments().get(&code).expect("event");
        assert_eq!(meta.pairings.len(), 1, "fixture needs one four-seat pod");
        assert_eq!(meta.pairings[0].players.len(), 4);
        let pairing_id = meta.pairings[0].id;

        // The drop itself is legitimate and must still work.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::DropFromTournament {
                code: code.clone(),
                player_token: tokens[0].clone(),
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out), "the drop must succeed: {out:?}");
        let meta = broker.tournaments().get(&code).expect("event");
        assert_eq!(
            meta.active_player_count(),
            3,
            "three active seats keep the pod pending"
        );
        assert!(
            meta.pairing(pairing_id).expect("pairing").outcome.is_none(),
            "the fixture is only hostile while the pod is unresolved"
        );

        // Crediting a still-active player: rejected on the reporter, not the
        // winner.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: tokens[0].clone(),
                outcome: PodOutcome::Decisive {
                    winner: "key-1".into(),
                    game_wins: std::collections::HashMap::new(),
                },
                request_id: None,
            },
            &env,
        );
        assert!(
            error_reason(&out).contains("has dropped"),
            "unexpected reason: {}",
            error_reason(&out)
        );
        // A draw names nobody at all, so it evades every winner-shaped check.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: tokens[0].clone(),
                outcome: PodOutcome::Draw,
                request_id: None,
            },
            &env,
        );
        assert!(
            error_reason(&out).contains("has dropped"),
            "unexpected reason: {}",
            error_reason(&out)
        );
        assert!(
            broker
                .tournaments()
                .get(&code)
                .expect("event")
                .pairing(pairing_id)
                .expect("pairing")
                .outcome
                .is_none(),
            "neither refusal may have settled the pod"
        );

        // Positive control: a still-active seat in the same pod reports the
        // same result and is accepted, so the refusals above are about the
        // drop and not about pod reporting.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: tokens[1].clone(),
                outcome: PodOutcome::Decisive {
                    winner: "key-1".into(),
                    game_wins: std::collections::HashMap::new(),
                },
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out), "{out:?}");
        assert_eq!(
            broker
                .tournaments()
                .get(&code)
                .expect("event")
                .pairing(pairing_id)
                .expect("pairing")
                .outcome,
            Some(PairingOutcome::Reported(PodOutcome::Decisive {
                winner: "key-1".into(),
                game_wins: std::collections::HashMap::new(),
            }))
        );
    }

    /// A second drop on an already-dropped token is refused rather than
    /// treated as an idempotent no-op.
    ///
    /// The observable stake is `last_activity_at`: `drop_player` bumps it
    /// unconditionally, so an accepted double drop would let a departed
    /// entrant keep pushing the staleness reaper back indefinitely. The clock
    /// is advanced between the two attempts so a bump would be visible.
    #[test]
    fn a_dropped_players_token_cannot_drop_again() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, _organizer_token, token_a, _token_b) =
            started_event(&mut conn, &mut broker, &env);

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::DropFromTournament {
                code: code.clone(),
                player_token: token_a.clone(),
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out), "the first drop must succeed: {out:?}");
        let after_first = broker
            .tournaments()
            .get(&code)
            .expect("event")
            .last_activity_at;

        env.advance_secs(60);
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::DropFromTournament {
                code: code.clone(),
                player_token: token_a,
                request_id: None,
            },
            &env,
        );
        assert!(
            error_reason(&out).contains("has dropped"),
            "unexpected reason: {}",
            error_reason(&out)
        );
        assert_eq!(
            broker
                .tournaments()
                .get(&code)
                .expect("event")
                .last_activity_at,
            after_first,
            "a refused drop must not renew the event's staleness clock"
        );
    }

    // -- Correlated gated settlement (V5, V10, V11, V12) ---------------------

    /// Every correlator carried by a gated point reply in `out`, ack or
    /// refusal alike.
    ///
    /// The match names both correlated variants explicitly rather than reaching
    /// for a field by serde: a third correlated reply added later must be
    /// classified here instead of going invisible to every correlation test in
    /// this module.
    fn correlators(out: &[Outbound]) -> Vec<TournamentRequestId> {
        out.iter()
            .filter_map(|ob| match ob {
                Outbound::ToSelf(LobbyServerMessage::TournamentActionAck {
                    request_id, ..
                })
                | Outbound::ToSelf(LobbyServerMessage::TournamentActionRejected {
                    request_id,
                    ..
                }) => Some(*request_id),
                _ => None,
            })
            .collect()
    }

    /// The single `TournamentActionAck` in `out`, or a panic naming what was
    /// there instead.
    fn ack_of(out: &[Outbound]) -> (TournamentRequestId, &str, &TournamentView) {
        out.iter()
            .find_map(|ob| match ob {
                Outbound::ToSelf(LobbyServerMessage::TournamentActionAck {
                    request_id,
                    code,
                    view,
                }) => Some((*request_id, code.as_str(), view)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a TournamentActionAck, got {out:?}"))
    }

    fn rejection_of(out: &[Outbound]) -> (TournamentRequestId, &str) {
        match out {
            [Outbound::ToSelf(LobbyServerMessage::TournamentActionRejected {
                request_id,
                message,
            })] => (*request_id, message.as_str()),
            other => panic!("expected a single TournamentActionRejected, got {other:?}"),
        }
    }

    fn has_ack(out: &[Outbound]) -> bool {
        out.iter().any(|ob| {
            matches!(
                ob,
                Outbound::ToSelf(LobbyServerMessage::TournamentActionAck { .. })
            )
        })
    }

    fn subscriber_update_view(out: &[Outbound]) -> &TournamentView {
        out.iter()
            .find_map(|ob| match ob {
                Outbound::ToSubscribers(LobbyServerMessage::TournamentUpdate { view, .. }) => {
                    Some(view)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a broadcast TournamentUpdate, got {out:?}"))
    }

    fn has_list_update(out: &[Outbound]) -> bool {
        out.iter().any(|ob| {
            matches!(
                ob,
                Outbound::ToSubscribers(LobbyServerMessage::TournamentListUpdate { .. })
            )
        })
    }

    /// V5 — THE MAINTAINER'S REGRESSION, broker half.
    ///
    /// Two connections act on ONE tournament through one `Broker`, each with
    /// its own correlator. The settlement each receives must carry *its own*
    /// id: the whole defect being fixed is that a caller could not tell its own
    /// outcome from another actor's frame for the same tournament.
    #[test]
    fn a_gated_settlement_carries_only_its_own_callers_request_id() {
        const ORGANIZER_ID: TournamentRequestId = TournamentRequestId(11);
        const ALICE_ID: TournamentRequestId = TournamentRequestId(22);
        const BOB_ID: TournamentRequestId = TournamentRequestId(33);

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();
        let mut alice = ConnState::default();
        let mut bob = ConnState::default();

        let (code, organizer_token) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        let token_a = join_tournament(&mut alice, &mut broker, &env, &code, "key-a", "Alice");
        join_tournament(&mut bob, &mut broker, &env, &code, "key-b", "Bob");

        // Hostile/negative pair, part 1: the true organizer's correlated action
        // is ACKED, not rejected — so the refusal below is about Bob's token,
        // not about correlation refusing everything.
        let started = broker.handle(
            &mut organizer,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: Some(ORGANIZER_ID),
            },
            &env,
        );
        assert_eq!(correlators(&started), vec![ORGANIZER_ID]);
        assert!(has_ack(&started), "{started:?}");

        let pairing_id = broker
            .tournaments()
            .get(&code)
            .expect("event")
            .pairings
            .first()
            .expect("round 1 paired")
            .id;

        // Alice reports her own pairing, correlated with HER id.
        let alice_out = broker.handle(
            &mut alice,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: token_a,
                outcome: PodOutcome::Draw,
                request_id: Some(ALICE_ID),
            },
            &env,
        );
        let (alice_ack_id, alice_ack_code, _) = ack_of(&alice_out);
        assert_eq!(alice_ack_id, ALICE_ID);
        assert_eq!(alice_ack_code, code);
        assert_eq!(correlators(&alice_out), vec![ALICE_ID]);

        // Bob — a real entrant of this very tournament, holding a real token
        // for the wrong authority tier — tries to end it, correlated with HIS
        // id. The refusal must be his, and must carry no trace of Alice's.
        let bob_out = broker.handle(
            &mut bob,
            LobbyClientMessage::EndTournament {
                code: code.clone(),
                organizer_token: "not-the-organizer".into(),
                request_id: Some(BOB_ID),
            },
            &env,
        );
        let (bob_id, bob_message) = rejection_of(&bob_out);
        assert_eq!(bob_id, BOB_ID);
        assert!(!bob_message.is_empty(), "a refusal must say why");
        assert!(
            !has_ack(&bob_out),
            "a refused action must not ack: {bob_out:?}"
        );
        assert_eq!(
            correlators(&bob_out),
            vec![BOB_ID],
            "Bob's refusal carried a correlator that was not his"
        );
        assert!(
            !correlators(&bob_out).contains(&ALICE_ID),
            "Alice's correlator reached Bob"
        );

        // Reach-guard: the refusal changed nothing. A rejection that also
        // completed the tournament would satisfy every assertion above.
        assert_eq!(
            broker.tournaments().get(&code).expect("event").status,
            TournamentStatus::InProgress
        );
    }

    /// V10 — an organizer who never subscribed still observes success.
    ///
    /// This is what the ack buys that the broadcast never could: before it, the
    /// four gated actions produced only a `ToSubscribers` frame, so an
    /// unsubscribed caller waited out its own timeout on an action that had
    /// already succeeded.
    #[test]
    fn a_gated_ack_reaches_an_unsubscribed_caller_alongside_the_broadcast() {
        const REQUEST_ID: TournamentRequestId = TournamentRequestId(7);

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();
        let mut player = ConnState::default();

        let (code, organizer_token) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        join_tournament(&mut player, &mut broker, &env, &code, "key-a", "Alice");
        join_tournament(&mut player, &mut broker, &env, &code, "key-b", "Bob");
        // The premise of the row: this connection never sent `SubscribeLobby`.
        assert!(!organizer.subscribed);

        let out = broker.handle(
            &mut organizer,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token,
                request_id: Some(REQUEST_ID),
            },
            &env,
        );

        let (ack_id, ack_code, ack_view) = ack_of(&out);
        assert_eq!(ack_id, REQUEST_ID);
        assert_eq!(ack_code, code);
        // The broadcast is still emitted alongside, and the two agree — the ack
        // is the same state, addressed, not a second and possibly divergent one.
        assert_eq!(ack_view, subscriber_update_view(&out));
        assert_eq!(ack_view.summary.status, TournamentStatus::InProgress);
        // Order is significant: the point reply precedes the fan-out, the same
        // convention `handle_join_tournament` follows.
        assert!(matches!(
            out.first(),
            Some(Outbound::ToSelf(
                LobbyServerMessage::TournamentActionAck { .. }
            ))
        ));
    }

    /// V11 — an uncorrelated caller's outbounds are unchanged by this fix.
    ///
    /// `request_id: None` is what every pre-correlation client sends, and it
    /// must keep producing exactly the vectors these handlers produced before
    /// `settle_gated` existed: the two-element tail for the three list-moving
    /// actions, the one-element tail for a result report, and a bare `Error`
    /// on refusal.
    #[test]
    fn an_uncorrelated_gated_caller_gets_the_pre_correlation_outbounds() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, organizer_token, token_a, _token_b) =
            started_event(&mut conn, &mut broker, &env);
        // `started_event` already ran the uncorrelated `StartTournamentRound`.
        let pairing_id = broker
            .tournaments()
            .get(&code)
            .expect("event")
            .pairings
            .first()
            .expect("round 1 paired")
            .id;

        let reported = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: token_a,
                outcome: PodOutcome::Draw,
                request_id: None,
            },
            &env,
        );
        assert!(matches!(
            reported.as_slice(),
            [Outbound::ToSubscribers(
                LobbyServerMessage::TournamentUpdate { .. }
            )]
        ));

        let ended = broker.handle(
            &mut conn,
            LobbyClientMessage::EndTournament {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: None,
            },
            &env,
        );
        assert!(matches!(
            ended.as_slice(),
            [
                Outbound::ToSubscribers(LobbyServerMessage::TournamentUpdate { .. }),
                Outbound::ToSubscribers(LobbyServerMessage::TournamentListUpdate { .. }),
            ]
        ));

        // The refusal half: still a bare `Error`, never the correlated variant.
        let refused = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: "wrong".into(),
                request_id: None,
            },
            &env,
        );
        assert!(is_error(&refused), "{refused:?}");
        assert!(correlators(&refused).is_empty());

        // Reach-guard in the same test: the SAME action, correlated, does gain
        // the ack — so the assertions above are about the correlator's absence,
        // not about a broker that stopped acking.
        let mut other = ConnState::default();
        let (code_two, organizer_two) =
            make_tournament(&mut other, &mut broker, &env, BracketShape::Swiss);
        join_tournament(&mut other, &mut broker, &env, &code_two, "key-a", "Alice");
        join_tournament(&mut other, &mut broker, &env, &code_two, "key-b", "Bob");
        let correlated = broker.handle(
            &mut other,
            LobbyClientMessage::StartTournamentRound {
                code: code_two,
                organizer_token: organizer_two,
                request_id: Some(TournamentRequestId(99)),
            },
            &env,
        );
        assert_eq!(correlated.len(), 3);
        assert_eq!(correlators(&correlated), vec![TournamentRequestId(99)]);
    }

    /// V12 — `ListRowEffect` preserves the report-result asymmetry.
    ///
    /// A result report moves no `TournamentSummary` field, so it must still be
    /// the one gated action that does not broadcast the list; the other three
    /// must still do so. Every arm also asserts its `TournamentUpdate` IS
    /// present, so "no list update" cannot pass by emitting nothing at all.
    #[test]
    fn only_a_result_report_leaves_the_list_row_untouched() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        let (code, organizer_token) =
            make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        let token_a = join_tournament(&mut conn, &mut broker, &env, &code, "key-a", "Alice");
        let token_b = join_tournament(&mut conn, &mut broker, &env, &code, "key-b", "Bob");

        let started = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: Some(TournamentRequestId(1)),
            },
            &env,
        );
        subscriber_update_view(&started);
        assert!(has_list_update(&started), "a new round moves the list row");

        let pairing_id = broker
            .tournaments()
            .get(&code)
            .expect("event")
            .pairings
            .first()
            .expect("round 1 paired")
            .id;
        let reported = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: token_a,
                outcome: PodOutcome::Draw,
                request_id: Some(TournamentRequestId(2)),
            },
            &env,
        );
        subscriber_update_view(&reported);
        assert!(
            !has_list_update(&reported),
            "a result report moves no summary field: {reported:?}"
        );

        let dropped = broker.handle(
            &mut conn,
            LobbyClientMessage::DropFromTournament {
                code: code.clone(),
                player_token: token_b,
                request_id: Some(TournamentRequestId(3)),
            },
            &env,
        );
        subscriber_update_view(&dropped);
        assert!(
            has_list_update(&dropped),
            "a drop lowers active_player_count: {dropped:?}"
        );

        let ended = broker.handle(
            &mut conn,
            LobbyClientMessage::EndTournament {
                code,
                organizer_token,
                request_id: Some(TournamentRequestId(4)),
            },
            &env,
        );
        subscriber_update_view(&ended);
        assert!(
            has_list_update(&ended),
            "completion moves status: {ended:?}"
        );
    }

    /// D6 — a gated frame refused at the inbound bounds guard, before any
    /// handler runs, still settles the caller's own correlator.
    ///
    /// `request_id` is read in `Broker::handle` ahead of `guard_inbound`
    /// specifically so this path is not a bare `Error`: a correlated caller
    /// deliberately ignores an uncorrelated `Error` (client module header,
    /// part 5), so a regression here would not fail loudly — it would hang
    /// every correlated caller to its timeout on an oversized-token frame the
    /// broker rejects instantly.
    #[test]
    fn a_guard_refused_gated_frame_still_settles_its_own_correlator() {
        const REQUEST_ID: TournamentRequestId = TournamentRequestId(42);
        let over_long = "t".repeat(crate::validation::MAX_TOKEN_LEN + 1);

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);

        let correlated = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: "TOUR01".into(),
                organizer_token: over_long.clone(),
                request_id: Some(REQUEST_ID),
            },
            &env,
        );
        let (id, message) = rejection_of(&correlated);
        assert_eq!(id, REQUEST_ID);
        // Discriminating, not just non-empty: `handle_start_tournament_round`'s
        // own refusals (bad token, tournament not found) route through this
        // same `settle_rejection` call and would otherwise satisfy a bare
        // non-empty check just as well — which would let this test silently
        // drift onto the handler path if the bounds check ever moved, taking
        // D6's only coverage with it without turning the suite red.
        assert!(
            message.contains("organizer_token") && message.contains("at most"),
            "expected the bounds-guard message, got: {message}"
        );

        // Reach-guard: the SAME oversized frame, uncorrelated, still produces
        // today's bare `Error` — proving the assertions above are about the
        // correlator's presence, not about a broker that started wrapping
        // every guard refusal in a tournament-shaped frame.
        let uncorrelated = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: "TOUR01".into(),
                organizer_token: over_long,
                request_id: None,
            },
            &env,
        );
        assert!(is_error(&uncorrelated), "{uncorrelated:?}");
    }

    // -- Row 5: the broker never short-circuits past the manager -------------

    #[test]
    fn token_gate_and_manager_gate_agree() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, organizer_token, token_a, _) = started_event(&mut conn, &mut broker, &env);

        let pairing_id = broker.tournaments().get(&code).expect("event").pairings[0].id;
        broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: token_a.clone(),
                outcome: PodOutcome::Draw,
                request_id: None,
            },
            &env,
        );
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::EndTournament {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out));
        assert_eq!(
            broker.tournaments().get(&code).expect("event").status,
            TournamentStatus::Completed
        );

        // A CORRECT token on a terminal tournament is still refused — by the
        // manager, whose message is surfaced verbatim. This is what proves the
        // broker's own token gate is defense in depth rather than the only
        // gate: passing it does not imply the call goes through.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: token_a,
                outcome: PodOutcome::Draw,
                request_id: None,
            },
            &env,
        );
        assert!(
            error_reason(&out).contains("no longer running"),
            "expected the manager's terminal-status message, got: {}",
            error_reason(&out)
        );

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code,
                organizer_token,
                request_id: None,
            },
            &env,
        );
        assert!(error_reason(&out).contains("no longer running"));
    }

    // -- Authority does not route through ConnState -------------------------

    /// The reconnect case, and the whole reason the model is token-based: a
    /// FRESH `ConnState` — empty `organized_tournaments`, no `ClientHello`,
    /// nothing — still exercises organizer authority with the right token.
    #[test]
    fn a_fresh_connection_with_the_right_token_still_has_authority() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut original = ConnState::default();
        let (code, organizer_token, ..) = started_event(&mut original, &mut broker, &env);

        // Simulate the socket closing entirely.
        let teardown = broker.on_disconnect(&mut original);
        assert!(
            !teardown.iter().any(|ob| matches!(
                ob,
                Outbound::ToSubscribers(LobbyServerMessage::TournamentRemoved { .. })
            )),
            "a disconnect must not remove a tournament"
        );
        assert!(
            broker.tournaments().get(&code).is_some(),
            "the event survives its organizer's socket"
        );

        let mut reconnected = ConnState::default();
        assert!(reconnected.organized_tournaments.is_empty());
        let pairing_id = broker.tournaments().get(&code).expect("event").pairings[0].id;
        broker.handle(
            &mut reconnected,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: broker.tournaments().get(&code).expect("event").players[0]
                    .player_token
                    .clone(),
                outcome: PodOutcome::Draw,
                request_id: None,
            },
            &env,
        );
        let out = broker.handle(
            &mut reconnected,
            LobbyClientMessage::EndTournament {
                code: code.clone(),
                organizer_token,
                request_id: None,
            },
            &env,
        );
        assert!(
            !is_error(&out),
            "authority must not route through ConnState"
        );
        assert_eq!(
            broker.tournaments().get(&code).expect("event").status,
            TournamentStatus::Completed
        );
    }

    // -- Rows 6 & 7: the widened reaper -------------------------------------

    #[test]
    fn reap_expired_recovers_lobby_and_tournament_events_together() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let game_code = game_code_of(&created);

        let mut organizer = ConnState::default();
        let (tour_code, _) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);

        // One sweep, past both the lobby timeout and the registration window.
        env.advance_secs(REGISTRATION_TIMEOUT_SECS + 1);
        let out = broker.reap_expired(300, &env);

        let msgs = subscriber_msgs(&out);
        assert!(
            msgs.iter().any(|m| matches!(
                m,
                LobbyServerMessage::LobbyGameRemoved { game_code: g } if *g == game_code
            )),
            "the pre-existing lobby path must still reap: {out:?}"
        );
        assert!(
            msgs.iter().any(|m| matches!(
                m,
                LobbyServerMessage::TournamentRemoved { code } if *code == tour_code
            )),
            "the tournament must reap in the SAME call: {out:?}"
        );
        // The list update reflects the now-empty registry, not a stale row.
        assert!(list_update(&out).is_empty());
        assert!(broker.tournaments().is_empty());
    }

    /// Negative control for the widening: a broker holding only a stale lobby
    /// game still reaps exactly as it did before, with no tournament noise.
    #[test]
    fn reap_with_no_tournaments_is_unchanged() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let created = create(&mut host, &mut broker, &env);
        let game_code = game_code_of(&created);

        env.advance_secs(301);
        let out = broker.reap_expired(300, &env);
        assert_eq!(
            out.as_slice(),
            [Outbound::ToSubscribers(
                LobbyServerMessage::LobbyGameRemoved { game_code }
            )],
            "no TournamentListUpdate may be emitted when nothing tournament-side changed"
        );
    }

    /// Replays the Durable Object's alarm loop (`lobby-worker/src/lobby-do.ts`
    /// `alarm()`: sweep -> snapshot -> `if (!broker.is_empty()) setAlarm(…)`)
    /// against a broker holding a tournament and NO lobby entries.
    ///
    /// That shape is the whole bug. [`Broker::is_empty`] is the alarm's only
    /// stop condition, so while it consulted the lobby alone it answered `true`
    /// here: the sweep that ran *before* the registration window closed was the
    /// last one ever scheduled, the tournament was never reaped, and the seat it
    /// holds in the bounded registry ([`MAX_TOURNAMENT_ENTRIES`]) was never
    /// returned. Nothing catches this from the lobby side — ordinary lobby
    /// traffic keeps rescheduling the alarm right over the top of it — and in
    /// production it stays silent until the registry is full.
    #[test]
    fn a_tournament_alone_keeps_the_broker_non_empty_until_it_is_reaped() {
        // `REAP_TIMEOUT_SECONDS` in lobby-do.ts. Lobby-only, and deliberately
        // NOT the tournament clock — see `Broker::reap_expired`.
        const LOBBY_TIMEOUT_SECS: u64 = 300;

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();

        // A broker holding nothing in either registry is still empty: the fix
        // must not pin the predicate to `false`, or no DO could ever hibernate.
        assert!(
            broker.is_empty(),
            "a fresh broker holds neither registry's state"
        );

        // Reach guard: `make_tournament` panics unless dispatch really replied
        // `TournamentCreated`, so the assertions below cannot pass vacuously on
        // a broker that rejected the frame and stored nothing.
        let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        assert!(
            broker.lobby().is_empty(),
            "no lobby entries — the exact condition the bug needs"
        );

        // THE regression assertion: a lobby-only predicate reports this
        // live-tournament broker as empty and lets `alarm()` stop rescheduling.
        assert!(
            !broker.is_empty(),
            "a live tournament must keep the reaper alarm scheduled"
        );

        // An alarm tick inside the registration window: nothing to reap yet,
        // and the shell must still reschedule.
        env.advance_secs(60);
        let early = broker.reap_expired(LOBBY_TIMEOUT_SECS, &env);
        assert!(
            early.is_empty(),
            "nothing expires inside the registration window: {early:?}"
        );
        assert!(
            !broker.is_empty(),
            "the tournament outlived an early sweep, so the alarm must too"
        );

        // The tick after the registration window closes — reachable only
        // because every sweep above rescheduled the alarm.
        env.advance_secs(REGISTRATION_TIMEOUT_SECS + 1);
        let swept = broker.reap_expired(LOBBY_TIMEOUT_SECS, &env);
        assert!(
            subscriber_msgs(&swept).iter().any(|m| matches!(
                m,
                LobbyServerMessage::TournamentRemoved { code: c } if *c == code
            )),
            "the idle tournament must be reaped, not merely uncounted: {swept:?}"
        );

        // Capacity recovered: the record is out of the bounded registry, and
        // only now may the shell stop its alarm and hibernate.
        assert!(
            broker.is_empty(),
            "both registries are empty once the timed-out tournament is reaped"
        );
    }

    /// Verification Matrix row 7: each `TournamentExpiryEvent` variant maps to
    /// its specified delivery, and N simultaneous expiries produce exactly ONE
    /// trailing list update, not N.
    #[test]
    fn expiry_delivery_contract_matches_the_design() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        // A `Registration` event that will be deleted outright...
        let (stale_code, _) = make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        // ...and an `InProgress` one that will be abandoned, in the same sweep.
        let (running_code, ..) = started_event(&mut conn, &mut broker, &env);

        env.advance_secs(IN_PROGRESS_ABANDON_SECS + 1);
        let out = broker.reap_expired(300, &env);
        let msgs = subscriber_msgs(&out);

        // Deleted -> TournamentRemoved.
        assert!(
            msgs.iter().any(|m| matches!(
                m,
                LobbyServerMessage::TournamentRemoved { code } if *code == stale_code
            )),
            "a stale Registration must be removed: {out:?}"
        );
        assert!(broker.tournaments().get(&stale_code).is_none());

        // Abandoned -> TournamentUpdate carrying the preserved record.
        let abandoned_view = msgs
            .iter()
            .find_map(|m| match m {
                LobbyServerMessage::TournamentUpdate { code, view } if *code == running_code => {
                    Some(view.clone())
                }
                _ => None,
            })
            .expect("an abandoned tournament is UPDATED, not removed");
        assert_eq!(abandoned_view.summary.status, TournamentStatus::Abandoned);
        assert_eq!(
            abandoned_view.players.len(),
            2,
            "the record and its history are preserved"
        );
        assert_eq!(abandoned_view.pairings.len(), 1);
        assert!(
            broker.tournaments().get(&running_code).is_some(),
            "an abandoned record is retained"
        );

        // Exactly ONE list update for two simultaneous events.
        let list_updates = msgs
            .iter()
            .filter(|m| matches!(m, LobbyServerMessage::TournamentListUpdate { .. }))
            .count();
        assert_eq!(
            list_updates, 1,
            "one sweep emits one trailing list update, not one per expiry"
        );
        // ...and it reflects the post-sweep state: the deleted one is gone,
        // the abandoned one remains.
        let list = list_update(&out);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].code, running_code);
        assert_eq!(list[0].status, TournamentStatus::Abandoned);
    }

    // -- Row 8: SubscribeLobby carries the current list ---------------------

    #[test]
    fn subscribe_includes_existing_tournaments_in_a_stable_order() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();
        let (first, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        let (second, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);

        let mut watcher = ConnState::default();
        let out = broker.handle(&mut watcher, LobbyClientMessage::SubscribeLobby, &env);
        let tournaments = match &out[2] {
            Outbound::ToSelf(LobbyServerMessage::TournamentListUpdate { tournaments }) => {
                tournaments.clone()
            }
            other => panic!("expected TournamentListUpdate third, got {other:?}"),
        };

        let mut expected = [first, second];
        expected.sort();
        let codes: Vec<String> = tournaments.iter().map(|t| t.code.clone()).collect();
        assert_eq!(codes, expected.to_vec(), "list order must be stable");

        // Repeating the subscription yields the identical order — the sort is
        // real, not an accident of one `HashMap` iteration.
        let again = broker.handle(&mut watcher, LobbyClientMessage::SubscribeLobby, &env);
        let codes_again: Vec<String> = match &again[2] {
            Outbound::ToSelf(LobbyServerMessage::TournamentListUpdate { tournaments }) => {
                tournaments.iter().map(|t| t.code.clone()).collect()
            }
            other => panic!("expected TournamentListUpdate, got {other:?}"),
        };
        assert_eq!(codes, codes_again);
    }

    // -- ConnState bookkeeping ---------------------------------------------

    #[test]
    fn conn_state_records_tournaments_without_granting_authority() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        assert!(conn.organized_tournaments.is_empty());
        assert!(conn.joined_tournaments.is_empty());

        let (code, _) = make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        let token = join_tournament(&mut conn, &mut broker, &env, &code, "key-a", "Alice");

        assert_eq!(conn.organized_tournaments, vec![code.clone()]);
        assert_eq!(conn.joined_tournaments, vec![code.clone()]);

        // Dropping does NOT remove the record that this connection was in the
        // event: the list is never pruned by a game action, only bounded at
        // its head by `MAX_CONN_TOURNAMENT_ENTRIES`, as documented.
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::DropFromTournament {
                code: code.clone(),
                player_token: token.clone(),
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&out));
        assert_eq!(conn.joined_tournaments, vec![code]);

        // ...and the record it keeps is the non-secret code, never the
        // `player_token` that authorized the join. The token was live enough
        // to drive a real drop immediately above, so this is a statement about
        // what is *retained*, not about an empty or inert value.
        assert!(!token.is_empty());
        let joined_json =
            serde_json::to_string(&conn.joined_tournaments).expect("joined_tournaments serializes");
        assert!(
            !joined_json.contains(token.as_str()),
            "player_token retained in ConnState::joined_tournaments: {joined_json}"
        );
    }

    /// The per-connection bookkeeping lists are bounded, and being at the
    /// bound never costs the client the create/join itself.
    ///
    /// Eviction is oldest-first, so the surviving window is the most recent
    /// `MAX_CONN_TOURNAMENT_ENTRIES` — the opposite of refusing the append,
    /// which would freeze the list on its oldest entries forever.
    #[test]
    fn per_connection_tournament_lists_are_bounded_without_failing_the_operation() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();
        let mut player = ConnState::default();

        let overshoot = 5;
        let mut codes = Vec::new();
        for _ in 0..MAX_CONN_TOURNAMENT_ENTRIES + overshoot {
            // Both helpers panic on an error outbound, so reaching the end of
            // the loop is itself the proof that the cap never failed a create
            // or a join.
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            join_tournament(&mut player, &mut broker, &env, &code, "key-a", "Alice");
            codes.push(code);
        }

        assert_eq!(
            organizer.organized_tournaments.len(),
            MAX_CONN_TOURNAMENT_ENTRIES
        );
        assert_eq!(player.joined_tournaments.len(), MAX_CONN_TOURNAMENT_ENTRIES);

        let newest = &codes[overshoot..];
        assert_eq!(
            organizer.organized_tournaments.as_slice(),
            newest,
            "the newest codes are the ones kept, in order"
        );
        assert_eq!(player.joined_tournaments.as_slice(), newest);

        // The registry itself is untouched by the per-connection bound: every
        // event still exists, only this socket's convenience list is trimmed.
        assert_eq!(
            broker.tournaments().len(),
            MAX_CONN_TOURNAMENT_ENTRIES + overshoot
        );
    }

    // -- Registry capacity --------------------------------------------------

    /// `CreateTournament` is refused at [`MAX_TOURNAMENT_ENTRIES`], the
    /// tournament-registry analogue of the lobby's `MAX_LOBBY_ENTRIES` gate,
    /// and capacity comes back when the reaper frees it.
    #[test]
    fn create_tournament_is_refused_at_capacity_and_recovers_after_a_reap() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        for _ in 0..MAX_TOURNAMENT_ENTRIES {
            make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        }
        assert_eq!(broker.tournaments().len(), MAX_TOURNAMENT_ENTRIES);

        let full = broker.handle(
            &mut conn,
            LobbyClientMessage::CreateTournament {
                name: "One Too Many".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: ScoringPolicy::default(),
                bracket: BracketShape::Swiss,
                total_rounds: None,
            },
            &env,
        );
        // `error_reason` also pins that the refusal is a *single* outbound:
        // nothing was created, so no list update may go out either.
        assert!(
            error_reason(&full).contains("tournament registry is full"),
            "unexpected reason: {}",
            error_reason(&full)
        );
        assert_eq!(
            broker.tournaments().len(),
            MAX_TOURNAMENT_ENTRIES,
            "a refused creation must not store anything"
        );

        // Capacity is recovered by the staleness reaper, the only path that
        // removes a `Registration` record.
        env.advance_secs(REGISTRATION_TIMEOUT_SECS + 1);
        broker.reap_expired(300, &env);
        assert!(broker.tournaments().is_empty());

        let (code, _) = make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);
        assert!(
            broker.tournaments().get(&code).is_some(),
            "creation must work again once the registry is below capacity"
        );
    }

    // -- Outbound shape per helper -----------------------------------------

    /// A mid-event result report is the ONE mutating tournament action that
    /// deliberately does not broadcast a list update, because no summary field
    /// changed. Pinning it stops a later edit from "fixing" the asymmetry.
    #[test]
    fn reporting_a_result_updates_the_detail_view_but_not_the_list() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, _, token_a, _) = started_event(&mut conn, &mut broker, &env);
        let pairing_id = broker.tournaments().get(&code).expect("event").pairings[0].id;

        let before = broker.tournaments().get(&code).expect("event").status;
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id,
                player_token: token_a,
                outcome: PodOutcome::Decisive {
                    winner: "key-a".into(),
                    game_wins: [("key-a".to_string(), 2u8), ("key-b".to_string(), 1u8)]
                        .into_iter()
                        .collect(),
                },
                request_id: None,
            },
            &env,
        );

        let msgs = subscriber_msgs(&out);
        assert_eq!(msgs.len(), 1, "exactly one broadcast: {out:?}");
        assert!(matches!(
            msgs[0],
            LobbyServerMessage::TournamentUpdate { .. }
        ));
        // Non-vacuity: the standings really did move.
        let view = view_of(&out);
        assert_eq!(
            view.pairings[0].outcome,
            Some(PairingOutcome::Reported(PodOutcome::Decisive {
                winner: "key-a".into(),
                game_wins: [("key-a".to_string(), 2u8), ("key-b".to_string(), 1u8)]
                    .into_iter()
                    .collect(),
            }))
        );
        assert!(view.standings.iter().any(|s| s.match_points > 0));
        // ...and the summary genuinely did not.
        assert_eq!(
            broker.tournaments().get(&code).expect("event").status,
            before
        );
    }

    /// A drop DOES change a summary field (`player_count` counts active
    /// entrants), so it pairs its detail update with a list update — the
    /// contrast that makes the test above meaningful.
    #[test]
    fn dropping_a_player_updates_both_the_view_and_the_list() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, _, token_a, _) = started_event(&mut conn, &mut broker, &env);

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::DropFromTournament {
                code: code.clone(),
                player_token: token_a,
                request_id: None,
            },
            &env,
        );
        assert_eq!(subscriber_msgs(&out).len(), 2);
        let list = list_update(&out);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].player_count, 1, "one of two entrants is left");
        assert_eq!(
            view_of(&out).players.len(),
            2,
            "the detail view keeps the dropped entrant"
        );
    }

    #[test]
    fn get_tournament_is_read_only_and_reports_a_missing_code() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, _) = make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::GetTournament { code: code.clone() },
            &env,
        );
        assert!(
            subscriber_msgs(&out).is_empty(),
            "a read must not broadcast"
        );
        assert_eq!(view_of(&out).summary.code, code);

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::GetTournament {
                code: "NOPE00".into(),
            },
            &env,
        );
        assert!(error_reason(&out).contains("Tournament not found"));
    }

    /// A `Broker` snapshot taken before the `tournaments` field existed must
    /// still restore its lobby entries rather than resetting the whole broker
    /// — the reason the field carries `#[serde(default)]`.
    #[test]
    fn a_pre_tournament_snapshot_still_deserializes() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        create(&mut host, &mut broker, &env);

        let full = serde_json::to_value(&broker).expect("snapshot");
        let mut legacy = full.as_object().expect("object").clone();
        legacy.remove("tournaments");
        assert!(legacy.contains_key("lobby"));

        let restored: Broker =
            serde_json::from_value(serde_json::Value::Object(legacy)).expect("legacy snapshot");
        assert_eq!(restored.lobby().len(), 1, "lobby entries survive");
        assert!(restored.tournaments().is_empty());
    }
}
