use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tracing::{error, info};

/// SQLite-backed persistence for active game sessions.
///
/// Uses `std::sync::Mutex` to make `Connection` `Send`, since
/// `rusqlite::Connection` is `!Send` (internal `RefCell`).
/// All operations acquire the lock briefly for a single SQL statement.
pub struct GameDb {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct RatingDelta {
    pub player_key: String,
    pub game_code: String,
    pub opponent_key: String,
    pub won: bool,
    pub rating_before: i32,
    pub rating_after: i32,
    pub rating_delta: i32,
}

impl GameDb {
    /// Open (or create) the game database at the given path.
    /// Enables WAL mode and creates the schema if needed.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS game_sessions (
                game_code TEXT PRIMARY KEY,
                session_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS draft_sessions (
                draft_code TEXT PRIMARY KEY,
                session_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS p2p_draft_backups (
                draft_code TEXT PRIMARY KEY,
                host_peer_id TEXT NOT NULL,
                snapshot_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS player_ratings (
                player_key TEXT PRIMARY KEY,
                rating INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS ranked_match_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                player_key TEXT NOT NULL,
                game_code TEXT NOT NULL,
                opponent_key TEXT NOT NULL,
                won INTEGER NOT NULL,
                rating_before INTEGER NOT NULL,
                rating_after INTEGER NOT NULL,
                rating_delta INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ranked_match_history_player_key
                ON ranked_match_history (player_key);",
        )?;
        info!("Game database opened at {}", path.display());
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Persist a game session (upsert).
    pub fn save_session(&self, game_code: &str, json: &str) -> rusqlite::Result<()> {
        let now = now_epoch();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO game_sessions (game_code, session_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(game_code) DO UPDATE SET session_json = ?2, updated_at = ?3",
            params![game_code, json, now],
        )?;
        Ok(())
    }

    /// Load all persisted sessions. Returns (game_code, json) pairs.
    pub fn load_all(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT game_code, session_json FROM game_sessions")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(pair) => results.push(pair),
                Err(e) => error!("Failed to read persisted session row: {}", e),
            }
        }
        Ok(results)
    }

    /// Delete a session by game code.
    pub fn delete_session(&self, game_code: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM game_sessions WHERE game_code = ?1",
            params![game_code],
        )?;
        Ok(())
    }

    /// Delete persisted sessions older than `max_age_secs` seconds across every
    /// session table — `game_sessions`, `draft_sessions`, and
    /// `p2p_draft_backups` — and return the total number of rows removed.
    ///
    /// Previously only `game_sessions` was pruned, so stale `draft_sessions`
    /// and `p2p_draft_backups` rows (abandoned drafts, hosts that never cleanly
    /// tore down a P2P pod) accumulated indefinitely and leaked database
    /// storage on long-running servers.
    pub fn delete_stale(&self, max_age_secs: u64) -> rusqlite::Result<usize> {
        let cutoff = now_epoch().saturating_sub(max_age_secs);
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut deleted = tx.execute(
            "DELETE FROM game_sessions WHERE updated_at < ?1",
            params![cutoff],
        )?;
        deleted += tx.execute(
            "DELETE FROM draft_sessions WHERE updated_at < ?1",
            params![cutoff],
        )?;
        deleted += tx.execute(
            "DELETE FROM p2p_draft_backups WHERE updated_at < ?1",
            params![cutoff],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    // ── Draft session persistence ──────────────────────────────────────────

    /// Persist a draft session (upsert).
    pub fn save_draft_session(&self, draft_code: &str, json: &str) -> rusqlite::Result<()> {
        let now = now_epoch();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO draft_sessions (draft_code, session_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(draft_code) DO UPDATE SET session_json = ?2, updated_at = ?3",
            params![draft_code, json, now],
        )?;
        Ok(())
    }

    /// Load all persisted draft sessions. Returns (draft_code, json) pairs.
    pub fn load_all_drafts(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT draft_code, session_json FROM draft_sessions")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(pair) => results.push(pair),
                Err(e) => error!("Failed to read persisted draft session row: {}", e),
            }
        }
        Ok(results)
    }

