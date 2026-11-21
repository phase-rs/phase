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

use engine::types::format::GameFormat;
use engine::types::match_config::MatchType;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::env::BrokerEnv;
use crate::lobby::{ExpiryConsumption, LobbyManager, LobbyRegistration, RegisterGameRequest};
use crate::protocol::{
    code_in_use_message, LobbyClientMessage, LobbyGame, LobbyServerMessage, ServerErrorCode,
    ServerMode, TournamentRequestId, TournamentView,
};
use crate::reservation_auth::{
    consume_owned_reservation, release_owned_reservation, ReservationConsume, ReservationRelease,
    NOT_OWNED_RESERVATION,
};
use crate::snapshot_codec::encoded_len;
use crate::tournament::{
    BracketShape, CreateTournamentRequest, CredentialVerdict, MatchArity, PairingId, PodOutcome,
    ScoringPolicy, TournamentExpiryEvent, TournamentManager, TournamentMeta, TournamentRole,
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
///
/// Derived rather than chosen: every admitted event holds
/// [`EVENT_RESERVE_ENCODED_BYTES`] of the snapshot ceiling, so the registry can
/// hold exactly as many events as reservations fit beside
/// [`LOBBY_RESERVE_ENCODED_BYTES`]. A larger count would promise capacity the
/// persisted value cannot provide.
pub const MAX_TOURNAMENT_ENTRIES: usize =
    (MAX_SNAPSHOT_ENCODED_BYTES - LOBBY_RESERVE_ENCODED_BYTES) / EVENT_RESERVE_ENCODED_BYTES;

/// Hard ceiling on the encoded size of the *complete* persisted broker — every
/// lobby row, every tournament's entrant list, pairing history, standings and
/// retained decks — serialized as one value.
///
/// The whole broker is persisted under a single Durable Object storage key
/// (`ctx.storage.put(SNAPSHOT_KEY, broker.snapshot())`, where `SNAPSHOT_KEY` is
/// [`SNAPSHOT_STORAGE_KEY`] and
/// [`WasmBroker::snapshot`](../../../lobby-worker/broker-wasm) is
/// [`Broker::snapshot_json`]). Cloudflare's SQLite-backed Durable Objects
/// document that a write's *key and value combined cannot exceed 8 MB* (the same
/// 8 MB is the maximum row/string/BLOB size) — so a snapshot that crosses it
/// fails the `put` and the broker silently stops persisting, losing every later
/// mutation on the next hibernation. No single sub-budget can stand in for this:
/// the per-deck and per-event deck caps bound *deck* bytes, but a Swiss field is
/// unbounded and entrant/pairing metadata grows in the same value independently
/// of any deck, so the only correct bound is on the full serialized
/// representation. Enforced by measuring the exact candidate before a
/// state-growing admission commits, at the single persistence boundary in
/// [`Broker::handle`] (see [`Broker::committed_bytes`]), which every growth path
/// crosses; a refusal rolls the growth back so the last persistable state — decks and readiness included —
/// is preserved.
///
/// Set to 7 MiB, a deliberate margin of over 640 KiB below
/// [`PHYSICAL_SNAPSHOT_LIMIT_BYTES`] (which already takes the decimal reading of
/// 8 MB and subtracts the storage key): it absorbs SQLite per-row overhead and
/// future `Broker` fields, while still leaving room for
/// [`MAX_TOURNAMENT_ENTRIES`] events at the full per-event guarantee
/// ([`EVENT_RESERVE_ENCODED_BYTES`]). Conservative on purpose —
/// the cost of refusing one late submission is far lower than the cost of a
/// snapshot that cannot be written at all.
pub const MAX_SNAPSHOT_ENCODED_BYTES: usize = 7 * 1024 * 1024;

/// The Durable Object storage key the Worker persists the broker snapshot
/// under. The Worker reads it through the broker WASM glue rather than spelling
/// its own copy, so the key whose bytes [`PHYSICAL_SNAPSHOT_LIMIT_BYTES`]
/// subtracts is the key the `put` writes.
pub const SNAPSHOT_STORAGE_KEY: &str = "broker_snapshot";

/// Cloudflare's per-write ceiling for SQLite-backed Durable Objects: "Key and
/// value combined cannot exceed 8 MB"
/// (<https://developers.cloudflare.com/durable-objects/platform/limits/>). The
/// page never says whether that MB is 10⁶ or 2²⁰ bytes, so this takes the
/// smaller, decimal reading.
const DURABLE_OBJECT_KEY_VALUE_LIMIT_BYTES: usize = 8_000_000;

/// The largest encoded snapshot value the store accepts under
/// [`SNAPSHOT_STORAGE_KEY`]: the key-and-value ceiling less the key's own
/// bytes. Distinct from [`MAX_SNAPSHOT_ENCODED_BYTES`], which is the
/// *admission* budget kept deliberately below it: reservations make growth
/// inside unused room free of charge against the committed total, so a
/// restored snapshot already over the admission budget can still grow in
/// actual encoded bytes. The persistence boundary therefore also refuses any
/// growth of the actual encoded size past this value, so the Worker's `put` can
/// never be handed an unstorable key and value.
pub const PHYSICAL_SNAPSHOT_LIMIT_BYTES: usize =
    DURABLE_OBJECT_KEY_VALUE_LIMIT_BYTES - SNAPSHOT_STORAGE_KEY.len() - V8_STRING_FRAMING_BYTES;

/// Worst-case bytes V8's value serializer adds around a string it stores: the
/// two-byte version header, an alignment pad, the string tag and a varint length
/// of at most five bytes. Reserved out of [`PHYSICAL_SNAPSHOT_LIMIT_BYTES`] so a
/// measured value plus its framing plus the key is what the row holds (a
/// 7,999,985 byte ASCII string serializes to 7,999,992).
const V8_STRING_FRAMING_BYTES: usize = 2 + 1 + 1 + 5;

const _: () = assert!(MAX_SNAPSHOT_ENCODED_BYTES < PHYSICAL_SNAPSHOT_LIMIT_BYTES);

/// Metadata capacity guaranteed to every *admitted* tournament, in encoded
/// bytes of that event's own record with its retained decks set aside
/// (entrants, pairings, standings).
///
/// [`MAX_SNAPSHOT_ENCODED_BYTES`] is shared by every event, so a bound on the
/// total alone lets whichever event grows first take the room another admitted
/// event still needs. The reservations close that: while an event's metadata is
/// smaller than this figure, the unused remainder counts as already spoken for
/// ([`Broker::committed_bytes`]), so no write to *another* event, to the lobby or
/// a new admission can be accepted if it would leave this event unable to reach
/// it. Metadata growth past it competes for the unreserved headroom of the shared
/// store and is refused when none is left; it can never draw on
/// [`EVENT_DECK_RESERVE_ENCODED_BYTES`], which is a separate account.
pub const EVENT_METADATA_RESERVE_ENCODED_BYTES: usize = 32 * 1024;

/// Deck capacity guaranteed to every *admitted* tournament: exactly the deck
/// budget the event advertises ([`crate::tournament::MAX_EVENT_DECK_ENCODED_BYTES`]),
/// so the room a submission was promised is the room admission holds for it.
///
/// A separate account from the metadata reserve, measured by
/// [`TournamentMeta::retained_deck_bytes`]: only this event's own deck
/// submissions consume it, and no metadata growth, in this event or another, and
/// no other event's decks can. Because the per-event deck gate refuses a deck
/// that would take the retained total past this same figure, a deck the event
/// accepts is, by construction, already paid for at the persistence boundary, at
/// any fill level of the shared store. At least two decks at the
/// [`crate::tournament::MAX_SINGLE_DECK_ENCODED_BYTES`] abuse ceiling fit (a
/// head-to-head pairing's two submissions).
pub const EVENT_DECK_RESERVE_ENCODED_BYTES: usize = crate::tournament::MAX_EVENT_DECK_ENCODED_BYTES;

/// Capacity guaranteed to every *admitted* tournament in total: its metadata
/// allowance plus its deck allowance, each protected independently (see
/// [`EVENT_METADATA_RESERVE_ENCODED_BYTES`] and
/// [`EVENT_DECK_RESERVE_ENCODED_BYTES`]). This is the unit the registry is sized
/// in ([`MAX_TOURNAMENT_ENTRIES`]) and the room a new admission must leave free.
pub const EVENT_RESERVE_ENCODED_BYTES: usize =
    EVENT_METADATA_RESERVE_ENCODED_BYTES + EVENT_DECK_RESERVE_ENCODED_BYTES;

// A head-to-head pairing's two supported submissions must fit the deck account.
const _: () = assert!(
    2 * crate::tournament::MAX_SINGLE_DECK_ENCODED_BYTES <= EVENT_DECK_RESERVE_ENCODED_BYTES
);

/// The same guarantee for the lobby registry as a whole: lobby rows may always
/// grow to this many encoded bytes, so tournament traffic cannot starve game
/// listings either.
pub const LOBBY_RESERVE_ENCODED_BYTES: usize = 512 * 1024;

// The reservations must fit inside the ceiling they divide, or the guarantee is
// unsatisfiable by construction.
const _: () = assert!(
    MAX_TOURNAMENT_ENTRIES * EVENT_RESERVE_ENCODED_BYTES + LOBBY_RESERVE_ENCODED_BYTES
        <= MAX_SNAPSHOT_ENCODED_BYTES
);

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
/// keeping the worst case per socket trivial. See `push_conn_tournament` for
/// why reaching the cap evicts rather than refuses.
pub const MAX_CONN_TOURNAMENT_ENTRIES: usize = 50;

/// First lobby version whose clients write today's format-config rules; never
/// derived from `LOBBY_PROTOCOL_VERSION`.
pub const MIN_LOBBY_PROTOCOL_FOR_COMMANDER_CONFIG_RULES: u32 = 23;

/// The client's self-reported identity from `ClientHello`. `build_commit` is
/// the join-compatibility gate; `client_version` is the display-only string
/// stamped into a registered entry's `host_version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHelloInfo {
    pub client_version: String,
    pub build_commit: String,
    /// The lobby version the hello declared. `default`: a connection attachment
    /// written before this field must still decode.
    #[serde(default)]
    pub lobby_protocol_version: Option<u32>,
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
    /// The registration this connection created as host, if any (ownership
    /// stamp). Disconnect / re-registration teardown and every host-only check
    /// key off this — and act only while it is still the registration listed
    /// under its code ([`LobbyManager::is_current`]): the listing is reaped
    /// once the timeout passes since its liveness clock last advanced. This
    /// socket's frames advance that clock at most once per
    /// [`LIVENESS_REFRESH_SECS`](crate::lobby::LIVENESS_REFRESH_SECS) (see
    /// [`LobbyManager::refresh_liveness`]), so the listing can be reaped up to
    /// that interval sooner than the timeout after its last frame. A half-open
    /// or throttled socket that stays open is reaped this way too, and the
    /// code can then be claimed by another host, whose listing this stamp must
    /// never touch.
    pub host_game: Option<LobbyRegistration>,
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
    /// `push_conn_tournament`, which holds the list at
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

impl ConnState {
    /// The accepted hello's build, `""` before a hello.
    fn hello_build_commit(&self) -> &str {
        self.client_hello
            .as_ref()
            .map_or("", |h| h.build_commit.as_str())
    }
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

/// Whether a subscriber on `viewer_build_commit` may see `game`: the join gate's answer.
pub fn lobby_row_visible_to(game: &LobbyGame, viewer_build_commit: &str) -> bool {
    check_build_commit(&game.host_build_commit, viewer_build_commit) == BuildCommitCheck::Allow
}

/// A row delta's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowDelta {
    Added,
    Updated,
}

/// What one subscriber receives for a row delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowDelivery {
    Deliver,
    Withhold,
    /// Send the row's `LobbyGameRemoved` instead.
    Retract,
}

pub fn row_delivery(delta: RowDelta, game: &LobbyGame, viewer_build_commit: &str) -> RowDelivery {
    match (lobby_row_visible_to(game, viewer_build_commit), delta) {
        (true, _) => RowDelivery::Deliver,
        (false, RowDelta::Added) => RowDelivery::Withhold,
        // The viewer may hold a copy listed before the host re-stamped its build.
        (false, RowDelta::Updated) => RowDelivery::Retract,
    }
}

/// `msg` as a subscriber on `viewer_build_commit` receives it, or `None`.
pub fn lobby_frame_for_viewer(
    msg: &LobbyServerMessage,
    viewer_build_commit: &str,
) -> Option<LobbyServerMessage> {
    let (delta, game) = match msg {
        LobbyServerMessage::LobbyGameAdded { game } => (RowDelta::Added, game),
        LobbyServerMessage::LobbyGameUpdated { game } => (RowDelta::Updated, game),
        LobbyServerMessage::ServerHello { .. }
        | LobbyServerMessage::GameCreated { .. }
        | LobbyServerMessage::Error { .. }
        | LobbyServerMessage::LobbyUpdate { .. }
        | LobbyServerMessage::LobbyGameRemoved { .. }
        | LobbyServerMessage::PlayerCount { .. }
        | LobbyServerMessage::PasswordRequired { .. }
        | LobbyServerMessage::JoinTargetInfo { .. }
        | LobbyServerMessage::Pong { .. }
        | LobbyServerMessage::PeerInfo { .. }
        | LobbyServerMessage::TournamentCreated { .. }
        | LobbyServerMessage::TournamentJoined { .. }
        | LobbyServerMessage::TournamentUpdate { .. }
        | LobbyServerMessage::TournamentRemoved { .. }
        | LobbyServerMessage::TournamentListUpdate { .. }
        | LobbyServerMessage::TournamentActionAck { .. }
        | LobbyServerMessage::TournamentActionRejected { .. }
        | LobbyServerMessage::TournamentCredentialRenewed { .. } => return Some(msg.clone()),
    };
    match row_delivery(delta, game, viewer_build_commit) {
        RowDelivery::Deliver => Some(msg.clone()),
        RowDelivery::Withhold => None,
        RowDelivery::Retract => Some(LobbyServerMessage::LobbyGameRemoved {
            game_code: game.game_code.clone(),
        }),
    }
}

