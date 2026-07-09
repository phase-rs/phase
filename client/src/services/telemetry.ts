/**
 * First-party, identity-free crash & usage telemetry.
 *
 * Design (see docs/telemetry-proposal.md §2, §6):
 * - **Fail open.** Telemetry must never affect gameplay: every send is
 *   fire-and-forget, dropped silently on failure, and wrapped so nothing here
 *   can throw into app code.
 * - **Build gate.** With no `__TELEMETRY_URL__` define (local dev, self-hosted
 *   builds) this module is a permanent no-op — `trackEvent`/`flushNow` return
 *   immediately and nothing is ever queued.
 * - **Runtime gate.** The `telemetryEnabled` preference (default on) is checked
 *   at enqueue time, so a mid-session toggle takes effect immediately.
 * - **No identity.** No install id, no IP, no user-agent — the events carry only
 *   the operational fields their call sites pass. The batch envelope adds build
 *   metadata (version, hash, release, platform) and nothing request-derived.
 */
import { usePreferencesStore } from "../stores/preferencesStore";
import { isTauri } from "./sidecar";

/** Max characters retained for any single string field (defence against a
 *  runaway panic message or stack frame bloating a batch). */
const MAX_STRING_LEN = 300;
/** Flush once the queue reaches this many events. */
const FLUSH_AT_COUNT = 20;
/** Flush this long after the first event was queued, if the count trigger
 *  hasn't already fired. */
const FLUSH_AFTER_MS = 10_000;
/** Hard per-session cap on total events (abuse / runaway-loop backstop). A
 *  navigation-heavy session can spend up to half this budget on `route_view`s
 *  before hitting the shared ceiling; that trade is accepted — the reserve
 *  exists for crash events (`js_error`/`engine_panic`), which fire early rather
 *  than late in a session. */
const MAX_EVENTS_PER_SESSION = 100;
/** Hard per-session cap on specific event types — a loop of any one kind must
 *  not drown the batch. Declarative so a new capped event is one entry, not a
 *  new counter+constant pair. Events absent here are bounded only by
 *  {@link MAX_EVENTS_PER_SESSION}. */
const PER_EVENT_CAPS: Record<string, number> = {
  js_error: 10,
  card_report: 10,
  route_view: 50,
};

/** A queued event: the caller's fields plus the event name and a client
 *  timestamp. Values are truncated (strings) at enqueue. */
interface QueuedEvent {
  event: string;
  ts: number;
  [field: string]: unknown;
}

/** The ingest URL, resolved once from the build define. Empty ⇒ permanent
 *  no-op. */
const TELEMETRY_URL = __TELEMETRY_URL__;
/** True when telemetry can never send in this build. */
const DISABLED = TELEMETRY_URL === "";

const queue: QueuedEvent[] = [];
let flushTimer: ReturnType<typeof setTimeout> | null = null;
let eventsThisSession = 0;
/** Per-event-type send counts this session, checked against {@link PER_EVENT_CAPS}. */
const perEventCounts: Record<string, number> = {};

/** Truncate a single string field to the session cap. Non-strings pass through
 *  untouched. */
function truncate(value: unknown): unknown {
  if (typeof value === "string" && value.length > MAX_STRING_LEN) {
    return value.slice(0, MAX_STRING_LEN);
  }
  return value;
}

/** Coarse platform tag — `"tauri"` in the desktop webview, `"web"` otherwise.
 *  No OS/version detail (that would edge toward fingerprinting). */
function platform(): string {
  return isTauri() ? "tauri" : "web";
}

/**
 * Enqueue a telemetry event. Silently drops when telemetry is build-disabled,
 * runtime-disabled, or over a session cap. Never throws.
 */
export function trackEvent(name: string, fields: Record<string, unknown> = {}): void {
  try {
    if (DISABLED) return;
    if (!usePreferencesStore.getState().telemetryEnabled) return;
    if (eventsThisSession >= MAX_EVENTS_PER_SESSION) return;
    const cap = PER_EVENT_CAPS[name];
    if (cap !== undefined && (perEventCounts[name] ?? 0) >= cap) return;

    const event: QueuedEvent = { event: name, ts: Date.now() };
    for (const [key, value] of Object.entries(fields)) {
      event[key] = truncate(value);
    }

    queue.push(event);
    eventsThisSession += 1;
    perEventCounts[name] = (perEventCounts[name] ?? 0) + 1;

    if (queue.length >= FLUSH_AT_COUNT) {
      flushNow();
    } else if (flushTimer === null) {
      flushTimer = setTimeout(flushNow, FLUSH_AFTER_MS);
    }
  } catch {
    // Telemetry must never surface an error into app code.
  }
}

/** Build the per-batch envelope (stamped once), per proposal §3. */
function buildBatch(events: QueuedEvent[]): string {
  return JSON.stringify({
    schema: 1,
    app_version: __APP_VERSION__,
    build_hash: __BUILD_HASH__,
    release: __IS_RELEASE_BUILD__,
    platform: platform(),
    events,
  });
}

/**
 * Flush the queue immediately. Exported for the chunk-reload path, which must
 * drain before `window.location.reload()`. Safe to call at any time — a no-op
 * when the queue is empty or telemetry is disabled.
 */
export function flushNow(): void {
  try {
    if (flushTimer !== null) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    if (DISABLED || queue.length === 0) return;

    const batch = queue.splice(0, queue.length);
    const body = buildBatch(batch);

    // A bare string body ⇒ Content-Type: text/plain ⇒ no CORS preflight.
    if (typeof navigator !== "undefined" && typeof navigator.sendBeacon === "function") {
      if (navigator.sendBeacon(TELEMETRY_URL, body)) return;
    }
    // Fallback: keepalive fetch survives the page teardown sendBeacon covers.
    // Errors are swallowed — telemetry is fire-and-forget.
    void fetch(TELEMETRY_URL, {
      method: "POST",
      keepalive: true,
      body,
      headers: { "Content-Type": "text/plain" },
    }).catch(() => {});
  } catch {
    // Never throw into app code.
  }
}

let lifecycleInstalled = false;

/**
 * Register the page-lifecycle flush hooks (`visibilitychange → hidden`,
 * `pagehide`) so a queued batch is not lost when the user backgrounds or closes
 * the tab. Idempotent; a no-op in build-disabled telemetry. Called by
 * `installTelemetry`.
 */
export function installTelemetryLifecycle(): void {
  if (DISABLED || lifecycleInstalled || typeof window === "undefined") return;
  lifecycleInstalled = true;

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flushNow();
  });
  window.addEventListener("pagehide", flushNow);
}
