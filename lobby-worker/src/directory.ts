// Server-directory functional core for the lobby Worker.
//
// Same shape as `stats.ts` and `telemetry.ts`: pure functions with no I/O, no
// clock and no platform bindings, unit-testable under `tsx --test`. The
// Durable Object (`lobby-do.ts`) owns every wasm call, the `ctx.storage.sql`
// and `ctx.storage` access, the KV read, the outbound verification `fetch` and
// all `Response` construction; the Worker entry (`index.ts`) owns routing and
// the rate-limit binding call. This module owns the decisions both of them
// make, so each of their branches is a call to something tested here.
//
// GLUE-FREE, and it is a hard rule rather than a style preference: this file
// must never import `../broker-wasm-pkg/*`, `./lobby-do` or `./index`. Any
// import chain that reaches the generated glue pulls in `broker_bg.wasm`,
// which `tsx --test` cannot load (ERR_UNKNOWN_FILE_EXTENSION) and which CI's
// `lobby-worker-test` job never even builds. The tooling enforces it; the rule
// is written down because the failure reads as a mysterious module error.
//
// Consequently every value this module needs from Rust — the directory
// version, the counter bucket width, the decay window, the RTT histogram
// edges, and every validation or comparison verdict — arrives as an ARGUMENT
// from the DO, which read it from the wasm exports. There is deliberately not
// one number here that Rust also declares.

import { EVENT_SCHEMAS, type SanitizedEvent } from "./telemetry";

// ── Bounds ─────────────────────────────────────────────────────────────────
//
// The three body caps — MAX_INFO_BYTES, MAX_ANNOUNCE_BYTES and
// MAX_METRICS_BYTES — count BYTES of UTF-8, never characters, and every
// enforcement site measures the same way: {@link readBoundedText} sums the
// stream's chunk lengths before decoding, and the Durable Object's two
// re-checks of a body it has already read encode it back. A `.length` check
// on a decoded string would count UTF-16 code units, which for a body of
// multi-byte text admits roughly three times the bytes the name promises.
//
// The other four caps here are not byte counts and do not claim to be: two
// are durations, MAX_REPORTS_PER_BATCH is a count of reports, and
// MAX_GAME_CODE_LEN is a count of characters. Each says so on its own doc.

/** A row older than this is not listed and is reaped. Three minutes against
 *  phase-server's 60 s announce heartbeat: two heartbeats may be lost to a
 *  restart or a network blip without dropping a healthy server out of the
 *  directory. */
export const ANNOUNCE_TIMEOUT_MS = 180_000;

/** Cap on the DECODED body of a verification `/info` fetch.
 *
 *  Decoded, not wire bytes, and that is the whole point: Workers `fetch`
 *  transparently decodes `Content-Encoding`, and phase-server's `/info` IS
 *  gzipped, so a `Content-Length` check would be a compressed-size check that
 *  says nothing about what the isolate buffers. `Content-Length` is also
 *  absent under chunked transfer and is in any case supplied by the party
 *  being verified. The decoded bound is the one that actually caps isolate
 *  memory and `JSON.parse` cost. See {@link readBoundedText}. */
export const MAX_INFO_BYTES = 4096;

/** Cap on an announce request body. An announcement is nine short fields. */
export const MAX_ANNOUNCE_BYTES = 4096;

/** Cap on a metrics batch body — the same number `/telemetry` uses, for the
 *  same reason: a batch of capped-field reports is far under it. */
export const MAX_METRICS_BYTES = 32 * 1024;

/** Reports accepted from one batch; the rest are dropped, never an error. */
export const MAX_REPORTS_PER_BATCH = 50;

/** Characters kept of a reported game code. */
export const MAX_GAME_CODE_LEN = 16;

/** Reported RTTs clamp into `[0, MAX_RTT_MS]` before they reach a histogram —
 *  ingest cannot trust a client's arithmetic. */
export const MAX_RTT_MS = 60_000;

