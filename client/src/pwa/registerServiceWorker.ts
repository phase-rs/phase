import { registerSW } from "virtual:pwa-register";

import { isBundledTauriOrigin } from "../services/platform";
import { markRemoteLoadOk } from "../services/legacyMigration";
import { getEffectiveOffline, subscribeEffectiveOffline } from "../stores/connectivityStore";
import { deferUntilMultiplayerSessionEnds, isMultiplayerGameLive } from "./multiplayerGuard";
import { claimServiceWorkerReload, hasServiceWorkerReloadBudget, markPendingAutoUpdate } from "./updateMarker";
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

const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;
const ACTIVATION_TIMEOUT_MS = 20 * 1000;
const PROGRESS_TICK_MS = 200;
const PROGRESS_RATE = 0.08;
const PROGRESS_CEILING = 95;
const BUILD_CHECK_TIMEOUT_MS = 5_000;

type Registration = ServiceWorkerRegistration;
type LifecycleStatus = "checking" | "deferred" | null;

export type AppShellReadiness =
  | { readonly status: "ready" }
  | {
      readonly status: "reload-required";
      readonly reason: "deferred-reload" | "controller-mismatch";
    }
  | {
      readonly status: "not-ready";
      readonly reason:
        | "update-in-progress"
        | "insecure-context"
        | "service-worker-unsupported"
        | "lifecycle-unavailable"
        | "active-worker-unavailable"
        | "controller-unavailable"
        | "shell-cache-unavailable"
        | "remote-load-marker-unavailable"
        | "lifecycle-changed";
    };

interface RegistrationAttempt {
  readonly token: number;
  terminal: boolean;
}

interface Lifecycle {
  readonly token: number;
  unsubscribe: (() => void) | null;
  registration: Registration | null;
  swUrl: string | null;
  attempt: RegistrationAttempt | null;
  successfulAttempt: RegistrationAttempt | null;
  retryRegistration: boolean;
  policyGeneration: number;
  intervalId: number | null;
  visibilityListener: (() => void) | null;
  probeAbort: AbortController | null;
  checkActive: boolean;
  checkQueued: boolean;
  queuedProbe: boolean;
  deferredReload: (() => void) | null;
  deferredReloadCancel: (() => void) | null;
  lifecycleStatus: LifecycleStatus;
  ownsLifecycleStatus: boolean;
  beforeUnload: (() => void) | null;
}

interface Installation {
  readonly token: number;
  ownsStatus: boolean;
}

let lifecycle: Lifecycle | null = null;
let lifecycleToken = 0;
let attemptToken = 0;
let installationToken = 0;
let activeInstallation: Installation | null = null;
let progressIntervalId: number | null = null;
let activationTimeoutId: number | null = null;
let simulatedProgress = 0;
let markerProbeSequence = 0;
// Notified by `submitRetainedReload` only once the reload budget is claimed
// and `window.location.reload()` is about to run.
const needReloadListeners = new Set<() => void>();

interface ShellProbe {
  readonly candidate: Lifecycle;
  readonly registration: Registration;
  readonly active: ServiceWorker;
}

function notReady(reason: Extract<AppShellReadiness, { status: "not-ready" }>["reason"]): AppShellReadiness {
  return { status: "not-ready", reason };
}

function markerProbeNonce(): string {
  const sequence = ++markerProbeSequence;
  try {
    const random = globalThis.crypto?.randomUUID?.();
    return random ? `${sequence}-${random}` : String(sequence);
  } catch {
    return String(sequence);
  }
}

function readinessGate(): AppShellReadiness | ShellProbe {
  const candidate = lifecycle;
  if (candidate?.deferredReload) return { status: "reload-required", reason: "deferred-reload" };

  const registration = candidate?.registration;
  if (registration && (registration.installing || registration.waiting)) {
    return notReady("update-in-progress");
  }
  if (!window.isSecureContext) return notReady("insecure-context");
  if (!("serviceWorker" in navigator)) return notReady("service-worker-unsupported");
  if (!candidate || !registration) return notReady("lifecycle-unavailable");
  const active = registration.active;
  if (!active) return notReady("active-worker-unavailable");
  if (active.state !== "activated") return notReady("update-in-progress");
  const controller = navigator.serviceWorker.controller;
  if (!controller) return notReady("controller-unavailable");
  if (controller !== active) return { status: "reload-required", reason: "controller-mismatch" };
  return { candidate, registration, active };
}

