/**
 * The client's write side of the lobby directory contract: identity-free
 * connect-outcome reports, batched to `POST /servers/metrics`.
 *
 * This is the opposite direction of `serverDirectory.ts` (which READS
 * `GET /servers`), and it is a separate module for that reason: the directory
 * service documents its "ONLY mutable state" as one `inFlight` promise, which a
 * report queue, a flush timer and two page-lifecycle listeners would falsify.
 *
 * Design, mirroring `services/telemetry.ts` (the first-party telemetry pipeline
 * this is modelled on) rather than reusing it — that queue is bound to a
 * different endpoint behind a `__TELEMETRY_URL__` build gate that is empty in
 * self-hosted builds, and the directory is a separate service that must still
 * receive evidence there:
 * - **Fail open.** Every send is fire-and-forget, dropped silently on failure,
 *   and wrapped so nothing here can throw into app code. Nothing awaits it.
 * - **Runtime gate.** The existing `telemetryEnabled` preference (default on) is
 *   checked at ENQUEUE, so a mid-session opt-out takes effect immediately.
 * - **No identity.** A report carries a URL, an outcome and a latency. No
 *   install id, no display name, no build metadata — unlike the telemetry
 *   envelope, which stamps `app_version`/`build_hash`/`platform`. The Worker's
 *   sanitiser would discard those fields anyway, and sending them would be
 *   gratuitous fingerprinting surface.
 * - **Announced keys only.** The caller passes the directory row's ANNOUNCED
 *   url; the Worker drops a report for any URL with no `servers` row, so a
 *   preset or hand-added (possibly private, possibly LAN) address never leaves
 *   this machine.
 */
import { usePreferencesStore } from "../stores/preferencesStore";
import { directoryUrl } from "./serverDirectory";

/** What a client observed about one server. Mirrors `ProbeOutcome` in
 *  `lobby-worker/src/directory.ts`; a value outside this union is dropped by
 *  the Worker's sanitiser. */
export type ProbeOutcome =
  | "connect_ok"
  | "connect_fail"
  // MIRRORED, NOT PRODUCED. No client producer for the two game outcomes exists
  // in this build (Final-Report follow-up F1): emitting `game_completed`
  // without `game_abandoned` would pin every reporting server's
  // `completion_rate` at 1.0, because the Worker's fold bumps `games_started`
  // on both. They are mirrored so the drift gate below compares the whole
  // contract rather than the half this build happens to use.
  | "game_completed"
  | "game_abandoned";

/** One report. Mirrors `ServerProbeReport` in `lobby-worker/src/directory.ts`
 *  field-for-field. The duplication is kept honest in BOTH directions:
 *  {@link SERVER_PROBE_REPORT_KEYS} ties this interface to a runtime array that
 *  the mirror gate in `__tests__/serverMetrics.test.ts` compares against the
 *  Worker's own source text, so neither a field added here nor a field added
 *  there can drift unnoticed. */
export interface ServerProbeReport {
  /** The ANNOUNCED url — `DirectoryRow.url`, the `servers` PRIMARY KEY. Never
   *  the client-canonical `LobbySource.url`, which is not invertible back to
   *  it (see `serverDirectory.ts`'s `DirectorySource` doc). */
  url: string;
  outcome: ProbeOutcome;
  /** Connect latency, sent only with `connect_ok`. */
  rtt_ms?: number;
  /** Mirrored only — see {@link ProbeOutcome}; nothing here ever sets it. */
  game_code?: string;
}

/**
 * The runtime field list and its compile-time exhaustiveness assertion.
 *
 * BOTH halves are required and neither subsumes the other, exactly as
 * `WIRE_SCORE_KEYS` in `serverDirectory.ts` needs both: `satisfies` rejects an
 * array member that is not a key of {@link ServerProbeReport}, while the
 * `Exclude` assertion rejects a key of the interface that is missing from the
 * array. Only the second catches a field added to the client mirror and
 * forgotten here — which is the direction a Worker-text comparison alone
 * cannot see.
 */
export const SERVER_PROBE_REPORT_KEYS = [
  "url",
  "outcome",
  "rtt_ms",
  "game_code",
] as const satisfies readonly (keyof ServerProbeReport)[];
type MissingProbeReportKey = Exclude<
  keyof ServerProbeReport,
  (typeof SERVER_PROBE_REPORT_KEYS)[number]
>;
export const SERVER_PROBE_REPORT_KEYS_ARE_EXHAUSTIVE: [MissingProbeReportKey] extends [never]
  ? true
  : never = true;

