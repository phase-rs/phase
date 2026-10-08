use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};
use server_core::game_log::{format_timestamp, GameFileCache, Stream};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Extension data stored on `game_session` spans: the game code (used to pick
/// the output file) and, when present, the player id — both inherited by
/// every event within the span unless the event itself restates the field
/// (`on_event` lets an event-level value win; see `SpanFieldsVisitor`'s doc
/// comment for why a row otherwise carries no seat attribution).
struct GameSessionFields {
    game: String,
    player: Option<String>,
}

/// Visitor that extracts the `game` and `player` fields from span
/// attributes. The span is created as `game = %game_code, player =
/// ?player_id` (`SocketIdentity::set_session`) — both `%` and `?` record via
/// `record_debug`, not `record_str` (tracing wraps the value in a `Debug`
/// adapter — `%`'s impl delegates to `Display`, `?`'s uses the value's own
/// `Debug` — so `record_debug` must capture both or every span silently
/// carries no game code / player id and this entire layer is a no-op).
/// Verified with a driven probe: the real macro form (`%`/`?`) reaches
/// `record_debug`; only a raw `&str` field reaches `record_str`.
#[derive(Default)]
struct SpanFieldsVisitor {
    game: Option<String>,
    player: Option<String>,
}

impl Visit for SpanFieldsVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "game" => self.game = Some(value.to_string()),
            "player" => self.player = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "game" => self.game = Some(format!("{value:?}")),
            "player" => self.player = Some(format!("{value:?}")),
            _ => {}
        }
    }
}

/// Visitor that collects every tracing event field into a JSON object, keyed
/// by field name verbatim (tracing already names the message field
/// `"message"`, so no special-casing is needed here).
struct JsonFieldVisitor(Map<String, Value>);

impl Visit for JsonFieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0
            .insert(field.name().to_string(), Value::String(value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), Value::Bool(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().to_string(), Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(
            field.name().to_string(),
            Value::String(format!("{value:?}")),
        );
    }
}

/// A tracing `Layer` that routes events occurring within a `game_session`
/// span to the `Stream::Session` per-game JSON-Lines stream. The engine's own
/// per-rules-event rows (`Stream::Events`) are written separately, directly
/// by `server-core` at the point each action result is minted — see
/// `server_core::game_log`'s module doc.
pub struct GameFileLayer {
    cache: Arc<GameFileCache>,
}

impl GameFileLayer {
    fn new(cache: Arc<GameFileCache>) -> Self {
        Self { cache }
    }
}

impl<S> Layer<S> for GameFileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if attrs.metadata().name() != "game_session" {
            return;
        }
        let mut visitor = SpanFieldsVisitor::default();
        attrs.record(&mut visitor);
        if let Some(game) = visitor.game {
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(GameSessionFields {
                    game,
                    player: visitor.player,
                });
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Walk up the span scope to find the nearest game_session span.
        let span_fields = ctx.event_span(event).and_then(|span| {
            // scope() yields the span itself first, then walks up to parents.
            for s in span.scope() {
                if let Some(gc) = s.extensions().get::<GameSessionFields>() {
                    return Some((gc.game.clone(), gc.player.clone()));
                }
            }
            None
        });

        let (game_code, player) = match span_fields {
            Some(v) => v,
            None => return, // Not inside a game_session span — skip.
        };

        let mut visitor = JsonFieldVisitor(Map::new());
        event.record(&mut visitor);
        let mut fields = visitor.0;
        // `or_insert`, not `insert`: an event field that happens to be named
        // `player`/`ts`/`level`/`target` must not be silently destroyed by
        // this transport/span metadata — the event's own data wins on
        // collision. `player` is seeded from the span (not the game code —
        // that's already the output filename, so restating it per row is
        // pure redundancy) because per-event `player = ?player_id` fields
        // are the exception, not the rule: most rows would otherwise carry
        // no seat attribution at all, which is the whole point of a
        // per-game debug log.
        if let Some(player) = player {
            fields
                .entry("player".to_string())
                .or_insert_with(|| Value::String(player));
        }
        fields
            .entry("ts".to_string())
            .or_insert_with(|| Value::String(format_timestamp()));
        fields
            .entry("level".to_string())
            .or_insert_with(|| Value::String(event.metadata().level().to_string()));
        fields
            .entry("target".to_string())
            .or_insert_with(|| Value::String(event.metadata().target().to_string()));

        if let Ok(line) = serde_json::to_string(&Value::Object(fields)) {
            self.cache.write_line(&game_code, Stream::Session, &line);
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let game_code = ctx.span(&id).and_then(|span| {
            span.extensions()
                .get::<GameSessionFields>()
                .map(|gc| gc.game.clone())
        });
        if let Some(game_code) = game_code {
            self.cache.close(&game_code);
        }
    }
}

