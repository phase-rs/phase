// /lfg state machine over bun:sqlite. One row per LFG post plus its seats. Every
// mutating method is a single `db.transaction` with no `await` inside, so a click
// is all-or-nothing and — Bun being single-threaded — clicks are serialized.
//
// The room code is the join capability: it is minted once, inside the `ready`
// transition, and is never logged.

import { Database } from "bun:sqlite";

import type { Build } from "./config";
import { findFormat, type LfgFormat, type LfgMode } from "./formats";

/** An open LFG idle for longer than this closes on its next click. Ready LFGs
 *  never idle-expire; they live until the 24 h sweep. */
export const LFG_IDLE_MS = 30 * 60_000;

/** Any row untouched for this long is deleted (bounded growth, no timer). */
const SWEEP_AFTER_MS = 24 * 60 * 60_000;

/** A game thread still open this long after its LFG became ready is ended by
 *  the bot's timer. Well inside SWEEP_AFTER_MS, so the row still exists then. */
export const GAME_THREAD_MAX_MS = 6 * 60 * 60_000;

const CODE_LENGTH = 6;
const CODE_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
/** Largest multiple of 36 that fits a byte (7 · 36); bytes at or above it are
 *  rejected so every symbol is equally likely. */
const CODE_BYTE_LIMIT = Math.floor(256 / CODE_ALPHABET.length) * CODE_ALPHABET.length;

export type LfgState = "open" | "ready" | "cancelled" | "expired";

export interface Lfg {
  id: string;
  guildId: string;
  creatorId: string;
  format: LfgFormat;
  seats: number;
  mode: LfgMode;
  build: Build;
  server: { url: string; name: string } | null;
  state: LfgState;
  code: string | null;
  touchedMs: number;
  /** Seated user ids in join order; the creator is first. */
  seated: string[];
  /** The ready game's private thread, once the bot has opened one. `ended`: a
   *  player pressed End game, or the thread timed out or failed to set up; the
   *  bot then closes it in Discord, retrying until that succeeds or Discord
   *  refuses (a 4xx). */
  thread: { id: string; ended: boolean } | null;
}

export interface NewLfg {
  guildId: string;
  creatorId: string;
  format: LfgFormat;
  seats: number;
  mode: LfgMode;
  build: Build;
  server: { url: string; name: string } | null;
}

export type Refusal =
  | "has_open"
  | "already_seated"
  | "not_seated"
  | "not_creator"
  | "too_few"
  | "not_open"
  | "not_ready";

export type Outcome =
  /** The post changed (→ UPDATE_MESSAGE). */
  | { kind: "changed"; lfg: Lfg; becameReady: boolean }
  /** Expired now, already closed, or gone (`null`) (→ UPDATE_MESSAGE, closed). */
  | { kind: "ended"; lfg: Lfg | null }
  /** The click is not allowed for this user (→ ephemeral reply). */
  | { kind: "refused"; reason: Refusal; lfg: Lfg }
  /** `linkFor` only: a seated user of a ready LFG (→ ephemeral link reply). */
  | { kind: "ready"; lfg: Lfg };

/** `endGame`'s result (the thread's End game button). */
export type EndResult =
  /** The game is now ended (→ close the thread in Discord). */
  | { kind: "ending"; threadId: string }
  | { kind: "refused"; reason: "not_seated"; lfg: Lfg }
  /** Already ended, or the LFG or its thread is gone. */
  | { kind: "ended" };

/** A game thread the bot still has to close in Discord. `timedOut`: nothing has
 *  ended it yet, it is just past GAME_THREAD_MAX_MS (→ post the notice once). */
export interface ThreadToClose {
  id: string;
  threadId: string;
  timedOut: boolean;
}

export type CreateResult =
  | { kind: "created"; lfg: Lfg }
  | { kind: "refused"; reason: "has_open" };

