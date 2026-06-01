//! `lobby-broker` — WASM-safe matchmaking-broker core.
//!
//! Functional core / imperative shell (mirrors the engine's `apply` reducer):
//! [`Broker::handle`] takes a connection's [`ConnState`] + a parsed
//! [`LobbyClientMessage`] + an injected [`BrokerEnv`] (time/rng) and returns an
//! ordered `Vec<Outbound>` of side effects for the transport shell to perform.
//! No tokio, no axum, no `SystemTime`, no `rand` — so the identical logic runs
//! in the native `phase-server` shell and a Cloudflare Durable Object (WASM).

pub mod broker;
pub mod env;
pub mod lobby;
pub mod protocol;
pub mod validation;

pub use broker::{
    check_build_commit, Broker, BuildCommitCheck, ClientHelloInfo, ConnState, Outbound,
    MAX_LOBBY_ENTRIES,
};
pub use env::BrokerEnv;
pub use lobby::{
    JoinTargetInfo, LobbyManager, LobbyReservation, RegisterGameRequest, PUBLIC_SEAT_RESERVATION_MS,
};
pub use protocol::{
    parse_lobby_client_message, DraftLobbyMetadata, LobbyClientMessage, LobbyGame,
    LobbyServerMessage, ParsedFrame, ServerMode,
};
pub use validation::validate_lobby_message;
