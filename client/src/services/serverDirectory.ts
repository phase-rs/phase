/**
 * The client's mirror of the lobby Worker's `GET /servers` contract, and the
 * projection that turns one directory row into a browsable lobby source.
 *
 * The type declarations below are a DELIBERATE DUPLICATE of
 * `lobby-worker/src/directory.ts`, not an oversight. `client/tsconfig.app.json`
 * is `"include": ["src"]` and `lobby-worker/` is a separate package with its own
 * tsconfig and its own ambient Cloudflare types, so there is no import path from
 * here to there and creating one would drag those types into the client build.
 * `__tests__/serverDirectory.test.ts` is what keeps the duplication honest: it
 * reads the Worker's source text at test time and compares it field-for-field
 * against the key arrays below.
 *
 * Structured after `services/changelog.ts`: a read-only JSON service whose
 * failures are silent by contract. A directory that cannot be read simply means
 * the lobby lists presets and hand-added sources, never an error surfaced to
 * the user.
 */

import { serverProtocolRejection, type ServerInfo } from "../adapter/ws-adapter";
import { OFFICIAL_MULTIPLAYER_SERVER_URL } from "../config/multiplayerServer";
import { parseWebSocketUrl } from "./serverDetection";
import { useMultiplayerStore, type LobbySource } from "../stores/multiplayerStore";

/** Version of the ANNOUNCEMENT shape this client understands. Mirrors
 *  `DIRECTORY_VERSION` in `crates/lobby-broker/src/directory.rs`; the two are
 *  pinned together by `scripts/check-protocol-version.mjs`, which runs inside
 *  `pnpm run type-check`. A `GET /servers` body declaring any other version is
 *  ignored whole — a shape this client cannot read is not partially readable. */
export const DIRECTORY_VERSION = 1;

/** How long a completed directory read is trusted before another is issued.
 *  Sits deliberately ABOVE the Worker's own `Cache-Control: public, max-age=60`
 *  so the browser's HTTP cache and this one compose rather than fight. */
export const DIRECTORY_TTL_MS = 5 * 60_000;

/** Bound on directory-listed sources this client will dial, mirroring
 *  `MAX_USER_LOBBY_SOURCES`. The directory is operator-curated, but its body is
 *  still external input and a list of unbounded length is an unbounded number
 *  of WebSockets. Rows are ranked by score before the cut, so the cap drops the
 *  worst-evidenced servers among rows the directory could actually rank. When
 *  scores TIE — including the all-unranked case, where every row is below
 *  Rust's `SCORE_MIN_SAMPLES` (or has zero total weight) and so projects to
 *  `undefined` — the sort is stable and leaves the directory's own order
 *  intact, so the cut is scan-order among equals. */
export const MAX_DIRECTORY_LOBBY_SOURCES = 8;

/** Bound on one directory read. One precedent in the client:
 *  `pwa/chunkReloadHandler.ts`. */
const DIRECTORY_FETCH_TIMEOUT_MS = 5_000;

/** Rust's `lobby_broker::directory::Score` in its serde form, as the Worker
 *  re-declares it. The field names are Rust's, not a TypeScript restatement.
 *
 *  `value` is null when Rust could not rank the server — below its
 *  `SCORE_MIN_SAMPLES`, or with zero total weight — while `samples` is still
 *  populated, which is what lets a consumer tell "too little evidence to rank"
 *  from "never reported". A consumer gating a health hint on `score === null`
 *  alone will render one off a three-sample window, because that window arrives
 *  as a present object whose `value` is null. Nothing in this build reads the
 *  components; they are carried on `DirectorySource.row.score` for the phase
 *  that adds health hints. */
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

/**
 * One entry of `GET /servers`: the nine stored columns plus the score computed
 * at read time.
 *
 * Declared FLAT on purpose. The Worker splits the same ten fields across
 * `StoredServerRow` and `DirectoryRow extends StoredServerRow`; the client has
 * no `StoredServerRow` of its own, and the mirror gate compares this one body
 * against the UNION of the Worker's two interfaces.
 *
 * `score` is null — never omitted — when the directory looked and found no
 * evidence, because an absent key and a present null are the same thing to
 * `JSON.parse` and different things to a reader of the contract.
 */