function isShellProbe(value: AppShellReadiness | ShellProbe): value is ShellProbe {
  return "candidate" in value;
}

function currentReadinessGate(
  candidate: Lifecycle,
  registration: Registration,
  active: ServiceWorker,
): AppShellReadiness | null {
  if (!isCurrent(candidate) || candidate.registration !== registration || registration.active !== active) {
    return notReady("lifecycle-changed");
  }
  const current = readinessGate();
  if (isShellProbe(current)) {
    return current.active === active ? null : notReady("lifecycle-changed");
  }
  return current;
}

function formatError(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "Unknown error";
}

function isCurrent(candidate: Lifecycle): boolean {
  return lifecycle === candidate && lifecycle.token === candidate.token;
}

function stopProgressSimulation(): void {
  if (progressIntervalId !== null) {
    window.clearInterval(progressIntervalId);
    progressIntervalId = null;
  }
}

function clearActivationTimeout(): void {
  if (activationTimeoutId !== null) {
    window.clearTimeout(activationTimeoutId);
    activationTimeoutId = null;
  }
}

function setLifecycleStatus(candidate: Lifecycle, status: Exclude<LifecycleStatus, null>): void {
  if (!isCurrent(candidate)) return;
  if (candidate.ownsLifecycleStatus && candidate.lifecycleStatus !== null && getUpdateStatus() !== candidate.lifecycleStatus) {
    // An installation (possibly created by the previous HMR module) has
    // taken over the shared badge. Its terminal handler owns settlement.
    candidate.ownsLifecycleStatus = false;
    candidate.lifecycleStatus = null;
  }
  if (!candidate.ownsLifecycleStatus) {
    if (getUpdateStatus() !== "idle") return;
    candidate.ownsLifecycleStatus = claimUpdateStatus("serviceWorker");
  }
  if (candidate.ownsLifecycleStatus) {
    candidate.lifecycleStatus = status;
    setUpdateStatus(status);
  }
}

function settleLifecycleStatus(candidate: Lifecycle): void {
  if (!candidate.ownsLifecycleStatus) return;
  // Workbox may synchronously start an installation from registration.update().
  // The badge owner is still the same updater, but the installation now owns
  // its terminal settlement; releasing here would strand that later handler.
  if (activeInstallation?.ownsStatus || getUpdateStatus() === "downloading" || getUpdateStatus() === "activating") {
    candidate.ownsLifecycleStatus = false;
    candidate.lifecycleStatus = null;
    return;
  }
  if (candidate.lifecycleStatus !== null && getUpdateStatus() === candidate.lifecycleStatus) {
    setUpdateStatus("idle");
    setDownloadProgress(0);
  }
  releaseUpdateStatus("serviceWorker");
  candidate.ownsLifecycleStatus = false;
  candidate.lifecycleStatus = null;
}

function startProgressSimulation(installation: Installation): void {
  if (!installation.ownsStatus) return;
  stopProgressSimulation();
  simulatedProgress = 0;
  setDownloadProgress(0);
  progressIntervalId = window.setInterval(() => {
    simulatedProgress += (PROGRESS_CEILING - simulatedProgress) * PROGRESS_RATE;
    setDownloadProgress(simulatedProgress);
  }, PROGRESS_TICK_MS);
}

function finishInstallation(installation: Installation, error?: string): void {
  if (activeInstallation !== installation) return;
  if (installation.ownsStatus) {
    stopProgressSimulation();
    clearActivationTimeout();
    if (error) setUpdateError(error);
    setUpdateStatus("idle");
    setDownloadProgress(0);
    releaseUpdateStatus("serviceWorker");
  }
  activeInstallation = null;
}

