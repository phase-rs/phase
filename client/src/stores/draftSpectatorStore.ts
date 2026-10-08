import { create } from "zustand";

import type { SpectatorDraftView } from "../adapter/draft-adapter";
import {
  connectDraftSpectator,
  type DraftSpectatorSession,
} from "../services/draftSpectatorSession";
import { detectServerUrl } from "../services/serverDetection";
import { getEffectiveOffline } from "./connectivityStore";

interface DraftSpectatorState {
  draftCode: string | null;
  view: SpectatorDraftView | null;
  status: "idle" | "connecting" | "connected" | "error";
  error: string | null;
  session: DraftSpectatorSession | null;

  /**
   * Watch a draft. `serverUrl` is the origin the `/draft-spectator` route
   * carried — the authority that listed the draft. Absent → this client's
   * hosting server via `detectServerUrl()`.
   */
  watchDraft: (draftCode: string, serverUrl?: string) => Promise<void>;
  leave: () => void;
}

let draftSpectatorRequestId = 0;

/** Spectator codes are server-issued identities, not five-character P2P room codes. */
export function normalizeSpectatorDraftCode(draftCode: string): string {
  return draftCode.trim().toUpperCase();
}

export const useDraftSpectatorStore = create<DraftSpectatorState>((set, get) => ({
  draftCode: null,
  view: null,
  status: "idle",
  error: null,
  session: null,

  watchDraft: async (draftCode, serverUrl) => {
    const requestedCode = normalizeSpectatorDraftCode(draftCode);
    if (!requestedCode || getEffectiveOffline()) return;

    get().leave();
    const requestId = ++draftSpectatorRequestId;
    set({ draftCode: requestedCode, status: "connecting", error: null, view: null });
    try {
      const url = import.meta.env.VITE_WS_URL ?? serverUrl ?? (await detectServerUrl());
      if (
        requestId !== draftSpectatorRequestId
        || get().draftCode !== requestedCode
        || getEffectiveOffline()
      ) return;
      const session = await connectDraftSpectator(url, requestedCode);
      if (requestId !== draftSpectatorRequestId || get().draftCode !== requestedCode) {
        session.close();
        return;
      }
      const unsub = session.onEvent((event) => {
        if (requestId !== draftSpectatorRequestId || get().draftCode !== requestedCode) return;
        if (event.type === "view") {
          set({ view: event.view, status: "connected" });
        } else if (event.type === "error") {
          set({ status: "error", error: event.message });
        } else if (event.type === "disconnected") {
          set({ status: "error", error: null });
        }
      });
      set({
        session: {
          close: () => {
            unsub();
            session.close();
          },
          onEvent: session.onEvent,
        },
      });
    } catch (err) {
      if (requestId !== draftSpectatorRequestId || get().draftCode !== requestedCode) return;
      set({
        status: "error",
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  leave: () => {
    draftSpectatorRequestId += 1;
    const { session } = get();
    session?.close();
    set({
      draftCode: null,
      view: null,
      status: "idle",
      error: null,
      session: null,
    });
  },
}));
