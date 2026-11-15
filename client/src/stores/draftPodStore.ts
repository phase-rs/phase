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
import i18n from "i18next";

import { DraftAdapter, distinctJoined, setPackSequence, type CubeDraftSettings, type DraftProcedure, type PackDistribution, type PoolInput, type PostDraftPlay, type SetLayoutKind, type SetPackSequence, type TournamentFormat, type PodPolicy } from "../adapter/draft-adapter";
import { CUSTOM_CUBE_SET_CODE } from "../adapter/draftKinds";
import type { DraftPackChoice } from "./draftStore";
import type { DraftPodHostConfig, DraftPodListing } from "../adapter/draftPodHostAdapter";
import type { DraftPodGuestConfig } from "../adapter/draftPodGuestAdapter";
import type { DraftLobbyMetadata } from "../adapter/types";
import { openBrokerClient, type BrokerClient } from "../services/brokerClient";
import {
  clearActiveDraftPodIfCurrent,
  inspectActiveDraftPod,
  loadDraftHostSession,
  type ActiveDraftPodMetaCapture,
  type DraftHostSessionLoadResult,
} from "../services/draftPersistence";
import { parseWebSocketUrl } from "../services/serverDetection";
import {
  DRAFT_OFFLINE_ERROR,
  useMultiplayerDraftStore,
  type DraftRole,
  type DraftSessionOpenOutcome,
  type HostedDraftRecoveryOutcome,
  type MultiplayerDraftPhase,
} from "./multiplayerDraftStore";
import { preflightDraftPodHostRecovery } from "../adapter/draftPodHostAdapter";
import { useMultiplayerStore } from "./multiplayerStore";
import { getEffectiveOffline } from "./connectivityStore";
import type { DraftKind } from "../components/draft/draftKind";

// ── Types ──────────────────────────────────────────────────────────────

export type PoolMode = "set" | "cube";
/** How a set-backed pod maps its selected sets onto boosters. */
export type SetDraftMode = "uniform" | "chaos";

/** Result of one host-recovery probe. Page entry owns the route policy. */
export type HostedPodResumeOutcome =
  | "resumed"
  | "absent"
  | "terminal"
  | "invalid"
  | "incompatible"
  | "readFailure"
  | "initializationFailure"
  | "offline"
  | "superseded";

export interface HostedPodRecoveryFailure {
  kind: "incompatible" | "readFailure" | "initializationFailure";
  capture: ActiveDraftPodMetaCapture;
  /** The route and store generation that were current when this became visible. */
  routeToken: number;
  orchestrationGeneration: number;
  raw?: unknown;
  error?: unknown;
}

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

// ── Lobby listing ──────────────────────────────────────────────────────

/** Public-lobby listing form state for the next created pod. */
export interface PodListing {
  isPublic: boolean;
  password: string;
  roomName: string;
}

/**
 * The lobby broker advertises at most this many seats for a new
 * registration (`lobby-broker` `Broker::handle_create_game`), so a larger
 * pod would be listed with fewer seats than it has.
 */
export const LOBBY_LISTING_MAX_SEATS = 6;

/** Whether a pod of `podSize` seats can be listed with a truthful seat count. */
export function podListingEligible(podSize: number): boolean {
  return podSize <= LOBBY_LISTING_MAX_SEATS;
}

/**
 * The lobby broker's bounds on each listing label (`lobby-broker`
 * `validation.rs`); the character bounds count code points, and the cube
 * name shares the room-name bound.
 */
export const LOBBY_HOST_NAME_MAX_CHARS = 20;
export const LOBBY_LABEL_MAX_CHARS = 40;
export const LOBBY_PASSWORD_MAX_BYTES = 128;

/** A pool source that can carry a truthful lobby-listing label. The legacy
 * `{ set_pool_json }` Set spelling carries no set code to list. */
type ListablePoolInput =
  | Exclude<PoolInput, { type: "Set" }>
  | { type: "Set"; data: SetPackSequence };

/**
 * The lobby-listing label for a pod's pool. Matches the server's own draft
 * source label (`server-core` `draft_lobby_source_label`): the Chaos arm
 * names only the host-chosen candidate sets, never the private per-seat
 * assignment.
 */
export function podLobbyMetadata(
  poolInput: ListablePoolInput,
  kind: DraftKind,
): DraftLobbyMetadata {
  switch (poolInput.type) {
    case "Set":
      return { setCode: distinctJoined(poolInput.data.sequence, "+"), draftKind: kind };
    case "Chaos":
      return { setCode: `Chaos:${poolInput.data.candidate_codes.join("+")}`, draftKind: kind };
    case "Cube":
      return {
        setCode: CUSTOM_CUBE_SET_CODE,
        draftKind: kind,
        cubeName: poolInput.data.cube_name.trim(),
      };
  }
}

