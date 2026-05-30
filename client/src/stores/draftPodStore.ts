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

import type { CubeDraftSettings, TournamentFormat, PodPolicy } from "../adapter/draft-adapter";
import type { DraftPodHostConfig } from "../adapter/draftPodHostAdapter";
import type { DraftPodGuestConfig } from "../adapter/draftPodGuestAdapter";
import {
  clearActiveDraftPod,
  loadActiveDraftPod,
  loadDraftHostSession,
} from "../services/draftPersistence";
import { useMultiplayerDraftStore } from "./multiplayerDraftStore";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftKind = "Premier" | "Traditional";

export type PoolMode = "set" | "cube";

export interface CubeForm {
  cubeName: string;
  cubeListText: string;
  settings: CubeDraftSettings;
}

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
  /** Which pool source the host is configuring: a Set pool or a custom Cube list. */
  poolMode: PoolMode;
  /** Cube form state (cube name + list text + settings); null when poolMode === "set". */
  cubeForm: CubeForm | null;
  /** Set pool JSON loaded from draft-pools.json. Set-mode cache only — unused in cube mode. */
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
  /** Switch between Set-pool and Cube-list pool modes. */
  setPoolMode: (mode: PoolMode) => void;
  /** Set the cube form (name + list text + settings) for cube-mode host setup. */
  setCubeForm: (form: CubeForm | null) => void;
  /** Load the set pool data and create a new pod as host. */
  createPod: () => Promise<void>;
  /** Join an existing pod as guest. */
  joinPod: () => Promise<void>;
  /** Resume the active hosted pod from local persistence. */
  resumeHostedPod: () => Promise<void>;
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
  poolMode: "set",
  cubeForm: null,
  setPoolJson: null,
  loadingPool: false,
  configError: null,
};

function normalizePodConfig(config: PodConfig): PodConfig {
  if (config.tournamentFormat === "SingleElimination") {
    return { ...config, podSize: 8 };
  }
  return config;
}

let resumeHostedPodPromise: Promise<void> | null = null;

// ── Store ──────────────────────────────────────────────────────────────

export const useDraftPodStore = create<DraftPodState & DraftPodActions>()(
  (set, get) => ({
    ...initialState,

    setConfig: (partial) => {
      set((prev) => ({
        config: normalizePodConfig({ ...prev.config, ...partial }),
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

    setPoolMode: (mode) => {
      set({ poolMode: mode, configError: null });
    },

    setCubeForm: (form) => {
      set({ cubeForm: form, configError: null });
    },

    createPod: async () => {
      const { config, hostDisplayName, poolMode, cubeForm } = get();

      if (!hostDisplayName.trim()) {
        set({ configError: "Enter a display name" });
        return;
      }

      if (poolMode === "set") {
        if (!config.setCode) {
          set({ configError: "Select a set first" });
          return;
        }

        set({ loadingPool: true, configError: null });

        try {
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

          const persistenceId = crypto.randomUUID();
          const hostConfig: DraftPodHostConfig = {
            poolInput: { type: "Set", data: { set_pool_json: poolJson } },
            kind: config.kind,
            podSize: config.podSize,
            hostDisplayName: hostDisplayName.trim(),
            tournamentFormat: config.tournamentFormat,
            podPolicy: config.podPolicy,
            persistenceId,
          };

          await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          set({ configError: message, loadingPool: false });
        }
        return;
      }

      // Cube mode: skip the draft-pools.json fetch entirely; everything the
      // host needs lives on the cubeForm object.
      if (!cubeForm || !cubeForm.cubeListText.trim()) {
        set({ configError: "Paste a cube list first" });
        return;
      }
      if (!cubeForm.cubeName.trim()) {
        set({ configError: "Enter a cube name" });
        return;
      }

      try {
        const persistenceId = crypto.randomUUID();
        const hostConfig: DraftPodHostConfig = {
          poolInput: {
            type: "Cube",
            data: {
              cube_list_text: cubeForm.cubeListText,
              cube_name: cubeForm.cubeName,
              cube_draft_settings: cubeForm.settings,
            },
          },
          kind: config.kind,
          podSize: config.podSize,
          hostDisplayName: hostDisplayName.trim(),
          tournamentFormat: config.tournamentFormat,
          podPolicy: config.podPolicy,
          persistenceId,
        };

        await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        set({ configError: message });
      }
    },

    resumeHostedPod: async () => {
      if (resumeHostedPodPromise) {
        return resumeHostedPodPromise;
      }

      resumeHostedPodPromise = (async () => {
        const meta = loadActiveDraftPod();
        if (!meta) {
          set({ configError: "No draft pod to resume" });
          return;
        }

        const activeDraft = useMultiplayerDraftStore.getState();
        if (
          activeDraft.role === "host" &&
          activeDraft.phase !== "idle" &&
          activeDraft.phase !== "error" &&
          activeDraft.roomCode === meta.roomCode
        ) {
          return;
        }

        const persisted = await loadDraftHostSession(meta.id);
        if (!persisted) {
          clearActiveDraftPod();
          set({ configError: "Saved draft pod was not found" });
          return;
        }

        // Branch on the persisted pool source: restore the matching UI
        // state (poolMode + cubeForm or setPoolJson cache) so a refresh
        // mid-pod lands the host back on the same tab they configured.
        if (persisted.poolInput.type === "Cube") {
          const cubeData = persisted.poolInput.data;
          set({
            config: {
              setCode: "custom-cube",
              setName: cubeData.cube_name,
              kind: persisted.kind,
              podSize: persisted.podSize,
              tournamentFormat: persisted.tournamentFormat,
              podPolicy: persisted.podPolicy,
            },
            hostDisplayName: persisted.hostDisplayName,
            poolMode: "cube",
            cubeForm: {
              cubeName: cubeData.cube_name,
              cubeListText: cubeData.cube_list_text,
              settings: cubeData.cube_draft_settings,
            },
            setPoolJson: null,
            loadingPool: false,
            configError: null,
          });
        } else {
          set({
            config: {
              setCode: "",
              setName: "Draft Pod",
              kind: persisted.kind,
              podSize: persisted.podSize,
              tournamentFormat: persisted.tournamentFormat,
              podPolicy: persisted.podPolicy,
            },
            hostDisplayName: persisted.hostDisplayName,
            poolMode: "set",
            cubeForm: null,
            setPoolJson: persisted.poolInput.data.set_pool_json,
            loadingPool: false,
            configError: null,
          });
        }

        const hostConfig: DraftPodHostConfig = {
          poolInput: persisted.poolInput,
          kind: persisted.kind,
          podSize: persisted.podSize,
          hostDisplayName: persisted.hostDisplayName,
          tournamentFormat: persisted.tournamentFormat,
          podPolicy: persisted.podPolicy,
          persistenceId: persisted.persistenceId,
          preferredRoomCode: persisted.roomCode || undefined,
        };

        await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
      })();

      try {
        await resumeHostedPodPromise;
      } finally {
        resumeHostedPodPromise = null;
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
      await useMultiplayerDraftStore.getState().startDraft(get().botFillEnabled);
    },

    reset: () => {
      set(initialState);
    },
  }),
);
