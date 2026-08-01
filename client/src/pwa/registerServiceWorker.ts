import { registerSW } from "virtual:pwa-register";
import { isBundledTauriOrigin } from "../services/platform";
import { isMultiplayerGameLive, whenMultiplayerGameEnds } from "./multiplayerGuard";
import { claimServiceWorkerReload, markPendingAutoUpdate } from "./updateMarker";
import {
  claimUpdateStatus,
  setUpdateStatus,
  getUpdateStatus,
  releaseUpdateStatus,
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
let ownsUpdateStatus = false;

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

function claimServiceWorkerUpdateStatus(): boolean {
  if (ownsUpdateStatus) return true;
  ownsUpdateStatus = claimUpdateStatus("serviceWorker");
  return ownsUpdateStatus;
}

function setServiceWorkerUpdateStatus(next: "checking" | "downloading" | "activating" | "deferred"): void {
  if (ownsUpdateStatus) setUpdateStatus(next);
}

function setServiceWorkerDownloadProgress(value: number): void {
  if (ownsUpdateStatus) setDownloadProgress(value);
}

function finishServiceWorkerUpdateStatus(): void {
  if (!ownsUpdateStatus) return;
  setUpdateStatus("idle");
  setDownloadProgress(0);
  releaseUpdateStatus("serviceWorker");
  ownsUpdateStatus = false;
}

function startProgressSimulation() {
  if (!claimServiceWorkerUpdateStatus()) return;
  stopProgressSimulation();
  simulatedProgress = 0;
  setServiceWorkerDownloadProgress(0);
  progressIntervalId = window.setInterval(() => {
    simulatedProgress += (PROGRESS_CEILING - simulatedProgress) * PROGRESS_RATE;
    setServiceWorkerDownloadProgress(simulatedProgress);
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
  setServiceWorkerDownloadProgress(100);
}

function clearActivationTimeout(): void {
  if (activationTimeoutId !== null) {
    window.clearTimeout(activationTimeoutId);
    activationTimeoutId = null;
  }
}

function setActivatingStatus(): void {
  if (!claimServiceWorkerUpdateStatus()) return;
  completeProgress();
  setServiceWorkerUpdateStatus("activating");
  pushUpdateDebug("Service worker is activating.");
  clearActivationTimeout();
  activationTimeoutId = window.setTimeout(() => {
    if (!ownsUpdateStatus || getUpdateStatus() !== "activating") return;
    setUpdateError("Service worker activation timed out after 20s.");
    finishServiceWorkerUpdateStatus();
    console.warn("[phase.rs] Service worker activation timed out; reset update indicator to idle.");
  }, ACTIVATION_TIMEOUT_MS);
}

export function checkForServiceWorkerUpdate(): boolean {
  if (import.meta.env.DEV || isBundledTauriOrigin() || !("serviceWorker" in navigator) || !manualCheckForUpdate) {
    pushUpdateDebug("Manual update check ignored (no service worker support or updater not ready).", "warn");
    return false;
  }

  const wasIdle = getUpdateStatus() === "idle";
  const ownsStatus = claimServiceWorkerUpdateStatus();
  if (wasIdle && ownsStatus) setServiceWorkerUpdateStatus("checking");
  pushUpdateDebug("Manual update check started.");
  manualCheckForUpdate()
    .then(() => {
      if (ownsStatus && getUpdateStatus() === "checking") {
        finishServiceWorkerUpdateStatus();
        pushUpdateDebug("Manual update check finished with no new version.");
      }
    })
    .catch((error: unknown) => {
      if (ownsStatus) {
        setUpdateError(`Manual update check failed: ${formatError(error)}`);
        finishServiceWorkerUpdateStatus();
      }
      console.warn("[phase.rs] Manual service worker update check failed.", error);
    });
  return true;
}

export function registerServiceWorker() {
  // The bundled Tauri origin (tauri.localhost / tauri://) does not reliably
  // support service workers. A remote-origin shell uses the normal PWA updater.
  if (import.meta.env.DEV || isBundledTauriOrigin() || !("serviceWorker" in navigator) || isRegistered) {
    return;
  }

  isRegistered = true;
  pushUpdateDebug("Registering service worker updater.");
  let hasReloadedOnControllerChange = false;

  // A `controllerchange` fires both on a genuine SW update AND on the first
  // `clientsClaim()` of a page that loaded *uncontrolled* — a cold PWA
  // launch, or after the OS evicted the SW. Only the former should reload;
  // capture the controller now so the handler can tell them apart, mirroring
  // the `!navigator.serviceWorker.controller` guard used for `updatefound`.
  const hadControllerAtRegister = !!navigator.serviceWorker.controller;

  navigator.serviceWorker.addEventListener("controllerchange", () => {
    // `hasReloadedOnControllerChange` latches true on the first event so we
    // don't reload twice if the browser fires it again. Set *after* the
    // deferral check so a second controllerchange during a deferred state
    // isn't simply dropped — though in practice once this listener has
    // deferred a reload, there's no way for a second controllerchange to
    // do anything useful (the deferred reload, when it fires, fetches the
    // live SW anyway).
    if (hasReloadedOnControllerChange) return;

    // Initial control handoff — the page loaded with no controller and the
    // SW just claimed it. That is not an update: the page already loaded its
    // current assets from the network, so reloading would only interrupt the
    // user (e.g. mid game-setup). Skip without latching, so a genuine update
    // later this session still reloads.
    if (!hadControllerAtRegister) {
      pushUpdateDebug(
        "Service worker took initial control of an uncontrolled page; skipping reload.",
      );
      return;
    }

    clearActivationTimeout();
    hasReloadedOnControllerChange = true;

    const doReload = () => {
      // Circuit-breaker: allow only the first SW-driven reload per session.
      // iOS standalone PWAs fire repeated spurious `controllerchange` events
      // while the SW lifecycle settles — suppressing every reload after the
      // first breaks the loop that makes the early game unplayable.
      if (!claimServiceWorkerReload()) {
        pushUpdateDebug(
          "Service worker controller changed again this session; suppressing reload to break a loop.",
          "warn",
        );
        return;
      }
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
      if (claimServiceWorkerUpdateStatus()) {
        setServiceWorkerUpdateStatus("deferred");
      }
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
          if (ownsUpdateStatus && getUpdateStatus() === "activating") {
            setUpdateError(`Failed to apply service worker update: ${formatError(error)}`);
            finishServiceWorkerUpdateStatus();
          } else if (ownsUpdateStatus) {
            setUpdateError(`Failed to apply service worker update: ${formatError(error)}`);
          }
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
        if (claimServiceWorkerUpdateStatus()) {
          setServiceWorkerDownloadProgress(0);
          setServiceWorkerUpdateStatus("deferred");
        }
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
        if (!claimServiceWorkerUpdateStatus()) return;

        const newWorker = swRegistration.installing;
        if (!newWorker) {
          releaseUpdateStatus("serviceWorker");
          ownsUpdateStatus = false;
          return;
        }
        setServiceWorkerUpdateStatus("downloading");
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
            if (ownsUpdateStatus && getUpdateStatus() === "activating") {
              finishServiceWorkerUpdateStatus();
              pushUpdateDebug("Service worker activated successfully.");
            }
            return;
          }

          if (newWorker.state === "redundant") {
            stopProgressSimulation();
            clearActivationTimeout();
            if (ownsUpdateStatus) {
              setUpdateError("Service worker became redundant before activation.");
            }
            if (ownsUpdateStatus && getUpdateStatus() !== "checking") {
              finishServiceWorkerUpdateStatus();
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
          releaseUpdateStatus("serviceWorker");
          ownsUpdateStatus = false;
        },
        { once: true },
      );
    },
    onRegisterError(error) {
      if (claimServiceWorkerUpdateStatus()) {
        setUpdateError(`Service worker registration failed: ${formatError(error)}`);
        releaseUpdateStatus("serviceWorker");
        ownsUpdateStatus = false;
      }
      console.error("Service worker registration failed", error);
    },
  });
}
