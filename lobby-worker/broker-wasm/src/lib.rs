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
    compare_announcement_to_info, info_url, normalize_announced_url, parse_lobby_client_message,
    score, validate_announcement, Broker, BrokerEnv, ConnState, InfoMatch, InfoMismatchField,
    LobbyClientMessage, LobbyServerMessage, Outbound, ParsedFrame, RawAnnouncement,
    ServerAnnouncement, ServerCounters, ServerInfoDocument, DIRECTORY_VERSION, INFO_PATH,
    PROTOCOL_VERSION, RTT_BUCKET_EDGES_MS, SCORE_BUCKET_MS, SCORE_WINDOW_MS,
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

/// Verdict of [`directory_validate_announcement`], flattened for the TS shell
/// the same way [`OutboundDto`] is: purely a boundary concern, so it lives here
/// rather than in the core.
///
/// The `Invalid` arm exists for the same reason `CallResult.reject` does — a
/// malformed body must produce a verdict, never a panic, because a panic in
/// wasm aborts the Durable Object.
#[derive(Serialize)]
#[serde(tag = "kind")]
enum ValidationDto {
    Valid { announcement: ServerAnnouncement },
    Invalid { error: String },
}

/// Verdict of [`directory_compare_announcement_to_info`].
///
/// Its `Invalid` arm is **wider** than [`ValidationDto`]'s, and has exactly
/// three sources:
///
///   1. `announcement_json` does not parse as a `RawAnnouncement`;
///   2. it parses but fails `validate_announcement` — the comparison has to
///      re-validate, because the core function takes a `&ServerAnnouncement`
///      and that type deliberately has no `Deserialize`;
///   3. `info_json` does not parse as a [`ServerInfoDocument`].
///
/// The shell cannot distinguish the three and has no reason to. A reader of
/// this verdict must not, though: source 3 is not a defect in the announcement
/// *body*; the announcement is simply unverified, and must be treated as such.
///
/// *From the announcement side*, an announcement that passed
/// [`directory_validate_announcement`] serializes — via the `Valid` arm's
/// `announcement` payload — to a body that re-validates identically, so
/// sources 1 and 2 are reachable only from a body that never passed validation
/// in the first place.
#[derive(Serialize)]
#[serde(tag = "kind")]
enum ComparisonDto {
    Match,
    Mismatch { field: InfoMismatchField },
    Invalid { error: String },
}

