import type { Update } from "@tauri-apps/plugin-updater";

import { isDesktopTauri } from "../services/platform";
import { getEffectiveOffline, subscribeEffectiveOffline } from "../stores/connectivityStore";
import { deferUntilMultiplayerSessionEnds, isMultiplayerGameLive } from "./multiplayerGuard";
import { markPendingAutoUpdate } from "./updateMarker";
import {
  claimUpdateStatus,
  clearUpdateError,
  getUpdateStatus,
  pushUpdateDebug,
  releaseUpdateStatus,
  setDownloadProgress,
  setUpdateError,
  setUpdateStatus,
} from "./updateStatus";

const TAURI_UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;
type LifecycleStatus = "checking" | "deferred" | null;

function isSharedInstallRunning(): boolean {
  const status = getUpdateStatus();
  return status === "downloading" || status === "activating";
}

interface Lifecycle {
  readonly token: number;
  unsubscribe: (() => void) | null;
  intervalId: number | null;
  policyGeneration: number;
  checkActive: boolean;
  checkQueued: boolean;
  deferredUpdate: Update | null;
  deferredCancel: (() => void) | null;
  ownsStatus: boolean;
  status: LifecycleStatus;
  beforeUnload: (() => void) | null;
}

let lifecycle: Lifecycle | null = null;
let lifecycleToken = 0;
let installInFlight = false;

function formatError(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "Unknown error";
}

function isCurrent(candidate: Lifecycle): boolean {
  return lifecycle === candidate && lifecycle.token === candidate.token;
}

function setLifecycleStatus(candidate: Lifecycle, status: Exclude<LifecycleStatus, null>): void {
  if (!isCurrent(candidate)) return;
  if (candidate.ownsStatus && candidate.status !== null && getUpdateStatus() !== candidate.status) {
    candidate.ownsStatus = false;
    candidate.status = null;
  }
  if (!candidate.ownsStatus) {
    if (getUpdateStatus() !== "idle") return;
    candidate.ownsStatus = claimUpdateStatus("tauri");
  }
  if (candidate.ownsStatus) {
    candidate.status = status;
    setUpdateStatus(status);
  }
}

function settleLifecycleStatus(candidate: Lifecycle): void {
  if (!candidate.ownsStatus) return;
  if (getUpdateStatus() === "downloading" || getUpdateStatus() === "activating") {
    candidate.ownsStatus = false;
    candidate.status = null;
    return;
  }
  if (candidate.status !== null && getUpdateStatus() === candidate.status) {
    setUpdateStatus("idle");
    setDownloadProgress(0);
  }
  releaseUpdateStatus("tauri");
  candidate.ownsStatus = false;
  candidate.status = null;
}

async function runInstall(update: Update, ownsStatus: boolean): Promise<void> {
  installInFlight = true;
  if (ownsStatus) {
    setUpdateStatus("downloading");
    setDownloadProgress(0);
  }
  let totalBytes = 0;
  let receivedBytes = 0;
  try {
    await update.downloadAndInstall((event) => {
      if (!ownsStatus) return;
      if (event.event === "Started") {
        totalBytes = event.data.contentLength ?? 0;
        setDownloadProgress(0);
      } else if (event.event === "Progress") {
        receivedBytes += event.data.chunkLength;
        if (totalBytes > 0) setDownloadProgress((receivedBytes / totalBytes) * 100);
      } else if (event.event === "Finished") {
        setDownloadProgress(100);
        setUpdateStatus("activating");
      }
    });
    markPendingAutoUpdate();
    const { relaunch } = await import("@tauri-apps/plugin-process");
    await relaunch();
  } catch (error: unknown) {
    if (ownsStatus) setUpdateError(`Tauri update install failed: ${formatError(error)}`);
    console.warn("[phase.rs] Tauri update install failed.", error);
  } finally {
    if (ownsStatus) {
      setUpdateStatus("idle");
      setDownloadProgress(0);
      releaseUpdateStatus("tauri");
    }
    installInFlight = false;
  }
}

function startInstall(update: Update): Promise<void> {
  const ownsStatus = claimUpdateStatus("tauri");
  if (!ownsStatus) {
    pushUpdateDebug("Tauri update install skipped — another updater owns the status.", "warn");
    return Promise.resolve();
  }
  return runInstall(update, true);
}

function scheduleInstall(candidate: Lifecycle, update: Update): Promise<void> {
  if (!isCurrent(candidate) || getEffectiveOffline() || isSharedInstallRunning()) return Promise.resolve();
  if (!isMultiplayerGameLive()) {
    settleLifecycleStatus(candidate);
    return startInstall(update);
  }

  candidate.deferredUpdate = update;
  setLifecycleStatus(candidate, "deferred");
  const scheduled = deferUntilMultiplayerSessionEnds(() => {
    const pending = candidate.deferredUpdate;
    candidate.deferredUpdate = null;
    candidate.deferredCancel = null;
    if (!isCurrent(candidate) || getEffectiveOffline() || !pending) return;
    settleLifecycleStatus(candidate);
    void startInstall(pending);
  }, "install");
  if (scheduled.deferred) {
    candidate.deferredCancel = scheduled.cancel;
    return Promise.resolve();
  }
  return Promise.resolve();
}