export interface DirectoryRow {
  url: string;
  name: string;
  mode: "Full" | "LobbyOnly";
  server_version: string;
  protocol_version: number;
  lobby_protocol_version: number;
  current_players: number;
  first_seen_ms: number;
  last_seen_ms: number;
  score: WireScore | null;
}

/** The `GET /servers` body. `directory_version` appears ONCE, on the envelope —
 *  it is the announcement shape's version gate, never a property of a row. */
export interface DirectoryBody {
  directory_version: number;
  servers: DirectoryRow[];
}

// The runtime key arrays and their compile-time exhaustiveness assertions.
// BOTH halves of each pair are required and neither subsumes the other:
// `satisfies` rejects an array entry that is not a key of the interface, while
// the `Exclude` assertion rejects a key of the interface that is missing from
// the array. Only the second catches a field added to the mirror and forgotten
// here, which is the drift the runtime gate in `__tests__` also watches for.

export const WIRE_SCORE_KEYS = [
  "value",
  "samples",
  "success_rate",
  "completion_rate",
  "median_rtt_ms",
] as const satisfies readonly (keyof WireScore)[];
type MissingWireScoreKey = Exclude<keyof WireScore, (typeof WIRE_SCORE_KEYS)[number]>;
export const WIRE_SCORE_KEYS_ARE_EXHAUSTIVE: [MissingWireScoreKey] extends [never]
  ? true
  : never = true;

export const DIRECTORY_ROW_KEYS = [
  "url",
  "name",
  "mode",
  "server_version",
  "protocol_version",
  "lobby_protocol_version",
  "current_players",
  "first_seen_ms",
  "last_seen_ms",
  "score",
] as const satisfies readonly (keyof DirectoryRow)[];
type MissingDirectoryRowKey = Exclude<
  keyof DirectoryRow,
  (typeof DIRECTORY_ROW_KEYS)[number]
>;
export const DIRECTORY_ROW_KEYS_ARE_EXHAUSTIVE: [MissingDirectoryRowKey] extends [never]
  ? true
  : never = true;

export const DIRECTORY_BODY_KEYS = [
  "directory_version",
  "servers",
] as const satisfies readonly (keyof DirectoryBody)[];
type MissingDirectoryBodyKey = Exclude<
  keyof DirectoryBody,
  (typeof DIRECTORY_BODY_KEYS)[number]
>;
export const DIRECTORY_BODY_KEYS_ARE_EXHAUSTIVE: [MissingDirectoryBodyKey] extends [never]
  ? true
  : never = true;

/**
 * One projected directory listing.
 *
 * Both spellings of "the server this row names" are kept, because they are not
 * always the same string. `row.url` is the ANNOUNCED key — Rust's
 * `AnnouncedUrl`, the directory table's PRIMARY KEY and the allow-list key,
 * carrying no trailing slash. `source.url` is the CLIENT key —
 * `parseWebSocketUrl(...).href`, the same canonicaliser `userLobbySource` uses,
 * which is what makes cross-origin dedupe possible at all. They differ for a
 * pathless authority (`wss://a.example` announces without a slash and
 * canonicalises with one), and the client key is not invertible back to the
 * announced one, so anything that has to name this server TO the directory must
 * read `row.url`.
 *
 * `row.score` retains the whole `WireScore`; `source.score` carries only
 * `value`, because `LobbySource.score` is the comparator's 0–100 number.
 */
export interface DirectorySource {
  source: LobbySource;
  row: DirectoryRow;
  /** Why this client cannot speak to the row's server on the LOBBY surface, or
   *  `null` when it can. Computed once here, from `serverProtocolRejection` —
   *  the single protocol-window authority — and thereafter only READ. */
  rejection: string | null;
  /** The same verdict for the FULL-GAME surface, which is the one a hosted
   *  match runs on. Separate from {@link DirectorySource.rejection} because the
   *  two windows are versioned independently: a `Full` server whose full-game
   *  protocol has drifted browses correctly and can still run no match.
   *  Computed once here, from the same single authority, and thereafter only
   *  READ. */
  fullRejection: string | null;
}

/** A listing's health, as a reader of its raw score components would describe
 *  it. A typed union rather than an `isSlow` boolean: a second reading would
 *  otherwise need a second flag, and two flags can contradict each other. */
