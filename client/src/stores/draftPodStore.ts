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

import { DraftAdapter, distinctJoined, setPackSequence, type CubeDraftSettings, type DraftProcedure, type PackDistribution, type PoolInput, type SetPackSequence, type TournamentFormat, type PodPolicy } from "../adapter/draft-adapter";
import type { DraftPackChoice } from "./draftStore";
import type { DraftPodHostConfig } from "../adapter/draftPodHostAdapter";
import type { DraftPodGuestConfig } from "../adapter/draftPodGuestAdapter";
import {
  clearActiveDraftPodIfCurrent,
  inspectActiveDraftPod,
  loadDraftHostSession,
  persistedDraftHostSessionState,
} from "../services/draftPersistence";
import { parseWebSocketUrl } from "../services/serverDetection";
import { DRAFT_OFFLINE_ERROR, useMultiplayerDraftStore } from "./multiplayerDraftStore";
import { useMultiplayerStore } from "./multiplayerStore";
import { getEffectiveOffline } from "./connectivityStore";
import type { DraftKind } from "../components/draft/draftKind";

// ── Types ──────────────────────────────────────────────────────────────

export type PoolMode = "set" | "cube";
/** How a set-backed pod maps its selected sets onto boosters. */
export type SetDraftMode = "uniform" | "chaos";

/** Result of one host-recovery probe. Page entry owns the route policy. */
export type HostedPodResumeOutcome = "resumed" | "absent" | "terminal" | "invalid" | "offline" | "superseded";

/** The backup API is served beside the selected phase-server's WebSocket API. */
function configuredBackupEndpoint(): string | undefined {
  const hosting = useMultiplayerStore.getState().hostingServer;
  const server = hosting === null ? null : parseWebSocketUrl(hosting);
  if (!server) return undefined;
  server.protocol = server.protocol === "wss:" ? "https:" : "http:";
  return server.origin;
}

export interface CubeForm {
  cubeName: string;
  cubeListText: string;
  settings: CubeDraftSettings;
}

export interface PodConfig {
  /**
   * The set filling each booster, in the order the host arranged them. One
   * entry per pack the pod opens; the same set may fill several. Empty until
   * the host picks, and a Cube pod leaves it empty — its pool is `cubeForm`.
   */
  packs: DraftPackChoice[];
  /**
   * Display label for the whole pool. Mirrors the engine's own source label
   * (`DraftSource::set_code`), which joins the DISTINCT set codes in
   * first-appearance order, so a mixed pod reads as "ISD+DKA+AVR".
   */
  setCode: string;
  setName: string;
  kind: DraftKind;
  podSize: number;
  tournamentFormat: TournamentFormat;
  podPolicy: PodPolicy;
}

type ProcedureCacheKey = Pick<PodConfig, "kind" | "tournamentFormat">;

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
  /** Set pods either follow the host's pack order or draw every seat's packs from candidates. */
  setDraftMode: SetDraftMode;
  /** Cube form state (cube name + list text + settings); null when poolMode === "set". */
  cubeForm: CubeForm | null;
  /** Set pool JSON loaded from draft-pools.json. Set-mode cache only — unused in cube mode. */
  setPoolJson: string | null;
  /** Loading state while fetching set pool data. */
  loadingPool: boolean;
  /** Error from pool loading or pod creation. */
  configError: string | null;
  /** Exact engine-allowed seat counts for the selected tournament format. */
  allowedPodSizes: number[] | null;
  /** Configuration for which the procedure cache was published. */
  procedureCacheKey: ProcedureCacheKey | null;
  /** A deep-link entry may adopt the engine procedure's default seat count once. */
  pendingProcedureDefault: ProcedureCacheKey | null;
  /** Engine-published pack delivery behavior. `null` until the kind procedure loads. */
  packDistribution: PackDistribution | null;
  /**
   * The kind's engine-published booster count (`DraftProcedure.packs_per_player`),
   * cached alongside `allowedPodSizes` and on the same terms: a copy of an engine
   * value, never a client derivation. It fixes how many sets the host arranges,
   * so a Sealed pod asks for six and a draft pod for three without the page
   * knowing either number.
   *
   * `null` until loaded and after `reset()`. Fail-CLOSED like the seat floor:
   * with no answer yet the selector cannot be filled in, and the engine still
   * refuses a sequence longer than the kind opens regardless of this cache.
   */
  packsPerPlayer: number | null;
}