    /// Delete a draft session by code.
    pub fn delete_draft_session(&self, draft_code: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM draft_sessions WHERE draft_code = ?1",
            params![draft_code],
        )?;
        Ok(())
    }

    // ── P2P draft backup persistence ───────────────────────────────────────

    /// Store a P2P draft backup snapshot (upsert).
    pub fn save_p2p_backup(
        &self,
        draft_code: &str,
        host_peer_id: &str,
        snapshot_json: &str,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO p2p_draft_backups (draft_code, host_peer_id, snapshot_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(draft_code) DO UPDATE SET host_peer_id = ?2, snapshot_json = ?3, updated_at = ?4",
            params![draft_code, host_peer_id, snapshot_json, now],
        )?;
        Ok(())
    }

    /// Load a P2P draft backup by code. Returns (host_peer_id, snapshot_json, updated_at).
    pub fn load_p2p_backup(
        &self,
        draft_code: &str,
    ) -> rusqlite::Result<Option<(String, String, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT host_peer_id, snapshot_json, updated_at FROM p2p_draft_backups WHERE draft_code = ?1",
        )?;
        let result = stmt.query_row(params![draft_code], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        });
        match result {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Delete a P2P draft backup by code.
    pub fn delete_p2p_backup(&self, draft_code: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM p2p_draft_backups WHERE draft_code = ?1",
            params![draft_code],
        )?;
        Ok(())
    }

    pub fn load_rating(&self, player_key: &str) -> rusqlite::Result<Option<i32>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT rating FROM player_ratings WHERE player_key = ?1 LIMIT 1")?;
        let result = stmt.query_row(params![player_key], |row| row.get::<_, i32>(0));
        match result {
            Ok(rating) => Ok(Some(rating)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn save_ranked_result(&self, deltas: &[RatingDelta]) -> rusqlite::Result<()> {
        if deltas.is_empty() {
            return Ok(());
        }
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for delta in deltas {
            tx.execute(
                "INSERT INTO player_ratings (player_key, rating, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(player_key) DO UPDATE SET rating = ?2, updated_at = ?3",
                params![delta.player_key, delta.rating_after, now],
            )?;
            tx.execute(
                "INSERT INTO ranked_match_history
                 (player_key, game_code, opponent_key, won, rating_before, rating_after, rating_delta, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    delta.player_key,
                    delta.game_code,
                    delta.opponent_key,
                    if delta.won { 1 } else { 0 },
                    delta.rating_before,
                    delta.rating_after,
                    delta.rating_delta,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn test_db() -> GameDb {
        let file = NamedTempFile::new().unwrap();
        GameDb::open(file.path()).unwrap()
    }

    #[test]
    fn save_and_load_roundtrip() {
        let db = test_db();
        db.save_session("ABC123", r#"{"game_code":"ABC123"}"#)
            .unwrap();
        let all = db.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "ABC123");
        assert!(all[0].1.contains("ABC123"));
    }

    #[test]
    fn upsert_overwrites() {
        let db = test_db();
        db.save_session("ABC123", "v1").unwrap();
        db.save_session("ABC123", "v2").unwrap();
        let all = db.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1, "v2");
    }

    #[test]
    fn delete_session_removes_row() {
        let db = test_db();
        db.save_session("ABC123", "data").unwrap();
        db.delete_session("ABC123").unwrap();
        let all = db.load_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn save_and_load_draft_roundtrip() {
        let db = test_db();
        db.save_draft_session("DRAF01", r#"{"draft_code":"DRAF01"}"#)
            .unwrap();
        let all = db.load_all_drafts().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "DRAF01");
        assert!(all[0].1.contains("DRAF01"));
    }

    #[test]
    fn draft_upsert_overwrites() {
        let db = test_db();
        db.save_draft_session("DRAF01", "v1").unwrap();
        db.save_draft_session("DRAF01", "v2").unwrap();
        let all = db.load_all_drafts().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1, "v2");
    }

    #[test]
    fn delete_draft_session_removes_row() {
        let db = test_db();
        db.save_draft_session("DRAF01", "data").unwrap();
        db.delete_draft_session("DRAF01").unwrap();
        let all = db.load_all_drafts().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn save_and_load_p2p_backup_roundtrip() {
        let db = test_db();
        db.save_p2p_backup("BACK01", "peer-abc", r#"{"snapshot":"data"}"#)
            .unwrap();
        let result = db.load_p2p_backup("BACK01").unwrap();
        assert!(result.is_some());
        let (peer_id, snapshot, _updated_at) = result.unwrap();
        assert_eq!(peer_id, "peer-abc");
        assert!(snapshot.contains("snapshot"));
    }

    #[test]
    fn p2p_backup_upsert_overwrites() {
        let db = test_db();
        db.save_p2p_backup("BACK01", "peer-1", "v1").unwrap();
        db.save_p2p_backup("BACK01", "peer-2", "v2").unwrap();
        let (peer_id, snapshot, _) = db.load_p2p_backup("BACK01").unwrap().unwrap();
        assert_eq!(peer_id, "peer-2");
        assert_eq!(snapshot, "v2");
    }

    #[test]
    fn delete_p2p_backup_removes_row() {
        let db = test_db();
        db.save_p2p_backup("BACK01", "peer-1", "data").unwrap();
        db.delete_p2p_backup("BACK01").unwrap();
        assert!(db.load_p2p_backup("BACK01").unwrap().is_none());
    }

    #[test]
    fn ranked_match_history_has_player_key_index() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("PRAGMA index_list('ranked_match_history')")
            .unwrap();
        let indexes = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(indexes
            .iter()
            .any(|name| name == "idx_ranked_match_history_player_key"));
    }

    #[test]
    fn load_p2p_backup_not_found() {
        let db = test_db();
        assert!(db.load_p2p_backup("NOPE01").unwrap().is_none());
    }

    #[test]
    fn delete_stale_removes_old_entries() {
        let db = test_db();
        // Insert with a very old timestamp
        db.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO game_sessions (game_code, session_json, updated_at) VALUES (?1, ?2, ?3)",
                params!["OLD001", "old", 1000u64],
            )
            .unwrap();
        db.save_session("NEW001", "new").unwrap();

        let deleted = db.delete_stale(86400).unwrap();
        assert_eq!(deleted, 1);

        let all = db.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "NEW001");
    }

    #[test]
    fn delete_stale_removes_old_draft_sessions() {
        let db = test_db();
        db.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO draft_sessions (draft_code, session_json, updated_at) VALUES (?1, ?2, ?3)",
                params!["OLDDRAFT", "old", 1000u64],
            )
            .unwrap();
        db.save_draft_session("NEWDRAFT", "new").unwrap();

        let deleted = db.delete_stale(86400).unwrap();
        assert_eq!(deleted, 1);

        let all = db.load_all_drafts().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "NEWDRAFT");
    }

    #[test]
    fn delete_stale_removes_old_p2p_backups() {
        let db = test_db();
        db.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO p2p_draft_backups (draft_code, host_peer_id, snapshot_json, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params!["OLDBACK", "peer", "old", 1000u64],
            )
            .unwrap();
        db.save_p2p_backup("NEWBACK", "peer", "new").unwrap();

        let deleted = db.delete_stale(86400).unwrap();
        assert_eq!(deleted, 1);

        assert!(db.load_p2p_backup("OLDBACK").unwrap().is_none());
        assert!(db.load_p2p_backup("NEWBACK").unwrap().is_some());
    }

    #[test]
    fn delete_stale_prunes_every_session_table_and_counts_all() {
        let db = test_db();
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO game_sessions (game_code, session_json, updated_at) VALUES (?1, ?2, ?3)",
                params!["G", "old", 1000u64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO draft_sessions (draft_code, session_json, updated_at) VALUES (?1, ?2, ?3)",
                params!["D", "old", 1000u64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO p2p_draft_backups (draft_code, host_peer_id, snapshot_json, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params!["B", "peer", "old", 1000u64],
            )
            .unwrap();
        }

        // Fresh rows in each table must survive.
        db.save_session("GNEW", "new").unwrap();
        db.save_draft_session("DNEW", "new").unwrap();
        db.save_p2p_backup("BNEW", "peer", "new").unwrap();

        let deleted = db.delete_stale(86400).unwrap();
        assert_eq!(deleted, 3);

        assert_eq!(db.load_all().unwrap().len(), 1);
        assert_eq!(db.load_all_drafts().unwrap().len(), 1);
        assert!(db.load_p2p_backup("B").unwrap().is_none());
        assert!(db.load_p2p_backup("BNEW").unwrap().is_some());
    }
}