function installWorkerProgress(candidate: Lifecycle, registration: Registration): void {
  registration.addEventListener("updatefound", () => {
    if (!isCurrent(candidate) || candidate.registration !== registration || !navigator.serviceWorker.controller) return;
    const worker = registration.installing;
    if (!worker || activeInstallation) return;

    const installation: Installation = {
      token: ++installationToken,
      ownsStatus: claimUpdateStatus("serviceWorker"),
    };
    activeInstallation = installation;
    if (installation.ownsStatus) {
      setUpdateStatus("downloading");
      pushUpdateDebug("Service worker download started.");
      startProgressSimulation(installation);
    }

    worker.addEventListener("statechange", () => {
      if (activeInstallation !== installation) return;
      pushUpdateDebug(`Service worker state changed: ${worker.state}`);
      if (worker.state === "installed") {
        if (!installation.ownsStatus) return;
        stopProgressSimulation();
        setDownloadProgress(100);
        setUpdateStatus("activating");
        clearActivationTimeout();
        activationTimeoutId = window.setTimeout(() => {
          finishInstallation(installation, "Service worker activation timed out after 20s.");
        }, ACTIVATION_TIMEOUT_MS);
      } else if (worker.state === "activated") {
        if (installation.ownsStatus) clearUpdateError();
        finishInstallation(installation);
      } else if (worker.state === "redundant") {
        finishInstallation(installation, "Service worker became redundant before activation.");
      }
    });
  });
}

function clearScheduler(candidate: Lifecycle): void {
  if (candidate.intervalId !== null) {
    window.clearInterval(candidate.intervalId);
    candidate.intervalId = null;
  }
  if (candidate.visibilityListener) {
    document.removeEventListener("visibilitychange", candidate.visibilityListener);
    candidate.visibilityListener = null;
  }
}

function pauseLifecycle(candidate: Lifecycle): void {
  candidate.policyGeneration += 1;
  clearScheduler(candidate);
  candidate.probeAbort?.abort();
  candidate.probeAbort = null;
  candidate.checkQueued = false;
  candidate.queuedProbe = false;
  candidate.deferredReloadCancel?.();
  candidate.deferredReloadCancel = null;
  settleLifecycleStatus(candidate);
}

async function runCheck(candidate: Lifecycle, probeScript: boolean): Promise<void> {
  const registration = candidate.registration;
  if (!registration || !isCurrent(candidate) || getEffectiveOffline()) return;
  if (registration.installing || candidate.deferredReload) return;

  setLifecycleStatus(candidate, "checking");
  const checkToken = candidate.token;
  const policyGeneration = candidate.policyGeneration;
  try {
    if (probeScript) {
      candidate.probeAbort = new AbortController();
      const response = await fetch(candidate.swUrl ?? registration.scope, {
        cache: "no-store",
        headers: { "cache-control": "no-cache" },
        signal: candidate.probeAbort.signal,
      });
      if (!isCurrent(candidate) || checkToken !== candidate.token || policyGeneration !== candidate.policyGeneration || getEffectiveOffline() || candidate.deferredReload) return;
      if (response.status !== 200) {
        setUpdateError(`SW script probe returned HTTP ${response.status}.`);
        return;
      }
    }
    if (!isCurrent(candidate) || checkToken !== candidate.token || policyGeneration !== candidate.policyGeneration || getEffectiveOffline() || candidate.deferredReload) return;
    await registration.update();
    if (!isCurrent(candidate) || checkToken !== candidate.token || policyGeneration !== candidate.policyGeneration || getEffectiveOffline()) return;
    clearUpdateError();
  } catch (error: unknown) {
    if (!isCurrent(candidate) || checkToken !== candidate.token || policyGeneration !== candidate.policyGeneration || getEffectiveOffline()) return;
    if (!(error instanceof DOMException && error.name === "AbortError")) {
      setUpdateError(`Service worker update check failed: ${formatError(error)}`);
      console.warn("[phase.rs] Service worker update check failed.", error);
    }
  } finally {
    if (isCurrent(candidate) && checkToken === candidate.token && policyGeneration === candidate.policyGeneration) candidate.probeAbort = null;
  }
}

function requestCheck(candidate: Lifecycle, reason: "startup" | "resume" | "interval" | "visibility" | "manual"): Promise<void> {
  if (!isCurrent(candidate) || getEffectiveOffline() || !candidate.registration) return Promise.resolve();
  if (candidate.deferredReload) return Promise.resolve();
  if (candidate.checkActive) {
    candidate.checkQueued = true;
    candidate.queuedProbe ||= reason !== "manual";
    return Promise.resolve();
  }

  candidate.checkActive = true;
  return (async () => {
    let probeScript = reason !== "manual";
    do {
      candidate.checkQueued = false;
      candidate.queuedProbe = false;
      await runCheck(candidate, probeScript);
      probeScript = candidate.queuedProbe;
    } while (isCurrent(candidate) && !getEffectiveOffline() && candidate.checkQueued && !candidate.deferredReload);
    if (isCurrent(candidate)) {
      candidate.checkActive = false;
      // A reconnect may have re-submitted a multiplayer-deferred reload while
      // this older check was unwinding. That retained reload, not the check,
      // owns the deferred badge until it runs, is rejected, or is cancelled.
      if (candidate.lifecycleStatus === "checking") settleLifecycleStatus(candidate);
    }
  })();
}

