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
//! arrive as arguments (see [`DirectoryEntry::from_announcement`]) rather than
//! being read from a clock, mirroring the [`crate::env::BrokerEnv`] injection
//! pattern.
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

/// A directory row: the stored projection of an announcement plus the
/// directory's own bookkeeping.
///
/// It *does* derive `Deserialize`, because rows are read back out of storage.
/// Its `url` re-validates on every row read (via [`AnnouncedUrl`]'s
/// `try_from`), which is the field the reachability and identity arguments
/// turn on; its label fields do not. That is the correct, weaker guarantee: a
/// row's `name` was validated when the announcement that wrote it was
/// accepted, and this type is a storage projection, not a validation
/// authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub url: AnnouncedUrl,
    pub name: String,
    pub mode: ServerMode,
    pub server_version: String,
    pub protocol_version: u32,
    pub lobby_protocol_version: u32,
    pub current_players: u32,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Health score, 0–100. `None` is "not yet measured", never `0`.
    pub score: Option<u8>,
}

impl DirectoryEntry {
    /// Project a validated announcement into a storage row.
    ///
    /// Timestamps are arguments, not clock reads, so this module stays
    /// WASM-safe and clock-free.
    ///
    /// Reachability note for the phase that adds the row upsert: **no wasm
    /// export reaches this today.** It takes a `&ServerAnnouncement`, which
    /// the Worker cannot construct (that type has no `Deserialize`), and
    /// nothing here wraps it in an export. That is not a foreclosure — the
    /// upsert phase owns both this file and the boundary crate — but it is an
    /// explicit choice to make: either add an export taking the raw body and
    /// routing it through [`validate_announcement`] (the same pattern the
    /// comparison export uses), or shape the row in TypeScript from the
    /// validation DTO's `announcement` payload.
    pub fn from_announcement(
        announcement: &ServerAnnouncement,
        first_seen_ms: u64,
        last_seen_ms: u64,
    ) -> Self {
        DirectoryEntry {
            url: announcement.url.clone(),
            name: announcement.name.clone(),
            mode: announcement.mode,
            server_version: announcement.server_version.clone(),
            protocol_version: announcement.protocol_version,
            lobby_protocol_version: announcement.lobby_protocol_version,
            current_players: announcement.current_players,
            first_seen_ms,
            last_seen_ms,
            score: None,
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InfoMismatchField {
    Mode,
    ServerVersion,
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

    // Rule 8: label rules. Requires at least two labels, which is what refuses
    // a bare `localhost`, an empty authority, and a ported bracketed IPv6
    // authority. Non-ASCII is refused so an IDN is announced as its punycode
    // A-label and one name has one spelling.
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
/// `mode` is compared first and `server_version` second; the precedence is
/// part of the contract, so a document differing in both reports
/// [`InfoMismatchField::Mode`].
///
/// **Neither protocol version is compared.** An announcement's
/// `protocol_version` and `lobby_protocol_version` are therefore the
/// announcer's *unverified word*, even though a client's "behind by N"
/// affordance consumes exactly those numbers. Do not read a `Match` as
/// evidence that the announced versions are real.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The composition the wasm boundary export uses: normalise a raw string,
    /// and derive an info URL only if it survived. Written here so the two
    /// entry points cannot drift.
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

    /// V6. Both directions, plus the documented precedence and the documented
    /// NON-coverage of the two protocol versions.
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

        // Pins the documented non-coverage: the protocol versions are NOT
        // verified, so a document disagreeing about them still matches. If a
        // later phase widens the comparison, this assertion must change
        // visibly rather than the widening happening silently.
        let mut other_protocol = matching_info();
        other_protocol.protocol_version = 54;
        other_protocol.lobby_protocol_version = 3;
        assert_eq!(
            compare_announcement_to_info(&announcement, &other_protocol),
            InfoMatch::Match
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

    /// V9. The row mirrors the announcement, carries injected timestamps, and
    /// starts unscored.
    #[test]
    fn directory_entry_carries_the_announcement_and_no_score() {
        let announcement = valid_announcement();
        // Distinct values, asserted separately, so a copy-paste assigning one
        // to both fields fails.
        let entry = DirectoryEntry::from_announcement(&announcement, 1_000, 2_000);

        assert_eq!(&entry.url, announcement.url());
        assert_eq!(entry.name, announcement.name());
        assert_eq!(entry.mode, announcement.mode());
        assert_eq!(entry.server_version, "0.9.1");
        assert_eq!(entry.protocol_version, 55);
        assert_eq!(entry.lobby_protocol_version, 4);
        assert_eq!(entry.current_players, announcement.current_players());
        assert_eq!(entry.first_seen_ms, 1_000);
        assert_eq!(entry.last_seen_ms, 2_000);
        assert_eq!(
            entry.score, None,
            "a fresh row is unscored, not zero-scored"
        );
    }
}
