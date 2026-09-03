// Durable Object shell for the official phase.rs lobby broker.
//
// This is a THIN imperative shell around the compiled Rust `lobby-broker` core
// (lobby-worker/broker-wasm -> broker-wasm-pkg). All protocol parsing, dispatch,
// reservations, capacity caps, build-commit gating and staleness reaping live in
// Rust — the SAME code the native phase-server runs (extracted in Phase A), so
// the two deployments behave identically by construction. The shell only:
//   - owns the WebSocket Hibernation lifecycle + DO storage,
//   - forwards raw frames into `WasmBroker.handle`,
//   - interprets the returned `Outbound` side effects over its transport,
//   - snapshots the broker to storage after mutations (a hibernated DO loses
//     in-memory state), and
//   - drives the reaper from a DO alarm (no tokio interval in a Worker).
// Public-lobby name moderation is intentionally applied here, not in the shared
// Rust broker, so self-hosted native servers keep their own policy surface.
//
// Mirrors the engine -> engine-wasm -> React-adapter pattern: the WASM owns the
// logic, the host language is a serialization boundary with zero game logic.

import wasmModule from "../broker-wasm-pkg/broker_bg.wasm";
import {
  directory_compare_announcement_to_info,
  directory_info_url,
  directory_normalize_url,
  directory_rtt_bucket_edges_ms,
  directory_score,
  directory_score_bucket_ms,
  directory_score_window_ms,
  directory_validate_announcement,
  directory_version,
  initSync,
  lobby_protocol_version,
  min_supported_lobby_protocol,
  protocol_version,
  WasmBroker,
} from "../broker-wasm-pkg/broker.js";
import {
  buildDirectoryResponse,
  DIRECTORY_READ_CORS,
  DIRECTORY_WRITE_CORS,
  foldMetricReports,
  MAX_ANNOUNCE_BYTES,
  MAX_INFO_BYTES,
  MAX_METRICS_BYTES,
  partitionServerRows,
  planAnnounceUpsert,
  readBoundedText,
  recordAnnouncedPlayers,
  sanitizeMetricsBatch,
  serverProbeEvents,
  shouldKeepAlarm,
  type ComparisonVerdict,
  type ServerCounters,
  type StoredServerRow,
  type ValidatedAnnouncement,
  type WireScore,
} from "./directory";
import { toDataPoint } from "./telemetry";
import {
  classifyHelloGate,
  helloGateErrorMessage,
  type ConnAttachment,
  type LobbyHelloPolicy,
} from "./hello-gate";
import { moderationErrorForLobbyFrame } from "./name-filter";
import {
  buildStatsPayload,
  countGameOutbounds,
  dayBucket,
  DAILY_PREFIX,
  DAILY_SERIES_LIMIT,
  GAMES_CREATED_KEY,
  GAMES_JOINED_KEY,
  PLAYERS_PEAK_KEY,
  type DailyStat,
} from "./stats";

// Instantiate the broker WASM once per isolate, at top level (CF imports `.wasm`
// as a WebAssembly.Module; `initSync` wires the wasm-bindgen imports
// synchronously). Doing this here — not per request — avoids re-instantiation.
initSync({ module: wasmModule });

const PROTOCOL_VERSION = protocol_version();
// The lobby's OWN message-set version. Independent of PROTOCOL_VERSION, which
// tracks the full-game GameState/GameAction surface this broker never parses.
const LOBBY_PROTOCOL_VERSION = lobby_protocol_version();
const HELLO_POLICY: LobbyHelloPolicy = {
  serverProtocolVersion: PROTOCOL_VERSION,
  lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
  minSupportedLobbyProtocol: min_supported_lobby_protocol(),
};
const SERVER_VERSION = "lobby-rs";
// build_commit is cosmetic for a LobbyOnly broker — the gameplay-relevant gate
// is each room's host_build_commit (enforced inside the Rust core), not the
// broker's own build.
const SERVER_BUILD_COMMIT = "lobby-rs";

// Staleness reaper. `REAP_TIMEOUT_SECONDS` mirrors the native phase-server
// (`broker.reap_expired(300, …)`). The DO alarm interval is coarser than the
// native 10s tokio tick because each alarm wakes the (otherwise hibernating) DO:
// 60s reaps a stale entry within a minute of the 300s threshold while still
// letting a fully idle lobby hibernate (the alarm stops rescheduling when empty).
const REAP_TIMEOUT_SECONDS = 300;
const REAP_INTERVAL_MS = 60_000;

