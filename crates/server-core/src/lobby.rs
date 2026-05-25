//! Lobby registry — moved into the WASM-safe `lobby-broker` crate.
//!
//! `LobbyManager` + `LobbyReservation` + `RegisterGameRequest` +
//! `JoinTargetInfo` now live in `lobby-broker` so the matchmaking logic is
//! shared by the native `phase-server` shell and a Cloudflare Durable Object
//! (WASM) shell without duplication. This module re-exports them so existing
//! `server_core::lobby::*` / `server_core::LobbyManager` paths keep working.
//!
//! The `LobbyManager` methods that need wall-clock time or fresh tokens now
//! take a `&impl lobby_broker::BrokerEnv`; the native shell passes a unit
//! struct delegating to `SystemTime` + `server_core::generate_*`.

pub use lobby_broker::{JoinTargetInfo, LobbyManager, LobbyReservation, RegisterGameRequest};