/** Envelope discriminant. `sanitizeMetricsBatch` returns `[]` for any other
 *  value, so a drift here is a silent 204 with zero ingest — which is why
 *  V-M9 compares this number against the Worker's guard. Mirrors the
 *  `batch.schema !== 1` check in `lobby-worker/src/directory.ts`. */
export const METRICS_SCHEMA = 1;

/** Mirrors `MAX_REPORTS_PER_BATCH` in `lobby-worker/src/directory.ts`. */
export const MAX_REPORTS_PER_BATCH = 50;
/** Mirrors `MAX_RTT_MS` in `lobby-worker/src/directory.ts`. */
export const MAX_RTT_MS = 60_000;
/** Mirrors `MAX_GAME_CODE_LEN` in `lobby-worker/src/directory.ts`. Mirrored,
 *  not used: no `game_code` is ever sent (F1). */
export const MAX_GAME_CODE_LEN = 16;
/** Mirrors `MAX_METRICS_BYTES` in `lobby-worker/src/directory.ts`. */
export const MAX_METRICS_BYTES = 32 * 1024;

/**
 * The runtime outcome list and its compile-time exhaustiveness assertion.
 *
 * BOTH halves are required, exactly as `WIRE_SCORE_KEYS` in
 * `serverDirectory.ts` needs both: `satisfies` rejects an array member that is
 * not a `ProbeOutcome`, while the `Exclude` assertion rejects a `ProbeOutcome`
 * missing from the array. Only the second catches an outcome added to the union
 * and forgotten here.
 */
export const PROBE_OUTCOMES = [
  "connect_ok",
  "connect_fail",
  "game_completed",
  "game_abandoned",
] as const satisfies readonly ProbeOutcome[];
type MissingProbeOutcome = Exclude<ProbeOutcome, (typeof PROBE_OUTCOMES)[number]>;
export const PROBE_OUTCOMES_ARE_EXHAUSTIVE: [MissingProbeOutcome] extends [never]
  ? true
  : never = true;

/** Flush once the queue reaches this many reports. Deliberately below the
 *  Worker's `MAX_REPORTS_PER_BATCH`, so a single flush is never truncated by
 *  its `.slice`. */
const FLUSH_AT_COUNT = 20;
/** Flush this long after the first report was queued, if the count trigger
 *  hasn't already fired. */
const FLUSH_AFTER_MS = 10_000;
/** Hard per-session cap (abuse / runaway-loop backstop), mirroring
 *  `telemetry.ts`'s `MAX_EVENTS_PER_SESSION`. */
const MAX_REPORTS_PER_SESSION = 200;

const queue: ServerProbeReport[] = [];
let flushTimer: ReturnType<typeof setTimeout> | null = null;
let reportsThisSession = 0;
let lifecycleInstalled = false;

/** The `visibilitychange` handler, held at module scope rather than inlined at
 *  registration: `removeEventListener` matches on identity, so an inline arrow
 *  would be unremovable and {@link __resetServerMetricsForTests} could only
 *  pretend to undo an install. Paired with {@link lifecycleInstalled}. */
function onVisibilityChange(): void {
  if (document.visibilityState === "hidden") flushMetricsNow();
}

/**
 * Drop every piece of this module's state: the queue, any armed timer, the
 * per-session counter, and the lifecycle latch TOGETHER WITH the two listeners
 * it guards.
 *
 * TEST-ONLY. Module state that no case can clear makes a suite order-dependent
 * — the session cap is consumed cumulatively across cases, and
 * `installServerMetricsLifecycle`'s latch means whichever case installs first
 * leaves listeners registered for the rest of the file. Production has exactly
 * one lifetime per page load and never needs this.
 */
export function __resetServerMetricsForTests(): void {
  queue.length = 0;
  if (flushTimer !== null) {
    clearTimeout(flushTimer);
    flushTimer = null;
  }
  reportsThisSession = 0;
  // The listeners come off with the latch. Clearing the latch alone would let
  // the next install register a SECOND pair, so a single hide would drain
  // twice — and the "installs once" assertion would pass while the module
  // leaked a listener per reset.
  if (lifecycleInstalled && typeof window !== "undefined") {
    document.removeEventListener("visibilitychange", onVisibilityChange);
    window.removeEventListener("pagehide", flushMetricsNow);
  }
  lifecycleInstalled = false;
}