// NOTE: there is deliberately no `COUNTER_BUCKET_MS` and no
// `COUNTER_WINDOW_MS` here, and no RTT edge list. All three are Rust
// constants, read by `lobby-do.ts` from the wasm exports and passed in as
// arguments. Re-declaring any of them in TypeScript would silently mis-cut
// buckets, mis-age evidence or mis-file latencies, and would fail no test on
// either side of the boundary. Treat one reappearing here as a defect.

// ── Stored and wire shapes ─────────────────────────────────────────────────

/** One row of the DO's `servers` table, exactly its nine columns. `mode` is
 *  `ServerMode`'s serde form. Every field is a `SqlStorageValue`: TEXT →
 *  string, INTEGER → number. */
export interface StoredServerRow {
  url: string;
  name: string;
  mode: "Full" | "LobbyOnly";
  server_version: string;
  protocol_version: number;
  lobby_protocol_version: number;
  current_players: number;
  first_seen_ms: number;
  last_seen_ms: number;
}

/** Rust's `lobby_broker::directory::Score` in its serde form. The field names
 *  are Rust's, not a TypeScript restatement.
 *
 *  `value` is null below Rust's `SCORE_MIN_SAMPLES` while `samples` is still
 *  populated — that is what lets a consumer tell "too little evidence to rank"
 *  from "never reported". A consumer gating a health hint on `score == null`
 *  alone will render one off a three-sample window. */
export interface WireScore {
  /** 0–100, or null when there is too little evidence to rank. */
  value: number | null;
  samples: number;
  /** 0–1. */
  success_rate: number;
  /** 0–1. */
  completion_rate: number;
  /** Upper edge of the median histogram cell; null when no RTT was reported. */
  median_rtt_ms: number | null;
}

/** One entry of `GET /servers`: the nine stored columns plus the score
 *  computed at read time.
 *
 *  This is the phase's only new public wire contract. It is consumed by the
 *  client's server-directory service from a later phase; treat every field
 *  name here as frozen once deployed. `score` is null — never omitted — when
 *  the directory looked and found no evidence, because an absent key and a
 *  present null are the same thing to `JSON.parse` and different things to a
 *  reader of the contract. */
export interface DirectoryRow extends StoredServerRow {
  score: WireScore | null;
}

/** The `GET /servers` body. `directory_version` appears ONCE, on the
 *  envelope — it is the announcement shape's version gate, never a property of
 *  a row. */
export interface DirectoryBody {
  directory_version: number;
  servers: DirectoryRow[];
}

export interface DirectoryResponse {
  body: DirectoryBody;
  headers: Record<string, string>;
}

// ── Counters (the TypeScript view of Rust's `ServerCounters`) ──────────────

/** One bucket of client-reported evidence. Mirrors
 *  `lobby_broker::directory::CounterBucket` field for field — the JSON crosses
 *  the wasm boundary into `directory_score`, so a rename on either side is a
 *  parse failure, not a silent drop. */
export interface CounterBucket {
  start_ms: number;
  connect_attempts: number;
  connect_successes: number;
  games_started: number;
  games_completed: number;
  /** One cell per RTT edge plus an overflow cell. Length is derived from the
   *  edge list the DO passes in, never typed here. */
  rtt_histogram: number[];
  /** Peak `current_players` this server ANNOUNCED in the window. Written only
   *  by {@link recordAnnouncedPlayers}; read by the game-outcome guard. */
  announced_players_max: number;
}

export interface ServerCounters {
  buckets: CounterBucket[];
}

// ── Metrics reports ────────────────────────────────────────────────────────

/** What a client observed. A typed union rather than a cluster of booleans:
 *  the four cases are mutually exclusive and each feeds a different counter. */
export type ProbeOutcome = "connect_ok" | "connect_fail" | "game_completed" | "game_abandoned";

const PROBE_OUTCOMES: readonly ProbeOutcome[] = [
  "connect_ok",
  "connect_fail",
  "game_completed",
  "game_abandoned",
];

