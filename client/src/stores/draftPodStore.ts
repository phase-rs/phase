/**
 * Draft Pod Store — UI state for P2P draft pod lobby management.
 *
 * This store manages pod-specific UI state that augments the
 * `multiplayerDraftStore` (which handles the adapter lifecycle,
 * draft picks, and deckbuilding). The pod store tracks:
 *
 * - Pod configuration (set, draft type, pod size)
 * - Bot-fill state (which empty seats to fill with bots on start)
 * - Lobby readiness and host controls
 *
 * The `multiplayerDraftStore` remains the source of truth for
 * adapter state, seat views, and draft phase. This store provides
 * the orchestration layer for the lobby UI.
 */

import { create } from "zustand";

import type { TournamentFormat, PodPolicy } from "../adapter/draft-adapter";
import type { DraftPodHostConfig } from "../adapter/draftPodHostAdapter";
import type { DraftPodGuestConfig } from "../adapter/draftPodGuestAdapter";
import { useMultiplayerDraftStore } from "./multiplayerDraftStore";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftKind = "Premier" | "Traditional";

export interface PodConfig {
  setCode: string;
  setName: string;
  kind: DraftKind;
  podSize: number;
  tournamentFormat: TournamentFormat;
  podPolicy: PodPolicy;
}

interface DraftPodState {
  /** Pod configuration selected by host before creating the pod. */
  config: PodConfig;
  /** Whether bot-fill is enabled (fill remaining seats with bots on start). */
  botFillEnabled: boolean;
  /** Host display name for the local player. */
  hostDisplayName: string;
  /** Join code entered by guest. */
  joinCode: string;
  /** Guest display name. */
  guestDisplayName: string;
  /** Set pool JSON loaded from draft-pools.json. */
  setPoolJson: string | null;
  /** Loading state while fetching set pool data. */
  loadingPool: boolean;
  /** Error from pool loading or pod creation. */
  configError: string | null;
}

interface DraftPodActions {
  /** Update pod configuration fields. */
  setConfig: (partial: Partial<PodConfig>) => void;
  /** Toggle bot-fill on/off. */
  toggleBotFill: () => void;
  /** Set host display name. */
  setHostDisplayName: (name: string) => void;
  /** Set guest display name. */
  setGuestDisplayName: (name: string) => void;
  /** Set join code for guest. */
  setJoinCode: (code: string) => void;
  /** Load the set pool data and create a new pod as host. */
  createPod: () => Promise<void>;
  /** Join an existing pod as guest. */
  joinPod: () => Promise<void>;
  /** Host: start the draft (delegates to multiplayerDraftStore). */
  startDraft: () => Promise<void>;
  /** Reset pod store state. */
  reset: () => void;
}

// ── Initial state ──────────────────────────────────────────────────────

const initialState: DraftPodState = {
  config: {
    setCode: "",
    setName: "",
    kind: "Premier",
    podSize: 8,
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
  },
  botFillEnabled: true,
  hostDisplayName: "",
  guestDisplayName: "",
  joinCode: "",
  setPoolJson: null,
  loadingPool: false,
  configError: null,
};

// ── Store ──────────────────────────────────────────────────────────────

export const useDraftPodStore = create<DraftPodState & DraftPodActions>()(
  (set, get) => ({
    ...initialState,

    setConfig: (partial) => {
      set((prev) => ({
        config: { ...prev.config, ...partial },
        configError: null,
      }));
    },

    toggleBotFill: () => {
      set((prev) => ({ botFillEnabled: !prev.botFillEnabled }));
    },

    setHostDisplayName: (name) => {
      set({ hostDisplayName: name });
    },

    setGuestDisplayName: (name) => {
      set({ guestDisplayName: name });
    },

    setJoinCode: (code) => {
      set({ joinCode: code });
    },

    createPod: async () => {
      const { config, hostDisplayName } = get();

      if (!config.setCode) {
        set({ configError: "Select a set first" });
        return;
      }
      if (!hostDisplayName.trim()) {
        set({ configError: "Enter a display name" });
        return;
      }

      set({ loadingPool: true, configError: null });

      try {
        // Load set pool data
        const resp = await fetch(__DRAFT_POOLS_URL__);
        if (!resp.ok) {
          throw new Error(`Failed to load draft pools: ${resp.status}`);
        }
        const allPools: Record<string, unknown> = await resp.json();
        const setPool =
          allPools[config.setCode.toLowerCase()] ??
          allPools[config.setCode.toUpperCase()];
        if (!setPool) {
          throw new Error(`No pool data for set: ${config.setCode}`);
        }

        const poolJson = JSON.stringify(setPool);
        set({ setPoolJson: poolJson, loadingPool: false });

        // Create the pod via multiplayerDraftStore
        const hostConfig: DraftPodHostConfig = {
          setPoolJson: poolJson,
          kind: config.kind,
          podSize: config.podSize,
          hostDisplayName: hostDisplayName.trim(),
          tournamentFormat: config.tournamentFormat,
          podPolicy: config.podPolicy,
        };

        await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        set({ configError: message, loadingPool: false });
      }
    },

    joinPod: async () => {
      const { joinCode, guestDisplayName } = get();

      if (!joinCode.trim()) {
        set({ configError: "Enter a room code" });
        return;
      }
      if (!guestDisplayName.trim()) {
        set({ configError: "Enter a display name" });
        return;
      }

      set({ configError: null });

      const guestConfig: DraftPodGuestConfig = {
        roomCode: joinCode.trim(),
        displayName: guestDisplayName.trim(),
      };

      try {
        await useMultiplayerDraftStore.getState().joinDraft(guestConfig);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        set({ configError: message });
      }
    },

    startDraft: async () => {
      await useMultiplayerDraftStore.getState().startDraft();
    },

    reset: () => {
      set(initialState);
    },
  }),
);
