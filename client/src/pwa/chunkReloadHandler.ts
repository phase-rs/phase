import { isMultiplayerGameLive, whenMultiplayerGameEnds } from "./multiplayerGuard";
import { pushUpdateDebug, setUpdateStatus } from "./updateStatus";
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
 */
let isInstalled = false;
let deferredReload: (() => void) | null = null;
let deferredReloadUnsub: (() => void) | null = null;

export function installChunkReloadHandler(): void {
  if (isInstalled) return;
  isInstalled = true;

  window.addEventListener("vite:preloadError", (event) => {
    // Suppressing the default error keeps the unhandled-rejection out of
    // the console — we're handling it by reloading (or deferring).
    event.preventDefault();

    const deferred = isMultiplayerGameLive();
    // The failed chunk identifier lives in the event's `.payload` Error
    // (its message carries the failing URL). Best-effort; truncated at enqueue.
    const chunk = (event as { payload?: Error }).payload?.message;
    trackEvent("chunk_reload", { reason: "preload-error", deferred, chunk });

    const doReload = () => {
      pushUpdateDebug("Chunk preload failed; reloading to pick up new bundle.", "warn");
      // Drain the telemetry queue before navigating away.
      flushNow();
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
