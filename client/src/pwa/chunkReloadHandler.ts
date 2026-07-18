import { isMultiplayerGameLive, whenMultiplayerGameEnds } from "./multiplayerGuard";
import { pushUpdateDebug, setUpdateError, setUpdateStatus } from "./updateStatus";
import { flushNow, trackEvent } from "../services/telemetry";

/**
 * Vite fires `vite:preloadError` when a lazy-imported chunk fails to load —
 * the canonical "user had a tab open across a deploy and the hashed chunk
 * filename changed" case. The service-worker updater handles the *worker*
 * half of post-deploy recovery (new SW activates → reload); this handles
 * the *chunk* half (running JS tries to import a chunk that no longer
 * exists on the server / in the precache → import rejects).
 *
 * Without this listener the user sees a partially-broken UI and must
 * hard-refresh manually. With it, the app self-recovers.
 *
 * Multiplayer safety: a chunk-load failure mid-lobby or mid-game would
 * still reload and drop the P2P/WebSocket connection. We mirror the
 * service-worker updater's deferral by parking the reload until
 * `whenMultiplayerGameEnds()` fires, so the running game isn't killed for
 * everyone else in it. The user lives with a degraded UI for the rest of
 * the game (one missing lazy route), but the game itself stays alive and
 * the reconnect-on-end story remains intact.
 *
 * Loop breaker: when the failure *persists* (bad cached edge variant,
 * broken client cache), reloading forever turns one broken client into a
 * telemetry storm — observed 2026-07-18 as ~6,500 events/hour from a
 * handful of clients. Reloads are counted per failing chunk in
 * `sessionStorage` (survives reload in the same tab; keys self-reset when
 * chunk hashes change on the next deploy). Only *executed* reloads count —
 * queuing a deferred multiplayer reload does not — so repeated failures
 * during one game can never trip the breaker. After
 * {@link RELOAD_GUARD_MAX} reloads inside {@link RELOAD_GUARD_WINDOW_MS},
 * the handler stops reloading, surfaces the failure via `setUpdateError`,
 * and reports what the failing URL actually returns (status +
 * `cf-cache-status` + `cf-ray` + SW-controlled bit) so a stuck client
 * diagnoses itself in telemetry.
 */
let isInstalled = false;
let deferredReload: (() => void) | null = null;
let deferredReloadUnsub: (() => void) | null = null;

/** Reloads allowed per failing chunk within {@link RELOAD_GUARD_WINDOW_MS}. */
const RELOAD_GUARD_MAX = 2;
const RELOAD_GUARD_WINDOW_MS = 10 * 60 * 1000;

interface ReloadGuardState {
  count: number;
  firstAt: number;
}

/** Read the guard state for a chunk key; `null` when absent, expired, or
 *  storage is unavailable (lockdown/embedded contexts — the breaker then
 *  degrades to the pre-guard always-reload behavior). */
function readReloadGuard(key: string): ReloadGuardState | null {
  try {
    const raw = window.sessionStorage.getItem(key);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as ReloadGuardState;
    if (typeof parsed.count !== "number" || typeof parsed.firstAt !== "number") return null;
    if (Date.now() - parsed.firstAt > RELOAD_GUARD_WINDOW_MS) return null;
    return parsed;
  } catch {
    return null;
  }
}

/** Count an *executed* reload (called immediately before `location.reload()`,
 *  so deferred reloads count when they fire, not when queued). */
function recordReload(key: string): void {
  try {
    const prior = readReloadGuard(key);
    const next: ReloadGuardState = prior
      ? { count: prior.count + 1, firstAt: prior.firstAt }
      : { count: 1, firstAt: Date.now() };
    window.sessionStorage.setItem(key, JSON.stringify(next));
  } catch {
    // Storage unavailable — degrade to always-reload.
  }
}

/** Best-effort diagnosis of a persistently failing chunk: refetch it and
 *  report status + Cloudflare cache/colo headers (same-origin, all readable)
 *  plus whether a service worker controls this page. Runs only on the
 *  loop-abort path, where no reload is pending — never delays recovery. */
async function probeFailedChunk(message: string): Promise<Record<string, unknown>> {
  const probeSw =
    typeof navigator !== "undefined" &&
    "serviceWorker" in navigator &&
    navigator.serviceWorker.controller !== null
      ? 1
      : 0;
  const url = /https?:\/\/[^\s'"()]+/.exec(message)?.[0];
  if (!url) return { probe_sw: probeSw };
  try {
    const res = await fetch(url, { cache: "no-store", signal: AbortSignal.timeout(3000) });
    return {
      probe_status: res.status,
      probe_cache: res.headers.get("cf-cache-status") ?? "",
      probe_ray: res.headers.get("cf-ray") ?? "",
      probe_sw: probeSw,
    };
  } catch {
    // Fetch threw or timed out — status 0 mirrors the browser's own
    // network-error convention.
    return { probe_status: 0, probe_sw: probeSw };
  }
}

export function installChunkReloadHandler(): void {
  if (isInstalled) return;
  isInstalled = true;

  window.addEventListener("vite:preloadError", (event) => {
    // Suppressing the default error keeps the unhandled-rejection out of
    // the console — we're handling it by reloading (or deferring).
    event.preventDefault();

    // The failed chunk identifier lives in the event's `.payload` Error
    // (its message carries the failing URL). Best-effort; truncated at enqueue.
    const chunk = (event as { payload?: Error }).payload?.message;
    const guardKey = `chunk-reload:${chunk ?? "unknown"}`;

    if ((readReloadGuard(guardKey)?.count ?? 0) >= RELOAD_GUARD_MAX) {
      // Loop breaker: two reloads inside the window didn't fix this chunk, so
      // a third won't either. Stop reloading and don't queue a new deferred
      // reload — an already-queued one from an earlier failure is left intact
      // (it's a single reload and may succeed after the game ends).
      setUpdateError("App update failed to load. Please refresh the page.");
      void probeFailedChunk(chunk ?? "").then((probe) => {
        trackEvent("chunk_reload", { reason: "loop-abort", deferred: false, chunk, ...probe });
        flushNow();
      });
      return;
    }

    const deferred = isMultiplayerGameLive();
    trackEvent("chunk_reload", { reason: "preload-error", deferred, chunk });

    const doReload = () => {
      pushUpdateDebug("Chunk preload failed; reloading to pick up new bundle.", "warn");
      // Drain the telemetry queue before navigating away.
      flushNow();
      recordReload(guardKey);
      window.location.reload();
    };

    if (deferred) {
      pushUpdateDebug(
        "Chunk preload failed during multiplayer game; deferring reload until game ends.",
        "warn",
      );
      setUpdateStatus("deferred");
      // First-failure-wins: if a second chunk fails before the game ends,
      // we already have a reload queued — replacing it changes nothing.
      if (deferredReload) return;
      deferredReload = doReload;
      deferredReloadUnsub = whenMultiplayerGameEnds(() => {
        const fn = deferredReload;
        deferredReload = null;
        deferredReloadUnsub = null;
        fn?.();
      });
      return;
    }

    doReload();
  });

  window.addEventListener(
    "beforeunload",
    () => {
      deferredReloadUnsub?.();
      deferredReloadUnsub = null;
      deferredReload = null;
    },
    { once: true },
  );
}