interface DraftPodActions {
  /** Update pod configuration fields. */
  setConfig: (partial: Partial<PodConfig>) => void;
  /** Enter pod setup for `kind`, adopting the ENGINE's per-kind table default for
   *  pod size (`DraftProcedure.pod_size`) rather than re-deriving one in the client.
   *  The host may still override it with the pod-size selector before creating. */
  enterKind: (kind: DraftKind) => Promise<void>;
  /** Enter from a URL intent, preserving the engine default across a competing setup refresh. */
  enterKindForEntry: (kind: DraftKind) => Promise<void>;
  /** Toggle bot-fill on/off. */
  toggleBotFill: () => void;
  /** Set host display name. */
  setHostDisplayName: (name: string) => void;
  /** Set guest display name. */
  setGuestDisplayName: (name: string) => void;
  /** Set join code for guest. */
  setJoinCode: (code: string) => void;
  /**
   * Refresh the cached engine procedure axes for the currently selected kind.
   *
   * `setConfig({ kind })` — what the kind radios call — records the host's
   * intent but publishes nothing, so the cached axes would otherwise describe
   * whichever kind was loaded last. Unlike `enterKind` this does NOT adopt the
   * kind's default pod size: the host may already have chosen one, and
   * switching kinds must not silently discard it.
   */
  refreshProcedure: () => Promise<void>;
  /** Switch between Set-pool and Cube-list pool modes. */
  setPoolMode: (mode: PoolMode) => void;
  /** Choose the host's ordered lineup or host-local Chaos candidate pool. */
  setSetDraftMode: (mode: SetDraftMode) => void;
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
    packs: [],
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
  setDraftMode: "uniform",
  cubeForm: null,
  setPoolJson: null,
  loadingPool: false,
  configError: null,
  allowedPodSizes: null,
  procedureCacheKey: null,
  pendingProcedureDefault: null,
  packDistribution: null,
  packsPerPlayer: null,
};

/**
 * The pack list a persisted pod was configured with.
 *
 * Only the live `SetPackSequence` spelling carries one. A pod persisted before
 * multi-set pods existed holds a single serialized pool and no sequence at all;
 * draft-wasm still starts it (every booster from that one set), so resuming
 * must not fail — it just has no per-pack list to show, and returns empty.
 * Names are not persisted, so each entry is labelled by its own code.
 */
function persistedPodPacks(poolInput: PoolInput): DraftPackChoice[] {
  if (poolInput.type === "Chaos") {
    return poolInput.data.candidate_codes.map((code) => ({ code, name: code }));
  }
  if (poolInput.type !== "Set") return [];
  const sequence = (poolInput.data as Partial<SetPackSequence>).sequence;
  if (!Array.isArray(sequence)) return [];
  return sequence.map((code) => ({ code, name: code }));
}

interface HostedPodResumeAttempt {
  routeToken: number;
  signal: AbortSignal | undefined;
  promise: Promise<HostedPodResumeOutcome>;
}

let resumeHostedPodAttempt: HostedPodResumeAttempt | null = null;

// Every procedure read competes to publish the same cache. A kind comparison
// alone rejects cross-kind replies, but cannot distinguish two reads for the
// same kind (for example, setup entry followed by a refresh). Keep one
// monotonically increasing identity so only the newest read may publish.
let podOrchestrationGeneration = 0;

function beginPodOrchestration(set: (partial: Partial<DraftPodState>) => void): number {
  podOrchestrationGeneration += 1;
  // A newer public operation retires any pool spinner owned by the older
  // generation before that older continuation can settle.
  set({ loadingPool: false });
  return podOrchestrationGeneration;
}