function submitRetainedReload(candidate: Lifecycle): void {
  const reload = candidate.deferredReload;
  if (!reload || !isCurrent(candidate) || getEffectiveOffline() || candidate.deferredReloadCancel) return;
  const scheduled = deferUntilMultiplayerSessionEnds(() => {
    if (!isCurrent(candidate) || getEffectiveOffline() || candidate.deferredReload !== reload) return;
    candidate.deferredReload = null;
    candidate.deferredReloadCancel = null;
    if (!claimServiceWorkerReload()) {
      pushUpdateDebug("Service worker reload was already claimed this session.", "warn");
      settleLifecycleStatus(candidate);
      return;
    }
    markPendingAutoUpdate();
    settleLifecycleStatus(candidate);
    pushUpdateDebug("Service worker update ready; reloading.");
    for (const listener of needReloadListeners) listener();
    window.location.reload();
  }, "reload");
  if (scheduled.deferred) {
    candidate.deferredReloadCancel = scheduled.cancel;
    setLifecycleStatus(candidate, "deferred");
    pushUpdateDebug("Service worker update ready during multiplayer; deferring reload.", "warn");
  }
}

function installScheduler(candidate: Lifecycle): void {
  if (!isCurrent(candidate) || getEffectiveOffline() || !candidate.registration || candidate.intervalId !== null) return;
  candidate.visibilityListener = () => {
    if (document.visibilityState === "visible") void requestCheck(candidate, "visibility");
  };
  document.addEventListener("visibilitychange", candidate.visibilityListener);
  candidate.intervalId = window.setInterval(() => void requestCheck(candidate, "interval"), UPDATE_CHECK_INTERVAL_MS);
  submitRetainedReload(candidate);
  void requestCheck(candidate, "resume");
}

function settleAttempt(candidate: Lifecycle, attempt: RegistrationAttempt, success: boolean): void {
  if (!isCurrent(candidate) || candidate.attempt !== attempt || attempt.terminal) return;
  attempt.terminal = true;
  candidate.attempt = null;
  if (success) {
    candidate.successfulAttempt = attempt;
    candidate.retryRegistration = false;
    return;
  }
  if (candidate.retryRegistration && !getEffectiveOffline()) {
    candidate.retryRegistration = false;
    startRegistration(candidate);
  }
}

function startRegistration(candidate: Lifecycle): void {
  if (!isCurrent(candidate) || getEffectiveOffline() || candidate.registration || candidate.attempt) return;
  const attempt: RegistrationAttempt = { token: ++attemptToken, terminal: false };
  candidate.attempt = attempt;
  pushUpdateDebug("Registering service worker updater.");
  registerSW({
    immediate: true,
    onNeedReload() {
      if (!isCurrent(candidate) || candidate.successfulAttempt !== attempt) return;
      candidate.deferredReload ??= () => {};
      submitRetainedReload(candidate);
    },
    onRegisteredSW(swUrl, registration) {
      if (!isCurrent(candidate) || candidate.attempt !== attempt || attempt.terminal) return;
      if (!registration) {
        settleAttempt(candidate, attempt, false);
        return;
      }
      candidate.registration = registration;
      candidate.swUrl = swUrl;
      settleAttempt(candidate, attempt, true);
      pushUpdateDebug(`Service worker registered: ${swUrl}`);
      installWorkerProgress(candidate, registration);
      if (!getEffectiveOffline()) installScheduler(candidate);
    },
    onRegisterError(error) {
      if (!isCurrent(candidate) || candidate.attempt !== attempt || attempt.terminal) return;
      setUpdateError(`Service worker registration failed: ${formatError(error)}`);
      console.error("Service worker registration failed", error);
      settleAttempt(candidate, attempt, false);
    },
  });
}