/** The outcomes that describe a GAME rather than a connection attempt.
 *
 *  One set, two consumers — the sanitiser's `game_code` gate and the fold's
 *  announced-players guard. The trap this closes is the two sites DISAGREEING,
 *  and it is worth naming both symptoms because they look nothing alike:
 *
 *    - fold has the outcome, sanitiser does not — no `game_code` is ever
 *      attached, so the guard's `!report.game_code` drops every report of that
 *      outcome;
 *    - sanitiser has it, fold does not — the guard is skipped, the report
 *      reaches the `switch` below, matches no arm (there is no `default`), and
 *      is counted as accepted while incrementing nothing: a materialised
 *      bucket and an Analytics Engine point for a report that moved no
 *      counter.
 *
 *  Neither symptom is a type error, and neither has a test that names it. One
 *  set is what makes the disagreement unrepresentable. */
const GAME_OUTCOMES: ReadonlySet<ProbeOutcome> = new Set(["game_completed", "game_abandoned"]);

/** One sanitised client report. Every field here survived the allow-list; a
 *  report object never carries anything else. */
export interface ServerProbeReport {
  url: string;
  outcome: ProbeOutcome;
  /** Connect latency, present only on `connect_ok` — see
   *  {@link sanitizeMetricsBatch}. */
  rtt_ms?: number;
  /** Required by the two `game_*` outcomes; dropped otherwise. */
  game_code?: string;
}

// ── CORS ───────────────────────────────────────────────────────────────────

/** The directory's CORS policy, parameterised by the methods an endpoint
 *  accepts.
 *
 *  Wildcard origin on purpose: this is public, non-secret data whose whole
 *  purpose is to be read by third-party clients on origins we do not
 *  enumerate. No `Vary: Origin` — the value is constant, and a `Vary` would
 *  fragment the 60 s cache for nothing.
 *
 *  Parameterised rather than a single shared record because the read endpoint
 *  and the two write endpoints do not share an
 *  `Access-Control-Allow-Methods`: a browser preflight for
 *  `POST /servers/metrics` fails against a `GET, OPTIONS` allow-list, and
 *  nothing in this phase would notice. */
export function directoryCorsHeaders(methods: string): Record<string, string> {
  return {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": methods,
    "Access-Control-Allow-Headers": "Content-Type",
  };
}

/** For `GET /servers` and its preflight. */
export const DIRECTORY_READ_CORS = directoryCorsHeaders("GET, OPTIONS");
/** For `POST /servers/announce`, `POST /servers/metrics` and their preflights. */
export const DIRECTORY_WRITE_CORS = directoryCorsHeaders("POST, OPTIONS");

/** Cache policy for the read endpoint alone. Not part of
 *  {@link directoryCorsHeaders}, which the write endpoints share. */
export const DIRECTORY_CACHE_CONTROL = "public, max-age=60";

// ── Ingest gate ────────────────────────────────────────────────────────────

export type IngestGate = { kind: "accept" } | { kind: "reject"; reason: "too_large" | "rate_limited" };

/** The subset of Cloudflare's `RateLimit` binding this gate uses. Declared
 *  structurally so a test can inject a counting stub without a binding. */