// ── Server directory ───────────────────────────────────────────────────────
//
// Announcement shape version, counter bucket width, decay window and RTT
// histogram edges, all read ONCE from the wasm exports at module scope. None
// of the four is a TypeScript literal anywhere in this Worker: Rust is the
// single authority, and a restated copy would mis-gate announcements, mis-cut
// buckets, mis-age evidence or mis-file latencies without failing a test.
const DIRECTORY_VERSION = directory_version();
const SCORE_BUCKET_MS = directory_score_bucket_ms();
const SCORE_WINDOW_MS = directory_score_window_ms();
const RTT_BUCKET_EDGES_MS = Array.from(directory_rtt_bucket_edges_ms());

/** Per-server counter blobs, one JSON object per key. Key-value rather than a
 *  second SQL table because they are folded and read whole, never queried by
 *  column — the same access pattern the daily stats buckets have. */
const COUNTERS_PREFIX = "directory:counters:";

/** Ceiling on the verification fetch, connection through last byte. One
 *  `AbortSignal.timeout` covers both halves: the abort tears down the response
 *  body stream as well as the connection, so a server that trickles bytes
 *  throws inside the reader rather than hanging. */
const INFO_FETCH_TIMEOUT_MS = 3_000;

/** `SqlStorageCursor` constrains its row type to `Record<string,
 *  SqlStorageValue>`. The intersection supplies that index signature at the
 *  cursor without putting one on `StoredServerRow`, which is a nine-column
 *  contract and must not become "any string key". */
type SqlRow<T> = T & Record<string, SqlStorageValue>;

/** Boundary mirror of `lobby_broker_wasm::ValidationDto`. */
type ValidationVerdict =
  | { kind: "Valid"; announcement: ValidatedAnnouncement }
  | { kind: "Invalid"; error: string };

/** Outcome of the verify-by-fetch. Every transport failure — DNS, TLS, a
 *  connect timeout, a mid-body abort, a non-2xx, a body-less response — maps
 *  to `unreachable`, because from the directory's point of view they are one
 *  fact: no usable document came back. */
type InfoFetch = { kind: "ok"; text: string } | { kind: "unreachable" } | { kind: "too_large" };

/** A refusal from a directory write endpoint.
 *
 *  The `reason` is a typed token, never prose: an announcer parses it, and a
 *  message a test could pin would make the wording part of the contract. The
 *  mismatch reasons carry the disagreeing field, which is what tells an
 *  operator whether their announcement names the wrong server or their `/info`
 *  is being served stale. */
function directoryError(status: number, reason: string): Response {
  return Response.json({ error: reason }, { status, headers: DIRECTORY_WRITE_CORS });
}

/** Bindings the Durable Object reads. Declared here and inherited by the
 *  Worker entry's `Env`, so the binding list has one home.
 *
 *  Both are optional, and they fail in OPPOSITE directions on purpose:
 *  without `TELEMETRY` the Analytics Engine mirror no-ops (fail-open, as it
 *  already did for client telemetry), while without `SERVER_ALLOWLIST` the
 *  directory lists nobody (fail-closed — an unconfigured directory must not
 *  publish an unvetted list of everyone who announced). */
export interface LobbyDoEnv {
  /** Directory allowlist. One key per canonical server URL; the VALUE is an
   *  operator-facing note the Worker never reads — membership is the whole
   *  contract, which is what keeps the allowlist a set rather than a second
   *  unvalidated schema. */
  SERVER_ALLOWLIST?: KVNamespace;
  /** Analytics Engine mirror for accepted `server_probe` reports. */
  TELEMETRY?: AnalyticsEngineDataset;
}

/// Per-socket state, mirroring `lobby_broker::ConnState::default()`. Stored in
/// the WebSocket attachment as a structured object; stringified across the WASM
/// boundary and written back from each call's result.
const DEFAULT_CONN: ConnAttachment = {
  client_hello: null,
  subscribed: false,
  host_game: null,
  reservations: [],
  organized_tournaments: [],
  joined_tournaments: [],
};

/** Boundary mirror of `lobby_broker_wasm::OutboundDto`. */
interface OutboundDto {
  kind: "ToSelf" | "ToSubscribers" | "AddSubscriber" | "RemoveSubscriber" | "SendPlayerCountToSelf";
  msg?: unknown;
}

/** Boundary mirror of `lobby_broker_wasm::CallResult`. */
interface CallResult {
  conn: unknown;
  outbounds: OutboundDto[];
  dirty: boolean;
  reject?: string;
}

const SNAPSHOT_KEY = "broker_snapshot";

// ── Usage analytics (durable, DO-storage-backed) ────────────────────────────
// The single global DO is a globally-consistent ledger, so a handful of KV
// counters here ARE the all-time totals — no fan-in across instances needed.
// Only monotonic facts are persisted; live gauges (players online, active
// games) are computed on read from the socket set + broker, never stored, so a
// hibernation-missed decrement can't drift them. The storage keys, stored
// shapes, and the pure folds/derivations live in `./stats`; this shell owns
// only the `ctx.storage` I/O and Response construction.