function isCurrentPodOrchestration(generation: number): boolean {
  return generation === podOrchestrationGeneration;
}

/**
 * Fetch `kind`'s engine-published procedure for the caller to publish.
 *
 * Callers cache the engine-owned allowed-size set only after their request
 * remains current. The client renders that set directly and the reducer still
 * validates every submitted pod size.
 */
async function loadProcedure(
  kind: DraftKind,
  tournamentFormat: TournamentFormat,
): Promise<DraftProcedure> {
  return new DraftAdapter().draftProcedure(kind, tournamentFormat);
}

function procedureTargetMatchesConfig(
  target: ProcedureCacheKey,
  config: PodConfig,
): boolean {
  return target.kind === config.kind && target.tournamentFormat === config.tournamentFormat;
}

function procedureCache(
  procedure: DraftProcedure,
  procedureCacheKey: ProcedureCacheKey,
): Pick<
  DraftPodState,
  "allowedPodSizes" | "procedureCacheKey" | "packDistribution" | "packsPerPlayer"
> {
  return {
    allowedPodSizes: procedure.allowed_pod_sizes,
    procedureCacheKey,
    packDistribution: procedure.distribution,
    packsPerPlayer: procedure.packs_per_player,
  };
}

// ── Store ──────────────────────────────────────────────────────────────

