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

import type { CubeDraftSettings, TournamentFormat, PodPolicy, DraftKind as CoreDraftKind } from "../adapter/draft-adapter";
import type { DraftPodHostConfig } from "../adapter/draftPodHostAdapter";
import type { DraftPodGuestConfig } from "../adapter/draftPodGuestAdapter";
import {
  clearActiveDraftPodIfCurrent,
  inspectActiveDraftPod,
  loadDraftHostSession,
  persistedDraftHostSessionState,
} from "../services/draftPersistence";
import { useMultiplayerDraftStore } from "./multiplayerDraftStore";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftKind = Exclude<CoreDraftKind, "Quick">;

export type PoolMode = "set" | "cube";

/** Result of one host-recovery probe. Page entry owns the route policy. */
export type HostedPodResumeOutcome = "resumed" | "absent" | "terminal" | "invalid" | "superseded";

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
  resumeHostedPod: (options?: { silent?: boolean; routeToken?: number; signal?: AbortSignal }) => Promise<HostedPodResumeOutcome>;
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

interface HostedPodResumeAttempt {
  routeToken: number;
  signal: AbortSignal | undefined;
  promise: Promise<HostedPodResumeOutcome>;
}

let resumeHostedPodAttempt: HostedPodResumeAttempt | null = null;

// ── Store ──────────────────────────────────────────────────────────────

export const useDraftPodStore = create<DraftPodState & DraftPodActions>()(
  (set, get) => ({
    ...initialState,

    setConfig: (partial) => {
      set((prev) => ({
        config: normalizePodConfig({ ...prev.config, ...partial }),
        poolMode: (partial.kind ?? prev.config.kind) === "Sealed" ? "set" : prev.poolMode,
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
      set((prev) => ({
        poolMode: prev.config.kind === "Sealed" ? "set" : mode,
        configError: null,
      }));
    },

    setCubeForm: (form) => {
      set({ cubeForm: form, configError: null });
    },

    createPod: async () => {
      const { config, hostDisplayName, poolMode, cubeForm } = get();

      if (config.kind === "Sealed" && poolMode !== "set") {
        set({ configError: "Sealed pods require a set pool" });
        return;
      }

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

    resumeHostedPod: async (options = {}) => {
      const routeToken = options.routeToken ?? 0;
      if (
        resumeHostedPodAttempt &&
        resumeHostedPodAttempt.routeToken === routeToken &&
        resumeHostedPodAttempt.signal === options.signal
      ) {
        return resumeHostedPodAttempt.promise;
      }

      let attempt!: HostedPodResumeAttempt;
      const isCurrentAttempt = () =>
        resumeHostedPodAttempt === attempt && !options.signal?.aborted;
      const promise = (async (): Promise<HostedPodResumeOutcome> => {
        if (options.signal?.aborted) return "superseded";
        const active = inspectActiveDraftPod();
        if (active.type === "absent") {
          if (!options.silent) set({ configError: "No draft pod to resume" });
          return "absent";
        }
        if (active.type === "invalid") {
          if (active.capture) clearActiveDraftPodIfCurrent(active.capture);
          if (!options.silent) set({ configError: "Saved draft pod is invalid" });
          return "invalid";
        }
        const { meta, capture } = active;

        const activeDraft = useMultiplayerDraftStore.getState();
        if (
          activeDraft.role === "host" &&
          activeDraft.phase !== "idle" &&
          activeDraft.phase !== "error" &&
          activeDraft.roomCode === meta.roomCode
        ) {
          return "resumed";
        }

        const persisted = await loadDraftHostSession(meta.id);
        if (!isCurrentAttempt()) return "superseded";
        if (!persisted) {
          clearActiveDraftPodIfCurrent(capture);
          if (!options.silent) set({ configError: "Saved draft pod was not found" });
          return "invalid";
        }
        const sessionState = persistedDraftHostSessionState(persisted);
        if (
          persisted.persistenceId !== meta.id ||
          persisted.roomCode !== meta.roomCode ||
          sessionState !== "live"
        ) {
          // Release only the active locator. A terminal snapshot is retained as
          // local history; a corrupt one is unreachable after this exact-match
          // cleanup and cannot poison a replacement pod.
          clearActiveDraftPodIfCurrent(capture);
          if (!options.silent) {
            set({ configError: sessionState === "terminal" ? "Saved draft pod is complete" : "Saved draft pod is invalid" });
          }
          return sessionState === "terminal" ? "terminal" : "invalid";
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

        if (!isCurrentAttempt()) return "superseded";
        const hosted = await useMultiplayerDraftStore.getState().hostDraft({
          ...hostConfig,
          signal: options.signal,
        });
        if (!isCurrentAttempt()) return "superseded";
        return hosted ? "resumed" : "invalid";
      })();
      attempt = { routeToken, signal: options.signal, promise };
      resumeHostedPodAttempt = attempt;

      try {
        return await promise;
      } finally {
        if (resumeHostedPodAttempt === attempt) resumeHostedPodAttempt = null;
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
