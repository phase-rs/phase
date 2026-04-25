import { registerSW } from "virtual:pwa-register";
import { isTauri } from "../services/sidecar";
import { isMultiplayerGameLive, whenMultiplayerGameEnds } from "./multiplayerGuard";
import { markPendingAutoUpdate } from "./updateMarker";
import {
  setUpdateStatus,
  getUpdateStatus,
  setDownloadProgress,
  pushUpdateDebug,
  setUpdateError,
  clearUpdateError,
} from "./updateStatus";

const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;
const ACTIVATION_TIMEOUT_MS = 20 * 1000;

/** Simulated progress: ticks every 200ms, decelerating toward 95%. */
const PROGRESS_TICK_MS = 200;
const PROGRESS_RATE = 0.08;
const PROGRESS_CEILING = 95;

let isRegistered = false;
let manualCheckForUpdate: (() => Promise<void>) | null = null;
let progressIntervalId: number | null = null;
let activationTimeoutId: number | null = null;
let simulatedProgress = 0;

/**
 * Deferred update closure captured at `onNeedRefresh` time when a MP game
 * is live. Applied when the game ends. Null when nothing is deferred.
 */
let deferredUpdate: (() => void) | null = null;
let deferredUpdateUnsub: (() => void) | null = null;

/**
 * Deferred reload closure captured at `controllerchange` time when a MP
 * game is live. Defense-in-depth for the case where another tab triggered
 * activation of a new SW while this tab is still mid-game.
 */
let deferredReload: (() => void) | null = null;
let deferredReloadUnsub: (() => void) | null = null;

function formatError(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "Unknown error";
}

function startProgressSimulation() {
  stopProgressSimulation();
  simulatedProgress = 0;
  setDownloadProgress(0);
  progressIntervalId = window.setInterval(() => {
    simulatedProgress += (PROGRESS_CEILING - simulatedProgress) * PROGRESS_RATE;
    setDownloadProgress(simulatedProgress);
  }, PROGRESS_TICK_MS);
}

function stopProgressSimulation() {
  if (progressIntervalId !== null) {
    window.clearInterval(progressIntervalId);
    progressIntervalId = null;
  }
}

function completeProgress() {
  stopProgressSimulation();
  setDownloadProgress(100);
}

function clearActivationTimeout(): void {
  if (activationTimeoutId !== null) {
    window.clearTimeout(activationTimeoutId);
    activationTimeoutId = null;
  }
}

function setActivatingStatus(): void {
  completeProgress();
  setUpdateStatus("activating");
  pushUpdateDebug("Service worker is activating.");
  clearActivationTimeout();
  activationTimeoutId = window.setTimeout(() => {
    if (getUpdateStatus() !== "activating") return;
    setUpdateStatus("idle");
    setDownloadProgress(0);
    setUpdateError("Service worker activation timed out after 20s.");
    console.warn("[phase.rs] Service worker activation timed out; reset update indicator to idle.");
  }, ACTIVATION_TIMEOUT_MS);
}

export function checkForServiceWorkerUpdate(): boolean {
  if (import.meta.env.DEV || isTauri() || !("serviceWorker" in navigator) || !manualCheckForUpdate) {
    pushUpdateDebug("Manual update check ignored (no service worker support or updater not ready).", "warn");
    return false;
  }

  setUpdateStatus("checking");
  pushUpdateDebug("Manual update check started.");
  manualCheckForUpdate()
    .then(() => {
      if (getUpdateStatus() === "checking") {
        setUpdateStatus("idle");
        pushUpdateDebug("Manual update check finished with no new version.");
      }
    })
    .catch((error: unknown) => {
      setUpdateStatus("idle");
      setUpdateError(`Manual update check failed: ${formatError(error)}`);
      console.warn("[phase.rs] Manual service worker update check failed.", error);
    });
  return true;
}

