import { useSyncExternalStore } from "react";

import { isDesktopTauri } from "./platform";

export type NativeEngineKey =
  | { release: { version: string } }
  | { preview: { fingerprint: string } };

export interface NativeEngineReady {
  port: number;
}

export type NativeEngineProgressPhase =
  | "resolving"
  | "downloading_binary"
  | "verifying"
  | "downloading_data"
  | "spawning"
  | "ready"
  | "failed";

export interface NativeEngineProgress {
  phase: NativeEngineProgressPhase;
  detail?: string;
}

/**
 * Returns the shell-verifiable artifact key for this first-party origin.
 * Preview builds without a stamped fingerprint intentionally return `null` so
 * local WASM remains the only engine path until preview artifact plumbing lands.
 */
export function nativeEngineKeyForCurrentOrigin(): NativeEngineKey | null {
  if (typeof window === "undefined") return null;

  if (window.location.origin === new URL(__RELEASE_SITE_URL__).origin) {
    return { release: { version: __APP_VERSION__ } };
  }
  if (
    window.location.origin === new URL(__PREVIEW_SITE_URL__).origin
    && __ENGINE_FINGERPRINT__ !== undefined
  ) {
    return { preview: { fingerprint: __ENGINE_FINGERPRINT__ } };
  }
  return null;
}

/** Native routing is only available from a supported desktop origin. */
export function canAttemptNativeEngine(enabled: boolean): boolean {
  return enabled && isDesktopTauri() && nativeEngineKeyForCurrentOrigin() !== null;
}

/**
 * Provisioning is in flight for exactly as long as an `ensureNativeEngine` call
 * is unsettled. This — not the shell's progress events — is the authority on
 * whether the shell is still working.
 *
 * The shell is a separately-versioned binary that updates far less often than
 * this remote content, so a shipped shell may emit phases this build has never
 * heard of, or (as `shell-v1.0.1` does on every failure) stop emitting without
 * a terminal phase at all. Progress events are advisory decoration on top of
 * the call's own lifetime; anything that must eventually stop belongs here.
 */
let provisioningCalls = 0;
const provisioningListeners = new Set<() => void>();

function setProvisioningCalls(next: number): void {
  provisioningCalls = next;
  for (const notify of provisioningListeners) notify();
}

function subscribeNativeEngineProvisioning(callback: () => void): () => void {
  provisioningListeners.add(callback);
  return () => provisioningListeners.delete(callback);
}

function getNativeEngineProvisioning(): boolean {
  return provisioningCalls > 0;
}

/** React hook — true while any `ensureNativeEngine` call is still unsettled. */
export function useNativeEngineProvisioning(): boolean {
  return useSyncExternalStore(subscribeNativeEngineProvisioning, getNativeEngineProvisioning);
}

/** Feature-detects the shell command at invocation time for plain-web fallback. */
export async function ensureNativeEngine(key: NativeEngineKey): Promise<NativeEngineReady> {
  if (!isDesktopTauri()) {
    throw new Error("Native engine provisioning is available only in the desktop shell.");
  }
  setProvisioningCalls(provisioningCalls + 1);
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<NativeEngineReady>("ensure_native_engine", { key });
  } finally {
    setProvisioningCalls(provisioningCalls - 1);
  }
}

/** Returns progress emitted before this webview registered its listener. */
export async function getNativeEngineProgress(): Promise<NativeEngineProgress | null> {
  if (!isDesktopTauri()) return null;

  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<NativeEngineProgress | null>("native_engine_progress");
}

/** Subscribes to the shell's native-engine provisioning progress. */
export async function subscribeNativeEngineProgress(
  listener: (progress: NativeEngineProgress) => void,
): Promise<() => void> {
  if (!isDesktopTauri()) return () => {};

  const { listen } = await import("@tauri-apps/api/event");
  return listen<NativeEngineProgress>("native-engine-progress", ({ payload }) => listener(payload));
}
