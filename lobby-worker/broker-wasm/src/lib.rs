//! wasm-bindgen bindings exposing the `lobby-broker` core to the Cloudflare
//! Durable Object shell (`lobby-worker/src/lobby-do.ts`).
//!
//! Mirrors the `engine` -> `engine-wasm` pattern: the pure broker lives in
//! `lobby-broker`; this crate is the thin transport boundary that (de)serializes
//! across the JS/WASM line and injects the runtime `BrokerEnv`. All protocol
//! parsing and dispatch stays in Rust — the TS shell only forwards raw frames
//! and interprets the returned `Outbound` side effects.
//!
//! State model: a hibernated DO loses memory, so the shell snapshots the whole
//! broker to DO storage after each mutating call ([`WasmBroker::snapshot`]) and
//! restores it on cold start ([`WasmBroker::from_snapshot`]). Per-connection
//! [`ConnState`] rides in the WebSocket attachment, round-tripped as JSON.

use lobby_broker::{
    parse_lobby_client_message, Broker, BrokerEnv, ConnState, LobbyClientMessage,
    LobbyServerMessage, Outbound, ParsedFrame, PROTOCOL_VERSION,
};
use rand::Rng;
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// [`BrokerEnv`] for the Cloudflare Worker runtime. Wall-clock is injected
/// per-call from JS `Date.now()`; randomness uses getrandom's JS-crypto backend
/// (`globalThis.crypto`) via `rand`. `new_token`/`new_game_code` mirror
/// `server_core::generate_player_token`/`generate_game_code` exactly so codes and
/// tokens are format-identical to the native phase-server shell.
struct WorkerEnv {
    now_ms: u64,
}

impl BrokerEnv for WorkerEnv {
    fn now_ms(&self) -> u64 {
        self.now_ms
    }

    fn new_token(&self) -> String {
        let mut rng = rand::rng();
        (0..32)
            .map(|_| format!("{:x}", rng.random_range(0u8..16)))
            .collect()
    }

    fn new_game_code(&self) -> String {
        let mut rng = rand::rng();
        let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect();
        (0..6)
            .map(|_| chars[rng.random_range(0..chars.len())])
            .collect()
    }
}

/// Transport-neutral view of an [`Outbound`] for the TS shell. The core enum
/// mixes newtype variants (carrying a `LobbyServerMessage`) and unit variants,
/// which serde would render heterogeneously; this flattens them to a uniform
/// `{ kind, msg? }` shape the shell can `switch` on. Lives here, not in the
/// core, because it is purely a boundary concern.
#[derive(Serialize)]
#[serde(tag = "kind")]
enum OutboundDto {
    ToSelf { msg: LobbyServerMessage },
    ToSubscribers { msg: LobbyServerMessage },
    AddSubscriber,
    RemoveSubscriber,
    SendPlayerCountToSelf,
}

impl From<Outbound> for OutboundDto {
    fn from(o: Outbound) -> Self {
        match o {
            Outbound::ToSelf(msg) => OutboundDto::ToSelf { msg },
            Outbound::ToSubscribers(msg) => OutboundDto::ToSubscribers { msg },
            Outbound::AddSubscriber => OutboundDto::AddSubscriber,
            Outbound::RemoveSubscriber => OutboundDto::RemoveSubscriber,
            Outbound::SendPlayerCountToSelf => OutboundDto::SendPlayerCountToSelf,
        }
    }
}

fn to_dtos(outs: Vec<Outbound>) -> Vec<OutboundDto> {
    outs.into_iter().map(OutboundDto::from).collect()
}

/// Single `Error` reply for a frame rejected at the parse/validation boundary.
/// Sent to the originating socket so the client's pending RPC fails fast rather
/// than waiting out its timeout. Malformed/unknown frames never reach
/// `Broker::handle`, so this boundary crate is the only place that can answer
/// them.
fn reject_reply(message: &str) -> Vec<Outbound> {
    vec![Outbound::ToSelf(LobbyServerMessage::error(message))]
}

/// Whether a client frame can mutate the shared `LobbyManager` (and therefore
/// requires the shell to re-snapshot it to DO storage). Conservative: read-only
/// frames return `false` so a periodic `Ping`/`SubscribeLobby` never triggers a
/// storage write (Subscribe only flips per-socket `ConnState`, which lives in
/// the WS attachment, not storage). Exhaustive so a new protocol variant forces
/// a deliberate classification here.
fn mutates_lobby(msg: &LobbyClientMessage) -> bool {
    match msg {
        LobbyClientMessage::CreateGameWithSettings { .. }
        | LobbyClientMessage::JoinGameWithPassword { .. }
        | LobbyClientMessage::LookupJoinTarget { .. }
        | LobbyClientMessage::UpdateLobbyMetadata { .. }
        | LobbyClientMessage::UnregisterLobby { .. }
        // Every tournament write lands in the `TournamentManager` the broker
        // snapshot carries, so each must mark the DO dirty. Classifying one of
        // these `false` would lose the write on the next hibernation —
        // silently, and only for tournaments, since lobby traffic would keep
        // re-snapshotting around it.
        | LobbyClientMessage::CreateTournament { .. }
        | LobbyClientMessage::JoinTournament { .. }
        | LobbyClientMessage::StartTournamentRound { .. }
        | LobbyClientMessage::ReportMatchResult { .. }
        | LobbyClientMessage::DropFromTournament { .. }
        | LobbyClientMessage::EndTournament { .. } => true,
        // `GetTournament` is a pure read, like `SubscribeLobby`: classifying
        // it `true` would write storage on every poll of a public listing.
        LobbyClientMessage::GetTournament { .. }
        | LobbyClientMessage::ClientHello { .. }
        | LobbyClientMessage::SubscribeLobby
        | LobbyClientMessage::UnsubscribeLobby
        | LobbyClientMessage::Ping { .. } => false,
    }
}

