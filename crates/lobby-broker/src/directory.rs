//! The server-directory contract: what a server may claim about itself, and
//! what a directory is allowed to believe.
//!
//! This module is the single authority shared by every party in the directory
//! flow — the native `phase-server` shell (which announces itself), the
//! Cloudflare Durable Object shell (which accepts announcements through the
//! `broker-wasm` boundary), and, from a later phase, the client's TypeScript
//! mirror. One rule set, applied byte-for-byte identically wherever an
//! announcement is examined.
//!
//! Like the rest of this crate it holds **no I/O, no `SystemTime`, no `rand`
//! and no tokio**, so it compiles to `wasm32-unknown-unknown`. Timestamps
//! arrive as arguments (see [`score`]) rather than being read from a clock,
//! mirroring the [`crate::env::BrokerEnv`] injection pattern.
//!
//! Like [`crate::validation`], this module does **size/shape/reachability
//! policy only** and never content moderation: it will refuse a name that is
//! too long or a host that cannot be dialled from the public internet, and it
//! will never have an opinion about what a name says. Moderation stays shell
//! policy.
//!
//! **Versioning asymmetry, on purpose.** [`ServerAnnouncement`] carries
//! [`DIRECTORY_VERSION`] and is version-gated; [`ServerInfoDocument`] carries
//! no version field and is not. An announcement is an unauthenticated *write
//! into* the directory, and the gate is what stops a future shape from being
//! silently accepted. The info document is a server's *self-description that
//! already exists in production* — `lobby-worker/src/lobby-do.ts` has served it
//! since before `DIRECTORY_VERSION` existed — so a required version field
//! would break every deployed broker on day one. Its compatibility contract is
//! structural instead: four required fields every existing producer already
//! emits, plus `Option` for everything added later.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::protocol::ServerMode;
use crate::validation::{validate_required_label, validate_token, MAX_TOKEN_LEN};

/// Version of the announcement shape itself. An announcement declaring a
/// different value is refused outright rather than partially interpreted:
/// the directory accepts writes from unauthenticated third-party servers, so a
/// shape it does not recognise must never be stored on a best-effort reading.
pub const DIRECTORY_VERSION: u32 = 1;

/// The single HTTP path every phase server kind answers with its
/// [`ServerInfoDocument`]. Routers register this constant, never a literal, so
/// the probe path is the same one for every server rather than the one that
/// happened to be typed at each site.
pub const INFO_PATH: &str = "/info";

/// Max announced-URL length, in bytes. A dialable `wss://` address is short;
/// this is a generous ceiling that still refuses multi-kilobyte junk from an
/// unauthenticated announcer.
pub const MAX_SERVER_URL_LEN: usize = 256;

/// Max server-name length, in characters. Deliberately **not**
/// [`crate::validation::MAX_ROOM_NAME_LEN`]: a server name is a host-derived
/// operator identity, not a user-typed room label, and the two caps have
/// independent reasons to move.
pub const MAX_SERVER_NAME_LEN: usize = 64;

/// Ceiling on the player count an announcement may claim. The announce is
/// unauthenticated and this number is displayed, so it is bounded for the same
/// reason [`crate::validation::MAX_PLAYER_COUNT`] and
/// [`crate::validation::MAX_TIMER_SECONDS`] are.
pub const MAX_ANNOUNCED_PLAYERS: u32 = 10_000;

/// Last labels that cannot name a host reachable from the public internet.
/// Not cosmetic: verifying an announcement means issuing an outbound fetch to
/// the announced host from the directory's own runtime, so a name that only
/// resolves inside someone's network is both useless in a listing and an
/// SSRF-shaped surface.
const NON_PUBLIC_TLDS: [&str; 4] = ["localhost", "local", "internal", "arpa"];

/// A `wss://` URL that has passed every rule in [`normalize_announced_url`].
///
/// Two properties make this a *proof* rather than a label:
///   1. the field is private and this module exposes no other constructor, and
///   2. `#[serde(try_from = "String")]` routes EVERY deserialization through
///      that same constructor — a derived `Deserialize` would be generated in
///      this module, see the private field, and silently become a second,
///      unvalidating way in.
///
/// `Serialize` stays derived: a newtype struct serializes as its inner value,
/// so the wire form is a plain JSON string.
///
/// **Do not** add `From<String>`, make the field public, add an inherent
/// `new`, or replace the `try_from` attribute with a plain derived
/// `Deserialize`. Each of those silently reopens the hole while every runtime
/// assertion about this type keeps passing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct AnnouncedUrl(String);

impl AnnouncedUrl {
    /// The canonical `wss://` form. The only accessor; there is deliberately
    /// no way to take the inner `String` back out mutably.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AnnouncedUrl {
    type Error = String;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        normalize_announced_url(&raw)
    }
}

/// An announcement that has passed [`validate_announcement`].
///
/// The fields are private and there is **no `Deserialize`**, so the only ways
/// to obtain a value are [`validate_announcement`] and
/// [`ServerAnnouncement::with_current_players`]. That is what makes "a
/// `ServerAnnouncement` value is proof the rules ran" literally true: a derived
/// `Deserialize` here would accept a 500-character name and an out-of-range
/// player count, since `AnnouncedUrl`'s own `try_from` closes only the URL
/// field. Untrusted JSON deserializes into [`RawAnnouncement`] instead.
///
/// **Do not** add a `Deserialize` derive *or* a hand-written impl, make any
/// field `pub`, or add a constructor besides [`validate_announcement`].
/// `with_current_players` is the only mutator. None of those violations has a
/// runtime symptom — the guarantee is structural.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerAnnouncement {
    directory_version: u32,
    url: AnnouncedUrl,
    name: String,
    mode: ServerMode,
    server_version: String,
    protocol_version: u32,
    lobby_protocol_version: u32,
    current_players: u32,
}

impl ServerAnnouncement {
    /// The canonical announced address. This is the directory's primary key.
    pub fn url(&self) -> &AnnouncedUrl {
        &self.url
    }

    /// The operator-facing server name, already bounded by
    /// [`MAX_SERVER_NAME_LEN`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The role this server claims — verified against its
    /// [`ServerInfoDocument`] by [`compare_announcement_to_info`].
    pub fn mode(&self) -> ServerMode {
        self.mode
    }

    /// Players connected at the moment this announcement was built.
    pub fn current_players(&self) -> u32 {
        self.current_players
    }

    /// The announcement-shape version this value was validated against; always
    /// [`DIRECTORY_VERSION`].
    pub fn directory_version(&self) -> u32 {
        self.directory_version
    }

    /// The second and last constructor: refresh only the live player count.
    ///
    /// A heartbeat re-sends the same announcement every period with one field
    /// moving. Clamping to [`MAX_ANNOUNCED_PLAYERS`] here means the result
    /// still satisfies every rule [`validate_announcement`] checked, so the
    /// caller neither re-runs validation nor has a per-tick failure to handle.
    pub fn with_current_players(&self, current_players: u32) -> ServerAnnouncement {
        ServerAnnouncement {
            current_players: current_players.min(MAX_ANNOUNCED_PLAYERS),
            ..self.clone()
        }
    }
}

