//! `LobbyManager` — the pure, WASM-safe registry of waiting lobby entries.
//!
//! Moved here verbatim from `server_core::lobby` (plan §2), with one change:
//! the two WASM hazards — `SystemTime::now()` (created_at / reservation
//! expiry / staleness) and `generate_player_token()` (reservation tokens) —
//! are replaced by [`BrokerEnv`] calls threaded into the methods that need
//! them. No game logic was altered; the unit tests carried over from
//! server-core (with a deterministic fake `BrokerEnv`) guard the behavior.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::env::BrokerEnv;
use crate::protocol::{DraftLobbyMetadata, LobbyGame};

/// Public-seat reservation lifetime (ms). DUPLICATED from
/// `server_core::session::PUBLIC_SEAT_RESERVATION_MS` deliberately (plan
/// decision M3): that const is also used by Full-mode `SessionManager`, so
/// importing it here would force `session.rs` to depend on this WASM-leaf
/// crate. It is a policy constant, not logic — keep the literal in both places
/// with a cross-reference so a future change touches both.
pub const PUBLIC_SEAT_RESERVATION_MS: u64 = 120_000;

/// Minimum advance (seconds) of a listing's liveness clock
/// ([`LobbyManager::refresh_liveness`]).
///
/// The refresh is quantized so a shell that persists the lobby can write
/// exactly when the clock advances: the in-memory clock then always equals
/// the persisted one, at a cost of one write per waiting host per quantum
/// instead of one per host frame. A host's listing is therefore at most one
/// quantum plus one inter-frame gap stale, which must stay well below the
/// shells' reap timeout (300 s).
pub const LIVENESS_REFRESH_SECS: u64 = 60;

/// Fields a caller supplies when registering a lobby entry. Using a struct
/// here rather than a long positional argument list means adding a new field
/// doesn't require touching every caller — just add it here with a `Default`
/// and populate where relevant.
#[derive(Debug, Clone, Default)]
pub struct RegisterGameRequest {
    pub host_name: String,
    pub public: bool,
    pub password: Option<String>,
    pub timer_seconds: Option<u32>,
    pub host_version: String,
    pub host_build_commit: String,
    pub current_players: u32,
    pub max_players: u32,
    pub format_config: Option<engine::types::format::FormatConfig>,
    pub match_config: engine::types::match_config::MatchConfig,
    /// Optional match-scoped label shown in lobby listings.
    pub room_name: Option<String>,
    /// PeerJS peer ID of the host for lobby-only server mode. Empty string on
    /// `Full`-mode servers.
    pub host_peer_id: String,
    /// Draft-specific metadata for lobby display. `None` for constructed-play.
    pub draft_metadata: Option<DraftLobbyMetadata>,
    pub ranked: bool,
}

/// Fields returned by `join_target_info` — everything the server needs to
/// answer a typed-code lookup or populate `PeerInfo` for a brokered join in
/// one atomic snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinTargetInfo {
    pub host_peer_id: String,
    pub max_players: u32,
    pub current_players: u32,
    pub format_config: Option<engine::types::format::FormatConfig>,
    pub match_config: engine::types::match_config::MatchConfig,
    pub is_p2p: bool,
    pub reservation_token: Option<String>,
    pub reservation_expires_at_ms: Option<u64>,
    pub draft_metadata: Option<DraftLobbyMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyReservation {
    pub token: String,
    pub display_name: String,
    pub expires_at_ms: Option<u64>,
}

/// One specific registration under a game code: the code plus the
/// registration's generation, rather than the code alone.
///
/// A code names a slot the lobby reuses: [`LobbyManager::register_game`]
/// overwrites whatever is registered under it, and an entry can be reaped by
/// age while whoever registered it is still around. Anything that holds on to
/// a registration across that gap — an expiry observation from
/// [`LobbyManager::check_expired`], or a host socket's ownership stamp
/// (`ConnState::host_game`) — must act on *this* registration, never on
/// whatever now sits under the code. Acting by code alone would delete a
/// replacement and broadcast its removal. [`LobbyManager::is_current`] is the
/// single identity check; [`LobbyManager::unregister_registration`] and
/// [`LobbyManager::unregister_expired`] consume through it.
///
/// Serialized because it rides in a host socket's `ConnState`, which the
/// Durable Object shell persists in the WebSocket attachment. No old-format
/// reader is needed: a code deploy disconnects every Durable Object WebSocket,
/// so no attachment written by an earlier build is ever read by this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyRegistration {
    game_code: String,
    generation: u64,
}

impl LobbyRegistration {
    /// The code this registration was listed under.
    pub fn game_code(&self) -> &str {
        &self.game_code
    }
}

/// What [`LobbyManager::unregister_expired`] did with an expiry observation.
///
/// Three outcomes rather than a bool because the two that remove nothing are
/// not the same act: one still owes subscribers a removal and the other must
/// stay silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryConsumption {
    /// The observed registration was still listed, and has been removed.
    Removed,
    /// Nothing is listed under the code — another path removed it between the
    /// observation and this call. Nothing was removed here, but subscribers are
    /// still told: a client that over-prunes a listing it no longer has
    /// recovers, where one that never hears of a removal keeps a dead entry.
    AlreadyGone,
    /// A *different* registration is listed under the code — `register_game`
    /// replaced the entry after it was observed. The replacement is live and
    /// has not expired, so it stays registered and no removal is announced.
    Superseded,
}

