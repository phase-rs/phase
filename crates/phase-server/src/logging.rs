use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Extension data stored on `game_session` spans to identify the game code.
struct GameCode(String);

/// Visitor that extracts the `game` field from span attributes.
struct GameCodeVisitor(Option<String>);

impl Visit for GameCodeVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "game" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

/// Visitor that formats all event fields as `key=value` pairs.
struct FieldFormatter(String);

impl Visit for FieldFormatter {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={}", field.name(), value));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.push_str(&format!("{:?}", value));
        } else {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={:?}", field.name(), value));
        }
    }
}

/// Format a UTC timestamp as `YYYY-MM-DDTHH:MM:SS.mmmZ` without external crates.
fn format_timestamp() -> String {
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();

    // Convert epoch seconds to date/time components.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Civil date from day count (algorithm from Howard Hinnant).
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, hours, minutes, seconds, millis
    )
}

/// A tracing `Layer` that routes events occurring within a `game_session` span
/// to per-game log files in the `games/` subdirectory.
///
/// File handles are lazily opened on first write and cleaned up when the
/// associated `game_session` span closes.
pub struct GameFileLayer {
    games_dir: PathBuf,
    /// `None` value = open was attempted and failed (sentinel to avoid retry storms).
    files: Mutex<HashMap<String, Option<BufWriter<File>>>>,
}

impl GameFileLayer {
    pub fn new(games_dir: PathBuf) -> Self {
        Self {
            games_dir,
            files: Mutex::new(HashMap::new()),
        }
    }

    fn open_file(&self, game_code: &str) -> Option<BufWriter<File>> {
        let path = self.games_dir.join(format!("{}.log", game_code));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(BufWriter::new)
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
        let mut visitor = GameCodeVisitor(None);
        attrs.record(&mut visitor);
        if let Some(game_code) = visitor.0 {
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(GameCode(game_code));
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Walk up the span scope to find the nearest game_session span.
        let game_code = ctx.event_span(event).and_then(|span| {
            // scope() yields the span itself first, then walks up to parents.
            for s in span.scope() {
                if let Some(gc) = s.extensions().get::<GameCode>() {
                    return Some(gc.0.clone());
                }
            }
            None
        });

        let game_code = match game_code {
            Some(gc) => gc,
            None => return, // Not inside a game_session span — skip.
        };

        // Format the event as a human-readable log line.
        let now = format_timestamp();
        let level = event.metadata().level();
        let mut formatter = FieldFormatter(String::new());
        event.record(&mut formatter);

        let line = format!("{} {:>5} {}\n", now, level, formatter.0);

        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let entry = files
            .entry(game_code.clone())
            .or_insert_with(|| self.open_file(&game_code));
        if let Some(writer) = entry {
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.flush();
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let game_code = ctx
            .span(&id)
            .and_then(|span| span.extensions().get::<GameCode>().map(|gc| gc.0.clone()));
        if let Some(game_code) = game_code {
            let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
            // Flush and remove — reopened lazily if another connection writes to the same game.
            if let Some(Some(mut writer)) = files.remove(&game_code) {
                let _ = writer.flush();
            }
        }
    }
}

/// Initialize the tracing subscriber.
///
/// When `log_dir` is `Some`, logs are written to files:
/// - Main log: `<dir>/phase-server.log` (daily rolling)
/// - Per-game logs: `<dir>/games/<GAME_CODE>.log` (human-readable)
///
/// When `log_dir` is `None`, logs are written to stdout (local dev mode).
///
/// The `json` flag controls the format of the main log. Per-game files are
/// always human-readable regardless of this flag.
///
/// Returns a `WorkerGuard` that must be held alive for the program's lifetime
/// to ensure buffered logs are flushed. Use a **named binding** (`let _guard = ...`),
/// NOT bare `_` which drops immediately.
pub fn init_logging(
    log_dir: Option<&str>,
    json: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
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

            let game_layer = GameFileLayer::new(games_dir);

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

            Some(guard)
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
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_is_valid_iso8601() {
        let ts = format_timestamp();
        // Expect: YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(ts.len(), 24);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert_eq!(&ts[19..20], ".");
    }

    #[test]
    fn game_file_layer_creates_log_files() {
        let tmp = tempfile::tempdir().unwrap();
        let games_dir = tmp.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();

        let layer = GameFileLayer::new(games_dir.clone());

        // Simulate opening a file for a game.
        let writer = layer.open_file("TEST01");
        assert!(writer.is_some());

        let log_path = games_dir.join("TEST01.log");
        assert!(log_path.exists());
    }

    #[test]
    fn game_file_layer_appends_to_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let games_dir = tmp.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();

        let layer = GameFileLayer::new(games_dir.clone());
        let log_path = games_dir.join("APPEND.log");

        // First open + write.
        {
            let mut writer = layer.open_file("APPEND").unwrap();
            writer.write_all(b"line 1\n").unwrap();
            writer.flush().unwrap();
        }

        // Second open + write (simulates re-open after on_close).
        {
            let mut writer = layer.open_file("APPEND").unwrap();
            writer.write_all(b"line 2\n").unwrap();
            writer.flush().unwrap();
        }

        let content = fs::read_to_string(&log_path).unwrap();
        assert_eq!(content, "line 1\nline 2\n");
    }

    #[test]
    fn game_file_layer_sentinel_on_bad_path() {
        // A GameFileLayer with a non-existent directory should return None.
        let layer = GameFileLayer::new(PathBuf::from("/nonexistent/path/games"));
        let writer = layer.open_file("FAIL01");
        assert!(writer.is_none());
    }
}
