import { create } from "zustand";
import { persist, subscribeWithSelector } from "zustand/middleware";

const CONNECTIVITY_STORAGE_KEY = "phase-connectivity-v1";

interface ConnectivityState {
  /** User preference persisted across launches. */
  forcedOffline: boolean;
  /** Current browser-reported connectivity; never persisted. */
  browserOnline: boolean;
  setForcedOffline: (forcedOffline: boolean) => void;
}

function browserIsOnline(): boolean {
  return typeof navigator === "undefined" || typeof navigator.onLine !== "boolean"
    ? true
    : navigator.onLine;
}

export function selectEffectiveOffline(state: Pick<ConnectivityState, "forcedOffline" | "browserOnline">): boolean {
  return state.forcedOffline || !state.browserOnline;
}

export const useConnectivityStore = create<ConnectivityState>()(
  subscribeWithSelector(
    persist(
      (set) => ({
        forcedOffline: false,
        browserOnline: browserIsOnline(),
        setForcedOffline: (forcedOffline) =>
          set((state) => (state.forcedOffline === forcedOffline ? state : { forcedOffline })),
      }),
      {
        name: CONNECTIVITY_STORAGE_KEY,
        version: 1,
        skipHydration: true,
        partialize: (state) => ({ forcedOffline: state.forcedOffline }),
      },
    ),
  ),
);

/** React hook for the sole effective-offline policy. */
export function useEffectiveOffline(): boolean {
  return useConnectivityStore(selectEffectiveOffline);
}

/** Imperative access for lifecycle owners outside React. */
export function getEffectiveOffline(): boolean {
  return selectEffectiveOffline(useConnectivityStore.getState());
}

/**
 * Subscribe to policy transitions, not its individual inputs. In particular,
 * browser events do not notify consumers while forced offline masks them.
 */
export function subscribeEffectiveOffline(
  listener: (effectiveOffline: boolean, previousEffectiveOffline: boolean) => void,
): () => void {
  return useConnectivityStore.subscribe(selectEffectiveOffline, listener, {
    equalityFn: Object.is,
  });
}

let initialized = false;
let initialization: Promise<void> | null = null;
let removeBrowserListeners: (() => void) | null = null;
let lifecycleVersion = 0;

function refreshBrowserOnline(): void {
  const browserOnline = browserIsOnline();
  if (useConnectivityStore.getState().browserOnline !== browserOnline) {
    useConnectivityStore.setState({ browserOnline });
  }
}

function installBrowserListeners(): void {
  if (removeBrowserListeners || typeof window === "undefined") return;

  const refresh = () => refreshBrowserOnline();
  window.addEventListener("online", refresh);
  window.addEventListener("offline", refresh);
  removeBrowserListeners = () => {
    window.removeEventListener("online", refresh);
    window.removeEventListener("offline", refresh);
    removeBrowserListeners = null;
  };
}

/**
 * Rehydrate the persisted preference only after legacy migration, then own one
 * browser listener pair. A disposed in-flight initialization is ignored so an
 * HMR replacement cannot install stale listeners after its successor starts.
 */
export function initializeConnectivity(): Promise<void> {
  if (initialized) return Promise.resolve();
  if (initialization) return initialization;

  const version = lifecycleVersion;
  const pending = (async () => {
    await useConnectivityStore.persist.rehydrate();
    if (version !== lifecycleVersion) return;

    refreshBrowserOnline();
    installBrowserListeners();
    initialized = true;
  })();
  initialization = pending;
  void pending.finally(() => {
    if (initialization === pending) initialization = null;
  });
  return pending;
}

/** Test seam and HMR cleanup owner. */
export function disposeConnectivity(): void {
  lifecycleVersion += 1;
  removeBrowserListeners?.();
  initialized = false;
  initialization = null;
}

if (import.meta.hot) {
  import.meta.hot.dispose(disposeConnectivity);
}