/**
 * The metrics endpoint for this build's official lobby.
 *
 * One path segment over {@link directoryUrl}, so the official-host derivation
 * is stated exactly once. A self-hosted build whose official URL is its own
 * phase-server will POST to a server with no such route; the response is
 * discarded either way — the same intended non-degradation `serverDirectory.ts`
 * documents for the read side.
 */
export function metricsUrl(): string {
  return `${directoryUrl()}/metrics`;
}

/**
 * Record one connect attempt against a directory-listed server.
 *
 * `announcedUrl` MUST be the row's announced key: the Worker drops a report for
 * any URL with no `servers` row. The outcome parameter is narrowed to the two
 * connect cases, so a call site that cannot observe a game outcome cannot type
 * one.
 *
 * Silently drops when the user has opted out or the session cap is reached.
 * Never throws.
 */
export function reportConnectOutcome(
  announcedUrl: string,
  outcome: "connect_ok" | "connect_fail",
  rttMs?: number,
): void {
  try {
    if (!usePreferencesStore.getState().telemetryEnabled) return;
    if (reportsThisSession >= MAX_REPORTS_PER_SESSION) return;

    const report: ServerProbeReport = { url: announcedUrl, outcome };
    // The client applies the Worker's own rule (`sanitizeMetricsBatch` rounds
    // and clamps identically) rather than sending a value the Worker would
    // silently reshape. A non-finite latency yields NO field at all — never a
    // null, which the sanitiser would drop anyway and which would misreport a
    // completed connect as having an unmeasurable one.
    if (outcome === "connect_ok" && typeof rttMs === "number" && Number.isFinite(rttMs)) {
      report.rtt_ms = Math.round(Math.min(Math.max(rttMs, 0), MAX_RTT_MS));
    }

    queue.push(report);
    reportsThisSession += 1;

    if (queue.length >= FLUSH_AT_COUNT) {
      flushMetricsNow();
    } else if (flushTimer === null) {
      flushTimer = setTimeout(flushMetricsNow, FLUSH_AFTER_MS);
    }
  } catch {
    // Metrics must never surface an error into app code.
  }
}

/**
 * Send the queued reports immediately. Safe to call at any time — a no-op when
 * the queue is empty.
 *
 * Splices at most one Worker-sized batch per call and re-arms the timer if
 * anything remains, so a burst can never present the Worker a batch its own
 * `.slice` would truncate.
 */
export function flushMetricsNow(): void {
  try {
    if (flushTimer !== null) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    if (queue.length === 0) return;

    const batch = queue.splice(0, MAX_REPORTS_PER_BATCH);
    const body = JSON.stringify({ schema: METRICS_SCHEMA, reports: batch });
    const url = metricsUrl();

    // A bare STRING body ⇒ `text/plain` ⇒ CORS-safelisted ⇒ no preflight. A
    // `Blob` would carry a content type and cost a round trip; the DO reads
    // `request.text()` and never consults the header either way.
    if (typeof navigator !== "undefined" && typeof navigator.sendBeacon === "function") {
      if (navigator.sendBeacon(url, body)) {
        if (queue.length > 0) flushTimer = setTimeout(flushMetricsNow, FLUSH_AFTER_MS);
        return;
      }
    }
    // Fallback: a keepalive fetch survives the page teardown `sendBeacon`
    // covers. Errors are swallowed — reporting is fire-and-forget.
    void fetch(url, {
      method: "POST",
      keepalive: true,
      body,
      headers: { "Content-Type": "text/plain" },
    }).catch(() => {});
    if (queue.length > 0) flushTimer = setTimeout(flushMetricsNow, FLUSH_AFTER_MS);
  } catch {
    // Never throw into app code.
  }
}

/**
 * Register the page-lifecycle drain hooks (`visibilitychange → hidden`,
 * `pagehide`) so a queued batch is not lost when the tab is backgrounded or
 * closed. Idempotent.
 *
 * Called from `MultiplayerPage`'s mount effect rather than at app boot — a
 * player who never opens multiplayer registers no listeners and queues nothing,
 * the same rule `LobbyView` states for the directory fetch itself. Reports
 * enqueued before this runs are not lost: they still flush on the count and
 * timer triggers.
 */
export function installServerMetricsLifecycle(): void {
  if (lifecycleInstalled || typeof window === "undefined") return;
  lifecycleInstalled = true;

  document.addEventListener("visibilitychange", onVisibilityChange);
  window.addEventListener("pagehide", flushMetricsNow);
}