export interface IngestLimiter {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

/**
 * Decide whether an ingest request may proceed.
 *
 * The order is load-bearing. An oversize body is refused WITHOUT consulting
 * the limiter, so a flood of large bodies cannot burn a caller's rate-limit
 * budget (and cannot make the limiter the thing that has to survive the
 * flood). `contentLength` is a header read, so this runs before the body is
 * touched and the original `Request` can be forwarded unmodified; the DO
 * re-checks the length it ACTUALLY read, because `Content-Length` is supplied
 * by the caller.
 *
 * An absent limiter accepts — fail-OPEN, matching the optional `TELEMETRY`
 * binding: a deploy without the binding still serves. That is the opposite
 * direction from the allowlist, which fails closed, and the asymmetry is
 * deliberate: the allowlist is the admission gate, a rate limit is only a
 * throttle, and the body cap still applies either way.
 */
export async function checkIngestGate(args: {
  contentLength: number | null;
  maxBytes: number;
  key: string;
  limiter?: IngestLimiter;
}): Promise<IngestGate> {
  const { contentLength, maxBytes, key, limiter } = args;
  if (contentLength !== null && Number.isFinite(contentLength) && contentLength > maxBytes) {
    return { kind: "reject", reason: "too_large" };
  }
  if (!limiter) return { kind: "accept" };
  const { success } = await limiter.limit({ key });
  return success ? { kind: "accept" } : { kind: "reject", reason: "rate_limited" };
}

// ── Bounded body read ──────────────────────────────────────────────────────

/**
 * Read a response body, giving up once the DECODED text exceeds `maxBytes`.
 *
 * Returns `null` when the bound is exceeded, cancelling the stream rather than
 * draining it. The boundary is `>`, so a body of exactly `maxBytes` is
 * accepted.
 *
 * This exists because there is no header that answers the question: the
 * verified server supplies `Content-Length`, it is absent under chunked
 * transfer, and it counts compressed bytes for a body the runtime decodes for
 * us. Counting what we have actually accumulated is the only bound that maps
 * onto the hazard — isolate memory and `JSON.parse` cost.
 */
export async function readBoundedText(
  stream: ReadableStream<Uint8Array>,
  maxBytes: number,
): Promise<string | null> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let text = "";
  let bytes = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (value) {
        // Counted BEFORE decoding, so an over-cap chunk is never appended to
        // the string we are trying not to grow — and so the bound is bytes of
        // UTF-8 rather than UTF-16 code units.
        bytes += value.byteLength;
        if (bytes > maxBytes) {
          await reader.cancel();
          return null;
        }
        text += decoder.decode(value, { stream: true });
      }
    }
    // Flush any trailing partial code point. This can only add characters for
    // bytes already counted above, so it cannot cross the bound.
    text += decoder.decode();
    return text;
  } finally {
    reader.releaseLock();
  }
}

// ── Announce planning ──────────────────────────────────────────────────────

/** The `announcement` payload of a `Valid` validation verdict: Rust's own
 *  serialisation of a `ServerAnnouncement` that has already passed
 *  `validate_announcement`. Every field below was bounded, normalised and
 *  version-gated by Rust before it got here — this module projects columns
 *  from it and validates nothing. */
export interface ValidatedAnnouncement {
  directory_version: number;
  url: string;
  name: string;
  mode: "Full" | "LobbyOnly";
  server_version: string;
  protocol_version: number;
  lobby_protocol_version: number;
  current_players: number;
}

/** Which compared field disagreed. Mirrors
 *  `lobby_broker::directory::InfoMismatchField`'s serde form; the four values
 *  cross as JSON strings, so a variant added in Rust reaches here with no code
 *  change and no error — which is why the tests enumerate all four rather than
 *  asserting "some mismatch". */
export type InfoMismatchField = "Mode" | "ServerVersion" | "ProtocolVersion" | "LobbyProtocolVersion";

/** Boundary mirror of `lobby_broker_wasm::ComparisonDto`. */
export type ComparisonVerdict =
  | { kind: "Match" }
  | { kind: "Mismatch"; field: InfoMismatchField }
  | { kind: "Invalid"; error: string };

export type AnnouncePlan =
  | { kind: "upsert"; row: StoredServerRow }
  | { kind: "reject"; reason: "mismatch"; field: InfoMismatchField }
  | { kind: "reject"; reason: "invalid" };

/**
 * Project a validated announcement onto the nine stored columns.
 *
 * `directory_version` is deliberately NOT a column: it gates the announcement
 * shape and rides on the response envelope, and a per-row copy would be a
 * second place for it to be wrong. `score` is not a column either — it is
 * computed from the counters at read time, so a stored copy would be a
 * staler second authority for the same number.
 */
export function storedRowFromAnnouncement(
  announcement: ValidatedAnnouncement,
  firstSeenMs: number,
  lastSeenMs: number,
): StoredServerRow {
  return {
    url: announcement.url,
    name: announcement.name,
    mode: announcement.mode,
    server_version: announcement.server_version,
    protocol_version: announcement.protocol_version,
    lobby_protocol_version: announcement.lobby_protocol_version,
    current_players: announcement.current_players,
    first_seen_ms: firstSeenMs,
    last_seen_ms: lastSeenMs,
  };
}