function resumeLifecycle(candidate: Lifecycle, wasOffline: boolean): void {
  if (!isCurrent(candidate)) return;
  if (candidate.attempt) {
    if (wasOffline) candidate.retryRegistration = true;
    return;
  }
  if (!candidate.registration) {
    // A deferred reconnect demand is consumed by this successor. If it too
    // fails, another offline→online transition is required before retrying.
    candidate.retryRegistration = false;
    startRegistration(candidate);
    return;
  }
  installScheduler(candidate);
}

/** Manual BuildBadge entry point. */
export function checkForServiceWorkerUpdate(): boolean {
  const candidate = lifecycle;
  if (!candidate || !isCurrent(candidate) || getEffectiveOffline() || !candidate.registration) {
    pushUpdateDebug("Manual update check ignored (offline or updater not ready).", "warn");
    return false;
  }
  void requestCheck(candidate, "manual");
  return true;
}

export type DeployedBuildStatus = "current" | "stale" | "unknown";
export type BuildUpdateOutcome = "reloading" | "manual";

/**
 * Compares the deployed build (`/build.json`, network-only) with this bundle's
 * `__BUILD_HASH__`. Any failure to read it is `"unknown"`, never `"stale"`.
 */
export async function checkDeployedBuild(): Promise<DeployedBuildStatus> {
  try {
    const response = await fetch("/build.json", {
      cache: "no-store",
      signal: AbortSignal.timeout(BUILD_CHECK_TIMEOUT_MS),
    });
    if (!response.ok) return "unknown";
    const payload: unknown = await response.json();
    const build = payload !== null && typeof payload === "object" ? (payload as { build?: unknown }).build : undefined;
    if (typeof build !== "string") return "unknown";
    return build === __BUILD_HASH__ ? "current" : "stale";
  } catch {
    return "unknown";
  }
}

/**
 * Moves this tab onto the deployed build. `"reloading"` means a reload is
 * running now; `"manual"` means none happened within `deadlineMs` and the user
 * must choose. The deadline bounds this wait only: a worker still downloading
 * at the deadline may activate later and reload the tab through the usual
 * `onNeedReload` path (see `isBuildUpdateInFlight`).
 */
export function updateToLatestBuild({ deadlineMs }: { deadlineMs: number }): Promise<BuildUpdateOutcome> {
  // A live game is never reloaded from here, on any branch; with no budget left
  // no automatic reload can happen this session.
  if (isMultiplayerGameLive() || !hasServiceWorkerReloadBudget()) return Promise.resolve("manual");
  if (!("serviceWorker" in navigator) || navigator.serviceWorker.controller === null) {
    // Nothing is cached by a worker, so a plain reload fetches the deployed
    // build. It spends the same per-session budget as the worker path.
    claimServiceWorkerReload();
    markPendingAutoUpdate();
    window.location.reload();
    return Promise.resolve("reloading");
  }
  const candidate = lifecycle;
  if (
    !candidate
    || !isCurrent(candidate)
    || getEffectiveOffline()
    // Registration failed and nothing is pending: no check will run in this document.
    || (candidate.attempt === null && candidate.registration === null)
  ) {
    return Promise.resolve("manual");
  }
  return new Promise((resolve) => {
    const settle = (outcome: BuildUpdateOutcome) => {
      window.clearTimeout(timeoutId);
      needReloadListeners.delete(listener);
      resolve(outcome);
    };
    const listener = () => settle("reloading");
    const timeoutId = window.setTimeout(() => settle("manual"), deadlineMs);
    needReloadListeners.add(listener);
    // Before registration lands, its `onRegisteredSW` → `installScheduler`
    // runs the "resume" check itself.
    if (candidate.registration) void requestCheck(candidate, "manual");
  });
}

/**
 * True when this lifecycle may still reload the tab without further user
 * action. Read live, when the user chooses to continue on this build.
 *
 * An installing or waiting worker may still activate: offline does not stop
 * that, and the reload it triggers is retained and submitted on reconnect. It
 * may also die (turn redundant), and then nothing reloads; the Discord-link
 * stash a caller keeps for that reload is bounded by its 10-minute expiry.
 */
export function isBuildUpdateInFlight(): boolean {
  const candidate = lifecycle;
  if (!candidate || !isCurrent(candidate) || !hasServiceWorkerReloadBudget()) return false;
  const registration = candidate.registration;
  return candidate.attempt !== null // registration pending: its onRegisteredSW runs a "resume" check
    || candidate.checkActive // registration.update() is running and may find a worker
    || candidate.deferredReload !== null // a reload is retained and not yet run
    || Boolean(registration?.installing || registration?.waiting); // a worker is downloading or activating
}

