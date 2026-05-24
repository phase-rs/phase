import { create } from "zustand";

import { getSharedAdapter } from "../adapter/wasm-adapter";

/**
 * App-level lifecycle status for the shared card-database warm.
 *
 * This is the UX authority that menu gating and the setup start-gate subscribe
 * to. The actual load work lives solely in the adapter
 * (`WasmAdapter.warmCardDatabase`); this store wraps the adapter's binary
 * `cardDbLoaded` latch with a multi-state status so the UI can distinguish
 * "loading" (gate the button) from "error" (un-gate so the user is never
 * trapped — downstream game/compat init retries the load best-effort).
 */
export type CardDataStatus = "idle" | "loading" | "ready" | "error";

interface CardDataStoreState {
  status: CardDataStatus;
  /**
   * Begin (or reuse) the single shared engine worker's card-database load.
   * Idempotent: a settled `ready` short-circuits and concurrent/repeat callers
   * share one in-flight load. Triggered on the main menu and on every
   * deck-requiring page mount so direct deep-links warm too.
   */
  warm: () => Promise<void>;
}

// Dedupes overlapping warm() calls across components into a single load.
let warmInFlight: Promise<void> | null = null;

export const useCardDataStore = create<CardDataStoreState>((set, get) => ({
  status: "idle",
  warm: () => {
    if (get().status === "ready") return Promise.resolve();
    if (warmInFlight) return warmInFlight;

    set({ status: "loading" });
    warmInFlight = getSharedAdapter()
      .warmCardDatabase()
      .then(() => set({ status: "ready" }))
      // The underlying cause is already logged by the adapter; surface only the
      // lifecycle state. A later warm() (e.g. another page mount) can retry.
      .catch(() => set({ status: "error" }))
      .finally(() => {
        warmInFlight = null;
      });
    return warmInFlight;
  },
}));
