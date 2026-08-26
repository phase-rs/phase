/** Check whether we are running inside a Tauri webview. */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export type HostPlatform = "desktop" | "android" | "ios";

let platform: HostPlatform | null = null;
let platformSettled = false;
let platformPromise: Promise<HostPlatform | null> | null = null;

function isHostPlatform(value: unknown): value is HostPlatform {
  return value === "desktop" || value === "android" || value === "ios";
}

function hasProvenDesktopUserAgent(): boolean {
  if (typeof navigator === "undefined") return false;
  const userAgent = navigator.userAgent;
  if (/Android|Mobile|iPhone|iPad|iPod/i.test(userAgent)) return false;
  if (/Macintosh/i.test(userAgent) && navigator.maxTouchPoints > 1) return false;
  return /Windows NT|Macintosh; Intel Mac OS X|X11; (?:Linux|Ubuntu)|Linux x86_64/i.test(
    userAgent,
  );
}

/**
 * Resolve the shell platform exactly once, before any platform-sensitive code
 * is allowed to run. Unknown successful results fail closed. When the probe is
 * rejected by a pre-command/pre-permission desktop shell, the legacy fallback
 * relies only on unambiguous desktop user-agent evidence; Tauri's rejection
 * text is not a stable API and differs depending on whether ACL or dispatch
 * rejects the command first.
 */
export function initializeHostPlatform(): Promise<HostPlatform | null> {
  if (platformPromise) return platformPromise;

  platformPromise = (async () => {
    if (!isTauri()) return null;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<unknown>("host_platform");
      return isHostPlatform(result) ? result : null;
    } catch {
      return hasProvenDesktopUserAgent() ? "desktop" : null;
    }
  })().then((result) => {
    platform = result;
    platformSettled = true;
    return result;
  });

  return platformPromise;
}

export function isDesktopTauri(): boolean {
  return platformSettled && platform === "desktop" && isTauri();
}

export function isAndroidTauri(): boolean {
  return platformSettled && platform === "android" && isTauri();
}

export function isIosTauri(): boolean {
  return platformSettled && platform === "ios" && isTauri();
}

/** Whether the app is served from Tauri's bundled custom origin. */
export function isBundledTauriOrigin(): boolean {
  return (
    typeof window !== "undefined" &&
    (window.location.protocol === "tauri:" || window.location.hostname === "tauri.localhost")
  );
}
