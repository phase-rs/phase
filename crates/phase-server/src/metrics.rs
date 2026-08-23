//! Prometheus text exposition for the counters an autoscaler needs.
//!
//! Served on its own listener (`--metrics-port`), never on the public port: the
//! numbers here describe capacity and occupancy, which is operator information
//! rather than something a player should be able to read through the ingress.
//!
//! Hand-rolled rather than pulled from a client crate. The exposition format is
//! a handful of lines, the process has exactly one registry, and every value is
//! read live from state the server already holds — a registry abstraction would
//! only add a second place for a counter to drift out of sync.
//!
//! The occupancy gauges (`phase_games_with_connected_humans`,
//! `phase_drafts_with_connected_humans`) exist for one decision: a StatefulSet
//! always removes its highest ordinal, so an autoscaler must be able to ask
//! "does *this* replica still hold someone?" before it is allowed to shrink.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::response::IntoResponse;

use crate::{AppState, ServerMode};

/// Prometheus' text exposition content type (the format this module writes).
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Why the server refused a connection or a game at admission.
///
/// A typed reason rather than a `&str` label so the exposition and the
/// increment sites cannot disagree about the spelling of a label value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// `ws_handler` refused the upgrade: `phase_connections` was at capacity.
    ConnectionLimit,
    /// A `CreateGame*` was refused: the session map was at capacity.
    GameLimit,
    /// `ws_handler` refused the upgrade: the `Origin` header was not allowed.
    OriginNotAllowed,
}

impl RejectReason {
    /// Every variant, in exposition order. Exhaustively matched in
    /// [`ServerMetrics::reject_count`], so a new variant fails to compile until
    /// it is counted and listed here.
    const ALL: [Self; 3] = [
        Self::ConnectionLimit,
        Self::GameLimit,
        Self::OriginNotAllowed,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::ConnectionLimit => "connection_limit",
            Self::GameLimit => "game_limit",
            Self::OriginNotAllowed => "origin_not_allowed",
        }
    }
}

/// Process-wide counters that cannot be recovered by reading live state.
///
/// Everything else in [`Snapshot`] is sampled from the session/connection maps
/// at scrape time; rejections leave no trace there, so they are counted here.
#[derive(Debug, Default)]
pub struct ServerMetrics {
    connection_limit: AtomicU64,
    game_limit: AtomicU64,
    origin_not_allowed: AtomicU64,
}

impl ServerMetrics {
    fn counter(&self, reason: RejectReason) -> &AtomicU64 {
        match reason {
            RejectReason::ConnectionLimit => &self.connection_limit,
            RejectReason::GameLimit => &self.game_limit,
            RejectReason::OriginNotAllowed => &self.origin_not_allowed,
        }
    }

    pub fn record_reject(&self, reason: RejectReason) {
        self.counter(reason).fetch_add(1, Ordering::Relaxed);
    }

    pub fn reject_count(&self, reason: RejectReason) -> u64 {
        self.counter(reason).load(Ordering::Relaxed)
    }
}

/// Build identity, mirroring what `ServerHello` advertises to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub mode: &'static str,
}

impl BuildInfo {
    pub fn current(mode: ServerMode) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: server_core::protocol::build_commit().to_string(),
            mode: match mode {
                ServerMode::Full => "full",
                ServerMode::LobbyOnly => "lobby_only",
            },
        }
    }
}

/// One scrape's worth of values. Separated from both collection and rendering
/// so the exposition can be asserted byte-for-byte against known numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub connections: u32,
    pub connections_capacity: u32,
    pub games_active: usize,
    pub games_with_connected_humans: usize,
    pub games_capacity: usize,
    pub drafts_active: usize,
    pub drafts_with_connected_humans: usize,
    /// This replica's ordinal within its StatefulSet, when the operator set
    /// one. `None` omits the gauge entirely — a single-process deployment has
    /// no ordinal, and emitting a placeholder would make "ordinal 0" ambiguous.
    pub replica_ordinal: Option<u32>,
    pub rejects: Vec<(RejectReason, u64)>,
    pub build: BuildInfo,
}