const SCHEMA = `
CREATE TABLE IF NOT EXISTS lfg (
  id          TEXT PRIMARY KEY,
  guild_id    TEXT NOT NULL,
  creator_id  TEXT NOT NULL,
  format      TEXT NOT NULL,
  seats       INTEGER NOT NULL,
  mode        TEXT NOT NULL CHECK (mode IN ('p2p','server')),
  build       TEXT NOT NULL CHECK (build IN ('release','preview')),
  server_url  TEXT,
  server_name TEXT,
  state       TEXT NOT NULL CHECK (state IN ('open','ready','cancelled','expired')),
  code        TEXT,
  touched_ms  INTEGER NOT NULL,
  CHECK ((mode = 'server') = (server_url IS NOT NULL)),
  CHECK ((state = 'ready') = (code IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS lfg_one_open ON lfg (guild_id, creator_id) WHERE state = 'open';
CREATE TABLE IF NOT EXISTS lfg_seat (
  lfg_id    TEXT NOT NULL REFERENCES lfg (id) ON DELETE CASCADE,
  user_id   TEXT NOT NULL,
  joined_ms INTEGER NOT NULL,
  PRIMARY KEY (lfg_id, user_id)
);
`;

interface LfgRow {
  id: string;
  guild_id: string;
  creator_id: string;
  format: string;
  seats: number;
  mode: LfgMode;
  build: Build;
  server_url: string | null;
  server_name: string | null;
  state: LfgState;
  code: string | null;
  touched_ms: number;
  thread_id: string | null;
  thread_end_ms: number | null;
}

/** Columns added after the first release, so an existing database gains them in
 *  place (CREATE TABLE IF NOT EXISTS never alters a table that exists). */
const ADDED_COLUMNS: readonly { name: string; type: string }[] = [
  { name: "thread_id", type: "TEXT" },
  /** When the game was ended (End game, time-out, or a failed setup). */
  { name: "thread_end_ms", type: "INTEGER" },
  /** When the bot stopped trying to close the thread: Discord closed it, or
   *  refused with a 4xx. */
  { name: "thread_closed_ms", type: "INTEGER" },
];

/**
 * A 6-symbol `[A-Z0-9]` room code. Bytes ≥ 252 are rejected (rejection sampling)
 * so `byte % 36` is uniform. `fill` is the byte source (injectable for tests).
 */
export function mintGameCode(
  fill: (bytes: Uint8Array) => void = (bytes) => void crypto.getRandomValues(bytes),
): string {
  const bytes = new Uint8Array(CODE_LENGTH);
  let code = "";
  while (code.length < CODE_LENGTH) {
    fill(bytes);
    for (const byte of bytes) {
      if (byte < CODE_BYTE_LIMIT && code.length < CODE_LENGTH) {
        code += CODE_ALPHABET[byte % CODE_ALPHABET.length];
      }
    }
  }
  return code;
}

export class LfgStore {
  private readonly db: Database;

  /** Opens (creating if needed) the database at `path` with the production
   *  options, so tests (":memory:") bind parameters exactly as production does.
   *  `mintCode` is injectable only so a test can fail the ready transition. */
  constructor(
    path: string,
    private readonly mintCode: () => string = mintGameCode,
  ) {
    this.db = new Database(path, { create: true, strict: true });
    this.db.run("PRAGMA journal_mode = WAL");
    this.db.run("PRAGMA foreign_keys = ON");
    this.db.run(SCHEMA);
    const existing = new Set(
      (this.db.query("PRAGMA table_info(lfg)").all() as { name: string }[]).map((c) => c.name),
    );
    for (const column of ADDED_COLUMNS) {
      if (!existing.has(column.name)) this.db.run(`ALTER TABLE lfg ADD COLUMN ${column.name} ${column.type}`);
    }
  }

  create(input: NewLfg, now: number): CreateResult {
    return this.db.transaction((): CreateResult => {
      this.sweep(now);
      // Lazily expire the creator's own stale open post, freeing the one-open index.
      this.db
        .query(
          `UPDATE lfg SET state = 'expired'
           WHERE guild_id = $guildId AND creator_id = $creatorId AND state = 'open'
             AND touched_ms < $cutoff`,
        )
        .run({ guildId: input.guildId, creatorId: input.creatorId, cutoff: now - LFG_IDLE_MS });
      const open = this.db
        .query(
          `SELECT 1 FROM lfg WHERE guild_id = $guildId AND creator_id = $creatorId AND state = 'open'`,
        )
        .get({ guildId: input.guildId, creatorId: input.creatorId });
      if (open) return { kind: "refused", reason: "has_open" };

      const id = crypto.randomUUID();
      this.db
        .query(
          `INSERT INTO lfg (id, guild_id, creator_id, format, seats, mode, build,
                            server_url, server_name, state, code, touched_ms)
           VALUES ($id, $guildId, $creatorId, $format, $seats, $mode, $build,
                   $serverUrl, $serverName, 'open', NULL, $now)`,
        )
        .run({
          id,
          guildId: input.guildId,
          creatorId: input.creatorId,
          format: input.format.format,
          seats: input.seats,
          mode: input.mode,
          build: input.build,
          serverUrl: input.server?.url ?? null,
          serverName: input.server?.name ?? null,
          now,
        });
      this.insertSeat(id, input.creatorId, now);
      return { kind: "created", lfg: this.mustLoad(id) };
    })();
  }

