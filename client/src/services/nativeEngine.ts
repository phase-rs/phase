import { useSyncExternalStore } from "react";

import { isDesktopTauri } from "./platform";
import { getEffectiveOffline } from "../stores/connectivityStore";

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
let tauriInvoke: Promise<typeof import("@tauri-apps/api/core").invoke> | null = null;

type NativeEngineIntent = "start_offline" | "prepare_for_offline";

interface NativeEngineCapabilities {
  intent_contract: 1;
}

// A desktop shell and its remote web content release independently. Cache the
// read-only contract probe so a shell that predates intent support fails before
// it receives an unfamiliar `ensure_native_engine` argument.
let intentCapabilities: Promise<void> | null = null;

function invokeTauri<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  tauriInvoke ??= import("@tauri-apps/api/core").then(({ invoke }) => invoke);
  return tauriInvoke.then((invoke) =>
    args === undefined ? invoke<T>(command) : invoke<T>(command, args),
  );
}

function requireIntentCapabilities(): Promise<void> {
  if (intentCapabilities) return intentCapabilities;

  intentCapabilities = (async () => {
    const capability = await invokeTauri<NativeEngineCapabilities>("native_engine_capabilities");
    if (capability?.intent_contract !== 1) {
      throw new Error("This desktop shell does not support native-engine offline preparation.");
    }
  })();
  return intentCapabilities;
}

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
async function invokeNativeEngine(
  key: NativeEngineKey,
  intent?: NativeEngineIntent,
): Promise<NativeEngineReady> {
  if (!isDesktopTauri()) {
    throw new Error("Native engine provisioning is available only in the desktop shell.");
  }
  setProvisioningCalls(provisioningCalls + 1);
  try {
    if (intent) await requireIntentCapabilities();
    return await invokeTauri<NativeEngineReady>(
      "ensure_native_engine",
      intent ? { key, intent } : { key },
    );
  } finally {
    setProvisioningCalls(provisioningCalls - 1);
  }
}

/**
 * Ensures the local engine for the current connectivity policy. Online startup
 * intentionally preserves the legacy omitted-intent payload for older shells.
 */
export function ensureNativeEngine(key: NativeEngineKey): Promise<NativeEngineReady> {
  return invokeNativeEngine(key, getEffectiveOffline() ? "start_offline" : undefined);
}

/** Explicit desktop preparation used by the offline settings flow. */
export function prepareNativeEngineForOffline(key: NativeEngineKey): Promise<NativeEngineReady> {
  return invokeNativeEngine(key, "prepare_for_offline");
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