fn comparison_dto(announcement_json: &str, info_json: &str) -> ComparisonDto {
    let raw = match serde_json::from_str::<RawAnnouncement>(announcement_json) {
        Ok(raw) => raw,
        Err(error) => {
            return ComparisonDto::Invalid {
                error: error.to_string(),
            }
        }
    };
    let announcement = match validate_announcement(&raw) {
        Ok(announcement) => announcement,
        Err(error) => return ComparisonDto::Invalid { error },
    };
    let info = match serde_json::from_str::<ServerInfoDocument>(info_json) {
        Ok(info) => info,
        Err(error) => {
            return ComparisonDto::Invalid {
                error: error.to_string(),
            }
        }
    };
    // Exhaustive: a future `InfoMatch` variant must force a deliberate
    // classification here rather than falling into a wildcard.
    match compare_announcement_to_info(&announcement, &info) {
        InfoMatch::Match => ComparisonDto::Match,
        InfoMatch::Mismatch { field } => ComparisonDto::Mismatch { field },
    }
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

/// Version of the server-directory announcement shape
/// (`lobby_broker::directory::DIRECTORY_VERSION`). An announcement declaring a
/// different value is refused by [`directory_validate_announcement`].
#[wasm_bindgen]
pub fn directory_version() -> u32 {
    DIRECTORY_VERSION
}

/// The HTTP path every phase server kind answers with its info document.
///
/// No TypeScript consumer: [`directory_info_url`] returns the whole probe URL
/// and subsumes it, and the Durable Object routes on literals of its own.
#[wasm_bindgen]
pub fn directory_info_path() -> String {
    INFO_PATH.to_string()
}

/// Validate an announcement body, returning a `{ kind, ... }` verdict as JSON.
///
/// Parses [`RawAnnouncement`], not `ServerAnnouncement`: the latter has no
/// `Deserialize` by design, so routing through the validator is enforced by the
/// compiler here rather than by convention.
#[wasm_bindgen]
pub fn directory_validate_announcement(json: &str) -> String {
    let dto = match serde_json::from_str::<RawAnnouncement>(json) {
        Ok(raw) => match validate_announcement(&raw) {
            Ok(announcement) => ValidationDto::Valid { announcement },
            Err(error) => ValidationDto::Invalid { error },
        },
        Err(error) => ValidationDto::Invalid {
            error: error.to_string(),
        },
    };
    serde_json::to_string(&dto).expect("validation verdict always serializes")
}

/// Confront an announcement body with the info document fetched from the host
/// it announced, returning a `{ kind, ... }` verdict as JSON. Re-validates the
/// announcement on the way in — see [`ComparisonDto`].
#[wasm_bindgen]
pub fn directory_compare_announcement_to_info(announcement_json: &str, info_json: &str) -> String {
    serde_json::to_string(&comparison_dto(announcement_json, info_json))
        .expect("comparison verdict always serializes")
}

/// The `https://` info-document URL for an announced `wss://` address, or
/// `None` when the address is not one the directory would accept.
///
/// Normalises internally, so this raw-string entry point and the typed
/// `info_url` are literally the same authority — the outbound fetch cannot be
/// pointed anywhere the storage path would have refused.
#[wasm_bindgen]
pub fn directory_info_url(announced_url: &str) -> Option<String> {
    normalize_announced_url(announced_url)
        .ok()
        .map(|url| info_url(&url))
}

/// The canonical `wss://` form of an announced address, or `None` when it is
/// not one the directory would accept.
///
/// Exists so the allowlist and the stored rows are keyed by the SAME
/// authority. The allowlist's keys are typed by a human, the row keys come out
/// of [`directory_validate_announcement`]; without running the human's key
/// through this, `wss://Host.Example:443/ws/` matches no row, lists nothing,
/// and reports no error anywhere.
#[wasm_bindgen]
pub fn directory_normalize_url(raw: &str) -> Option<String> {
    normalize_announced_url(raw)
        .ok()
        .map(|url| url.as_str().to_string())
}

/// A server's health score from its counters: `ServerCounters` JSON in,
/// `Score` JSON — or the JSON literal `null` — out.
///
/// `null` covers both "no live evidence" (the core returned `None`) and
/// "unreadable counters". A caller cannot distinguish them and has no reason
/// to: both mean the same thing to a listing, and the alternative is a panic,
/// which in wasm aborts the Durable Object.
///
/// `now_ms` is an `f64` because it crosses from JS `Date.now()`; a `u64` would
/// arrive as a `BigInt` that cannot be mixed with JS number arithmetic without
/// a conversion at every call site. Negative and non-finite inputs clamp to 0
/// rather than wrapping.
#[wasm_bindgen]
pub fn directory_score(counters_json: &str, now_ms: f64) -> String {
    let Ok(counters) = serde_json::from_str::<ServerCounters>(counters_json) else {
        return "null".to_string();
    };
    let now = if now_ms.is_finite() && now_ms > 0.0 {
        now_ms as u64
    } else {
        0
    };
    serde_json::to_string(&score(&counters, now)).expect("score always serializes")
}

/// Width of one counter bucket, in ms
/// (`lobby_broker::directory::SCORE_BUCKET_MS`). The TypeScript fold cuts
/// buckets on this; it is never a TypeScript literal.
#[wasm_bindgen]
pub fn directory_score_bucket_ms() -> f64 {
    SCORE_BUCKET_MS as f64
}

/// The score's decay window, in ms
/// (`lobby_broker::directory::SCORE_WINDOW_MS`). Rust weights by it; the
/// TypeScript fold ages buckets out by it.
#[wasm_bindgen]
pub fn directory_score_window_ms() -> f64 {
    SCORE_WINDOW_MS as f64
}

/// Upper edges of the RTT histogram's cells, in ms
/// (`lobby_broker::directory::RTT_BUCKET_EDGES_MS`).
///
/// Exported for the same reason as the two constants above, and the drift it
/// prevents is the least visible of the three: the TypeScript fold decides
/// which cell a reported RTT lands in, and Rust reads the resulting histogram
/// positionally. A TypeScript edge list one entry out of step would file every
/// latency into the neighbouring cell, move every median, and fail no test on
/// either side. The cell COUNT is derived from the length here as it is in
/// Rust, so the two arrays cannot differ in size either.
#[wasm_bindgen]
pub fn directory_rtt_bucket_edges_ms() -> Vec<u32> {
    RTT_BUCKET_EDGES_MS.to_vec()
}

fn result_json(r: CallResult) -> String {
    serde_json::to_string(&r).expect("call result always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_broker::{BracketShape, MatchArity, PodOutcome, ScoringPolicy, ServerMode};

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

    fn raw_announcement() -> RawAnnouncement {
        RawAnnouncement {
            directory_version: DIRECTORY_VERSION,
            // Deliberately spelled non-canonically so the round-trip below
            // proves the export normalises rather than echoes.
            url: "wss://Play.Example.com:443/ws/".to_string(),
            name: "play.example.com".to_string(),
            mode: ServerMode::Full,
            server_version: "0.9.1".to_string(),
            protocol_version: PROTOCOL_VERSION,
            lobby_protocol_version: lobby_broker::LOBBY_PROTOCOL_VERSION,
            current_players: 2,
        }
    }

    fn info_json(mode: ServerMode, server_version: &str) -> String {
        serde_json::to_string(&ServerInfoDocument {
            mode,
            protocol_version: PROTOCOL_VERSION,
            lobby_protocol_version: lobby_broker::LOBBY_PROTOCOL_VERSION,
            server_version: server_version.to_string(),
            build_commit: None,
            public_url: None,
        })
        .expect("info document serializes")
    }

    fn parse(json: String) -> serde_json::Value {
        serde_json::from_str(&json).expect("every export returns JSON")
    }

    /// V10. The boundary exports must agree with the core functions they wrap,
    /// and must answer malformed input with a verdict rather than a panic — a
    /// panic in wasm aborts the Durable Object.
    ///
    /// Note this test lives OUTSIDE `lobby_broker::directory`, so it cannot
    /// build a `ServerAnnouncement` by struct literal (every field is private).
    /// It obtains one the only way anyone outside that module can: by calling
    /// `validate_announcement` on a `RawAnnouncement`.
    #[test]
    fn directory_exports_match_the_core_verdicts() {
        assert_eq!(directory_version(), DIRECTORY_VERSION);
        assert_eq!(directory_info_path(), INFO_PATH);
        // V-U14f. Every exported CONSTANT is asserted against its core, in one
        // place, so the parity pattern is uniform rather than remembered per
        // export. These three are the numbers TypeScript is forbidden to
        // declare for itself; a drifting export would mis-cut every bucket or
        // mis-file every latency and fail nothing else.
        assert_eq!(directory_score_bucket_ms(), SCORE_BUCKET_MS as f64);
        assert_eq!(directory_score_window_ms(), SCORE_WINDOW_MS as f64);
        assert_eq!(
            directory_rtt_bucket_edges_ms(),
            RTT_BUCKET_EDGES_MS.to_vec()
        );

        let raw = raw_announcement();
        let raw_json = serde_json::to_string(&raw).expect("raw announcement serializes");

        // Valid: the export's payload is exactly the core function's value.
        let verdict = parse(directory_validate_announcement(&raw_json));
        assert_eq!(verdict["kind"], "Valid");
        let core = validate_announcement(&raw).expect("the core accepts this fixture");
        assert_eq!(
            verdict["announcement"],
            serde_json::to_value(&core).expect("core value serializes")
        );
        assert_eq!(verdict["announcement"]["url"], "wss://play.example.com/ws");

        // Invalid, paired with the valid case above so a hard-coded verdict
        // fails one of the two.
        let mut hostile = raw_announcement();
        hostile.url = "wss://evil@real.example/ws".to_string();
        let hostile_json = serde_json::to_string(&hostile).expect("raw announcement serializes");
        assert_eq!(
            parse(directory_validate_announcement(&hostile_json))["kind"],
            "Invalid"
        );
        assert_eq!(
            parse(directory_validate_announcement("{"))["kind"],
            "Invalid"
        );

        // The comparison export, both directions.
        let matching = info_json(ServerMode::Full, "0.9.1");
        assert_eq!(
            parse(directory_compare_announcement_to_info(&raw_json, &matching))["kind"],
            "Match"
        );
        let other_mode = info_json(ServerMode::LobbyOnly, "0.9.1");
        let mismatch = parse(directory_compare_announcement_to_info(
            &raw_json,
            &other_mode,
        ));
        assert_eq!(mismatch["kind"], "Mismatch");
        assert_eq!(mismatch["field"], "Mode");

        // A body whose `url` never passed validation, and malformed JSON: both
        // land in the wider `Invalid` arm rather than panicking.
        assert_eq!(
            parse(directory_compare_announcement_to_info(
                &hostile_json,
                &matching
            ))["kind"],
            "Invalid"
        );
        assert_eq!(
            parse(directory_compare_announcement_to_info("{", &matching))["kind"],
            "Invalid"
        );
        // Source 3: a VALID announcement whose info document is unparseable —
        // the announced host served garbage. Distinct from the two cases above
        // in what is unverified, identical in verdict, and the case the `Invalid`
        // arm's "only from the announcement side" wording is scoped around.
        assert_eq!(
            parse(directory_compare_announcement_to_info(&raw_json, "{"))["kind"],
            "Invalid"
        );

        // Round-trip: the `Valid` arm's `announcement` payload, fed back to the
        // compare export, must Match. This is the assertion that would catch a
        // future normalisation change making validation non-idempotent — the
        // one way the ANNOUNCEMENT side could start returning `Invalid` for a
        // body that DID pass announce-time validation. (An unparseable info
        // document is the other, unrelated source, asserted just above.)
        let round_trip = verdict["announcement"].to_string();
        assert_eq!(
            parse(directory_compare_announcement_to_info(
                &round_trip,
                &matching
            ))["kind"],
            "Match"
        );

        // The info-URL export, hostile and valid paired.
        assert_eq!(directory_info_url("wss://evil@real.example/ws"), None);
        assert_eq!(directory_info_url("wss://localhost/ws"), None);
        assert_eq!(
            directory_info_url("wss://Host.Example:443/ws/"),
            Some(format!("https://host.example{INFO_PATH}"))
        );
    }

    /// V-U14e + V-U17e. The two directory exports that carry a VALUE across
    /// the boundary (rather than a verdict or a constant) must agree with the
    /// core function they wrap, and must answer hostile input with a value
    /// rather than a panic — a panic in wasm aborts the Durable Object.
    #[test]
    fn directory_score_and_normalize_exports_match_their_cores() {
        // One live hour-bucket. Built as JSON, not as a struct literal, so
        // this also pins the field names the TypeScript fold has to write:
        // a rename on either side lands here as a parse failure.
        const NOW_MS: f64 = 3_600_000_000.0;
        const COUNTERS: &str = r#"{"buckets":[{"start_ms":3600000000,"connect_attempts":100,
            "connect_successes":100,"games_started":10,"games_completed":8,
            "rtt_histogram":[0,100,0,0,0,0,0,0],"announced_players_max":4}]}"#;

        let core = serde_json::from_str::<ServerCounters>(COUNTERS).expect("fixture parses");
        let core_score = score(&core, NOW_MS as u64);
        assert!(
            core_score.as_ref().and_then(|s| s.value).is_some(),
            "the fixture must be rankable, or the equality below is vacuous"
        );
        // Compared as STRINGS, not as `serde_json::Value`s. `Score`'s rates
        // are `f32`, and widening one to `f64` through `to_value` renders
        // `0.8` as `0.800000011920929` while serializing it directly renders
        // `0.8` — a difference in the instrument, not in the export. The
        // string is also what actually crosses the boundary, so this asserts
        // the wire form rather than a re-interpretation of it.
        assert_eq!(
            directory_score(COUNTERS, NOW_MS),
            serde_json::to_string(&core_score).expect("core score serializes")
        );

        // `null` for both kinds of nothing: unreadable counters, and counters
        // whose every bucket has aged out of the window. Indistinguishable on
        // purpose — a listing does the same thing with either.
        assert_eq!(directory_score("{", 0.0), "null");
        assert_eq!(directory_score(r#"{"buckets":[]}"#, NOW_MS), "null");
        const AGED_OUT: &str = r#"{"buckets":[{"start_ms":3000000000,"connect_attempts":100,
            "connect_successes":100,"games_started":0,"games_completed":0,
            "rtt_histogram":[0,100,0,0,0,0,0,0],"announced_players_max":0}]}"#;
        assert_eq!(directory_score(AGED_OUT, NOW_MS), "null");
        // A negative or non-finite clock clamps to 0 rather than wrapping into
        // a huge `u64`. Asserted as "same answer as now = 0", not as `null`:
        // at now = 0 every stored bucket is in the FUTURE, and
        // `bucket_weight`'s `saturating_sub` deliberately treats a
        // future-dated bucket as brand new rather than as maximally aged.
        assert_eq!(
            directory_score(COUNTERS, -1.0),
            directory_score(COUNTERS, 0.0)
        );
        assert_eq!(
            directory_score(COUNTERS, f64::NAN),
            directory_score(COUNTERS, 0.0)
        );
        assert_eq!(
            directory_score(COUNTERS, f64::INFINITY),
            directory_score(COUNTERS, 0.0)
        );

        // V-U17e. The allowlist normaliser is the SAME authority as the row
        // key — an operator's spelling and the announcer's spelling of one
        // address must produce one string, or the intersection silently
        // under-lists.
        assert_eq!(
            directory_normalize_url("wss://Host.Example:443/ws/"),
            Some("wss://host.example/ws".to_string())
        );
        assert_eq!(
            directory_normalize_url("wss://host.example/ws"),
            directory_normalize_url("wss://Host.Example:443/ws/")
        );
        // Hostile keys are dropped rather than listed: an operator can type
        // anything into KV, and these are what the Worker refuses.
        assert_eq!(directory_normalize_url("wss://localhost/ws"), None);
        assert_eq!(directory_normalize_url("wss://evil@real.example/ws"), None);
        assert_eq!(directory_normalize_url("ws://host.example/ws"), None);
        assert_eq!(directory_normalize_url("wss://127.0.0.1/ws"), None);
    }
}