  join(id: string, guildId: string, userId: string, now: number): Outcome {
    return this.act(id, guildId, now, (lfg) => {
      if (lfg.state !== "open") return { kind: "refused", reason: "not_open", lfg };
      if (lfg.seated.includes(userId)) return { kind: "refused", reason: "already_seated", lfg };
      this.insertSeat(id, userId, now);
      this.touch(id, now);
      if (lfg.seated.length + 1 === lfg.seats) return this.becomeReady(id, now);
      return { kind: "changed", lfg: this.mustLoad(id), becameReady: false };
    });
  }

  leave(id: string, guildId: string, userId: string, now: number): Outcome {
    return this.act(id, guildId, now, (lfg) => {
      if (lfg.state !== "open") return { kind: "refused", reason: "not_open", lfg };
      if (!lfg.seated.includes(userId)) return { kind: "refused", reason: "not_seated", lfg };
      if (userId === lfg.creatorId) {
        this.setState(id, "cancelled", now);
      } else {
        this.db
          .query("DELETE FROM lfg_seat WHERE lfg_id = $id AND user_id = $userId")
          .run({ id, userId });
        this.touch(id, now);
      }
      return { kind: "changed", lfg: this.mustLoad(id), becameReady: false };
    });
  }

  start(id: string, guildId: string, userId: string, now: number): Outcome {
    return this.act(id, guildId, now, (lfg) => {
      if (lfg.state !== "open") return { kind: "refused", reason: "not_open", lfg };
      if (userId !== lfg.creatorId) return { kind: "refused", reason: "not_creator", lfg };
      if (lfg.seated.length < lfg.format.min_players) {
        return { kind: "refused", reason: "too_few", lfg };
      }
      return this.becomeReady(id, now);
    });
  }

  /** A seated user's link request. Never changes the post and never touches the
   *  row, so a ready LFG's links stay available until the 24 h sweep. */
  linkFor(id: string, guildId: string, userId: string, now: number): Outcome {
    return this.act(id, guildId, now, (lfg) => {
      if (lfg.state !== "ready") return { kind: "refused", reason: "not_ready", lfg };
      if (!lfg.seated.includes(userId)) return { kind: "refused", reason: "not_seated", lfg };
      return { kind: "ready", lfg };
    });
  }

  /** Records the private thread opened for a ready LFG (whose `becameReady`
   *  click just returned, so the row is ready and has no thread yet). */
  attachThread(id: string, threadId: string): void {
    this.db.query("UPDATE lfg SET thread_id = $threadId WHERE id = $id").run({ id, threadId });
  }

  /** A seated player's End game click in the game thread. */
  endGame(id: string, guildId: string, userId: string, now: number): EndResult {
    return this.db.transaction((): EndResult => {
      const lfg = this.loadForClick(id, guildId, now);
      if (lfg === null || lfg.thread === null || lfg.thread.ended) return { kind: "ended" };
      if (!lfg.seated.includes(userId)) return { kind: "refused", reason: "not_seated", lfg };
      this.endThread(id, now);
      return { kind: "ending", threadId: lfg.thread.id };
    })();
  }

  /** Marks the game's thread ended, so the timer closes it whatever its age. */
  endThread(id: string, now: number): void {
    this.db
      .query("UPDATE lfg SET thread_end_ms = $now WHERE id = $id AND thread_end_ms IS NULL")
      .run({ id, now });
  }

  /** Threads not yet closed in Discord that are ended, or that became ready at
   *  least GAME_THREAD_MAX_MS ago. A ready row's `touched_ms` is its ready time:
   *  nothing touches it afterwards. */
  threadsToClose(now: number): ThreadToClose[] {
    return (
      this.db
        .query(
          `SELECT id, thread_id, thread_end_ms FROM lfg
           WHERE thread_id IS NOT NULL AND thread_closed_ms IS NULL
             AND (thread_end_ms IS NOT NULL OR touched_ms <= $cutoff)`,
        )
        .all({ cutoff: now - GAME_THREAD_MAX_MS }) as { id: string; thread_id: string; thread_end_ms: number | null }[]
    ).map((row) => ({ id: row.id, threadId: row.thread_id, timedOut: row.thread_end_ms === null }));
  }