/// Sample every gauge from live server state.
///
/// Each mutex is taken and released on its own. The scrape must not hold two at
/// once: the game loop takes `sessions` and `connections` together in places,
/// and a scrape that acquired them in the other order would be a lock-ordering
/// edge introduced purely by observability.
pub async fn collect(app: &AppState) -> Snapshot {
    let (games_active, game_codes) = {
        let mgr = app.sessions.lock().await;
        let codes: HashSet<String> = mgr.sessions.keys().cloned().collect();
        (mgr.sessions.len(), codes)
    };

    let (drafts_active, draft_codes, drafts_with_seated_humans) = {
        let mgr = app.draft_sessions.lock().await;
        let codes: HashSet<String> = mgr.sessions.keys().cloned().collect();
        let seated: HashSet<String> = mgr
            .sessions
            .iter()
            .filter(|(_, session)| session.connected.iter().any(|connected| *connected))
            .map(|(code, _)| code.clone())
            .collect();
        (mgr.sessions.len(), codes, seated)
    };

    // A disconnect does not remove the player's sender from `connections` (the
    // socket may reconnect into the same slot), so presence of a key proves
    // nothing. The socket task owns the receiver, so `is_closed()` flipping is
    // what actually tracks "someone is on the other end" — the same test the
    // spectator pruning helpers use.
    let games_with_live_players = live_keys(&*app.connections.lock().await, |players| {
        players.values().any(|tx| !tx.is_closed())
    });
    let games_with_live_spectators = live_keys(&*app.game_spectators.lock().await, |senders| {
        senders.iter().any(|tx| !tx.is_closed())
    });
    let drafts_with_live_spectators = live_keys(&*app.draft_spectators.lock().await, |watchers| {
        watchers.iter().any(|(_, tx)| !tx.is_closed())
    });

    // Intersect with the live session maps: `connections` and the spectator
    // maps can still hold an entry for a game that has already been retired.
    let games_with_connected_humans = game_codes
        .iter()
        .filter(|code| {
            games_with_live_players.contains(*code) || games_with_live_spectators.contains(*code)
        })
        .count();
    let drafts_with_connected_humans = draft_codes
        .iter()
        .filter(|code| {
            drafts_with_seated_humans.contains(*code) || drafts_with_live_spectators.contains(*code)
        })
        .count();

    Snapshot {
        connections: app.player_count.load(Ordering::Relaxed),
        connections_capacity: app.context.limits.max_connections,
        games_active,
        games_with_connected_humans,
        games_capacity: app.context.limits.max_games,
        drafts_active,
        drafts_with_connected_humans,
        replica_ordinal: app.context.replica_ordinal,
        rejects: RejectReason::ALL
            .iter()
            .map(|&reason| (reason, app.context.metrics.reject_count(reason)))
            .collect(),
        build: BuildInfo::current(app.mode),
    }
}

/// Keys of `map` whose value holds at least one still-open sender.
fn live_keys<V>(
    map: &std::collections::HashMap<String, V>,
    any_open: impl Fn(&V) -> bool,
) -> HashSet<String> {
    map.iter()
        .filter(|(_, value)| any_open(value))
        .map(|(key, _)| key.clone())
        .collect()
}

/// Escape a Prometheus label value: backslash, double quote and newline.
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

