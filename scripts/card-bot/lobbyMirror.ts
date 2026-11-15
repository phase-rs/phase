// Lobby mirror: posts each build's public web-lobby rooms in a Discord channel,
// edits a post as its seats change, and deletes it once the room leaves the
// lobby, so the channel shows only live rooms. Polls the build's lobby
// Worker `GET /games` (the listing the lobby's `LobbyUpdate` carries) rather
// than subscribing over WebSocket, which would count the bot as a player online.
// A listing is identified by (build, code, created_at): a reused code
// re-registers with a new created_at, so it gets a new post, while a listing
// that already has a row is never posted again.

import { Database } from "bun:sqlite";

import { type Build, BUILD_ENDPOINTS, BUILDS } from "./config";
import type { MessageApi } from "./discord";
import { type LobbyRoom, renderLobbyPost } from "./lobbyView";
import { type FetchFn, isFiniteNumber, isRecord, REQUEST_TIMEOUT_MS } from "./servers";

export const LOBBY_POLL_INTERVAL_MS = 30_000;

/** A room is first posted only once it has been listed this long, so a room
 *  that fills (or is abandoned) within seconds is never posted. */
export const LOBBY_SETTLE_MS = 30_000;

/** Minimum time a closed post's row is kept after it closes; a row whose listing is
 *  still in `/games` is kept regardless, so its room is never reposted. */
export const CLOSED_POST_RETENTION_MS = 24 * 60 * 60_000;

/** The `/games` fields the mirror reads (lobby-broker `LobbyGame`). */
export interface LobbyGameRow {
  game_code: string;
  host_name: string;
  /** Registration time, in seconds. */
  created_at: number;
  has_password: boolean;
  current_players: number;
  max_players: number;
  is_sandbox: boolean;
  format?: string | null;
  room_name?: string | null;
  /** Serialized only for a draft pod. */
  draft_metadata?: unknown;
}

function isNullableString(value: unknown): boolean {
  return value === undefined || value === null || typeof value === "string";
}

function isLobbyGameRow(row: unknown): row is LobbyGameRow {
  return (
    isRecord(row) &&
    typeof row.game_code === "string" &&
    typeof row.host_name === "string" &&
    isFiniteNumber(row.created_at) &&
    typeof row.has_password === "boolean" &&
    isFiniteNumber(row.current_players) &&
    isFiniteNumber(row.max_players) &&
    typeof row.is_sandbox === "boolean" &&
    isNullableString(row.format) &&
    isNullableString(row.room_name)
  );
}

/**
 * Reads a `GET /games` body (untrusted). `null` — the build is unknown — unless
 * it is an object with a `games` array: a Worker not yet redeployed with
 * `/games` answers its info document instead. Otherwise only well-formed rows
 * are kept.
 */
export function parseLobbyGames(body: unknown): LobbyGameRow[] | null {
  if (!isRecord(body) || !Array.isArray(body.games)) return null;
  return body.games.filter(isLobbyGameRow);
}

/** A build's listing, or `null` on any failure (timeout, network error, non-2xx
 *  status, unreadable body). Not logged here: the sync logs state changes. */
export async function fetchLobbyGames(fetchFn: FetchFn, lobbyHttp: string): Promise<LobbyGameRow[] | null> {
  try {
    const res = await fetchFn(`${lobbyHttp}/games`, { signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) });
    if (!res.ok) return null;
    return parseLobbyGames(await res.json());
  } catch {
    return null;
  }
}

function toRoom(row: LobbyGameRow): LobbyRoom {
  const roomName = row.room_name ?? null;
  return {
    code: row.game_code,
    createdAt: row.created_at,
    format: row.format ?? null,
    name: roomName ?? row.host_name,
    host: roomName === null ? null : row.host_name,
    current: row.current_players,
    max: row.max_players,
  };
}

