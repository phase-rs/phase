//! Injected time and randomness for the WASM-safe broker core.
//!
//! The core never calls `SystemTime::now()` or `rand` directly — those are
//! WASM hazards and make the core untestable. Instead every method that needs
//! the wall clock or fresh randomness receives a `&impl BrokerEnv`. The native
//! `phase-server` shell supplies a unit struct delegating to `SystemTime` +
//! `server_core::generate_*`; a Cloudflare Durable Object shell supplies one
//! backed by `Date.now()` + `crypto.randomUUID`. Tests supply a deterministic
//! fake.

/// Environment capabilities the broker core needs but must not implement
/// itself. `&self` (not `&mut self`) so a single shared instance can be passed
/// to every `Broker` method on a connection without borrow contention.
pub trait BrokerEnv {
    /// Current wall-clock time in milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
    /// A fresh per-player reservation/session token.
    fn new_token(&self) -> String;
    /// A fresh game code for a newly registered lobby entry.
    fn new_game_code(&self) -> String;
}
