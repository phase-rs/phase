import { create } from "zustand";

import type { SpectatorDraftView } from "../adapter/draft-adapter";
import {
  connectDraftSpectator,
  type DraftSpectatorSession,
} from "../services/draftSpectatorSession";
import { detectServerUrl } from "../services/serverDetection";

interface DraftSpectatorState {
  draftCode: string | null;
  view: SpectatorDraftView | null;
  status: "idle" | "connecting" | "connected" | "error";
  error: string | null;
  session: DraftSpectatorSession | null;

  watchDraft: (draftCode: string) => Promise<void>;
  leave: () => void;
}

export const useDraftSpectatorStore = create<DraftSpectatorState>((set, get) => ({
  draftCode: null,
  view: null,
  status: "idle",
  error: null,
  session: null,

  watchDraft: async (draftCode) => {
    get().leave();
    set({ draftCode, status: "connecting", error: null, view: null });
    try {
      const serverUrl = import.meta.env.VITE_WS_URL ?? (await detectServerUrl());
      const session = await connectDraftSpectator(serverUrl, draftCode);
      const unsub = session.onEvent((event) => {
        if (event.type === "view") {
          set({ view: event.view, status: "connected" });
        } else if (event.type === "error") {
          set({ status: "error", error: event.message });
        } else if (event.type === "disconnected") {
          set({ status: "error", error: "Disconnected" });
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
      set({
        status: "error",
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  leave: () => {
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