/** Rooms the mirror shows: constructed tables anyone can join. Draft pods need
 *  their own rendering (their identity is the set and pod, not the format), and
 *  sandbox rooms are debug tables, not games looking for players. */
function eligible(row: LobbyGameRow): boolean {
  return !row.has_password && !row.is_sandbox && row.draft_metadata === undefined;
}

const SCHEMA = `
CREATE TABLE IF NOT EXISTS lobby_post (
  build      TEXT NOT NULL CHECK (build IN ('release','preview')),
  code       TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  message_id TEXT NOT NULL,
  shown      TEXT NOT NULL,
  closed_ms  INTEGER,
  PRIMARY KEY (build, code, created_at)
);
`;

/** A post not yet deleted or found gone. */
export interface OpenPost {
  messageId: string;
  /** What the post shows (its `shown` JSON). */
  room: LobbyRoom;
}

/** The mirror's posts: one row per posted listing. `shown` is the JSON of the
 *  `LobbyRoom` the post last rendered; `closed_ms` is set once the post is
 *  deleted (its room left the lobby) or found already gone. */
export class LobbyPostStore {
  private readonly db: Database;

  /** Opens (creating if needed) the database at `path` with LfgStore's options.
   *  Its own connection: statements are synchronous in one process. */
  constructor(path: string) {
    this.db = new Database(path, { create: true, strict: true });
    this.db.run("PRAGMA journal_mode = WAL");
    this.db.run(SCHEMA);
  }

  /** Whether the listing was ever posted (open or closed). */
  has(build: Build, code: string, createdAt: number): boolean {
    return (
      this.db
        .query("SELECT 1 FROM lobby_post WHERE build = $build AND code = $code AND created_at = $createdAt")
        .get({ build, code, createdAt }) !== null
    );
  }

  /** Records a new post of `room`. */
  posted(build: Build, room: LobbyRoom, messageId: string): void {
    this.db
      .query(
        `INSERT INTO lobby_post (build, code, created_at, message_id, shown, closed_ms)
         VALUES ($build, $code, $createdAt, $messageId, $shown, NULL)`,
      )
      .run({ build, code: room.code, createdAt: room.createdAt, messageId, shown: JSON.stringify(room) });
  }

  /** The build's posts not yet closed. */
  open(build: Build): OpenPost[] {
    return (
      this.db
        .query("SELECT message_id, shown FROM lobby_post WHERE build = $build AND closed_ms IS NULL")
        .all({ build }) as { message_id: string; shown: string }[]
    ).map((row) => ({ messageId: row.message_id, room: JSON.parse(row.shown) as LobbyRoom }));
  }

  /** The post was edited to show `room`. */
  reshown(build: Build, room: LobbyRoom): void {
    this.db
      .query(
        `UPDATE lobby_post SET shown = $shown
         WHERE build = $build AND code = $code AND created_at = $createdAt`,
      )
      .run({ build, code: room.code, createdAt: room.createdAt, shown: JSON.stringify(room) });
  }

  /** The post was deleted, or is gone; it is not touched again. */
  close(build: Build, code: string, createdAt: number, now: number): void {
    this.db
      .query(
        `UPDATE lobby_post SET closed_ms = $now
         WHERE build = $build AND code = $code AND created_at = $createdAt`,
      )
      .run({ build, code, createdAt, now });
  }

  /** Deletes the build's rows closed more than CLOSED_POST_RETENTION_MS ago,
   *  except those whose listing is still in `listed` (this pass's `/games`): a
   *  moderator-deleted post of a room still in the lobby keeps its row, so the
   *  listing is never reposted. */
  prune(build: Build, listed: readonly LobbyGameRow[], now: number): void {
    this.db
      .query(
        `DELETE FROM lobby_post
         WHERE build = $build AND closed_ms < $cutoff
           AND NOT EXISTS (
             SELECT 1 FROM json_each($listed)
             WHERE json_extract(value, '$[0]') = code AND json_extract(value, '$[1]') = created_at
           )`,
      )
      .run({
        build,
        cutoff: now - CLOSED_POST_RETENTION_MS,
        listed: JSON.stringify(listed.map((row) => [row.game_code, row.created_at])),
      });
  }
}