/// Initialize the tracing subscriber.
///
/// When `log_dir` is `Some`, logs are written to files:
/// - Main log: `<dir>/phase-server.log` (daily rolling; JSON-formatted iff `json`)
/// - Per-game logs: `<dir>/games/<GAME_CODE>.session.jsonl` (this file's
///   `GameFileLayer`) and `<dir>/games/<GAME_CODE>.events.jsonl` (the
///   engine's own `GameLogEntry` rows, written by `server-core` — see
///   `server_core::game_log`) — always JSON-Lines, independent of `json`.
///   The per-game format is not optional: it exists so a game's logs are
///   parseable, and gating that behind a flag would leave the default output
///   exactly as unparseable as before this existed.
///
/// When `log_dir` is `None`, logs are written to stdout (local dev mode) and
/// the returned [`GameFileCache`] is disabled (every write a no-op).
///
/// Returns a `WorkerGuard` that must be held alive for the program's lifetime
/// to ensure buffered logs are flushed. Use a **named binding** (`let _guard = ...`),
/// NOT bare `_` which drops immediately.
///
/// The returned [`GameFileCache`] must be installed on the live
/// `SessionManager` (`SessionManager.game_log`) — that's what makes the
/// `events` stream live; this function only wires up the `session` stream.
pub fn init_logging(
    log_dir: Option<&str>,
    json: bool,
) -> (
    Option<tracing_appender::non_blocking::WorkerGuard>,
    Arc<GameFileCache>,
) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        "phase_server=info,server_core=info,phase_ai=info"
            .parse()
            .unwrap()
    });

    match log_dir {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            let games_dir = dir.join("games");
            fs::create_dir_all(&games_dir).expect("failed to create log directory");

            let file_appender = tracing_appender::rolling::daily(&dir, "phase-server.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

            let game_cache = Arc::new(GameFileCache::new(games_dir));
            let game_layer = GameFileLayer::new(Arc::clone(&game_cache));

            if json {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .json()
                            .with_writer(non_blocking)
                            .with_target(true),
                    )
                    .with(game_layer)
                    .init();
            } else {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(
                        tracing_subscriber::fmt::layer()
                            .with_writer(non_blocking)
                            .with_ansi(false),
                    )
                    .with(game_layer)
                    .init();
            }

            (Some(guard), game_cache)
        }
        None => {
            // Stdout mode — preserves current behavior for local dev.
            if json {
                tracing_subscriber::fmt()
                    .json()
                    .with_env_filter(env_filter)
                    .with_target(true)
                    .init();
            } else {
                tracing_subscriber::fmt().with_env_filter(env_filter).init();
            }
            (None, Arc::new(GameFileCache::disabled()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// End-to-end pin for the HIGH bug the second review round caught: a
    /// `game_session` span created as `game = %code` (the real macro form
    /// `SocketIdentity::set_session` uses) dispatches through `record_debug`,
    /// not `record_str`. Before that visitor arm existed, `on_new_span` never
    /// captured a `GameSessionFields` extension, so `on_event`'s span walk always hit
    /// `None => return` and no session-stream file was ever created — this
    /// test drives the real span/event macros through a real `GameFileLayer`
    /// and would fail exactly that way on the old code.
    #[test]
    fn game_session_span_with_display_recorded_code_writes_session_stream_file() {
        let dir = tempfile::tempdir().unwrap();
        let games_dir = dir.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();

        let cache = Arc::new(GameFileCache::new(games_dir.clone()));
        let layer = GameFileLayer::new(Arc::clone(&cache));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("game_session", game = %"CODE123");
            let _enter = span.enter();
            tracing::info!(extra = "value", "test event");
        });

        let path = games_dir.join("CODE123.session.jsonl");
        let content = fs::read_to_string(&path)
            .expect("session stream file must exist — proves the %-recorded `game` field reached record_debug");
        let row: Value =
            serde_json::from_str(content.lines().next().unwrap()).expect("row must be valid JSON");
        assert_eq!(row["extra"], "value");
        assert_eq!(row["message"], "test event");
        assert_eq!(row["level"], "INFO");
        assert!(row["ts"].is_string());
    }

    /// Pins the LOW clobbering fix: an event field literally named `level`
    /// must survive — `or_insert` only synthesizes the metadata key when the
    /// event didn't already supply one.
    #[test]
    fn user_supplied_field_wins_over_synthesized_metadata_key() {
        let dir = tempfile::tempdir().unwrap();
        let games_dir = dir.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();

        let cache = Arc::new(GameFileCache::new(games_dir.clone()));
        let layer = GameFileLayer::new(Arc::clone(&cache));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("game_session", game = %"CODE456");
            let _enter = span.enter();
            tracing::info!(level = "custom-level-value", "test event");
        });

        let path = games_dir.join("CODE456.session.jsonl");
        let content = fs::read_to_string(&path).expect("session stream file must exist");
        let row: Value =
            serde_json::from_str(content.lines().next().unwrap()).expect("row must be valid JSON");
        assert_eq!(
            row["level"], "custom-level-value",
            "a real event field named `level` must not be clobbered by the synthesized one"
        );
    }

    /// CodeRabbit finding on PR #8245: the span's `player` field (recorded
    /// via `?`, same `record_debug` dispatch as `game`) never reached the
    /// JSON row unless the event itself restated it — most rows carried no
    /// seat attribution at all. This event omits `player` entirely, so the
    /// row must inherit it from the span.
    #[test]
    fn player_field_inherited_from_span_when_event_omits_it() {
        let dir = tempfile::tempdir().unwrap();
        let games_dir = dir.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();

        let cache = Arc::new(GameFileCache::new(games_dir.clone()));
        let layer = GameFileLayer::new(Arc::clone(&cache));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("game_session", game = %"CODE789", player = ?7u8);
            let _enter = span.enter();
            tracing::info!("test event without an explicit player field");
        });

        let path = games_dir.join("CODE789.session.jsonl");
        let content = fs::read_to_string(&path).expect("session stream file must exist");
        let row: Value =
            serde_json::from_str(content.lines().next().unwrap()).expect("row must be valid JSON");
        assert_eq!(
            row["player"], "7",
            "row must inherit the span's player field when the event doesn't restate it"
        );
    }

    /// An event that DOES restate `player` must win over the span-inherited
    /// value — span inheritance is a fallback, not an override.
    #[test]
    fn event_supplied_player_field_wins_over_span_inherited_value() {
        let dir = tempfile::tempdir().unwrap();
        let games_dir = dir.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();

        let cache = Arc::new(GameFileCache::new(games_dir.clone()));
        let layer = GameFileLayer::new(Arc::clone(&cache));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("game_session", game = %"CODE790", player = ?7u8);
            let _enter = span.enter();
            tracing::info!(player = "explicit-override", "test event");
        });

        let path = games_dir.join("CODE790.session.jsonl");
        let content = fs::read_to_string(&path).expect("session stream file must exist");
        let row: Value =
            serde_json::from_str(content.lines().next().unwrap()).expect("row must be valid JSON");
        assert_eq!(row["player"], "explicit-override");
    }
}