export type HealthHint = "slow" | "unreliable";

/** Median handshake RTT at or above which a listing reads as slow.
 *
 *  MUST be one of Rust's `RTT_BUCKET_EDGES_MS` — `median_rtt_ms` is the upper
 *  edge of a histogram cell (`crates/lobby-broker/src/directory.rs`), never an
 *  arbitrary number, so a threshold between two edges would behave identically
 *  to the next edge down while reading as if it discriminated. `400` is the
 *  fourth edge: two cells above Rust's own full-credit `RTT_FAST_MS` point, and
 *  below where its latency component is already near zero. */
export const SLOW_MEDIAN_RTT_MS = 400;

/** Connect success rate below which a listing reads as unreliable — one failed
 *  connect in ten. */
export const UNRELIABLE_SUCCESS_RATE = 0.9;

/**
 * How a listing's raw score components read to a human, or `null` for "say
 * nothing".
 *
 * Takes the whole {@link WireScore} and not `LobbySource.score`, because the
 * two "no score" cases are DIFFERENT and only one is visible from the collapsed
 * number: `score === null` means Rust found no live evidence at all, while
 * `score.value === null` means evidence exists but is below its
 * `SCORE_MIN_SAMPLES` (or carries zero total weight). Both must stay silent —
 * rendering a hint off a three-sample window is the specific defect this gate
 * exists to prevent.
 *
 * PRECEDENCE: unreliable before slow, stated here so the ordering is a decision
 * and not an accident of `if` order. A server you often cannot reach is worse
 * to not know about than one you reach slowly, and a row has space for one
 * badge.
 *
 * Nothing is recomputed: every number read here is produced by
 * `lobby_broker::directory::score` in Rust and served whole by the Worker,
 * whose own comment states the contract from the other side — a client renders
 * "slow" or "unreliable" from these components and must never recompute the
 * score itself. This function performs two comparisons and returns a label,
 * which is presentation.
 */
export function healthHint(score: WireScore | null): HealthHint | null {
  if (score === null) return null;
  if (score.value === null) return null;
  if (score.success_rate < UNRELIABLE_SUCCESS_RATE) return "unreliable";
  if (score.median_rtt_ms !== null && score.median_rtt_ms >= SLOW_MEDIAN_RTT_MS) return "slow";
  return null;
}

/**
 * The `GET /servers` endpoint for this build's official lobby.
 *
 * Rebuilt from `host` rather than by mutating `URL.protocol`, because the
 * announced path (`/ws`) has to be dropped too: `/servers` is served at the
 * Worker root. A self-hosted build whose official URL is its own phase-server
 * will `GET /servers` on a server with no such route and list presets plus
 * hand-added sources only — the intended behaviour, not a degradation. Which
 * arm it lands on depends on that server's CORS: under phase-server's default
 * permissive policy the GET resolves 404 and backs off for the TTL, while a
 * pinned `--cors-origin` makes the browser reject it, so nothing is stamped and
 * the next lobby mount retries.
 */