/// Result of a connection-scoped broker call ([`WasmBroker::handle`] /
/// [`WasmBroker::on_disconnect`]). `conn` is the post-call per-socket state to
/// write back to the WebSocket attachment.
#[derive(Serialize)]
struct CallResult {
    conn: ConnState,
    outbounds: Vec<OutboundDto>,
    /// `true` when the shared lobby state changed and the shell must re-snapshot
    /// it to DO storage. `false` for read-only frames (avoids a storage write on
    /// every `Ping`/`SubscribeLobby`).
    dirty: bool,
    /// Set when a frame was unknown/malformed; the shell logs it and drops the
    /// frame (no outbounds). `None` on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    reject: Option<String>,
}

/// The compiled Rust broker, owned by one Durable Object instance.
#[wasm_bindgen]
pub struct WasmBroker {
    inner: Broker,
}

#[wasm_bindgen]
impl WasmBroker {
    /// Fresh empty broker — cold start with no stored snapshot.
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmBroker {
        WasmBroker {
            inner: Broker::new(),
        }
    }

    /// Restore from a DO-storage snapshot. Falls back to an empty broker if the
    /// snapshot is absent or from an incompatible older format — a lobby reset
    /// (entries are ephemeral and short-lived) is preferable to failing to boot.
    pub fn from_snapshot(json: &str) -> WasmBroker {
        match serde_json::from_str::<Broker>(json) {
            Ok(inner) => WasmBroker { inner },
            Err(_) => WasmBroker {
                inner: Broker::new(),
            },
        }
    }

    /// Serialize the whole broker for DO storage. Infallible for our types.
    pub fn snapshot(&self) -> String {
        serde_json::to_string(&self.inner).expect("broker state always serializes")
    }

    /// `true` when the broker holds nothing in any durable registry. Lets the
    /// DO shell stop rescheduling the reaper alarm so a truly idle Durable
    /// Object hibernates fully (alarms keep a DO awake).
    ///
    /// Purely the boundary exposure: `Broker::is_empty` decides it, because
    /// which registries count is broker knowledge and must stay in lockstep
    /// with the sweep (`Broker::reap_expired`) rather than being recomputed
    /// out here. Do not re-derive the predicate at this layer — a shim that
    /// spelled it as "the lobby is empty" is what let a DO holding a live
    /// tournament and zero lobby entries stop its alarm and never reap.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Number of currently registered lobby entries (games waiting for players).
    /// Read-only, so the shell need not re-snapshot after calling — this is the
    /// live "active games" gauge surfaced by the `/stats` endpoint.
    pub fn active_games(&self) -> usize {
        self.inner.lobby().len()
    }

    /// Handle one raw client frame (the exact JSON the client sent over the
    /// WebSocket). Parsing + dispatch happen in Rust; the shell never inspects
    /// the protocol. `conn_json` is the per-socket [`ConnState`] from the WS
    /// attachment, `now_ms` is JS `Date.now()`. Returns a [`CallResult`] as JSON.
    pub fn handle(&mut self, conn_json: &str, raw_frame: &str, now_ms: f64) -> String {
        let mut conn: ConnState = serde_json::from_str(conn_json).unwrap_or_default();
        let env = WorkerEnv {
            now_ms: now_ms as u64,
        };

        let (outbounds, dirty, reject) = match parse_lobby_client_message(raw_frame) {
            ParsedFrame::Message(msg) => {
                let dirty = mutates_lobby(&msg);
                (self.inner.handle(&mut conn, *msg, &env), dirty, None)
            }
            // A frame the parser couldn't accept — an unknown tag or a field
            // that failed validation (e.g. a blank display_name). Reply with an
            // `Error` so the client's pending RPC resolves immediately instead
            // of hanging until its timeout, and still flag `reject` so the shell
            // logs it and skips the state snapshot (nothing mutated).
            ParsedFrame::UnknownTag(tag) => {
                let reason = format!("unknown tag: {tag}");
                (reject_reply(&reason), false, Some(reason))
            }
            ParsedFrame::Malformed(e) => {
                let reason = format!("malformed frame: {e}");
                (reject_reply(&reason), false, Some(reason))
            }
        };

        result_json(CallResult {
            conn,
            outbounds: to_dtos(outbounds),
            dirty,
            reject,
        })
    }