/**
 * The user-driven Refresh: reloads unless a multiplayer game is live, in which
 * case it returns `false` and reloads nothing. It does not claim the service
 * worker reload budget.
 */
export function reloadIfNoLiveGame(): boolean {
  if (isMultiplayerGameLive()) return false;
  window.location.reload();
  return true;
}

/**
 * Verifies that the active worker can serve this build's app shell without a
 * network request. Step 7 uses the typed result to describe offline readiness.
 */
export async function checkAppShellReadiness(): Promise<AppShellReadiness> {
  const initial = readinessGate();
  if (!isShellProbe(initial)) return initial;

  const { candidate, registration, active } = initial;
  const stale = () => currentReadinessGate(candidate, registration, active);
  const markerUrl = `/offline-shell-${__BUILD_HASH__}.json?phase-precache-probe=${encodeURIComponent(markerProbeNonce())}`;

  try {
    const response = await fetch(markerUrl, {
      mode: "same-origin",
      cache: "only-if-cached",
    });
    const afterProbe = stale();
    if (afterProbe) return afterProbe;
    if (!response.ok) return notReady("shell-cache-unavailable");

    const payload: unknown = await response.json();
    const afterPayload = stale();
    if (afterPayload) return afterPayload;
    if (
      payload === null ||
      typeof payload !== "object" ||
      !("build" in payload) ||
      (payload as { build?: unknown }).build !== __BUILD_HASH__
    ) {
      return notReady("shell-cache-unavailable");
    }

    const markerWritten = await markRemoteLoadOk();
    const afterMarker = stale();
    if (afterMarker) return afterMarker;
    return markerWritten ? { status: "ready" } : notReady("remote-load-marker-unavailable");
  } catch {
    return stale() ?? notReady("shell-cache-unavailable");
  }
}

/** Registers the PWA updater only once an effective-online lifecycle exists. */
export function registerServiceWorker(): void {
  if (import.meta.env.DEV || isBundledTauriOrigin() || !("serviceWorker" in navigator) || lifecycle) return;
  const candidate: Lifecycle = {
    token: ++lifecycleToken,
    unsubscribe: null,
    registration: null,
    swUrl: null,
    attempt: null,
    successfulAttempt: null,
    retryRegistration: false,
    policyGeneration: 0,
    intervalId: null,
    visibilityListener: null,
    probeAbort: null,
    checkActive: false,
    checkQueued: false,
    queuedProbe: false,
    deferredReload: null,
    deferredReloadCancel: null,
    lifecycleStatus: null,
    ownsLifecycleStatus: false,
    beforeUnload: null,
  };
  lifecycle = candidate;
  candidate.unsubscribe = subscribeEffectiveOffline((offline, previousOffline) => {
    if (!isCurrent(candidate)) return;
    if (offline) pauseLifecycle(candidate);
    else resumeLifecycle(candidate, previousOffline);
  });
  candidate.beforeUnload = () => disposeCurrentServiceWorkerLifecycle(candidate);
  window.addEventListener("beforeunload", candidate.beforeUnload, { once: true });
  if (!getEffectiveOffline()) resumeLifecycle(candidate, true);
}

/** Test seam and HMR lifecycle owner. It intentionally retains controller and caches. */
export function disposeServiceWorkerUpdater(): void {
  const candidate = lifecycle;
  if (!candidate) return;
  disposeCurrentServiceWorkerLifecycle(candidate);
}

function disposeCurrentServiceWorkerLifecycle(candidate: Lifecycle): void {
  if (!isCurrent(candidate)) return;
  candidate.policyGeneration += 1;
  lifecycle = null;
  candidate.unsubscribe?.();
  candidate.unsubscribe = null;
  clearScheduler(candidate);
  candidate.probeAbort?.abort();
  candidate.probeAbort = null;
  candidate.deferredReloadCancel?.();
  candidate.deferredReloadCancel = null;
  candidate.deferredReload = null;
  if (candidate.beforeUnload) window.removeEventListener("beforeunload", candidate.beforeUnload);
  settleLifecycleStatus(candidate);
}

if (import.meta.hot) import.meta.hot.dispose(disposeServiceWorkerUpdater);