export function directoryUrl(): string {
  const parsed = parseWebSocketUrl(OFFICIAL_MULTIPLAYER_SERVER_URL);
  // `OFFICIAL_MULTIPLAYER_SERVER_URL` is a build-time define and is always a
  // ws(s) URL; the guard is the type narrowing, not a runtime possibility.
  const scheme = parsed?.protocol === "ws:" ? "http:" : "https:";
  return `${scheme}//${parsed?.host ?? ""}/servers`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isNullableFiniteNumber(value: unknown): value is number | null {
  return value === null || isFiniteNumber(value);
}

/** `null` (absent evidence) or a fully-formed `WireScore`; `undefined` signals
 *  "this is not a score at all", which drops the whole row. */
function projectScore(value: unknown): WireScore | null | undefined {
  if (value === null) return null;
  if (!isRecord(value)) return undefined;
  if (!isNullableFiniteNumber(value.value)) return undefined;
  if (!isFiniteNumber(value.samples)) return undefined;
  if (!isFiniteNumber(value.success_rate)) return undefined;
  if (!isFiniteNumber(value.completion_rate)) return undefined;
  if (!isNullableFiniteNumber(value.median_rtt_ms)) return undefined;
  return {
    value: value.value,
    samples: value.samples,
    success_rate: value.success_rate,
    completion_rate: value.completion_rate,
    median_rtt_ms: value.median_rtt_ms,
  };
}

/**
 * Validate and project one row of a `GET /servers` body. `null` drops this row
 * and only this row — a directory body is a list, and one malformed entry among
 * ten must not blank a working directory.
 *
 * Every declared field of {@link DirectoryRow} is checked with its declared
 * runtime type: this is the client's boundary validator for an authority the
 * user never typed, so it trusts nothing the announce path already enforced.
 */
export function projectDirectoryRow(value: unknown): DirectorySource | null {
  if (!isRecord(value)) return null;
  if (typeof value.url !== "string") return null;
  if (typeof value.name !== "string") return null;
  if (value.mode !== "Full" && value.mode !== "LobbyOnly") return null;
  if (typeof value.server_version !== "string") return null;
  if (!isFiniteNumber(value.protocol_version)) return null;
  if (!isFiniteNumber(value.lobby_protocol_version)) return null;
  if (!isFiniteNumber(value.current_players)) return null;
  if (!isFiniteNumber(value.first_seen_ms)) return null;
  if (!isFiniteNumber(value.last_seen_ms)) return null;
  const score = projectScore(value.score);
  if (score === undefined) return null;

  const parsed = parseWebSocketUrl(value.url);
  // A row is an authority the user never typed, so it must never downgrade this
  // client to plaintext. Rust already refuses a non-`wss` announcement; this is
  // the client refusing to TRUST that, which is what a boundary validator is
  // for. This guard is the ONLY refusal on the paths where mixed-content policy
  // does not apply — the Tauri shell and an `http://localhost` dev page both
  // allow a plaintext socket — so it cannot be justified as a redundant
  // belt-and-braces check on top of the browser's.
  if (!parsed || parsed.protocol !== "wss:") return null;

  const row: DirectoryRow = {
    url: value.url,
    name: value.name,
    mode: value.mode,
    server_version: value.server_version,
    protocol_version: value.protocol_version,
    lobby_protocol_version: value.lobby_protocol_version,
    current_players: value.current_players,
    first_seen_ms: value.first_seen_ms,
    last_seen_ms: value.last_seen_ms,
    score,
  };

  const info: ServerInfo = {
    version: row.server_version,
    // A directory row is not a `ServerHello`, and the announcement shape has no
    // build-commit field to carry one: `RawAnnouncement` declares none and the
    // Worker stores none. `ServerInfo.buildCommit` is a required `string`, so a
    // placeholder is unavoidable — and it is inert, because
    // `serverProtocolRejection` reads only `mode`, `lobbyProtocolVersion` and
    // `protocolVersion`. Nothing else ever sees this object: it is constructed
    // here, passed once, and discarded.
    buildCommit: "",
    protocolVersion: row.protocol_version,
    mode: row.mode,
    lobbyProtocolVersion: row.lobby_protocol_version,
  };

  return {
    source: {
      url: parsed.href,
      name: row.name,
      origin: "directory",
      // From the row, so an incompatible source — which will never handshake —
      // still renders its kind, and a compatible one shows it before its socket
      // opens. `lobbySources`' decoration still overwrites `kind` from
      // `sourceStatus` once the handshake lands, so live truth wins.
      kind: row.mode,
      // The number, never the `WireScore` object: `LobbySource.score` is the
      // comparator's 0–100 rank and `undefined` is its "unranked".
      score: row.score?.value ?? undefined,
    },
    row,
    // The LOBBY surface, explicitly. A `Full` server whose full-game protocol
    // has drifted is still perfectly browsable, and the browse socket carries
    // no `GameState`.
    rejection: serverProtocolRejection(info, "lobby"),
    // The FULL-GAME surface, for the one consumer that places a match on this
    // server rather than browsing it. Same authority, same object, different
    // surface argument — nothing here compares a version number.
    fullRejection: serverProtocolRejection(info, "full"),
  };
}

/**
 * Validate a whole `GET /servers` body. `null` means "ignore this response
 * entirely"; an array (possibly empty) is the new directory.
 *
 * Envelope failure is TOTAL while row failure is PARTIAL, and the asymmetry is
 * deliberate: an unreadable envelope means the client does not know what it
 * received, whereas one malformed row among ten must not blank a working
 * directory.
 */
export function projectDirectoryBody(value: unknown): DirectorySource[] | null {
  if (!isRecord(value)) return null;
  if (typeof value.directory_version !== "number") return null;
  if (value.directory_version !== DIRECTORY_VERSION) return null;
  if (!Array.isArray(value.servers)) return null;

  const projected: DirectorySource[] = [];
  for (const entry of value.servers) {
    const source = projectDirectoryRow(entry);
    if (!source) continue;
    if (projected.some((existing) => existing.source.url === source.source.url)) continue;
    projected.push(source);
  }
  // Dedupe runs BEFORE the sort, so a canonicalisation collision keeps the
  // FIRST row in body order rather than the better-scored one. Deliberate: two
  // rows that canonicalise to one client URL are one server announcing itself
  // twice, and the directory's own order is the only tie-break that does not
  // invent a preference between two records of the same authority.
  //
  // Rank before the cut so the bound drops the worst-evidenced servers rather
  // than an arbitrary suffix of the directory's scan order — but only among
  // rows the directory could rank. When scores tie, including the all-unranked
  // case below `SCORE_MIN_SAMPLES`, every row projects to `undefined` (→ -1),
  // the comparator returns 0, and this stable sort leaves the directory's own
  // order, so the cut is scan-order among equals. V-U11f pins that semantics,
  // so introducing a tie-breaker later is a visible test change.
  projected.sort((a, b) => (b.source.score ?? -1) - (a.source.score ?? -1));
  return projected.slice(0, MAX_DIRECTORY_LOBBY_SOURCES);
}

/** Dedupe of concurrent callers. The module's ONLY mutable state, and it
 *  self-clears in a `finally` — every cached value lives in the store, so a
 *  test resets the directory with the same `setState` it resets everything
 *  else with, and nothing leaks between test files. */
let inFlight: Promise<void> | null = null;

/**
 * Read the official directory once per TTL and publish the projection into the
 * store.
 *
 * Failure is silent by contract, exactly as `changelog.ts` documents it: no
 * toast, no modal, no error state, and nothing thrown to the caller. A refresh
 * that fails leaves `directorySources` untouched, which IS the last-good
 * fallback the lobby falls back through.
 */
export function refreshServerDirectory(): Promise<void> {
  const { directoryFetchedAtMs } = useMultiplayerStore.getState();
  if (
    directoryFetchedAtMs !== null &&
    Date.now() - directoryFetchedAtMs < DIRECTORY_TTL_MS
  ) {
    return Promise.resolve();
  }
  if (inFlight !== null) return inFlight;

  inFlight = (async () => {
    try {
      const res = await fetch(directoryUrl(), {
        signal: AbortSignal.timeout(DIRECTORY_FETCH_TIMEOUT_MS),
      });
      // One rule, one sentence: the timestamp is written whenever the fetch
      // RESOLVES, whatever the status. A resolved response is an answer, so a
      // self-hosted build taking a permanent 404 — or a client too old for the
      // deployed `directory_version` — backs off for the TTL instead of
      // re-asking on every lobby mount. A REJECTION (offline, DNS, timeout, or
      // a CORS policy that refuses this origin, which a phase-server pinned
      // with `--cors-origin` will do) writes nothing, so the next mount retries
      // immediately.
      useMultiplayerStore.setState({ directoryFetchedAtMs: Date.now() });
      if (!res.ok) return;
      const projected = projectDirectoryBody(await res.json());
      // A foreign version or a malformed envelope: resolved but unusable, so
      // the timestamp stands and the sources do not change.
      if (projected === null) return;
      useMultiplayerStore.setState({ directorySources: projected });
    } catch {
      // Offline, aborted, or a body that is not JSON. Silent by contract.
    }
    // `.finally` rather than a `try`/`finally` INSIDE the async body: an async
    // function runs synchronously up to its first `await`, so a `fetch` that
    // threw synchronously would clear `inFlight` before the assignment below
    // ever set it, latching a stale promise for the rest of the session. A
    // `.finally` callback is always a microtask and therefore always runs after
    // the assignment.
  })().finally(() => {
    inFlight = null;
  });
  return inFlight;
}