/// The unvalidated wire shape of an announcement — what actually arrives on
/// the announce endpoint.
///
/// Its fields are `pub` precisely *because* it carries no invariant; the
/// contrast with [`ServerAnnouncement`]'s private fields is the point.
/// Unknown fields are ignored (serde's default): `directory_version` already
/// refuses a cross-version body, so an unknown field is a same-version
/// additive one, and `deny_unknown_fields` would make additive evolution a
/// breaking change for no security gain — every field is independently
/// validated anyway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawAnnouncement {
    pub directory_version: u32,
    pub url: String,
    pub name: String,
    pub mode: ServerMode,
    pub server_version: String,
    pub protocol_version: u32,
    pub lobby_protocol_version: u32,
    pub current_players: u32,
}

/// A server's self-description, served over plain HTTP at [`INFO_PATH`] by
/// both server kinds and fetched as the *evidence* for an announcement's
/// *claim*.
///
/// No invariant, so no privacy: this is a parsed view of someone else's
/// document, and every consumer compares its fields rather than trusting them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfoDocument {
    pub mode: ServerMode,
    pub protocol_version: u32,
    pub lobby_protocol_version: u32,
    pub server_version: String,
    /// Absent from the Cloudflare Durable Object's document
    /// (`lobby-worker/src/lobby-do.ts`, the four-field body), which predates
    /// this type — hence `Option` rather than a required field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,
    /// Absent from the Durable Object's document for the same reason as
    /// `build_commit`; also genuinely `None` on a phase-server with no
    /// advertisable public URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

/// Verdict of comparing an announcement's claim against the info document
/// fetched from the announced host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InfoMatch {
    Match,
    Mismatch { field: InfoMismatchField },
}

/// Which compared field disagreed. A typed field rather than a `bool` or a
/// reason string, so a consumer across the wasm boundary can switch on it.
///
/// The two version selectors name *different* numbers and are deliberately
/// separate variants: `ProtocolVersion` is the full-game wire surface
/// ([`crate::PROTOCOL_VERSION`]) and `LobbyProtocolVersion` is the lobby
/// message set ([`crate::LOBBY_PROTOCOL_VERSION`]), which move independently.
/// Collapsing them into one `Version` selector would tell a listing client
/// that a server disagrees about *a* version without saying which — and the
/// two drive different client affordances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InfoMismatchField {
    Mode,
    ServerVersion,
    ProtocolVersion,
    LobbyProtocolVersion,
}

/// Normalise and validate an announced `wss://` URL, returning the canonical
/// form.
///
/// Ten ordered rules, each with one exit. Normalisation is what makes the
/// result an *identity* rather than a spelling: one server must not be able to
/// occupy two directory rows by announcing two spellings of one address.
///
/// Interior path slashes are deliberately **preserved** (rule 10): the path is
/// opaque to the directory, and no directory is entitled to decide that `//ws`
/// and `/ws` are the same route on someone else's reverse proxy.
pub fn normalize_announced_url(raw: &str) -> Result<AnnouncedUrl, String> {
    // Rule 1: trim. An untrimmed value would otherwise be stored and handed
    // out verbatim. Note this removes *whitespace* only — a trailing NUL or
    // DEL survives to rule 2 and is refused there.
    let trimmed = raw.trim();

    // Rule 2: byte cap and control characters, both from the shared bound
    // helper. The two halves catch different things: the cap catches a long
    // URL whose every label is short, the control-character check catches a
    // control byte anywhere, including in the path where rule 8 never looks.
    validate_token("url", trimmed, MAX_SERVER_URL_LEN)?;

    // Rule 3: lowercase `wss://` only. Not cosmetic in either direction — the
    // directory keys on this string, so two accepted spellings of the scheme
    // mint two rows for one server; and because rule 10 rebuilds the value
    // with an unconditional `wss://` prefix, accepting other schemes here
    // would silently *rewrite* a plaintext `ws://` announcement into a `wss://`
    // listing.
    let rest = trimmed
        .strip_prefix("wss://")
        .ok_or_else(|| "url must start with wss://".to_string())?;

    // Rule 4: no query, fragment, userinfo or space. `wss://evil@real.example`
    // is the classic host-confusion vector and has no place in a dialable
    // directory address.
    if rest.contains(['?', '#', '@', ' ']) {
        return Err("url must not contain a query, fragment, userinfo or space".to_string());
    }

    // Rule 5: split authority from path at the first `/`, then take an
    // optional port. `parse::<u16>()` rejects out-of-range and negative ports
    // by construction. It is also what refuses an *un-ported* bracketed IPv6
    // authority: `rsplit_once(':')` on `[::1]` leaves `1]`, which is not a
    // port. (A *ported* bracketed authority parses here and is rejected by
    // rule 8 instead. Both forms are refused; a dedicated bracket guard would
    // discriminate nothing, which is why there is not one.)
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => {
            let port: u16 = port
                .parse()
                .map_err(|_| "url port must be a number in 1..=65535".to_string())?;
            if port == 0 {
                return Err("url port must be a number in 1..=65535".to_string());
            }
            (host, Some(port))
        }
        None => (authority, None),
    };

    // Rule 6: ASCII-lowercase the host and drop one trailing DNS root label.
    // `host.example.` and `host.example` are one name and must be one key.
    let lowered = host.to_ascii_lowercase();
    let host = lowered.strip_suffix('.').unwrap_or(&lowered);

    // Rule 7: no IP literals. Covers every IPv4 address (including link-local
    // metadata addresses) and bare IPv6.
    if host.parse::<IpAddr>().is_ok() {
        return Err("url host must be a DNS name, not an IP literal".to_string());
    }

    // Rule 8: label rules — at least two labels, each non-empty, at most 63
    // bytes, ASCII alphanumeric or hyphen, and neither hyphen-initial nor
    // hyphen-final. Non-ASCII is refused so an IDN is announced as its punycode
    // A-label and one name has one spelling. Which of these sub-checks fires
    // for a given input depends on the label split and is deliberately not
    // pinned, here or in any test; rule 5 carries the bracketed-IPv6 record.
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return Err("url host must be a public DNS name with at least two labels".to_string());
    }
    for label in &labels {
        if label.is_empty() {
            return Err("url host labels must not be empty".to_string());
        }
        if label.len() > 63 {
            return Err("url host labels must be at most 63 bytes".to_string());
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("url host labels must be ASCII letters, digits or hyphens".to_string());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("url host labels must not start or end with a hyphen".to_string());
        }
    }

    // Rule 9: refuse names that cannot be reached from the public internet.
    let last_label = labels.last().copied().unwrap_or_default();
    if NON_PUBLIC_TLDS.contains(&last_label) {
        return Err("url host must be reachable from the public internet".to_string());
    }

    // Rule 10: rebuild. `:443` is the `wss` scheme default and is dropped, so
    // `wss://h/ws` and `wss://h:443/ws` are one key. Trailing slashes are
    // trimmed for the same reason. Interior slashes are left alone — see the
    // function doc comment.
    let port_suffix = match port {
        Some(443) | None => String::new(),
        Some(port) => format!(":{port}"),
    };
    let path = path.trim_end_matches('/');
    Ok(AnnouncedUrl(format!("wss://{host}{port_suffix}{path}")))
}