export class LobbyDO {
  private ctx: DurableObjectState;
  private env: LobbyDoEnv;
  /** In-memory broker, restored from the DO-storage snapshot on first use after
   *  a cold start / hibernation wake. */
  private broker: WasmBroker | null = null;
  /** Whether this isolate has run the directory DDL yet. */
  private serversTableReady = false;

  constructor(ctx: DurableObjectState, env: LobbyDoEnv) {
    this.ctx = ctx;
    this.env = env;
  }

  private async loadBroker(): Promise<WasmBroker> {
    if (!this.broker) {
      const snap = await this.ctx.storage.get<string>(SNAPSHOT_KEY);
      this.broker = snap ? WasmBroker.from_snapshot(snap) : new WasmBroker();
    }
    return this.broker;
  }

  // ── HTTP / WS entry ────────────────────────────────────────────────────

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade") !== "websocket") {
      const pathname = new URL(request.url).pathname;
      // Public usage/analytics snapshot (read by the in-app lobby stats panel).
      if (pathname === "/stats") {
        return this.statsResponse();
      }
      // Server directory. These MUST be branched explicitly: this method's
      // default answer for any non-WebSocket path is the info document, so a
      // directory path that fell through would be answered with a version
      // blob and a 200.
      if (pathname === "/servers") {
        if (request.method === "OPTIONS") {
          return new Response(null, { status: 204, headers: DIRECTORY_READ_CORS });
        }
        return this.serversResponse();
      }
      if (pathname === "/servers/announce") {
        if (request.method === "OPTIONS") {
          return new Response(null, { status: 204, headers: DIRECTORY_WRITE_CORS });
        }
        return this.announceResponse(request);
      }
      if (pathname === "/servers/metrics") {
        if (request.method === "OPTIONS") {
          return new Response(null, { status: 204, headers: DIRECTORY_WRITE_CORS });
        }
        return this.metricsResponse(request);
      }
      // Plain GET → version/health endpoint (deploy smoke check asserts
      // protocol_version == released client's).
      return Response.json({
        mode: "LobbyOnly",
        protocol_version: PROTOCOL_VERSION,
        lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
        server_version: SERVER_VERSION,
      });
    }

    const { 0: client, 1: server } = new WebSocketPair();
    // Hibernation API: the runtime owns the socket and wakes the DO via the
    // webSocket* handlers, so an idle lobby incurs no duration charge.
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment(DEFAULT_CONN);
    this.sendHello(server);
    // A new connection changes the live player count for existing subscribers.
    this.broadcastPlayerCount();
    // Lobby occupancy heartbeat — once per connection (not per frame). Lets you
    // see usage and spot connection leaks (count that never returns to 0).
    const players = this.ctx.getWebSockets().length;
    console.log({ event: "lobby_connect", players });
    await this.recordPeakPlayers(players);
    return new Response(null, { status: 101, webSocket: client });
  }

  // ── WebSocket Hibernation handlers ─────────────────────────────────────

  async webSocketMessage(ws: WebSocket, raw: string | ArrayBuffer): Promise<void> {
    const broker = await this.loadBroker();
    const conn = ws.deserializeAttachment() ?? DEFAULT_CONN;
    const text = typeof raw === "string" ? raw : new TextDecoder().decode(raw);
    const moderationError = moderationErrorForLobbyFrame(text);
    if (moderationError) {
      ws.send(JSON.stringify({ type: "Error", data: { message: moderationError } }));
      console.warn({ event: "lobby_name_rejected" });
      return;
    }

    let frame: { type?: string; data?: Record<string, unknown> };
    try {
      frame = JSON.parse(text) as { type?: string; data?: Record<string, unknown> };
    } catch {
      console.warn({ event: "lobby_frame_rejected", reason: "invalid_json" });
      return;
    }

    const attachment = conn as ConnAttachment;
    const gate = classifyHelloGate(attachment.client_hello != null, frame, HELLO_POLICY);
    const gateError = helloGateErrorMessage(gate);
    if (gateError) {
      ws.send(JSON.stringify({ type: "Error", data: { message: gateError } }));
      console.warn({ event: "lobby_hello_gate_rejected", reason: gate.kind });
      return;
    }
    if (gate.kind === "ignore") {
      return;
    }

    const result = JSON.parse(broker.handle(JSON.stringify(conn), text, Date.now())) as CallResult;

    if (result.reject) {
      // Unknown tag / malformed frame — the Rust parser rejected it. No state
      // changed (attachment/snapshot untouched), but the broker attaches an
      // Error reply so the client's pending RPC fails fast instead of hanging
      // until its timeout. Deliver it, then log so it surfaces in Workers Logs.
      console.warn({ event: "lobby_frame_rejected", reason: result.reject });
      this.interpret(ws, result.outbounds);
      return;
    }

    ws.serializeAttachment(result.conn);
    this.interpret(ws, result.outbounds);

    if (result.dirty) {
      await this.ctx.storage.put(SNAPSHOT_KEY, broker.snapshot());
      await this.ensureAlarm();
    }

    // Best-effort usage counters — recorded AFTER the authoritative broker
    // snapshot so a storage fault here can never preempt persisting the lobby
    // entry (which the hibernation-recovery path depends on).
    await this.recordGameStats(result.outbounds);
  }

  async webSocketClose(ws: WebSocket): Promise<void> {
    const broker = await this.loadBroker();
    const conn = ws.deserializeAttachment() ?? DEFAULT_CONN;
    // Releases the connection's reservations + removes any hosted entry; emits
    // LobbyGameUpdated/Removed to the remaining subscribers (the closing socket
    // is already excluded from getWebSockets()).
    const result = JSON.parse(broker.on_disconnect(JSON.stringify(conn))) as CallResult;
    this.interpret(ws, result.outbounds);
    await this.ctx.storage.put(SNAPSHOT_KEY, broker.snapshot());
    // Player-count decrement+broadcast is shell-owned on close (the broker
    // cannot know the socket set). getWebSockets() already excludes the closing
    // socket, so this count reflects who remains.
    this.broadcastPlayerCount();
    console.log({ event: "lobby_disconnect", players: this.ctx.getWebSockets().length });
  }

  async webSocketError(ws: WebSocket): Promise<void> {
    // Distinguish abnormal closes (protocol/transport error) from clean ones —
    // teardown is identical, but a spike here points at a client/network fault.
    console.warn({ event: "lobby_ws_error" });
    await this.webSocketClose(ws);
  }

  // ── Staleness reaper (DO alarm) ────────────────────────────────────────

  async alarm(): Promise<void> {
    const broker = await this.loadBroker();
    const outbounds = JSON.parse(
      broker.reap_expired(REAP_TIMEOUT_SECONDS, Date.now()),
    ) as OutboundDto[];
    // Every reaper outbound is ToSubscribers — no connection scope — but the
    // PAYLOAD is no longer only LobbyGameRemoved: a sweep can also emit
    // TournamentRemoved / TournamentUpdate per expired tournament, plus one
    // trailing TournamentListUpdate (see `Broker::reap_expired`). This loop is
    // deliberately variant-agnostic — dispatchOutbound switches on the
    // Outbound's own `kind` alone and never on the LobbyServerMessage carried
    // inside it — so a new server message needs no change here.
    for (const o of outbounds) this.dispatchOutbound(null, o);
    // One log per non-empty sweep (≤ once/REAP_INTERVAL_MS). `count` is
    // outbounds emitted, which is no longer 1:1 with entries reaped: a
    // tournament sweep adds a single trailing TournamentListUpdate on top of
    // its per-entry messages.
    if (outbounds.length > 0) {
      console.log({ event: "lobby_reaped", count: outbounds.length });
    }
    await this.ctx.storage.put(SNAPSHOT_KEY, broker.snapshot());
    // The same sweep, for the server directory. Storage hygiene only: the
    // liveness AUTHORITY is the read path's filter, which applies the same
    // `isServerLive` predicate, so a row that expires between alarms is
    // already unlisted before it is deleted here.
    await this.reapServerRows();
    // Keep reaping while anything reapable remains — lobby entries OR
    // directory rows. An idle DO with neither stops rescheduling so it can
    // hibernate fully.
    if (shouldKeepAlarm(broker.is_empty(), this.serverRowCount())) {
      await this.ctx.storage.setAlarm(Date.now() + REAP_INTERVAL_MS);
    }
  }

  /** Delete rows that stopped announcing, and their counters with them: a
   *  counter blob for a server with no row can never be read again (the
   *  metrics origin guard keys on row existence) and would leak storage. */
  private async reapServerRows(): Promise<void> {
    this.ensureServersTable();
    const { expired } = partitionServerRows(this.serverRows(), Date.now());
    if (expired.length === 0) return;
    for (const row of expired) {
      this.ctx.storage.sql.exec("DELETE FROM servers WHERE url = ?", row.url);
      await this.ctx.storage.delete(`${COUNTERS_PREFIX}${row.url}`);
    }
    console.log({ event: "directory_reaped", count: expired.length });
  }

  // ── Usage analytics ────────────────────────────────────────────────────

  /** Fold GameCreated / PeerInfo outbounds into the durable totals + today's
   *  bucket. GameCreated = a room was hosted; PeerInfo = a guest was handed the
   *  host's peer id, i.e. a P2P match actually began. */
  private async recordGameStats(outbounds: OutboundDto[]): Promise<void> {
    const { created, joined } = countGameOutbounds(outbounds);
    if (created === 0 && joined === 0) return;

    const dailyKey = `${DAILY_PREFIX}${dayBucket(Date.now())}`;
    const [createdTotal, joinedTotal, daily] = await Promise.all([
      this.ctx.storage.get<number>(GAMES_CREATED_KEY),
      this.ctx.storage.get<number>(GAMES_JOINED_KEY),
      this.ctx.storage.get<DailyStat>(dailyKey),
    ]);
    const bucket = daily ?? { created: 0, joined: 0 };
    // Batched write. The DO input gate serializes handler invocations, so this
    // read-modify-write can't interleave with another frame's increment.
    await this.ctx.storage.put({
      [GAMES_CREATED_KEY]: (createdTotal ?? 0) + created,
      [GAMES_JOINED_KEY]: (joinedTotal ?? 0) + joined,
      [dailyKey]: { created: bucket.created + created, joined: bucket.joined + joined },
    });
  }

  /** Raise the persisted concurrent-players high-water mark if `players`
   *  exceeds it. Called on connect — the only moment the live count can rise. */
  private async recordPeakPlayers(players: number): Promise<void> {
    const peak = (await this.ctx.storage.get<number>(PLAYERS_PEAK_KEY)) ?? 0;
    if (players > peak) await this.ctx.storage.put(PLAYERS_PEAK_KEY, players);
  }

  /** Build the `/stats` JSON: live gauges from the socket set + broker, durable
   *  totals / peak / 30-day series from DO storage. Public, non-sensitive
   *  counts, so a permissive CORS header lets the browser read it cross-origin. */
  private async statsResponse(): Promise<Response> {
    const broker = await this.loadBroker();
    const [createdTotal, joinedTotal, peak, daily] = await Promise.all([
      this.ctx.storage.get<number>(GAMES_CREATED_KEY),
      this.ctx.storage.get<number>(GAMES_JOINED_KEY),
      this.ctx.storage.get<number>(PLAYERS_PEAK_KEY),
      this.ctx.storage.list<DailyStat>({
        prefix: DAILY_PREFIX,
        reverse: true,
        limit: DAILY_SERIES_LIMIT,
      }),
    ]);
    const payload = buildStatsPayload({
      playersOnline: this.ctx.getWebSockets().length,
      playersPeak: peak ?? 0,
      activeGames: broker.active_games(),
      gamesCreatedTotal: createdTotal ?? 0,
      gamesJoinedTotal: joinedTotal ?? 0,
      daily,
      nowMs: Date.now(),
    });
    return Response.json(payload, {
      headers: { "Access-Control-Allow-Origin": "*", "Cache-Control": "no-store" },
    });
  }

  // ── Server directory ───────────────────────────────────────────────────
  //
  // Every decision below is a call into `./directory`; this shell contributes
  // the storage, the KV read, the outbound fetch, the four wasm calls and the
  // `Response`s, and nothing else.

  /** Lazy DDL, once per isolate.
   *
   *  Deliberately NOT in the constructor: a `sql.exec` there would put a
   *  directory-only failure on the critical path of every WebSocket connect.
   *  Calling it at the top of each directory path bounds the blast radius to
   *  the directory endpoints, for the same reason both directory bindings are
   *  optional.
   *
   *  Column set is `StoredServerRow` (`./directory.ts`) — the shape
   *  `storedRowFromAnnouncement` builds — i.e. `DirectoryRow` minus `score`:
   *  the score is computed from the counters at read time, so a stored copy
   *  would be a staler second authority for the same number. */
  private ensureServersTable(): void {
    if (this.serversTableReady) return;
    this.ctx.storage.sql.exec(
      `CREATE TABLE IF NOT EXISTS servers (
         url                    TEXT    PRIMARY KEY,
         name                   TEXT    NOT NULL,
         mode                   TEXT    NOT NULL,
         server_version         TEXT    NOT NULL,
         protocol_version       INTEGER NOT NULL,
         lobby_protocol_version INTEGER NOT NULL,
         current_players        INTEGER NOT NULL,
         first_seen_ms          INTEGER NOT NULL,
         last_seen_ms           INTEGER NOT NULL
       )`,
    );
    this.serversTableReady = true;
  }

  /** The nine columns, typed once on the read side by the same names the
   *  upsert binds. */
  private serverRows(): StoredServerRow[] {
    return this.ctx.storage.sql.exec<SqlRow<StoredServerRow>>("SELECT * FROM servers").toArray();
  }

  private serverRowCount(): number {
    this.ensureServersTable();
    return this.ctx.storage.sql.exec<SqlRow<{ n: number }>>("SELECT COUNT(*) AS n FROM servers").one().n;
  }

  private hasServerRow(url: string): boolean {
    return (
      this.ctx.storage.sql
        .exec<SqlRow<{ n: number }>>("SELECT COUNT(*) AS n FROM servers WHERE url = ?", url)
        .one().n > 0
    );
  }

  /** One statement, no read-modify-write. `first_seen_ms` is preserved by
   *  construction — it is absent from the conflict clause, so a heartbeat
   *  cannot reset a server's age. */
  private upsertServerRow(row: StoredServerRow): void {
    this.ctx.storage.sql.exec(
      `INSERT INTO servers (url,name,mode,server_version,protocol_version,
                            lobby_protocol_version,current_players,first_seen_ms,last_seen_ms)
       VALUES (?,?,?,?,?,?,?,?,?)
       ON CONFLICT(url) DO UPDATE SET
         name=excluded.name, mode=excluded.mode, server_version=excluded.server_version,
         protocol_version=excluded.protocol_version,
         lobby_protocol_version=excluded.lobby_protocol_version,
         current_players=excluded.current_players, last_seen_ms=excluded.last_seen_ms`,
      row.url,
      row.name,
      row.mode,
      row.server_version,
      row.protocol_version,
      row.lobby_protocol_version,
      row.current_players,
      row.first_seen_ms,
      row.last_seen_ms,
    );
  }

  private async readCounters(url: string): Promise<ServerCounters> {
    return (
      (await this.ctx.storage.get<ServerCounters>(`${COUNTERS_PREFIX}${url}`)) ?? { buckets: [] }
    );
  }

  /** The allowlist as canonical URLs.
   *
   *  Each KV key goes through `directory_normalize_url` — the SAME Rust
   *  authority that produced the row keys — and a key that does not normalise
   *  is dropped. Without this an operator key of `wss://Host.Example:443/ws/`
   *  would match no row, list nothing, and report no error anywhere.
   *
   *  An absent binding returns an EMPTY set, which lists nothing. Fail-closed:
   *  an unconfigured directory must advertise nobody. */
  private async allowlist(): Promise<Set<string>> {
    const binding = this.env.SERVER_ALLOWLIST;
    const canonical = new Set<string>();
    if (!binding) return canonical;
    let cursor: string | undefined;
    for (;;) {
      const page = await binding.list({ cursor });
      for (const key of page.keys) {
        const normalized = directory_normalize_url(key.name);
        if (normalized) canonical.add(normalized);
      }
      if (page.list_complete) return canonical;
      cursor = page.cursor;
    }
  }

  /** `GET /servers` — live rows ∩ allowlist, each with its score. */
  private async serversResponse(): Promise<Response> {
    this.ensureServersTable();
    const nowMs = Date.now();
    const rows = this.serverRows();
    const [allowlist, stored] = await Promise.all([
      this.allowlist(),
      this.ctx.storage.list<ServerCounters>({ prefix: COUNTERS_PREFIX }),
    ]);

    // One score per row, computed live. `directory_score` answers the JSON
    // literal `null` for both "no evidence" and "unreadable counters", which
    // are the same thing to a listing.
    const scores = new Map<string, WireScore | null>();
    for (const row of rows) {
      const counters = stored.get(`${COUNTERS_PREFIX}${row.url}`);
      const score = counters
        ? (JSON.parse(directory_score(JSON.stringify(counters), nowMs)) as WireScore | null)
        : null;
      scores.set(row.url, score);
    }

    const { body, headers } = buildDirectoryResponse({
      rows,
      allowlist,
      scores,
      directoryVersion: DIRECTORY_VERSION,
      nowMs,
    });
    return Response.json(body, { headers });
  }

  /** `POST /servers/announce` — validate, verify by fetching the announced
   *  host's own info document, compare, upsert. */
  private async announceResponse(request: Request): Promise<Response> {
    this.ensureServersTable();
    // Bounded as it arrives, exactly as the outbound `/info` read is. Reading
    // the whole body first and measuring afterwards buffers an unbounded string
    // in the isolate, and `Content-Length` cannot be the bound: `index.ts`
    // refuses only the bodies that admit it, and a request without the header
    // is precisely the case `readBoundedText` exists for. A `null` body is a
    // POST with nothing in it, which the validator refuses as empty text.
    const text =
      request.body === null
        ? ""
        : await readBoundedText(request.body, MAX_ANNOUNCE_BYTES);
    if (text === null) return directoryError(413, "too_large");

    const verdict = JSON.parse(directory_validate_announcement(text)) as ValidationVerdict;
    if (verdict.kind !== "Valid") return directoryError(400, "invalid");

    // The ONLY way the probe URL is built. `directory_info_url` normalises
    // internally, so the authority fetched is provably the one the storage
    // path would accept — no literal path, no concatenation. `null` is
    // unreachable from a `Valid` verdict and is handled rather than asserted
    // away, because the export's type admits it.
    const infoUrl = directory_info_url(verdict.announcement.url);
    if (!infoUrl) return directoryError(400, "invalid");

    const info = await this.fetchInfoDocument(infoUrl);
    if (info.kind !== "ok") {
      return directoryError(422, info.kind === "too_large" ? "info_too_large" : "info_unreachable");
    }

    // The RAW body text is compared, not a re-serialised announcement: the
    // export re-validates internally, so this shell never has to reproduce
    // Rust's serialisation.
    const comparison = JSON.parse(
      directory_compare_announcement_to_info(text, info.text),
    ) as ComparisonVerdict;

    const nowMs = Date.now();
    const plan = planAnnounceUpsert({
      announcement: verdict.announcement,
      comparison,
      // Ignored by the upsert when the row already exists — `first_seen_ms` is
      // outside the conflict clause.
      firstSeenMs: nowMs,
      lastSeenMs: nowMs,
    });
    if (plan.kind === "reject") {
      const reason = plan.reason === "mismatch" ? `mismatch:${plan.field}` : "unverified";
      return directoryError(422, reason);
    }

    this.upsertServerRow(plan.row);
    // The sole producer of `announced_players_max`, which the metrics
    // game-outcome guard reads. Without this write the guard drops every game
    // outcome forever. It is also the counters' only WRITER until a client
    // reporter exists, so it is what keeps the blob inside one decay window —
    // hence `SCORE_WINDOW_MS`.
    const counters = recordAnnouncedPlayers(
      await this.readCounters(plan.row.url),
      plan.row.current_players,
      nowMs,
      SCORE_BUCKET_MS,
      SCORE_WINDOW_MS,
      RTT_BUCKET_EDGES_MS,
    );
    await this.ctx.storage.put(`${COUNTERS_PREFIX}${plan.row.url}`, counters);
    // Second call site, and the one that is easy to miss: an announce into an
    // otherwise idle DO schedules no alarm through the WebSocket path, so its
    // row would never be reaped.
    await this.ensureAlarm();
    return Response.json({ status: "accepted" }, { status: 202, headers: DIRECTORY_WRITE_CORS });
  }

  /** Fetch and read the announced host's info document under one timeout.
   *
   *  Both the fetch and the body read sit inside ONE `try` with ONE
   *  `AbortSignal.timeout`: the abort tears down the response body stream as
   *  well as the connection, so a server that trickles bytes throws inside the
   *  reader rather than hanging past the timeout. Nothing throws out of here —
   *  the announcer gets a 4xx, never a 5xx.
   *
   *  `redirect: "error"` is load-bearing twice over. The question this fetch
   *  asks is whether THIS host serves an info document matching THIS
   *  announcement, and a redirect answers a different question — some other
   *  host does — so following one would verify the wrong server even with no
   *  attacker present. It also closes the pivot: announcing is open by design
   *  (see the charter), so a follower would let any caller aim the verifier at
   *  a public hostname that redirects wherever it likes. Failing closed on a
   *  redirect keeps the destination equal to the announced one.
   *
   *  What this deliberately does NOT do is resolve the hostname and reject
   *  private, loopback or link-local addresses. Workers exposes no DNS
   *  resolution API and no connection-time hook, so that check cannot be
   *  written here; the platform's own edge is what keeps a Worker fetch off
   *  RFC1918 space. Stated rather than left implied, so nobody reads the
   *  absence as an oversight. */
  private async fetchInfoDocument(infoUrl: string): Promise<InfoFetch> {
    try {
      const response = await fetch(infoUrl, {
        signal: AbortSignal.timeout(INFO_FETCH_TIMEOUT_MS),
        headers: { accept: "application/json" },
        redirect: "error",
      });
      // A 404 and a refused connection are one fact here: no usable document.
      // `body === null` is handled explicitly rather than optional-chained,
      // which would silently read `undefined`.
      if (!response.ok || response.body === null) return { kind: "unreachable" };
      const text = await readBoundedText(response.body, MAX_INFO_BYTES);
      return text === null ? { kind: "too_large" } : { kind: "ok", text };
    } catch {
      return { kind: "unreachable" };
    }
  }

  /** `POST /servers/metrics` — sanitise, guard, fold, mirror. Always 204,
   *  exactly like `/telemetry`: an ingest failure must never surface to the
   *  client or pollute Workers Metrics. */
  private async metricsResponse(request: Request): Promise<Response> {
    const ok = () => new Response(null, { status: 204, headers: DIRECTORY_WRITE_CORS });
    try {
      this.ensureServersTable();
      // Bounded as it arrives — see `announceResponse`. An over-cap batch is
      // dropped silently, like every other metrics ingest failure.
      const text =
        request.body === null
          ? ""
          : await readBoundedText(request.body, MAX_METRICS_BYTES);
      if (text === null) return ok();

      let body: unknown = null;
      try {
        body = JSON.parse(text);
      } catch {
        body = null;
      }

      const reports = sanitizeMetricsBatch(body);
      if (reports.length === 0) return ok();

      // The origin guard's input: only URLs that actually have a row. A
      // report for anything else is dropped by the fold, so a forged URL
      // cannot grow storage.
      const known = new Map<string, ServerCounters>();
      for (const url of new Set(reports.map((report) => report.url))) {
        if (this.hasServerRow(url)) known.set(url, await this.readCounters(url));
      }

      const fold = foldMetricReports(
        reports,
        known,
        Date.now(),
        SCORE_BUCKET_MS,
        SCORE_WINDOW_MS,
        RTT_BUCKET_EDGES_MS,
      );
      for (const [url, counters] of fold.counters) {
        await this.ctx.storage.put(`${COUNTERS_PREFIX}${url}`, counters);
      }
      // Dashboard mirror of what was actually counted. Write-only: no counter
      // and no score ever reads back from Analytics Engine.
      for (const event of serverProbeEvents(fold.accepted)) {
        this.env.TELEMETRY?.writeDataPoint(toDataPoint(event));
      }
      // `urls` is plural because a batch is not scoped to one server: a client
      // reports on every server it probed.
      console.log({
        event: "server_metrics_ingested",
        urls: [...fold.counters.keys()],
        accepted: fold.accepted.length,
        dropped: fold.dropped,
      });
    } catch {
      // Swallow — ingest is best-effort and must never fail the request.
    }
    return ok();
  }

  // ── Outbound side-effect interpretation ────────────────────────────────

  private interpret(ws: WebSocket, outbounds: OutboundDto[]): void {
    for (const o of outbounds) this.dispatchOutbound(ws, o);
  }

  private dispatchOutbound(ws: WebSocket | null, o: OutboundDto): void {
    switch (o.kind) {
      case "ToSelf":
        if (ws) ws.send(JSON.stringify(o.msg));
        return;
      case "ToSubscribers":
        this.broadcastToSubscribers(JSON.stringify(o.msg));
        return;
      case "SendPlayerCountToSelf":
        if (ws) ws.send(this.playerCountFrame());
        return;
      case "AddSubscriber":
      case "RemoveSubscriber":
        // No-op: the subscriber registry IS each socket's persisted
        // ConnState.subscribed (set by the broker, read in
        // broadcastToSubscribers). A separate in-memory set would be lost on
        // hibernation, so the attachment is the single source of truth.
        return;
    }
  }

  // ── Messaging helpers ──────────────────────────────────────────────────

  private broadcastToSubscribers(frame: string): void {
    for (const sock of this.ctx.getWebSockets()) {
      if (this.isSubscribed(sock)) sock.send(frame);
    }
  }

  private broadcastPlayerCount(): void {
    const frame = this.playerCountFrame();
    for (const sock of this.ctx.getWebSockets()) {
      if (this.isSubscribed(sock)) sock.send(frame);
    }
  }

  private isSubscribed(sock: WebSocket): boolean {
    const conn = sock.deserializeAttachment() as { subscribed?: boolean } | null;
    return conn?.subscribed === true;
  }

  private playerCountFrame(): string {
    // PlayerCount is shell-owned: the broker emits SendPlayerCountToSelf and the
    // shell fills the count from the live socket set.
    return JSON.stringify({
      type: "PlayerCount",
      data: { count: this.ctx.getWebSockets().length },
    });
  }

  private async ensureAlarm(): Promise<void> {
    if ((await this.ctx.storage.getAlarm()) === null) {
      await this.ctx.storage.setAlarm(Date.now() + REAP_INTERVAL_MS);
    }
  }

  private sendHello(ws: WebSocket): void {
    ws.send(
      JSON.stringify({
        type: "ServerHello",
        data: {
          server_version: SERVER_VERSION,
          build_commit: SERVER_BUILD_COMMIT,
          protocol_version: PROTOCOL_VERSION,
          mode: "LobbyOnly",
          // Advertised ALONGSIDE protocol_version, never instead of it:
          // clients built before the lobby owned a version still gate on that
          // field, so it must keep tracking the full-game constant.
          lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
        },
      }),
    );
  }
}