/// [`lobby_frame_for_viewer`] over a serialized frame; an unparsable frame is sent unchanged.
pub fn lobby_frame_json_for_viewer(frame_json: &str, viewer_build_commit: &str) -> Option<String> {
    let Ok(msg) = serde_json::from_str::<LobbyServerMessage>(frame_json) else {
        return Some(frame_json.to_string());
    };
    lobby_frame_for_viewer(&msg, viewer_build_commit)
        .map(|frame| serde_json::to_string(&frame).expect("a lobby frame serializes"))
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

/// What one sweep did, reported in the pieces a caller actually needs rather
/// than as one vector it has to take apart again.
///
/// The halves are separate because a caller that recovered them by arithmetic
/// got it wrong: consumption is identity-conditional, so a superseded
/// observation is consumed without an outbound, and deriving the lobby count by
/// subtracting what was handed in underflowed on exactly the tick the identity
/// check fires. Nothing here has to be subtracted.
///
/// `lobby_removed` is also what a caller's own cleanup must key off. A
/// superseded code names a live replacement, so pruning its connections or
/// spectators would reach into a game this sweep did not retire.
#[derive(Debug)]
pub struct ReapOutcome {
    /// Codes whose removal was announced — every handled observation but a
    /// superseded one, in the order handed in. One `LobbyGameRemoved` was
    /// emitted per element.
    pub lobby_removed: Vec<String>,
    /// Handled observations left alone because a replacement holds the code.
    pub superseded: usize,
    /// The tournament half, which no caller can decline and which is swept
    /// here rather than reported (see [`TournamentManager::check_expired`]).
    pub tournament: Vec<Outbound>,
}

impl ReapOutcome {
    /// Every outbound this sweep produced, in the wire order the lobby
    /// protocol has always used: one `LobbyGameRemoved` per removed code,
    /// then the tournament lifecycle events, then at most one trailing
    /// `TournamentListUpdate`.
    pub fn into_outbounds(self) -> Vec<Outbound> {
        self.lobby_removed
            .into_iter()
            .map(|game_code| {
                Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved { game_code })
            })
            .chain(self.tournament)
            .collect()
    }
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

    /// The heartbeat timestamp echo, independent of listing presence refresh.
    /// Both broker dispatch and shells with a separate refresh task use this
    /// constructor, so answering a heartbeat never needs a fabricated connection.
    #[must_use]
    pub fn heartbeat_reply(timestamp: u64) -> LobbyServerMessage {
        LobbyServerMessage::Pong { timestamp }
    }
    /// The reply to a frame that needs no broker state and has no side effects,
    /// or `None` when the frame must go through [`Broker::handle`].
    ///
    /// Takes no `self`, so a shell can answer without taking the lock that
    /// guards the broker. Today that is a `Ping` from a connection that does
    /// not host a listing: a host's frame must also refresh its listing's
    /// liveness clock (see the prelude of [`Broker::handle`]), which needs the
    /// broker, so a hosting connection always yields `None`. The reply is a
    /// point reply to the sender, hence a bare [`LobbyServerMessage`] rather
    /// than an [`Outbound`] list. [`Broker::handle`] answers through this same
    /// function, so the reply has exactly one definition.
    #[must_use]
    pub fn stateless_reply(
        conn: &ConnState,
        msg: &LobbyClientMessage,
    ) -> Option<LobbyServerMessage> {
        match msg {
            LobbyClientMessage::Ping { timestamp } if conn.host_game.is_none() => {
                Some(Self::heartbeat_reply(*timestamp))
            }
            _ => None,
        }
    }

    /// Single entry for client frames. Returns the ordered side effects the
    /// shell must perform. The error helper keeps the many reject paths terse.
    ///
    /// Shells that persist the broker (the Durable Object) call
    /// [`Broker::handle_reporting`] instead, which also says whether the frame
    /// changed durable state.
    pub fn handle(
        &mut self,
        conn: &mut ConnState,
        msg: LobbyClientMessage,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        self.handle_reporting(conn, msg, env).outbounds
    }

    /// [`Broker::handle`], plus whether durable state changed
    /// ([`Handled::persisted`]), so a persisting shell writes its snapshot only
    /// for a frame that needs one. The host-liveness refresh is not part of this
    /// report: it is decided by [`Broker::host_liveness_refresh_due`] before the
    /// frame runs, and a shell combines the two.
    pub fn handle_reporting(
        &mut self,
        conn: &mut ConnState,
        msg: LobbyClientMessage,
        env: &impl BrokerEnv,
    ) -> Handled {
        // Frames answerable without broker state (an unhosted Ping) never
        // reach the liveness refresh, the guard or the dispatch below: none of
        // them has anything to do for such a frame.
        if let Some(reply) = Self::stateless_reply(conn, &msg) {
            return Handled::unchanged(vec![Outbound::ToSelf(reply)]);
        }
        // Any frame from the socket that owns the current listing is proof its
        // host is alive, so it refreshes the listing's liveness clock. Scoped
        // by identity, so a stale stamp cannot extend a reclaimer's listing;
        // quantized, see `LIVENESS_REFRESH_SECS`. Ahead of the guard so a shell
        // that persists on `host_liveness_refresh_due` sees exactly the frames
        // that write — a guard-rejected frame from the host still proves life.
        if let Some(registration) = &conn.host_game {
            self.lobby.refresh_liveness(registration, env);
        }
        // The correlator is read BEFORE the guard, so a gated frame refused at
        // the bounds check is refused *to that request* rather than by a bare
        // `Error` a correlated caller is designed to ignore — which would be a
        // hang until timeout. Exhaustive by construction on the message enum,
        // so a future gated variant that forgets its correlator is visible at
        // `LobbyClientMessage::tournament_request_id` rather than silently
        // uncorrelated here.
        let request_id = msg.tournament_request_id();
        if let Err(reason) = crate::inbound_guard::guard_inbound(&msg) {
            return Handled::unchanged(vec![settle_rejection(request_id, &reason)]);
        }
        match persisted_effect(&msg) {
            PersistedEffect::None => {
                return Handled::unchanged(self.dispatch(conn, msg, request_id, env));
            }
            // A removal only deletes, so it cannot outgrow the store and needs no
            // admission; it does change the persisted value.
            PersistedEffect::Removes => {
                return Handled::changed(self.dispatch(conn, msg, request_id, env));
            }
            PersistedEffect::MayGrow => {}
        }
        // A frame addressed to a tournament that does not exist can only be
        // refused: every handler looks the event up first and mutates nothing
        // before it succeeds. Answering it here skips the checkpoint clone and
        // the whole-broker accounting below, whose cost is proportional to all
        // retained state, not to the size of the request.
        if self.addresses_missing_tournament(&msg) {
            return Handled::unchanged(self.dispatch(conn, msg, request_id, env));
        }
        // The persistence boundary. Every frame that can grow the persisted value
        // runs against a checkpoint and is admitted only if the result still fits
        // the snapshot ceiling and every reservation (see
        // [`Broker::committed_bytes`]) and never grows the actual encoded value
        // past the physical key limit; a refusal restores the checkpoint, so the
        // caller never observes a success for state the Durable Object could not
        // store, and no per-handler check can be forgotten by a new growth path.
        let checkpoint = Checkpoint {
            lobby: self.lobby.clone(),
            tournaments: self.tournaments.clone(),
            conn: conn.clone(),
        };
        let outbounds = self.dispatch(conn, msg, request_id, env);
        match self.admit_or_restore(checkpoint, conn) {
            Ok(persisted) => Handled {
                outbounds,
                persisted,
            },
            Err(reason) => {
                warn!(%reason, "write refused at the persistence boundary");
                Handled::unchanged(vec![settle_rejection(request_id, &reason)])
            }
        }
    }

    /// Whether `msg` is addressed, by code, to a tournament this broker does not
    /// hold. Exhaustive over the frames that name an event, so a new one must be
    /// classified deliberately.
    fn addresses_missing_tournament(&self, msg: &LobbyClientMessage) -> bool {
        let code = match msg {
            LobbyClientMessage::JoinTournament { code, .. }
            | LobbyClientMessage::RenewTournamentCredential { code, .. }
            | LobbyClientMessage::StartTournamentRound { code, .. }
            | LobbyClientMessage::ReportMatchResult { code, .. }
            | LobbyClientMessage::SubmitTournamentDeck { code, .. }
            | LobbyClientMessage::DropFromTournament { code, .. }
            | LobbyClientMessage::EndTournament { code, .. } => code,
            LobbyClientMessage::CreateGameWithSettings { .. }
            | LobbyClientMessage::JoinGameWithPassword { .. }
            | LobbyClientMessage::LookupJoinTarget { .. }
            | LobbyClientMessage::UpdateLobbyMetadata { .. }
            | LobbyClientMessage::CreateTournament { .. }
            | LobbyClientMessage::ClientHello { .. }
            | LobbyClientMessage::SubscribeLobby
            | LobbyClientMessage::UnsubscribeLobby
            | LobbyClientMessage::Ping { .. }
            | LobbyClientMessage::GetTournament { .. }
            | LobbyClientMessage::UnregisterLobby { .. } => return false,
        };
        self.tournaments.get(code).is_none()
    }

    /// Whether the persisted state after `self` would stay within capacity,
    /// otherwise restores `checkpoint` (broker and connection) and says why.
    /// On admission, says whether durable state actually changed.
    ///
    /// Two independent bounds. The committed total ([`Self::committed_bytes`])
    /// must stay within [`MAX_SNAPSHOT_ENCODED_BYTES`] unless the write does not
    /// raise it, so a state restored from an older, larger snapshot can still
    /// shrink (drop, end, unregister). And the *actual* stored size must never
    /// grow past [`PHYSICAL_SNAPSHOT_LIMIT_BYTES`]: growth inside an unused
    /// reservation leaves the committed total flat, so for a restored
    /// over-budget snapshot the committed bound alone would let the real value
    /// creep past what the store can hold. A write that *strictly* reduces the
    /// stored size is always admitted (it is progress toward a storable value);
    /// a write that leaves it equal is admitted only inside the physical bound.
    ///
    /// A frame that left the stored snapshot byte-for-byte as it was (a request
    /// the handler refused without mutating anything) is admitted as
    /// [`PersistedChange::Unchanged`] whatever the size, so it neither reports
    /// dirty nor is mistaken for a growth refusal: its own correlated refusal is
    /// delivered and no write is attempted.
    fn admit_or_restore(
        &mut self,
        checkpoint: Checkpoint,
        conn: &mut ConnState,
    ) -> Result<PersistedChange, String> {
        let (actual_after, committed_after) = self.accounting();
        if committed_after <= MAX_SNAPSHOT_ENCODED_BYTES {
            // Committed bounds actual, so this value is inside the physical limit.
            return Ok(PersistedChange::Changed);
        }
        // Over the ceiling: swap the checkpoint in to measure what it held.
        // `checkpoint` then holds the candidate, so either side can be kept.
        let Checkpoint {
            mut lobby,
            mut tournaments,
            conn: prior_conn,
        } = checkpoint;
        std::mem::swap(&mut self.lobby, &mut lobby);
        std::mem::swap(&mut self.tournaments, &mut tournaments);
        let (actual_before, committed_before) = self.accounting();
        // Equal stored size is the only case a no-op can hide in; compare the
        // stored text itself (the one authority) only then.
        let before_text = (actual_after == actual_before).then(|| self.snapshot_json());
        std::mem::swap(&mut self.lobby, &mut lobby);
        std::mem::swap(&mut self.tournaments, &mut tournaments);
        if let Some(before_text) = before_text {
            if self.snapshot_json() == before_text {
                return Ok(PersistedChange::Unchanged);
            }
        }
        let strictly_shrinks = actual_after < actual_before;
        let within_bounds =
            committed_after <= committed_before && actual_after <= PHYSICAL_SNAPSHOT_LIMIT_BYTES;
        if strictly_shrinks || within_bounds {
            return Ok(PersistedChange::Changed);
        }
        // Refused: put the checkpoint back (it is in the locals after the swap back).
        std::mem::swap(&mut self.lobby, &mut lobby);
        std::mem::swap(&mut self.tournaments, &mut tournaments);
        *conn = prior_conn;
        Err(format!(
            "Refused: the server's persisted tournament state is at capacity \
             ({MAX_SNAPSHOT_ENCODED_BYTES} encoded bytes, including space held for \
             other events, and never past {PHYSICAL_SNAPSHOT_LIMIT_BYTES}); retry once \
             activity subsides"
        ))
    }

    /// Dispatch one admitted, guard-checked frame to its handler.
    fn dispatch(
        &mut self,
        conn: &mut ConnState,
        msg: LobbyClientMessage,
        request_id: Option<TournamentRequestId>,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        match msg {
            LobbyClientMessage::ClientHello {
                client_version,
                build_commit,
                protocol_version: _,
                lobby_protocol_version,
            } => {
                // The handshake gate (protocol-version check, first-frame
                // enforcement) stays in the shell; by the time a ClientHello
                // reaches the broker it has been accepted. Record the commit so
                // join gates can compare it. No outbound.
                info!(version = %client_version, commit = %build_commit, "ClientHello accepted");
                conn.client_hello = Some(ClientHelloInfo {
                    client_version,
                    build_commit,
                    lobby_protocol_version,
                });
                vec![]
            }

            LobbyClientMessage::SubscribeLobby => {
                debug!("lobby subscription");
                conn.subscribed = true;
                let viewer = conn.hello_build_commit();
                let games: Vec<LobbyGame> = self
                    .lobby
                    .public_games()
                    .into_iter()
                    .filter(|game| lobby_row_visible_to(game, viewer))
                    .collect();
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
                requested_code,
            } => {
                // A pre-change client's config is relayed as its engine wrote it; a current
                // client sends only what today's validator admits.
                let declares_current = conn
                    .client_hello
                    .as_ref()
                    .and_then(|hello| hello.lobby_protocol_version)
                    .is_some_and(|version| {
                        version >= MIN_LOBBY_PROTOCOL_FOR_COMMANDER_CONFIG_RULES
                    });
                if declares_current {
                    if let Some(config) = &format_config {
                        if let Err(reason) = config.check_strict_admission() {
                            return vec![settle_rejection(request_id, &reason)];
                        }
                    }
                }
                self.handle_create_game(
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
                    requested_code,
                    env,
                )
            }

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

            // Reached only by a hosting connection (an unhosted Ping is
            // answered by `stateless_reply` above), after its liveness refresh.
            LobbyClientMessage::Ping { timestamp } => {
                vec![Outbound::ToSelf(Self::heartbeat_reply(timestamp))]
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
                plus_rounds,
                format,
                match_type,
            } => self.handle_create_tournament(
                conn,
                name,
                arity,
                scoring,
                bracket,
                total_rounds,
                plus_rounds,
                format,
                match_type,
                env,
            ),

            LobbyClientMessage::JoinTournament {
                code,
                player_key,
                display_name,
            } => self.handle_join_tournament(conn, code, player_key, display_name, env),

            LobbyClientMessage::GetTournament { code } => self.handle_get_tournament(code),

            LobbyClientMessage::RenewTournamentCredential {
                code,
                role,
                token,
                rotation_nonce,
            } => self.handle_renew_tournament_credential(code, role, token, rotation_nonce, env),

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

            LobbyClientMessage::SubmitTournamentDeck {
                code,
                player_token,
                deck,
                request_id,
            } => {
                let outcome = self.handle_submit_tournament_deck(code, player_token, deck, env);
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

        if let Some(registration) = conn.host_game.take() {
            if self.lobby.unregister_registration(&registration) {
                let game_code = registration.game_code().to_string();
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
    /// expired codes from [`Broker::lobby`]`.check_expired` directly, and
    /// tournaments have no equivalent server-run session to clean up.
    ///
    /// A lobby entry expires once its liveness clock — advanced by its host's
    /// frames through [`Broker::handle`] — is older than `timeout_secs`, so a
    /// host that keeps its socket talking is never reaped.
    ///
    /// Consumes **every** expired lobby entry, which is right for a shell whose
    /// sweep cannot decline: a Durable Object has no session registry to
    /// contend on, so reporting an entry and disposing of it are the same act.
    /// Nothing can replace an entry between the two halves here — they run
    /// under one `&mut self` — so every observation this path makes is still
    /// good when it is consumed.
    /// A shell that *can* decline — the Full-mode server, whose `try_session`
    /// defers a game mid-transition — reports with
    /// [`Broker::lobby`]`.check_expired` and consumes with
    /// [`Broker::reap_expired_handled`] instead, so an entry it skipped
    /// survives to be reported again.
    ///
    /// `timeout_secs` applies to lobby entries only. Tournament expiry runs on
    /// the three fixed lifecycle clocks
    /// ([`crate::tournament::REGISTRATION_TIMEOUT_SECS`] and siblings) that
    /// [`TournamentManager::check_expired`] owns, which is why it takes no
    /// timeout argument: a registration window, a 7-day abandonment and a
    /// 30-day retention are not the same duration as a lobby listing's, and
    /// threading one number through both would imply they are.
    pub fn reap_expired(&mut self, timeout_secs: u64, env: &impl BrokerEnv) -> Vec<Outbound> {
        let expired = self.lobby.check_expired(timeout_secs, env);
        self.reap_expired_handled(&expired, env).into_outbounds()
    }

    /// [`Broker::reap_expired`]'s sweep for a caller that already reported the
    /// expired lobby codes itself and acted on them: it consumes exactly
    /// `handled` and leaves every other entry registered.
    ///
    /// Reports one removed code per element of `handled` whose observation is
    /// still good, in order, alongside the tournament half unchanged — the
    /// tournament registry is swept here rather than by the caller because no
    /// consumer of it can decline (see [`TournamentManager::check_expired`]),
    /// so there is nothing to report separately. Call it on every tick for that
    /// reason, not only on the ticks a lobby entry lapsed.
    /// [`ReapOutcome::into_outbounds`] renders both halves in wire order.
    ///
    /// Consumption is identity-conditional. The lobby lock is released between
    /// the caller's report and this call, so what sits under a reported code
    /// may no longer be what was reported, and the two ways that can happen are
    /// not the same act (see [`ExpiryConsumption`]). A code another path
    /// already unregistered still emits its removal — a client that over-prunes
    /// recovers where one that never hears of the removal does not. A code a
    /// `register_game` has since *replaced* emits nothing and keeps its entry:
    /// that listing is live and unexpired, and removing it here would delist a
    /// game nobody retired.
    pub fn reap_expired_handled(
        &mut self,
        handled: &[LobbyRegistration],
        env: &impl BrokerEnv,
    ) -> ReapOutcome {
        let mut lobby_removed: Vec<String> = Vec::new();
        let mut superseded = 0usize;
        for expired in handled {
            match self.lobby.unregister_expired(expired) {
                ExpiryConsumption::Removed | ExpiryConsumption::AlreadyGone => {
                    lobby_removed.push(expired.game_code().to_string())
                }
                ExpiryConsumption::Superseded => superseded += 1,
            }
        }
        let mut out: Vec<Outbound> = Vec::new();

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

        ReapOutcome {
            lobby_removed,
            superseded,
            tournament: out,
        }
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
        requested_code: Option<String>,
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
            if self.lobby.unregister_registration(&previous) {
                let game_code = previous.game_code().to_string();
                info!(game = %game_code, "replacing previous lobby entry from same socket");
                out.push(Outbound::ToSubscribers(
                    LobbyServerMessage::LobbyGameRemoved { game_code },
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

        let game_code = match requested_code {
            Some(code) if self.lobby.has_game(&code) => {
                warn!(game = %code, "requested game code already registered");
                out.push(Outbound::ToSelf(LobbyServerMessage::error_with_code(
                    ServerErrorCode::CodeInUse,
                    code_in_use_message(&code),
                )));
                return out;
            }
            Some(code) => code,
            None => env.new_game_code(),
        };
        let player_token = env.new_token();
        let pc = requested_player_count.clamp(2, 6);
        let (host_version, host_build_commit) = conn
            .client_hello
            .as_ref()
            .map(|h| (h.client_version.clone(), h.build_commit.clone()))
            .unwrap_or_default();

        let registration = self.lobby.register_game(
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

        conn.host_game = Some(registration);

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

    /// Whether [`Broker::handle`] would advance the liveness clock of the
    /// listing `conn` hosts if it handled a frame from `conn` now. The
    /// persistence query for a shell that snapshots: evaluate it **before**
    /// `handle`, since the refresh it predicts happens there.
    pub fn host_liveness_refresh_due(&self, conn: &ConnState, env: &impl BrokerEnv) -> bool {
        conn.host_game
            .as_ref()
            .is_some_and(|r| self.lobby.liveness_refresh_due(r, env))
    }

    /// Whether `conn` hosts the listing currently under `game_code`. A stamp
    /// whose listing was reaped (and possibly re-claimed by another host) does
    /// not count — see [`ConnState::host_game`].
    fn hosts_current(&self, conn: &ConnState, game_code: &str) -> bool {
        conn.host_game
            .as_ref()
            .is_some_and(|r| r.game_code() == game_code && self.lobby.is_current(r))
    }

    fn handle_join(
        &mut self,
        conn: &mut ConnState,
        game_code: String,
        display_name: String,
        password: Option<String>,
        reservation_token: Option<String>,
    ) -> Vec<Outbound> {
        if self.hosts_current(conn, &game_code) {
            return vec![error("You are already hosting this game")];
        }

        let guest_commit = conn.hello_build_commit();
        let host_commit = self.lobby.host_build_commit(&game_code).unwrap_or("");
        if let BuildCommitCheck::Reject { host, guest } =
            check_build_commit(host_commit, guest_commit)
        {
            warn!(game = %game_code, %host, %guest, "build mismatch — refusing join (lobby-only)");
            return vec![error(&format!(
                "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
            ))];
        }

        // Existence first: `verify_password` reports a missing game as untyped
        // prose, and a caller waiting on a pre-minted code keys on the typed code.
        let Some(info) = self.lobby.join_target_info(&game_code) else {
            return vec![Outbound::ToSelf(LobbyServerMessage::error_with_code(
                ServerErrorCode::GameNotFound,
                format!("Game not found in lobby: {game_code}"),
            ))];
        };

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

        if self.hosts_current(conn, &game_code) {
            return vec![error("You are already hosting this game")];
        }

        // --- build-commit + password gates, then snapshot ---
        let guest_commit = conn.hello_build_commit();
        let host_commit = self.lobby.host_build_commit(&game_code).unwrap_or("");
        if let BuildCommitCheck::Reject { host, guest } =
            check_build_commit(host_commit, guest_commit)
        {
            warn!(game = %game_code, %host, %guest, "build mismatch — refusing lookup");
            return vec![error(&format!(
                "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
            ))];
        }

        // Existence first: `verify_password` reports a missing game as untyped
        // prose, and a caller waiting on a pre-minted code keys on the typed code.
        let Some(mut info) = self.lobby.join_target_info(&game_code) else {
            return vec![Outbound::ToSelf(LobbyServerMessage::error_with_code(
                ServerErrorCode::GameNotFound,
                format!("Game not found in lobby: {game_code}"),
            ))];
        };
        match self.lobby.verify_password(&game_code, password.as_deref()) {
            Ok(()) => {}
            Err(e) if e == "password_required" => {
                return vec![Outbound::ToSelf(LobbyServerMessage::PasswordRequired {
                    game_code,
                })];
            }
            Err(e) => {
                warn!(game = %game_code, error = %e, "lookup password verification failed");
                return vec![error(&e)];
            }
        }

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
            draft_metadata: info.draft_metadata,
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
        let Some(registration) = conn
            .host_game
            .as_ref()
            .filter(|r| r.game_code() == game_code)
        else {
            return vec![error("Only the lobby host can update metadata")];
        };
        // This socket's listing was reaped; the code may now name another
        // host's listing, which only that host may update.
        if !self.lobby.is_current(registration) {
            return vec![];
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
        // Take (clearing it so disconnect cleanup doesn't try again) only the
        // stamp for this code; a request naming any other code leaves it held.
        let Some(registration) = conn.host_game.take_if(|r| r.game_code() == game_code) else {
            warn!(game = %game_code, "UnregisterLobby rejected — socket is not the registered host");
            return vec![error(
                "UnregisterLobby only allowed for the host that registered the game",
            )];
        };

        if self.lobby.unregister_registration(&registration) {
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
    ///
    /// THREE `Err` shapes now rather than two: an EXPIRED credential is told
    /// apart from a wrong one, because they are different client-side
    /// situations and only the first is recoverable — by
    /// `RenewTournamentCredential`. The distinction leaks nothing: `Expired` is
    /// reachable only by a caller who already presented the exact stored
    /// secret.
    ///
    /// The comparison itself is [`crate::tournament::TournamentCredential`]'s,
    /// not this function's: it is constant-time and it carries the expiry
    /// conjunct, so no call site here can spell a plaintext `==` or forget the
    /// clock. An empty presented token cannot match — the stored secret is
    /// never empty and the comparison is length-checked first.
    fn authorize_organizer(
        &self,
        code: &str,
        presented: &str,
        env: &impl BrokerEnv,
    ) -> Result<(), String> {
        let meta = self
            .tournaments
            .get(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        match meta.organizer_token.verdict(presented, env.now_ms()) {
            CredentialVerdict::Accepted => Ok(()),
            CredentialVerdict::Expired => Err(format!(
                "Organizer credential for tournament {code} has expired - renew it and retry"
            )),
            CredentialVerdict::Mismatch => {
                Err(format!("Invalid organizer token for tournament {code}"))
            }
        }
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
    /// FOUR distinct `Err` shapes — missing tournament, unusable token,
    /// EXPIRED token, dropped entrant — because they are four different
    /// client-side situations and the caller replies with the message
    /// verbatim. The dropped and expired cases reveal nothing: each is told
    /// only to the holder of that player's own token.
    ///
    /// The comparison is [`crate::tournament::TournamentCredential`]'s, so it
    /// is constant-time and carries the expiry conjunct; see
    /// [`Broker::authorize_organizer`].
    fn authorize_player(
        &self,
        code: &str,
        presented: &str,
        env: &impl BrokerEnv,
    ) -> Result<String, String> {
        let meta = self
            .tournaments
            .get(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        let now_ms = env.now_ms();
        // The scan records an expiry it walked past so a holder of the RIGHT
        // secret is told that it lapsed rather than that it was never valid.
        // It cannot short-circuit on the first expired match, because a
        // rotation may have left an older seat holding a stale copy; only a
        // full pass proves no seat still accepts this secret.
        let mut expired = false;
        let player =
            meta.players
                .iter()
                .find(|p| match p.player_token.verdict(presented, now_ms) {
                    CredentialVerdict::Accepted => true,
                    CredentialVerdict::Expired => {
                        expired = true;
                        false
                    }
                    CredentialVerdict::Mismatch => false,
                });
        let Some(player) = player else {
            return Err(if expired {
                format!("Player credential for tournament {code} has expired - renew it and retry")
            } else {
                format!("Invalid player token for tournament {code}")
            });
        };
        if player.dropped {
            return Err(format!("Player has dropped from tournament {code}"));
        }
        Ok(player.player_key.clone())
    }

    /// The complete persisted snapshot: the whole broker as the ASCII-only JSON
    /// the Durable Object stores ([`crate::snapshot_codec`]). The one producer of
    /// the stored text, so the bytes [`Self::snapshot_encoded_bytes`] measures
    /// are exactly the bytes written. Infallible for the broker's types.
    pub fn snapshot_json(&self) -> String {
        self.stored_snapshot().text
    }

    /// Whether [`Self::snapshot_json`] fits the store: its stored size, key and
    /// V8 framing included, is within the row limit. Always true for a state the
    /// persistence boundary admitted; the shell checks it before the writes that
    /// bypass admission (the reaper and disconnect), so one measure bounds
    /// every `put`.
    pub fn snapshot_fits_storage(&self) -> bool {
        self.snapshot_encoded_bytes() <= PHYSICAL_SNAPSHOT_LIMIT_BYTES
    }

    /// The text to store and its stored size, from the one authority
    /// ([`crate::snapshot_codec::stored_snapshot`]) that picks the ASCII-only
    /// form, or, for a snapshot restored from the pre-codec raw-UTF-8 form whose
    /// escaped size would not fit, the raw form.
    fn stored_snapshot(&self) -> crate::snapshot_codec::StoredSnapshot {
        crate::snapshot_codec::stored_snapshot(self, PHYSICAL_SNAPSHOT_LIMIT_BYTES)
            .expect("broker state always serializes")
    }

    /// The exact byte length of the complete persisted snapshot — the whole
    /// broker encoded as [`Self::snapshot_json`] writes it (ASCII-only, so its
    /// UTF-8 length is also its JS string length and, as a one-byte V8 string,
    /// its stored size). This is the quantity Cloudflare's 8 MB key+value ceiling
    /// applies to, so it is measured directly rather than approximated from any
    /// per-field budget. A hypothetical serialization failure yields
    /// [`usize::MAX`], so the admission check refuses rather than under-counts.
    fn snapshot_encoded_bytes(&self) -> usize {
        crate::snapshot_codec::stored_snapshot(self, PHYSICAL_SNAPSHOT_LIMIT_BYTES)
            .map_or(usize::MAX, |stored| stored.stored_bytes)
    }

    /// The snapshot size plus every reservation not yet used: what the persisted
    /// value is *committed* to occupy. Each tournament contributes
    /// `max(metadata, EVENT_METADATA_RESERVE_ENCODED_BYTES) +
    /// max(decks, EVENT_DECK_RESERVE_ENCODED_BYTES)` (see
    /// [`unused_event_reserve`]) and the lobby
    /// `max(lobby, LOBBY_RESERVE_ENCODED_BYTES)` (the measured snapshot already
    /// holds the used part; this adds the unused remainder). Admitting a write
    /// only while this stays within [`MAX_SNAPSHOT_ENCODED_BYTES`] makes the
    /// reservations an invariant: growth inside an owner's reservation never
    /// raises it, so it is always admitted, and growth by anyone else can never
    /// consume the room another owner is still guaranteed.
    #[cfg(test)]
    fn committed_bytes(&self) -> usize {
        self.accounting().1
    }

    /// `(actual, committed)`: the exact encoded snapshot size and the committed
    /// total built on it, from the existing encoded-size helpers. The one
    /// place both quantities are computed, so no caller re-serializes to learn
    /// the other.
    fn accounting(&self) -> (usize, usize) {
        #[cfg(test)]
        ACCOUNTING_PASSES.with(|passes| passes.set(passes.get() + 1));
        let unused_events = self
            .tournaments
            .iter()
            .map(unused_event_reserve)
            .fold(0usize, usize::saturating_add);
        let unused_lobby = LOBBY_RESERVE_ENCODED_BYTES.saturating_sub(encoded_len(&self.lobby));
        let actual = self.snapshot_encoded_bytes();
        let committed = actual
            .saturating_add(unused_events)
            .saturating_add(unused_lobby);
        (actual, committed)
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_create_tournament(
        &mut self,
        conn: &mut ConnState,
        name: String,
        arity: MatchArity,
        scoring: Option<ScoringPolicy>,
        bracket: BracketShape,
        total_rounds: Option<u32>,
        plus_rounds: Option<u32>,
        format: Option<GameFormat>,
        match_type: Option<MatchType>,
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
        // An omitted `scoring` is resolved HERE, by the broker, from the same
        // `default_for_arity` authority the organizer would otherwise have had
        // to reimplement client-side — and the resolved value goes back out on
        // `TournamentSummary::scoring`, so nobody has to recompute it to see
        // what their event scores. The same shape `total_rounds` already has.
        let scoring = scoring.unwrap_or_else(|| ScoringPolicy::default_for_arity(arity));
        let minted = match self.tournaments.create_tournament(
            &code,
            CreateTournamentRequest {
                name: name.clone(),
                arity,
                scoring,
                bracket,
                total_rounds,
                plus_rounds,
                format,
                match_type,
            },
            env,
        ) {
            Ok(minted) => minted,
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
                organizer_token: minted.secret,
                expires_at_ms: minted.expires_at_ms,
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
        let minted = match self
            .tournaments
            .join_tournament(&code, &player_key, &display_name, env)
        {
            Ok(minted) => minted,
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
                player_token: minted.secret,
                expires_at_ms: minted.expires_at_ms,
                view: view.clone(),
            }),
            Outbound::ToSubscribers(LobbyServerMessage::TournamentUpdate { code, view }),
            self.tournament_list_update(),
        ]
    }

    /// Token-gated, but not one of the four gated ACTIONS: it settles through
    /// its own point reply rather than through [`Broker::settle_gated`],
    /// because it carries no [`TournamentRequestId`] and mutates no tournament
    /// state a subscriber could be watching.
    ///
    /// **Exactly one outbound, and it is `ToSelf`.** The rotated secret must
    /// never be fanned out, and nothing on `TournamentSummary` or
    /// `TournamentView` moved, so a broadcast would be a re-render carrying a
    /// secret for no reason.
    fn handle_renew_tournament_credential(
        &mut self,
        code: String,
        role: TournamentRole,
        token: String,
        rotation_nonce: String,
        env: &impl BrokerEnv,
    ) -> Vec<Outbound> {
        match self
            .tournaments
            .renew_credential(&code, role, &token, &rotation_nonce, env)
        {
            Ok(minted) => {
                // The code and the role, never the secret — the same rule
                // `ConnState`'s tournament bookkeeping already follows.
                info!(tournament = %code, ?role, "tournament credential rotated");
                vec![Outbound::ToSelf(
                    LobbyServerMessage::TournamentCredentialRenewed {
                        code,
                        role,
                        token: minted.secret,
                        expires_at_ms: minted.expires_at_ms,
                    },
                )]
            }
            Err(reason) => {
                warn!(tournament = %code, ?role, "RenewTournamentCredential rejected");
                vec![error(&reason)]
            }
        }
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
        if let Err(reason) = self.authorize_organizer(&code, &organizer_token, env) {
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
        let reporter = match self.authorize_player(&code, &player_token, env) {
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

    /// Player-gated: store the token owner's submitted deck for hosted play.
    /// Resolving the token to its owner ([`Self::authorize_player`]) confines a
    /// submission to the presenting entrant — the key is never taken from the
    /// payload, so a client cannot submit a deck on someone else's behalf. Unlike
    /// [`Self::handle_report_match_result`], no pairing-seat check applies: a deck
    /// is per-event, not per-pairing.
    fn handle_submit_tournament_deck(
        &mut self,
        code: String,
        player_token: String,
        deck: engine::starter_decks::DeckData,
        env: &impl BrokerEnv,
    ) -> Result<GatedEffect, String> {
        let player_key = match self.authorize_player(&code, &player_token, env) {
            Ok(key) => key,
            Err(reason) => {
                warn!(tournament = %code, %reason, "SubmitTournamentDeck rejected — player not authorized");
                return Err(reason);
            }
        };
        // The per-deck and per-event deck gates run inside `submit_deck`, which
        // mutates only on success. The complete-snapshot ceiling and the capacity
        // reservations are not checked here: they apply to every growth path and
        // are enforced once, at the persistence boundary in [`Self::handle`], which
        // restores the prior state if this submission cannot be persisted.
        self.tournaments
            .submit_deck(&code, &player_key, deck, env)?;
        let Some(view) = self.tournament_view(&code) else {
            return Err(format!("Tournament not found: {code}"));
        };
        info!(tournament = %code, player = %player_key, "tournament deck submitted");

        // A submission flips only `PlayerSummary.deck_submitted`, a detail-view
        // field — no `TournamentSummary` field changes, so the list row is
        // untouched, exactly like a result report.
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
        let player_key = match self.authorize_player(&code, &player_token, env) {
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
        if let Err(reason) = self.authorize_organizer(&code, &organizer_token, env) {
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

/// The part of one event's guarantee its record has not yet used: the unused
/// metadata allowance plus the unused deck allowance, tracked as two separate
/// accounts. The deck account is charged only by this event's retained decks
/// ([`TournamentMeta::retained_deck_bytes`]); everything else the record encodes
/// is metadata. Metadata growth therefore never eats the deck account, and one
/// event's decks never touch another event's allowance.
fn unused_event_reserve(meta: &TournamentMeta) -> usize {
    let deck_bytes = meta.retained_deck_bytes();
    let metadata_bytes = encoded_len(meta).saturating_sub(deck_bytes);
    EVENT_METADATA_RESERVE_ENCODED_BYTES
        .saturating_sub(metadata_bytes)
        .saturating_add(EVENT_DECK_RESERVE_ENCODED_BYTES.saturating_sub(deck_bytes))
}

/// The state a growth-capable frame may change, captured before it runs so a
/// refusal at the persistence boundary can put it back ([`Broker::handle`]).
struct Checkpoint {
    lobby: LobbyManager,
    tournaments: TournamentManager,
    conn: ConnState,
}

// Whole-broker accounting passes run on this thread, so a test can show that a
// refused no-op request performed none. Test-only instrumentation.
#[cfg(test)]
thread_local! {
    static ACCOUNTING_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How a client frame can change the persisted broker. Exhaustive so a new
/// frame must be classified deliberately: `MayGrow` routes it through the
/// capacity boundary. Pure reads are `None`; an unregister only deletes
/// (`Removes`); everything that registers, joins, reserves, rotates a credential
/// or records tournament progress is `MayGrow`.
fn persisted_effect(msg: &LobbyClientMessage) -> PersistedEffect {
    match msg {
        LobbyClientMessage::CreateGameWithSettings { .. }
        | LobbyClientMessage::JoinGameWithPassword { .. }
        | LobbyClientMessage::LookupJoinTarget { .. }
        | LobbyClientMessage::UpdateLobbyMetadata { .. }
        | LobbyClientMessage::CreateTournament { .. }
        | LobbyClientMessage::JoinTournament { .. }
        | LobbyClientMessage::RenewTournamentCredential { .. }
        | LobbyClientMessage::StartTournamentRound { .. }
        | LobbyClientMessage::ReportMatchResult { .. }
        | LobbyClientMessage::SubmitTournamentDeck { .. }
        | LobbyClientMessage::DropFromTournament { .. }
        | LobbyClientMessage::EndTournament { .. } => PersistedEffect::MayGrow,
        LobbyClientMessage::UnregisterLobby { .. } => PersistedEffect::Removes,
        LobbyClientMessage::ClientHello { .. }
        | LobbyClientMessage::SubscribeLobby
        | LobbyClientMessage::UnsubscribeLobby
        | LobbyClientMessage::Ping { .. }
        | LobbyClientMessage::GetTournament { .. } => PersistedEffect::None,
    }
}

/// What a frame can do to the persisted broker; see [`persisted_effect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistedEffect {
    None,
    Removes,
    MayGrow,
}

/// Whether handling a frame changed the state a persisting shell must store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistedChange {
    /// Nothing durable changed: a read, a stateless reply, or a request refused
    /// without effect (including one the persistence boundary restored). The
    /// shell skips its snapshot write.
    Unchanged,
    /// The frame was applied to durable state, so the shell must snapshot it.
    Changed,
}

/// The result of [`Broker::handle_reporting`]: the shell's side effects and
/// whether durable state changed.
#[derive(Debug)]
pub struct Handled {
    pub outbounds: Vec<Outbound>,
    pub persisted: PersistedChange,
}

impl Handled {
    fn unchanged(outbounds: Vec<Outbound>) -> Self {
        Self {
            outbounds,
            persisted: PersistedChange::Unchanged,
        }
    }

    fn changed(outbounds: Vec<Outbound>) -> Self {
        Self {
            outbounds,
            persisted: PersistedChange::Changed,
        }
    }
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
    use crate::protocol::{DraftLobbyMetadata, LobbyClientMessage, PROTOCOL_VERSION};
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
        hello_as(conn, broker, env, "abc");
    }

    fn hello_as(conn: &mut ConnState, broker: &mut Broker, env: &FakeEnv, build: &str) {
        broker.handle(
            conn,
            LobbyClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: build.into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version: Some(crate::protocol::LOBBY_PROTOCOL_VERSION),
            },
            env,
        );
    }

    fn create(conn: &mut ConnState, broker: &mut Broker, env: &FakeEnv) -> Vec<Outbound> {
        create_requesting(conn, broker, env, None)
    }

    fn create_requesting(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
        requested: Option<&str>,
    ) -> Vec<Outbound> {
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
                requested_code: requested.map(str::to_string),
            },
            env,
        )
    }

    /// The single `Error` reply in `out`, as `(message, code)`.
    fn only_error(out: &[Outbound]) -> (String, Option<ServerErrorCode>) {
        match out {
            [Outbound::ToSelf(LobbyServerMessage::Error { message, code })] => {
                (message.clone(), *code)
            }
            other => panic!("expected exactly one Error, got {other:?}"),
        }
    }

    #[test]
    fn create_honors_a_requested_code() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);

        let out = create_requesting(&mut conn, &mut broker, &env, Some("BOTAB1"));

        // `FakeEnv` would have minted `CODE00`.
        assert_eq!(game_code_of(&out), "BOTAB1");
        assert_eq!(
            conn.host_game.as_ref().map(LobbyRegistration::game_code),
            Some("BOTAB1")
        );
        assert!(broker.lobby().has_game("BOTAB1"));
    }

    #[test]
    fn a_requested_code_held_by_another_socket_is_code_in_use() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut a = ConnState::default();
        let mut b = ConnState::default();
        hello(&mut a, &mut broker, &env);
        hello(&mut b, &mut broker, &env);
        assert_eq!(
            game_code_of(&create_requesting(
                &mut a,
                &mut broker,
                &env,
                Some("BOTAB1")
            )),
            "BOTAB1"
        );

        let out = create_requesting(&mut b, &mut broker, &env, Some("BOTAB1"));

        let (message, code) = only_error(&out);
        assert_eq!(code, Some(ServerErrorCode::CodeInUse));
        let lower = message.to_lowercase();
        for legacy in ["not found", "full", "password", "build mismatch"] {
            assert!(
                !lower.contains(legacy),
                "legacy classifiers must not match {legacy:?}: {message}"
            );
        }
        assert_eq!(b.host_game, None);
        assert!(
            broker.lobby().public_game("BOTAB1").is_some(),
            "the holder's listing survives the refused claim"
        );
    }

    /// Socket A registers `BOTAB1`, the listing is reaped by age while A's
    /// socket stays open, and socket B claims the freed code. A still holds
    /// its (now stale) host stamp for `BOTAB1`.
    fn reaped_then_reclaimed() -> (FakeEnv, Broker, ConnState, ConnState) {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut a = ConnState::default();
        let mut b = ConnState::default();
        hello(&mut a, &mut broker, &env);
        hello(&mut b, &mut broker, &env);
        create_requesting(&mut a, &mut broker, &env, Some("BOTAB1"));

        env.advance_secs(301);
        let reaped = broker.reap_expired(300, &env);
        assert!(
            removes(&reaped, "BOTAB1"),
            "reach guard: the expiry path reaped A's listing"
        );
        assert!(a.host_game.is_some(), "A's socket still carries its stamp");

        let claimed = create_requesting(&mut b, &mut broker, &env, Some("BOTAB1"));
        assert_eq!(game_code_of(&claimed), "BOTAB1", "B claimed the freed code");
        (env, broker, a, b)
    }

    fn removes(out: &[Outbound], code: &str) -> bool {
        out.iter().any(|o| {
            matches!(
                o,
                Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved { game_code })
                    if game_code == code
            )
        })
    }

    /// B's listing is still the one under `BOTAB1`, unmodified.
    fn assert_new_holder_untouched(broker: &Broker, b: &ConnState) {
        let registration = b.host_game.as_ref().expect("B holds BOTAB1");
        assert!(
            broker.lobby().is_current(registration),
            "B's registration is still the one listed under BOTAB1"
        );
        let game = broker.lobby().public_game("BOTAB1").expect("B's listing");
        assert_eq!(game.current_players, 1, "B's listing was not updated by A");
    }

    #[test]
    fn a_reaped_hosts_disconnect_leaves_the_code_reclaimer_listed() {
        let (_env, mut broker, mut a, b) = reaped_then_reclaimed();

        let out = broker.on_disconnect(&mut a);

        assert!(
            !removes(&out, "BOTAB1"),
            "no removal broadcast for B's code"
        );
        assert_new_holder_untouched(&broker, &b);
    }

    #[test]
    fn a_reaped_hosts_re_create_leaves_the_code_reclaimer_listed() {
        let (env, mut broker, mut a, b) = reaped_then_reclaimed();

        let out = create_requesting(&mut a, &mut broker, &env, None);

        assert_eq!(game_code_of(&out), "CODE00", "reach guard: A re-created");
        assert!(
            !removes(&out, "BOTAB1"),
            "no removal broadcast for B's code"
        );
        assert_new_holder_untouched(&broker, &b);
    }

    #[test]
    fn a_reaped_hosts_unregister_leaves_the_code_reclaimer_listed() {
        let (env, mut broker, mut a, b) = reaped_then_reclaimed();

        let out = broker.handle(
            &mut a,
            LobbyClientMessage::UnregisterLobby {
                game_code: "BOTAB1".into(),
            },
            &env,
        );

        assert!(out.is_empty(), "nothing to remove or report: {out:?}");
        assert_eq!(a.host_game, None, "A's stale stamp is released");
        assert_new_holder_untouched(&broker, &b);
    }

    #[test]
    fn a_reaped_hosts_metadata_update_leaves_the_code_reclaimer_untouched() {
        let (env, mut broker, mut a, b) = reaped_then_reclaimed();

        let out = broker.handle(
            &mut a,
            LobbyClientMessage::UpdateLobbyMetadata {
                game_code: "BOTAB1".into(),
                current_players: 3,
                max_players: 3,
                consumed_reservation_tokens: vec![],
            },
            &env,
        );

        assert!(
            out.is_empty(),
            "no update broadcast for B's listing: {out:?}"
        );
        assert_new_holder_untouched(&broker, &b);
        assert_eq!(
            broker.lobby().public_game("BOTAB1").map(|g| g.max_players),
            Some(4),
            "B's seat count was not rewritten by A"
        );
    }

    /// A's stale stamp no longer makes `BOTAB1` "the game A is hosting", so A
    /// may look up B's game like any other guest.
    #[test]
    fn a_reaped_host_may_look_up_the_code_reclaimers_game() {
        let (env, mut broker, mut a, _b) = reaped_then_reclaimed();

        let out = broker.handle(
            &mut a,
            LobbyClientMessage::LookupJoinTarget {
                game_code: "BOTAB1".into(),
                password: None,
                reserve: false,
                display_name: None,
                release_reservation_token: None,
            },
            &env,
        );

        assert!(
            out.iter().any(|o| matches!(
                o,
                Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo { .. })
            )),
            "A is answered as a guest of B's game: {out:?}"
        );
    }

    fn ping(conn: &mut ConnState, broker: &mut Broker, env: &FakeEnv) -> Vec<Outbound> {
        broker.handle(conn, LobbyClientMessage::Ping { timestamp: 0 }, env)
    }

    /// A connected host that keeps pinging is never reaped, while a silent
    /// host's listing still is — and a guest's pings refresh nothing.
    #[test]
    fn a_pinging_hosts_listing_outlives_the_reap_timeout() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut a = ConnState::default();
        let mut c = ConnState::default();
        let mut g = ConnState::default();
        hello(&mut a, &mut broker, &env);
        hello(&mut c, &mut broker, &env);
        hello(&mut g, &mut broker, &env);
        assert_eq!(
            game_code_of(&create_requesting(
                &mut a,
                &mut broker,
                &env,
                Some("LFGA01")
            )),
            "LFGA01"
        );
        assert_eq!(
            game_code_of(&create_requesting(
                &mut c,
                &mut broker,
                &env,
                Some("LFGC01")
            )),
            "LFGC01"
        );

        let mut silent_reaped_after = None;
        for minute in 1..=10u64 {
            env.advance_secs(60);
            ping(&mut a, &mut broker, &env);
            assert!(
                !broker.host_liveness_refresh_due(&g, &env),
                "a guest holds no stamp, so its frames are never a liveness write"
            );
            ping(&mut g, &mut broker, &env);
            let swept = broker.reap_expired(300, &env);
            assert!(
                !removes(&swept, "LFGA01"),
                "the pinging host's listing was reaped after {minute} min"
            );
            if removes(&swept, "LFGC01") {
                silent_reaped_after.get_or_insert(minute * 60);
            }
        }
        assert_eq!(
            silent_reaped_after,
            Some(360),
            "reach guard: the silent host's listing lapsed on the first sweep past 300 s"
        );
        assert!(
            lookup(&mut g, &mut broker, &env, "LFGA01", None)
                .iter()
                .any(|o| matches!(
                    o,
                    Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo { .. })
                )),
            "the pinging host's room is still joinable after 10 minutes"
        );

        // Abandonment is still reaped: once A falls silent, its listing lapses
        // 300 s after its last refresh.
        env.advance_secs(300);
        assert!(!removes(&broker.reap_expired(300, &env), "LFGA01"));
        env.advance_secs(1);
        assert!(removes(&broker.reap_expired(300, &env), "LFGA01"));
    }

    /// A's stamp outlived its listing and B reclaimed the code: A's pings must
    /// not keep B's listing alive.
    #[test]
    fn a_reaped_hosts_ping_does_not_refresh_the_code_reclaimer() {
        let (env, mut broker, mut a, b) = reaped_then_reclaimed();

        for _ in 0..5 {
            env.advance_secs(60);
            ping(&mut a, &mut broker, &env);
        }
        env.advance_secs(1);
        assert!(
            !broker.host_liveness_refresh_due(&a, &env),
            "A's stale stamp names no current listing"
        );
        assert!(
            broker.host_liveness_refresh_due(&b, &env),
            "reach: B's listing is due a refresh it never received"
        );
        assert!(
            removes(&broker.reap_expired(300, &env), "BOTAB1"),
            "B's silent listing lapses 301 s after its registration"
        );
    }

    /// Positive arm: while A's own registration is still listed, disconnect and
    /// `UnregisterLobby` remove it and announce the removal.
    #[test]
    fn a_live_hosts_disconnect_and_unregister_remove_its_own_listing() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut a = ConnState::default();
        hello(&mut a, &mut broker, &env);
        create_requesting(&mut a, &mut broker, &env, Some("BOTAB1"));
        let out = broker.on_disconnect(&mut a);
        assert!(removes(&out, "BOTAB1"));
        assert!(!broker.lobby().has_game("BOTAB1"));

        let mut c = ConnState::default();
        hello(&mut c, &mut broker, &env);
        create_requesting(&mut c, &mut broker, &env, Some("BOTAB2"));
        let out = broker.handle(
            &mut c,
            LobbyClientMessage::UnregisterLobby {
                game_code: "BOTAB2".into(),
            },
            &env,
        );
        assert!(removes(&out, "BOTAB2"));
        assert!(!broker.lobby().has_game("BOTAB2"));
        assert_eq!(c.host_game, None);
    }

    #[test]
    fn the_same_socket_can_re_create_its_own_requested_code() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);
        create_requesting(&mut conn, &mut broker, &env, Some("BOTAB1"));

        let out = create_requesting(&mut conn, &mut broker, &env, Some("BOTAB1"));

        // The claim runs after the same-socket cleanup, so the socket's own
        // listing does not hold the code against it.
        assert!(
            matches!(
                out.as_slice(),
                [
                    Outbound::ToSubscribers(LobbyServerMessage::LobbyGameRemoved { game_code: removed }),
                    Outbound::ToSelf(LobbyServerMessage::GameCreated { game_code: created, .. }),
                    Outbound::ToSubscribers(LobbyServerMessage::LobbyGameAdded { .. }),
                ] if removed == "BOTAB1" && created == "BOTAB1"
            ),
            "got {out:?}"
        );
    }

    #[test]
    fn a_malformed_requested_code_is_rejected_before_the_handler() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        hello(&mut conn, &mut broker, &env);

        let out = create_requesting(&mut conn, &mut broker, &env, Some("bad"));

        let (message, code) = only_error(&out);
        assert_eq!(code, None);
        assert!(message.contains("requested_code"), "{message}");
        assert_eq!(broker.lobby().len(), 0);
    }

    fn lookup(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
        code: &str,
        password: Option<&str>,
    ) -> Vec<Outbound> {
        broker.handle(
            conn,
            LobbyClientMessage::LookupJoinTarget {
                game_code: code.into(),
                password: password.map(str::to_string),
                reserve: false,
                display_name: None,
                release_reservation_token: None,
            },
            env,
        )
    }

    fn join(conn: &mut ConnState, broker: &mut Broker, env: &FakeEnv, code: &str) -> Vec<Outbound> {
        broker.handle(
            conn,
            LobbyClientMessage::JoinGameWithPassword {
                game_code: code.into(),
                deck: test_deck(),
                display_name: "Guest".into(),
                password: None,
                reservation_token: None,
            },
            env,
        )
    }

    #[test]
    fn lookup_of_an_unknown_code_is_game_not_found() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        let mut guest = ConnState::default();
        hello(&mut host, &mut broker, &env);
        hello(&mut guest, &mut broker, &env);
        let code = game_code_of(&create(&mut host, &mut broker, &env));

        let (message, error_code) =
            only_error(&lookup(&mut guest, &mut broker, &env, "NOPE01", None));
        assert_eq!(error_code, Some(ServerErrorCode::GameNotFound));
        assert!(message.contains("not found"), "{message}");

        // Positive: a listed code resolves.
        assert!(
            lookup(&mut guest, &mut broker, &env, &code, None)
                .iter()
                .any(|o| matches!(
                    o,
                    Outbound::ToSelf(LobbyServerMessage::JoinTargetInfo { .. })
                )),
            "a created game resolves"
        );

        // Sibling: a wrong password on a listed game stays uncoded.
        let mut locked_host = ConnState::default();
        hello(&mut locked_host, &mut broker, &env);
        let locked = game_code_of(&broker.handle(
            &mut locked_host,
            LobbyClientMessage::CreateGameWithSettings {
                deck: test_deck(),
                display_name: "Host".into(),
                public: true,
                password: Some("secret".into()),
                timer_seconds: None,
                player_count: 4,
                match_config: Default::default(),
                format_config: None,
                room_name: None,
                host_peer_id: Some("peer-2".into()),
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
                requested_code: None,
            },
            &env,
        ));
        let (_, wrong_password_code) = only_error(&lookup(
            &mut guest,
            &mut broker,
            &env,
            &locked,
            Some("wrong"),
        ));
        assert_eq!(wrong_password_code, None);
    }

    #[test]
    fn lookup_reports_the_listing_draft_metadata() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        let mut guest = ConnState::default();
        hello(&mut host, &mut broker, &env);
        hello(&mut guest, &mut broker, &env);

        let code = game_code_of(&broker.handle(
            &mut host,
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
                draft_metadata: Some(DraftLobbyMetadata {
                    set_code: "MKM".into(),
                    draft_kind: "Premier".into(),
                    cube_name: None,
                }),
                start_when_full: true,
                ranked: false,
                requested_code: None,
            },
            &env,
        ));

        let out = lookup(&mut guest, &mut broker, &env, &code, None);
        let [Outbound::ToSelf(msg)] = out.as_slice() else {
            panic!("expected exactly one ToSelf reply, got {out:?}");
        };
        let value = serde_json::to_value(msg).expect("message serializes");
        assert_eq!(value["type"], "JoinTargetInfo", "reach guard");
        assert_eq!(value["data"]["is_p2p"], true);
        assert_eq!(value["data"]["draft_metadata"]["setCode"], "MKM");
        assert_eq!(value["data"]["draft_metadata"]["draftKind"], "Premier");
    }

    #[test]
    fn lookup_omits_draft_metadata_for_a_constructed_game() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        let mut guest = ConnState::default();
        hello(&mut host, &mut broker, &env);
        hello(&mut guest, &mut broker, &env);
        let code = game_code_of(&create(&mut host, &mut broker, &env));

        let out = lookup(&mut guest, &mut broker, &env, &code, None);
        let [Outbound::ToSelf(msg)] = out.as_slice() else {
            panic!("expected exactly one ToSelf reply, got {out:?}");
        };
        let value = serde_json::to_value(msg).expect("message serializes");
        assert_eq!(value["type"], "JoinTargetInfo", "reach guard");
        assert!(
            value["data"].get("draft_metadata").is_none(),
            "skip_serializing_if omits the key for a constructed game: {value}"
        );
    }

    #[test]
    fn join_of_an_unknown_code_is_game_not_found() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut host = ConnState::default();
        let mut guest = ConnState::default();
        hello(&mut host, &mut broker, &env);
        hello(&mut guest, &mut broker, &env);
        let code = game_code_of(&create(&mut host, &mut broker, &env));

        let (message, error_code) = only_error(&join(&mut guest, &mut broker, &env, "NOPE01"));
        assert_eq!(error_code, Some(ServerErrorCode::GameNotFound));
        assert!(message.contains("not found"), "{message}");

        // Positive: joining a listed P2P game reaches PeerInfo.
        assert!(
            matches!(
                join(&mut guest, &mut broker, &env, &code).as_slice(),
                [Outbound::ToSelf(LobbyServerMessage::PeerInfo { .. })]
            ),
            "a created P2P game is joinable"
        );
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
        assert_eq!(
            conn.host_game.as_ref().map(LobbyRegistration::game_code),
            Some("CODE00")
        );
    }

    fn hello_declaring(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
        lobby_protocol_version: Option<u32>,
    ) {
        broker.handle(
            conn,
            LobbyClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: "abc".into(),
                protocol_version: PROTOCOL_VERSION,
                lobby_protocol_version,
            },
            env,
        );
    }

    /// A two-seat settings-create frame as the client sends it, with `edit`
    /// applied to its serialized `format_config`.
    fn create_frame(
        config: engine::types::format::FormatConfig,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> String {
        let mut frame = serde_json::to_value(LobbyClientMessage::CreateGameWithSettings {
            deck: test_deck(),
            display_name: "Host".into(),
            public: true,
            password: None,
            timer_seconds: None,
            player_count: 2,
            match_config: Default::default(),
            format_config: Some(config),
            room_name: None,
            host_peer_id: Some("peer-1".into()),
            draft_metadata: None,
            start_when_full: true,
            ranked: false,
            requested_code: None,
        })
        .unwrap();
        edit(&mut frame["data"]["format_config"]);
        frame.to_string()
    }

    fn parsed(frame: &str) -> LobbyClientMessage {
        match crate::protocol::parse_lobby_client_message(frame) {
            crate::protocol::ParsedFrame::Message(msg) => *msg,
            other => panic!("expected a parsed message, got {other:?}"),
        }
    }

    /// A Custom config under `Enabled { threshold, Standard }`.
    fn custom_command_zone(
        commander_damage_threshold: Option<u8>,
    ) -> engine::types::format::FormatConfig {
        use engine::types::custom_format::{
            old_school_93_94, CommandZoneMode, CommanderEligibilityRule,
        };
        let mut rules = old_school_93_94().rules;
        rules.structural.command_zone_mode = CommandZoneMode::Enabled {
            commander_damage_threshold,
            eligibility_rule: CommanderEligibilityRule::Standard,
        };
        engine::types::format::FormatConfig::for_custom_rules(&rules)
    }

    fn legacy_threshold(config: &mut serde_json::Value) {
        config["commander_damage_threshold"] = 21.into();
    }

    fn peer_format_config(out: &[Outbound]) -> engine::types::format::FormatConfig {
        match out {
            [Outbound::ToSelf(LobbyServerMessage::PeerInfo {
                format_config: Some(config),
                ..
            })] => config.clone(),
            other => panic!("expected PeerInfo with a format config, got {other:?}"),
        }
    }

    /// CR 903.12h: a pre-change client's Brawl config carries CR 704.6c's 21. The
    /// lobby keeps what that client's engine wrote, through a snapshot restore, and
    /// relays it to the same build.
    #[test]
    fn legacy_client_registers_a_relayed_brawl_room() {
        use engine::types::format::FormatConfig;
        type Row = (FormatConfig, fn(&mut serde_json::Value), fn(&FormatConfig));
        let rows: [Row; 4] = [
            (FormatConfig::brawl(), legacy_threshold, |relayed| {
                assert_eq!(relayed.commander_damage_threshold, Some(21));
            }),
            (
                FormatConfig::historic_brawl(),
                legacy_threshold,
                |relayed| {
                    assert_eq!(relayed.commander_damage_threshold, Some(21));
                },
            ),
            (
                custom_command_zone(None),
                |config| config["uses_commander"] = false.into(),
                |relayed| assert!(!relayed.uses_commander),
            ),
            (
                FormatConfig::brawl(),
                |_| {},
                |relayed| {
                    assert_eq!(relayed.commander_damage_threshold, None);
                },
            ),
        ];
        for (config, edit, check) in rows {
            let format = config.format;
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut host = ConnState::default();
            hello_declaring(
                &mut host,
                &mut broker,
                &env,
                Some(MIN_LOBBY_PROTOCOL_FOR_COMMANDER_CONFIG_RULES - 1),
            );
            let out = broker.handle(&mut host, parsed(&create_frame(config, edit)), &env);
            let code = game_code_of(&out);

            let snapshot = serde_json::to_string(&broker).unwrap();
            let mut broker: Broker = serde_json::from_str(&snapshot).expect("snapshot restores");
            assert_eq!(snapshot_codes(&mut broker, &env, "abc"), vec![code.clone()]);

            let mut guest = ConnState::default();
            hello(&mut guest, &mut broker, &env);
            let relayed = peer_format_config(&join(&mut guest, &mut broker, &env, &code));
            assert_eq!(relayed.format, format);
            check(&relayed);
        }
    }

    /// The relaxed decode admits only what an earlier engine could have written.
    #[test]
    fn lobby_decode_still_refuses_configs_no_engine_wrote() {
        let zero = create_frame(engine::types::format::FormatConfig::brawl(), |config| {
            config["commander_damage_threshold"] = 0.into();
        });
        let forged = create_frame(custom_command_zone(Some(21)), |config| {
            config["uses_commander"] = false.into();
        });
        for frame in [zero, forged] {
            assert!(
                matches!(
                    crate::protocol::parse_lobby_client_message(&frame),
                    crate::protocol::ParsedFrame::Malformed(_)
                ),
                "{frame}"
            );
        }
        parsed(&create_frame(custom_command_zone(Some(21)), |_| {}));
    }

    /// A client declaring the current lobby version sends only what today's
    /// validator admits; earlier or undeclared versions keep the relay.
    #[test]
    fn current_client_legacy_brawl_room_is_refused() {
        use engine::types::format::FormatConfig;
        for config in [FormatConfig::brawl(), FormatConfig::historic_brawl()] {
            let frame = create_frame(config.clone(), legacy_threshold);

            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut host = ConnState::default();
            hello_declaring(
                &mut host,
                &mut broker,
                &env,
                Some(MIN_LOBBY_PROTOCOL_FOR_COMMANDER_CONFIG_RULES),
            );
            let out = broker.handle(&mut host, parsed(&frame), &env);
            let (message, _) = only_error(&out);
            assert!(message.contains("commander_damage_threshold"), "{message}");
            assert!(broker.lobby.public_games().is_empty());
            assert!(host.host_game.is_none());

            let current = create_frame(config, |_| {});
            game_code_of(&broker.handle(&mut host, parsed(&current), &env));

            for declared in [
                Some(MIN_LOBBY_PROTOCOL_FOR_COMMANDER_CONFIG_RULES - 1),
                None,
            ] {
                let mut broker = Broker::new();
                let mut host = ConnState::default();
                hello_declaring(&mut host, &mut broker, &env, declared);
                game_code_of(&broker.handle(&mut host, parsed(&frame), &env));
            }
        }
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
        assert_ne!(
            conn.host_game.as_ref().map(LobbyRegistration::game_code),
            Some(first_code.as_str())
        );
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
                requested_code: None,
            },
            &env,
        );

        assert!(matches!(
            out.as_slice(),
            [Outbound::ToSelf(LobbyServerMessage::Error { .. })]
        ));
        assert_eq!(
            conn.host_game.as_ref().map(LobbyRegistration::game_code),
            Some(first_code.as_str())
        );
        assert!(broker.lobby_mut().public_game(&first_code).is_some());
    }

    /// The sorted codes of the `LobbyUpdate` snapshot a subscriber on `build` is sent.
    fn snapshot_codes(broker: &mut Broker, env: &FakeEnv, build: &str) -> Vec<String> {
        let mut conn = ConnState::default();
        hello_as(&mut conn, broker, env, build);
        let out = broker.handle(&mut conn, LobbyClientMessage::SubscribeLobby, env);
        let mut codes: Vec<String> = out
            .iter()
            .find_map(|o| match o {
                Outbound::ToSelf(LobbyServerMessage::LobbyUpdate { games }) => {
                    Some(games.iter().map(|g| g.game_code.clone()).collect())
                }
                _ => None,
            })
            .expect("LobbyUpdate present");
        codes.sort();
        codes
    }

    #[test]
    fn subscribe_snapshot_lists_only_rows_the_subscriber_can_join() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut codes = Vec::new();
        for build in ["abc", "xyz", ""] {
            let mut host = ConnState::default();
            hello_as(&mut host, &mut broker, &env, build);
            codes.push(game_code_of(&create(&mut host, &mut broker, &env)));
        }
        let (abc, xyz, empty) = (codes[0].clone(), codes[1].clone(), codes[2].clone());

        assert_eq!(
            snapshot_codes(&mut broker, &env, ""),
            [abc.clone(), xyz, empty.clone()]
        );
        assert_eq!(snapshot_codes(&mut broker, &env, "abc"), [abc, empty]);
    }

    fn row(build: &str) -> LobbyGame {
        LobbyGame {
            game_code: "ROW1".into(),
            host_name: "Host".into(),
            created_at: 0,
            has_password: false,
            host_version: String::new(),
            host_build_commit: build.into(),
            current_players: 1,
            max_players: 2,
            format: None,
            room_name: None,
            is_p2p: true,
            is_sandbox: false,
            is_ranked: false,
            draft_metadata: None,
        }
    }

    #[test]
    fn lobby_row_visibility_is_the_join_gate() {
        assert!(lobby_row_visible_to(&row("abc"), "abc"));
        assert!(!lobby_row_visible_to(&row("abc"), "xyz"));
        assert!(lobby_row_visible_to(&row(""), "xyz"));
        assert!(lobby_row_visible_to(&row("abc"), ""));
    }

    #[test]
    fn row_delivery_withholds_added_and_retracts_updated() {
        let game = row("abc");
        assert_eq!(
            row_delivery(RowDelta::Added, &game, "abc"),
            RowDelivery::Deliver
        );
        assert_eq!(
            row_delivery(RowDelta::Updated, &game, "abc"),
            RowDelivery::Deliver
        );
        assert_eq!(
            row_delivery(RowDelta::Added, &game, "xyz"),
            RowDelivery::Withhold
        );
        assert_eq!(
            row_delivery(RowDelta::Updated, &game, "xyz"),
            RowDelivery::Retract
        );
    }

    #[test]
    fn lobby_frame_for_viewer_projects_only_row_frames() {
        let added = LobbyServerMessage::LobbyGameAdded { game: row("abc") };
        let updated = LobbyServerMessage::LobbyGameUpdated { game: row("abc") };
        let removed = LobbyServerMessage::LobbyGameRemoved {
            game_code: "ROW1".into(),
        };

        assert_eq!(lobby_frame_for_viewer(&added, "abc"), Some(added.clone()));
        assert_eq!(
            lobby_frame_for_viewer(&updated, "abc"),
            Some(updated.clone())
        );
        assert_eq!(lobby_frame_for_viewer(&added, "xyz"), None);
        assert_eq!(
            lobby_frame_for_viewer(&updated, "xyz"),
            Some(removed.clone())
        );
        for other in [
            removed,
            LobbyServerMessage::PlayerCount { count: 3 },
            LobbyServerMessage::TournamentListUpdate {
                tournaments: vec![],
            },
        ] {
            assert_eq!(lobby_frame_for_viewer(&other, "xyz"), Some(other.clone()));
        }
    }

    #[test]
    fn lobby_frame_json_for_viewer_round_trips_frames() {
        let added = LobbyServerMessage::LobbyGameAdded { game: row("abc") };
        let updated = LobbyServerMessage::LobbyGameUpdated { game: row("abc") };
        let parse = |json: Option<String>| -> LobbyServerMessage {
            serde_json::from_str(&json.expect("a frame")).expect("a lobby frame")
        };

        let json = serde_json::to_string(&added).unwrap();
        assert_eq!(parse(lobby_frame_json_for_viewer(&json, "abc")), added);
        let json = serde_json::to_string(&updated).unwrap();
        assert_eq!(
            parse(lobby_frame_json_for_viewer(&json, "xyz")),
            LobbyServerMessage::LobbyGameRemoved {
                game_code: "ROW1".into()
            }
        );
        assert_eq!(
            lobby_frame_json_for_viewer("not json", "xyz").as_deref(),
            Some("not json")
        );
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
                requested_code: None,
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
                requested_code: None,
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
                requested_code: None,
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

    #[test]
    fn stateless_reply_answers_an_unhosted_ping_and_nothing_else() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        // A connection that hosts no listing is answered without the broker.
        let guest = ConnState::default();
        assert_eq!(
            Broker::stateless_reply(&guest, &LobbyClientMessage::Ping { timestamp: 9 }),
            Some(LobbyServerMessage::Pong { timestamp: 9 })
        );
        // The reply is the one `handle` produces for the same frame.
        let mut guest_conn = ConnState::default();
        assert_eq!(
            broker.handle(
                &mut guest_conn,
                LobbyClientMessage::Ping { timestamp: 9 },
                &env
            ),
            [Outbound::ToSelf(LobbyServerMessage::Pong { timestamp: 9 })]
        );

        // A hosting connection's Ping must reach `handle` to refresh liveness.
        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        create(&mut host, &mut broker, &env);
        assert!(host.host_game.is_some());
        assert_eq!(
            Broker::stateless_reply(&host, &LobbyClientMessage::Ping { timestamp: 9 }),
            None
        );

        // Any other frame needs the broker, hosting or not.
        assert_eq!(
            Broker::stateless_reply(&guest, &LobbyClientMessage::SubscribeLobby),
            None
        );
        assert_eq!(
            Broker::stateless_reply(&host, &LobbyClientMessage::SubscribeLobby),
            None
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
        BracketShape, MatchArity, PairingOutcome, PodOutcome, ScoringPolicy, TournamentAction,
        TournamentStatus, IN_PROGRESS_ABANDON_SECS, REGISTRATION_TIMEOUT_SECS,
        TOURNAMENT_CREDENTIAL_TTL_MS,
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
                scoring: Some(ScoringPolicy::default()),
                bracket,
                total_rounds: None,
                plus_rounds: None,
                format: None,
                match_type: None,
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
                scoring: Some(ScoringPolicy::default()),
                bracket: BracketShape::Swiss,
                total_rounds: None,
                plus_rounds: None,
                format: None,
                match_type: None,
            },
            &env,
        );

        let (code, organizer_token) = match &out[0] {
            Outbound::ToSelf(LobbyServerMessage::TournamentCreated {
                code,
                organizer_token,
                expires_at_ms: _,
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
                scoring: Some(ScoringPolicy::default_for_arity(arity)),
                bracket: BracketShape::Swiss,
                total_rounds: None,
                plus_rounds: None,
                format: None,
                match_type: None,
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

    /// Drives submission through `Broker::handle`, proving its dispatch,
    /// token-to-entrant authority, private storage, and correlated settlement.
    #[test]
    fn submit_tournament_deck_via_broker_handle_stores_privately_and_correlates_outcomes() {
        const SUBMISSION_ID: TournamentRequestId = TournamentRequestId(51);
        const REFUSAL_ID: TournamentRequestId = TournamentRequestId(52);
        const PRIVATE_CARD: &str = "Private Submission Card Sentinel";

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();
        let mut alice = ConnState::default();
        let mut bob = ConnState::default();

        let (code, _organizer_token) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        let alice_token = join_tournament(&mut alice, &mut broker, &env, &code, "key-a", "Alice");
        join_tournament(&mut bob, &mut broker, &env, &code, "key-b", "Bob");

        let submitted_deck = engine::starter_decks::DeckData {
            main_deck: vec![PRIVATE_CARD.into()],
            ..Default::default()
        };
        let submitted = broker.handle(
            &mut alice,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.clone(),
                player_token: alice_token.clone(),
                deck: submitted_deck.clone(),
                request_id: Some(SUBMISSION_ID),
            },
            &env,
        );

        let (ack_id, ack_code, ack_view) = ack_of(&submitted);
        assert_eq!(ack_id, SUBMISSION_ID);
        assert_eq!(ack_code, code);
        assert_eq!(correlators(&submitted), vec![SUBMISSION_ID]);
        let public_view = subscriber_update_view(&submitted);
        assert_eq!(ack_view, public_view);
        assert_eq!(subscriber_msgs(&submitted).len(), 1);
        assert!(!has_list_update(&submitted));
        assert!(
            public_view
                .players
                .iter()
                .find(|player| player.player_key == "key-a")
                .expect("Alice is in the public view")
                .deck_submitted,
            "the submission broadcast must expose updated readiness"
        );
        assert!(
            !public_view
                .players
                .iter()
                .find(|player| player.player_key == "key-b")
                .expect("Bob is in the public view")
                .deck_submitted,
            "Alice's submission must not make the opponent appear ready"
        );
        assert!(
            !outbounds_contain(&submitted, PRIVATE_CARD),
            "the private deck card leaked in submission outbounds: {submitted:?}"
        );
        assert!(
            !outbounds_contain(&submitted, &alice_token),
            "the player's credential leaked in submission outbounds: {submitted:?}"
        );

        let event = broker.tournaments().get(&code).expect("event");
        assert_eq!(
            event
                .players
                .iter()
                .find(|player| player.player_key == "key-a")
                .expect("Alice is registered")
                .deck
                .as_ref(),
            Some(&submitted_deck),
            "Broker::handle must store the exact submitted deck for its token owner"
        );
        assert!(
            event
                .players
                .iter()
                .find(|player| player.player_key == "key-b")
                .expect("Bob is registered")
                .deck
                .is_none(),
            "Alice's submission must not change the opponent's stored deck"
        );

        let refused = broker.handle(
            &mut alice,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.clone(),
                player_token: "valid-shape-wrong-token".into(),
                deck: engine::starter_decks::DeckData {
                    main_deck: vec!["Rejected Submission Card Sentinel".into()],
                    ..Default::default()
                },
                request_id: Some(REFUSAL_ID),
            },
            &env,
        );
        let (refusal_id, reason) = rejection_of(&refused);
        assert_eq!(refusal_id, REFUSAL_ID);
        assert!(
            reason.starts_with("Invalid player token for tournament "),
            "valid-shape wrong token must reach player authorization, got: {reason}"
        );
        assert_eq!(correlators(&refused), vec![REFUSAL_ID]);
        assert!(subscriber_msgs(&refused).is_empty());

        let event = broker.tournaments().get(&code).expect("event");
        assert_eq!(
            event
                .players
                .iter()
                .find(|player| player.player_key == "key-a")
                .expect("Alice is registered")
                .deck
                .as_ref(),
            Some(&submitted_deck),
            "a refused submission must leave Alice's stored deck unchanged"
        );
        assert!(
            event
                .players
                .iter()
                .find(|player| player.player_key == "key-b")
                .expect("Bob is registered")
                .deck
                .is_none(),
            "a refused submission must leave Bob's stored deck unchanged"
        );
    }

    /// Per-event persistence regression at the transport path — the HIGH the
    /// name-list-only accounting missed. Decks that each pass the per-deck inbound
    /// guard (empty name lists plus a large `printings` sidecar) but together would
    /// push one event's retained decks past
    /// [`crate::tournament::MAX_EVENT_DECK_ENCODED_BYTES`] are refused through
    /// `Broker::handle` with a correlated `TournamentActionRejected`, leaving every
    /// retained deck and readiness flag untouched. Replacing a heavy slot with a
    /// modest deck then frees the budget for the previously-refused entrant,
    /// proving the replaced slot is excluded from the retained sum. REVERT-FAIL:
    /// restore the name-list-only accounting and both printing-heavy decks are
    /// admitted, overrunning the per-event budget that keeps one event's deck
    /// footprint isolated.
    #[test]
    fn event_deck_budget_counts_printings_through_the_broker_path() {
        // Empty name lists, `entries` printings at the per-entry caps — the
        // review's witness: individually guard-accepted, but a large persisted
        // contribution a name-list sum scores as zero.
        fn printings_only_deck(entries: usize) -> engine::starter_decks::DeckData {
            use engine::types::card_printing::{
                CardPrinting, DeckPrinting, DeckSection, MAX_DECK_CARD_NAME_LEN,
                MAX_PRINTING_COLLECTOR_NUMBER_LEN, MAX_PRINTING_SET_CODE_LEN,
            };
            engine::starter_decks::DeckData {
                printings: (0..entries)
                    .map(|_| DeckPrinting {
                        section: DeckSection::MainDeck,
                        name: "x".repeat(MAX_DECK_CARD_NAME_LEN),
                        count: 1,
                        printing: CardPrinting::owner(
                            "s".repeat(MAX_PRINTING_SET_CODE_LEN),
                            "c".repeat(MAX_PRINTING_COLLECTOR_NUMBER_LEN),
                        ),
                    })
                    .collect(),
                ..Default::default()
            }
        }

        const REPLACE_ID: TournamentRequestId = TournamentRequestId(73);
        const ZOE_ID: TournamentRequestId = TournamentRequestId(79);
        const ZOE_RETRY_ID: TournamentRequestId = TournamentRequestId(80);

        let budget = crate::tournament::MAX_EVENT_DECK_ENCODED_BYTES;
        // A printings-only deck under the per-deck cap. Its name lists are empty, so
        // a sum scoring only name lists would score it zero — the witness for
        // counting the *persisted* encoding. No single deck may exceed 1/8 of the
        // per-event budget (the per-deck cap), so it takes several to fill it.
        let heavy = printings_only_deck(120);
        let each = serde_json::to_vec(&heavy).expect("deck serializes").len();
        assert!(
            each < crate::tournament::MAX_SINGLE_DECK_ENCODED_BYTES,
            "each witness deck must clear the per-deck cap (encoded {each})"
        );
        assert!(
            heavy.main_deck.is_empty() && each > 0,
            "the witness deck's cost must come from printings, not name lists"
        );
        // The number of such decks that fit one event; the next submission into it
        // overflows its per-event budget.
        let fill = budget / each;
        assert!(
            fill >= 2,
            "fixture must need several decks to fill the budget"
        );
        // A one-card replacement deck, with the headroom proof that swapping one
        // heavy slot down to it re-admits Zoe: her retry lands the event total at
        // `fill*each + light_bytes`, which must stay within budget.
        let light = engine::starter_decks::DeckData {
            main_deck: vec!["Island".into()],
            ..Default::default()
        };
        let light_bytes = serde_json::to_vec(&light).expect("deck serializes").len();
        assert!(
            light_bytes <= budget - fill * each,
            "fixture: post-replacement headroom ({}) must admit the light deck ({light_bytes})",
            budget - fill * each
        );
        // Each heavy deck is individually accepted by the per-deck inbound guard —
        // the precondition for "accepted production inputs" the review required.
        assert!(
            crate::inbound_guard::guard_inbound(&LobbyClientMessage::SubmitTournamentDeck {
                code: "PLACEHOLDER".into(),
                player_token: "placeholder".into(),
                deck: heavy.clone(),
                request_id: Some(REPLACE_ID),
            })
            .is_ok(),
            "the witness deck must pass the per-deck inbound bounds guard"
        );

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();
        let (code, _org) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);

        // Entrant 0 is kept addressable so it can later replace its deck.
        let mut first = ConnState::default();
        let first_token = join_tournament(&mut first, &mut broker, &env, &code, "key-0", "P0");
        let admitted = broker.handle(
            &mut first,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.clone(),
                player_token: first_token.clone(),
                deck: heavy.clone(),
                request_id: Some(TournamentRequestId(100)),
            },
            &env,
        );
        assert_eq!(ack_of(&admitted).0, TournamentRequestId(100));

        // The remaining fill entrants top this event up to just under its budget.
        for i in 1..fill {
            let mut conn = ConnState::default();
            let token = join_tournament(
                &mut conn,
                &mut broker,
                &env,
                &code,
                &format!("key-{i}"),
                &format!("P{i}"),
            );
            let out = broker.handle(
                &mut conn,
                LobbyClientMessage::SubmitTournamentDeck {
                    code: code.clone(),
                    player_token: token,
                    deck: heavy.clone(),
                    request_id: Some(TournamentRequestId(100 + i as u64)),
                },
                &env,
            );
            assert_eq!(
                ack_of(&out).0,
                TournamentRequestId(100 + i as u64),
                "fill deck {i} must be admitted within budget"
            );
        }

        // Zoe's equally heavy deck would push this event past its per-event budget
        // → a correlated refusal naming the event budget, and no broadcast.
        let mut zoe = ConnState::default();
        let zoe_token = join_tournament(&mut zoe, &mut broker, &env, &code, "key-zoe", "Zoe");
        let refused = broker.handle(
            &mut zoe,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.clone(),
                player_token: zoe_token.clone(),
                deck: heavy.clone(),
                request_id: Some(ZOE_ID),
            },
            &env,
        );
        let (refusal_id, reason) = rejection_of(&refused);
        assert_eq!(refusal_id, ZOE_ID);
        assert!(
            reason.contains("this event's deck budget"),
            "the refusal must name the per-event budget, got: {reason}"
        );
        assert!(
            subscriber_msgs(&refused).is_empty(),
            "a refused submission must not broadcast"
        );
        assert!(
            broker
                .tournaments()
                .get(&code)
                .expect("event")
                .players
                .iter()
                .find(|player| player.player_key == "key-zoe")
                .expect("Zoe is registered")
                .deck
                .is_none(),
            "Zoe's refused deck must not be stored"
        );

        // Replacement frees the budget: entrant 0 overwrites its heavy deck with the
        // modest one, so Zoe's heavy deck now fits — proving the replaced slot is
        // excluded from the retained sum rather than double-counted.
        let replaced = broker.handle(
            &mut first,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.clone(),
                player_token: first_token,
                deck: light,
                request_id: Some(REPLACE_ID),
            },
            &env,
        );
        assert_eq!(ack_of(&replaced).0, REPLACE_ID);

        let zoe_retry = broker.handle(
            &mut zoe,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.clone(),
                player_token: zoe_token,
                deck: heavy,
                request_id: Some(ZOE_RETRY_ID),
            },
            &env,
        );
        assert_eq!(
            ack_of(&zoe_retry).0,
            ZOE_RETRY_ID,
            "Zoe's heavy deck fits once entrant 0's heavy slot is replaced by a modest deck"
        );
    }

    /// Event isolation — the cross-event starvation a shared global deck pool would
    /// leave open. Each event's retained decks are bounded by its own
    /// [`crate::tournament::MAX_EVENT_DECK_ENCODED_BYTES`] and nothing else: there is
    /// no shared cross-event deck budget, so a single event — even filled to its cap
    /// through caller-chosen entrant keys — cannot consume the capacity an
    /// *unrelated, still-live* event needs. Through the production `Broker::handle`
    /// path: event A is filled to its per-event budget and its next entrant is
    /// refused naming the event budget; an unrelated live event B then admits the
    /// identical submission while A stays live, demonstrating B's allowance is wholly
    /// its own (this is the "an already admitted unrelated event can still submit"
    /// property). REVERT-FAIL: drop the per-event gate and A's overflow deck is
    /// admitted (the complete-snapshot bound has ample room), so the per-event
    /// refusal this asserts never fires.
    #[test]
    fn an_events_deck_budget_is_isolated_from_other_live_events() {
        fn heavy_deck(entries: usize) -> engine::starter_decks::DeckData {
            use engine::types::card_printing::{
                CardPrinting, DeckPrinting, DeckSection, MAX_DECK_CARD_NAME_LEN,
                MAX_PRINTING_COLLECTOR_NUMBER_LEN, MAX_PRINTING_SET_CODE_LEN,
            };
            engine::starter_decks::DeckData {
                printings: (0..entries)
                    .map(|_| DeckPrinting {
                        section: DeckSection::MainDeck,
                        name: "x".repeat(MAX_DECK_CARD_NAME_LEN),
                        count: 1,
                        printing: CardPrinting::owner(
                            "s".repeat(MAX_PRINTING_SET_CODE_LEN),
                            "c".repeat(MAX_PRINTING_COLLECTOR_NUMBER_LEN),
                        ),
                    })
                    .collect(),
                ..Default::default()
            }
        }

        // A deck under the per-deck cap, so each is individually admitted; several
        // fill one event's per-event budget.
        let heavy = heavy_deck(120);
        let each = serde_json::to_vec(&heavy).expect("serializes").len();
        assert!(
            each < crate::tournament::MAX_SINGLE_DECK_ENCODED_BYTES,
            "each deck must pass the per-deck cap (was {each})"
        );
        let fill = crate::tournament::MAX_EVENT_DECK_ENCODED_BYTES / each;
        assert!(
            fill >= 2,
            "fixture must need more than one deck to fill the per-event budget"
        );

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();

        // Event A: fill it to its per-event budget with `fill` guard-accepted decks.
        let (code_a, _org_a) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        for i in 0..fill {
            let mut conn = ConnState::default();
            let token = join_tournament(
                &mut conn,
                &mut broker,
                &env,
                &code_a,
                &format!("key-a{i}"),
                &format!("A{i}"),
            );
            let out = broker.handle(
                &mut conn,
                LobbyClientMessage::SubmitTournamentDeck {
                    code: code_a.clone(),
                    player_token: token,
                    deck: heavy.clone(),
                    request_id: Some(TournamentRequestId(100 + i as u64)),
                },
                &env,
            );
            assert_eq!(
                ack_of(&out).0,
                TournamentRequestId(100 + i as u64),
                "fill deck {i} must be admitted"
            );
        }

        // One more entrant in A is refused — A has reached its own per-event budget.
        const A_OVER_ID: TournamentRequestId = TournamentRequestId(200);
        let mut a_over = ConnState::default();
        let a_over_token = join_tournament(
            &mut a_over,
            &mut broker,
            &env,
            &code_a,
            "key-a-over",
            "AOver",
        );
        let a_refused = broker.handle(
            &mut a_over,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code_a.clone(),
                player_token: a_over_token,
                deck: heavy.clone(),
                request_id: Some(A_OVER_ID),
            },
            &env,
        );
        let (a_refusal_id, a_reason) = rejection_of(&a_refused);
        assert_eq!(a_refusal_id, A_OVER_ID);
        assert!(
            a_reason.contains("this event's deck budget"),
            "A's overflow must name the per-event budget, got: {a_reason}"
        );

        // Unrelated event B, still live: its entrant submits the identical heavy deck
        // and is ADMITTED — event A sitting at its full per-event budget did not
        // starve B, because A's footprint is bounded to its own per-event budget and
        // no shared cross-event pool exists for A to have drained.
        const B_ID: TournamentRequestId = TournamentRequestId(300);
        let mut bob = ConnState::default();
        let (code_b, _org_b) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        let bob_token = join_tournament(&mut bob, &mut broker, &env, &code_b, "key-b", "Bob");
        let admitted = broker.handle(
            &mut bob,
            LobbyClientMessage::SubmitTournamentDeck {
                code: code_b.clone(),
                player_token: bob_token,
                deck: heavy,
                request_id: Some(B_ID),
            },
            &env,
        );
        assert_eq!(
            ack_of(&admitted).0,
            B_ID,
            "an unrelated live event must still admit a deck while another event sits at its per-event cap"
        );
    }

    /// A terminal event stops charging the deck budget: when the real inactivity
    /// sweep ([`Broker::reap_expired`]) abandons an in-progress event, its retained
    /// decks are released, so they hold neither the per-event budget nor the
    /// complete-snapshot bound through the 30-day retention window. REVERT-FAIL: drop the
    /// `release_retained_decks` call in the abandon arm and the abandoned event keeps
    /// every deck, charging the budget for pairings it can no longer host.
    #[test]
    fn a_terminal_event_releases_its_deck_budget_on_abandonment() {
        fn heavy_deck(entries: usize) -> engine::starter_decks::DeckData {
            use engine::types::card_printing::{
                CardPrinting, DeckPrinting, DeckSection, MAX_DECK_CARD_NAME_LEN,
                MAX_PRINTING_COLLECTOR_NUMBER_LEN, MAX_PRINTING_SET_CODE_LEN,
            };
            engine::starter_decks::DeckData {
                printings: (0..entries)
                    .map(|_| DeckPrinting {
                        section: DeckSection::MainDeck,
                        name: "x".repeat(MAX_DECK_CARD_NAME_LEN),
                        count: 1,
                        printing: CardPrinting::owner(
                            "s".repeat(MAX_PRINTING_SET_CODE_LEN),
                            "c".repeat(MAX_PRINTING_COLLECTOR_NUMBER_LEN),
                        ),
                    })
                    .collect(),
                ..Default::default()
            }
        }

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();

        // Event A: two entrants submit decks while it is still in registration.
        let (code_a, org_a) =
            make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
        let heavy = heavy_deck(120);
        for (i, (key, name)) in [("key-a0", "A0"), ("key-a1", "A1")].iter().enumerate() {
            let mut conn = ConnState::default();
            let token = join_tournament(&mut conn, &mut broker, &env, &code_a, key, name);
            let out = broker.handle(
                &mut conn,
                LobbyClientMessage::SubmitTournamentDeck {
                    code: code_a.clone(),
                    player_token: token,
                    deck: heavy.clone(),
                    request_id: Some(TournamentRequestId(100 + i as u64)),
                },
                &env,
            );
            assert_eq!(ack_of(&out).0, TournamentRequestId(100 + i as u64));
        }

        // Start it, then idle it past the in-progress abandon window and run the
        // real sweep.
        broker.handle(
            &mut organizer,
            LobbyClientMessage::StartTournamentRound {
                code: code_a.clone(),
                organizer_token: org_a,
                request_id: None,
            },
            &env,
        );
        assert_eq!(
            broker.tournaments().get(&code_a).expect("A").status,
            TournamentStatus::InProgress
        );
        env.advance_secs(IN_PROGRESS_ABANDON_SECS + 1);
        broker.reap_expired(300, &env);

        let a = broker.tournaments().get(&code_a).expect("A retained");
        assert_eq!(a.status, TournamentStatus::Abandoned);
        assert!(
            a.players.iter().all(|p| p.deck.is_none()),
            "the abandoned event must retain no decks — its budget is reclaimed"
        );
    }

    /// Blocker-1 proof: the complete-snapshot bound caps the *whole* persisted
    /// broker — every event's entrant/pairing metadata as well as its decks — not
    /// deck bytes alone, and a write that would cross it is a state-preserving
    /// refusal. Every event the registry admits holds a deck (each within its
    /// own per-event budget, and paid for by its own reservation); one greedy event
    /// then registers entrants through the production `Broker::handle` path until
    /// the shared remainder is gone. That registration is refused naming the
    /// persistence ceiling, while a deck admitted earlier — and the slot that
    /// readiness derives from — is left exactly as it was, and the refused
    /// entrant is not registered. REVERT-FAIL: drop the persistence boundary in
    /// `Broker::handle` and the registration is admitted, letting the persisted
    /// snapshot grow past the 8 MB Durable Object key ceiling that silently fails
    /// the next `put`.
    #[test]
    fn a_registration_past_the_snapshot_budget_is_refused_without_losing_state() {
        fn heavy_deck(entries: usize) -> engine::starter_decks::DeckData {
            use engine::types::card_printing::{
                CardPrinting, DeckPrinting, DeckSection, MAX_DECK_CARD_NAME_LEN,
                MAX_PRINTING_COLLECTOR_NUMBER_LEN, MAX_PRINTING_SET_CODE_LEN,
            };
            engine::starter_decks::DeckData {
                printings: (0..entries)
                    .map(|_| DeckPrinting {
                        section: DeckSection::MainDeck,
                        name: "x".repeat(MAX_DECK_CARD_NAME_LEN),
                        count: 1,
                        printing: CardPrinting::owner(
                            "s".repeat(MAX_PRINTING_SET_CODE_LEN),
                            "c".repeat(MAX_PRINTING_COLLECTOR_NUMBER_LEN),
                        ),
                    })
                    .collect(),
                ..Default::default()
            }
        }

        // A deck under the per-deck cap, so each is individually admitted.
        let heavy = heavy_deck(1);
        let each = serde_json::to_vec(&heavy).expect("serializes").len();
        assert!(
            each < crate::tournament::MAX_SINGLE_DECK_ENCODED_BYTES,
            "each deck must pass the per-deck cap (was {each})"
        );

        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut organizer = ConnState::default();

        // Fill the registry; each event admits one deck. The first is kept to
        // prove it survives the later refusal.
        let mut witness: Option<(String, String)> = None; // (code, player_key)
        let mut greedy = String::new();
        for ev in 0..MAX_TOURNAMENT_ENTRIES {
            let (code, _org) =
                make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            let key = format!("k{ev}");
            let mut conn = ConnState::default();
            let token = join_tournament(&mut conn, &mut broker, &env, &code, &key, "Deck Owner");
            let id = TournamentRequestId(1_000_000 + ev as u64);
            let out = broker.handle(
                &mut conn,
                LobbyClientMessage::SubmitTournamentDeck {
                    code: code.clone(),
                    player_token: token,
                    deck: heavy.clone(),
                    request_id: Some(id),
                },
                &env,
            );
            assert_eq!(ack_of(&out).0, id, "a deck within its event's budget fits");
            witness.get_or_insert_with(|| (code.clone(), key.clone()));
            greedy = code;
        }
        let (witness_code, witness_key) = witness.expect("a deck was admitted");

        // Register entrants into one event until the shared snapshot is full.
        let mut refused: Option<(String, String)> = None; // (player_key, reason)
        for seat in 0..10_000 {
            let key = format!("late-{seat:0>w$}", w = crate::validation::MAX_TOKEN_LEN - 5);
            let mut conn = ConnState::default();
            let out = broker.handle(
                &mut conn,
                LobbyClientMessage::JoinTournament {
                    code: greedy.clone(),
                    player_key: key.clone(),
                    display_name: format!(
                        "{seat:0>w$}",
                        w = crate::validation::MAX_DISPLAY_NAME_LEN
                    ),
                },
                &env,
            );
            if matches!(
                out.first(),
                Some(Outbound::ToSelf(
                    LobbyServerMessage::TournamentJoined { .. }
                ))
            ) {
                continue;
            }
            refused = Some((key, gated_rejection_reason(&out)));
            break;
        }
        let (refused_key, reason) =
            refused.expect("the snapshot guard must eventually refuse a registration");
        assert!(
            reason.contains("persisted tournament state is at capacity"),
            "the refusal must name the persistence ceiling, got: {reason}"
        );
        // State-preserving refusal: the refused entrant was never registered.
        assert!(
            broker
                .tournaments()
                .get(&greedy)
                .expect("event retained")
                .player(&refused_key)
                .is_none(),
            "a snapshot-refused registration must leave no entrant behind"
        );
        // And a deck admitted before the guard fired — the slot `deck_submitted`
        // readiness derives from — is left exactly as it was.
        let witness_player = broker
            .tournaments()
            .get(&witness_code)
            .expect("witness event retained")
            .player(&witness_key)
            .expect("witness entrant retained");
        assert!(
            witness_player.deck.is_some(),
            "a deck admitted before the snapshot filled must survive a later refusal"
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
        let (code, organizer_token, token_a, _token_b) =
            started_event(&mut original, &mut broker, &env);

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
                // The secret handed to the entrant at join, not one read back
                // out of the stored credential: `TournamentCredential` keeps
                // its secret private, which is exactly the property that makes
                // a plaintext `==` at a call site unspellable.
                player_token: token_a,
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

    /// The broker half of the non-destructive sweep: a shell that can decline
    /// reports with `lobby().check_expired` and hands back only the codes it
    /// dispositioned, so an entry it skipped stays listed and is reported
    /// again. While the report consumed, that skip was permanent — the listing
    /// was already gone and no later tick could see it.
    #[test]
    fn reap_expired_handled_consumes_only_what_the_shell_handled() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host_a = ConnState::default();
        hello(&mut host_a, &mut broker, &env);
        let acted = game_code_of(&create(&mut host_a, &mut broker, &env));
        let mut host_b = ConnState::default();
        hello(&mut host_b, &mut broker, &env);
        let deferred = game_code_of(&create(&mut host_b, &mut broker, &env));

        env.advance_secs(301);

        // Reach guard: both listings really lapsed, so a one-entry fixture
        // cannot make "consumes only what it was handed" pass by coincidence.
        let reported = broker.lobby().check_expired(300, &env);
        let mut reported_codes: Vec<String> =
            reported.iter().map(|e| e.game_code().to_string()).collect();
        reported_codes.sort();
        let mut both = vec![acted.clone(), deferred.clone()];
        both.sort();
        assert_eq!(reported_codes, both);

        let acted_observation = reported
            .iter()
            .find(|e| e.game_code() == acted)
            .expect("the handled entry was reported")
            .clone();
        let first = broker.reap_expired_handled(std::slice::from_ref(&acted_observation), &env);
        assert_eq!(
            first.lobby_removed,
            vec![acted.clone()],
            "the handled entry is reported as removed"
        );
        assert_eq!(first.superseded, 0);
        let out = first.into_outbounds();
        assert_eq!(
            out,
            [Outbound::ToSubscribers(
                LobbyServerMessage::LobbyGameRemoved {
                    game_code: acted.clone()
                }
            )],
            "only the handled entry is announced"
        );
        assert!(!broker.lobby().has_game(&acted), "and only it is consumed");
        assert!(
            broker.lobby().has_game(&deferred),
            "the deferred entry keeps its listing"
        );

        // The next tick reports the deferred entry again — that is the retry.
        let retried = broker.lobby().check_expired(300, &env);
        assert_eq!(
            retried
                .iter()
                .map(|e| e.game_code().to_string())
                .collect::<Vec<_>>(),
            vec![deferred.clone()],
            "a deferred entry must be reported again"
        );
        let second = broker.reap_expired_handled(&retried, &env).into_outbounds();
        assert_eq!(
            second,
            [Outbound::ToSubscribers(
                LobbyServerMessage::LobbyGameRemoved {
                    game_code: deferred
                }
            )]
        );
        assert!(
            broker.lobby().is_empty(),
            "a deferred entry is consumed by the tick that can act on it"
        );
    }

    /// The shell releases the lobby lock between reporting an expiry and
    /// handing it back here, and `register_game` overwrites whatever is
    /// registered under a code. A consume keyed on the code alone therefore
    /// removed the *replacement* and told every subscriber to delist it — a
    /// live, unexpired listing silently deleted. The consume is keyed on the
    /// registration that was observed instead.
    #[test]
    fn a_replacement_listing_survives_the_consume_of_the_entry_it_replaced() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host_a = ConnState::default();
        hello(&mut host_a, &mut broker, &env);
        let code = game_code_of(&create(&mut host_a, &mut broker, &env));

        env.advance_secs(301);

        let reported = broker.lobby().check_expired(300, &env);
        assert_eq!(
            reported
                .iter()
                .map(|e| e.game_code().to_string())
                .collect::<Vec<_>>(),
            vec![code.clone()],
            "reach guard: the lapsed listing really was observed"
        );

        // The gap: the shell has dropped the lobby lock to do its session work,
        // and the freed code is registered again before the consume lands.
        broker.lobby_mut().register_game(
            &code,
            RegisterGameRequest {
                host_name: "replacement".to_string(),
                public: true,
                ..Default::default()
            },
            &env,
        );

        let outcome = broker.reap_expired_handled(&reported, &env);
        assert!(
            outcome.lobby_removed.is_empty(),
            "a superseded observation must not be reported as removed, or the \
             shell prunes a live game's connections"
        );
        assert_eq!(outcome.superseded, 1, "and is counted as superseded");
        let out = outcome.into_outbounds();
        assert!(out.is_empty(), "nor may it announce anything: {out:?}");
        assert!(
            broker.lobby().has_game(&code),
            "and the replacement keeps its listing"
        );
        assert_eq!(
            broker
                .lobby()
                .public_games()
                .iter()
                .map(|g| g.host_name.clone())
                .collect::<Vec<_>>(),
            vec!["replacement".to_string()],
            "the entry standing is the replacement, not the one that lapsed"
        );
    }

    /// The split a one-entry fixture cannot see. Consumption stopped being one
    /// outbound per handled code the moment it became conditional, so a mixed
    /// tick is the shape that shows each half of [`ReapOutcome`] tracking what
    /// the sweep did rather than what it was handed.
    #[test]
    fn a_mixed_tick_announces_only_the_entries_it_removed() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host_a = ConnState::default();
        hello(&mut host_a, &mut broker, &env);
        let kept = game_code_of(&create(&mut host_a, &mut broker, &env));
        let mut host_b = ConnState::default();
        hello(&mut host_b, &mut broker, &env);
        let replaced = game_code_of(&create(&mut host_b, &mut broker, &env));

        env.advance_secs(301);
        let reported = broker.lobby().check_expired(300, &env);
        assert_eq!(reported.len(), 2, "reach guard: both listings lapsed");

        // One of the two codes is taken by a replacement in the released-lock gap.
        broker.lobby_mut().register_game(
            &replaced,
            RegisterGameRequest {
                host_name: "replacement".to_string(),
                public: true,
                ..Default::default()
            },
            &env,
        );

        let outcome = broker.reap_expired_handled(&reported, &env);
        assert_eq!(
            outcome.lobby_removed,
            vec![kept.clone()],
            "only the entry actually removed is reported removed"
        );
        assert_eq!(
            outcome.superseded, 1,
            "and the other is counted, not silently dropped — this is the only \
             witness a tick that superseded everything did any work at all"
        );
        assert!(
            outcome.tournament.is_empty(),
            "reach guard: no tournament expiry is muddying the counts"
        );
        assert_eq!(
            outcome.into_outbounds().len(),
            1,
            "one outbound, fewer than the two observations handed in"
        );
        assert!(
            broker.lobby().has_game(&replaced),
            "the replacement survives the tick that consumed its predecessor"
        );
        assert!(!broker.lobby().has_game(&kept));
    }

    /// `AlreadyGone` must still broadcast. Base announced a removal for every
    /// handled code, including one another path had already unregistered — a
    /// client that over-prunes recovers where one that never hears of the
    /// removal keeps a dead entry. Only `Superseded` suppresses the broadcast,
    /// and nothing else asserts that at the broker, where the decision lives.
    #[test]
    fn an_entry_another_path_removed_is_still_announced() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();

        let mut host = ConnState::default();
        hello(&mut host, &mut broker, &env);
        let code = game_code_of(&create(&mut host, &mut broker, &env));

        env.advance_secs(301);
        let reported = broker.lobby().check_expired(300, &env);
        assert_eq!(reported.len(), 1, "reach guard: the listing lapsed");

        // Another path disposes of the entry while the lobby lock is down.
        broker.lobby_mut().unregister_game(&code);
        assert!(
            !broker.lobby().has_game(&code),
            "reach guard: nothing is listed, so this is the AlreadyGone branch \
             and not the Removed one"
        );

        let outcome = broker.reap_expired_handled(&reported, &env);
        assert_eq!(
            outcome.lobby_removed,
            vec![code.clone()],
            "an entry another path removed is still reported removed, so the \
             shell still prunes it"
        );
        assert_eq!(outcome.superseded, 0);
        assert_eq!(
            outcome.into_outbounds(),
            [Outbound::ToSubscribers(
                LobbyServerMessage::LobbyGameRemoved { game_code: code }
            )],
            "and its removal is still announced"
        );
    }

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
        let mut reaped = 0;
        for _ in 0..MAX_CONN_TOURNAMENT_ENTRIES + overshoot {
            // The registry holds fewer events than this socket's list bound, so
            // the staleness reaper frees it whenever it fills.
            if broker.tournaments().len() >= MAX_TOURNAMENT_ENTRIES {
                reaped += broker.tournaments().len();
                env.advance_secs(REGISTRATION_TIMEOUT_SECS + 1);
                broker.reap_expired(300, &env);
                assert!(broker.tournaments().is_empty());
            }
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

        // The registry itself is untouched by the per-connection bound: only the
        // reaper removed events, this socket's convenience list is what is trimmed.
        assert_eq!(
            broker.tournaments().len() + reaped,
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
                scoring: Some(ScoringPolicy::default()),
                bracket: BracketShape::Swiss,
                total_rounds: None,
                plus_rounds: None,
                format: None,
                match_type: None,
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

    // -- Broker-owned default scoring (V6) ----------------------------------

    /// Creates a tournament with an explicitly chosen `scoring` and returns
    /// `(code, organizer_token, resolved_scoring)` read back off the reply's
    /// own summary — the value a client would actually see.
    fn create_with_scoring(
        conn: &mut ConnState,
        broker: &mut Broker,
        env: &FakeEnv,
        arity: MatchArity,
        scoring: Option<ScoringPolicy>,
    ) -> (String, String, ScoringPolicy, u64) {
        let out = broker.handle(
            conn,
            LobbyClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity,
                scoring,
                bracket: BracketShape::Swiss,
                total_rounds: None,
                plus_rounds: None,
                format: None,
                match_type: None,
            },
            env,
        );
        match out.first() {
            Some(Outbound::ToSelf(LobbyServerMessage::TournamentCreated {
                code,
                organizer_token,
                expires_at_ms,
                view,
            })) => (
                code.clone(),
                organizer_token.clone(),
                view.summary.scoring,
                *expires_at_ms,
            ),
            other => panic!("expected TournamentCreated, got {other:?}"),
        }
    }

    /// V6. An omitted `scoring` is resolved by the BROKER from
    /// `ScoringPolicy::default_for_arity`, and the resolved value comes back
    /// on the summary so no client has to recompute it — the same shape
    /// `total_rounds` already has.
    ///
    /// Two arities, because a single one cannot tell "the broker applied the
    /// arity default" apart from "the broker applied a constant".
    #[test]
    fn an_omitted_scoring_is_resolved_by_the_broker_from_the_arity() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        let (code, _, head_to_head, _) =
            create_with_scoring(&mut conn, &mut broker, &env, MatchArity::HEAD_TO_HEAD, None);
        assert_eq!(
            (
                head_to_head.win_points(),
                head_to_head.draw_points(),
                head_to_head.loss_points()
            ),
            (3, 1, 0),
            "MTR 2.1's 3/1/0 at two seats"
        );

        let (_, _, pod, _) = create_with_scoring(
            &mut conn,
            &mut broker,
            &env,
            MatchArity::COMMANDER_POD,
            None,
        );
        assert_eq!(
            (pod.win_points(), pod.draw_points(), pod.loss_points()),
            (7, 1, 0),
            "MSTR's 2n-1 at four seats"
        );

        // The stored record agrees with what went out on the wire: the
        // resolution happens once, at creation, and is not recomputed per view.
        assert_eq!(
            broker.tournaments().get(&code).expect("event").scoring,
            head_to_head
        );

        // An explicit override survives untouched — the discriminating case
        // against a broker that defaulted unconditionally.
        let explicit = ScoringPolicy::new(9, 2, 1).expect("valid policy");
        let (_, _, kept, _) = create_with_scoring(
            &mut conn,
            &mut broker,
            &env,
            MatchArity::COMMANDER_POD,
            Some(explicit),
        );
        assert_eq!(kept, explicit);
    }

    // -- Credential expiry on the mint replies (V26) ------------------------

    /// V26. Both replies that MINT a credential carry that credential's
    /// expiry, and the value is clock-derived rather than constant.
    ///
    /// The assertion is deliberately clock-relative — `env.now_ms() +
    /// TOURNAMENT_CREDENTIAL_TTL_MS` at the instant of the call. It is not
    /// satisfied by a hardcoded constant, by a `> now` sentinel, or by a value
    /// read from a different clock tick, and unlike an equality against the
    /// stored credential's field it is constructible here:
    /// `TournamentCredential` keeps its expiry private behind `accepts()`, and
    /// adding an accessor purely to test with would weaken exactly the
    /// single-authority property that field's privacy exists to hold.
    #[test]
    fn both_mint_replies_carry_the_credentials_expiry() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();

        let at_create = env.now_ms();
        let (code, organizer_token, _, created_expiry) =
            create_with_scoring(&mut conn, &mut broker, &env, MatchArity::HEAD_TO_HEAD, None);
        assert_eq!(created_expiry, at_create + TOURNAMENT_CREDENTIAL_TTL_MS);

        // Positive control #1: advance the clock and the next mint's expiry
        // moves by exactly the same delta. A constant would not.
        env.advance_secs(3_600);
        let at_join = env.now_ms();
        assert_eq!(at_join, at_create + 3_600 * 1_000);

        let mut entrant = ConnState::default();
        let out = broker.handle(
            &mut entrant,
            LobbyClientMessage::JoinTournament {
                code: code.clone(),
                player_key: "key-a".into(),
                display_name: "Alice".into(),
            },
            &env,
        );
        let (player_token, joined_expiry) = match out.first() {
            Some(Outbound::ToSelf(LobbyServerMessage::TournamentJoined {
                player_token,
                expires_at_ms,
                ..
            })) => (player_token.clone(), *expires_at_ms),
            other => panic!("expected TournamentJoined, got {other:?}"),
        };
        assert_eq!(joined_expiry, at_join + TOURNAMENT_CREDENTIAL_TTL_MS);
        assert_eq!(joined_expiry - created_expiry, 3_600 * 1_000);

        // Positive control #2, and the one that reaches the STORED credential
        // without new public surface: the wire value is pinned to the
        // credential's own behavior through the accessor that already exists.
        // The boundary is exclusive, stated once on `TournamentCredential`.
        let meta = broker.tournaments().get(&code).expect("event");
        assert!(meta
            .organizer_token
            .accepts(&organizer_token, created_expiry - 1));
        assert!(!meta
            .organizer_token
            .accepts(&organizer_token, created_expiry));
        let player = meta.player("key-a").expect("entrant");
        assert!(player
            .player_token
            .accepts(&player_token, joined_expiry - 1));
        assert!(!player.player_token.accepts(&player_token, joined_expiry));

        // Hostile: the RENEWAL reply's expiry is strictly greater than the
        // mint reply's once the clock has moved, which proves rotation re-bound
        // the expiry rather than echoing the original.
        env.advance_secs(60);
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::RenewTournamentCredential {
                code: code.clone(),
                role: TournamentRole::Organizer,
                token: organizer_token.clone(),
                rotation_nonce: "nonce-a".to_string(),
            },
            &env,
        );
        match out.as_slice() {
            [Outbound::ToSelf(LobbyServerMessage::TournamentCredentialRenewed {
                code: reply_code,
                role,
                token,
                expires_at_ms,
            })] => {
                assert_eq!(reply_code, &code);
                assert_eq!(*role, TournamentRole::Organizer);
                assert_ne!(token, &organizer_token, "rotation mints a NEW secret");
                assert!(
                    *expires_at_ms > created_expiry,
                    "the rotated expiry must be re-derived, not echoed"
                );
                assert_eq!(*expires_at_ms, env.now_ms() + TOURNAMENT_CREDENTIAL_TTL_MS);
            }
            other => panic!("expected exactly one TournamentCredentialRenewed, got {other:?}"),
        }
    }

    /// The rotated secret is never fanned out: rotation answers with exactly
    /// one `ToSelf` outbound and no broadcast, because nothing a subscriber
    /// watches changed and a secret must not ride a frame with more than one
    /// recipient.
    #[test]
    fn credential_rotation_answers_only_the_caller() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, organizer_token) =
            make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);

        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::RenewTournamentCredential {
                code: code.clone(),
                role: TournamentRole::Organizer,
                token: organizer_token.clone(),
                rotation_nonce: "nonce-a".to_string(),
            },
            &env,
        );
        assert_eq!(out.len(), 1, "rotation is a point reply: {out:?}");
        assert!(
            !out.iter()
                .any(|ob| matches!(ob, Outbound::ToSubscribers(_))),
            "a rotated secret must never be broadcast: {out:?}"
        );

        // The superseded secret stops authorizing actions the instant it is
        // rotated away — only the current secret does, at the broker's own gate
        // as inside the manager. Its sole residual power is an idempotent replay
        // through RenewTournamentCredential with the matching nonce, exercised in
        // the tournament unit tests; it can never authorize an action.
        let refused = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code,
                organizer_token,
                request_id: None,
            },
            &env,
        );
        assert!(
            error_reason_contains(&refused, "Invalid organizer token"),
            "the rotated-away secret must not authorize an action: {refused:?}"
        );
    }

    /// An expired credential is refused by the broker's own authority check
    /// with a message that says so, so a holder can tell "renew and retry"
    /// apart from "you were never authorized".
    #[test]
    fn the_broker_tells_an_expired_credential_apart_from_a_wrong_one() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, organizer_token) =
            make_tournament(&mut conn, &mut broker, &env, BracketShape::Swiss);

        // Reach-guard: the same call is accepted while the credential is live,
        // so the refusal below is about expiry and not about the fixture.
        let ok = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: None,
            },
            &env,
        );
        assert!(
            !error_reason_contains(&ok, "Invalid organizer token"),
            "a live credential must authorize: {ok:?}"
        );

        env.advance_secs(TOURNAMENT_CREDENTIAL_TTL_MS / 1_000);
        let out = broker.handle(
            &mut conn,
            LobbyClientMessage::StartTournamentRound {
                code,
                organizer_token,
                request_id: None,
            },
            &env,
        );
        let reason = gated_rejection_reason(&out);
        assert!(
            reason.contains("expired"),
            "expected the expiry message, got: {reason}"
        );
    }

    /// V3, production half. The `open_actions` a client reads off the wire and
    /// the refusal the DISPATCH path produces come from one authority, so a
    /// terminal transition between the view and the dispatch cannot be talked
    /// past.
    ///
    /// This goes through `broker.handle` rather than the manager directly,
    /// because `handle` is the entry point a real client reaches and the one
    /// that routes a gated action through `settle_gated`.
    #[test]
    fn a_stale_open_actions_read_off_the_wire_does_not_survive_the_dispatch() {
        let env = FakeEnv::new();
        let mut broker = Broker::new();
        let mut conn = ConnState::default();
        let (code, organizer_token, token_a, _token_b) =
            started_event(&mut conn, &mut broker, &env);

        // What the client was actually shown, read off the wire projection.
        let shown = broker
            .tournament_view(&code)
            .expect("view")
            .summary
            .open_actions;
        assert!(
            shown.contains(&TournamentAction::StartRound)
                && shown.contains(&TournamentAction::Drop)
                && shown.contains(&TournamentAction::EndTournament),
            "the reach-guard: a running event advertises all three, got {shown:?}"
        );

        // The event ends between the view and the dispatch.
        let pairing_id = broker.tournaments().get(&code).expect("event").pairings[0].id;
        broker.handle(
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
        let ended = broker.handle(
            &mut conn,
            LobbyClientMessage::EndTournament {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: None,
            },
            &env,
        );
        assert!(!is_error(&ended), "the event must actually end: {ended:?}");

        // The wire now advertises nothing, and each stale dispatch is refused
        // with the lifecycle message rather than being admitted.
        let after = broker
            .tournament_view(&code)
            .expect("view")
            .summary
            .open_actions;
        assert!(
            after.is_empty(),
            "a terminal event advertises nothing: {after:?}"
        );

        for msg in [
            LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: organizer_token.clone(),
                request_id: None,
            },
            LobbyClientMessage::EndTournament {
                code: code.clone(),
                organizer_token,
                request_id: None,
            },
        ] {
            let out = broker.handle(&mut conn, msg, &env);
            let reason = gated_rejection_reason(&out);
            assert!(
                reason.contains("no longer running") || reason.contains("already finished"),
                "expected the lifecycle refusal, got: {reason}"
            );
        }
    }

    /// True when `out` carries an `Error` or a gated rejection whose message
    /// contains `needle`. Used for reach-guards, where the point is only that a
    /// specific refusal did NOT happen.
    fn error_reason_contains(out: &[Outbound], needle: &str) -> bool {
        out.iter().any(|ob| match ob {
            Outbound::ToSelf(LobbyServerMessage::Error { message, .. }) => message.contains(needle),
            Outbound::ToSelf(LobbyServerMessage::TournamentActionRejected { message, .. }) => {
                message.contains(needle)
            }
            _ => false,
        })
    }

    /// The message from whichever refusal shape a gated action settled with —
    /// a bare `Error` or a correlated `TournamentActionRejected`.
    fn gated_rejection_reason(out: &[Outbound]) -> String {
        for ob in out {
            match ob {
                Outbound::ToSelf(LobbyServerMessage::Error { message, .. })
                | Outbound::ToSelf(LobbyServerMessage::TournamentActionRejected {
                    message, ..
                }) => return message.clone(),
                _ => {}
            }
        }
        panic!("expected a refusal outbound, got {out:?}")
    }

    // -- Persistence boundary ----------------------------------------------
    //
    // Every growth path crosses `Broker::handle`, which admits a frame only if
    // the persisted value still fits the snapshot ceiling and every reservation
    // (`Broker::committed_bytes`), and otherwise restores the prior state. The
    // fixtures below drive the shared store to an exact byte headroom with state
    // that is itself legitimately admitted (events within their per-event deck
    // budget), then exercise each growth path at that edge.
    mod persistence_boundary {
        use super::*;
        use crate::inbound_guard::MAX_MAIN_DECK_ENTRIES;
        use crate::tournament::{
            retained_deck_charge, MAX_EVENT_DECK_ENCODED_BYTES, MAX_SINGLE_DECK_ENCODED_BYTES,
        };
        use engine::starter_decks::DeckData;

        /// A deck whose encoded size grows by exactly one byte per character, so a
        /// resubmission can tune the snapshot size to the byte.
        fn deck_of(chars: usize) -> DeckData {
            DeckData {
                main_deck: vec!["x".repeat(chars)],
                ..Default::default()
            }
        }

        /// A deck that passes the inbound wire guard (entries of at most 120
        /// bytes, 500 of them) and encodes to roughly 61 KB, a maximum-size valid
        /// submission just under the per-deck ceiling.
        fn wire_max_deck() -> DeckData {
            DeckData {
                main_deck: vec!["y".repeat(120); 500],
                ..Default::default()
            }
        }

        /// Characters in a "big" deck: under the per-deck ceiling with margin for
        /// the encoding's own fields.
        const BIG: usize = 60_000;
        // Eight big decks fit one event's deck budget.
        const _: () = assert!(MAX_EVENT_DECK_ENCODED_BYTES > 8 * BIG);

        fn serialized(broker: &Broker) -> String {
            broker.snapshot_json()
        }

        /// Bytes the snapshot may still grow by before a growth write is refused.
        fn headroom(broker: &Broker) -> usize {
            MAX_SNAPSHOT_ENCODED_BYTES.saturating_sub(broker.committed_bytes())
        }

        /// Writes straight into the registry, deliberately bypassing the boundary,
        /// to construct a state the boundary would itself have admitted.
        fn put_deck(broker: &mut Broker, env: &FakeEnv, code: &str, key: &str, chars: usize) {
            let tournaments = broker.tournaments_mut();
            if tournaments
                .get(code)
                .is_some_and(|meta| meta.player(key).is_none())
            {
                tournaments
                    .join_tournament(code, key, key, env)
                    .expect("fixture join");
            }
            tournaments
                .submit_deck(code, key, deck_of(chars), env)
                .expect("fixture deck");
        }

        /// Sets event `code`'s name to exactly `chars` bytes. The name is event
        /// metadata of any size the fixture likes (the wire guard is bypassed on
        /// purpose), so this grows or shrinks the record byte for byte without
        /// touching its decks, entrants or pairings.
        fn set_event_name_chars(broker: &mut Broker, code: &str, chars: usize) {
            broker
                .tournaments_mut()
                .fixture_meta_mut(code)
                .expect("event")
                .name = "n".repeat(chars);
        }

        /// The encoded bytes of event `code`'s record outside its retained decks.
        fn metadata_bytes(broker: &Broker, code: &str) -> usize {
            let meta = broker.tournaments().get(code).expect("event");
            encoded_len(meta) - meta.retained_deck_bytes()
        }

        /// Pushes event `code`'s metadata past its metadata allowance, so its
        /// further metadata growth competes for shared headroom instead of its own
        /// guarantee.
        fn inflate(broker: &mut Broker, code: &str) {
            set_event_name_chars(broker, code, 2 * EVENT_METADATA_RESERVE_ENCODED_BYTES);
            let size = metadata_bytes(broker, code);
            assert!(
                size > EVENT_METADATA_RESERVE_ENCODED_BYTES,
                "fixture: event {code} ({size} metadata bytes) must exceed its allowance"
            );
        }

        /// The tunable absorber [`fill_to_headroom`] leaves behind: shrinking its
        /// name gives capacity back, growing it takes capacity away, byte for byte.
        struct Filler {
            code: String,
            chars: usize,
        }

        impl Filler {
            fn set_name_chars(&mut self, broker: &mut Broker, chars: usize) {
                set_event_name_chars(broker, &self.code, chars);
                self.chars = chars;
            }

            /// Give `bytes` of shared headroom back.
            fn release(&mut self, broker: &mut Broker, bytes: usize) {
                let chars = self.chars - bytes;
                self.set_name_chars(broker, chars);
            }
        }

        /// Drives `broker` to exactly `want` bytes below the ceiling (as measured
        /// by [`Broker::committed_bytes`]) with fresh, empty events that each hold
        /// their own full reservation, then one tunable absorber whose metadata is
        /// already past its allowance, so every byte it grows is shared headroom.
        fn fill_to_headroom(broker: &mut Broker, env: &FakeEnv, want: usize) -> Filler {
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, broker, env, BracketShape::Swiss);
            let mut filler = Filler { code, chars: 0 };
            filler.set_name_chars(broker, 2 * EVENT_METADATA_RESERVE_ENCODED_BYTES);
            let target = MAX_SNAPSHOT_ENCODED_BYTES - want;

            // Bulk: whole events, while one more reservation still fits the target.
            while target.saturating_sub(broker.committed_bytes())
                >= EVENT_RESERVE_ENCODED_BYTES + 1_024
            {
                make_tournament(&mut organizer, broker, env, BracketShape::Swiss);
            }

            let base = broker.committed_bytes();
            assert!(base <= target, "fixture overshot: {base} > {target}");
            let chars = filler.chars + (target - base);
            filler.set_name_chars(broker, chars);
            assert_eq!(
                headroom(broker),
                want,
                "fixture: store must sit at exactly the requested headroom"
            );
            filler
        }

        fn join_frame(code: &str, key: &str) -> LobbyClientMessage {
            LobbyClientMessage::JoinTournament {
                code: code.to_string(),
                player_key: key.to_string(),
                display_name: key.to_string(),
            }
        }

        fn create_frame() -> LobbyClientMessage {
            LobbyClientMessage::CreateTournament {
                name: "Late Event".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: Some(ScoringPolicy::default()),
                bracket: BracketShape::Swiss,
                total_rounds: None,
                plus_rounds: None,
                format: None,
                match_type: None,
            }
        }

        /// The refusal contract every growth path shares: one refusal reply, no
        /// success reply, and the broker and the connection byte-identical.
        fn assert_refused_intact(
            out: &[Outbound],
            before_broker: &str,
            broker: &Broker,
            before_conn: &ConnState,
            conn: &ConnState,
        ) {
            assert_eq!(
                serialized(broker),
                before_broker,
                "a capacity refusal must leave every persisted byte as it was"
            );
            assert_eq!(conn, before_conn, "the connection must be restored too");
            assert!(
                !out.iter().any(|ob| matches!(
                    ob,
                    Outbound::ToSelf(
                        LobbyServerMessage::TournamentJoined { .. }
                            | LobbyServerMessage::TournamentCreated { .. }
                            | LobbyServerMessage::TournamentActionAck { .. }
                    ) | Outbound::ToSubscribers(_)
                )),
                "a refused write must not report success or broadcast it: {out:?}"
            );
            let reason = gated_rejection_reason(out);
            assert!(
                reason.contains("at capacity"),
                "unexpected refusal: {reason}"
            );
        }

        /// Registration is metadata growth with no deck involved — the path the
        /// deck-only check left open. At the ceiling it is refused with prior
        /// state intact; once capacity returns the identical frame is admitted.
        #[test]
        fn a_registration_past_the_ceiling_is_refused_and_leaves_state_intact() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            inflate(&mut broker, &code);
            let mut filler = fill_to_headroom(&mut broker, &env, 40);

            let before = serialized(&broker);
            let players = broker
                .tournaments()
                .get(&code)
                .expect("event")
                .players
                .len();
            let mut joiner = ConnState::default();
            let before_conn = joiner.clone();
            let out = broker.handle(&mut joiner, join_frame(&code, "late"), &env);
            assert_refused_intact(&out, &before, &broker, &before_conn, &joiner);
            assert_eq!(
                broker
                    .tournaments()
                    .get(&code)
                    .expect("event")
                    .players
                    .len(),
                players
            );

            filler.release(&mut broker, 5_000);
            let out = broker.handle(&mut joiner, join_frame(&code, "late"), &env);
            assert!(
                matches!(
                    out.first(),
                    Some(Outbound::ToSelf(
                        LobbyServerMessage::TournamentJoined { .. }
                    ))
                ),
                "with capacity back the same registration is admitted: {out:?}"
            );
            assert!(broker.committed_bytes() <= MAX_SNAPSHOT_ENCODED_BYTES);
        }

        /// Creation is admitted only while the new event's reservation fits: one
        /// byte short is refused with nothing stored; exactly enough is admitted.
        #[test]
        fn creating_an_event_requires_its_reservation_to_fit() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut filler = fill_to_headroom(&mut broker, &env, EVENT_RESERVE_ENCODED_BYTES - 1);
            let before = serialized(&broker);
            let events = broker.tournaments().len();
            let mut conn = ConnState::default();
            let before_conn = conn.clone();
            let out = broker.handle(&mut conn, create_frame(), &env);
            assert_refused_intact(&out, &before, &broker, &before_conn, &conn);
            assert_eq!(broker.tournaments().len(), events);
            assert!(conn.organized_tournaments.is_empty());

            // One byte of capacity returned still does not fit it: the event's
            // registry key and separator are persisted outside its record.
            filler.release(&mut broker, 1);
            let out = broker.handle(&mut conn, create_frame(), &env);
            assert!(error_reason(&out).contains("at capacity"), "{out:?}");

            // With room for the record, its key and separator, it is admitted.
            filler.release(&mut broker, 63);
            let out = broker.handle(&mut conn, create_frame(), &env);
            assert!(
                matches!(
                    out.first(),
                    Some(Outbound::ToSelf(
                        LobbyServerMessage::TournamentCreated { .. }
                    ))
                ),
                "admitted once its reservation fits: {out:?}"
            );
            assert!(broker.committed_bytes() <= MAX_SNAPSHOT_ENCODED_BYTES);
        }

        /// Starting a round writes the pairing list; a result writes the outcome.
        /// Both are refused at the ceiling with the event's status and pairings
        /// untouched, and a correlated refusal reaches the requester.
        #[test]
        fn round_start_and_result_growth_are_refused_at_the_ceiling() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, org) =
                make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            let mut alice = ConnState::default();
            let alice_token = join_tournament(&mut alice, &mut broker, &env, &code, "a", "Alice");
            let mut bob = ConnState::default();
            join_tournament(&mut bob, &mut broker, &env, &code, "b", "Bob");
            inflate(&mut broker, &code);
            let mut filler = fill_to_headroom(&mut broker, &env, 10);

            // Round start.
            let start = |request: u64| LobbyClientMessage::StartTournamentRound {
                code: code.clone(),
                organizer_token: org.clone(),
                request_id: Some(TournamentRequestId(request)),
            };
            let before = serialized(&broker);
            let before_conn = organizer.clone();
            let out = broker.handle(&mut organizer, start(7), &env);
            assert_refused_intact(&out, &before, &broker, &before_conn, &organizer);
            assert_eq!(rejection_of(&out).0, TournamentRequestId(7));
            assert_eq!(
                broker.tournaments().get(&code).expect("event").status,
                TournamentStatus::Registration
            );
            filler.release(&mut broker, 20_000);
            let out = broker.handle(&mut organizer, start(8), &env);
            assert_eq!(ack_of(&out).0, TournamentRequestId(8));

            // Result report, at the ceiling again.
            let pairing = broker.tournaments().get(&code).expect("event").pairings[0].id;
            let tighten = headroom(&broker) - 10;
            let chars = filler.chars + tighten;
            filler.set_name_chars(&mut broker, chars);
            assert_eq!(headroom(&broker), 10);

            let report = |request: u64| LobbyClientMessage::ReportMatchResult {
                code: code.clone(),
                pairing_id: pairing,
                player_token: alice_token.clone(),
                outcome: PodOutcome::Draw,
                request_id: Some(TournamentRequestId(request)),
            };
            let before = serialized(&broker);
            let before_conn = alice.clone();
            let out = broker.handle(&mut alice, report(9), &env);
            assert_refused_intact(&out, &before, &broker, &before_conn, &alice);
            assert_eq!(rejection_of(&out).0, TournamentRequestId(9));
            filler.release(&mut broker, 20_000);
            let out = broker.handle(&mut alice, report(10), &env);
            assert_eq!(ack_of(&out).0, TournamentRequestId(10));
        }

        /// F2: an admitted event's deck guarantee is its own account. With the
        /// shared store at its ceiling, an event whose metadata has already grown
        /// to its whole allowance (the next metadata byte is refused) still takes
        /// both head-to-head deck submissions at the maximum supported size:
        /// metadata growth draws only on the metadata allowance or the shared
        /// remainder, never on the deck allowance.
        #[test]
        fn metadata_growth_cannot_consume_an_admitted_events_deck_guarantee() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            let mut alice = ConnState::default();
            let alice_token = join_tournament(&mut alice, &mut broker, &env, &code, "a", "Alice");
            let mut bob = ConnState::default();
            let bob_token = join_tournament(&mut bob, &mut broker, &env, &code, "b", "Bob");
            let _filler = fill_to_headroom(&mut broker, &env, 0);
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);

            // Grow the event's metadata through production registrations until the
            // shared store refuses the next one.
            let mut refused = None;
            for seat in 0..2_000 {
                let mut entrant = ConnState::default();
                let key = format!("p{seat}");
                let before = serialized(&broker);
                let before_conn = entrant.clone();
                let out = broker.handle(&mut entrant, join_frame(&code, &key), &env);
                if !matches!(
                    out.first(),
                    Some(Outbound::ToSelf(
                        LobbyServerMessage::TournamentJoined { .. }
                    ))
                ) {
                    assert_refused_intact(&out, &before, &broker, &before_conn, &entrant);
                    refused = Some(seat);
                    break;
                }
            }
            assert!(
                refused.is_some_and(|seat| seat > 0),
                "registrations were admitted up to the allowance, then refused"
            );
            let metadata = metadata_bytes(&broker, &code);
            assert!(
                metadata <= EVENT_METADATA_RESERVE_ENCODED_BYTES
                    && metadata + 1_024 > EVENT_METADATA_RESERVE_ENCODED_BYTES,
                "fixture: the event's metadata sits at its allowance ({metadata})"
            );
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);

            // Two supported head-to-head submissions are still admitted.
            let max_deck = wire_max_deck();
            for (request, conn, token) in [(61, &mut alice, alice_token), (62, &mut bob, bob_token)]
            {
                let out = broker.handle(
                    conn,
                    LobbyClientMessage::SubmitTournamentDeck {
                        code: code.clone(),
                        player_token: token,
                        deck: max_deck.clone(),
                        request_id: Some(TournamentRequestId(request)),
                    },
                    &env,
                );
                assert_eq!(
                    ack_of(&out).0,
                    TournamentRequestId(request),
                    "metadata growth must not have consumed the deck guarantee"
                );
            }
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);
            assert_eq!(
                metadata_bytes(&broker, &code),
                metadata,
                "deck traffic changes only the deck account"
            );
        }

        /// The deck account is exact: whatever decks an event retains, the rest of
        /// its record (its metadata) encodes to the same size, so a submission or a
        /// resubmission can never be charged to the metadata allowance.
        #[test]
        fn a_deck_submission_changes_only_the_deck_account() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            put_deck(&mut broker, &env, &code, "a", 1);
            let baseline = metadata_bytes(&broker, &code);
            for chars in [1_000, 61_000, 7] {
                put_deck(&mut broker, &env, &code, "a", chars);
                assert_eq!(metadata_bytes(&broker, &code), baseline);
            }
        }

        /// The capacity guarantee. With the shared store at exactly its ceiling:
        /// an admitted event that is still within its reservation keeps its full
        /// reservation — a registration and a maximum-size deck are both admitted
        /// — while an event past its reservation and a new admission cannot take
        /// that room. The lobby keeps its own reservation likewise.
        #[test]
        fn an_admitted_event_keeps_its_reservation_when_the_store_is_full() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            // Admitted before the store fills, still tiny.
            let (quiet, _) =
                make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            // A greedy event, past its reservation.
            let (greedy, _) =
                make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            inflate(&mut broker, &greedy);
            let _filler = fill_to_headroom(&mut broker, &env, 0);
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);

            // The greedy event cannot take another byte of anyone's room.
            let mut intruder = ConnState::default();
            let before = serialized(&broker);
            let before_conn = intruder.clone();
            let out = broker.handle(&mut intruder, join_frame(&greedy, "g2"), &env);
            assert_refused_intact(&out, &before, &broker, &before_conn, &intruder);

            // Nor can a new admission.
            let mut newcomer = ConnState::default();
            let out = broker.handle(&mut newcomer, create_frame(), &env);
            assert!(
                error_reason(&out).contains("tournament registry is full")
                    || error_reason(&out).contains("at capacity"),
                "{out:?}"
            );

            // The unrelated, still-reserved event is unaffected: it registers both
            // entrants of a head-to-head pairing and takes a maximum-size deck
            // from each, within its reservation.
            let max_deck = wire_max_deck();
            let encoded = serde_json::to_vec(&max_deck).expect("encodes").len();
            assert!(
                encoded > MAX_SINGLE_DECK_ENCODED_BYTES - 8 * 1024
                    && encoded <= MAX_SINGLE_DECK_ENCODED_BYTES,
                "fixture: a maximum-size valid deck ({encoded} bytes)"
            );
            for (seat, (key, name)) in [("q", "Quinn"), ("r", "Rae")].into_iter().enumerate() {
                let mut entrant = ConnState::default();
                let token = join_tournament(&mut entrant, &mut broker, &env, &quiet, key, name);
                let request = 31 + seat as u64;
                let out = broker.handle(
                    &mut entrant,
                    LobbyClientMessage::SubmitTournamentDeck {
                        code: quiet.clone(),
                        player_token: token,
                        deck: max_deck.clone(),
                        request_id: Some(TournamentRequestId(request)),
                    },
                    &env,
                );
                assert_eq!(
                    ack_of(&out).0,
                    TournamentRequestId(request),
                    "an admitted event's valid submissions fit even at the ceiling ({key})"
                );
            }
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);
            assert!(serialized(&broker).len() <= MAX_SNAPSHOT_ENCODED_BYTES);

            // Lobby listings keep their reservation: a host can still register.
            let mut host = ConnState::default();
            hello(&mut host, &mut broker, &env);
            let out = create(&mut host, &mut broker, &env);
            assert!(
                host.host_game.is_some(),
                "a lobby listing is admitted at the ceiling: {out:?}"
            );
        }

        /// A deck that passes the inbound wire guard and whose
        /// [`retained_deck_charge`] is exactly `charge` bytes: a full main deck of
        /// 120-byte names, topped up with sideboard names, then trimmed one byte
        /// per main-deck name. `fill` varies the content between submissions.
        fn deck_charging(charge: usize, fill: char) -> DeckData {
            let name = fill.to_string().repeat(120);
            let mut deck = DeckData {
                main_deck: vec![name.clone(); MAX_MAIN_DECK_ENTRIES],
                ..Default::default()
            };
            while retained_deck_charge(&deck) < charge {
                deck.sideboard.push(name.clone());
            }
            let over = retained_deck_charge(&deck) - charge;
            for name in deck.main_deck.iter_mut().take(over) {
                name.pop();
            }
            assert_eq!(retained_deck_charge(&deck), charge, "fixture: exact charge");
            deck
        }

        /// Deck admission and the deck reservation charge the same unit. With the
        /// shared store at its ceiling, an admitted event takes eight decks at the
        /// per-deck limit through `Broker::handle` (its whole advertised deck
        /// allowance) and an exact-limit resubmission, all inside its reservation.
        /// A resubmission one byte over the limit is refused by the per-deck cap
        /// and a ninth entrant's deck by the event budget, each with its own
        /// reason and prior state intact, never by the capacity boundary.
        /// REVERT-FAIL: gate admission on the bare deck encoding while the
        /// reservation also charges the `deck` member, and the one-byte-over
        /// resubmission passes both admission gates and is refused at capacity.
        #[test]
        fn exact_limit_decks_fill_the_event_allowance_at_the_ceiling() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            let mut entrants: Vec<(ConnState, String)> = (0..9)
                .map(|seat| {
                    let mut conn = ConnState::default();
                    let key = format!("p{seat}");
                    let token = join_tournament(&mut conn, &mut broker, &env, &code, &key, &key);
                    (conn, token)
                })
                .collect();
            let _filler = fill_to_headroom(&mut broker, &env, 0);
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);

            let submit = |broker: &mut Broker,
                          (conn, token): &mut (ConnState, String),
                          deck: DeckData,
                          request: u64| {
                broker.handle(
                    conn,
                    LobbyClientMessage::SubmitTournamentDeck {
                        code: code.clone(),
                        player_token: token.clone(),
                        deck,
                        request_id: Some(TournamentRequestId(request)),
                    },
                    &env,
                )
            };

            for (seat, entrant) in entrants.iter_mut().take(8).enumerate() {
                let request = 40 + seat as u64;
                let deck = deck_charging(MAX_SINGLE_DECK_ENCODED_BYTES, 'y');
                let out = submit(&mut broker, entrant, deck, request);
                assert_eq!(
                    ack_of(&out).0,
                    TournamentRequestId(request),
                    "exact-limit deck {seat} is inside the event's allowance"
                );
            }
            let event = broker.tournaments().get(&code).expect("event");
            assert_eq!(
                event.retained_deck_bytes(),
                EVENT_DECK_RESERVE_ENCODED_BYTES
            );
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);

            let resubmit = deck_charging(MAX_SINGLE_DECK_ENCODED_BYTES, 'z');
            let out = submit(&mut broker, &mut entrants[0], resubmit, 50);
            assert_eq!(
                ack_of(&out).0,
                TournamentRequestId(50),
                "an exact-limit resubmission replaces its own charge"
            );
            assert_eq!(broker.committed_bytes(), MAX_SNAPSHOT_ENCODED_BYTES);

            let before = serialized(&broker);
            let over = deck_charging(MAX_SINGLE_DECK_ENCODED_BYTES + 1, 'y');
            let out = submit(&mut broker, &mut entrants[0], over, 51);
            let (request, reason) = rejection_of(&out);
            assert_eq!(request, TournamentRequestId(51));
            assert!(reason.contains("per-deck limit"), "{reason}");
            assert_eq!(serialized(&broker), before);

            let out = submit(&mut broker, &mut entrants[8], deck_of(1), 52);
            let (request, reason) = rejection_of(&out);
            assert_eq!(request, TournamentRequestId(52));
            assert!(reason.contains("this event's deck budget"), "{reason}");
            assert_eq!(serialized(&broker), before);
        }

        /// The boundary refuses *growth*, never a write that does not enlarge the
        /// committed state: a state restored over the ceiling (for example from a
        /// snapshot written by an older build) can still be answered, and its own
        /// refusals keep their specific reasons rather than a capacity error.
        #[test]
        fn a_write_that_does_not_grow_is_never_refused_for_capacity() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            let mut alice = ConnState::default();
            join_tournament(&mut alice, &mut broker, &env, &code, "a", "Alice");
            inflate(&mut broker, &code);
            let mut filler = fill_to_headroom(&mut broker, &env, 0);
            // Over the ceiling, as a legacy snapshot could be.
            let chars = filler.chars + 4_000;
            filler.set_name_chars(&mut broker, chars);
            assert!(broker.committed_bytes() > MAX_SNAPSHOT_ENCODED_BYTES);

            let mut impostor = ConnState::default();
            let out = broker.handle(&mut impostor, join_frame(&code, "a"), &env);
            assert!(
                error_reason(&out).contains("already joined"),
                "a no-op refusal keeps its own reason: {out:?}"
            );
        }

        /// A broker the previous writer left far past today's admission budget,
        /// sitting `gap` bytes under the physical single-key ceiling, serialized
        /// and restored the way a cold start does. Returns it with the code of a
        /// small event that still holds an unused reservation (the growth room).
        fn restored_near_the_physical_ceiling(env: &FakeEnv, gap: usize) -> (Broker, String) {
            let mut organizer = ConnState::default();
            let mut broker = Broker::new();
            let mut codes = Vec::new();
            for _ in 0..MAX_TOURNAMENT_ENTRIES {
                codes
                    .push(make_tournament(&mut organizer, &mut broker, env, BracketShape::Swiss).0);
            }
            let victim = codes.pop().expect("a spare event");
            let target = PHYSICAL_SNAPSHOT_LIMIT_BYTES - gap;
            // The previous writer's events had grown past their allowances: spread
            // the bulk over their names, then tune the last one to the exact byte.
            let pad = (target - broker.snapshot_encoded_bytes()) / codes.len();
            for code in &codes {
                set_event_name_chars(&mut broker, code, pad);
            }
            let last = codes.last().expect("an event to tune");
            let short = target - broker.snapshot_encoded_bytes();
            set_event_name_chars(&mut broker, last, pad + short);
            assert_eq!(
                broker.snapshot_encoded_bytes(),
                target,
                "fixture lands on target"
            );

            let restored: Broker =
                serde_json::from_str(&serialized(&broker)).expect("snapshot restores");
            assert!(
                restored.committed_bytes() > MAX_SNAPSHOT_ENCODED_BYTES,
                "reach guard: the restored snapshot is over today's admission budget"
            );
            (restored, victim)
        }

        /// A submission whose every card name is non-ASCII (`Ā`: 2 UTF-8 bytes, 6
        /// as the stored `\u0100` escape), ~`chars` stored bytes in all. It is
        /// the input whose UTF-8 length understates what storage writes.
        fn submit_frame_unicode(
            code: &str,
            token: &str,
            chars: usize,
            request: u64,
        ) -> LobbyClientMessage {
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.to_string(),
                player_token: token.to_string(),
                deck: DeckData {
                    main_deck: vec!["\u{0100}".repeat(50); chars / 300],
                    ..Default::default()
                },
                request_id: Some(TournamentRequestId(request)),
            }
        }

        fn submit_frame(code: &str, token: &str, chars: usize, request: u64) -> LobbyClientMessage {
            LobbyClientMessage::SubmitTournamentDeck {
                code: code.to_string(),
                player_token: token.to_string(),
                // Entries within the inbound wire guard, ~chars bytes in all.
                deck: DeckData {
                    main_deck: vec!["y".repeat(100); chars / 100],
                    ..Default::default()
                },
                request_id: Some(TournamentRequestId(request)),
            }
        }

        /// F1: a snapshot restored over the admission budget still loads, growth
        /// inside an unused reservation cannot carry the real encoded value past
        /// the physical key ceiling (refused with its request id, prior state
        /// intact), and a write that shrinks the value always succeeds.
        #[test]
        fn a_restored_over_budget_snapshot_cannot_grow_past_the_physical_ceiling() {
            let env = FakeEnv::new();
            let (mut broker, victim) = restored_near_the_physical_ceiling(&env, 20_000);
            assert!(broker.snapshot_encoded_bytes() < PHYSICAL_SNAPSHOT_LIMIT_BYTES);

            let mut alice = ConnState::default();
            let token = join_tournament(&mut alice, &mut broker, &env, &victim, "a", "Alice");
            let out = broker.handle(&mut alice, submit_frame(&victim, &token, 10_000, 41), &env);
            assert_eq!(
                ack_of(&out).0,
                TournamentRequestId(41),
                "growth that stays under the physical ceiling is still served"
            );

            // This resubmission would add ~20 KB to the value, past the ceiling.
            let before = serialized(&broker);
            let before_conn = alice.clone();
            let out = broker.handle(&mut alice, submit_frame(&victim, &token, 30_000, 42), &env);
            assert_eq!(serialized(&broker), before, "the prior snapshot is intact");
            assert_eq!(alice, before_conn);
            let (id, reason) = rejection_of(&out);
            assert_eq!(id, TournamentRequestId(42), "refusal keeps its correlation");
            assert!(reason.contains("at capacity"), "{reason}");
            assert!(broker.snapshot_encoded_bytes() <= PHYSICAL_SNAPSHOT_LIMIT_BYTES);

            // A write that reduces the encoded size always succeeds.
            let size_before = broker.snapshot_encoded_bytes();
            let out = broker.handle(&mut alice, submit_frame(&victim, &token, 2_000, 43), &env);
            assert_eq!(ack_of(&out).0, TournamentRequestId(43));
            assert!(broker.snapshot_encoded_bytes() < size_before);
        }

        /// The physical ceiling leaves room for the storage key: a restored
        /// over-budget snapshot may grow to exactly [`PHYSICAL_SNAPSHOT_LIMIT_BYTES`]
        /// of value, whose `put` together with [`SNAPSHOT_STORAGE_KEY`] fills the
        /// store's decimal 8 MB, and one byte more is refused with its request id
        /// and the prior state intact; a write that shrinks the value still
        /// succeeds from the boundary.
        #[test]
        fn the_physical_ceiling_counts_the_storage_key_at_the_exact_boundary() {
            assert_eq!(
                PHYSICAL_SNAPSHOT_LIMIT_BYTES
                    + SNAPSHOT_STORAGE_KEY.len()
                    + V8_STRING_FRAMING_BYTES,
                8_000_000,
                "key, value and V8 framing together fill exactly the decimal 8 MB"
            );
            assert_physical_boundary(submit_frame);
        }

        /// The same exact boundary for a submission whose card names are
        /// non-ASCII, measured as the stored ASCII-only escape form: UTF-8 length
        /// would admit it up to three times too large. The stored text is also
        /// pure ASCII, so its string length equals the measured bytes and V8 keeps
        /// it as a one-byte string (no two-bytes-per-unit expansion).
        #[test]
        fn the_physical_ceiling_bounds_unicode_input_in_its_stored_form() {
            assert_physical_boundary(submit_frame_unicode);

            let env = FakeEnv::new();
            let (mut broker, victim) = restored_near_the_physical_ceiling(&env, 20_000);
            let mut alice = ConnState::default();
            let token = join_tournament(&mut alice, &mut broker, &env, &victim, "a", "Alice");
            let out = broker.handle(
                &mut alice,
                submit_frame_unicode(&victim, &token, 9_000, 7),
                &env,
            );
            ack_of(&out);
            let stored = broker.snapshot_json();
            assert!(stored.is_ascii(), "the stored text is ASCII-only");
            assert!(stored.contains(r"\u0100"));
            assert_eq!(
                stored.len(),
                broker.snapshot_encoded_bytes(),
                "the bytes measured are exactly the bytes stored"
            );
            // Accepted Unicode input persists and restores.
            let restored: Broker = serde_json::from_str(&stored).expect("snapshot restores");
            // Map iteration order is not stable across restores, so compare as values.
            assert_eq!(
                serde_json::to_value(&restored).expect("value"),
                serde_json::to_value(&broker).expect("value"),
                "restore is lossless"
            );
            assert_eq!(restored.snapshot_encoded_bytes(), stored.len());
            let deck = restored
                .tournaments()
                .get(&victim)
                .and_then(|t| t.players.iter().find_map(|p| p.deck.as_ref()))
                .expect("the submitted deck survives the round trip");
            assert!(deck.main_deck.iter().all(|c| c == &"\u{0100}".repeat(50)));
        }

        /// A snapshot written before the ASCII-only codec holds raw UTF-8 (the old
        /// `serde_json::to_string`); it must still restore, and the next write
        /// re-emits it ASCII-only.
        #[test]
        fn a_legacy_raw_utf8_snapshot_still_restores() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            broker
                .tournaments_mut()
                .fixture_meta_mut(&code)
                .expect("event")
                .name = "Ā \u{1F409} 日本".to_string();
            let legacy = serde_json::to_string(&broker).expect("legacy encoding");
            assert!(!legacy.is_ascii(), "the legacy form holds raw non-ASCII");
            let restored: Broker = serde_json::from_str(&legacy).expect("legacy restores");
            assert_eq!(
                restored.tournaments().get(&code).expect("event").name,
                "Ā \u{1F409} 日本"
            );
            assert!(restored.snapshot_json().is_ascii());
        }

        // ---- Genuine v0.126.0 snapshot shape -------------------------------
        //
        // The fixtures below are hand-written JSON in the shape the *released*
        // v0.126.0 writer stored (`git show v0.126.0:crates/lobby-broker/src/`):
        // a `TournamentPlayer` is `{player_key, player_token, display_name,
        // dropped}` with NO `deck`, and everything else is spelled out literally.
        // They are deliberately not produced by serializing today's structs: a
        // fixture built that way can never notice a field a later version adds and
        // writes back, which is exactly how a stored snapshot grows on restore.

        const RELEASED_CODE: &str = "ABCDEF";

        /// Secret of released entrant `index`, 32 characters like a minted token.
        fn released_secret(index: usize) -> String {
            format!("{index:032}")
        }

        /// One v0.126.0 entrant: 20 `Ā` plus five digits for the key (45 UTF-8
        /// bytes), 20 `Ā` for the display name.
        fn released_entrant(index: usize) -> String {
            let accent = "\u{0100}".repeat(20);
            format!(
                "{{\"player_key\":\"{accent}{index:05}\",\
                 \"player_token\":{{\"secret\":\"{}\",\"expires_at_ms\":9000000,\"last_rotation\":null}},\
                 \"display_name\":\"{accent}\",\"dropped\":false}}",
                released_secret(index)
            )
        }

        /// A complete v0.126.0 broker snapshot: an empty lobby and one
        /// registration-stage event of `entrants` Unicode entrants whose name is
        /// `name_chars` copies of `Ā`, in raw UTF-8 as the released writer stored it.
        fn released_snapshot(entrants: usize, name_chars: usize) -> String {
            let players = (0..entrants)
                .map(released_entrant)
                .collect::<Vec<_>>()
                .join(",");
            let name = "\u{0100}".repeat(name_chars);
            format!(
                "{{\"lobby\":{{\"games\":{{}},\"next_generation\":0}},\
                 \"tournaments\":{{\"tournaments\":{{\"{RELEASED_CODE}\":{{\
                 \"code\":\"{RELEASED_CODE}\",\"name\":\"{name}\",\
                 \"organizer_token\":{{\"secret\":\"{}\",\"expires_at_ms\":9000000,\"last_rotation\":null}},\
                 \"arity\":2,\
                 \"scoring\":{{\"win_points\":3,\"draw_points\":1,\"loss_points\":0}},\
                 \"bracket\":\"Swiss\",\"total_rounds_override\":null,\
                 \"resolved_total_rounds\":null,\"plus_rounds\":null,\"format\":null,\
                 \"match_type\":\"Bo3\",\"current_round\":0,\"status\":\"Registration\",\
                 \"players\":[{players}],\"pairings\":[],\"hosted\":{{}},\
                 \"created_at\":1000,\"last_activity_at\":1000}}}}}}}}",
                released_secret(999_999)
            )
        }

        /// What the store holds for `text` (V8 framing aside), from the one
        /// authority the broker itself measures with.
        fn stored_bytes_of(text: &str) -> usize {
            crate::snapshot_codec::v8_string_bytes(text)
        }

        /// A released snapshot whose raw stored size sits `gap` bytes under the
        /// physical limit while its ASCII-escaped form is far over it. Sizing is
        /// linear in the event name, so one probe lands it within a code unit of
        /// the target.
        fn released_snapshot_near_the_limit(gap: usize) -> String {
            let entrants = 15_000;
            let probe = released_snapshot(entrants, 1_000);
            let per_char =
                stored_bytes_of(&released_snapshot(entrants, 1_001)) - stored_bytes_of(&probe);
            let target = PHYSICAL_SNAPSHOT_LIMIT_BYTES - gap;
            let name_chars = 1_000 + (target - stored_bytes_of(&probe)) / per_char;
            let text = released_snapshot(entrants, name_chars);
            let stored = stored_bytes_of(&text);
            assert!(
                (target - per_char..=target).contains(&stored),
                "fixture lands on target: {stored} vs {target}"
            );
            text
        }

        fn restore_released(text: &str) -> Broker {
            serde_json::from_str(text).expect("a released snapshot restores")
        }

        /// F1 (a): restoring a genuine v0.126.0 snapshot and writing it back adds
        /// no key the released writer did not write. The raw text is identical,
        /// so the restored snapshot can never be larger than the stored one, and
        /// no state is lost.
        #[test]
        fn a_released_snapshot_reserializes_byte_for_byte() {
            let released = released_snapshot(25, 40);
            assert!(!released.contains("\"deck\""), "released shape has no deck");
            let restored = restore_released(&released);
            assert_eq!(
                serde_json::to_string(&restored).expect("raw encoding"),
                released,
                "no added keys, no reordered fields"
            );
            let event = restored.tournaments().get(RELEASED_CODE).expect("event");
            assert_eq!(event.players.len(), 25);
            assert!(event.players.iter().all(|p| p.deck.is_none() && !p.dropped));
            assert_eq!(event.name, "\u{0100}".repeat(40));

            // The stored form never exceeds what was stored, and is lossless.
            let stored =
                crate::snapshot_codec::stored_snapshot(&restored, usize::MAX).expect("encodes");
            assert!(stored.stored_bytes <= stored_bytes_of(&released));
            let again: Broker = serde_json::from_str(&stored.text).expect("restores");
            assert_eq!(
                serde_json::to_value(&again).expect("value"),
                serde_json::to_value(&restored).expect("value")
            );
        }

        /// F1 (b): a released Unicode snapshot near the physical limit restores
        /// into the representation it was stored in (raw, since its escaped form is
        /// far over the limit), still fits storage, and loses nothing.
        #[test]
        fn a_released_unicode_snapshot_near_the_limit_stays_storable() {
            let released = released_snapshot_near_the_limit(1_000);
            let original_bytes = stored_bytes_of(&released);
            assert!(original_bytes <= PHYSICAL_SNAPSHOT_LIMIT_BYTES);
            assert!(
                crate::snapshot_codec::to_ascii_json_string(&restore_released(&released))
                    .expect("ascii")
                    .len()
                    > PHYSICAL_SNAPSHOT_LIMIT_BYTES,
                "reach guard: the escaped form would not fit"
            );

            let restored = restore_released(&released);
            assert_eq!(
                restored.snapshot_json(),
                released,
                "the stored text is what was stored"
            );
            assert!(restored.snapshot_encoded_bytes() <= original_bytes);
            assert!(restored.snapshot_fits_storage());
            assert_eq!(
                restored
                    .tournaments()
                    .get(RELEASED_CODE)
                    .expect("event")
                    .players
                    .len(),
                15_000,
                "no entrant lost"
            );
        }

        /// F1 (c): on that restored, near-capacity state a guard-valid, correlated
        /// `SubmitTournamentDeck` with an invalid token is refused without touching
        /// state. It is delivered as its own correlated refusal and is NOT reported
        /// dirty, so no write (let alone an oversized one) follows it.
        #[test]
        fn a_refused_message_on_a_restored_released_snapshot_is_not_dirty() {
            let env = FakeEnv::new();
            let mut broker = restore_released(&released_snapshot_near_the_limit(1_000));
            let before = broker.snapshot_json();
            let mut conn = ConnState::default();
            let handled = broker.handle_reporting(
                &mut conn,
                LobbyClientMessage::SubmitTournamentDeck {
                    code: RELEASED_CODE.to_string(),
                    player_token: "f".repeat(32),
                    deck: DeckData {
                        main_deck: vec!["Island".to_string()],
                        ..Default::default()
                    },
                    request_id: Some(TournamentRequestId(77)),
                },
                &env,
            );
            assert_eq!(handled.persisted, PersistedChange::Unchanged);
            let (id, _) = rejection_of(&handled.outbounds);
            assert_eq!(id, TournamentRequestId(77), "the refusal is correlated");
            assert_eq!(broker.snapshot_json(), before, "state is unchanged");
            assert!(broker.snapshot_fits_storage());
        }

        /// F1: every admitted, dirty result stays storable. On the restored
        /// near-capacity state growth past the limit is refused with its request id
        /// and the prior state intact, while a successful storable reduction (an
        /// entrant dropping) is still admitted and reported dirty.
        #[test]
        fn a_restored_released_snapshot_refuses_growth_and_admits_reductions() {
            let env = FakeEnv::new();
            let mut broker = restore_released(&released_snapshot_near_the_limit(1_000));
            let token = released_secret(3);

            // A deck far larger than the 1 kB left would push the value past the
            // physical limit: refused, correlated, nothing changed.
            let before = broker.snapshot_json();
            let mut conn = ConnState::default();
            let handled = broker.handle_reporting(
                &mut conn,
                LobbyClientMessage::SubmitTournamentDeck {
                    code: RELEASED_CODE.to_string(),
                    player_token: token.clone(),
                    deck: DeckData {
                        main_deck: vec!["y".repeat(100); 100],
                        ..Default::default()
                    },
                    request_id: Some(TournamentRequestId(81)),
                },
                &env,
            );
            let (id, reason) = rejection_of(&handled.outbounds);
            assert_eq!(id, TournamentRequestId(81));
            assert!(reason.contains("at capacity"), "{reason}");
            assert_eq!(handled.persisted, PersistedChange::Unchanged);
            assert_eq!(broker.snapshot_json(), before, "the prior state is intact");

            // Dropping reduces the stored size: admitted, dirty, still storable.
            let size_before = broker.snapshot_encoded_bytes();
            let handled = broker.handle_reporting(
                &mut conn,
                LobbyClientMessage::DropFromTournament {
                    code: RELEASED_CODE.to_string(),
                    player_token: token,
                    request_id: Some(TournamentRequestId(82)),
                },
                &env,
            );
            assert_eq!(ack_of(&handled.outbounds).0, TournamentRequestId(82));
            assert_eq!(handled.persisted, PersistedChange::Changed);
            assert!(broker.snapshot_encoded_bytes() < size_before);
            assert!(broker.snapshot_fits_storage());
        }

        /// A broker whose every form exceeds the store is reported unstorable
        /// rather than silently written.
        #[test]
        fn a_snapshot_over_the_limit_in_every_form_is_not_storable() {
            let env = FakeEnv::new();
            let mut organizer = ConnState::default();
            let mut broker = Broker::new();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            set_event_name_chars(&mut broker, &code, PHYSICAL_SNAPSHOT_LIMIT_BYTES);
            assert!(!broker.snapshot_fits_storage());
        }

        fn assert_physical_boundary(
            submit_frame: fn(&str, &str, usize, u64) -> LobbyClientMessage,
        ) {
            let env = FakeEnv::new();
            let (mut broker, victim) = restored_near_the_physical_ceiling(&env, 20_000);
            let mut alice = ConnState::default();
            let token = join_tournament(&mut alice, &mut broker, &env, &victim, "a", "Alice");
            let restore = |broker: &Broker| -> Broker {
                serde_json::from_str(&serialized(broker)).expect("snapshot restores")
            };

            // The submission's exact growth, measured on a restored copy.
            let growth = {
                let mut probe = restore(&broker);
                let mut conn = alice.clone();
                let before = probe.snapshot_encoded_bytes();
                let out = probe.handle(&mut conn, submit_frame(&victim, &token, 10_000, 40), &env);
                ack_of(&out);
                probe.snapshot_encoded_bytes() - before
            };
            // Tune the victim's name so the same submission lands `over` bytes past
            // the ceiling, then restore the result the way a cold start would.
            let land_past_ceiling = |broker: &Broker, over: usize| -> Broker {
                let mut tuned = restore(broker);
                let name = tuned.tournaments().get(&victim).expect("event").name.len();
                let target = PHYSICAL_SNAPSHOT_LIMIT_BYTES + over - growth;
                let chars = name + target - tuned.snapshot_encoded_bytes();
                set_event_name_chars(&mut tuned, &victim, chars);
                assert_eq!(
                    tuned.snapshot_encoded_bytes(),
                    target,
                    "fixture lands on target"
                );
                let restored = restore(&tuned);
                assert!(
                    restored.committed_bytes() > MAX_SNAPSHOT_ENCODED_BYTES,
                    "reach guard: the boundary is reached only past the admission budget"
                );
                restored
            };

            // One byte past the ceiling: refused, correlated, nothing changed.
            let mut broker = land_past_ceiling(&broker, 1);
            let before = serialized(&broker);
            let before_conn = alice.clone();
            let out = broker.handle(&mut alice, submit_frame(&victim, &token, 10_000, 41), &env);
            assert_eq!(serialized(&broker), before, "the prior snapshot is intact");
            assert_eq!(alice, before_conn);
            let (id, reason) = rejection_of(&out);
            assert_eq!(id, TournamentRequestId(41), "refusal keeps its correlation");
            assert!(reason.contains("at capacity"), "{reason}");

            // Exactly at the ceiling: admitted, and the value fills it to the byte.
            let mut broker = land_past_ceiling(&broker, 0);
            let out = broker.handle(&mut alice, submit_frame(&victim, &token, 10_000, 42), &env);
            assert_eq!(ack_of(&out).0, TournamentRequestId(42));
            assert_eq!(
                broker.snapshot_encoded_bytes(),
                PHYSICAL_SNAPSHOT_LIMIT_BYTES
            );

            // From the boundary, a write that reduces the encoded size succeeds.
            let out = broker.handle(&mut alice, submit_frame(&victim, &token, 2_000, 43), &env);
            assert_eq!(ack_of(&out).0, TournamentRequestId(43));
            assert!(broker.snapshot_encoded_bytes() < PHYSICAL_SNAPSHOT_LIMIT_BYTES);
        }

        /// F4: a request for a tournament that does not exist is refused with its
        /// correlated error, reported as changing nothing, and costs no
        /// whole-broker checkpoint or accounting pass, however much state the
        /// broker retains.
        #[test]
        fn a_missing_tournament_request_is_refused_without_admission_work() {
            let env = FakeEnv::new();
            let mut broker = Broker::new();
            let mut organizer = ConnState::default();
            let (code, _) = make_tournament(&mut organizer, &mut broker, &env, BracketShape::Swiss);
            inflate(&mut broker, &code);
            let before = serialized(&broker);

            let mut conn = ConnState::default();
            let passes = ACCOUNTING_PASSES.with(std::cell::Cell::get);
            let handled = broker.handle_reporting(&mut conn, join_frame("NOSUCH", "z"), &env);
            assert_eq!(
                ACCOUNTING_PASSES.with(std::cell::Cell::get),
                passes,
                "no admission or accounting pass ran"
            );
            assert_eq!(handled.persisted, PersistedChange::Unchanged);
            assert!(
                error_reason(&handled.outbounds).contains("not found"),
                "{:?}",
                handled.outbounds
            );

            // A correlated gated frame keeps its request id.
            let handled =
                broker.handle_reporting(&mut conn, submit_frame("NOSUCH", "tok", 10, 51), &env);
            assert_eq!(handled.persisted, PersistedChange::Unchanged);
            assert_eq!(rejection_of(&handled.outbounds).0, TournamentRequestId(51));
            assert_eq!(serialized(&broker), before);

            // Control: the same frame against a real event is a real write.
            let passes = ACCOUNTING_PASSES.with(std::cell::Cell::get);
            let handled = broker.handle_reporting(&mut conn, join_frame(&code, "z"), &env);
            assert_eq!(handled.persisted, PersistedChange::Changed);
            assert!(ACCOUNTING_PASSES.with(std::cell::Cell::get) > passes);
        }
    }
}