#[derive(Serialize, Deserialize)]
struct LobbyGameMeta {
    host_name: String,
    created_at: u64,
    /// When the listing last showed liveness (seconds, same clock as
    /// `created_at`) — the clock [`LobbyManager::check_expired`] ages. Set to
    /// `created_at` at registration and advanced only by
    /// [`LobbyManager::refresh_liveness`]; `created_at` stays the advertised
    /// listing time.
    ///
    /// `default` covers a snapshot written by a build that predates the field;
    /// [`LobbyManagerSnapshot`] floors it at `created_at` so such a snapshot
    /// does not read every entry as lapsed.
    #[serde(default)]
    last_seen: u64,
    /// Identity of this registration, distinguishing it from any other
    /// registration listed under the same code. See [`LobbyRegistration`].
    ///
    /// `default` covers a snapshot written by a build that predates the field;
    /// [`LobbyManagerSnapshot`] is what keeps such a snapshot from colliding
    /// with a later registration.
    #[serde(default)]
    generation: u64,
    password: Option<String>,
    has_password: bool,
    timer_seconds: Option<u32>,
    public: bool,
    host_version: String,
    host_build_commit: String,
    current_players: u32,
    max_players: u32,
    format_config: Option<engine::types::format::FormatConfig>,
    match_config: engine::types::match_config::MatchConfig,
    room_name: Option<String>,
    host_peer_id: String,
    draft_metadata: Option<DraftLobbyMetadata>,
    ranked: bool,
    reservations: HashMap<String, LobbyReservation>,
}

/// Wire form of [`LobbyManager`]. It exists so the generation counter can be
/// seeded **once**, at the deserialize boundary, above every generation the
/// snapshot carries.
///
/// A snapshot written by a build predating these fields reads the counter and
/// every entry's generation as `0`. Deriving the next identity from the live
/// map instead would not fix that, because removing an entry *lowers* the
/// derived floor: empty the map and it falls back to `0`, handing a fresh
/// registration the identity an outstanding observation still names. Seeding
/// here makes [`LobbyManager::next_generation`] monotone for the manager's
/// whole life, which no removal can undo.
///
/// It is also where each entry's `last_seen` is floored at its `created_at`.
#[derive(Deserialize)]
struct LobbyManagerSnapshot {
    games: HashMap<String, LobbyGameMeta>,
    #[serde(default)]
    next_generation: u64,
}