fn gauge(out: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

/// Render a snapshot as Prometheus text exposition.
pub fn render(snapshot: &Snapshot) -> String {
    let mut out = String::new();

    gauge(
        &mut out,
        "phase_connections",
        "Currently open WebSocket connections.",
        snapshot.connections,
    );
    gauge(
        &mut out,
        "phase_connections_capacity",
        "Connections this process admits before refusing upgrades.",
        snapshot.connections_capacity,
    );
    gauge(
        &mut out,
        "phase_games_active",
        "Game sessions held by this process.",
        snapshot.games_active,
    );
    gauge(
        &mut out,
        "phase_games_with_connected_humans",
        "Game sessions with at least one live player or spectator socket.",
        snapshot.games_with_connected_humans,
    );
    gauge(
        &mut out,
        "phase_games_capacity",
        "Game sessions this process admits before refusing CreateGame.",
        snapshot.games_capacity,
    );
    gauge(
        &mut out,
        "phase_drafts_active",
        "Draft sessions held by this process.",
        snapshot.drafts_active,
    );
    gauge(
        &mut out,
        "phase_drafts_with_connected_humans",
        "Draft sessions with at least one connected seat or live spectator socket.",
        snapshot.drafts_with_connected_humans,
    );
    if let Some(ordinal) = snapshot.replica_ordinal {
        gauge(
            &mut out,
            "phase_replica_ordinal",
            "Ordinal of this replica within its StatefulSet.",
            ordinal,
        );
    }

    let _ = writeln!(
        &mut out,
        "# HELP phase_admission_rejects_total Connections and games refused at admission."
    );
    let _ = writeln!(&mut out, "# TYPE phase_admission_rejects_total counter");
    for (reason, count) in &snapshot.rejects {
        let _ = writeln!(
            &mut out,
            "phase_admission_rejects_total{{reason=\"{}\"}} {count}",
            reason.label()
        );
    }

    let _ = writeln!(
        &mut out,
        "# HELP phase_build_info Build identity of this process; the value is always 1."
    );
    let _ = writeln!(&mut out, "# TYPE phase_build_info gauge");
    let _ = writeln!(
        &mut out,
        "phase_build_info{{version=\"{}\",commit=\"{}\",mode=\"{}\"}} 1",
        escape_label(&snapshot.build.version),
        escape_label(&snapshot.build.commit),
        escape_label(snapshot.build.mode),
    );

    out
}

/// `GET /metrics` on the metrics listener.
pub async fn handler(State(app): State<AppState>) -> impl IntoResponse {
    let body = render(&collect(&app).await);
    ([(http::header::CONTENT_TYPE, CONTENT_TYPE)], body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            connections: 7,
            connections_capacity: 200,
            games_active: 4,
            games_with_connected_humans: 2,
            games_capacity: 100,
            drafts_active: 1,
            drafts_with_connected_humans: 1,
            replica_ordinal: Some(3),
            rejects: vec![
                (RejectReason::ConnectionLimit, 5),
                (RejectReason::GameLimit, 0),
                (RejectReason::OriginNotAllowed, 2),
            ],
            build: BuildInfo {
                version: "1.2.3".to_string(),
                commit: "abc123".to_string(),
                mode: "full",
            },
        }
    }

    /// The exposition is a wire contract a scraper parses, so assert the whole
    /// body rather than probing for substrings: a renamed metric, a dropped
    /// TYPE line or a value read from the wrong field all fail here.
    #[test]
    fn render_emits_every_family_with_its_values() {
        let expected = concat!(
            "# HELP phase_connections Currently open WebSocket connections.\n",
            "# TYPE phase_connections gauge\n",
            "phase_connections 7\n",
            "# HELP phase_connections_capacity Connections this process admits before refusing upgrades.\n",
            "# TYPE phase_connections_capacity gauge\n",
            "phase_connections_capacity 200\n",
            "# HELP phase_games_active Game sessions held by this process.\n",
            "# TYPE phase_games_active gauge\n",
            "phase_games_active 4\n",
            "# HELP phase_games_with_connected_humans Game sessions with at least one live player or spectator socket.\n",
            "# TYPE phase_games_with_connected_humans gauge\n",
            "phase_games_with_connected_humans 2\n",
            "# HELP phase_games_capacity Game sessions this process admits before refusing CreateGame.\n",
            "# TYPE phase_games_capacity gauge\n",
            "phase_games_capacity 100\n",
            "# HELP phase_drafts_active Draft sessions held by this process.\n",
            "# TYPE phase_drafts_active gauge\n",
            "phase_drafts_active 1\n",
            "# HELP phase_drafts_with_connected_humans Draft sessions with at least one connected seat or live spectator socket.\n",
            "# TYPE phase_drafts_with_connected_humans gauge\n",
            "phase_drafts_with_connected_humans 1\n",
            "# HELP phase_replica_ordinal Ordinal of this replica within its StatefulSet.\n",
            "# TYPE phase_replica_ordinal gauge\n",
            "phase_replica_ordinal 3\n",
            "# HELP phase_admission_rejects_total Connections and games refused at admission.\n",
            "# TYPE phase_admission_rejects_total counter\n",
            "phase_admission_rejects_total{reason=\"connection_limit\"} 5\n",
            "phase_admission_rejects_total{reason=\"game_limit\"} 0\n",
            "phase_admission_rejects_total{reason=\"origin_not_allowed\"} 2\n",
            "# HELP phase_build_info Build identity of this process; the value is always 1.\n",
            "# TYPE phase_build_info gauge\n",
            "phase_build_info{version=\"1.2.3\",commit=\"abc123\",mode=\"full\"} 1\n",
        );

        assert_eq!(render(&snapshot()), expected);
    }

    /// `None` must drop the family entirely. Emitting `phase_replica_ordinal 0`
    /// for an un-ordinalled process would be indistinguishable from ordinal 0,
    /// and the scale-in floor is computed from exactly that number.
    #[test]
    fn replica_ordinal_family_is_absent_when_the_process_has_no_ordinal() {
        let with_ordinal = render(&snapshot());
        let without = render(&Snapshot {
            replica_ordinal: None,
            ..snapshot()
        });

        assert!(with_ordinal.contains("phase_replica_ordinal 3\n"));
        assert!(!without.contains("phase_replica_ordinal"));
        // Nothing else moved: the two renders differ by exactly the three lines.
        assert_eq!(
            with_ordinal.lines().count() - without.lines().count(),
            3,
            "removing the ordinal must drop only its HELP/TYPE/value lines"
        );
    }

    /// A commit label is operator-supplied build metadata; unescaped `"` would
    /// terminate the label value and corrupt every following series in the
    /// scrape, so the escaping is load-bearing rather than cosmetic.
    #[test]
    fn label_values_escape_backslash_quote_and_newline() {
        assert_eq!(escape_label(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_label(r"a\b"), r"a\\b");
        assert_eq!(escape_label("a\nb"), r"a\nb");
        assert_eq!(escape_label("plain-0.1.2"), "plain-0.1.2");

        let rendered = render(&Snapshot {
            build: BuildInfo {
                version: "1.0".to_string(),
                commit: "we\"ird\\ni".to_string(),
                mode: "full",
            },
            ..snapshot()
        });
        assert!(
            rendered
                .contains(r#"phase_build_info{version="1.0",commit="we\"ird\\ni",mode="full"} 1"#),
            "escaped label missing from:\n{rendered}"
        );
    }

    #[test]
    fn every_reject_reason_gets_its_own_line_in_the_render() {
        // `label` and `counter` match exhaustively, so a new variant breaks
        // their compilation — but nothing forces it into `ALL`, and a variant
        // missing from `ALL` is simply absent from /metrics. This match is that
        // reminder: adding a variant stops compiling here.
        match RejectReason::ConnectionLimit {
            RejectReason::ConnectionLimit
            | RejectReason::GameLimit
            | RejectReason::OriginNotAllowed => {}
        }
        assert_eq!(RejectReason::ALL.len(), 3);

        let body = render(&Snapshot {
            rejects: RejectReason::ALL
                .iter()
                .map(|reason| (*reason, 0))
                .collect(),
            ..snapshot()
        });

        for reason in RejectReason::ALL {
            let line = format!(
                "phase_admission_rejects_total{{reason=\"{}\"}} 0",
                reason.label()
            );
            assert_eq!(
                body.matches(&line).count(),
                1,
                "{} should appear exactly once in:\n{body}",
                reason.label()
            );
        }
    }

    #[test]
    fn recorded_rejections_are_counted_per_reason() {
        let metrics = ServerMetrics::default();
        metrics.record_reject(RejectReason::ConnectionLimit);
        metrics.record_reject(RejectReason::ConnectionLimit);
        metrics.record_reject(RejectReason::OriginNotAllowed);

        assert_eq!(metrics.reject_count(RejectReason::ConnectionLimit), 2);
        assert_eq!(metrics.reject_count(RejectReason::OriginNotAllowed), 1);
        // Untouched reasons must not move: one shared counter behind all three
        // labels would still pass the two assertions above.
        assert_eq!(metrics.reject_count(RejectReason::GameLimit), 0);
    }
}