/**
 * The broker registration request for a listed pod. A blank room name lists
 * under the same default HostSetup shows as its placeholder.
 */
export function podListingRequest(
  poolInput: ListablePoolInput,
  hostConfig: Pick<DraftPodHostConfig, "hostDisplayName" | "podSize" | "kind">,
  listing: PodListing,
): DraftPodListing["request"] {
  return {
    displayName: hostConfig.hostDisplayName,
    public: true,
    password: listing.password || null,
    timerSeconds: null,
    playerCount: hostConfig.podSize,
    matchConfig: { match_type: "Bo1" },
    formatConfig: null,
    roomName: listing.roomName.trim()
      || i18n.t("multiplayer:hostSetup.roomNameDefaultPlaceholder", { name: hostConfig.hostDisplayName }),
    draftMetadata: podLobbyMetadata(poolInput, hostConfig.kind),
    ranked: false,
  };
}

/**
 * The first listing label out of the broker's bounds, localized, or `null`
 * when every label is within them.
 */
export function podListingRefusal(request: DraftPodListing["request"]): string | null {
  if (Array.from(request.displayName).length > LOBBY_HOST_NAME_MAX_CHARS) {
    return i18n.t("draft:podSetup.listingHostNameTooLong", { max: LOBBY_HOST_NAME_MAX_CHARS });
  }
  if (Array.from(request.roomName ?? "").length > LOBBY_LABEL_MAX_CHARS) {
    return i18n.t("draft:podSetup.listingRoomNameTooLong", { max: LOBBY_LABEL_MAX_CHARS });
  }
  const cubeName = request.draftMetadata.cubeName;
  if (cubeName !== undefined && Array.from(cubeName).length > LOBBY_LABEL_MAX_CHARS) {
    return i18n.t("draft:podSetup.listingCubeNameTooLong", { max: LOBBY_LABEL_MAX_CHARS });
  }
  if (request.password !== null && new TextEncoder().encode(request.password).length > LOBBY_PASSWORD_MAX_BYTES) {
    return i18n.t("draft:podSetup.listingPasswordTooLong");
  }
  return null;
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
  /** Public-lobby listing chosen for the next created pod. The store's own
   * initial value is unlisted; the setup form seeds the host's remembered
   * choice through `adoptRememberedListing`. A pod above
   * `LOBBY_LISTING_MAX_SEATS` is still created unlisted regardless. */
  listing: PodListing;
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
  /** Capture-bound host recovery error. It is deliberately not guest state. */
  hostRecoveryFailure: HostedPodRecoveryFailure | null;
  /** Exact engine-allowed seat counts for the selected tournament format. */
  allowedPodSizes: number[] | null;
  /** Configuration for which the procedure cache was published. */
  procedureCacheKey: ProcedureCacheKey | null;
  /** A deep-link entry may adopt the engine procedure's default seat count once. */
  pendingProcedureDefault: ProcedureCacheKey | null;
  /**
   * Engine-published pack delivery behavior. `null` until the kind procedure
   * loads.
   *
   * `PackDistribution` now carries the tagged `{ SharedStackPiles: { … } }`
   * member as well as its two string members, and every
   * `packDistribution === "AllAtOnce"` comparison in the client stays both
   * type-valid and meaning-correct against it: each asks "is this the one-shot
   * sealed shape?", whose answer for a shared stack is `false`. They are
   * verified against the widened union, deliberately not rewritten — a
   * centralizing helper would buy consistency and no compiler force.
   */
  packDistribution: PackDistribution | null;
  /**
   * Which set-layout shapes the selected kind admits, as published by the
   * engine. `null` until a procedure has been published for the current
   * selection -- the same "not yet known" shape `allowedPodSizes` uses.
   */
  allowedSetLayouts: SetLayoutKind[] | null;
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
  /** Engine-published cube deck-size floor for `procedureCacheKey`. */
  cubeMinDeckSize: number | null;
  /** Engine-published post-draft play for the selected kind; `null` until published, kept across a format change. */
  postDraftPlay: PostDraftPlay | null;
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
  /** Merge into the pod's public-lobby listing. */
  setListing: (partial: Partial<PodListing>) => void;
  /** Set guest display name. */
  setGuestDisplayName: (name: string) => void;
  /**
   * Seed both pod display-name fields from the saved multiplayer identity
   * (`multiplayerStore.displayName`), filling only a field still empty.
   *
   * Read at call time rather than captured into `initialState`, because that
   * identity is editable — `PlayerIdentityBanner`, and Preferences →
   * Multiplayer — long after this module is evaluated.
   *
   * The empty-field guard exists for the name the player typed for this pod.
   * A host session restored by `resumeHostedPod` survives for a different
   * reason: that path assigns `hostDisplayName` unconditionally, and on the
   * page it lands after this seed rather than before it.
   */
  adoptSavedDisplayName: () => void;
  /** Seeds the listing choice from the host's last submitted one, on when
   *  there is none. */
  adoptRememberedListing: () => void;
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
  /** Resume the active hosted pod from local persistence. With `entry: "auto"`
   *  it yields (`"superseded"`) to a session this tab already holds. */
  resumeHostedPod: (options?: {
    silent?: boolean;
    entry?: "auto" | "host";
    routeToken?: number;
    signal?: AbortSignal;
    /** A rendered failure may retry only the exact attempt that produced it. */
    expectedFailure?: HostedPodRecoveryFailure;
  }) => Promise<HostedPodResumeOutcome>;
  /**
   * Retires a host error before an intentional guest/new-route transition.
   * It also ends a pending host recovery, and starts no pod operation.
   */
  retireHostedPodRecoveryFailure: () => void;
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
  listing: { isPublic: false, password: "", roomName: "" },
  guestDisplayName: "",
  joinCode: "",
  poolMode: "set",
  setDraftMode: "uniform",
  cubeForm: null,
  setPoolJson: null,
  loadingPool: false,
  configError: null,
  hostRecoveryFailure: null,
  allowedPodSizes: null,
  procedureCacheKey: null,
  pendingProcedureDefault: null,
  packDistribution: null,
  allowedSetLayouts: null,
  packsPerPlayer: null,
  cubeMinDeckSize: null,
  postDraftPlay: null,
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
  orchestrationGeneration: number;
  signal: AbortSignal | undefined;
  controller: AbortController;
  promise: Promise<HostedPodResumeOutcome>;
}