impl From<LobbyManagerSnapshot> for LobbyManager {
    fn from(mut snapshot: LobbyManagerSnapshot) -> Self {
        // A snapshot written before `last_seen` existed reads it as `0`, which
        // would reap every restored entry on the first sweep. `last_seen >=
        // created_at` holds by construction, so flooring at `created_at` is
        // exact for such a snapshot and a no-op for any other.
        for meta in snapshot.games.values_mut() {
            meta.last_seen = meta.last_seen.max(meta.created_at);
        }
        let seeded = snapshot
            .games
            .values()
            .map(|meta| meta.generation + 1)
            .max()
            .unwrap_or(0)
            .max(snapshot.next_generation);
        Self {
            games: snapshot.games,
            next_generation: seeded,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(from = "LobbyManagerSnapshot")]
pub struct LobbyManager {
    games: HashMap<String, LobbyGameMeta>,
    /// Strictly monotone source of registration identities, seeded past the
    /// snapshot's generations by [`LobbyManagerSnapshot`] and never lowered.
    ///
    /// Neither this struct nor [`LobbyGameMeta`] denies unknown fields, so a
    /// snapshot written by a *newer* build also loads on an older one — the
    /// extra fields are dropped and the old build behaves as it did before.
    next_generation: u64,
}

impl LobbyManager {
    pub fn new() -> Self {
        Self {
            games: HashMap::new(),
            next_generation: 0,
        }
    }

    /// Lists `req` under `game_code`, replacing any entry already there, and
    /// returns the identity of the new registration.
    pub fn register_game(
        &mut self,
        game_code: &str,
        req: RegisterGameRequest,
        env: &impl BrokerEnv,
    ) -> LobbyRegistration {
        let has_password = req.password.is_some();
        let created_at = env.now_ms() / 1000;
        // Monotone for the manager's whole life — see `next_generation`. A
        // removal never lowers it, so an identity an outstanding expiry
        // observation still names cannot be handed out again.
        let generation = self.next_generation;
        self.next_generation += 1;

        debug!(
            game = %game_code,
            host = %req.host_name,
            version = %req.host_version,
            commit = %req.host_build_commit,
            "lobby game registered"
        );

        self.games.insert(
            game_code.to_string(),
            LobbyGameMeta {
                host_name: req.host_name,
                created_at,
                last_seen: created_at,
                generation,
                password: req.password,
                has_password,
                timer_seconds: req.timer_seconds,
                public: req.public,
                host_version: req.host_version,
                host_build_commit: req.host_build_commit,
                current_players: req.current_players,
                max_players: req.max_players,
                format_config: req.format_config,
                match_config: req.match_config,
                room_name: req.room_name,
                host_peer_id: req.host_peer_id,
                draft_metadata: req.draft_metadata,
                ranked: req.ranked,
                reservations: HashMap::new(),
            },
        );
        LobbyRegistration {
            game_code: game_code.to_string(),
            generation,
        }
    }

    fn cleanup_expired_for(meta: &mut LobbyGameMeta, now_ms: u64) -> bool {
        let before = meta.reservations.len();
        meta.reservations.retain(|_, reservation| {
            reservation
                .expires_at_ms
                .is_none_or(|expires| expires > now_ms)
        });
        before != meta.reservations.len()
    }

    pub fn cleanup_expired_reservations(&mut self, game_code: &str, env: &impl BrokerEnv) -> bool {
        let now = env.now_ms();
        self.games
            .get_mut(game_code)
            .is_some_and(|meta| Self::cleanup_expired_for(meta, now))
    }

    pub fn reserve_seat(
        &mut self,
        game_code: &str,
        display_name: String,
        env: &impl BrokerEnv,
    ) -> Result<LobbyReservation, String> {
        let now = env.now_ms();
        let meta = self
            .games
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found in lobby: {}", game_code))?;
        Self::cleanup_expired_for(meta, now);
        let occupied = meta.current_players + meta.reservations.len() as u32;
        if meta.max_players > 0 && occupied >= meta.max_players {
            return Err(format!("Game {game_code} is full"));
        }
        let token = env.new_token();
        let reservation = LobbyReservation {
            token: token.clone(),
            display_name,
            expires_at_ms: Some(now + PUBLIC_SEAT_RESERVATION_MS),
        };
        meta.reservations.insert(token, reservation.clone());
        Ok(reservation)
    }

    pub fn release_reservation(&mut self, game_code: &str, token: &str) -> bool {
        self.games
            .get_mut(game_code)
            .and_then(|meta| meta.reservations.remove(token))
            .is_some()
    }

    pub fn has_active_reservation(
        &mut self,
        game_code: &str,
        token: &str,
        env: &impl BrokerEnv,
    ) -> bool {
        let Some(meta) = self.games.get_mut(game_code) else {
            return false;
        };
        Self::cleanup_expired_for(meta, env.now_ms());
        meta.reservations.contains_key(token)
    }

    pub fn release_reservations(&mut self, reservations: &[(String, String)]) -> bool {
        let mut changed = false;
        for (game_code, token) in reservations {
            changed |= self.release_reservation(game_code, token);
        }
        changed
    }

    pub fn consume_reservation(&mut self, game_code: &str, token: &str) -> bool {
        let Some(meta) = self.games.get_mut(game_code) else {
            return false;
        };
        if meta.reservations.remove(token).is_none() {
            return false;
        }
        meta.current_players = (meta.current_players + 1).min(meta.max_players);
        true
    }

    /// Returns the seated player count, excluding pending reservations.
    pub fn seated_player_count(&self, game_code: &str) -> Option<u32> {
        self.games.get(game_code).map(|meta| meta.current_players)
    }

    /// Updates the `current_players` count for an existing lobby entry. No-op
    /// if the game isn't tracked.
    pub fn set_current_players(
        &mut self,
        game_code: &str,
        current_players: u32,
        env: &impl BrokerEnv,
    ) {
        let now = env.now_ms();
        if let Some(meta) = self.games.get_mut(game_code) {
            Self::cleanup_expired_for(meta, now);
            meta.current_players = current_players;
        }
    }

    /// Updates the `max_players` count for an existing lobby entry. No-op if
    /// the game isn't tracked.
    pub fn set_max_players(&mut self, game_code: &str, max: u8) {
        if let Some(meta) = self.games.get_mut(game_code) {
            meta.max_players = max as u32;
        }
    }

    /// Returns the host's build identity for a game, used to gate joins when
    /// the guest's build differs from the host's.
    pub fn host_build_commit(&self, game_code: &str) -> Option<&str> {
        self.games
            .get(game_code)
            .map(|meta| meta.host_build_commit.as_str())
    }

    pub fn unregister_game(&mut self, game_code: &str) {
        self.games.remove(game_code);
        debug!(game = %game_code, "lobby game unregistered");
    }

    pub fn verify_password(&self, game_code: &str, password: Option<&str>) -> Result<(), String> {
        let meta = self
            .games
            .get(game_code)
            .ok_or_else(|| format!("Game not found in lobby: {}", game_code))?;

        match (&meta.password, password) {
            (None, _) => Ok(()),
            (Some(_), None) => Err("password_required".to_string()),
            (Some(expected), Some(provided)) => {
                if expected == provided {
                    Ok(())
                } else {
                    warn!(game = %game_code, "wrong password");
                    Err("Wrong password".to_string())
                }
            }
        }
    }

    /// Returns the public-lobby view of a single game by code, or `None` if
    /// the game isn't tracked or isn't public.
    pub fn public_game(&self, game_code: &str) -> Option<LobbyGame> {
        let meta = self.games.get(game_code)?;
        if !meta.public {
            return None;
        }
        Some(Self::meta_to_lobby_game(game_code, meta))
    }

    pub fn public_games(&self) -> Vec<LobbyGame> {
        self.games
            .iter()
            .filter(|(_, meta)| meta.public)
            .map(|(code, meta)| Self::meta_to_lobby_game(code, meta))
            .collect()
    }

    /// Converts internal `LobbyGameMeta` to the wire-level `LobbyGame`. Single
    /// construction site prevents field drift when new metadata fields are added.
    fn meta_to_lobby_game(game_code: &str, meta: &LobbyGameMeta) -> LobbyGame {
        LobbyGame {
            game_code: game_code.to_string(),
            host_name: meta.host_name.clone(),
            created_at: meta.created_at,
            has_password: meta.has_password,
            host_version: meta.host_version.clone(),
            host_build_commit: meta.host_build_commit.clone(),
            current_players: (meta.current_players + meta.reservations.len() as u32)
                .min(meta.max_players),
            max_players: meta.max_players,
            format: meta.format_config.as_ref().map(|fc| fc.format),
            room_name: meta.room_name.clone(),
            is_p2p: !meta.host_peer_id.is_empty(),
            is_sandbox: meta
                .format_config
                .as_ref()
                .is_some_and(|fc| fc.allow_debug_actions),
            is_ranked: meta.ranked,
            draft_metadata: meta.draft_metadata.clone(),
        }
    }

    pub fn has_game(&self, game_code: &str) -> bool {
        self.games.contains_key(game_code)
    }

    /// Current number of registered lobby entries. Used by the broker path to
    /// enforce a capacity cap (`LobbyManager` itself is unbounded).
    pub fn len(&self) -> usize {
        self.games.len()
    }

    /// Reports whether the lobby has any registered entries.
    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    /// Atomic lookup of the fields a typed-code join needs to route correctly.
    /// Returns `None` if the game isn't registered.
    pub fn join_target_info(&self, game_code: &str) -> Option<JoinTargetInfo> {
        let meta = self.games.get(game_code)?;
        let is_p2p = !meta.host_peer_id.is_empty();
        Some(JoinTargetInfo {
            host_peer_id: meta.host_peer_id.clone(),
            max_players: meta.max_players,
            current_players: (meta.current_players + meta.reservations.len() as u32)
                .min(meta.max_players),
            format_config: meta.format_config.clone(),
            match_config: meta.match_config,
            is_p2p,
            reservation_token: None,
            reservation_expires_at_ms: None,
            draft_metadata: meta.draft_metadata.clone(),
        })
    }

    pub fn timer_seconds(&self, game_code: &str) -> Option<u32> {
        self.games
            .get(game_code)
            .and_then(|meta| meta.timer_seconds)
    }

    /// Reports the games whose host has shown no liveness for longer than
    /// `timeout_secs` **without removing them**. A listing's liveness clock
    /// starts at registration and is advanced by [`Self::refresh_liveness`].
    ///
    /// **Reporting is not consuming.** The Full-mode sweep declines to act on a
    /// game whose session is contended (`SessionManager::try_session` never
    /// waits), so erasing the entry on report made that decline permanent: the
    /// listing was already gone, nothing re-reported it, and the unstarted
    /// session it named never retired — its game code stayed held until a later
    /// startup aged it out. A reported entry is consumed by
    /// [`Self::unregister_expired`], which the sweep calls once it has
    /// established there is nothing left to retire; until then the same code is
    /// reported on the next tick, which is the retry the sweep's cadence
    /// already assumes.
    ///
    /// Each report carries the identity of the registration it observed, not
    /// just its code, because the lock released in between lets a replacement
    /// take that code — see [`LobbyRegistration`].
    ///
    /// [`crate::broker::Broker::reap_expired`] reports and consumes in one
    /// call, which is right for a shell that cannot decline — the Durable
    /// Object has no session registry to contend on.
    /// [`crate::broker::Broker::reap_expired_handled`] is the two-step form for
    /// a shell that can.
    pub fn check_expired(&self, timeout_secs: u64, env: &impl BrokerEnv) -> Vec<LobbyRegistration> {
        let now = env.now_ms() / 1000;
        self.games
            .iter()
            .filter(|(_, meta)| now.saturating_sub(meta.last_seen) > timeout_secs)
            .map(|(code, meta)| LobbyRegistration {
                game_code: code.clone(),
                generation: meta.generation,
            })
            .collect()
    }

    /// Consumes an expiry observation, removing the listing **only** when the
    /// registration currently under that code is the one that was observed.
    ///
    /// The identity check is the whole point: the lobby lock is released
    /// between [`Self::check_expired`] and this call, so `register_game` can
    /// replace the entry in between with a live, unexpired listing. Removing
    /// that replacement would also broadcast a `LobbyGameRemoved` that delists
    /// it for every subscriber — the caller keys that broadcast off the
    /// returned [`ExpiryConsumption`], which is why the two no-op outcomes are
    /// distinguished rather than collapsed.
    ///
    /// [`Self::unregister_game`] stays the unconditional form, for the paths
    /// that own the entry they remove (a host leaving, a game starting) and
    /// hold the lock across their own decision.
    pub fn unregister_expired(&mut self, expired: &LobbyRegistration) -> ExpiryConsumption {
        if self.unregister_registration(expired) {
            ExpiryConsumption::Removed
        } else if self.has_game(&expired.game_code) {
            debug!(
                game = %expired.game_code,
                "lobby expiry observation superseded — replacement listing kept"
            );
            ExpiryConsumption::Superseded
        } else {
            ExpiryConsumption::AlreadyGone
        }
    }

    /// Whether `registration` is the one currently listed under its code — the
    /// single identity check every registration-scoped path goes through. See
    /// [`LobbyRegistration`].
    pub fn is_current(&self, registration: &LobbyRegistration) -> bool {
        self.games
            .get(&registration.game_code)
            .is_some_and(|meta| meta.generation == registration.generation)
    }

    /// Whether [`Self::refresh_liveness`] would advance `registration`'s
    /// liveness clock now: the registration is current and at least
    /// [`LIVENESS_REFRESH_SECS`] have passed since the clock last moved. The
    /// single predicate, so a shell's persistence precheck and the write
    /// cannot drift apart.
    pub fn liveness_refresh_due(
        &self,
        registration: &LobbyRegistration,
        env: &impl BrokerEnv,
    ) -> bool {
        let now = env.now_ms() / 1000;
        self.is_current(registration)
            && self
                .games
                .get(&registration.game_code)
                .is_some_and(|meta| now.saturating_sub(meta.last_seen) >= LIVENESS_REFRESH_SECS)
    }

    /// Records that `registration`'s host is still alive, advancing its
    /// listing's liveness clock to now when [`Self::liveness_refresh_due`],
    /// and returns whether it wrote. Scoped by identity: a registration that
    /// is no longer current (reaped, removed, or superseded) refreshes
    /// nothing, so a stale stamp can never extend a replacement's listing.
    /// `created_at` is never touched.
    pub fn refresh_liveness(
        &mut self,
        registration: &LobbyRegistration,
        env: &impl BrokerEnv,
    ) -> bool {
        if !self.liveness_refresh_due(registration, env) {
            return false;
        }
        let now = env.now_ms() / 1000;
        if let Some(meta) = self.games.get_mut(&registration.game_code) {
            meta.last_seen = now;
        }
        true
    }

    /// Removes `registration`'s listing **only** if it is still the one under
    /// its code, returning whether it removed anything. A replacement listed
    /// under the same code since is left untouched.
    pub fn unregister_registration(&mut self, registration: &LobbyRegistration) -> bool {
        let current = self.is_current(registration);
        if current {
            self.unregister_game(&registration.game_code);
        }
        current
    }
}

impl Default for LobbyManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The codes an expiry report named, for assertions about which listings
    /// were seen rather than which registration.
    fn codes(observed: &[LobbyRegistration]) -> Vec<String> {
        observed.iter().map(|e| e.game_code().to_string()).collect()
    }

    /// Round-trips a manager through the snapshot shape an older build wrote:
    /// no `generation` on any entry and no `next_generation` on the manager.
    fn restore_without_generations(seed: &LobbyManager) -> LobbyManager {
        let mut raw: serde_json::Value = serde_json::to_value(seed).expect("manager serializes");
        raw.as_object_mut().unwrap().remove("next_generation");
        for entry in raw["games"].as_object_mut().unwrap().values_mut() {
            entry.as_object_mut().unwrap().remove("generation");
        }
        serde_json::from_value(raw).expect("an older snapshot still loads")
    }

    /// Round-trips a manager through the snapshot shape a build predating the
    /// liveness clock wrote: no `last_seen` on any entry.
    fn restore_without_last_seen(seed: &LobbyManager) -> LobbyManager {
        let mut raw: serde_json::Value = serde_json::to_value(seed).expect("manager serializes");
        for entry in raw["games"].as_object_mut().unwrap().values_mut() {
            let removed = entry.as_object_mut().unwrap().remove("last_seen");
            assert!(removed.is_some(), "reach guard: the field was serialized");
        }
        serde_json::from_value(raw).expect("an older snapshot still loads")
    }

    /// `FakeEnv::new`'s starting clock, in seconds.
    const T0_SECS: u64 = 1_000;

    fn at_secs(env: &FakeEnv, secs: u64) {
        env.set_now_ms(secs * 1000);
    }
    use engine::types::format::{FormatConfig, GameFormat};
    use engine::types::match_config::MatchConfig;
    use std::cell::Cell;

    /// Deterministic `BrokerEnv` for tests. `now_ms` is settable; tokens and
    /// codes are monotonic counters so assertions are stable.
    struct FakeEnv {
        now: Cell<u64>,
        token_counter: Cell<u64>,
        code_counter: Cell<u64>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                now: Cell::new(1_000_000),
                token_counter: Cell::new(0),
                code_counter: Cell::new(0),
            }
        }
        fn set_now_ms(&self, ms: u64) {
            self.now.set(ms);
        }
    }

    impl BrokerEnv for FakeEnv {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn new_token(&self) -> String {
            let n = self.token_counter.get();
            self.token_counter.set(n + 1);
            format!("token-{n}")
        }
        fn new_game_code(&self) -> String {
            let n = self.code_counter.get();
            self.code_counter.set(n + 1);
            format!("CODE{n:02}")
        }
    }

    fn register_basic(
        lobby: &mut LobbyManager,
        code: &str,
        host: &str,
        public: bool,
        password: Option<String>,
        timer: Option<u32>,
        env: &impl BrokerEnv,
    ) -> LobbyRegistration {
        lobby.register_game(
            code,
            RegisterGameRequest {
                host_name: host.to_string(),
                public,
                password,
                timer_seconds: timer,
                ..Default::default()
            },
            env,
        )
    }

    /// A registration held across its own removal and a re-registration of the
    /// same code — a host socket's stamp after an age reap — names only itself:
    /// it is no longer current and cannot remove the replacement.
    #[test]
    fn a_superseded_registration_cannot_remove_the_replacement() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        let first = register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        assert!(
            lobby.is_current(&first),
            "reach guard: a fresh stamp is current"
        );

        lobby.unregister_game("GAME01");
        assert!(
            !lobby.is_current(&first),
            "a removed registration is not current"
        );
        let second = register_basic(&mut lobby, "GAME01", "Bob", true, None, None, &env);

        assert!(!lobby.is_current(&first));
        assert!(!lobby.unregister_registration(&first), "nothing removed");
        assert!(lobby.is_current(&second), "the replacement stays listed");

        assert!(
            lobby.unregister_registration(&second),
            "its own holder removes it"
        );
        assert!(!lobby.has_game("GAME01"));
    }

    #[test]
    fn register_and_list_public_games() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        register_basic(&mut lobby, "GAME02", "Bob", false, None, None, &env);
        register_basic(
            &mut lobby,
            "GAME03",
            "Carol",
            true,
            Some("pw".to_string()),
            Some(60),
            &env,
        );

        let public = lobby.public_games();
        assert_eq!(public.len(), 2);
        let codes: Vec<&str> = public.iter().map(|g| g.game_code.as_str()).collect();
        assert!(codes.contains(&"GAME01"));
        assert!(codes.contains(&"GAME03"));
    }

    #[test]
    fn public_game_derives_is_p2p_from_host_peer_id() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "FULL01",
            RegisterGameRequest {
                host_name: "FullHost".to_string(),
                public: true,
                ..Default::default()
            },
            &env,
        );
        lobby.register_game(
            "P2P01",
            RegisterGameRequest {
                host_name: "BrokerHost".to_string(),
                public: true,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );

        let full = lobby.public_game("FULL01").expect("full entry listed");
        let p2p = lobby.public_game("P2P01").expect("p2p entry listed");
        assert!(!full.is_p2p);
        assert!(p2p.is_p2p);

        let all = lobby.public_games();
        let full = all.iter().find(|g| g.game_code == "FULL01").unwrap();
        let p2p = all.iter().find(|g| g.game_code == "P2P01").unwrap();
        assert!(!full.is_p2p);
        assert!(p2p.is_p2p);
    }

    #[test]
    fn unregister_removes_game() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        assert_eq!(lobby.public_games().len(), 1);

        lobby.unregister_game("GAME01");
        assert_eq!(lobby.public_games().len(), 0);
    }

    #[test]
    fn verify_password_no_password_required() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);

        assert!(lobby.verify_password("GAME01", None).is_ok());
        assert!(lobby.verify_password("GAME01", Some("anything")).is_ok());
    }

    #[test]
    fn verify_password_correct() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
            &env,
        );

        assert!(lobby.verify_password("GAME01", Some("secret")).is_ok());
    }

    #[test]
    fn verify_password_wrong() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
            &env,
        );

        let result = lobby.verify_password("GAME01", Some("wrong"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Wrong password");
    }

    #[test]
    fn verify_password_required_but_missing() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
            &env,
        );

        let result = lobby.verify_password("GAME01", None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "password_required");
    }

    /// Pins the answer a joiner gets once the entry is gone: "not found", not
    /// "Wrong password". A destroyed session's entry is delisted, so this is
    /// the arm every stale code now lands in.
    #[test]
    fn verify_password_game_not_found() {
        let lobby = LobbyManager::new();
        let result = lobby.verify_password("NOPE", None);
        assert_eq!(result.unwrap_err(), "Game not found in lobby: NOPE");
    }

    #[test]
    fn timer_seconds_returns_configured_value() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, Some(90), &env);
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None, &env);

        assert_eq!(lobby.timer_seconds("GAME01"), Some(90));
        assert_eq!(lobby.timer_seconds("GAME02"), None);
        assert_eq!(lobby.timer_seconds("NOPE"), None);
    }

    #[test]
    fn check_expired_reports_old_games_without_consuming_them() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        env.set_now_ms(1_000_000 + 301_000);

        let expired = lobby.check_expired(300, &env);
        assert_eq!(codes(&expired), vec!["GAME01".to_string()]);

        // Reporting is not consuming. The sweep that acts on this code can
        // decline — its session may be mid-transition — and an entry erased on
        // report is one nothing ever reports again.
        assert_eq!(
            codes(&lobby.check_expired(300, &env)),
            vec!["GAME01".to_string()],
            "a tick that did not act must leave the entry for the next one"
        );
        assert_eq!(
            lobby.public_games().len(),
            1,
            "and the listing stands until someone disposes of it"
        );

        // Consumption is the caller handing the observation back.
        assert_eq!(
            lobby.unregister_expired(&expired[0]),
            ExpiryConsumption::Removed
        );
        assert!(lobby.check_expired(300, &env).is_empty());
        assert!(lobby.public_games().is_empty());
    }

    /// The blocker this identity exists for: the lobby lock is released between
    /// the report and the consume, so a `register_game` can take the code in
    /// between. Consuming by code alone would delete that replacement — a live,
    /// unexpired listing — and tell every subscriber to delist it.
    #[test]
    fn a_replacement_registered_after_the_report_is_not_consumed_by_it() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        env.set_now_ms(1_000_000 + 301_000);

        let observed = lobby.check_expired(300, &env);
        assert_eq!(
            codes(&observed),
            vec!["GAME01".to_string()],
            "reach guard: the lapsed listing really was observed"
        );

        // The gap. A new host takes the freed code while the lobby lock is down.
        register_basic(&mut lobby, "GAME01", "Bob", true, None, None, &env);

        assert_eq!(
            lobby.unregister_expired(&observed[0]),
            ExpiryConsumption::Superseded,
            "the observation names a registration that is no longer listed"
        );
        let games = lobby.public_games();
        assert_eq!(games.len(), 1, "the replacement keeps its listing");
        assert_eq!(
            games[0].host_name, "Bob",
            "and it is the replacement that stands, not the entry that lapsed"
        );
        assert!(
            lobby.check_expired(300, &env).is_empty(),
            "the replacement is fresh, so it is not itself reported as expired"
        );
    }

    /// The outcome that must stay an announcement: nothing is listed, because
    /// another path removed it between the report and the consume. Distinct
    /// from `Superseded`, whose whole point is that subscribers hear nothing.
    #[test]
    fn an_entry_removed_by_another_path_is_reported_already_gone() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        env.set_now_ms(1_000_000 + 301_000);

        let observed = lobby.check_expired(300, &env);
        lobby.unregister_game("GAME01");

        assert_eq!(
            lobby.unregister_expired(&observed[0]),
            ExpiryConsumption::AlreadyGone
        );
    }

    /// The ordering a floor derived from the *live* map gets wrong: removing an
    /// entry lowers that floor, so emptying the map hands the next registration
    /// the identity an outstanding observation still names. Seeding the counter
    /// once at load makes it monotone, and no removal can undo it.
    #[test]
    fn an_identity_is_not_reissued_after_the_observed_entry_is_removed() {
        let env = FakeEnv::new();
        let mut seed = LobbyManager::new();
        register_basic(&mut seed, "GAME01", "Alice", true, None, None, &env);
        env.set_now_ms(1_000_000 + 301_000);
        let mut lobby = restore_without_generations(&seed);

        let observed = lobby.check_expired(300, &env);
        assert_eq!(
            codes(&observed),
            vec!["GAME01".to_string()],
            "reach guard: the restored entry is the one observed"
        );

        // Another path disposes of the entry, emptying the map, before the
        // code is registered again — the sequence that used to reset the floor.
        lobby.unregister_game("GAME01");
        register_basic(&mut lobby, "GAME01", "Bob", true, None, None, &env);

        assert_eq!(
            lobby.unregister_expired(&observed[0]),
            ExpiryConsumption::Superseded,
            "an identity an outstanding observation names must never be reissued"
        );
        assert_eq!(
            lobby.public_games()[0].host_name,
            "Bob",
            "so the replacement survives"
        );
    }

    /// The arm that makes `next_generation` worth serializing at all: with the
    /// map empty there is nothing to seed *from*, so only the stored counter
    /// carries the identities already handed out. Without it a hibernation
    /// round-trip taken while no game is listed would restart at `0` and
    /// reissue every identity in order.
    #[test]
    fn an_emptied_manager_carries_its_counter_across_a_round_trip() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None, &env);
        lobby.unregister_game("GAME01");
        lobby.unregister_game("GAME02");
        assert!(
            lobby.public_games().is_empty(),
            "reach guard: the map is empty, so the floor can seed nothing"
        );

        let json = serde_json::to_string(&lobby).expect("manager serializes");
        let mut restored: LobbyManager = serde_json::from_str(&json).expect("and deserializes");

        register_basic(&mut restored, "GAME03", "Carol", true, None, None, &env);
        assert_eq!(
            restored.games["GAME03"].generation, 2,
            "the next identity must follow the two already issued, not restart"
        );
    }

    /// Restore safety. The whole `Broker` round-trips through serde for Durable
    /// Object hibernation, and a snapshot written before these fields existed
    /// deserializes the counter *and* every entry's generation as `0`. Without
    /// the floor in `allocate_generation`, the first registration after such a
    /// restore would be handed `0` — the identity the restored entry already
    /// carries — and `Superseded` would collapse back into `Removed`.
    #[test]
    fn a_registration_after_a_generation_less_restore_cannot_reuse_an_identity() {
        let env = FakeEnv::new();
        let mut seed = LobbyManager::new();
        register_basic(&mut seed, "GAME01", "Alice", true, None, None, &env);
        env.set_now_ms(1_000_000 + 301_000);

        let mut lobby = restore_without_generations(&seed);

        let observed = lobby.check_expired(300, &env);
        assert_eq!(
            codes(&observed),
            vec!["GAME01".to_string()],
            "reach guard: the restored entry is the one observed"
        );

        register_basic(&mut lobby, "GAME01", "Bob", true, None, None, &env);
        assert_eq!(
            lobby.unregister_expired(&observed[0]),
            ExpiryConsumption::Superseded,
            "a restored entry's identity must not be handed out again"
        );
        assert_eq!(
            lobby.public_games()[0].host_name,
            "Bob",
            "so the replacement survives a restore exactly as it does without one"
        );
    }

    /// A listing whose host showed liveness is aged from that refresh, not
    /// from its creation — and the clock still expires once the host is quiet.
    #[test]
    fn a_refreshed_listing_outlives_its_creation_age() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        let live = register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None, &env);

        at_secs(&env, T0_SECS + 250);
        assert!(
            lobby.refresh_liveness(&live, &env),
            "reach: the refresh wrote"
        );

        at_secs(&env, T0_SECS + 350);
        assert_eq!(
            codes(&lobby.check_expired(300, &env)),
            vec!["GAME02".to_string()],
            "the refreshed listing is kept past its creation age; the never-refreshed control is reported"
        );

        at_secs(&env, T0_SECS + 250 + 301);
        let mut reported = codes(&lobby.check_expired(300, &env));
        reported.sort();
        assert_eq!(
            reported,
            vec!["GAME01".to_string(), "GAME02".to_string()],
            "a quiet host's listing still lapses, measured from its last refresh"
        );
    }

    /// The refresh only writes once a full quantum has passed, and reports
    /// exactly whether it wrote.
    #[test]
    fn a_refresh_inside_the_quantum_writes_nothing() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        let reg = register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);

        at_secs(&env, T0_SECS + 30);
        assert!(!lobby.liveness_refresh_due(&reg, &env));
        assert!(
            !lobby.refresh_liveness(&reg, &env),
            "inside the quantum nothing is written"
        );
        at_secs(&env, T0_SECS + 301);
        assert_eq!(
            codes(&lobby.check_expired(300, &env)),
            vec!["GAME01".to_string()],
            "the clock did not move, so the listing still lapses from its registration"
        );

        // Boundary: exactly one quantum after the clock last moved is due.
        let other = register_basic(&mut lobby, "GAME02", "Bob", true, None, None, &env);
        at_secs(&env, T0_SECS + 301 + LIVENESS_REFRESH_SECS - 1);
        assert!(!lobby.refresh_liveness(&other, &env));
        at_secs(&env, T0_SECS + 301 + LIVENESS_REFRESH_SECS);
        assert!(lobby.liveness_refresh_due(&other, &env));
        assert!(lobby.refresh_liveness(&other, &env));
        assert!(
            !lobby.liveness_refresh_due(&other, &env),
            "a refresh restarts the quantum"
        );
    }

    /// A registration held across its own removal and a re-registration of the
    /// same code cannot keep the replacement alive.
    #[test]
    fn a_superseded_registration_cannot_refresh_the_replacement() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        let first = register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        lobby.unregister_game("GAME01");

        let t1 = T0_SECS + 10;
        at_secs(&env, t1);
        let second = register_basic(&mut lobby, "GAME01", "Bob", true, None, None, &env);

        at_secs(&env, t1 + 200);
        assert!(
            !lobby.refresh_liveness(&first, &env),
            "a superseded stamp refreshes nothing"
        );

        at_secs(&env, t1 + 301);
        let reported = lobby.check_expired(300, &env);
        assert_eq!(
            reported,
            vec![second.clone()],
            "the replacement ages from its own registration"
        );
        assert!(
            lobby.refresh_liveness(&second, &env),
            "reach: the current holder's refresh does fire"
        );
    }

    /// The wire `created_at` is the advertised listing time the client sorts
    /// and renders from; a liveness refresh must not move it.
    #[test]
    fn a_refresh_leaves_the_advertised_created_at_alone() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        let reg = register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);

        at_secs(&env, T0_SECS + 120);
        assert!(
            lobby.refresh_liveness(&reg, &env),
            "reach: the refresh wrote"
        );
        assert_eq!(
            lobby.public_game("GAME01").expect("listed").created_at,
            T0_SECS
        );
    }

    /// A snapshot from a build without `last_seen` restores with the clock at
    /// the registration time, not `0` — otherwise the first sweep after a
    /// deploy would reap every restored listing.
    #[test]
    fn a_snapshot_without_last_seen_ages_from_created_at() {
        let env = FakeEnv::new();
        let mut seed = LobbyManager::new();
        register_basic(&mut seed, "GAME01", "Alice", true, None, None, &env);
        let lobby = restore_without_last_seen(&seed);
        assert!(lobby.has_game("GAME01"), "reach guard: the entry restored");

        at_secs(&env, T0_SECS + 299);
        assert!(lobby.check_expired(300, &env).is_empty());
        at_secs(&env, T0_SECS + 301);
        assert_eq!(
            codes(&lobby.check_expired(300, &env)),
            vec!["GAME01".to_string()]
        );
    }

    /// The persisted field is the one consumed: a refreshed clock survives a
    /// snapshot round-trip, which is the Durable Object restore path.
    #[test]
    fn a_snapshot_round_trip_keeps_a_refreshed_clock() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        let reg = register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        at_secs(&env, T0_SECS + 250);
        assert!(
            lobby.refresh_liveness(&reg, &env),
            "reach: the refresh wrote"
        );

        let json = serde_json::to_string(&lobby).expect("manager serializes");
        let restored: LobbyManager = serde_json::from_str(&json).expect("and deserializes");
        assert!(
            restored.has_game("GAME01"),
            "reach guard: the entry restored"
        );

        at_secs(&env, T0_SECS + 350);
        assert!(restored.check_expired(300, &env).is_empty());
    }

    #[test]
    fn check_expired_retains_and_does_not_report_fresh_games() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);

        let expired = lobby.check_expired(300, &env);
        assert!(expired.is_empty());
        assert_eq!(lobby.public_games().len(), 1);
    }

    #[test]
    fn lobby_game_has_password_flag() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("pw".to_string()),
            None,
            &env,
        );
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None, &env);

        let games = lobby.public_games();
        let g1 = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        let g2 = games.iter().find(|g| g.game_code == "GAME02").unwrap();
        assert!(g1.has_password);
        assert!(!g2.has_password);
    }

    #[test]
    fn host_build_commit_returned_from_register() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                host_version: "0.1.11".to_string(),
                host_build_commit: "abc1234".to_string(),
                ..Default::default()
            },
            &env,
        );
        assert_eq!(lobby.host_build_commit("GAME01"), Some("abc1234"));
        assert_eq!(lobby.host_build_commit("NOPE"), None);

        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.host_version, "0.1.11");
        assert_eq!(g.host_build_commit, "abc1234");
    }

    #[test]
    fn extended_fields_roundtrip_through_public_games() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 2,
                max_players: 4,
                format_config: Some(FormatConfig::commander()),
                ..Default::default()
            },
            &env,
        );
        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.current_players, 2);
        assert_eq!(g.max_players, 4);
        assert_eq!(g.format, Some(GameFormat::Commander));
    }

    #[test]
    fn set_current_players_updates_existing_entry() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 1,
                max_players: 4,
                ..Default::default()
            },
            &env,
        );

        lobby.set_current_players("GAME01", 3, &env);
        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.current_players, 3);
    }

    #[test]
    fn public_game_returns_entry_when_public() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 2,
                max_players: 4,
                format_config: Some(FormatConfig::commander()),
                ..Default::default()
            },
            &env,
        );

        let game = lobby.public_game("GAME01").expect("entry should exist");
        assert_eq!(game.game_code, "GAME01");
        assert_eq!(game.current_players, 2);
        assert_eq!(game.format, Some(GameFormat::Commander));
    }

    #[test]
    fn public_game_returns_none_for_private_entry() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", false, None, None, &env);
        assert!(lobby.public_game("GAME01").is_none());
    }

    #[test]
    fn public_game_returns_none_for_missing_entry() {
        let lobby = LobbyManager::new();
        assert!(lobby.public_game("NOPE").is_none());
    }

    #[test]
    fn join_target_info_returns_atomic_snapshot() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                host_peer_id: "peer-xyz".to_string(),
                current_players: 1,
                max_players: 4,
                format_config: Some(FormatConfig::commander()),
                ..Default::default()
            },
            &env,
        );
        assert_eq!(
            lobby.join_target_info("GAME01"),
            Some(JoinTargetInfo {
                host_peer_id: "peer-xyz".to_string(),
                max_players: 4,
                current_players: 1,
                format_config: Some(FormatConfig::commander()),
                match_config: MatchConfig::default(),
                is_p2p: true,
                reservation_token: None,
                reservation_expires_at_ms: None,
                draft_metadata: None,
            })
        );
    }

    #[test]
    fn join_target_info_returns_none_for_missing_game() {
        let lobby = LobbyManager::new();
        assert_eq!(lobby.join_target_info("NOPE"), None);
    }

    #[test]
    fn join_target_info_marks_full_mode_entries_as_non_p2p() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                format_config: Some(FormatConfig::standard()),
                ..Default::default()
            },
            &env,
        );
        assert_eq!(
            lobby.join_target_info("GAME01"),
            Some(JoinTargetInfo {
                host_peer_id: String::new(),
                max_players: 0,
                current_players: 0,
                format_config: Some(FormatConfig::standard()),
                match_config: MatchConfig::default(),
                is_p2p: false,
                reservation_token: None,
                reservation_expires_at_ms: None,
                draft_metadata: None,
            })
        );
        assert!(lobby.has_game("GAME01"));
    }

    #[test]
    fn len_and_is_empty_reflect_registration() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        assert!(lobby.is_empty());
        assert_eq!(lobby.len(), 0);
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        assert!(!lobby.is_empty());
        assert_eq!(lobby.len(), 1);
        lobby.unregister_game("GAME01");
        assert!(lobby.is_empty());
    }

    #[test]
    fn set_current_players_no_op_on_missing_game() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.set_current_players("NOPE", 5, &env);
        assert!(lobby.public_games().is_empty());
    }

    #[test]
    fn public_seat_reservation_expires_at_uses_env_clock() {
        let env = FakeEnv::new();
        env.set_now_ms(5_000);
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                max_players: 4,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );
        let res = lobby
            .reserve_seat("GAME01", "Bob".to_string(), &env)
            .expect("seat reserved");
        assert_eq!(res.expires_at_ms, Some(5_000 + PUBLIC_SEAT_RESERVATION_MS));
    }

    #[test]
    fn password_protected_reservation_expires() {
        let env = FakeEnv::new();
        env.set_now_ms(5_000);
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                password: Some("pw".to_string()),
                max_players: 4,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );
        let res = lobby
            .reserve_seat("GAME01", "Bob".to_string(), &env)
            .expect("seat reserved");
        assert_eq!(res.expires_at_ms, Some(5_000 + PUBLIC_SEAT_RESERVATION_MS));
    }

    #[test]
    fn expired_reservation_is_cleaned_up() {
        let env = FakeEnv::new();
        env.set_now_ms(1_000);
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                max_players: 4,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );
        lobby
            .reserve_seat("GAME01", "Bob".to_string(), &env)
            .expect("seat reserved");
        // Advance past the reservation lifetime.
        env.set_now_ms(1_000 + PUBLIC_SEAT_RESERVATION_MS + 1);
        assert!(lobby.cleanup_expired_reservations("GAME01", &env));
        // Public listing no longer counts the lapsed reservation.
        let g = lobby.public_game("GAME01").unwrap();
        assert_eq!(g.current_players, 0);
    }
}