/// Validate an announcement arriving from an unauthenticated announcer,
/// returning the normalised value.
///
/// Returns the canonical [`ServerAnnouncement`] rather than `()` so no caller
/// can validate and then go on to use the raw input — the same shape
/// `phase-server`'s own `validate_public_url` uses.
pub fn validate_announcement(raw: &RawAnnouncement) -> Result<ServerAnnouncement, String> {
    if raw.directory_version != DIRECTORY_VERSION {
        return Err(format!(
            "directory_version must be {DIRECTORY_VERSION}, got {}",
            raw.directory_version
        ));
    }
    let url = normalize_announced_url(&raw.url)?;
    validate_required_label("name", &raw.name, MAX_SERVER_NAME_LEN)?;
    validate_token("server_version", &raw.server_version, MAX_TOKEN_LEN)?;
    if raw.current_players > MAX_ANNOUNCED_PLAYERS {
        return Err(format!(
            "current_players must be at most {MAX_ANNOUNCED_PLAYERS}"
        ));
    }
    Ok(ServerAnnouncement {
        directory_version: raw.directory_version,
        url,
        name: raw.name.clone(),
        mode: raw.mode,
        server_version: raw.server_version.clone(),
        protocol_version: raw.protocol_version,
        lobby_protocol_version: raw.lobby_protocol_version,
        current_players: raw.current_players,
    })
}

/// Confront an announcement's claim with the info document fetched from the
/// host it announced.
///
/// `mode` is compared first, `server_version` second, `protocol_version`
/// third and `lobby_protocol_version` fourth. The order is contractual rather
/// than implementation-defined, so a document differing in both `mode` and
/// `server_version` reports [`InfoMismatchField::Mode`], and one differing in
/// both version numbers reports [`InfoMismatchField::ProtocolVersion`].
///
/// The order is not arbitrary. `mode` and `server_version` describe *what the
/// server is*, so disagreement there means the announcement is about a
/// different server than the one answering at that address — an identity
/// failure. Disagreement about a version number means one server's two
/// documents disagree — a skew failure, typically a cache or a proxy serving a
/// stale document. Reporting the identity failure first keeps the more serious
/// diagnosis on top.
///
/// **All four fields are compared, and that buys exactly one thing.** A
/// `Match` says the announcer controls the host it announced and that the
/// host's two documents agree with each other. It does **not** make the
/// numbers true: both documents are produced by the same announcer, so a
/// `Match` is evidence of consistency and of control, never of correctness.
/// A client's "behind by N" affordance consumes these numbers and inherits
/// that limit.
pub fn compare_announcement_to_info(
    announcement: &ServerAnnouncement,
    info: &ServerInfoDocument,
) -> InfoMatch {
    if announcement.mode != info.mode {
        return InfoMatch::Mismatch {
            field: InfoMismatchField::Mode,
        };
    }
    if announcement.server_version != info.server_version {
        return InfoMatch::Mismatch {
            field: InfoMismatchField::ServerVersion,
        };
    }
    if announcement.protocol_version != info.protocol_version {
        return InfoMatch::Mismatch {
            field: InfoMismatchField::ProtocolVersion,
        };
    }
    if announcement.lobby_protocol_version != info.lobby_protocol_version {
        return InfoMatch::Mismatch {
            field: InfoMismatchField::LobbyProtocolVersion,
        };
    }
    InfoMatch::Match
}

/// The `https://` [`INFO_PATH`] URL to fetch an announced server's
/// [`ServerInfoDocument`] from.
///
/// Infallible, and that is a property of the argument type rather than of this
/// body: a value of [`AnnouncedUrl`] has already passed
/// [`normalize_announced_url`], so the `wss://` prefix is present and the
/// authority is a public DNS name. A raw `&str` cannot be passed here, which
/// is what stops the evidence-fetching half of the contract from being pointed
/// somewhere the storage half would have refused.
pub fn info_url(url: &AnnouncedUrl) -> String {
    let rest = url.as_str().strip_prefix("wss://").unwrap_or(url.as_str());
    let authority = rest.split('/').next().unwrap_or(rest);
    format!("https://{authority}{INFO_PATH}")
}

// ── Health score ───────────────────────────────────────────────────────────
//
// Client-reported evidence about a listed server, decayed over a day and
// folded into one 0–100 number plus the raw components it was built from.
//
// The whole computation lives here rather than in the Worker for the reason
// the rest of this module does: it is a contract, every party must produce the
// same number from the same evidence, and a directory that scored servers
// differently from the client's own reading of the components would be
// publishing an ordering nobody could check. The Worker owns the counters'
// storage and their bucket arithmetic; this owns what the counters MEAN.
//
// Clock-free like everything else here: `now_ms` is an argument.

/// Width of one counter bucket, in ms.
///
/// Not used by [`score`]'s own arithmetic — the fold sums whatever buckets it
/// is handed, so a caller could supply half-hour buckets and the weighting
/// would still be correct. It exists as the single source of truth for the
/// TypeScript fold that CUTS the buckets, which reads it through the
/// `directory_score_bucket_ms` wasm export rather than declaring its own
/// number. A second declaration would silently mis-cut every bucket and fail
/// no test.
pub const SCORE_BUCKET_MS: u64 = 3_600_000;

/// The decay window. Evidence older than this carries zero weight and is
/// dropped by the fold, which is what bounds a server's stored counters at 24
/// buckets. Both languages use it: Rust weights by it, the TypeScript fold
/// ages buckets out by it.
pub const SCORE_WINDOW_MS: u64 = 24 * SCORE_BUCKET_MS;

/// Weighted samples below which [`Score::value`] is `None`.
///
/// `None` here means "not enough evidence to rank this server", NOT "no
/// evidence": [`Score::samples`] is still populated, so a consumer can tell
/// the two apart. See [`Score::value`].
pub const SCORE_MIN_SAMPLES: u32 = 20;

/// Upper edges of the RTT histogram's first seven cells, in ms. A latency
/// lands in the first cell whose edge it does not EXCEED, so exactly `3200`
/// lands in the seventh; the eighth cell is the overflow (`> 3200`), reported
/// as `3200`.
///
/// A histogram rather than a mean because one 30 s outlier destroys a mean,
/// and rather than a raw sample list because the store has to be aggregatable
/// and decayable — a list is neither.
pub const RTT_BUCKET_EDGES_MS: [u32; 7] = [50, 100, 200, 400, 800, 1600, 3200];