/**
 * Turn a comparison verdict plus a validated announcement into the write the
 * DO should perform, or the typed reason it should refuse.
 *
 * The `field` of a mismatch is carried through rather than collapsed into one
 * "mismatch" answer: the four fields mean different things to an operator
 * (a `Mode` or `ServerVersion` disagreement says the announcement is about a
 * different server than the one answering at that address; a version
 * disagreement says one server's two documents disagree, typically a stale
 * cache or proxy).
 */
export function planAnnounceUpsert(args: {
  announcement: ValidatedAnnouncement;
  comparison: ComparisonVerdict;
  firstSeenMs: number;
  lastSeenMs: number;
}): AnnouncePlan {
  const { announcement, comparison, firstSeenMs, lastSeenMs } = args;
  switch (comparison.kind) {
    case "Match":
      return {
        kind: "upsert",
        row: storedRowFromAnnouncement(announcement, firstSeenMs, lastSeenMs),
      };
    case "Mismatch":
      return { kind: "reject", reason: "mismatch", field: comparison.field };
    case "Invalid":
      return { kind: "reject", reason: "invalid" };
  }
}

// ── Liveness, reaping, alarm ───────────────────────────────────────────────

/** One liveness predicate, two consumers: the reaper's alarm and the read
 *  path's filter. A duplicated threshold is the classic reaper bug — the
 *  sweep and the listing disagree about what is alive and a dead row shows up
 *  between alarms. */
export function isServerLive(lastSeenMs: number, nowMs: number): boolean {
  return nowMs - lastSeenMs <= ANNOUNCE_TIMEOUT_MS;
}

/** Split rows into the ones to keep and the ones to delete.
 *
 *  A fold rather than a `DELETE ... WHERE` predicate: it yields the reap count
 *  the structured log carries and it is unit-testable, and the table is
 *  allowlist-scale (tens of rows), so reading it to partition costs nothing. */
export function partitionServerRows<T extends { url: string; last_seen_ms: number }>(
  rows: readonly T[],
  nowMs: number,
): { live: T[]; expired: T[] } {
  const live: T[] = [];
  const expired: T[] = [];
  for (const row of rows) {
    if (isServerLive(row.last_seen_ms, nowMs)) live.push(row);
    else expired.push(row);
  }
  return { live, expired };
}

/** Whether the DO still has anything to reap, and so must reschedule its
 *  alarm.
 *
 *  Extends the existing "keep reaping while lobby entries remain" rule instead
 *  of duplicating it: the directory adds a second kind of reapable state, and
 *  an alarm that stopped while rows remained would leave them listed until the
 *  next unrelated write. */
export function shouldKeepAlarm(brokerEmpty: boolean, directoryRowCount: number): boolean {
  return !brokerEmpty || directoryRowCount > 0;
}

// ── GET /servers ───────────────────────────────────────────────────────────

/**
 * Shape the directory listing: live ∩ allowlist, each row carrying its score.
 *
 * Both halves of the intersection are load-bearing and fail in opposite
 * directions. An empty allowlist lists NOTHING (fail-closed): an unconfigured
 * directory must advertise nobody, because listing every announcer would
 * publish an unvetted list. A row that has stopped announcing is dropped at
 * read time even if the reaper has not swept yet, so the alarm interval never
 * becomes part of the liveness contract.
 *
 * `allowlist` must already be canonicalised by the same Rust authority that
 * produced the row keys — the DO runs each KV key through
 * `directory_normalize_url`. Comparing a human-typed key with a normalised row
 * key by string equality would silently under-list.
 */
