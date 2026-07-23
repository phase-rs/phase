import { isTauri } from "./platform";

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
  return enabled && isTauri() && nativeEngineKeyForCurrentOrigin() !== null;
}

/** Feature-detects the shell command at invocation time for plain-web fallback. */
export async function ensureNativeEngine(key: NativeEngineKey): Promise<NativeEngineReady> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<NativeEngineReady>("ensure_native_engine", { key });
}

/** Subscribes to the shell's native-engine provisioning progress. */
export async function subscribeNativeEngineProgress(
  listener: (progress: NativeEngineProgress) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};

  const { listen } = await import("@tauri-apps/api/event");
  return listen<NativeEngineProgress>("native-engine-progress", ({ payload }) => listener(payload));
}