/// Cell count of [`CounterBucket::rtt_histogram`]: one per edge plus the
/// overflow. Derived, never typed twice, so the array cannot drift from the
/// edge list.
pub const RTT_CELLS: usize = RTT_BUCKET_EDGES_MS.len() + 1;

/// Strength of the smoothing prior, in pseudo-observations. See [`smoothed`].
const PRIOR_WEIGHT: f64 = 5.0;
/// The rate the prior assumes before any evidence arrives — deliberately
/// "no opinion", not "good".
const PRIOR_RATE: f64 = 0.5;
/// At or below this median RTT the latency component is a full 1.0.
const RTT_FAST_MS: f64 = 100.0;
/// At or above this median RTT the latency component is 0.0.
const RTT_SLOW_MS: f64 = 1000.0;

/// Component weights. Re-normalised over the components that actually have
/// evidence, so a LobbyOnly broker — which can never start a game — is not
/// dragged by a completion rate it has no way to earn.
const W_SUCCESS: f64 = 0.5;
const W_RTT: f64 = 0.3;
const W_COMPLETION: f64 = 0.2;

/// One hour of client-reported evidence about one server.
///
/// Buckets are cut by the Worker at [`SCORE_BUCKET_MS`] boundaries and folded
/// whole; nothing here is queried by field, which is why the counters live in
/// the Durable Object's key-value storage rather than in a second SQL table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterBucket {
    /// Start of the bucket's window, epoch ms.
    pub start_ms: u64,
    pub connect_attempts: u32,
    pub connect_successes: u32,
    pub games_started: u32,
    pub games_completed: u32,
    /// Counts per [`RTT_BUCKET_EDGES_MS`] cell, last cell being the overflow.
    pub rtt_histogram: [u32; RTT_CELLS],
    /// Peak `current_players` this server ANNOUNCED during the window.
    ///
    /// Written by the announce path, never by a client report, and read by the
    /// game-outcome guard: a server that never had a player online in a window
    /// cannot have completed a game in it. That asymmetry is the point — it is
    /// the one field in this struct a forger cannot raise.
    pub announced_players_max: u32,
}

/// Every live bucket for one server.
///
/// Bounded at one window's worth of entries — 24 at the default hour-wide
/// bucket — but only because the Worker's writers prune it: [`score`] itself
/// merely SKIPS a decayed bucket, so the bound is a property of the write
/// path, not of this type. Both TypeScript writers drop zero-weight buckets
/// before storing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCounters {
    pub buckets: Vec<CounterBucket>,
}

/// A server's health, and the evidence it was computed from.
///
/// The components ride along with the number deliberately: a client renders
/// "slow" or "unreliable" from them, and must never recompute the score
/// itself — one authority for the ordering, several readings of the same
/// evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Score {
    /// 0–100, or `None` when fewer than [`SCORE_MIN_SAMPLES`] weighted samples
    /// remain.
    ///
    /// `None` with a populated [`Score::samples`] means "too little evidence
    /// to rank"; [`score`] returning `None` at all means "no live evidence".
    /// A consumer that treats an absent score as an absent `Score` will render
    /// health hints off a three-sample window.
    pub value: Option<u8>,
    /// Weighted connect attempts plus games started, rounded. Always present,
    /// including when [`Score::value`] is `None` — that is what makes the two
    /// kinds of "no score" distinguishable.
    pub samples: u32,
    /// Raw, UNSMOOTHED connect success rate, 0–1. The smoothing prior applies
    /// to the ranking number, not to the reported evidence.
    pub success_rate: f32,
    /// Raw, unsmoothed completed-of-started rate, 0–1. `0.0` when no game was
    /// started in the window.
    pub completion_rate: f32,
    /// Upper edge of the histogram cell where the weighted median falls, or
    /// `None` when no RTT was ever reported. `3200` means "at least 3200".
    pub median_rtt_ms: Option<u32>,
}

/// Linear decay: full weight at the bucket's start, zero at
/// [`SCORE_WINDOW_MS`].
fn bucket_weight(now_ms: u64, start_ms: u64) -> f64 {
    // `saturating_sub`, not a signed difference: a bucket whose `start_ms` is
    // in the future (a skewed reporter, or a Worker clock behind a stored
    // bucket) is treated as brand new rather than as maximally aged.
    let age = now_ms.saturating_sub(start_ms) as f64;
    let weight = 1.0 - age / SCORE_WINDOW_MS as f64;
    if weight <= 0.0 {
        0.0
    } else {
        weight.min(1.0)
    }
}

/// A rate pulled towards [`PRIOR_RATE`] by [`PRIOR_WEIGHT`] pseudo-samples.
///
/// This is what makes decay MEAN something, and it was measured rather than
/// assumed: re-weighting buckets leaves every *rate* unchanged under uniform
/// ageing, so "identical counters with older timestamps score lower" is simply
/// false without a prior. With one, aged evidence carries less weight against
/// the prior, so a better-than-prior server decays towards it (measured
/// 98 -> 95 over 12 h). It also stops a 1-of-1 perfect server outranking a
/// 1000-of-1000 one.
fn smoothed(numer: f64, denom: f64) -> f64 {
    (numer + PRIOR_WEIGHT * PRIOR_RATE) / (denom + PRIOR_WEIGHT)
}