export function buildDirectoryResponse(args: {
  rows: readonly StoredServerRow[];
  allowlist: ReadonlySet<string>;
  scores: ReadonlyMap<string, WireScore | null>;
  directoryVersion: number;
  nowMs: number;
}): DirectoryResponse {
  const { rows, allowlist, scores, directoryVersion, nowMs } = args;
  const servers: DirectoryRow[] = [];
  for (const row of rows) {
    if (!isServerLive(row.last_seen_ms, nowMs)) continue;
    if (!allowlist.has(row.url)) continue;
    servers.push({
      url: row.url,
      name: row.name,
      mode: row.mode,
      server_version: row.server_version,
      protocol_version: row.protocol_version,
      lobby_protocol_version: row.lobby_protocol_version,
      current_players: row.current_players,
      first_seen_ms: row.first_seen_ms,
      last_seen_ms: row.last_seen_ms,
      score: scores.get(row.url) ?? null,
    });
  }
  return {
    body: { directory_version: directoryVersion, servers },
    headers: { ...DIRECTORY_READ_CORS, "Cache-Control": DIRECTORY_CACHE_CONTROL },
  };
}

// ── Metrics ingest ─────────────────────────────────────────────────────────

function truncate(value: string, max: number): string {
  return value.length > max ? value.slice(0, max) : value;
}

/**
 * Validate and sanitise a raw metrics batch into the accepted reports.
 *
 * Mirrors `sanitizeTelemetryBatch`: `[]` for any malformed input rather than
 * an error, unknown outcomes dropped, every field outside the allow-list
 * discarded. Ingest cannot trust the client, and a newer client adding a field
 * must not break an older Worker.
 *
 * One rule worth stating once: `rtt_ms` survives only on `connect_ok`. An RTT
 * attached to a `connect_fail` is not a slow handshake, it is the timing of a
 * handshake that never completed, and folding it into the latency histogram
 * would make a server that refuses connections quickly look fast. Because the
 * sanitiser enforces it, the fold never has to ask which outcome it is looking
 * at before touching the histogram: a present `rtt_ms` is always a completed
 * connect's latency.
 */
export function sanitizeMetricsBatch(body: unknown): ServerProbeReport[] {
  if (!body || typeof body !== "object") return [];
  const batch = body as Record<string, unknown>;
  if (batch.schema !== 1) return [];
  if (!Array.isArray(batch.reports)) return [];

  const out: ServerProbeReport[] = [];
  for (const raw of batch.reports.slice(0, MAX_REPORTS_PER_BATCH)) {
    if (!raw || typeof raw !== "object") continue;
    const fields = raw as Record<string, unknown>;

    const url = fields.url;
    if (typeof url !== "string" || url.length === 0) continue;
    const outcome = fields.outcome;
    if (typeof outcome !== "string") continue;
    if (!PROBE_OUTCOMES.includes(outcome as ProbeOutcome)) continue;

    const report: ServerProbeReport = { url, outcome: outcome as ProbeOutcome };

    const rtt = fields.rtt_ms;
    if (outcome === "connect_ok" && typeof rtt === "number" && Number.isFinite(rtt)) {
      report.rtt_ms = Math.round(Math.min(Math.max(rtt, 0), MAX_RTT_MS));
    }
    const gameCode = fields.game_code;
    if (GAME_OUTCOMES.has(report.outcome) && typeof gameCode === "string" && gameCode.length > 0) {
      report.game_code = truncate(gameCode, MAX_GAME_CODE_LEN);
    }

    out.push(report);
  }
  return out;
}

/** Start of the bucket `nowMs` falls in. `bucketMs` is Rust's
 *  `SCORE_BUCKET_MS`, passed in — see the note beside the bounds above. */
function bucketStart(nowMs: number, bucketMs: number): number {
  return nowMs - (nowMs % bucketMs);
}

function emptyBucket(startMs: number, cellCount: number): CounterBucket {
  return {
    start_ms: startMs,
    connect_attempts: 0,
    connect_successes: 0,
    games_started: 0,
    games_completed: 0,
    rtt_histogram: new Array<number>(cellCount).fill(0),
    announced_players_max: 0,
  };
}