  /** Discord closed the thread (or it cannot be closed); the timer stops trying. */
  markThreadClosed(id: string, now: number): void {
    this.db.query("UPDATE lfg SET thread_closed_ms = $now WHERE id = $id").run({ id, now });
  }

  /** Every click's first step: sweep, then load the LFG, or null when it is
   *  gone or belongs to another guild (a custom_id is only honoured in its own
   *  guild). Runs inside the caller's transaction. */
  private loadForClick(id: string, guildId: string, now: number): Lfg | null {
    this.sweep(now);
    const lfg = this.load(id);
    return lfg === null || lfg.guildId !== guildId ? null : lfg;
  }

  /** Shared prologue of every post click: `loadForClick`, lazy expiry of an idle
   *  open post, closed posts → `ended`; then the action, all in one transaction. */
  private act(
    id: string,
    guildId: string,
    now: number,
    action: (lfg: Lfg) => Outcome,
  ): Outcome {
    return this.db.transaction((): Outcome => {
      const lfg = this.loadForClick(id, guildId, now);
      if (lfg === null) return { kind: "ended", lfg: null };
      if (lfg.state === "open" && now - lfg.touchedMs > LFG_IDLE_MS) {
        this.db.query("UPDATE lfg SET state = 'expired' WHERE id = $id").run({ id });
        return { kind: "ended", lfg: this.mustLoad(id) };
      }
      if (lfg.state === "cancelled" || lfg.state === "expired") return { kind: "ended", lfg };
      return action(lfg);
    })();
  }

  private becomeReady(id: string, now: number): Outcome {
    this.db
      .query("UPDATE lfg SET state = 'ready', code = $code, touched_ms = $now WHERE id = $id")
      .run({ id, code: this.mintCode(), now });
    return { kind: "changed", lfg: this.mustLoad(id), becameReady: true };
  }

  private sweep(now: number): void {
    this.db.query("DELETE FROM lfg WHERE touched_ms < $cutoff").run({ cutoff: now - SWEEP_AFTER_MS });
  }

  private insertSeat(id: string, userId: string, now: number): void {
    this.db
      .query("INSERT INTO lfg_seat (lfg_id, user_id, joined_ms) VALUES ($id, $userId, $now)")
      .run({ id, userId, now });
  }

  private touch(id: string, now: number): void {
    this.db.query("UPDATE lfg SET touched_ms = $now WHERE id = $id").run({ id, now });
  }

  private setState(id: string, state: LfgState, now: number): void {
    this.db
      .query("UPDATE lfg SET state = $state, touched_ms = $now WHERE id = $id")
      .run({ id, state, now });
  }

  /** The LFG, or `null` when it is gone — or when its format key disappeared
   *  from FORMATS after an upgrade, which callers treat the same way (ended). */
  private load(id: string): Lfg | null {
    const row = this.db.query("SELECT * FROM lfg WHERE id = $id").get({ id }) as LfgRow | null;
    if (row === null) return null;
    const format = findFormat(row.format);
    if (format === undefined) return null;
    const seated = (
      this.db
        .query("SELECT user_id FROM lfg_seat WHERE lfg_id = $id ORDER BY joined_ms, rowid")
        .all({ id }) as { user_id: string }[]
    ).map((seat) => seat.user_id);
    return {
      id: row.id,
      guildId: row.guild_id,
      creatorId: row.creator_id,
      format,
      seats: row.seats,
      mode: row.mode,
      build: row.build,
      server:
        row.server_url === null ? null : { url: row.server_url, name: row.server_name ?? row.server_url },
      state: row.state,
      code: row.code,
      touchedMs: row.touched_ms,
      seated,
      thread: row.thread_id === null ? null : { id: row.thread_id, ended: row.thread_end_ms !== null },
    };
  }

  /** `load` for a row this transaction just wrote (it cannot be missing). */
  private mustLoad(id: string): Lfg {
    const lfg = this.load(id);
    if (lfg === null) throw new Error(`lfg ${id} vanished inside its own transaction`);
    return lfg;
  }
}
