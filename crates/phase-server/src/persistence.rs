use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use engine::types::player::PlayerId;
use rusqlite::{params, Connection, OptionalExtension};
use server_core::{
    CurrentTerminalDelivery, FullPersistDisposition, FullPersistSnapshot, FullSessionKey,
    PersistedSession, TerminalBootstrapRequest, TerminalCredential, TerminalDeliveryId,
    TerminalMatchDisplay,
};
use sha2::{Digest, Sha256};
use tracing::{error, info};

/// How the game-session store bounds retention of `game_sessions` rows.
///
/// This is a property of the deployment, not of any individual save call, so it
/// lives on the store and `save_session` is its single enforcement point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionRetention {
    /// Online server: every `game_code` persists independently. Many games run
    /// concurrently across distinct seats, so a save only upserts its own row.
    Multiplayer,
    /// Single-user desktop instance: at most one solo game exists at a time.
    /// The client tracks a single active-game pointer, so starting a new game
    /// orphans the previous session server-side. Saving a new `game_code`
    /// prunes every other game session, keeping the store bounded to one row
    /// without relying on the stale-age purge (which single-user disables so a
    /// suspended game never expires).
    SingleUser,
}

/// SQLite-backed persistence for active game sessions.
///
/// Uses `std::sync::Mutex` to make `Connection` `Send`, since
/// `rusqlite::Connection` is `!Send` (internal `RefCell`).
/// All operations acquire the lock briefly for a single SQL statement.
pub struct GameDb {
    conn: Mutex<Connection>,
    retention: SessionRetention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatingDelta {
    pub player_key: String,
    pub game_code: String,
    pub opponent_key: String,
    pub won: bool,
    pub rating_before: i32,
    pub rating_after: i32,
    pub rating_delta: i32,
}

/// Server-private terminal artifact supplied by the future Full finalizer.
/// The pre-terminal tokens are used only during preparation to derive stable
/// recipient credentials; neither raw token nor raw credential reaches SQLite.
#[derive(Debug, Clone)]
pub struct FullTerminalArtifact {
    pub key: FullSessionKey,
    pub terminal_revision: u64,
    pub display: TerminalMatchDisplay,
    pub recipients: Vec<TerminalRecipient>,
}

#[derive(Debug, Clone)]
pub struct TerminalRecipient {
    pub player_id: PlayerId,
    pub pre_terminal_player_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareFullTerminalDisposition {
    Prepared,
    AlreadyPrepared,
}

impl GameDb {
    pub fn is_single_user(&self) -> bool {
        self.retention == SessionRetention::SingleUser
    }

    /// Open (or create) the game database at the given path.
    /// Enables WAL mode and creates the schema if needed.
    pub fn open(path: &Path, retention: SessionRetention) -> rusqlite::Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        migrate_game_sessions(&mut conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS draft_sessions (
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
            retention,
        })
    }

    /// Legacy persistence path retained while Full runtime callers migrate to
    /// generation-fenced snapshots. New lifecycle code must use
    /// [`Self::save_full_session`] instead.
    #[cfg(test)]
    pub(crate) fn save_session(&self, game_code: &str, json: &str) -> rusqlite::Result<()> {
        let now = now_epoch();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO game_sessions
                (game_code, generation, mutation_revision, activation_epoch, retired, session_json, updated_at)
             VALUES (?1, 0, 0, NULL, 0, ?2, ?3)
             ON CONFLICT(game_code) DO UPDATE SET
                session_json = ?2, updated_at = ?3
             WHERE game_sessions.generation = 0 AND game_sessions.retired = 0",
            params![game_code, json, now],
        )?;
        Ok(())
    }

    /// Load all persisted sessions. Returns (game_code, json) pairs.
    #[cfg(test)]
    pub(crate) fn load_all(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT game_code, session_json FROM game_sessions
             WHERE retired = 0 AND session_json IS NOT NULL",
        )?;
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

    /// Legacy destructive removal retained until Full terminal finalization is
    /// routed through retirement tombstones.
    #[cfg(test)]
    pub(crate) fn delete_session(&self, game_code: &str) -> rusqlite::Result<()> {
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
            "DELETE FROM game_sessions WHERE retired = 0 AND updated_at < ?1",
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

    /// Allocates and durably binds a new Full key to `game_code` before any
    /// caller can publish credentials for that session. The placeholder row is
    /// intentionally snapshot-less; the first mutation save supplies its
    /// authoritative state and revision.
    pub fn create_full_session_key(&self, game_code: &str) -> rusqlite::Result<FullSessionKey> {
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing_active = tx
            .query_row(
                "SELECT retired FROM game_sessions WHERE game_code = ?1",
                params![game_code],
                |row| row.get::<_, bool>(0),
            )
            .optional()?;
        if existing_active == Some(false) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let previous = tx
            .query_row(
                "SELECT generation FROM full_generation_high_water WHERE game_code = ?1",
                params![game_code],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let generation = previous.saturating_add(1);
        tx.execute(
            "INSERT INTO full_generation_high_water (game_code, generation)
             VALUES (?1, ?2)
             ON CONFLICT(game_code) DO UPDATE SET generation = excluded.generation",
            params![game_code, generation],
        )?;
        tx.execute(
            "INSERT INTO game_sessions
                (game_code, generation, mutation_revision, activation_epoch, retired, session_json, updated_at)
             VALUES (?1, ?2, 0, NULL, 0, NULL, ?3)
             ON CONFLICT(game_code) DO UPDATE SET
                generation = excluded.generation,
                mutation_revision = 0,
                activation_epoch = NULL,
                retired = 0,
                session_json = NULL,
                updated_at = excluded.updated_at
             WHERE game_sessions.retired = 1",
            params![game_code, generation, now],
        )?;
        tx.commit()?;
        Ok(FullSessionKey {
            game_code: game_code.to_string(),
            generation,
        })
    }

    /// Returns the exact live key for a game code, never reconstructing a
    /// generation from the code string or from a client payload.
    pub fn load_active_full_key(
        &self,
        game_code: &str,
    ) -> rusqlite::Result<Option<FullSessionKey>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT generation FROM game_sessions WHERE game_code = ?1 AND retired = 0",
            params![game_code],
            |row| {
                Ok(FullSessionKey {
                    game_code: game_code.to_string(),
                    generation: row.get(0)?,
                })
            },
        )
        .optional()
    }

