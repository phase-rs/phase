//! Per-game JSON-Lines debug log sink (GitHub issue #7978).
//!
//! Shared by `server-core` (writes the engine's own [`GameLogEntry`] rows at
//! the point each action result is minted — `SessionManager::handle_action`,
//! `SessionManager::handle_interaction_with_rejection`,
//! `GameSession::run_ai_action_batch` — so every path that produces a
//! transition is covered by construction, not by remembering to call a hook
//! at each of `phase-server`'s call sites) and `phase-server` (writes
//! transport/session-lifecycle tracing events to a separate stream).
//! Configured via `PHASE_LOG_DIR`; [`GameFileCache::default`] is a disabled
//! no-op sink.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use engine::types::log::GameLogEntry;
use serde_json::Value;

/// Which per-game JSON-Lines file a row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    /// Transport/session-lifecycle tracing events (connects, actions
    /// dispatched, game-over, errors) — `phase-server`'s `game_session` span.
    Session,
    /// The engine's own [`GameLogEntry`] rows, one per rules-level event.
    Events,
}

impl Stream {
    fn as_str(self) -> &'static str {
        match self {
            Stream::Session => "session",
            Stream::Events => "events",
        }
    }
}

/// Format a UTC timestamp as `YYYY-MM-DDTHH:MM:SS.mmmZ` without external crates.
pub fn format_timestamp() -> String {
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

type FileMap = HashMap<(String, Stream), Option<BufWriter<File>>>;

/// Shared per-game JSON-Lines file cache. Each `(game_code, stream)` pair
/// maps to a lazily-opened, append-only file, flushed after every write so a
/// crash never loses more than the write in flight. `games_dir: None` means
/// logging is disabled (stdout-only run) and every write is a no-op.
pub struct GameFileCache {
    games_dir: Option<PathBuf>,
    /// `None` value = open was attempted and failed (sentinel to avoid retry storms).
    files: Mutex<FileMap>,
}

impl GameFileCache {
    pub fn new(games_dir: PathBuf) -> Self {
        Self {
            games_dir: Some(games_dir),
            files: Mutex::new(HashMap::new()),
        }
    }

    pub fn disabled() -> Self {
        Self {
            games_dir: None,
            files: Mutex::new(HashMap::new()),
        }
    }

    fn open_file(&self, game_code: &str, stream: Stream) -> Option<BufWriter<File>> {
        let dir = self.games_dir.as_ref()?;
        let path = dir.join(format!("{game_code}.{}.jsonl", stream.as_str()));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(BufWriter::new)
    }

    pub fn write_line(&self, game_code: &str, stream: Stream, line: &str) {
        if self.games_dir.is_none() {
            return;
        }
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let entry = files
            .entry((game_code.to_string(), stream))
            .or_insert_with(|| self.open_file(game_code, stream));
        if let Some(writer) = entry {
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.write_all(b"\n");
            let _ = writer.flush();
        }
    }

    /// Write one JSON-Lines row per [`GameLogEntry`], stamped with the
    /// wall-clock moment of writing — the entries themselves carry only
    /// game-time (`turn`/`phase`); `seq` is left at whatever the engine set
    /// (currently always `0` here — see `GameLogEntry`'s doc comment, it is
    /// assigned downstream by a UI consumer, not by this write path).
    /// Ordering for this stream comes from write order (callers write while
    /// holding the session lock, so it's the actual game order), not `seq`.
    pub fn write_game_log_entries(&self, game_code: &str, entries: &[GameLogEntry]) {
        if self.games_dir.is_none() {
            return;
        }
        for entry in entries {
            let Ok(Value::Object(mut fields)) = serde_json::to_value(entry) else {
                continue;
            };
            // `or_insert`, not `insert`: `GameLogEntry` has no `ts` field
            // today, but this keeps the same non-clobbering contract as the
            // `phase-server` session-stream writer if that ever changes.
            fields
                .entry("ts".to_string())
                .or_insert_with(|| Value::String(format_timestamp()));
            if let Ok(line) = serde_json::to_string(&Value::Object(fields)) {
                self.write_line(game_code, Stream::Events, &line);
            }
        }
    }

    /// Flush and drop every stream's cached writer for a game. Reopened
    /// lazily (in append mode) if another connection resumes writing to the
    /// same game.
    pub fn close(&self, game_code: &str) {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        for stream in [Stream::Session, Stream::Events] {
            if let Some(Some(mut writer)) = files.remove(&(game_code.to_string(), stream)) {
                let _ = writer.flush();
            }
        }
    }

    /// Number of cached writer entries (open or sentinel) across all games.
    /// Test-only introspection for proving `close`/`SessionManager::remove_game`
    /// actually evict the cache, not just that the written file's content is
    /// correct — `files` is private, so `server_core::session`'s tests need
    /// this to observe eviction from outside `game_log`.
    #[cfg(test)]
    pub(crate) fn cached_entry_count(&self) -> usize {
        self.files.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Default for GameFileCache {
    /// A disabled cache, so callers that default-construct their owner
    /// (`SessionManager`, `ServerContext`, ...) get a real, inert no-op
    /// writer rather than requiring every test to wire one up.
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::log::{LogCategory, LogPresentation};
    use engine::types::phase::Phase;
    use std::fs;

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
    fn game_file_cache_creates_session_stream_file() {
        let tmp = tempfile::tempdir().unwrap();
        let games_dir = tmp.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();

        let cache = GameFileCache::new(games_dir.clone());
        cache.write_line("TEST01", Stream::Session, r#"{"message":"hello"}"#);

        let log_path = games_dir.join("TEST01.session.jsonl");
        assert!(log_path.exists());
    }

    #[test]
    fn game_file_cache_appends_across_reopen_after_close() {
        // Not just "two writes in a row" (which never exercises `open_file`
        // twice, since the cached writer stays open) — this closes the
        // session between writes, forcing a real reopen in append mode.
        // Flipping `OpenOptions::append(true)` to `truncate(true)` at the
        // reopen would make this fail.
        let tmp = tempfile::tempdir().unwrap();
        let games_dir = tmp.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();
        let cache = GameFileCache::new(games_dir.clone());
        let log_path = games_dir.join("APPEND.session.jsonl");

        cache.write_line("APPEND", Stream::Session, r#"{"n":1}"#);
        cache.close("APPEND");
        cache.write_line("APPEND", Stream::Session, r#"{"n":2}"#);

        let content = fs::read_to_string(&log_path).unwrap();
        assert_eq!(content, "{\"n\":1}\n{\"n\":2}\n");
    }

    #[test]
    fn open_file_sentinel_on_bad_path_does_not_panic_or_retry_loop() {
        // A non-existent, uncreatable parent directory: `open_file` must
        // return `None` (not panic), and `write_line` must not panic either.
        let cache = GameFileCache::new(PathBuf::from("/nonexistent/path/games"));
        assert!(cache.open_file("FAIL01", Stream::Session).is_none());
        cache.write_line("FAIL01", Stream::Session, r#"{"n":1}"#);
        // The failed open is cached as a `None` sentinel, not retried.
        // `matches!`, not `assert_eq!`: `BufWriter<File>` has no `PartialEq`.
        let files = cache.files.lock().unwrap();
        assert!(matches!(
            files.get(&("FAIL01".to_string(), Stream::Session)),
            Some(None)
        ));
    }

    #[test]
    fn game_file_cache_disabled_write_touches_no_state() {
        // A disabled cache (stdout mode, no log_dir) must not create files
        // AND must not populate its internal map — asserting only
        // `games_dir.is_none()` (a constructor property) would stay green
        // even if the `games_dir.is_none()` early-return in `write_line`
        // were deleted, since `open_file` also short-circuits to `None` and
        // gets cached as a sentinel either way. This asserts the map itself
        // stays empty, which only the early-return guarantees.
        let cache = GameFileCache::disabled();
        cache.write_line("FAIL01", Stream::Session, r#"{"n":1}"#);
        assert!(cache.files.lock().unwrap().is_empty());
    }

    fn sample_entry(category: LogCategory) -> GameLogEntry {
        GameLogEntry {
            // The engine always emits `seq: 0` here (assigned downstream by
            // a UI consumer, not this layer) — using a non-zero value would
            // test a fixture production never produces.
            seq: 0,
            turn: 1,
            phase: Phase::PreCombatMain,
            category,
            segments: Vec::new(),
            presentation: LogPresentation::default(),
        }
    }

    #[test]
    fn write_game_log_entries_preserves_category_and_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let games_dir = tmp.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();
        let cache = GameFileCache::new(games_dir.clone());

        cache.write_game_log_entries("CAT01", &[sample_entry(LogCategory::Combat)]);

        let content = fs::read_to_string(games_dir.join("CAT01.events.jsonl")).unwrap();
        let line: Value = serde_json::from_str(content.trim_end()).unwrap();
        // Discriminating: a bug that dropped the category field, or one that
        // always tagged entries `Debug`, fails this assertion against a
        // deliberately non-Debug variant.
        assert_eq!(line["category"], "Combat");
        assert!(line["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn write_game_log_entries_writes_one_line_per_entry_in_order_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let games_dir = tmp.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();
        let cache = GameFileCache::new(games_dir.clone());

        cache.write_game_log_entries(
            "SEQ01",
            &[
                sample_entry(LogCategory::Turn),
                sample_entry(LogCategory::Combat),
            ],
        );

        let content = fs::read_to_string(games_dir.join("SEQ01.events.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "each entry must be its own line, not overwritten"
        );
        // `seq` is always 0 in production (see `sample_entry`) — order is
        // established by write/line position, not by `.seq`.
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["category"], "Turn");
        assert_eq!(second["category"], "Combat");
    }

    #[test]
    fn closing_session_stream_does_not_lose_flushed_events_stream_content() {
        let tmp = tempfile::tempdir().unwrap();
        let games_dir = tmp.path().join("games");
        fs::create_dir_all(&games_dir).unwrap();
        let cache = GameFileCache::new(games_dir.clone());

        cache.write_game_log_entries("BOTH01", &[sample_entry(LogCategory::Zone)]);
        cache.write_line("BOTH01", Stream::Session, r#"{"message":"joined"}"#);

        // Simulate the game_session span closing — this must not touch
        // content already flushed to the independent "events" stream, and a
        // write after close (a reopen) must still land in the same file.
        cache.close("BOTH01");
        cache.write_game_log_entries("BOTH01", &[sample_entry(LogCategory::Life)]);

        let events_content = fs::read_to_string(games_dir.join("BOTH01.events.jsonl")).unwrap();
        assert_eq!(events_content.lines().count(), 2);
    }
}