export interface LobbyMirrorDeps {
  posts: LobbyPostStore;
  messages: MessageApi;
  channelId: string;
  fetchFn: FetchFn;
  /** Whether each build's `/games` was readable on the previous pass (absent:
   *  not yet polled). Owned by the caller across passes, so a build that stays
   *  unreadable — the release Worker until its next redeploy — is logged once,
   *  not every pass. */
  readable: Map<Build, boolean>;
  now: () => number;
}

/** One mirror pass: new posts, then edits or removal of the open ones, then
 *  pruning. Builds are independent; an unreadable build's posts and rows are
 *  left unchanged. */
export async function syncLobbyPosts(deps: LobbyMirrorDeps): Promise<void> {
  for (const build of BUILDS) await syncBuild(deps, build);
}

async function syncBuild(deps: LobbyMirrorDeps, build: Build): Promise<void> {
  const rows = await fetchLobbyGames(deps.fetchFn, BUILD_ENDPOINTS[build].lobbyHttp);
  const wasReadable = deps.readable.get(build);
  deps.readable.set(build, rows !== null);
  if (rows === null) {
    if (wasReadable !== false) console.error(`[lobby-mirror] ${build} /games unreadable; its posts are left as they are`);
    return;
  }
  if (wasReadable === false) console.log(`[lobby-mirror] ${build} /games readable again`);

  for (const row of rows) {
    if (
      !eligible(row) ||
      row.current_players >= row.max_players ||
      deps.now() - row.created_at * 1000 < LOBBY_SETTLE_MS ||
      deps.posts.has(build, row.game_code, row.created_at)
    ) {
      continue;
    }
    const room = toRoom(row);
    // ≤ 25 chars (Discord's nonce limit): a 6-char code and a 10-digit time.
    const nonce = `${build[0]}${row.game_code}${row.created_at}`;
    try {
      const messageId = await deps.messages.create(deps.channelId, renderLobbyPost(build, room), nonce);
      deps.posts.posted(build, room, messageId);
      console.log(`[lobby-mirror] posted ${build} room ${row.game_code}`);
    } catch (err) {
      // No row is written, so the next pass retries; the nonce dedupes a create
      // that reached Discord but whose response was lost.
      console.error(`[lobby-mirror] posting ${build} room ${row.game_code} failed:`, err);
    }
  }

  for (const { messageId, room: shown } of deps.posts.open(build)) {
    const row = rows.find((r) => eligible(r) && r.game_code === shown.code && r.created_at === shown.createdAt);
    try {
      if (row === undefined) {
        // The room left the lobby: remove its post (one already deleted counts).
        await deps.messages.delete(deps.channelId, messageId);
        deps.posts.close(build, shown.code, shown.createdAt, deps.now());
        console.log(`[lobby-mirror] removed ${build} room ${shown.code}'s post`);
        continue;
      }
      const room = toRoom(row);
      if (JSON.stringify(room) === JSON.stringify(shown)) continue;
      if ((await deps.messages.edit(deps.channelId, messageId, renderLobbyPost(build, room))) === "gone") {
        // A moderator deleted the post: closing its row keeps the listing from
        // being reposted.
        deps.posts.close(build, room.code, room.createdAt, deps.now());
        console.log(`[lobby-mirror] ${build} room ${room.code}'s post is gone; not reposting`);
      } else {
        deps.posts.reshown(build, room);
      }
    } catch (err) {
      console.error(`[lobby-mirror] updating ${build} room ${shown.code}'s post failed; retrying on the next pass:`, err);
    }
  }

  deps.posts.prune(build, rows, deps.now());
}