let resumeHostedPodAttempt: HostedPodResumeAttempt | null = null;

// Every procedure read competes to publish the same cache. A kind comparison
// alone rejects cross-kind replies, but cannot distinguish two reads for the
// same kind (for example, setup entry followed by a refresh). Keep one
// monotonically increasing identity so only the newest read may publish.
let podOrchestrationGeneration = 0;

/**
 * A recovery probe has its own ownership, but retiring it must not always
 * supersede the shared setup procedure. In particular, an absent persistence
 * locator is a page-entry fallback, not a new host-setup operation.
 */
function retireHostedPodResumeAttempt(): void {
  resumeHostedPodAttempt?.controller.abort();
  resumeHostedPodAttempt = null;
}

function beginPodOrchestration(set?: (partial: Partial<DraftPodState>) => void): number {
  podOrchestrationGeneration += 1;
  // A newer public operation retires any pool spinner and deduplicated resume
  // attempt owned by the older generation before its continuation can settle.
  retireHostedPodResumeAttempt();
  // A visible host failure belongs only to the attempt that published it.
  // Every new entry, retry, reset, or configuration replacement retires the
  // old UI state before publishing its own route.
  set?.({ loadingPool: false, hostRecoveryFailure: null });
  return podOrchestrationGeneration;
}

function isCurrentPodOrchestration(generation: number): boolean {
  return generation === podOrchestrationGeneration;
}

function sameActivePodCapture(
  left: ActiveDraftPodMetaCapture,
  right: ActiveDraftPodMetaCapture,
): boolean {
  return left.id === right.id && left.roomCode === right.roomCode && left.updatedAt === right.updatedAt;
}

export function isHostedPodRecoveryFailureCurrent(
  failure: HostedPodRecoveryFailure | null,
  routeToken: number,
): failure is HostedPodRecoveryFailure {
  if (!failure || failure.routeToken !== routeToken) return false;
  if (!isCurrentPodOrchestration(failure.orchestrationGeneration)) return false;
  const active = inspectActiveDraftPod();
  return active.type === "present" && sameActivePodCapture(active.capture, failure.capture);
}

/** Whether this tab holds a pod session that an entry or an exit must leave running. */
export function isDraftPodSessionHeld(
  session: { role: DraftRole | null; phase: MultiplayerDraftPhase },
): boolean {
  return session.role !== null && session.phase !== "idle" && session.phase !== "error";
}

/**
 * Whether a hosted recovery attempt can still commit: one is registered,
 * neither its own controller nor its route signal is aborted, and its
 * orchestration is still current. An attempt that a route cleanup aborted
 * stays registered until it settles, but it no longer counts.
 */