export const useDraftPodStore = create<DraftPodState & DraftPodActions>()(
  (set, get) => ({
    ...initialState,

    setConfig: (partial) => {
      set((prev) => {
        const kindChanged = partial.kind !== undefined && partial.kind !== prev.config.kind;
        const tournamentFormatChanged =
          partial.tournamentFormat !== undefined
          && partial.tournamentFormat !== prev.config.tournamentFormat;
        const procedureChanged = kindChanged || tournamentFormatChanged;
        const packDistribution = kindChanged ? null : prev.packDistribution;

        return {
          config: { ...prev.config, ...partial },
          poolMode: packDistribution === "AllAtOnce" ? "set" : prev.poolMode,
          // The allowed seat set belongs to both the draft kind and tournament
          // format. Clear it until the engine publishes the newly selected
          // procedure so the selector cannot offer values from the prior shape.
          allowedPodSizes: procedureChanged ? null : prev.allowedPodSizes,
          procedureCacheKey: procedureChanged ? null : prev.procedureCacheKey,
          pendingProcedureDefault: procedureChanged ? null : prev.pendingProcedureDefault,
          packDistribution,
          packsPerPlayer: kindChanged ? null : prev.packsPerPlayer,
          loadingPool: false,
          configError: null,
        };
      });
    },

    enterKind: async (kind) => {
      if (getEffectiveOffline()) {
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
      const procedureRequest = beginPodOrchestration(set);
      // Apply the kind first: it is the entry point's whole purpose and must not
      // depend on the wasm load succeeding. `setConfig` is the single authority for
      // the Sealed pool-mode rule.
      get().setConfig({ kind });
      const target: ProcedureCacheKey = {
        kind,
        tournamentFormat: get().config.tournamentFormat,
      };
      try {
        const procedure = await loadProcedure(target.kind, target.tournamentFormat);
        // A newer entry or refresh can target the same kind, so kind equality
        // alone is insufficient to protect the cache and adopted default.
        if (
          !isCurrentPodOrchestration(procedureRequest)
          || !procedureTargetMatchesConfig(target, get().config)
        ) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        set({ ...procedureCache(procedure, target), pendingProcedureDefault: null });
        get().setConfig({ podSize: procedure.pod_size });
      } catch (err) {
        if (
          !isCurrentPodOrchestration(procedureRequest)
          || !procedureTargetMatchesConfig(target, get().config)
        ) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        set({ configError: err instanceof Error ? err.message : String(err) });
      }
    },

    enterKindForEntry: async (kind) => {
      if (getEffectiveOffline()) {
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
      const entering = get().enterKind(kind);
      const target: ProcedureCacheKey = {
        kind,
        tournamentFormat: get().config.tournamentFormat,
      };
      set({ pendingProcedureDefault: target });
      await entering;
    },

    refreshProcedure: async () => {
      if (getEffectiveOffline()) {
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
      const { kind, tournamentFormat } = get().config;
      const target: ProcedureCacheKey = { kind, tournamentFormat };
      const procedureRequest = beginPodOrchestration(set);
      try {
        const procedure = await loadProcedure(target.kind, target.tournamentFormat);
        // The host may have switched kinds or superseded this read with another
        // entry/refresh while it was in flight; drop that stale publication.
        if (
          !isCurrentPodOrchestration(procedureRequest)
          || !procedureTargetMatchesConfig(target, get().config)
        ) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        const adoptsProcedureDefault = get().pendingProcedureDefault?.kind === target.kind
          && get().pendingProcedureDefault?.tournamentFormat === target.tournamentFormat;
        set({
          ...procedureCache(procedure, target),
          pendingProcedureDefault: adoptsProcedureDefault ? null : get().pendingProcedureDefault,
        });
        if (adoptsProcedureDefault) {
          get().setConfig({ podSize: procedure.pod_size });
        } else if (!procedure.allowed_pod_sizes.includes(get().config.podSize)) {
          get().setConfig({ podSize: procedure.allowed_pod_sizes[0] });
        }
      } catch (err) {
        if (
          !isCurrentPodOrchestration(procedureRequest)
          || !procedureTargetMatchesConfig(target, get().config)
        ) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        set({ configError: err instanceof Error ? err.message : String(err) });
      }
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
        poolMode: prev.packDistribution === "AllAtOnce" ? "set" : mode,
        configError: null,
      }));
    },

    setSetDraftMode: (setDraftMode) => {
      set({ setDraftMode, configError: null });
    },

    setCubeForm: (form) => {
      set({ cubeForm: form, configError: null });
    },

    createPod: async () => {
      if (getEffectiveOffline()) {
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
      let { config, poolMode, setDraftMode } = get();
      const { hostDisplayName, cubeForm } = get();

      if (!hostDisplayName.trim()) {
        set({ configError: "Enter a display name" });
        return;
      }

      // Cache every engine-published procedure axis before either host branch;
      // both lead to the same lobby. The newest request wins if setup changes
      // while this asynchronous read is in flight.
      const procedureRequest = beginPodOrchestration(set);
      const target: ProcedureCacheKey = {
        kind: config.kind,
        tournamentFormat: config.tournamentFormat,
      };
      try {
        const procedure = await loadProcedure(target.kind, target.tournamentFormat);
        if (
          !isCurrentPodOrchestration(procedureRequest)
          || !procedureTargetMatchesConfig(target, get().config)
          || get().config !== config
        ) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        if (procedure.distribution === "AllAtOnce" && poolMode !== "set") {
          set({ configError: "This procedure requires a set pool" });
          return;
        }
        set(procedureCache(procedure, target));
        if (!procedure.allowed_pod_sizes.includes(config.podSize)) {
          get().setConfig({ podSize: procedure.allowed_pod_sizes[0] });
        }
        get().setConfig({});
        config = get().config;
        poolMode = get().poolMode;
        setDraftMode = get().setDraftMode;
      } catch (err) {
        if (
          !isCurrentPodOrchestration(procedureRequest)
          || !procedureTargetMatchesConfig(target, get().config)
          || get().config !== config
        ) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        set({ configError: err instanceof Error ? err.message : String(err) });
      }

      if (poolMode === "set") {
        if (config.packs.length === 0) {
          set({ configError: "Select a set first" });
          return;
        }

        // No `configError: null`: every other writer above in this function
        // returns, so clearing here would only erase `loadProcedure`'s catch.
        set({ loadingPool: true });

        try {
          const resp = await fetch(__DRAFT_POOLS_URL__);
          if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR, loadingPool: false });
            return;
          }
          if (!resp.ok) {
            throw new Error(`Failed to load draft pools: ${resp.status}`);
          }
          const allPools: Record<string, unknown> = await resp.json();
          const selection = setPackSequence(config.packs, allPools);

          if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR, loadingPool: false });
            return;
          }

          set({ setPoolJson: JSON.stringify(selection), loadingPool: false });

          const persistenceId = crypto.randomUUID();
          const poolInput: PoolInput = setDraftMode === "chaos"
            ? {
                type: "Chaos",
                data: {
                  pools: selection.pools,
                  candidate_codes: selection.sequence,
                },
              }
            : { type: "Set", data: selection };
          const hostConfig: DraftPodHostConfig = {
            poolInput,
            kind: config.kind,
            podSize: config.podSize,
            hostDisplayName: hostDisplayName.trim(),
            tournamentFormat: config.tournamentFormat,
            podPolicy: config.podPolicy,
            persistenceId,
            backupEndpoint: configuredBackupEndpoint(),
          };

          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR });
            return;
          }
          const hosted = await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
          if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
          if (hosted) return;
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR });
            return;
          }
          set({ configError: useMultiplayerDraftStore.getState().error ?? "Unable to host draft pod" });
        } catch (err) {
          if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR, loadingPool: false });
            return;
          }
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
        if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
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
          backupEndpoint: configuredBackupEndpoint(),
        };

        const hosted = await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
        if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
        if (hosted) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        set({ configError: useMultiplayerDraftStore.getState().error ?? "Unable to host draft pod" });
      } catch (err) {
        if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        const message = err instanceof Error ? err.message : String(err);
        set({ configError: message });
      }
    },

    resumeHostedPod: async (options = {}) => {
      if (getEffectiveOffline()) {
        set({ configError: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      const routeToken = options.routeToken ?? 0;
      if (
        resumeHostedPodAttempt &&
        resumeHostedPodAttempt.routeToken === routeToken &&
        resumeHostedPodAttempt.signal === options.signal
      ) {
        return resumeHostedPodAttempt.promise;
      }

      const attempt: HostedPodResumeAttempt = {
        routeToken,
        signal: options.signal,
        promise: Promise.resolve("superseded"),
      };
      const isCurrentAttempt = () =>
        resumeHostedPodAttempt === attempt && !options.signal?.aborted;
      attempt.promise = (async (): Promise<HostedPodResumeOutcome> => {
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

        // Fence the persisted read itself. A new entry, refresh, creation, or
        // reset that starts while storage is pending must win before this
        // resume can publish its recovered configuration.
        const procedureRequest = beginPodOrchestration(set);
        let persisted: Awaited<ReturnType<typeof loadDraftHostSession>>;
        try {
          persisted = await loadDraftHostSession(meta.id);
        } catch (err) {
          if (!isCurrentAttempt() || !isCurrentPodOrchestration(procedureRequest)) return "superseded";
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR });
            return "offline";
          }
          set({ configError: err instanceof Error ? err.message : String(err) });
          return "invalid";
        }
        if (!isCurrentAttempt() || !isCurrentPodOrchestration(procedureRequest)) return "superseded";
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return "offline";
        }
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
              packs: [],
              setCode: "custom-cube",
              setName: cubeData.cube_name,
              kind: persisted.kind,
              podSize: persisted.podSize,
              tournamentFormat: persisted.tournamentFormat,
              podPolicy: persisted.podPolicy,
            },
            hostDisplayName: persisted.hostDisplayName,
            poolMode: "cube",
            setDraftMode: "uniform",
            cubeForm: {
              cubeName: cubeData.cube_name,
              cubeListText: cubeData.cube_list_text,
              settings: cubeData.cube_draft_settings,
            },
            setPoolJson: null,
            loadingPool: false,
            configError: null,
            allowedPodSizes: null,
            procedureCacheKey: null,
            packDistribution: null,
            packsPerPlayer: null,
          });
        } else {
          // Restore the pack sequence the pod was configured with, so a host
          // who refreshes mid-lobby sees the sets they arranged rather than an
          // unlabelled pod. A snapshot in the pre-multi-set spelling has no
          // sequence to restore; its packs stay empty and the label falls back.
          const packs = persistedPodPacks(persisted.poolInput);
          set({
            config: {
              packs,
              setCode: distinctJoined(packs.map((pack) => pack.code), "+"),
              setName: packs.length > 0 ? distinctJoined(packs.map((pack) => pack.name), " · ") : "Draft Pod",
              kind: persisted.kind,
              podSize: persisted.podSize,
              tournamentFormat: persisted.tournamentFormat,
              podPolicy: persisted.podPolicy,
            },
            hostDisplayName: persisted.hostDisplayName,
            poolMode: "set",
            setDraftMode: persisted.poolInput.type === "Chaos" ? "chaos" : "uniform",
            cubeForm: null,
            setPoolJson: JSON.stringify(persisted.poolInput.data),
            loadingPool: false,
            configError: null,
            allowedPodSizes: null,
            procedureCacheKey: null,
            packDistribution: null,
            packsPerPlayer: null,
          });
        }

        // Rehydrate the complete engine procedure cache for the resumed lobby.
        // A later entry/refresh is allowed to supersede this publication.
        const target: ProcedureCacheKey = {
          kind: persisted.kind,
          tournamentFormat: persisted.tournamentFormat,
        };
        try {
          const procedure = await loadProcedure(target.kind, target.tournamentFormat);
          if (
            !isCurrentAttempt()
            || !isCurrentPodOrchestration(procedureRequest)
            || !procedureTargetMatchesConfig(target, get().config)
          ) return "superseded";
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR });
            return "offline";
          }
          set(procedureCache(procedure, target));
          if (!procedure.allowed_pod_sizes.includes(get().config.podSize)) {
            get().setConfig({ podSize: procedure.allowed_pod_sizes[0] });
          }
        } catch (err) {
          if (
            !isCurrentAttempt()
            || !isCurrentPodOrchestration(procedureRequest)
            || !procedureTargetMatchesConfig(target, get().config)
          ) return "superseded";
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR });
            return "offline";
          }
          set({ configError: err instanceof Error ? err.message : String(err) });
        }

        const hostConfig: DraftPodHostConfig = {
          poolInput: persisted.poolInput,
          kind: persisted.kind,
          podSize: get().config.podSize,
          hostDisplayName: persisted.hostDisplayName,
          tournamentFormat: persisted.tournamentFormat,
          podPolicy: persisted.podPolicy,
          persistenceId: persisted.persistenceId,
          preferredRoomCode: persisted.roomCode || undefined,
          backupEndpoint: configuredBackupEndpoint(),
        };

        if (!isCurrentAttempt() || !isCurrentPodOrchestration(procedureRequest)) {
          return "superseded";
        }
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return "offline";
        }
        const hosted = await useMultiplayerDraftStore.getState().hostDraft({
          ...hostConfig,
          signal: options.signal,
        });
        if (!isCurrentAttempt() || !isCurrentPodOrchestration(procedureRequest)) {
          return "superseded";
        }
        if (!hosted && getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return "offline";
        }
        return hosted ? "resumed" : "invalid";
      })();
      resumeHostedPodAttempt = attempt;

      try {
        return await attempt.promise;
      } finally {
        if (resumeHostedPodAttempt === attempt) resumeHostedPodAttempt = null;
      }
    },

    joinPod: async () => {
      if (getEffectiveOffline()) {
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
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
        kind: "new",
        roomCode: joinCode.trim(),
        displayName: guestDisplayName.trim(),
      };

      const request = beginPodOrchestration(set);
      try {
        if (!isCurrentPodOrchestration(request)) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        const joined = await useMultiplayerDraftStore.getState().joinDraft(guestConfig);
        if (!isCurrentPodOrchestration(request)) return;
        if (!joined && getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
        }
      } catch (err) {
        if (!isCurrentPodOrchestration(request)) return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        const message = err instanceof Error ? err.message : String(err);
        set({ configError: message });
      }
    },

    startDraft: async () => {
      if (getEffectiveOffline()) {
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
      await useMultiplayerDraftStore.getState().startDraft(get().botFillEnabled);
    },

    reset: () => {
      beginPodOrchestration(set);
      set(initialState);
    },
  }),
);