    /// Saves a Full authoritative snapshot only when it is newer than the
    /// retained lifetime. Equal revisions are intentionally no-ops.
    pub fn save_full_session(
        &self,
        snapshot: &FullPersistSnapshot,
    ) -> rusqlite::Result<FullPersistDisposition> {
        let json = serde_json::to_string(&snapshot.persisted)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        if let Some(epoch) = snapshot.activation_epoch {
            let active = tx
                .query_row(
                    "SELECT game_code, generation, activation_epoch FROM full_active_session WHERE slot = 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, u64>(1)?,
                            row.get::<_, u64>(2)?,
                        ))
                    },
                )
                .optional()?;
            if active
                != Some((
                    snapshot.key.game_code.clone(),
                    snapshot.key.generation,
                    epoch,
                ))
            {
                return Ok(FullPersistDisposition::NotCurrentActivation);
            }
        }

        let changed = tx.execute(
            "INSERT INTO game_sessions
                (game_code, generation, mutation_revision, activation_epoch, retired, session_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)
             ON CONFLICT(game_code) DO UPDATE SET
                generation = excluded.generation,
                mutation_revision = excluded.mutation_revision,
                activation_epoch = excluded.activation_epoch,
                retired = 0,
                session_json = excluded.session_json,
                updated_at = excluded.updated_at
             WHERE excluded.generation > game_sessions.generation
                OR (excluded.generation = game_sessions.generation
                    AND game_sessions.retired = 0
                    AND (excluded.mutation_revision > game_sessions.mutation_revision
                        OR game_sessions.session_json IS NULL))",
            params![
                snapshot.key.game_code,
                snapshot.key.generation,
                snapshot.mutation_revision,
                snapshot.activation_epoch,
                json,
                now,
            ],
        )?;
        if changed == 1 {
            tx.execute(
                "INSERT INTO full_generation_high_water (game_code, generation)
                 VALUES (?1, ?2)
                 ON CONFLICT(game_code) DO UPDATE SET generation = MAX(generation, excluded.generation)",
                params![snapshot.key.game_code, snapshot.key.generation],
            )?;
            tx.commit()?;
            Ok(FullPersistDisposition::Applied)
        } else {
            tx.commit()?;
            Ok(FullPersistDisposition::SupersededOrRetired)
        }
    }

    /// Activates a newly-created Full session in the desktop singleton. The
    /// pointer transition and retirement of the previous active row are one
    /// SQLite transaction, so a delayed save from the prior activation cannot
    /// become current again.
    pub fn activate_single_user_session(
        &self,
        snapshot: &FullPersistSnapshot,
    ) -> rusqlite::Result<(u64, FullPersistDisposition)> {
        if self.retention != SessionRetention::SingleUser {
            return Err(rusqlite::Error::InvalidQuery);
        }

        let json = serde_json::to_string(&snapshot.persisted)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let next_epoch = tx
            .query_row(
                "SELECT activation_epoch FROM full_active_session WHERE slot = 1",
                [],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .saturating_add(1);

        let current_generation = tx
            .query_row(
                "SELECT generation FROM game_sessions WHERE game_code = ?1",
                params![snapshot.key.game_code],
                |row| row.get::<_, u64>(0),
            )
            .optional()?;
        if current_generation.is_some_and(|generation| generation > snapshot.key.generation) {
            return Ok((next_epoch, FullPersistDisposition::SupersededOrRetired));
        }

        tx.execute(
            "UPDATE game_sessions
             SET retired = 1, session_json = NULL, updated_at = ?1
             WHERE (game_code, generation) IN (
                 SELECT game_code, generation FROM full_active_session WHERE slot = 1
             ) AND retired = 0",
            params![now],
        )?;
        tx.execute(
            "INSERT INTO game_sessions
                (game_code, generation, mutation_revision, activation_epoch, retired, session_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)
             ON CONFLICT(game_code) DO UPDATE SET
                generation = excluded.generation,
                mutation_revision = excluded.mutation_revision,
                activation_epoch = excluded.activation_epoch,
                retired = 0,
                session_json = excluded.session_json,
                updated_at = excluded.updated_at
             WHERE excluded.generation >= game_sessions.generation",
            params![
                snapshot.key.game_code,
                snapshot.key.generation,
                snapshot.mutation_revision,
                next_epoch,
                json,
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO full_active_session (slot, game_code, generation, activation_epoch)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(slot) DO UPDATE SET
                game_code = excluded.game_code,
                generation = excluded.generation,
                activation_epoch = excluded.activation_epoch",
            params![snapshot.key.game_code, snapshot.key.generation, next_epoch],
        )?;
        tx.execute(
            "INSERT INTO full_generation_high_water (game_code, generation)
             VALUES (?1, ?2)
             ON CONFLICT(game_code) DO UPDATE SET generation = MAX(generation, excluded.generation)",
            params![snapshot.key.game_code, snapshot.key.generation],
        )?;
        tx.commit()?;
        Ok((next_epoch, FullPersistDisposition::Applied))
    }

    /// Retires a never-started Full session while retaining its generation
    /// tombstone. A started game must use the terminal finalizer instead.
    pub fn retire_unstarted_full_session(
        &self,
        key: &FullSessionKey,
        activation_epoch: Option<u64>,
    ) -> rusqlite::Result<FullPersistDisposition> {
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        if let Some(epoch) = activation_epoch {
            let active = tx
                .query_row(
                    "SELECT game_code, generation, activation_epoch FROM full_active_session WHERE slot = 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, u64>(1)?,
                            row.get::<_, u64>(2)?,
                        ))
                    },
                )
                .optional()?;
            if active != Some((key.game_code.clone(), key.generation, epoch)) {
                return Ok(FullPersistDisposition::NotCurrentActivation);
            }
        }

        let row = tx
            .query_row(
                "SELECT retired, session_json FROM game_sessions
                 WHERE game_code = ?1 AND generation = ?2",
                params![key.game_code, key.generation],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        let Some((retired, json)) = row else {
            return Ok(FullPersistDisposition::SupersededOrRetired);
        };
        if retired {
            return Ok(FullPersistDisposition::SupersededOrRetired);
        }
        if let Some(json) = json {
            let persisted: PersistedSession = serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            if persisted.game_started {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }

        tx.execute(
            "UPDATE game_sessions
             SET retired = 1, session_json = NULL, updated_at = ?1
             WHERE game_code = ?2 AND generation = ?3 AND retired = 0",
            params![now, key.game_code, key.generation],
        )?;
        if activation_epoch.is_some() {
            tx.execute("DELETE FROM full_active_session WHERE slot = 1", [])?;
        }
        tx.commit()?;
        Ok(FullPersistDisposition::Applied)
    }

    /// Loads only non-retired Full session rows. Terminal tombstones remain in
    /// SQLite to fence stale writers but are never reconstructed at startup.
    pub fn load_active_full_sessions(&self) -> rusqlite::Result<Vec<FullPersistSnapshot>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT game_code, generation, mutation_revision, activation_epoch, session_json
             FROM game_sessions WHERE retired = 0 AND session_json IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, Option<u64>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut snapshots = Vec::new();
        for row in rows {
            let (game_code, generation, mutation_revision, activation_epoch, json) = row?;
            match serde_json::from_str(&json) {
                Ok(persisted) => snapshots.push(FullPersistSnapshot {
                    key: FullSessionKey {
                        game_code,
                        generation,
                    },
                    mutation_revision,
                    activation_epoch,
                    persisted,
                }),
                Err(error) => error!("Failed to deserialize Full session row: {error}"),
            }
        }
        Ok(snapshots)
    }

    /// Atomically records one immutable Full terminal artifact, creates one
    /// delivery row per occupied seat, and retires the matching active row.
    /// Replaying byte-identical artifact content is idempotent; any changed
    /// replay is rejected rather than replacing a result already shown to a
    /// recipient.
    pub fn prepare_full_terminal(
        &self,
        artifact: &FullTerminalArtifact,
    ) -> rusqlite::Result<PrepareFullTerminalDisposition> {
        let display_json = serde_json::to_string(&artifact.display)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let digest = terminal_artifact_digest(artifact, &display_json);
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        if let Some(existing_digest) = tx
            .query_row(
                "SELECT artifact_digest FROM terminal_match_results
                 WHERE game_code = ?1 AND generation = ?2",
                params![artifact.key.game_code, artifact.key.generation],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing_digest == digest {
                tx.commit()?;
                return Ok(PrepareFullTerminalDisposition::AlreadyPrepared);
            }
            return Err(rusqlite::Error::InvalidQuery);
        }

        let active = tx
            .query_row(
                "SELECT retired FROM game_sessions WHERE game_code = ?1 AND generation = ?2",
                params![artifact.key.game_code, artifact.key.generation],
                |row| row.get::<_, bool>(0),
            )
            .optional()?;
        if active != Some(false) || artifact.recipients.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }

        let mut seen_players = std::collections::HashSet::new();
        for recipient in &artifact.recipients {
            if recipient.pre_terminal_player_token.is_empty()
                || !seen_players.insert(recipient.player_id)
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }

        tx.execute(
            "INSERT INTO terminal_match_results
                (game_code, generation, terminal_revision, artifact_digest, display_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                artifact.key.game_code,
                artifact.key.generation,
                artifact.terminal_revision,
                digest,
                display_json,
                now,
            ],
        )?;
        for recipient in &artifact.recipients {
            let credential = terminal_credential(
                &artifact.key,
                artifact.terminal_revision,
                recipient.player_id,
                &recipient.pre_terminal_player_token,
            );
            tx.execute(
                "INSERT INTO terminal_match_delivery
                    (game_code, generation, player_id, terminal_revision, delivery_id,
                     pre_terminal_token_verifier, credential_verifier, acknowledged_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                params![
                    artifact.key.game_code,
                    artifact.key.generation,
                    recipient.player_id.0,
                    artifact.terminal_revision,
                    terminal_delivery_id(
                        &artifact.key,
                        artifact.terminal_revision,
                        recipient.player_id
                    )
                    .0,
                    verifier(&recipient.pre_terminal_player_token),
                    verifier(&credential.0),
                ],
            )?;
        }
        tx.execute(
            "UPDATE game_sessions
             SET retired = 1, session_json = NULL, updated_at = ?1
             WHERE game_code = ?2 AND generation = ?3 AND retired = 0",
            params![now, artifact.key.game_code, artifact.key.generation],
        )?;
        tx.execute(
            "DELETE FROM full_active_session WHERE game_code = ?1 AND generation = ?2",
            params![artifact.key.game_code, artifact.key.generation],
        )?;
        tx.commit()?;
        Ok(PrepareFullTerminalDisposition::Prepared)
    }

    /// Returns a recipient's currently prepared terminal delivery after
    /// proving the retained pre-terminal token. This is used by the Full
    /// finalizer before it sends a GameOver in Wave 3.
    pub fn current_terminal_delivery_for_recipient(
        &self,
        key: &FullSessionKey,
        player_id: PlayerId,
        pre_terminal_player_token: &str,
    ) -> rusqlite::Result<Option<CurrentTerminalDelivery>> {
        let conn = self.conn.lock().unwrap();
        current_terminal_delivery(&conn, key, player_id, pre_terminal_player_token)
    }

    /// Terminal-only bootstrap. A repeated request id must belong to exactly
    /// the same key and recipient; it returns the same deterministic delivery
    /// tuple and never attaches a normal game session.
    pub fn bootstrap_terminal_delivery(
        &self,
        request: &TerminalBootstrapRequest,
    ) -> rusqlite::Result<Option<CurrentTerminalDelivery>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let recipient = tx
            .query_row(
                "SELECT player_id FROM terminal_match_delivery
                 WHERE game_code = ?1 AND generation = ?2
                   AND pre_terminal_token_verifier = ?3",
                params![
                    request.key.game_code,
                    request.key.generation,
                    verifier(&request.player_token),
                ],
                |row| row.get::<_, u8>(0).map(PlayerId),
            )
            .optional()?;
        let Some(player_id) = recipient else {
            tx.commit()?;
            return Ok(None);
        };

        if let Some(existing) = tx
            .query_row(
                "SELECT game_code, generation, player_id FROM terminal_bootstrap_requests
                 WHERE request_id = ?1",
                params![request.request_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, u8>(2)?,
                    ))
                },
            )
            .optional()?
        {
            if existing
                != (
                    request.key.game_code.clone(),
                    request.key.generation,
                    player_id.0,
                )
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        } else {
            tx.execute(
                "INSERT INTO terminal_bootstrap_requests
                    (request_id, game_code, generation, player_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    request.request_id,
                    request.key.game_code,
                    request.key.generation,
                    player_id.0,
                    now_epoch(),
                ],
            )?;
        }
        let delivery =
            current_terminal_delivery(&tx, &request.key, player_id, &request.player_token)?;
        tx.commit()?;
        Ok(delivery)
    }

    /// Looks up a terminal delivery by its recipient-only credential.
    pub fn read_terminal_result(
        &self,
        credential: &TerminalCredential,
    ) -> rusqlite::Result<Option<CurrentTerminalDelivery>> {
        let conn = self.conn.lock().unwrap();
        terminal_delivery_by_credential(&conn, credential)
    }

    /// Idempotently marks a terminal delivery acknowledged. An invalid
    /// delivery-id/credential pair has no effect and reports `false`.
    pub fn ack_terminal_delivery(
        &self,
        delivery_id: &TerminalDeliveryId,
        credential: &TerminalCredential,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE terminal_match_delivery SET acknowledged_at = COALESCE(acknowledged_at, ?1)
             WHERE delivery_id = ?2 AND credential_verifier = ?3",
            params![now_epoch(), delivery_id.0, verifier(&credential.0)],
        )?;
        Ok(changed == 1)
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

    /// Records one ranked result exactly once per game. A retry returns the
    /// original receipt instead of applying a second rating change.
    pub fn save_ranked_result_idempotent(
        &self,
        deltas: &[RatingDelta],
    ) -> rusqlite::Result<Vec<RatingDelta>> {
        if deltas.is_empty() {
            return Ok(Vec::new());
        }
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let game_code = &deltas[0].game_code;
        if deltas.iter().any(|delta| delta.game_code != *game_code) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut existing_statement = tx.prepare(
            "SELECT player_key, game_code, opponent_key, won, rating_before, rating_after, rating_delta
             FROM ranked_match_history WHERE game_code = ?1 ORDER BY id",
        )?;
        let existing = existing_statement
            .query_map(params![game_code], |row| {
                Ok(RatingDelta {
                    player_key: row.get(0)?,
                    game_code: row.get(1)?,
                    opponent_key: row.get(2)?,
                    won: row.get::<_, i64>(3)? != 0,
                    rating_before: row.get(4)?,
                    rating_after: row.get(5)?,
                    rating_delta: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(existing_statement);
        if !existing.is_empty() {
            if existing.len() != deltas.len()
                || existing.iter().zip(deltas).any(|(saved, requested)| {
                    saved.player_key != requested.player_key
                        || saved.opponent_key != requested.opponent_key
                        || saved.won != requested.won
                })
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.commit()?;
            return Ok(existing);
        }
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
        Ok(deltas.to_vec())
    }
}

fn migrate_game_sessions(conn: &mut Connection) -> rusqlite::Result<()> {
    let game_sessions_exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'game_sessions'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();

    if !game_sessions_exists {
        return create_full_game_session_schema(conn);
    }

    let mut statement = conn.prepare("PRAGMA table_info(game_sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if columns.iter().any(|column| column == "generation") {
        return create_full_game_session_schema(conn);
    }

    let transaction = conn.transaction()?;
    transaction.execute_batch(
        "ALTER TABLE game_sessions RENAME TO game_sessions_legacy;
         CREATE TABLE game_sessions (
             game_code TEXT PRIMARY KEY,
             generation INTEGER NOT NULL DEFAULT 0,
             mutation_revision INTEGER NOT NULL DEFAULT 0,
             activation_epoch INTEGER,
             retired INTEGER NOT NULL DEFAULT 0,
             session_json TEXT,
             updated_at INTEGER NOT NULL
         );
         INSERT INTO game_sessions
             (game_code, generation, mutation_revision, activation_epoch, retired, session_json, updated_at)
         SELECT game_code, 0, 0, NULL, 0, session_json, updated_at
         FROM game_sessions_legacy;
         DROP TABLE game_sessions_legacy;",
    )?;
    transaction.commit()?;
    create_full_game_session_schema(conn)
}

fn create_full_game_session_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS game_sessions (
             game_code TEXT PRIMARY KEY,
             generation INTEGER NOT NULL DEFAULT 0,
             mutation_revision INTEGER NOT NULL DEFAULT 0,
             activation_epoch INTEGER,
             retired INTEGER NOT NULL DEFAULT 0,
             session_json TEXT,
             updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS full_generation_high_water (
             game_code TEXT PRIMARY KEY,
             generation INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS full_active_session (
             slot INTEGER PRIMARY KEY CHECK (slot = 1),
             game_code TEXT NOT NULL,
             generation INTEGER NOT NULL,
             activation_epoch INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS terminal_match_results (
             game_code TEXT NOT NULL,
             generation INTEGER NOT NULL,
             terminal_revision INTEGER NOT NULL,
             artifact_digest TEXT NOT NULL,
             display_json TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (game_code, generation)
         );
         CREATE TABLE IF NOT EXISTS terminal_match_delivery (
             game_code TEXT NOT NULL,
             generation INTEGER NOT NULL,
             player_id INTEGER NOT NULL,
             terminal_revision INTEGER NOT NULL,
             delivery_id TEXT NOT NULL UNIQUE,
             pre_terminal_token_verifier TEXT NOT NULL,
             credential_verifier TEXT NOT NULL,
             acknowledged_at INTEGER,
             PRIMARY KEY (game_code, generation, player_id)
         );
         CREATE TABLE IF NOT EXISTS terminal_bootstrap_requests (
             request_id TEXT PRIMARY KEY,
             game_code TEXT NOT NULL,
             generation INTEGER NOT NULL,
             player_id INTEGER NOT NULL,
             created_at INTEGER NOT NULL
         );",
    )
}

fn current_terminal_delivery(
    conn: &Connection,
    key: &FullSessionKey,
    player_id: PlayerId,
    pre_terminal_player_token: &str,
) -> rusqlite::Result<Option<CurrentTerminalDelivery>> {
    let row = conn
        .query_row(
            "SELECT d.terminal_revision, d.delivery_id, r.display_json
             FROM terminal_match_delivery d
             JOIN terminal_match_results r
               ON r.game_code = d.game_code AND r.generation = d.generation
             WHERE d.game_code = ?1 AND d.generation = ?2 AND d.player_id = ?3
               AND d.pre_terminal_token_verifier = ?4",
            params![
                key.game_code,
                key.generation,
                player_id.0,
                verifier(pre_terminal_player_token),
            ],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((terminal_revision, delivery_id, display_json)) = row else {
        return Ok(None);
    };
    let display = serde_json::from_str(&display_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Some(CurrentTerminalDelivery {
        key: key.clone(),
        terminal_revision,
        delivery_id: TerminalDeliveryId(delivery_id),
        credential: terminal_credential(
            key,
            terminal_revision,
            player_id,
            pre_terminal_player_token,
        ),
        display,
    }))
}

fn terminal_delivery_by_credential(
    conn: &Connection,
    credential: &TerminalCredential,
) -> rusqlite::Result<Option<CurrentTerminalDelivery>> {
    let row = conn
        .query_row(
            "SELECT d.game_code, d.generation, d.player_id, d.terminal_revision,
                    d.delivery_id, r.display_json
             FROM terminal_match_delivery d
             JOIN terminal_match_results r
               ON r.game_code = d.game_code AND r.generation = d.generation
             WHERE d.credential_verifier = ?1",
            params![verifier(&credential.0)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u8>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((game_code, generation, _player_id, terminal_revision, delivery_id, display_json)) =
        row
    else {
        return Ok(None);
    };
    let display = serde_json::from_str(&display_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Some(CurrentTerminalDelivery {
        key: FullSessionKey {
            game_code,
            generation,
        },
        terminal_revision,
        delivery_id: TerminalDeliveryId(delivery_id),
        credential: credential.clone(),
        display,
    }))
}

fn terminal_artifact_digest(artifact: &FullTerminalArtifact, display_json: &str) -> String {
    let mut material = format!(
        "{}:{}:{}:{}",
        artifact.key.game_code, artifact.key.generation, artifact.terminal_revision, display_json
    );
    let mut recipients = artifact
        .recipients
        .iter()
        .map(|recipient| {
            (
                recipient.player_id.0,
                verifier(&recipient.pre_terminal_player_token),
            )
        })
        .collect::<Vec<_>>();
    recipients.sort_unstable();
    for (player_id, token_verifier) in recipients {
        material.push_str(&format!(":{player_id}:{token_verifier}"));
    }
    verifier(&material)
}

fn terminal_delivery_id(
    key: &FullSessionKey,
    terminal_revision: u64,
    player_id: PlayerId,
) -> TerminalDeliveryId {
    TerminalDeliveryId(verifier(&format!(
        "terminal-delivery:{}:{}:{terminal_revision}:{}",
        key.game_code, key.generation, player_id.0
    )))
}

fn terminal_credential(
    key: &FullSessionKey,
    terminal_revision: u64,
    player_id: PlayerId,
    pre_terminal_player_token: &str,
) -> TerminalCredential {
    TerminalCredential(verifier(&format!(
        "terminal-credential:{}:{}:{terminal_revision}:{}:{pre_terminal_player_token}",
        key.game_code, key.generation, player_id.0
    )))
}

fn verifier(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    format!("{digest:x}")
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
    use engine::types::game_state::{GameState, PersistedGameState};
    use tempfile::NamedTempFile;

    fn test_db() -> GameDb {
        let file = NamedTempFile::new().unwrap();
        GameDb::open(file.path(), SessionRetention::Multiplayer).unwrap()
    }

    fn full_snapshot(
        game_code: &str,
        generation: u64,
        mutation_revision: u64,
        activation_epoch: Option<u64>,
        game_started: bool,
    ) -> FullPersistSnapshot {
        FullPersistSnapshot {
            key: FullSessionKey {
                game_code: game_code.to_string(),
                generation,
            },
            mutation_revision,
            activation_epoch,
            persisted: PersistedSession {
                game_code: game_code.to_string(),
                state_revision: mutation_revision,
                ai_driver_fault: None,
                next_ai_driver_fault_id: 1,
                state: PersistedGameState::capture(GameState::new_two_player(7)),
                player_tokens: vec!["player-0".to_string(), "player-1".to_string()],
                deck_choices: vec![None, None],
                display_names: vec!["P0".to_string(), "P1".to_string()],
                timer_seconds: None,
                player_count: 2,
                ai_seats: vec![],
                ai_difficulties: Default::default(),
                game_started,
                start_when_full: true,
                ranked: false,
                lobby_meta: None,
            },
        }
    }

    /// A session with a live Full runtime, so `full_persist_snapshot` produces
    /// the same shape production persists.
    fn seeded_full_session(db: &GameDb) -> (server_core::session::SessionManager, String) {
        use server_core::session::{FullRuntime, SessionManager};

        let mut mgr = SessionManager::new();
        let (code, _token) = mgr.create_game(Default::default(), None);
        let key = db.create_full_session_key(&code).expect("full key");
        let session = mgr.sessions.get_mut(&code).expect("session");
        session.full_runtime = Some(FullRuntime {
            key,
            activation_epoch: None,
        });
        (mgr, code)
    }

    fn save(
        db: &GameDb,
        mgr: &server_core::session::SessionManager,
        code: &str,
    ) -> FullPersistDisposition {
        let snapshot = mgr
            .sessions
            .get(code)
            .expect("session")
            .full_persist_snapshot()
            .expect("a runtime-bound session has a snapshot");
        db.save_full_session(&snapshot).expect("save")
    }

    /// The production symptom: a seat edit's snapshot must clear the
    /// `mutation_revision` fence, or the write silently never lands.
    #[test]
    fn a_seat_edits_persist_is_accepted() {
        use seat_reducer::types::{SeatDelta, SeatState};

        let db = test_db();
        let (mut mgr, code) = seeded_full_session(&db);
        // Reach guard: a mis-seeded fixture would return
        // `SupersededOrRetired` here for an unrelated reason.
        assert_eq!(save(&db, &mgr, &code), FullPersistDisposition::Applied);

        let session = mgr.sessions.get_mut(&code).expect("session");
        let seat_state: SeatState = session.seat_state();
        session.apply_seat_delta(
            seat_state,
            &SeatDelta::empty(),
            &engine::database::CardDatabase::default(),
        );

        assert_eq!(save(&db, &mgr, &code), FullPersistDisposition::Applied);
    }

    #[test]
    fn a_joins_persist_is_accepted() {
        let db = test_db();
        let (mut mgr, code) = seeded_full_session(&db);
        assert_eq!(save(&db, &mgr, &code), FullPersistDisposition::Applied);

        mgr.join_game(&code, Default::default(), None)
            .expect("seat 1 is open");

        assert_eq!(save(&db, &mgr, &code), FullPersistDisposition::Applied);
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
    fn legacy_single_user_save_does_not_prune_other_game_sessions() {
        let file = NamedTempFile::new().unwrap();
        let db = GameDb::open(file.path(), SessionRetention::SingleUser).unwrap();

        // Legacy callers cannot safely implement the active-session transition:
        // they must not delete an unrelated row that may be a retained Full
        // tombstone. Wave 3 routes single-user creation through activation.
        db.save_session("GAME_A", "a").unwrap();
        assert_eq!(db.load_all().unwrap().len(), 1);

        // Re-saving the same game (autosave) keeps exactly one row.
        db.save_session("GAME_A", "a2").unwrap();
        let all = db.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1, "a2");

        // A second legacy save keeps both rows until lifecycle code adopts the
        // atomic active pointer API below.
        db.save_session("GAME_B", "b").unwrap();
        let all = db.load_all().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|(code, _)| code == "GAME_A"));
        assert!(all.iter().any(|(code, _)| code == "GAME_B"));
    }

    #[test]
    fn full_save_rejects_equal_and_stale_mutation_revisions() {
        let db = test_db();
        let initial = full_snapshot("FENCE1", 1, 3, None, false);
        assert_eq!(
            db.save_full_session(&initial).unwrap(),
            FullPersistDisposition::Applied
        );
        assert_eq!(
            db.save_full_session(&full_snapshot("FENCE1", 1, 3, None, false))
                .unwrap(),
            FullPersistDisposition::SupersededOrRetired
        );
        assert_eq!(
            db.save_full_session(&full_snapshot("FENCE1", 1, 2, None, false))
                .unwrap(),
            FullPersistDisposition::SupersededOrRetired
        );
        assert_eq!(
            db.load_active_full_sessions().unwrap()[0].mutation_revision,
            3
        );
    }

    #[test]
    fn full_generation_fences_a_delayed_save_from_an_older_lifetime() {
        let db = test_db();
        let first = db.create_full_session_key("REUSE1").unwrap();
        db.retire_unstarted_full_session(&first, None).unwrap();
        let second = db.create_full_session_key("REUSE1").unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
        assert_eq!(
            db.save_full_session(&full_snapshot("REUSE1", second.generation, 1, None, false))
                .unwrap(),
            FullPersistDisposition::Applied
        );
        assert_eq!(
            db.save_full_session(&full_snapshot("REUSE1", first.generation, 99, None, false))
                .unwrap(),
            FullPersistDisposition::SupersededOrRetired
        );
        assert_eq!(
            db.load_active_full_sessions().unwrap()[0].key.generation,
            second.generation
        );
    }

    #[test]
    fn single_user_activation_retires_previous_row_and_fences_late_save() {
        let file = NamedTempFile::new().unwrap();
        let db = GameDb::open(file.path(), SessionRetention::SingleUser).unwrap();
        let first = full_snapshot("SOLO_A", 1, 1, None, false);
        let (first_epoch, first_result) = db.activate_single_user_session(&first).unwrap();
        assert_eq!(first_result, FullPersistDisposition::Applied);

        let second = full_snapshot("SOLO_B", 1, 1, None, false);
        let (second_epoch, second_result) = db.activate_single_user_session(&second).unwrap();
        assert_eq!(second_result, FullPersistDisposition::Applied);
        assert!(second_epoch > first_epoch);

        assert_eq!(
            db.save_full_session(&full_snapshot("SOLO_A", 1, 2, Some(first_epoch), false))
                .unwrap(),
            FullPersistDisposition::NotCurrentActivation
        );
        let loaded = db.load_active_full_sessions().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].key.game_code, "SOLO_B");
        assert_eq!(loaded[0].activation_epoch, Some(second_epoch));
    }

    #[test]
    fn retiring_unstarted_full_session_keeps_tombstone_out_of_restore() {
        let db = test_db();
        let snapshot = full_snapshot("RETIRE1", 1, 1, None, false);
        db.save_full_session(&snapshot).unwrap();
        assert_eq!(
            db.retire_unstarted_full_session(&snapshot.key, None)
                .unwrap(),
            FullPersistDisposition::Applied
        );
        assert!(db.load_active_full_sessions().unwrap().is_empty());
        assert_eq!(
            db.save_full_session(&full_snapshot("RETIRE1", 1, 2, None, false))
                .unwrap(),
            FullPersistDisposition::SupersededOrRetired
        );
    }

    #[test]
    fn full_key_allocation_is_durable_and_never_derived_from_code() {
        let db = test_db();
        let first = db.create_full_session_key("KEY001").unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(
            db.load_active_full_key("KEY001").unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            db.save_full_session(&FullPersistSnapshot {
                key: first.clone(),
                mutation_revision: 0,
                activation_epoch: None,
                persisted: full_snapshot("KEY001", 1, 0, None, false).persisted,
            })
            .unwrap(),
            FullPersistDisposition::Applied
        );
        db.retire_unstarted_full_session(&first, None).unwrap();
        let second = db.create_full_session_key("KEY001").unwrap();
        assert_eq!(second.generation, 2);
        assert_eq!(db.load_active_full_key("KEY001").unwrap(), Some(second));
    }

    #[test]
    fn prepared_terminal_bootstraps_reads_and_acknowledges_idempotently() {
        let db = test_db();
        let snapshot = full_snapshot("TERM01", 1, 1, None, true);
        db.save_full_session(&snapshot).unwrap();
        let artifact = FullTerminalArtifact {
            key: snapshot.key.clone(),
            terminal_revision: 2,
            display: TerminalMatchDisplay {
                winner: Some(PlayerId(1)),
                reason: "Match conceded".to_string(),
                ranked_result: None,
            },
            recipients: vec![
                TerminalRecipient {
                    player_id: PlayerId(0),
                    pre_terminal_player_token: "player-0".to_string(),
                },
                TerminalRecipient {
                    player_id: PlayerId(1),
                    pre_terminal_player_token: "player-1".to_string(),
                },
            ],
        };
        assert_eq!(
            db.prepare_full_terminal(&artifact).unwrap(),
            PrepareFullTerminalDisposition::Prepared
        );
        assert_eq!(
            db.prepare_full_terminal(&artifact).unwrap(),
            PrepareFullTerminalDisposition::AlreadyPrepared
        );

        let request = TerminalBootstrapRequest {
            key: snapshot.key.clone(),
            player_token: "player-0".to_string(),
            request_id: "lost-first-frame".to_string(),
        };
        let first = db.bootstrap_terminal_delivery(&request).unwrap().unwrap();
        let retry = db.bootstrap_terminal_delivery(&request).unwrap().unwrap();
        assert_eq!(first.delivery_id, retry.delivery_id);
        assert_eq!(first.credential, retry.credential);
        assert_eq!(first.display.reason, "Match conceded");
        assert!(db.load_active_full_sessions().unwrap().is_empty());

        let read = db.read_terminal_result(&first.credential).unwrap().unwrap();
        assert_eq!(read.delivery_id, first.delivery_id);
        assert!(db
            .ack_terminal_delivery(&first.delivery_id, &first.credential)
            .unwrap());
        // A lost acknowledgement response retries safely against the same row.
        assert!(db
            .ack_terminal_delivery(&first.delivery_id, &first.credential)
            .unwrap());
    }

    #[test]
    fn terminal_bootstrap_rejects_wrong_token_and_changed_artifact() {
        let db = test_db();
        let snapshot = full_snapshot("TERM02", 1, 1, None, true);
        db.save_full_session(&snapshot).unwrap();
        let artifact = FullTerminalArtifact {
            key: snapshot.key.clone(),
            terminal_revision: 2,
            display: TerminalMatchDisplay {
                winner: Some(PlayerId(0)),
                reason: "Game ended".to_string(),
                ranked_result: None,
            },
            recipients: vec![TerminalRecipient {
                player_id: PlayerId(0),
                pre_terminal_player_token: "player-0".to_string(),
            }],
        };
        db.prepare_full_terminal(&artifact).unwrap();
        assert!(db
            .bootstrap_terminal_delivery(&TerminalBootstrapRequest {
                key: snapshot.key.clone(),
                player_token: "wrong-token".to_string(),
                request_id: "wrong".to_string(),
            })
            .unwrap()
            .is_none());

        let mut changed = artifact.clone();
        changed.display.reason = "Changed replay".to_string();
        assert!(matches!(
            db.prepare_full_terminal(&changed),
            Err(rusqlite::Error::InvalidQuery)
        ));
    }

    #[test]
    fn ranked_result_retry_reuses_the_original_receipt() {
        let db = test_db();
        let initial = vec![
            RatingDelta {
                player_key: "alice".to_string(),
                game_code: "RANK01".to_string(),
                opponent_key: "bob".to_string(),
                won: true,
                rating_before: 1200,
                rating_after: 1212,
                rating_delta: 12,
            },
            RatingDelta {
                player_key: "bob".to_string(),
                game_code: "RANK01".to_string(),
                opponent_key: "alice".to_string(),
                won: false,
                rating_before: 1200,
                rating_after: 1188,
                rating_delta: -12,
            },
        ];
        assert_eq!(
            db.save_ranked_result_idempotent(&initial).unwrap()[0].rating_after,
            1212
        );

        let retry = vec![
            RatingDelta {
                rating_before: 1212,
                rating_after: 1224,
                rating_delta: 12,
                ..initial[0].clone()
            },
            RatingDelta {
                rating_before: 1188,
                rating_after: 1176,
                rating_delta: -12,
                ..initial[1].clone()
            },
        ];
        let receipt = db.save_ranked_result_idempotent(&retry).unwrap();
        assert_eq!(receipt, initial);
        assert_eq!(db.load_rating("alice").unwrap(), Some(1212));
        assert_eq!(db.load_rating("bob").unwrap(), Some(1188));
    }

    #[test]
    fn multiplayer_save_retains_every_game_session() {
        let db = test_db(); // SessionRetention::Multiplayer
        db.save_session("GAME_A", "a").unwrap();
        db.save_session("GAME_B", "b").unwrap();
        // Online mode keeps every concurrent game independently.
        assert_eq!(db.load_all().unwrap().len(), 2);
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