/** Drop buckets that have decayed to zero weight. Mirrors Rust's
 *  `bucket_weight`: age `>= windowMs` is weightless.
 *
 *  Storage stays bounded at one window's worth only because EVERY writer of a
 *  counter blob runs this — {@link foldMetricReports} and
 *  {@link recordAnnouncedPlayers} both do. A writer that skipped it would grow
 *  the blob without bound, and no reader prunes. */
function liveBuckets(buckets: readonly CounterBucket[], nowMs: number, windowMs: number): CounterBucket[] {
  return buckets.filter((bucket) => nowMs - bucket.start_ms < windowMs);
}

/** The histogram cell a latency falls in: the first edge it does not exceed,
 *  or the overflow cell. `edgesMs` is Rust's `RTT_BUCKET_EDGES_MS`, passed in
 *  — the cell count is `edgesMs.length + 1` on both sides, so the two arrays
 *  cannot differ in length either. */
function rttCell(rttMs: number, edgesMs: readonly number[]): number {
  for (let i = 0; i < edgesMs.length; i += 1) {
    if (rttMs <= edgesMs[i]) return i;
  }
  return edgesMs.length;
}

/** Raise the CURRENT bucket's announced-player peak to `players`, dropping
 *  decayed buckets on the way through.
 *
 *  The sole writer of `announced_players_max`, called from the announce path
 *  after an accepted upsert. A raise, never an overwrite: a heartbeat that
 *  happens to catch an empty moment must not erase the peak the window already
 *  saw. It raises no other bucket — an older window's peak is a fact about
 *  that window.
 *
 *  It ages buckets out for the same reason {@link foldMetricReports} does, and
 *  the ageing is not optional here: until a client reporter exists this is the
 *  counters' ONLY writer, so a version of it that merely appended would grow a
 *  listed server's blob by one bucket per announce window forever, with
 *  nothing else ever pruning it. */
export function recordAnnouncedPlayers(
  counters: ServerCounters,
  players: number,
  nowMs: number,
  bucketMs: number,
  windowMs: number,
  edgesMs: readonly number[],
): ServerCounters {
  const startMs = bucketStart(nowMs, bucketMs);
  // Age out FIRST, so the returned value can never carry a bucket the window
  // has already discarded.
  const buckets = liveBuckets(counters.buckets, nowMs, windowMs).map((bucket) => ({ ...bucket }));
  let current = buckets.find((bucket) => bucket.start_ms === startMs);
  if (!current) {
    current = emptyBucket(startMs, edgesMs.length + 1);
    buckets.push(current);
  }
  current.announced_players_max = Math.max(current.announced_players_max, players);
  return { buckets };
}

export interface MetricsFold {
  /** Updated counters, keyed by server URL. Only servers whose counters
   *  actually changed appear. */
  counters: Map<string, ServerCounters>;
  /** The reports that survived both guards. Returned as the reports rather
   *  than as a count so the Analytics Engine mirror can be built from exactly
   *  what was counted — mirroring the pre-guard batch would put forged URLs in
   *  the dashboard beside real evidence. */
  accepted: ServerProbeReport[];
  dropped: number;
}

/**
 * Fold sanitised reports into per-server counters.
 *
 * `known` is the origin guard AND the source of the current counters: its keys
 * are exactly the URLs that have a row in `servers`. A report for anything
 * else is dropped, which is what stops a forged URL from growing storage and
 * what keeps counters attached to servers that actually announced.
 *
 * A `game_completed` / `game_abandoned` report is dropped unless it carries a
 * game code AND the origin's bucket for this window shows it had announced
 * players. A server that never had a player online in the window cannot have
 * had a game finish in it. `connect_*` reports are deliberately exempt: a
 * failed connect is precisely the case where nobody was ever online.
 *
 * That guard reads the CURRENT bucket only, and the sole writer of
 * `announced_players_max` is the announce path at its 60 s heartbeat. A game
 * finishing in the first ~60 s of a new bucket — before that bucket's first
 * announce lands — is therefore dropped: roughly 1-in-60 of completion
 * reports, biased towards games ending at a bucket boundary. It is fail-safe
 * (it depresses `completion_rate`, never inflates it) and unreachable while no
 * client produces game outcomes at all.
 *
 * `bucketMs`, `windowMs` and `edgesMs` are Rust's constants, passed in by the
 * DO from the wasm exports. They are parameters rather than module constants
 * so that there is no second declaration of them anywhere — and so a test can
 * drive the fold at any width without patching a module.
 */
