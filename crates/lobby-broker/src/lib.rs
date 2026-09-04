//! `lobby-broker` — WASM-safe matchmaking-broker core.
//!
//! Functional core / imperative shell (mirrors the engine's `apply` reducer):
//! [`Broker::handle`] takes a connection's [`ConnState`] + a parsed
//! [`LobbyClientMessage`] + an injected [`BrokerEnv`] (time/rng) and returns an
//! ordered `Vec<Outbound>` of side effects for the transport shell to perform.
//! No tokio, no axum, no `SystemTime`, no `rand` — so the identical logic runs
//! in the native `phase-server` shell and a Cloudflare Durable Object (WASM).
//! [`directory`] shares that same no-I/O constraint: it is the server-directory
//! contract both shells (and, later, the client's TypeScript mirror) apply.

pub mod broker;
pub mod directory;
pub mod env;
pub mod inbound_guard;
pub mod lobby;
pub mod protocol;
pub mod reservation_auth;
pub mod tournament;
pub mod validation;

pub use broker::{
    check_build_commit, Broker, BuildCommitCheck, ClientHelloInfo, ConnState, Outbound,
    MAX_LOBBY_ENTRIES,
};
pub use directory::{
    compare_announcement_to_info, info_url, normalize_announced_url, score, validate_announcement,
    AnnouncedUrl, CounterBucket, InfoMatch, InfoMismatchField, RawAnnouncement, Score,
    ServerAnnouncement, ServerCounters, ServerInfoDocument, DIRECTORY_VERSION, INFO_PATH,
    MAX_ANNOUNCED_PLAYERS, MAX_SERVER_NAME_LEN, MAX_SERVER_URL_LEN, RTT_BUCKET_EDGES_MS,
    SCORE_BUCKET_MS, SCORE_MIN_SAMPLES, SCORE_WINDOW_MS,
};
pub use env::BrokerEnv;
pub use inbound_guard::{
    guard_create_game_settings_inbound, guard_inbound, guard_join_game_with_password_inbound,
    guard_lookup_join_target_inbound, validate_create_game_settings_inbound_fields,
    validate_deck_payload, CreateGameSettingsInbound, JoinGameWithPasswordInbound,
    LookupJoinTargetInbound,
};
pub use lobby::{
    JoinTargetInfo, LobbyManager, LobbyReservation, RegisterGameRequest, PUBLIC_SEAT_RESERVATION_MS,
};
pub use protocol::{
    parse_lobby_client_message, DraftLobbyMetadata, LobbyClientMessage, LobbyGame,
    LobbyServerMessage, ParsedFrame, ServerErrorCode, ServerMode, LOBBY_PROTOCOL_VERSION,
    MIN_SUPPORTED_LOBBY_PROTOCOL, MIN_SUPPORTED_PROTOCOL, PROTOCOL_VERSION,
};
pub use reservation_auth::{
    conn_holds_reservation, consume_owned_reservation, release_owned_reservation,
    ReservationConsume, ReservationRelease, NOT_OWNED_RESERVATION,
};
pub use tournament::{
    build_single_elimination_round, build_swiss_round, default_total_rounds, had_bye,
    had_short_pod, partition_round, prior_opponents, validate_match_result, BracketShape,
    CreateTournamentRequest, MatchArity, PairingId, PairingOutcome, PodOutcome, RawScoringPolicy,
    RoundPartition, ScoringPolicy, TiebreakOrder, Tiebreaks, TournamentExpiryEvent,
    TournamentManager, TournamentMeta, TournamentPairing, TournamentPlayer, TournamentStanding,
    TournamentStatus, IN_PROGRESS_ABANDON_SECS, REGISTRATION_TIMEOUT_SECS,
    SINGLE_ELIMINATION_MAX_PLAYERS, SINGLE_ELIMINATION_MIN_PLAYERS, TERMINAL_RETENTION_SECS,
};
pub use validation::validate_lobby_message;
