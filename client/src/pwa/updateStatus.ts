import { useSyncExternalStore } from "react";

export type UpdateStatus = "idle" | "checking" | "downloading" | "activating" | "deferred";
type DebugLevel = "info" | "warn" | "error";

interface UpdateDebugEvent {
  at: number;
  level: DebugLevel;
  message: string;
}

let status: UpdateStatus = "idle";
const listeners = new Set<() => void>();

let updateError: string | null = null;
let debugEvents: UpdateDebugEvent[] = [];
const debugListeners = new Set<() => void>();
const MAX_DEBUG_EVENTS = 40;

export function setUpdateStatus(next: UpdateStatus) {
  if (status === next) return;
  status = next;
  for (const fn of listeners) fn();
}

export function getUpdateStatus(): UpdateStatus {
  return status;
}

function subscribe(callback: () => void): () => void {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

function notifyDebugListeners(): void {
  for (const fn of debugListeners) fn();
}

function subscribeDebug(callback: () => void): () => void {
  debugListeners.add(callback);
  return () => debugListeners.delete(callback);
}

export function pushUpdateDebug(message: string, level: DebugLevel = "info"): void {
  debugEvents = [...debugEvents, { at: Date.now(), level, message }].slice(-MAX_DEBUG_EVENTS);
  notifyDebugListeners();
}

export function setUpdateError(message: string): void {
  updateError = message;
  pushUpdateDebug(message, "error");
}

export function clearUpdateError(): void {
  if (!updateError) return;
  updateError = null;
  notifyDebugListeners();
}

export function getUpdateError(): string | null {
  return updateError;
}

export function useUpdateError(): string | null {
  return useSyncExternalStore(subscribeDebug, getUpdateError);
}

export function getUpdateDebugReport(): string {
  const lines = [
    `time=${new Date().toISOString()}`,
    `version=${__APP_VERSION__}`,
    `build=${__BUILD_HASH__}`,
    `status=${status}`,
    `downloadProgress=${downloadProgress}%`,
    `error=${updateError ?? "none"}`,
    `online=${typeof navigator !== "undefined" && "onLine" in navigator ? String(navigator.onLine) : "unknown"}`,
    `serviceWorkerSupported=${typeof navigator !== "undefined" ? String("serviceWorker" in navigator) : "false"}`,
  ];

  if (typeof navigator !== "undefined" && "serviceWorker" in navigator) {
    const controllerUrl = navigator.serviceWorker.controller?.scriptURL ?? "none";
    lines.push(`controller=${controllerUrl}`);
  }

  lines.push("events:");
  if (debugEvents.length === 0) {
    lines.push("  (none)");
  } else {
    for (const event of debugEvents) {
      const timestamp = new Date(event.at).toISOString();
      lines.push(`  [${timestamp}] ${event.level.toUpperCase()} ${event.message}`);
    }
  }

  return lines.join("\n");
}

/** React hook — subscribes to the module-level update status. */
export function useUpdateStatus(): UpdateStatus {
  return useSyncExternalStore(subscribe, getUpdateStatus);
}

// --- Download progress (0–100) ---

let downloadProgress = 0;
const progressListeners = new Set<() => void>();

export function setDownloadProgress(value: number) {
  const clamped = Math.max(0, Math.min(100, Math.round(value)));
  if (downloadProgress === clamped) return;
  downloadProgress = clamped;
  for (const fn of progressListeners) fn();
}

export function getDownloadProgress(): number {
  return downloadProgress;
}

function subscribeProgress(callback: () => void): () => void {
  progressListeners.add(callback);
  return () => progressListeners.delete(callback);
}

/** React hook — subscribes to the simulated download progress (0–100). */
export function useDownloadProgress(): number {
  return useSyncExternalStore(subscribeProgress, getDownloadProgress);
}