async function runCheck(candidate: Lifecycle, reason: "startup" | "resume" | "interval" | "manual"): Promise<void> {
  if (!isCurrent(candidate) || getEffectiveOffline() || candidate.deferredUpdate || installInFlight || isSharedInstallRunning()) return;
  const checkToken = candidate.token;
  const policyGeneration = candidate.policyGeneration;
  setLifecycleStatus(candidate, "checking");
  pushUpdateDebug(`Tauri update check started (${reason}).`);
  let update: Update | null;
  try {
    const { check } = await import("@tauri-apps/plugin-updater");
    if (!isCurrent(candidate) || checkToken !== candidate.token || policyGeneration !== candidate.policyGeneration || getEffectiveOffline()) return;
    update = await check();
  } catch (error: unknown) {
    if (isCurrent(candidate) && checkToken === candidate.token && policyGeneration === candidate.policyGeneration && !getEffectiveOffline()) {
      setUpdateError(`Tauri update check failed: ${formatError(error)}`);
      console.warn("[phase.rs] Tauri update check failed.", error);
    }
    return;
  }
  if (!isCurrent(candidate) || checkToken !== candidate.token || policyGeneration !== candidate.policyGeneration || getEffectiveOffline()) return;
  if (!update) {
    clearUpdateError();
    pushUpdateDebug("Tauri update check finished with no new version.");
    return;
  }
  clearUpdateError();
  pushUpdateDebug(`Tauri update available: v${update.version} (current v${update.currentVersion}).`);
  await scheduleInstall(candidate, update);
}

function requestCheck(candidate: Lifecycle, reason: "startup" | "resume" | "interval" | "manual"): Promise<void> {
  if (!isCurrent(candidate) || getEffectiveOffline()) return Promise.resolve();
  if (candidate.checkActive) {
    candidate.checkQueued = true;
    return Promise.resolve();
  }
  candidate.checkActive = true;
  return (async () => {
    do {
      candidate.checkQueued = false;
      await runCheck(candidate, reason);
    } while (isCurrent(candidate) && !getEffectiveOffline() && candidate.checkQueued);
    if (isCurrent(candidate)) {
      candidate.checkActive = false;
      if (!candidate.deferredUpdate) settleLifecycleStatus(candidate);
    }
  })();
}

function pauseLifecycle(candidate: Lifecycle): void {
  candidate.policyGeneration += 1;
  if (candidate.intervalId !== null) {
    window.clearInterval(candidate.intervalId);
    candidate.intervalId = null;
  }
  candidate.checkQueued = false;
  candidate.deferredCancel?.();
  candidate.deferredCancel = null;
  candidate.deferredUpdate = null;
  settleLifecycleStatus(candidate);
}

function resumeLifecycle(candidate: Lifecycle): void {
  if (!isCurrent(candidate) || getEffectiveOffline()) return;
  if (candidate.intervalId === null) {
    candidate.intervalId = window.setInterval(() => void requestCheck(candidate, "interval"), TAURI_UPDATE_CHECK_INTERVAL_MS);
  }
  void requestCheck(candidate, "resume");
}

/** Manual BuildBadge entry point. */
export function checkForTauriUpdate(): boolean {
  const candidate = lifecycle;
  if (!candidate || !isCurrent(candidate) || getEffectiveOffline()) {
    pushUpdateDebug("Manual Tauri update check ignored (offline or updater not initialized).", "warn");
    return false;
  }
  void requestCheck(candidate, "manual");
  return true;
}

/** Installs a single connectivity-owned updater lifecycle in desktop builds. */
export function registerTauriUpdater(): void {
  if (import.meta.env.DEV || !isDesktopTauri() || lifecycle) return;
  const candidate: Lifecycle = {
    token: ++lifecycleToken,
    unsubscribe: null,
    intervalId: null,
    policyGeneration: 0,
    checkActive: false,
    checkQueued: false,
    deferredUpdate: null,
    deferredCancel: null,
    ownsStatus: false,
    status: null,
    beforeUnload: null,
  };
  lifecycle = candidate;
  candidate.unsubscribe = subscribeEffectiveOffline((offline) => {
    if (!isCurrent(candidate)) return;
    if (offline) pauseLifecycle(candidate);
    else resumeLifecycle(candidate);
  });
  candidate.beforeUnload = () => disposeCurrentTauriLifecycle(candidate);
  window.addEventListener("beforeunload", candidate.beforeUnload, { once: true });
  if (!getEffectiveOffline()) resumeLifecycle(candidate);
}

/** Test seam and HMR lifecycle owner. Running plugin installs deliberately continue. */
export function disposeTauriUpdater(): void {
  const candidate = lifecycle;
  if (!candidate) return;
  disposeCurrentTauriLifecycle(candidate);
}

function disposeCurrentTauriLifecycle(candidate: Lifecycle): void {
  if (!isCurrent(candidate)) return;
  candidate.policyGeneration += 1;
  lifecycle = null;
  candidate.unsubscribe?.();
  candidate.unsubscribe = null;
  if (candidate.intervalId !== null) window.clearInterval(candidate.intervalId);
  candidate.intervalId = null;
  candidate.deferredCancel?.();
  candidate.deferredCancel = null;
  candidate.deferredUpdate = null;
  if (candidate.beforeUnload) window.removeEventListener("beforeunload", candidate.beforeUnload);
  settleLifecycleStatus(candidate);
}

if (import.meta.hot) import.meta.hot.dispose(disposeTauriUpdater);