export function foldMetricReports(
  reports: readonly ServerProbeReport[],
  known: ReadonlyMap<string, ServerCounters>,
  nowMs: number,
  bucketMs: number,
  windowMs: number,
  edgesMs: readonly number[],
): MetricsFold {
  const cellCount = edgesMs.length + 1;
  const startMs = bucketStart(nowMs, bucketMs);
  const counters = new Map<string, ServerCounters>();
  const accepted: ServerProbeReport[] = [];
  let dropped = 0;

  const currentBucketFor = (url: string): CounterBucket => {
    let entry = counters.get(url);
    if (!entry) {
      const existing = known.get(url) ?? { buckets: [] };
      // `rtt_histogram` is copied explicitly: a spread copies it by reference,
      // and it is the one field this fold mutates in place, so a shallow copy
      // would count latencies into the caller's own map.
      entry = {
        buckets: liveBuckets(existing.buckets, nowMs, windowMs).map((b) => ({
          ...b,
          rtt_histogram: [...b.rtt_histogram],
        })),
      };
      counters.set(url, entry);
    }
    let bucket = entry.buckets.find((b) => b.start_ms === startMs);
    if (!bucket) {
      bucket = emptyBucket(startMs, cellCount);
      entry.buckets.push(bucket);
    }
    return bucket;
  };

  for (const report of reports) {
    if (!known.has(report.url)) {
      dropped += 1;
      continue;
    }
    if (GAME_OUTCOMES.has(report.outcome)) {
      // Peek WITHOUT materialising a bucket, so a rejected game outcome cannot
      // create the very bucket whose emptiness rejected it.
      const existing = counters.get(report.url) ?? known.get(report.url);
      const window = existing?.buckets.find((b) => b.start_ms === startMs);
      if (!report.game_code || !window || window.announced_players_max <= 0) {
        dropped += 1;
        continue;
      }
    }

    const bucket = currentBucketFor(report.url);
    switch (report.outcome) {
      case "connect_ok":
        bucket.connect_attempts += 1;
        bucket.connect_successes += 1;
        if (report.rtt_ms !== undefined) {
          bucket.rtt_histogram[rttCell(report.rtt_ms, edgesMs)] += 1;
        }
        break;
      case "connect_fail":
        bucket.connect_attempts += 1;
        break;
      case "game_completed":
        bucket.games_started += 1;
        bucket.games_completed += 1;
        break;
      case "game_abandoned":
        bucket.games_started += 1;
        break;
    }
    accepted.push(report);
  }

  return { counters, accepted, dropped };
}

/** Project accepted reports onto Analytics Engine points, through the shared
 *  `EVENT_SCHEMAS` column layout rather than a second column list here. AE
 *  columns are positional and permanent, so the schema is the authority for
 *  the order.
 *
 *  The envelope fields are empty strings: a server probe has no client app
 *  version, build hash or platform, and inventing one would put a value in a
 *  column that means something else for every other event. */
export function serverProbeEvents(reports: readonly ServerProbeReport[]): SanitizedEvent[] {
  const schema = EVENT_SCHEMAS.server_probe;
  return reports.map((report) => {
    const fields: Record<string, unknown> = {
      url: report.url,
      outcome: report.outcome,
      game_code: report.game_code,
      rtt_ms: report.rtt_ms,
    };
    return {
      event: "server_probe",
      appVersion: "",
      buildHash: "",
      platform: "",
      blobs: schema.blobs.map((key) => (typeof fields[key] === "string" ? (fields[key] as string) : "")),
      doubles: schema.doubles.map((key) =>
        typeof fields[key] === "number" && Number.isFinite(fields[key]) ? (fields[key] as number) : 0,
      ),
    };
  });
}