    /// Socket-close teardown: release the connection's seat reservations and
    /// remove any lobby entry it hosted. Player-count rebroadcast is shell-owned.
    pub fn on_disconnect(&mut self, conn_json: &str) -> String {
        let mut conn: ConnState = serde_json::from_str(conn_json).unwrap_or_default();
        let outbounds = self.inner.on_disconnect(&mut conn);
        // A close releases reservations / removes a hosted entry — treat as a
        // mutation so the shell snapshots (cheap: close is low-frequency).
        result_json(CallResult {
            conn,
            outbounds: to_dtos(outbounds),
            dirty: true,
            reject: None,
        })
    }

    /// Staleness reaper, driven by a DO alarm (a hibernated DO has no tokio
    /// interval). Returns the ordered `Outbound`s (a `LobbyGameRemoved` per
    /// reaped entry) as a JSON array — there is no connection scope here.
    pub fn reap_expired(&mut self, timeout_secs: f64, now_ms: f64) -> String {
        let env = WorkerEnv {
            now_ms: now_ms as u64,
        };
        let outbounds = self.inner.reap_expired(timeout_secs as u64, &env);
        serde_json::to_string(&to_dtos(outbounds)).expect("outbounds always serialize")
    }
}

impl Default for WasmBroker {
    fn default() -> Self {
        Self::new()
    }
}

/// The shared phase.rs wire-protocol version. The Cloudflare Worker shell uses
/// this for `ServerHello` and its pre-broker handshake gate, so it cannot drift
/// from the Rust protocol constant.
#[wasm_bindgen]
pub fn protocol_version() -> u32 {
    PROTOCOL_VERSION
}

/// The **lobby** message-set version, independent of [`protocol_version`].
/// The Worker advertises this on `ServerHello` and gates incoming
/// `ClientHello` frames on it, so a full-game bump the broker never parses no
/// longer slides the lobby's compatibility window.
#[wasm_bindgen]
pub fn lobby_protocol_version() -> u32 {
    lobby_broker::LOBBY_PROTOCOL_VERSION
}

/// Lowest client lobby protocol this broker accepts. No ceiling — see
/// `lobby_broker::protocol::MIN_SUPPORTED_LOBBY_PROTOCOL`.
#[wasm_bindgen]
pub fn min_supported_lobby_protocol() -> u32 {
    lobby_broker::MIN_SUPPORTED_LOBBY_PROTOCOL
}

fn result_json(r: CallResult) -> String {
    serde_json::to_string(&r).expect("call result always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_broker::{BracketShape, MatchArity, PodOutcome, ScoringPolicy};

    /// The classification is the whole contract, and getting it wrong is
    /// SILENT: a mutating frame classified `false` leaves the shell skipping
    /// its snapshot, so the write survives until the DO next hibernates and
    /// then vanishes. Every variant is enumerated explicitly rather than
    /// sampled, because there is no runtime symptom to catch a missed one.
    #[test]
    fn tournament_variants_are_classified_by_whether_they_write() {
        let mutating = [
            LobbyClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: ScoringPolicy::default(),
                bracket: BracketShape::Swiss,
                total_rounds: None,
            },
            LobbyClientMessage::JoinTournament {
                code: "TOUR01".into(),
                player_key: "key-a".into(),
                display_name: "Alice".into(),
            },
            LobbyClientMessage::StartTournamentRound {
                code: "TOUR01".into(),
                organizer_token: "tok".into(),
            },
            LobbyClientMessage::ReportMatchResult {
                code: "TOUR01".into(),
                pairing_id: 0,
                player_token: "tok".into(),
                outcome: PodOutcome::Draw,
            },
            LobbyClientMessage::DropFromTournament {
                code: "TOUR01".into(),
                player_token: "tok".into(),
            },
            LobbyClientMessage::EndTournament {
                code: "TOUR01".into(),
                organizer_token: "tok".into(),
            },
        ];
        for msg in &mutating {
            assert!(
                mutates_lobby(msg),
                "{msg:?} writes tournament state and MUST mark the DO dirty"
            );
        }

        assert!(
            !mutates_lobby(&LobbyClientMessage::GetTournament {
                code: "TOUR01".into()
            }),
            "GetTournament is a pure read; a `true` here writes storage on every poll"
        );
    }

    /// Regression guard: the pre-existing classifications are untouched by
    /// this extension.
    #[test]
    fn pre_existing_variant_classifications_are_unchanged() {
        assert!(mutates_lobby(&LobbyClientMessage::UnregisterLobby {
            game_code: "GAME01".into()
        }));
        assert!(mutates_lobby(&LobbyClientMessage::UpdateLobbyMetadata {
            game_code: "GAME01".into(),
            current_players: 1,
            max_players: 2,
            consumed_reservation_tokens: Vec::new(),
        }));
        assert!(!mutates_lobby(&LobbyClientMessage::SubscribeLobby));
        assert!(!mutates_lobby(&LobbyClientMessage::UnsubscribeLobby));
        assert!(!mutates_lobby(&LobbyClientMessage::Ping { timestamp: 1 }));
    }
}