export function registerServiceWorker() {
  // Tauri serves the app from a custom scheme (tauri.localhost / tauri://) where
  // service workers don't register reliably; updates ship via the Tauri updater instead.
  if (import.meta.env.DEV || isTauri() || !("serviceWorker" in navigator) || isRegistered) {
    return;
  }

  isRegistered = true;
  pushUpdateDebug("Registering service worker updater.");
  let hasReloadedOnControllerChange = false;

  navigator.serviceWorker.addEventListener("controllerchange", () => {
    // `hasReloadedOnControllerChange` latches true on the first event so we
    // don't reload twice if the browser fires it again. Set *after* the
    // deferral check so a second controllerchange during a deferred state
    // isn't simply dropped — though in practice once this listener has
    // deferred a reload, there's no way for a second controllerchange to
    // do anything useful (the deferred reload, when it fires, fetches the
    // live SW anyway).
    if (hasReloadedOnControllerChange) return;
    clearActivationTimeout();
    hasReloadedOnControllerChange = true;

    const doReload = () => {
      pushUpdateDebug("Service worker controller changed; reloading.");
      window.location.reload();
    };

    // Defer reload until a live multiplayer game ends — reloading mid-game
    // tears down the P2P DataChannel / WebSocket, forcing the opponent
    // into the disconnect grace window and breaking continuity.
    if (isMultiplayerGameLive()) {
      pushUpdateDebug(
        "Service worker controller changed during multiplayer game; deferring reload until game ends.",
        "warn",
      );
      setUpdateStatus("deferred");
      deferredReload = doReload;
      deferredReloadUnsub = whenMultiplayerGameEnds(() => {
        pushUpdateDebug("Multiplayer game ended; applying deferred reload.");
        const fn = deferredReload;
        deferredReload = null;
        deferredReloadUnsub = null;
        fn?.();
      });
      return;
    }

    doReload();
  });

  const updateSW = registerSW({
    immediate: true,
    onNeedRefresh() {
      const applyUpdate = () => {
        pushUpdateDebug("Service worker reported update ready; applying update.");
        markPendingAutoUpdate();
        setActivatingStatus();
        void updateSW(true).catch((error: unknown) => {
          clearActivationTimeout();
          if (getUpdateStatus() === "activating") {
            setUpdateStatus("idle");
            setDownloadProgress(0);
          }
          setUpdateError(`Failed to apply service worker update: ${formatError(error)}`);
          console.warn("[phase.rs] Failed to apply service worker update.", error);
        });
      };

      // Defer activation while a multiplayer game is live. Calling
      // `updateSW(true)` triggers skipWaiting → controllerchange → reload,
      // which would drop the user's live connection mid-game. Leave the new
      // SW parked in "installed" until the game ends, then activate.
      if (isMultiplayerGameLive()) {
        pushUpdateDebug(
          "Update ready during multiplayer game; deferring activation until game ends.",
          "warn",
        );
        // Clear the 20s activation timer that the `installed` statechange
        // started — otherwise the user sees a spurious "activation timed
        // out after 20s" error during a deferral that may last much longer.
        clearActivationTimeout();
        setDownloadProgress(0);
        setUpdateStatus("deferred");
        deferredUpdate = applyUpdate;
        deferredUpdateUnsub?.();
        deferredUpdateUnsub = whenMultiplayerGameEnds(() => {
          pushUpdateDebug("Multiplayer game ended; applying deferred update.");
          const fn = deferredUpdate;
          deferredUpdate = null;
          deferredUpdateUnsub = null;
          fn?.();
        });
        return;
      }

      applyUpdate();
    },
    onRegisteredSW(swUrl, swRegistration) {
      if (!swRegistration) return;
      pushUpdateDebug(`Service worker registered: ${swUrl}`);

      // Surface the download phase — fires when a new SW starts installing
      swRegistration.addEventListener("updatefound", () => {
        if (!navigator.serviceWorker.controller) return;

        const newWorker = swRegistration.installing;
        if (!newWorker) return;
        setUpdateStatus("downloading");
        pushUpdateDebug("Service worker download started.");
        startProgressSimulation();

        newWorker.addEventListener("statechange", () => {
          pushUpdateDebug(`Service worker state changed: ${newWorker.state}`);
          if (newWorker.state === "installed") {
            setActivatingStatus();
            return;
          }

          if (newWorker.state === "activated") {
            clearActivationTimeout();
            clearUpdateError();
            if (getUpdateStatus() === "activating") {
              setUpdateStatus("idle");
              setDownloadProgress(0);
              pushUpdateDebug("Service worker activated successfully.");
            }
            return;
          }

          if (newWorker.state === "redundant") {
            stopProgressSimulation();
            clearActivationTimeout();
            setUpdateError("Service worker became redundant before activation.");
            if (getUpdateStatus() !== "checking") {
              setUpdateStatus("idle");
              setDownloadProgress(0);
            }
          }
        });
      });

      const doUpdate = async (probeScript: boolean) => {
        if (swRegistration.installing) return;

        if (probeScript) {
          if ("onLine" in navigator && !navigator.onLine) return;

          try {
            const response = await fetch(swUrl, {
              cache: "no-store",
              headers: { "cache-control": "no-cache" },
            });
            if (response.status !== 200) {
              setUpdateError(`SW script probe returned HTTP ${response.status}.`);
              return;
            }
          } catch {
            setUpdateError("SW script probe failed before update check.");
            return;
          }
        }

        await swRegistration.update();
        clearUpdateError();
      };

      const autoCheck = async () => {
        try {
          await doUpdate(true);
        } catch (error: unknown) {
          setUpdateError(`Automatic update check failed: ${formatError(error)}`);
          console.warn("[phase.rs] Automatic service worker update check failed.", error);
        }
      };

      const handleVisibilityChange = () => {
        if (document.visibilityState !== "visible") return;
        void autoCheck();
      };

      manualCheckForUpdate = () => doUpdate(false);
      void autoCheck();
      const intervalId = window.setInterval(() => {
        void autoCheck();
      }, UPDATE_CHECK_INTERVAL_MS);
      document.addEventListener("visibilitychange", handleVisibilityChange);

      window.addEventListener(
        "beforeunload",
        () => {
          window.clearInterval(intervalId);
          stopProgressSimulation();
          clearActivationTimeout();
          document.removeEventListener("visibilitychange", handleVisibilityChange);
          manualCheckForUpdate = null;
          deferredUpdateUnsub?.();
          deferredReloadUnsub?.();
        },
        { once: true },
      );
    },
    onRegisterError(error) {
      setUpdateError(`Service worker registration failed: ${formatError(error)}`);
      console.error("Service worker registration failed", error);
    },
  });
}