/// Fold a server's counters into a [`Score`] as of `now_ms`.
///
/// `None` when no live evidence remains at all — never reported, or every
/// bucket aged past [`SCORE_WINDOW_MS`]. A server with evidence but too little
/// of it returns `Some` with a `None` [`Score::value`]; see that field.
///
/// Clock-free: `now_ms` is injected, like every other timestamp in this
/// module.
pub fn score(counters: &ServerCounters, now_ms: u64) -> Option<Score> {
    let mut attempts = 0.0;
    let mut successes = 0.0;
    let mut started = 0.0;
    let mut completed = 0.0;
    let mut rtt = [0.0f64; RTT_CELLS];

    for bucket in &counters.buckets {
        let weight = bucket_weight(now_ms, bucket.start_ms);
        if weight == 0.0 {
            continue;
        }
        attempts += weight * f64::from(bucket.connect_attempts);
        successes += weight * f64::from(bucket.connect_successes);
        started += weight * f64::from(bucket.games_started);
        completed += weight * f64::from(bucket.games_completed);
        for (cell, count) in bucket.rtt_histogram.iter().enumerate() {
            rtt[cell] += weight * f64::from(*count);
        }
    }

    let samples_f = attempts + started;
    if samples_f <= 0.0 {
        return None;
    }

    // Weighted median: the cell where the cumulative count crosses half,
    // reported as that cell's upper edge.
    let rtt_total: f64 = rtt.iter().sum();
    let median_rtt_ms = if rtt_total > 0.0 {
        let half = rtt_total / 2.0;
        let mut cumulative = 0.0;
        let mut cell = RTT_CELLS - 1;
        for (index, count) in rtt.iter().enumerate() {
            cumulative += count;
            if cumulative >= half {
                cell = index;
                break;
            }
        }
        // The overflow cell has no edge of its own and reports the last one,
        // which is why `3200` reads as "at least 3200".
        Some(RTT_BUCKET_EDGES_MS[cell.min(RTT_BUCKET_EDGES_MS.len() - 1)])
    } else {
        None
    };

    let success = smoothed(successes, attempts);
    let completion = smoothed(completed, started);
    let rtt_component = median_rtt_ms.map(|ms| {
        let ms = f64::from(ms);
        let raw = if ms <= RTT_FAST_MS {
            1.0
        } else if ms >= RTT_SLOW_MS {
            0.0
        } else {
            (RTT_SLOW_MS - ms) / (RTT_SLOW_MS - RTT_FAST_MS)
        };
        // Smoothed on the same footing as the two rates, so a single fast
        // sample does not buy a full latency component.
        smoothed(raw * rtt_total, rtt_total)
    });

    // Re-normalise over the components that have evidence.
    let mut weighted = 0.0;
    let mut total_weight = 0.0;
    if attempts > 0.0 {
        weighted += W_SUCCESS * success;
        total_weight += W_SUCCESS;
    }
    if started > 0.0 {
        weighted += W_COMPLETION * completion;
        total_weight += W_COMPLETION;
    }
    if let Some(component) = rtt_component {
        weighted += W_RTT * component;
        total_weight += W_RTT;
    }

    let samples = samples_f.round() as u32;
    let value = if samples < SCORE_MIN_SAMPLES || total_weight == 0.0 {
        None
    } else {
        Some(
            (100.0 * (weighted / total_weight))
                .round()
                .clamp(0.0, 100.0) as u8,
        )
    };

    Some(Score {
        value,
        samples,
        success_rate: (if attempts > 0.0 {
            successes / attempts
        } else {
            0.0
        }) as f32,
        completion_rate: (if started > 0.0 {
            completed / started
        } else {
            0.0
        }) as f32,
        median_rtt_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the composition the wasm export uses — normalise a raw string,
    /// and derive an info URL only if it survived — so this file's hostile set
    /// covers it too; the export itself is pinned by broker-wasm's V10. This is
    /// a re-implementation in a different crate, so it cannot by itself stop
    /// the export from drifting away from these rules.
    fn directory_info_url_equivalent(raw: &str) -> Option<String> {
        normalize_announced_url(raw).ok().map(|url| info_url(&url))
    }

    fn valid_raw_announcement() -> RawAnnouncement {
        RawAnnouncement {
            directory_version: DIRECTORY_VERSION,
            url: "wss://play.example.com/ws".to_string(),
            name: "play.example.com".to_string(),
            mode: ServerMode::Full,
            server_version: "0.9.1".to_string(),
            protocol_version: 55,
            lobby_protocol_version: 4,
            current_players: 3,
        }
    }

    fn valid_announcement() -> ServerAnnouncement {
        validate_announcement(&valid_raw_announcement()).expect("fixture announcement is valid")
    }

    fn matching_info() -> ServerInfoDocument {
        ServerInfoDocument {
            mode: ServerMode::Full,
            protocol_version: 55,
            lobby_protocol_version: 4,
            server_version: "0.9.1".to_string(),
            build_commit: Some("abc1234".to_string()),
            public_url: Some("https://play.example.com".to_string()),
        }
    }

    /// V1. Rule 3 is not cosmetic: rule 10 rebuilds the value with an
    /// unconditional `wss://` prefix, so without this gate a plaintext `ws://`
    /// announcement would be silently *upgraded* into a `wss://` listing —
    /// strictly worse than refusing it.
    #[test]
    fn announced_url_requires_a_lowercase_wss_scheme() {
        for raw in [
            "ws://host.example/ws",
            "https://host.example/ws",
            "WSS://host.example/ws",
        ] {
            assert!(
                normalize_announced_url(raw).is_err(),
                "{raw} must be refused by rule 3"
            );
        }

        // Paired positive reach-guard: an early return that refused everything
        // would pass the loop above and fail here.
        assert!(normalize_announced_url("wss://host.example/ws").is_ok());
    }

    /// V2. Every fixture asserts `is_err()` and never the message text.
    #[test]
    fn announced_host_must_be_a_public_dns_name() {
        for raw in [
            // rule 8: a single label.
            "wss://localhost/ws",
            // rule 7: IP literals, including a link-local metadata address.
            "wss://127.0.0.1:9374/ws",
            "wss://192.168.1.5/ws",
            "wss://169.254.169.254/ws",
            // Both bracketed IPv6 siblings are asserted because they exercise
            // DIFFERENT rules, and between them they are what licensed
            // deleting a dedicated bracket guard: the un-ported form dies at
            // rule 5 (`rsplit_once(':')` leaves a non-port), the ported one
            // reaches rule 8.
            "wss://[::1]/ws",
            "wss://[::1]:443/ws",
            // rule 9: names unreachable from the public internet.
            "wss://broker.local/ws",
            "wss://foo.localhost/ws",
            "wss://metadata.internal/ws",
            "wss://1.0.0.127.in-addr.arpa/ws",
            // rule 4: userinfo is the classic host-confusion vector.
            "wss://user@real.example/ws",
        ] {
            assert!(
                normalize_announced_url(raw).is_err(),
                "{raw} must not be announceable"
            );
        }

        // Paired positive reach-guard.
        assert!(normalize_announced_url("wss://play.example.com/ws").is_ok());
    }

    /// V3. Normalisation is what makes the announced URL an identity rather
    /// than a spelling.
    #[test]
    fn announced_url_normalises_case_default_port_root_dot_and_trailing_slash() {
        let cases = [
            ("wss://Host.Example/", "wss://host.example"),
            ("wss://Host.Example/ws/", "wss://host.example/ws"),
            ("wss://host.example:443/ws", "wss://host.example/ws"),
            ("wss://host.example.:443/ws", "wss://host.example/ws"),
            // The negative: a NON-default port is not dropped.
            ("wss://host.example:8443/ws", "wss://host.example:8443/ws"),
            // Trailing-only. Interior slashes are preserved on purpose — a
            // future "tidy-up" that collapsed them would hand out an address
            // the server may not answer, and would fail here.
            ("wss://host.example//ws", "wss://host.example//ws"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                normalize_announced_url(raw)
                    .unwrap_or_else(|error| panic!("{raw} should normalise, got {error}"))
                    .as_str(),
                expected
            );
        }

        // Multi-authority hostile fixture: two spellings of one server must
        // become ONE key, or one server occupies two directory rows and a
        // client opens two sockets to it.
        assert_eq!(
            normalize_announced_url("wss://Host.Example:443/ws/").expect("spelling a"),
            normalize_announced_url("wss://host.example/ws").expect("spelling b")
        );
    }

    /// V4. Shape and size, with each fixture chosen so exactly one rule can be
    /// what refuses it.
    #[test]
    fn announced_url_shape_and_size_are_bounded() {
        // Every label is 7 bytes, so rule 8's 63-byte label cap CANNOT be what
        // refuses this; only rule 2's byte cap can.
        let long_url = format!("wss://{}com/ws", "abcdefg.".repeat(33));
        assert!(long_url.len() > MAX_SERVER_URL_LEN);
        assert!(
            long_url
                .trim_start_matches("wss://")
                .split(['.', '/'])
                .all(|label| label.len() <= 7),
            "the fixture must not be refusable by the label cap"
        );
        assert!(normalize_announced_url(&long_url).is_err());

        let label_63 = "a".repeat(63);
        let label_64 = "a".repeat(64);
        let control_in_path = "wss://host.example/w\u{0}s";

        for raw in [
            "wss:///ws",
            "wss://.example/ws",
            "wss://:443/ws",
            "wss://host_name.example/ws",
            "wss://bücher.example/ws",
            "wss://host.example:0/ws",
            "wss://host.example:99999/ws",
            "wss://host.example:-1/ws",
            // An INTERIOR control character in the PATH: rule 1's trim cannot
            // remove it and rule 8 only inspects host labels, so this reaches
            // rule 2's control-character check and nothing else.
            control_in_path,
            "wss://host example/ws",
            &format!("wss://{label_64}.example/ws"),
        ] {
            assert!(
                normalize_announced_url(raw).is_err(),
                "{raw:?} must be refused"
            );
        }

        // Paired positives, each isolating the boundary just above.
        assert!(normalize_announced_url(&format!("wss://{label_63}.example/ws")).is_ok());
        let valid_250 = format!("wss://{}com/ws", "abcdefg.".repeat(29));
        assert!(valid_250.len() <= MAX_SERVER_URL_LEN);
        assert!(normalize_announced_url(&valid_250).is_ok());
        // Rule 1 removes trailing WHITESPACE, so this is accepted; the
        // control-character fixture above is what exercises rule 2.
        assert!(normalize_announced_url("wss://host.example/ws\n").is_ok());
        assert!(normalize_announced_url("wss://host.example/ws ").is_ok());
    }

    /// V5. Field bounds and the cross-version gate.
    #[test]
    fn announcement_fields_are_bounded_and_version_gated() {
        // The version gate is checked before any field work: this announcement
        // is valid in every other respect.
        let mut wrong_version = valid_raw_announcement();
        wrong_version.directory_version = DIRECTORY_VERSION + 1;
        assert!(validate_announcement(&wrong_version).is_err());

        let mut long_name = valid_raw_announcement();
        long_name.name = "n".repeat(MAX_SERVER_NAME_LEN + 1);
        assert!(validate_announcement(&long_name).is_err());

        let mut blank_name = valid_raw_announcement();
        blank_name.name = "   ".to_string();
        assert!(validate_announcement(&blank_name).is_err());

        let mut long_version = valid_raw_announcement();
        long_version.server_version = "v".repeat(MAX_TOKEN_LEN + 1);
        assert!(validate_announcement(&long_version).is_err());

        let mut too_many = valid_raw_announcement();
        too_many.current_players = MAX_ANNOUNCED_PLAYERS + 1;
        assert!(validate_announcement(&too_many).is_err());

        // Paired positive — and it proves the function returns the CANONICAL
        // value rather than echoing its input.
        let mut spelled_oddly = valid_raw_announcement();
        spelled_oddly.url = "wss://Play.Example.com:443/ws/".to_string();
        let validated = validate_announcement(&spelled_oddly).expect("valid announcement");
        assert_eq!(validated.url().as_str(), "wss://play.example.com/ws");
        assert_eq!(validated.directory_version(), DIRECTORY_VERSION);
        assert_eq!(validated.current_players(), 3);

        // The clamping mutator keeps the bound the validator enforced.
        assert_eq!(
            validated
                .with_current_players(MAX_ANNOUNCED_PLAYERS + 5)
                .current_players(),
            MAX_ANNOUNCED_PLAYERS
        );
    }

    /// V6. Both directions, plus the documented precedence across all four
    /// compared fields and the coverage of both version numbers.
    #[test]
    fn info_document_mismatch_is_reported_per_field() {
        let announcement = valid_announcement();

        let mut other_mode = matching_info();
        other_mode.mode = ServerMode::LobbyOnly;
        assert_eq!(
            compare_announcement_to_info(&announcement, &other_mode),
            InfoMatch::Mismatch {
                field: InfoMismatchField::Mode
            }
        );

        let mut other_version = matching_info();
        other_version.server_version = "0.9.0".to_string();
        assert_eq!(
            compare_announcement_to_info(&announcement, &other_version),
            InfoMatch::Mismatch {
                field: InfoMismatchField::ServerVersion
            }
        );

        // Both fields differ: `Mode` is reported, pinning the documented
        // precedence rather than leaving it implementation-defined.
        let mut both = matching_info();
        both.mode = ServerMode::LobbyOnly;
        both.server_version = "0.9.0".to_string();
        assert_eq!(
            compare_announcement_to_info(&announcement, &both),
            InfoMatch::Mismatch {
                field: InfoMismatchField::Mode
            }
        );

        // Each version number alone. Asserted separately because the two are
        // distinct selectors: a comparison that collapsed them into one
        // `Version` field would report something plausible-looking here and
        // still be wrong about which number a client should act on.
        let mut other_protocol_only = matching_info();
        other_protocol_only.protocol_version = 54;
        assert_eq!(
            compare_announcement_to_info(&announcement, &other_protocol_only),
            InfoMatch::Mismatch {
                field: InfoMismatchField::ProtocolVersion
            }
        );

        let mut other_lobby_protocol_only = matching_info();
        other_lobby_protocol_only.lobby_protocol_version = 3;
        assert_eq!(
            compare_announcement_to_info(&announcement, &other_lobby_protocol_only),
            InfoMatch::Mismatch {
                field: InfoMismatchField::LobbyProtocolVersion
            }
        );

        // This case carries a contract forward. Phase 2 planted it asserting
        // `Match` for exactly this fixture, with the instruction that a later
        // phase widening the comparison must change the assertion VISIBLY
        // rather than the widening happening silently. Phase 3 widened it, and
        // this edit is that visible change: the same fixture now reports
        // `ProtocolVersion`, because both numbers differ and the precedence
        // reports the earlier one. The instruction still binds whatever is
        // compared next — a `build_commit`, say — which must land as an edit
        // here rather than sliding through a green suite.
        let mut other_protocol = matching_info();
        other_protocol.protocol_version = 54;
        other_protocol.lobby_protocol_version = 3;
        assert_eq!(
            compare_announcement_to_info(&announcement, &other_protocol),
            InfoMatch::Mismatch {
                field: InfoMismatchField::ProtocolVersion
            }
        );

        // Three more multi-field documents, so the precedence is pinned across
        // the whole order rather than only at its head. Together with the
        // both-`mode`-and-`server_version` case above and the case
        // immediately above, five of the six ordered pairs are asserted:
        // `mode` before `server_version`, `mode` before `protocol_version`,
        // `server_version` before `protocol_version`, `server_version` before
        // `lobby_protocol_version`, and `protocol_version` before
        // `lobby_protocol_version`. The sixth, `mode` before
        // `lobby_protocol_version`, follows from the other five by
        // transitivity, so the four `if`s admit exactly one ordering.
        let mut mode_and_protocol = matching_info();
        mode_and_protocol.mode = ServerMode::LobbyOnly;
        mode_and_protocol.protocol_version = 54;
        assert_eq!(
            compare_announcement_to_info(&announcement, &mode_and_protocol),
            InfoMatch::Mismatch {
                field: InfoMismatchField::Mode
            }
        );

        let mut version_and_lobby_protocol = matching_info();
        version_and_lobby_protocol.server_version = "0.9.0".to_string();
        version_and_lobby_protocol.lobby_protocol_version = 3;
        assert_eq!(
            compare_announcement_to_info(&announcement, &version_and_lobby_protocol),
            InfoMatch::Mismatch {
                field: InfoMismatchField::ServerVersion
            }
        );

        // The adjacent pair the other cases leave open: without it, swapping
        // the `server_version` and `protocol_version` arms passes every other
        // assertion here.
        let mut version_and_protocol = matching_info();
        version_and_protocol.server_version = "0.9.0".to_string();
        version_and_protocol.protocol_version = 54;
        assert_eq!(
            compare_announcement_to_info(&announcement, &version_and_protocol),
            InfoMatch::Mismatch {
                field: InfoMismatchField::ServerVersion
            }
        );

        // Paired positive reach-guard: a function returning `Mismatch`
        // unconditionally fails here.
        assert_eq!(
            compare_announcement_to_info(&announcement, &matching_info()),
            InfoMatch::Match
        );
    }

    /// V7. The Durable Object's document is four fields, not six.
    #[test]
    fn do_info_body_deserializes_with_absent_optional_fields() {
        // Verbatim shape of the body `lobby-worker/src/lobby-do.ts` serves
        // today (`Response.json({ mode, protocol_version,
        // lobby_protocol_version, server_version })`).
        const DO_BODY: &str = r#"{"mode":"LobbyOnly","protocol_version":55,"lobby_protocol_version":4,"server_version":"0.9.1"}"#;

        let parsed: ServerInfoDocument = serde_json::from_str(DO_BODY).expect("DO body parses");
        assert_eq!(parsed.mode, ServerMode::LobbyOnly);
        assert_eq!(parsed.protocol_version, 55);
        assert_eq!(parsed.lobby_protocol_version, 4);
        assert_eq!(parsed.server_version, "0.9.1");
        assert_eq!(parsed.build_commit, None);
        assert_eq!(parsed.public_url, None);

        // Paired negative: the four remaining fields are genuinely required,
        // so this is not a struct that accepts anything.
        const MISSING_MODE: &str =
            r#"{"protocol_version":55,"lobby_protocol_version":4,"server_version":"0.9.1"}"#;
        assert!(serde_json::from_str::<ServerInfoDocument>(MISSING_MODE).is_err());

        // Forward compatibility within one DIRECTORY_VERSION: an unknown
        // additive field must not break the parse.
        const EXTRA_FIELD: &str = r#"{"mode":"LobbyOnly","protocol_version":55,"lobby_protocol_version":4,"server_version":"0.9.1","region":"eu-west"}"#;
        assert!(serde_json::from_str::<ServerInfoDocument>(EXTRA_FIELD).is_ok());
    }

    /// V8. The hostile set is run through BOTH construction paths.
    ///
    /// The structural half of this guarantee cannot be asserted at runtime and
    /// must be read from the code: `info_url` takes `&AnnouncedUrl` (so
    /// calling it on a `&str` does not compile), `AnnouncedUrl`'s field is
    /// private, `#[serde(try_from = "String")]` is what stops the derived
    /// `Deserialize` from becoming a second unvalidating constructor, and
    /// `ServerAnnouncement` has no `Deserialize` at all. Weaken any one of
    /// those four and every assertion below still passes.
    #[test]
    fn info_url_is_reachable_only_through_the_normalised_form() {
        let hostile = [
            "wss://evil@real.example/ws",
            "wss://localhost/ws",
            "wss://127.0.0.1:9374/ws",
            "wss://[::1]/ws",
            "wss://169.254.169.254/ws",
            "wss://metadata.internal/ws",
        ];

        for raw in hostile {
            // (i) the deserialization path — the regression guard for a
            // derived `Deserialize` silently becoming a second constructor.
            let json = serde_json::to_string(raw).expect("string serializes");
            assert!(
                serde_json::from_str::<AnnouncedUrl>(&json).is_err(),
                "{raw} must not deserialize into an AnnouncedUrl"
            );
            // (ii) the raw-string path the wasm export takes.
            assert_eq!(directory_info_url_equivalent(raw), None, "{raw}");
        }

        // Paired positives on BOTH paths: without these, dropping `try_from`
        // fails the loop above while a normalise-everything stub would pass
        // it, and a function returning `None` unconditionally would too.
        assert!(serde_json::from_str::<AnnouncedUrl>(r#""wss://host.example/ws""#).is_ok());
        assert_eq!(
            directory_info_url_equivalent("wss://Host.Example:443/ws/"),
            Some(format!("https://host.example{INFO_PATH}"))
        );
        assert_eq!(
            directory_info_url_equivalent("wss://host.example:8443/ws"),
            Some(format!("https://host.example:8443{INFO_PATH}"))
        );

        // The derived URL is built from the constant, so changing INFO_PATH
        // cannot leave this function stale.
        let url = normalize_announced_url("wss://host.example/ws").expect("valid");
        assert!(info_url(&url).ends_with(INFO_PATH));
    }

    /// One hour-bucket of connect evidence, all RTTs landing in one cell.
    fn counter_bucket(
        start_ms: u64,
        attempts: u32,
        successes: u32,
        rtt_cell: usize,
        rtt_count: u32,
    ) -> CounterBucket {
        let mut rtt_histogram = [0u32; RTT_CELLS];
        rtt_histogram[rtt_cell] = rtt_count;
        CounterBucket {
            start_ms,
            connect_attempts: attempts,
            connect_successes: successes,
            games_started: 0,
            games_completed: 0,
            rtt_histogram,
            announced_players_max: 0,
        }
    }

    fn counters(buckets: Vec<CounterBucket>) -> ServerCounters {
        ServerCounters { buckets }
    }

    /// A `now` far from the epoch so `saturating_sub` is never what makes an
    /// aged fixture look fresh.
    const SCORE_NOW: u64 = 1_000 * SCORE_BUCKET_MS;

    /// V-U14a. `None` means "no live evidence" — and the aged-out fixture is
    /// what distinguishes that from "never reported", since both must be
    /// `None` while a fresh fixture must not.
    #[test]
    fn score_is_none_without_live_evidence() {
        assert_eq!(score(&counters(Vec::new()), SCORE_NOW), None);

        // Every bucket past the window: weight 0, so nothing is summed.
        let aged_out = counters(vec![counter_bucket(
            SCORE_NOW - 25 * SCORE_BUCKET_MS,
            100,
            100,
            1,
            100,
        )]);
        assert_eq!(score(&aged_out, SCORE_NOW), None);

        // Paired positive reach-guard: the same counters inside the window
        // score, so a `score` returning `None` unconditionally fails here.
        let fresh = counters(vec![counter_bucket(SCORE_NOW, 100, 100, 1, 100)]);
        assert!(score(&fresh, SCORE_NOW).is_some());
    }

    /// V-U14b. Below the minimum, the VALUE is absent but the sample count is
    /// not. This is the distinction the whole `Score` shape exists for: a
    /// consumer must be able to tell "too little evidence to rank" from "no
    /// evidence at all", and it can only do that if `samples` survives.
    #[test]
    fn score_below_the_minimum_has_no_value_but_a_visible_sample_count() {
        let thin = counters(vec![counter_bucket(SCORE_NOW, 3, 3, 1, 3)]);
        let thin_score = score(&thin, SCORE_NOW).expect("three samples are still live evidence");
        assert_eq!(thin_score.value, None);
        assert_eq!(thin_score.samples, 3);
        // The components are populated too — a perfect 3-of-3 reads as 1.0
        // even though it is not rankable.
        assert_eq!(thin_score.success_rate, 1.0);
        assert_eq!(thin_score.median_rtt_ms, Some(100));

        // Paired positive: above the minimum the value appears.
        let thick = counters(vec![counter_bucket(SCORE_NOW, 100, 100, 1, 100)]);
        let thick_score = score(&thick, SCORE_NOW).expect("live evidence");
        assert!(thick_score.value.is_some());
        assert!(thick_score.samples >= SCORE_MIN_SAMPLES);
    }

    /// V-U14c. Identical counters, older timestamps, strictly lower score.
    ///
    /// This is the row that had to be measured rather than assumed: a pure
    /// re-weighting of buckets leaves every RATE unchanged under uniform
    /// ageing, so without the smoothing prior this assertion is false for a
    /// perfectly reasonable implementation. It is the prior that makes aged
    /// evidence decay towards it.
    #[test]
    fn score_decays_as_its_evidence_ages() {
        let fresh = counters(vec![counter_bucket(SCORE_NOW, 100, 100, 1, 100)]);
        let aged = counters(vec![counter_bucket(
            SCORE_NOW - 12 * SCORE_BUCKET_MS,
            100,
            100,
            1,
            100,
        )]);

        let fresh_value = score(&fresh, SCORE_NOW)
            .and_then(|s| s.value)
            .expect("fresh evidence is rankable");
        let aged_value = score(&aged, SCORE_NOW)
            .and_then(|s| s.value)
            .expect("12h-old evidence is still rankable");

        assert!(
            aged_value < fresh_value,
            "identical counters 12h older must score strictly lower, got {aged_value} vs {fresh_value}"
        );
        // The unsmoothed component is deliberately NOT what decayed — the
        // reported evidence is the raw rate; only the ranking number moves.
        assert_eq!(score(&aged, SCORE_NOW).expect("aged").success_rate, 1.0);
    }

    /// V-U14d. Each component moves the number in the direction it should,
    /// with everything else held equal.
    ///
    /// All four fixtures share their volume and `now_ms`, so the varied axis
    /// is the only thing that can explain a difference.
    #[test]
    fn score_orders_by_success_latency_and_completion() {
        let perfect = counters(vec![counter_bucket(SCORE_NOW, 100, 100, 1, 100)]);
        let perfect_value = score(&perfect, SCORE_NOW)
            .and_then(|s| s.value)
            .expect("rankable");

        // Success: same volume, half the connects land.
        let unreliable = counters(vec![counter_bucket(SCORE_NOW, 100, 50, 1, 100)]);
        let unreliable_value = score(&unreliable, SCORE_NOW)
            .and_then(|s| s.value)
            .expect("rankable");
        assert!(
            perfect_value > unreliable_value,
            "100% success must outrank 50%, got {perfect_value} vs {unreliable_value}"
        );

        // Latency: same success, RTTs in the 1600-3200 cell instead of <=100.
        let slow = counters(vec![counter_bucket(SCORE_NOW, 100, 100, 6, 100)]);
        let slow_value = score(&slow, SCORE_NOW)
            .and_then(|s| s.value)
            .expect("rankable");
        assert!(
            perfect_value > slow_value,
            "fast RTT must outrank slow, got {perfect_value} vs {slow_value}"
        );

        // Completion: the same perfect connects, plus games that mostly did
        // not finish. The completion component only exists once games were
        // started, which is what keeps a LobbyOnly broker out of it.
        let mut abandons = counter_bucket(SCORE_NOW, 100, 100, 1, 100);
        abandons.games_started = 10;
        abandons.games_completed = 2;
        let abandoning = counters(vec![abandons]);
        let abandoning_score = score(&abandoning, SCORE_NOW).expect("rankable");
        assert!(
            abandoning_score.value.expect("rankable") < perfect_value,
            "a poor completion rate must drag the score"
        );
        assert_eq!(abandoning_score.completion_rate, 0.2);
        // The no-games case reports 0.0 completion and is NOT dragged by it —
        // paired with the line above, this is what proves re-normalisation
        // rather than a zero component being folded in.
        assert_eq!(
            score(&perfect, SCORE_NOW)
                .expect("rankable")
                .completion_rate,
            0.0
        );

        // The median cell, pinned separately: the weighted cumulative count
        // crosses half inside cell 3, so the reported median is that cell's
        // upper edge. A histogram spread across cells is the only fixture that
        // can catch an off-by-one in the crossing test.
        let mut spread = counter_bucket(SCORE_NOW, 100, 100, 0, 0);
        spread.rtt_histogram = [10, 10, 10, 40, 10, 10, 5, 5];
        assert_eq!(
            score(&counters(vec![spread]), SCORE_NOW)
                .expect("rankable")
                .median_rtt_ms,
            Some(400)
        );

        // The overflow cell has no edge of its own and reports the last one.
        let mut overflow = counter_bucket(SCORE_NOW, 100, 100, 7, 100);
        overflow.connect_successes = 100;
        assert_eq!(
            score(&counters(vec![overflow]), SCORE_NOW)
                .expect("rankable")
                .median_rtt_ms,
            Some(3200)
        );
    }
}