export function isHostedPodRecoveryInFlight(): boolean {
  const attempt = resumeHostedPodAttempt;
  return attempt !== null
    && !attempt.controller.signal.aborted
    && !attempt.signal?.aborted
    && isCurrentPodOrchestration(attempt.orchestrationGeneration);
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
  | "allowedPodSizes"
  | "procedureCacheKey"
  | "packDistribution"
  | "allowedSetLayouts"
  | "packsPerPlayer"
  | "cubeMinDeckSize"
  | "postDraftPlay"
> {
  return {
    allowedPodSizes: procedure.allowed_pod_sizes,
    procedureCacheKey,
    packDistribution: procedure.distribution,
    allowedSetLayouts: procedure.allowed_set_layouts,
    packsPerPlayer: procedure.packs_per_player,
    cubeMinDeckSize: procedure.cube_min_deck_size,
    postDraftPlay: procedure.post_draft_play,
  };
}

/**
 * The set-to-booster mapping a distribution can actually express.
 *
 * A `Chaos` pod draws each `(seat, round)` booster from its own set and keeps
 * the draw private. `SharedStackPiles` shuffles every booster into one stack
 * before the first decision, so no seat holds the packs generated for it and
 * nothing distinguishes the result from a mixed pool — except that the players
 * cannot see which sets they are drafting. The engine refuses the pair
 * (`DraftProcedure::validate_source`); this keeps the setup page from offering
 * a choice that refusal would reject, and is the same dispatch on the same
 * engine-published discriminant the Cube tab already uses for `AllAtOnce`.
 */
/** Which published layout shape each setup-page mode asks the engine for. */
const SET_LAYOUT_KIND_BY_MODE: Record<SetDraftMode, SetLayoutKind> = {
  uniform: "UniformByRound",
  chaos: "Chaos",
};

function setDraftModeFor(
  allowedSetLayouts: SetLayoutKind[] | null,
  requested: SetDraftMode,
): SetDraftMode {
  // NOT PUBLISHED YET IS NOT PERMISSION. This used to keep the request until the
  // contract arrived, on the reasoning that the engine refuses at `StartDraft`
  // anyway -- but "the engine will refuse later" is not a reason to offer a
  // choice now, and a stale Chaos selection surviving a kind change is exactly
  // how a host reaches a control the engine cannot honour. Absent contract means
  // the only layout every distribution admits.
  // NULLISH, not `=== null`: a payload that omits the field entirely arrives as
  // `undefined`, and `undefined.includes` is a TypeError rather than a refusal.
  if (allowedSetLayouts == null) return "uniform";
  return allowedSetLayouts.includes(SET_LAYOUT_KIND_BY_MODE[requested]) ? requested : "uniform";
}

function procedurePublication(
  prev: DraftPodState,
  procedure: DraftProcedure,
  target: ProcedureCacheKey,
  adoptsProcedureDefault: boolean,
): Partial<DraftPodState> {
  const podSize = adoptsProcedureDefault
    ? procedure.pod_size
    : procedure.allowed_pod_sizes.includes(prev.config.podSize)
      ? prev.config.podSize
      : procedure.allowed_pod_sizes[0];

  return {
    ...procedureCache(procedure, target),
    config: podSize === prev.config.podSize
      ? prev.config
      : { ...prev.config, podSize },
    pendingProcedureDefault: adoptsProcedureDefault ? null : prev.pendingProcedureDefault,
    poolMode: procedure.distribution === "AllAtOnce" ? "set" : prev.poolMode,
    setDraftMode: setDraftModeFor(procedure.allowed_set_layouts, prev.setDraftMode),
    loadingPool: false,
    configError: null,
  };
}

/**
 * A recovery that ends with no host and no failure owes the setup screen the
 * procedure its refresh skipped or lost to that recovery. This starts no new pod
 * operation, so any newer one wins, and it keeps the message the recovery left.
 */
async function publishProcedureAfterRecovery(
  set: (update: (prev: DraftPodState) => Partial<DraftPodState>) => void,
  get: () => DraftPodState,
  orchestrationGeneration: number,
): Promise<void> {
  if (getEffectiveOffline()) return;
  const { kind, tournamentFormat } = get().config;
  const target: ProcedureCacheKey = { kind, tournamentFormat };
  const isStale = () => !isCurrentPodOrchestration(orchestrationGeneration)
    || !procedureTargetMatchesConfig(target, get().config)
    || getEffectiveOffline();
  try {
    const procedure = await loadProcedure(target.kind, target.tournamentFormat);
    if (isStale()) return;
    set((prev) => {
      const adoptsProcedureDefault = prev.pendingProcedureDefault?.kind === target.kind
        && prev.pendingProcedureDefault?.tournamentFormat === target.tournamentFormat;
      return { ...procedurePublication(prev, procedure, target, adoptsProcedureDefault), configError: prev.configError };
    });
  } catch (err) {
    if (isStale()) return;
    set((prev) => ({ configError: prev.configError ?? (err instanceof Error ? err.message : String(err)) }));
  }
}

type HostPodOutcome = DraftSessionOpenOutcome | { readonly status: "refused" };

/**
 * Host `hostConfig`'s pod. From the moment an opened broker client is
 * available until `hostDraft` resolves `"opened"`, this function owns it and
 * closes it on every other outcome — `hostDraft` can settle otherwise before
 * its own `initialize` runs (a superseding pod, or going offline).
 */
async function hostPod(
  set: (partial: Partial<DraftPodState>) => void,
  get: () => DraftPodState,
  procedureRequest: number,
  config: PodConfig,
  listing: PodListing,
  poolInput: ListablePoolInput,
  hostConfig: DraftPodHostConfig,
): Promise<HostPodOutcome> {
  const stale = () => !isCurrentPodOrchestration(procedureRequest) || get().config !== config;

  if (!listing.isPublic || !podListingEligible(hostConfig.podSize)) {
    if (getEffectiveOffline()) {
      set({ configError: DRAFT_OFFLINE_ERROR });
      return { status: "refused" };
    }
    return useMultiplayerDraftStore.getState().hostDraft(hostConfig);
  }

  const request = podListingRequest(poolInput, hostConfig, listing);
  const refusal = podListingRefusal(request);
  if (refusal !== null) {
    set({ configError: refusal });
    return { status: "refused" };
  }

  const { url, socket } = await useMultiplayerStore
    .getState()
    .resolveP2PBroker(useMultiplayerStore.getState().hostingServer);
  if (stale()) return { status: "refused" };
  if (socket?.serverInfo.mode !== "LobbyOnly") {
    set({ configError: i18n.t("draft:podSetup.lobbyUnavailable") });
    return { status: "refused" };
  }

  let broker: BrokerClient;
  try {
    broker = await openBrokerClient(url);
  } catch {
    if (!stale()) set({ configError: i18n.t("draft:podSetup.lobbyUnavailable") });
    return { status: "refused" };
  }

  let outcome: HostPodOutcome = { status: "refused" };
  try {
    if (stale()) return { status: "refused" };
    if (getEffectiveOffline()) {
      set({ configError: DRAFT_OFFLINE_ERROR });
      return { status: "refused" };
    }
    outcome = await useMultiplayerDraftStore.getState().hostDraft({
      ...hostConfig,
      listing: { broker, request },
    });
    return outcome;
  } finally {
    if (outcome.status !== "opened") broker.close();
  }
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
        if (procedureChanged) beginPodOrchestration();
        const packDistribution = procedureChanged ? null : prev.packDistribution;

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
          allowedSetLayouts: procedureChanged ? null : prev.allowedSetLayouts,
          packsPerPlayer: procedureChanged ? null : prev.packsPerPlayer,
          cubeMinDeckSize: procedureChanged ? null : prev.cubeMinDeckSize,
          postDraftPlay: kindChanged ? null : prev.postDraftPlay,
          loadingPool: false,
          configError: null,
        };
      });
    },

    enterKind: async (kind) => {
      if (getEffectiveOffline()) {
        beginPodOrchestration(set);
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
      // Apply the kind first: it is the entry point's whole purpose and must not
      // depend on the wasm load succeeding. `setConfig` is the single authority for
      // the Sealed pool-mode rule.
      get().setConfig({ kind });
      const procedureRequest = beginPodOrchestration(set);
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
        set((prev) => procedurePublication(prev, procedure, target, true));
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
        beginPodOrchestration(set);
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
        beginPodOrchestration(set);
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
        set((prev) => {
          const adoptsProcedureDefault = prev.pendingProcedureDefault?.kind === target.kind
            && prev.pendingProcedureDefault?.tournamentFormat === target.tournamentFormat;
          return procedurePublication(prev, procedure, target, adoptsProcedureDefault);
        });
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

    setListing: (partial) => {
      set((prev) => ({ listing: { ...prev.listing, ...partial } }));
    },

    setGuestDisplayName: (name) => {
      set({ guestDisplayName: name });
    },

    adoptSavedDisplayName: () => {
      // `typeof` rather than a bare `.trim()`, even though the field is typed
      // `string`: `multiplayerStore`'s persist `merge` normalizes five of its
      // siblings under "Persisted state is external input" but spreads
      // `displayName` through unvalidated, so a corrupt localStorage blob can
      // hydrate a non-string. `PodSetup` calls this from a mount effect, where
      // a throw takes the whole Draft Pod route down over a cosmetic prefill.
      const persisted: unknown = useMultiplayerStore.getState().displayName;
      const saved = typeof persisted === "string" ? persisted.trim() : "";
      if (!saved) return;
      set((prev) => ({
        hostDisplayName: prev.hostDisplayName || saved,
        guestDisplayName: prev.guestDisplayName || saved,
      }));
    },

    adoptRememberedListing: () => {
      const remembered = useMultiplayerStore.getState().lastPodListingPublic;
      set((prev) => ({
        listing: { ...prev.listing, isPublic: remembered ?? true },
      }));
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
      set((prev) => ({
        setDraftMode: setDraftModeFor(prev.allowedSetLayouts, setDraftMode),
        configError: null,
      }));
    },

    setCubeForm: (form) => {
      set({ cubeForm: form, configError: null });
    },

    createPod: async () => {
      if (getEffectiveOffline()) {
        beginPodOrchestration(set);
        set({ configError: DRAFT_OFFLINE_ERROR });
        return;
      }
      let { config, poolMode, setDraftMode } = get();
      const { hostDisplayName, cubeForm, listing } = get();

      if (!hostDisplayName.trim()) {
        set({ configError: "Enter a display name" });
        return;
      }

      // The host's toggle is remembered, not whether the pod was listed, so a
      // choice held off by the seat ceiling is kept.
      useMultiplayerStore.getState().rememberPodListingPublic(listing.isPublic);

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
        set((prev) => procedurePublication(prev, procedure, target, false));
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
          set({ configError: DRAFT_OFFLINE_ERROR, loadingPool: false });
          return;
        }
        set({
          configError: err instanceof Error ? err.message : String(err),
          loadingPool: false,
        });
        return;
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
          const poolInput: ListablePoolInput = setDraftMode === "chaos"
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

          const outcome = await hostPod(set, get, procedureRequest, config, listing, poolInput, hostConfig);
          if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
          if (outcome.status !== "failed") return;
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR });
            return;
          }
          set({ configError: outcome.error ?? "Unable to host draft pod" });
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
        const poolInput: ListablePoolInput = {
          type: "Cube",
          data: {
            cube_list_text: cubeForm.cubeListText,
            cube_name: cubeForm.cubeName,
            cube_draft_settings: cubeForm.settings,
          },
        };
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

        const outcome = await hostPod(set, get, procedureRequest, config, listing, poolInput, hostConfig);
        if (!isCurrentPodOrchestration(procedureRequest) || get().config !== config) return;
        if (outcome.status !== "failed") return;
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return;
        }
        set({ configError: outcome.error ?? "Unable to host draft pod" });
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
      const routeToken = options.routeToken ?? 0;
      const expected = options.expectedFailure;
      if (expected) {
        const visible = get().hostRecoveryFailure;
        const expectedStillVisible = visible !== null
          && visible.kind === expected.kind
          && visible.routeToken === expected.routeToken
          && visible.orchestrationGeneration === expected.orchestrationGeneration
          && sameActivePodCapture(visible.capture, expected.capture);
        if (!expectedStillVisible || !isHostedPodRecoveryFailureCurrent(expected, expected.routeToken)) {
          return "superseded";
        }
      }
      // An offline entry is a fresh public operation, not a request to join a
      // held recovery probe. Retire that probe before reporting offline so it
      // cannot later publish against this route.
      if (getEffectiveOffline()) {
        beginPodOrchestration(set);
        set({ configError: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      if (
        resumeHostedPodAttempt
        && resumeHostedPodAttempt.routeToken === routeToken
        && resumeHostedPodAttempt.signal === options.signal
      ) {
        return resumeHostedPodAttempt.promise;
      }

      // Automatic entry carries no request to switch pods, so it never
      // replaces a session this tab holds, whatever its role or room. It must
      // run before the saved pod is read: an `"absent"` outcome sends the page
      // on to `resumeDraft`, which can replace a held host.
      if (options.entry === "auto") {
        if (isDraftPodSessionHeld(useMultiplayerDraftStore.getState())) {
          // This entry asks for no recovery, so it retires any failure an
          // earlier route left: route tokens restart with each page mount.
          set({ hostRecoveryFailure: null });
          return "superseded";
        }
      }

      // A missing (or invalid) locator is an established page-entry fallback,
      // not a replacement host setup. Retire only recovery-owned state here:
      // advancing the shared procedure generation would discard an ordinary
      // host setup that is already loading its engine procedure.
      const active = inspectActiveDraftPod();
      if (active.type === "absent" || active.type === "invalid") {
        retireHostedPodResumeAttempt();
        set({ hostRecoveryFailure: null });
        if (active.type === "invalid" && active.capture) {
          clearActiveDraftPodIfCurrent(active.capture);
        }
        if (!options.silent) {
          set({ configError: active.type === "absent" ? "No draft pod to resume" : "Saved draft pod is invalid" });
        }
        return active.type === "absent" ? "absent" : "invalid";
      }

      // Do this before any probe so a new host entry/retry cannot leave an
      // old failure on top of the replacement route while storage is pending.
      const orchestrationGeneration = beginPodOrchestration(set);

      const attempt: HostedPodResumeAttempt = {
        routeToken,
        orchestrationGeneration,
        signal: options.signal,
        controller: new AbortController(),
        promise: Promise.resolve("superseded"),
      };
      const abortAttempt = () => attempt.controller.abort();
      options.signal?.addEventListener("abort", abortAttempt, { once: true });
      const isCurrentAttempt = () =>
        resumeHostedPodAttempt === attempt
        && !options.signal?.aborted
        && !attempt.controller.signal.aborted
        && isCurrentPodOrchestration(orchestrationGeneration);
      const failure = (
        kind: HostedPodRecoveryFailure["kind"],
        capture: ActiveDraftPodMetaCapture,
        details: Pick<HostedPodRecoveryFailure, "raw" | "error"> = {},
      ): HostedPodRecoveryFailure => ({
        kind,
        capture,
        routeToken,
        orchestrationGeneration,
        ...details,
      });

      // An async IIFE executes through its first await immediately. Register
      // the attempt first so its initial ownership check sees this exact
      // attempt rather than treating every fresh probe as superseded.
      resumeHostedPodAttempt = attempt;
      attempt.promise = (async (): Promise<HostedPodResumeOutcome> => {
        if (!isCurrentAttempt()) return "superseded";
        const { meta, capture } = active;
        // Until the lifecycle store's fenced commit, the saved pod record must
        // still be the exact record this recovery read. The committed host
        // then re-saves that record with a new update time, so after the
        // commit only its pod identity is compared: another pod's record, or
        // none, still supersedes. A later failure is recorded against the
        // record this check last accepted.
        let committed = false;
        let validatedCapture = capture;
        const captureIsCurrent = () => {
          const current = inspectActiveDraftPod();
          if (current.type !== "present") return false;
          if (!committed) return sameActivePodCapture(current.capture, capture);
          if (current.capture.id !== capture.id) return false;
          validatedCapture = current.capture;
          return true;
        };
        const isCurrentCapture = () => isCurrentAttempt() && captureIsCurrent();

        const activeDraft = useMultiplayerDraftStore.getState();
        if (
          activeDraft.role === "host"
          && activeDraft.phase !== "idle"
          && activeDraft.phase !== "error"
          && activeDraft.hostPersistenceId === meta.id
        ) {
          return "resumed";
        }

        let loaded: DraftHostSessionLoadResult;
        try {
          loaded = await loadDraftHostSession(meta.id);
        } catch (error) {
          loaded = { type: "readFailure", error };
        }
        if (!isCurrentCapture()) return "superseded";
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return "offline";
        }

        if (loaded.type === "readFailure") {
          set({
            hostRecoveryFailure: failure("readFailure", capture, { error: loaded.error }),
            configError: loaded.error instanceof Error ? loaded.error.message : String(loaded.error),
          });
          return "readFailure";
        }
        if (loaded.type === "absent") {
          clearActiveDraftPodIfCurrent(capture);
          if (!options.silent) set({ configError: "Saved draft pod was not found" });
          return "absent";
        }
        if (loaded.type === "invalid" || loaded.type === "terminal") {
          const terminal = loaded.type === "terminal";
          clearActiveDraftPodIfCurrent(capture);
          if (!options.silent) set({ configError: terminal ? "Saved draft pod is complete" : "Saved draft pod is invalid" });
          return terminal ? "terminal" : "invalid";
        }

        const persisted = loaded.session;
        if (persisted.persistenceId !== meta.id || persisted.roomCode !== meta.roomCode) {
          clearActiveDraftPodIfCurrent(capture);
          if (!options.silent) set({ configError: "Saved draft pod is invalid" });
          return "invalid";
        }

        const recovery = { session: persisted, raw: loaded.raw };
        let preflight: Awaited<ReturnType<typeof preflightDraftPodHostRecovery>>;
        try {
          preflight = await preflightDraftPodHostRecovery(recovery);
        } catch (error) {
          // A check that cannot answer admits nothing: the saved pod stays
          // the recovery source, reported as a connection failure.
          if (!isCurrentCapture()) return "superseded";
          set({ hostRecoveryFailure: failure("initializationFailure", capture, { error }), configError: "podError.connection" });
          return "initializationFailure";
        }
        if (preflight.type === "incompatible") {
          if (!isCurrentCapture()) return "superseded";
          set({ hostRecoveryFailure: failure("incompatible", capture, { raw: loaded.raw }), configError: "podError.incompatible" });
          return "incompatible";
        }

        // Build, but do not publish, the recovered pod state: it is published
        // only once the lifecycle store reports B resumed. A live A stays
        // authoritative until the lifecycle store's fenced commit, which comes
        // before B restores a saved draft session or, when the record carries
        // none, after B has initialized.
        const recoveredPodState: Partial<DraftPodState> = persisted.poolInput.type === "Cube"
          ? {
              config: {
                packs: [], setCode: CUSTOM_CUBE_SET_CODE, setName: persisted.poolInput.data.cube_name,
                kind: persisted.kind, podSize: persisted.podSize,
                tournamentFormat: persisted.tournamentFormat, podPolicy: persisted.podPolicy,
              },
              hostDisplayName: persisted.hostDisplayName,
              poolMode: "cube",
              setDraftMode: "uniform",
              cubeForm: {
                cubeName: persisted.poolInput.data.cube_name,
                cubeListText: persisted.poolInput.data.cube_list_text,
                settings: persisted.poolInput.data.cube_draft_settings,
              },
              setPoolJson: null,
            }
          : (() => {
              const packs = persistedPodPacks(persisted.poolInput);
              return {
                config: {
                  packs,
                  setCode: distinctJoined(packs.map((pack) => pack.code), "+"),
                  setName: packs.length > 0 ? distinctJoined(packs.map((pack) => pack.name), " · ") : "Draft Pod",
                  kind: persisted.kind, podSize: persisted.podSize,
                  tournamentFormat: persisted.tournamentFormat, podPolicy: persisted.podPolicy,
                },
                hostDisplayName: persisted.hostDisplayName,
                poolMode: "set" as const,
                setDraftMode: persisted.poolInput.type === "Chaos" ? "chaos" as const : "uniform" as const,
                cubeForm: null,
                setPoolJson: JSON.stringify(persisted.poolInput.data),
              };
            })();
        const target: ProcedureCacheKey = { kind: persisted.kind, tournamentFormat: persisted.tournamentFormat };
        let procedure: DraftProcedure;
        try {
          procedure = await loadProcedure(target.kind, target.tournamentFormat);
        } catch (error) {
          if (!isCurrentCapture()) return "superseded";
          if (getEffectiveOffline()) {
            set({ configError: DRAFT_OFFLINE_ERROR });
            return "offline";
          }
          set({
            hostRecoveryFailure: failure("initializationFailure", capture, { error }),
            configError: error instanceof Error ? error.message : String(error),
          });
          return "initializationFailure";
        }
        if (!isCurrentCapture()) return "superseded";
        if (getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return "offline";
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
          backupEndpoint: configuredBackupEndpoint(),
          recovery,
        };
        const isCurrentRecovery = () =>
          isCurrentCapture()
          && hostConfig.persistenceId === capture.id
          && hostConfig.preferredRoomCode === capture.roomCode;
        const hosted: HostedDraftRecoveryOutcome = await useMultiplayerDraftStore.getState().resumeHostedDraft({
          config: { ...hostConfig, signal: attempt.controller.signal },
          isCurrent: isCurrentRecovery,
          routeSignal: options.signal,
          onCommitted: () => {
            committed = true;
          },
        });
        // After the lifecycle store's commit the committed host re-saves this
        // locator, so any outcome but resumed compares only its pod identity.
        // Successful installation republishes this locator with its new update
        // time. The lifecycle validated the original capture at commit; only
        // non-installed outcomes still compare that pre-install timestamp.
        if (!isCurrentAttempt() || (hosted !== "resumed" && !captureIsCurrent())) return "superseded";
        if (hosted === "offline" || getEffectiveOffline()) {
          set({ configError: DRAFT_OFFLINE_ERROR });
          return "offline";
        }
        if (hosted === "aborted" || hosted === "superseded") return "superseded";
        if (hosted === "incompatible" || hosted === "readFailure" || hosted === "initializationFailure") {
          const kind = hosted === "incompatible" ? "incompatible" : hosted === "readFailure" ? "readFailure" : "initializationFailure";
          set({
            hostRecoveryFailure: failure(kind, validatedCapture, hosted === "incompatible" ? { raw: loaded.raw } : {}),
            configError: hosted === "incompatible" ? "podError.incompatible" : "podError.connection",
          });
          return hosted;
        }
        if (hosted !== "resumed") return "invalid";

        // The lifecycle commit succeeded. Only now does B's recovered
        // configuration become visible in this store/page.
        set((previous) => {
          const staged = { ...previous, ...recoveredPodState, config: recoveredPodState.config! } as DraftPodState;
          return {
            ...recoveredPodState,
            ...procedurePublication(staged, procedure, target, false),
            pendingProcedureDefault: null,
            hostRecoveryFailure: null,
            configError: null,
          };
        });
        return "resumed";
      })();

      try {
        const outcome = await attempt.promise;
        if (outcome === "absent" || outcome === "invalid" || outcome === "terminal") {
          void publishProcedureAfterRecovery(set, get, orchestrationGeneration);
        }
        return outcome;
      } finally {
        options.signal?.removeEventListener("abort", abortAttempt);
        if (resumeHostedPodAttempt === attempt) resumeHostedPodAttempt = null;
      }
    },

    retireHostedPodRecoveryFailure: () => {
      retireHostedPodResumeAttempt();
      set({ hostRecoveryFailure: null });
    },

    joinPod: async () => {
      if (getEffectiveOffline()) {
        beginPodOrchestration(set);
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
        if (joined.status === "failed" && getEffectiveOffline()) {
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
        beginPodOrchestration(set);
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
