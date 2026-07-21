// Tauri auto-update integration. Wraps @tauri-apps/plugin-updater into the
// shared `updateStatus` state machine that powers the BuildBadge UI, so the
// desktop and web update flows surface identically.
//
// Tauri serves the app from a custom scheme where service workers don't
// register reliably; updates ship via the Tauri updater (signed artifacts +
// minisign verification) instead.

import type { Update } from "@tauri-apps/plugin-updater";

import { isTauri } from "../services/platform";
import { isMultiplayerGameLive, whenMultiplayerGameEnds } from "./multiplayerGuard";
import { markPendingAutoUpdate } from "./updateMarker";
import {
  claimUpdateStatus,
  clearUpdateError,
  pushUpdateDebug,
  releaseUpdateStatus,
  setDownloadProgress,
  setUpdateError,
  setUpdateStatus,
} from "./updateStatus";

const TAURI_UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;

let initialized = false;
let manualCheck: (() => Promise<void>) | null = null;
let inFlight: Promise<void> | null = null;

/**
 * Latch held while an update has been detected mid-MP-game and is waiting
 * for the game to end. Prevents:
 * - Subsequent interval checks from finding the same update and stacking a
 *   second deferred install (the second `runInstall` would fail because
 *   the bundle is already swapped in by the first).
 * - Manual `↻` clicks from triggering parallel installs during the wait.
 */
let deferredUnsub: (() => void) | null = null;

function setTauriUpdateStatus(next: "checking" | "downloading" | "activating" | "deferred", ownsStatus: boolean): void {
  if (ownsStatus) setUpdateStatus(next);
}

function finishTauriUpdateStatus(ownsStatus: boolean): void {
  if (!ownsStatus) return;
  setUpdateStatus("idle");
  releaseUpdateStatus("tauri");
}

function setTauriDownloadProgress(value: number, ownsStatus: boolean): void {
  if (ownsStatus) setDownloadProgress(value);
}

function formatError(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "Unknown error";
}

async function runInstall(update: Update, ownsStatus: boolean): Promise<void> {
  setTauriUpdateStatus("downloading", ownsStatus);
  setTauriDownloadProgress(0, ownsStatus);

  let totalBytes = 0;
  let receivedBytes = 0;

  try {
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        totalBytes = event.data.contentLength ?? 0;
        pushUpdateDebug(`Tauri update download started (${totalBytes || "unknown"} bytes).`);
        setTauriDownloadProgress(0, ownsStatus);
        return;
      }
      if (event.event === "Progress") {
        receivedBytes += event.data.chunkLength;
        if (totalBytes > 0) {
          setTauriDownloadProgress((receivedBytes / totalBytes) * 100, ownsStatus);
        }
        return;
      }
      if (event.event === "Finished") {
        setTauriDownloadProgress(100, ownsStatus);
        setTauriUpdateStatus("activating", ownsStatus);
        pushUpdateDebug("Tauri update download finished; relaunching.");
      }
    });

    markPendingAutoUpdate();
    const { relaunch } = await import("@tauri-apps/plugin-process");
    await relaunch();
  } catch (error: unknown) {
    if (ownsStatus) setUpdateError(`Tauri update install failed: ${formatError(error)}`);
    setTauriDownloadProgress(0, ownsStatus);
    finishTauriUpdateStatus(ownsStatus);
    console.warn("[phase.rs] Tauri update install failed.", error);
  }
}

async function performCheck(reason: "startup" | "interval" | "manual"): Promise<void> {
  if (deferredUnsub) {
    pushUpdateDebug(
      `Tauri update check (${reason}) skipped — install already deferred for end of multiplayer game.`,
    );
    return;
  }
  if (inFlight) {
    pushUpdateDebug(`Tauri update check (${reason}) skipped — another check is in flight.`);
    return inFlight;
  }

  if (typeof navigator !== "undefined" && "onLine" in navigator && !navigator.onLine) {
    pushUpdateDebug(`Tauri update check (${reason}) skipped — offline.`);
    return;
  }

  const run = (async () => {
    const ownsStatus = claimUpdateStatus("tauri");
    setTauriUpdateStatus("checking", ownsStatus);
    pushUpdateDebug(`Tauri update check started (${reason}).`);

    let update: Update | null = null;
    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      update = await check();
    } catch (error: unknown) {
      if (ownsStatus) setUpdateError(`Tauri update check failed: ${formatError(error)}`);
      finishTauriUpdateStatus(ownsStatus);
      console.warn("[phase.rs] Tauri update check failed.", error);
      return;
    }

    if (!update) {
      finishTauriUpdateStatus(ownsStatus);
      pushUpdateDebug("Tauri update check finished with no new version.");
      return;
    }

    pushUpdateDebug(
      `Tauri update available: v${update.version} (current v${update.currentVersion}).`,
    );
    if (ownsStatus) clearUpdateError();

    if (isMultiplayerGameLive()) {
      pushUpdateDebug(
        "Tauri update available during multiplayer game; deferring install until game ends.",
        "warn",
      );
      setTauriUpdateStatus("deferred", ownsStatus);
      const pending = update;
      deferredUnsub = whenMultiplayerGameEnds(() => {
        deferredUnsub = null;
        pushUpdateDebug("Multiplayer game ended; applying deferred Tauri update.");
        void runInstall(pending, ownsStatus);
      });
      return;
    }

    await runInstall(update, ownsStatus);
  })();

  inFlight = run.finally(() => {
    inFlight = null;
  });
  return inFlight;
}

/**
 * Trigger a manual Tauri update check (called by the BuildBadge ↻ button).
 * Returns true if the check was dispatched, false if not in a Tauri build
 * or the updater hasn't been initialized yet.
 */
export function checkForTauriUpdate(): boolean {
  if (!isTauri() || !manualCheck) {
    pushUpdateDebug(
      "Manual Tauri update check ignored (not a Tauri build or updater not initialized).",
      "warn",
    );
    return false;
  }
  void manualCheck();
  return true;
}

/**
 * Register the Tauri updater. Performs a startup check, then polls hourly.
 * No-op outside Tauri so the call site can stay symmetric with
 * `registerServiceWorker()` in `main.tsx`.
 */
export function registerTauriUpdater(): void {
  if (initialized || !isTauri()) return;
  initialized = true;
  pushUpdateDebug("Registering Tauri updater.");

  manualCheck = () => performCheck("manual");

  void performCheck("startup");

  const intervalId = window.setInterval(() => {
    void performCheck("interval");
  }, TAURI_UPDATE_CHECK_INTERVAL_MS);

  window.addEventListener(
    "beforeunload",
    () => {
      window.clearInterval(intervalId);
      manualCheck = null;
      deferredUnsub?.();
      deferredUnsub = null;
      releaseUpdateStatus("tauri");
    },
    { once: true },
  );
}
