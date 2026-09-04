/**
 * Zustand store for multiplayer P2P draft state.
 *
 * Separate from `draftStore` (Quick Draft, single-player) because the
 * multiplayer draft lifecycle is fundamentally different:
 * - Host/guest asymmetry (host runs WASM, guest is stateless receiver)
 * - Lobby phase with seat management before draft starts
 * - Network events (disconnect, reconnect, kick, pause/resume)
 * - Pairing and match handoff after deckbuilding
 *
 * The store wraps `DraftPodHostAdapter` or `DraftPodGuestAdapter` and
 * projects their events into reactive Zustand state for the React UI.
 */

import { create, type StateCreator } from "zustand";

import type {
  DraftCardInstance,
  DraftPlayerView,
  PairingView,
  SeatPublicView,
  StandingEntry,
} from "../adapter/draft-adapter";
import type { EngineAdapter, GameAction, GameEvent, GameLogEntry, MatchScore, SubmitResult } from "../adapter/types";
import type { DraftMatchDeckPayload, DraftMatchLaunch, DraftMatchSettlement, DraftPauseReason } from "../network/draftProtocol";
import { MAX_MATERIALIZED_VIRTUAL_BASICS } from "../components/draft/workspace/types";
import type { DraftCardPlacement, DraftWorkspaceState } from "../components/draft/workspace/types";
import {
  appendWorkspaceInstanceToResolvedDestination,
  createDraftWorkspaceState,
  makeInteractiveVirtualBasicInstanceId,
  reconcileWorkspaceState,
  updateWorkspacePlacement,
} from "../components/draft/workspace/workspacePlacement";
import {
  addVirtualBasic,
  countProjectedNames,
  projectWorkspaceLandCounts,
  projectWorkspaceMainDeck,
  projectWorkspacePartition,
  removeVirtualBasic,
  type DraftWorkspacePartition,
} from "../components/draft/workspace/workspaceProjection";
import type { AISeatBinding } from "../game/controllers/aiController";
import { createGameLoopController, type GameLoopController } from "../game/controllers/gameLoopController";
import { processRemoteUpdate } from "../game/dispatch";
import { debugLog } from "../game/debugLog";
import { reportStructuredActionRejection } from "../game/actionRejectionReporter";
import { resyncFromAdapterSafely } from "../game/staleStateWatchdog";
import { DRAFT_DECK_SESSION_KEY } from "./draftStore";
import { useGameStore } from "./gameStore";
import {
  DraftPodHostAdapter,
  type DraftPodHostConfig,
  type DraftPodHostEvent,
  type DraftPodHostStatus,
} from "../adapter/draftPodHostAdapter";
import {
  DraftPodGuestAdapter,
  type DraftPodGuestConfig,
  type DraftPodGuestEvent,
  type DraftPodGuestStatus,
} from "../adapter/draftPodGuestAdapter";
import type { DraftGuestRecoveryFailure } from "../adapter/p2p-draft-guest";
import {
  clearActiveDraftPod,
  clearActiveDraftGuest,
  clearActiveDraftGuestIfCurrent,
  clearDraftSettlementOutbox,
  loadDraftIntergameCommands,
  loadActiveDraftPod,
  inspectActiveDraftGuest,
  loadDraftGuestSession,
  loadDraftSettlementOutbox,
  saveActiveDraftPod,
  saveDraftIntergameCommands,
  saveDraftSettlementOutbox,
  type ActiveDraftPodMeta,
  type ActiveDraftPodPhase,
} from "../services/draftPersistence";
import { getEffectiveOffline } from "./connectivityStore";
import {
  commandAcknowledgement,
  consumeIntergamePermit,
  draftIntergameDigest,
  IntergameCommandController,
  type DraftIntergameCommand,
  type DraftIntergameCommandAck,
  type DraftIntergameCommandPayload,
} from "../services/intergameCommandLedger";
import type {
  DraftAutoPickPlacementHints,
  DraftPickDestination,
  DraftPickOutcome,
  DraftPickPlacementHint,
  PendingDraftPickIntent,
} from "./draftStore";
import { FORMAT_DEFAULTS } from "./multiplayerStore";

// ── Types ──────────────────────────────────────────────────────────────

export type DraftRole = "host" | "guest";

export const DRAFT_OFFLINE_ERROR = "offline.startUnavailable";

export type GuestDraftResumeOutcome = "resumed" | "absent" | "invalid" | "failed" | "offline" | "superseded";

/**
 * The pod SESSION's phase.
 *
 * Every member is the projection of an engine or adapter session status — the
 * union of the ranges of `phaseForDraftViewStatus`, `hostStatusToPhase` and
 * `guestStatusToPhase`. Nothing else belongs here.
 *
 * The Bo3 intergame window is deliberately NOT a member: the pod session is
 * `MatchInProgress` for the whole match, individual games included. See
 * {@link DraftPodScreen} and {@link draftPodScreen}.
 */
export type MultiplayerDraftPhase =
  | "idle"
  | "connecting"
  | "lobby"
  | "drafting"
  | "deckbuilding"
  | "pairing"
  | "matchInProgress"
  | "roundComplete"
  | "complete"
  | "error"
  | "kicked"
  | "hostLeft";

/** True only while a connected pod role has a live draft-session phase. */
export function isMultiplayerDraftPodLive(
  state: { role: DraftRole | null; phase: MultiplayerDraftPhase },
): boolean {
  return state.role !== null && (
    state.phase === "connecting"
    || state.phase === "lobby"
    || state.phase === "drafting"
    || state.phase === "deckbuilding"
    || state.phase === "pairing"
    || state.phase === "matchInProgress"
    || state.phase === "roundComplete"
  );
}

/**
 * The screen the pod UI shows.
 *
 * Every member but `betweenGames` is a `MultiplayerDraftPhase` — the projection
 * of the pod session's engine/adapter status. `betweenGames` is not a sibling of
 * `matchInProgress`: the pod session is `MatchInProgress` for the whole match,
 * individual games included, so the Bo3 intergame window is a refinement *within*
 * that phase, not an alternative to it. Keeping it out of `MultiplayerDraftPhase`
 * is what stops the five status writers (`statusChanged`, `viewUpdated`,
 * `draftStarted` on the host; `statusChanged`, `viewUpdated` on the guest) from
 * overwriting it — the clobber is no longer a rule to remember, it is a value
 * `tsc` refuses to accept.
 */
export type DraftPodScreen = MultiplayerDraftPhase | "betweenGames";

/**
 * Single authority for which pod screen the store's state calls for.
 *
 * The overlay is called for exactly while BOTH hold:
 *   1. the pod session is still in a match (`phase === "matchInProgress"`), and
 *   2. an intergame prompt is live (`sideboardPrompt !== null`).
 *
 * Conjunct 1 is released by every writer that moves the pod session on — round
 * boundary, tournament end, `Abandoned`, adapter error, `kicked`, `hostLeft`,
 * `leave`, `reset`. Conjunct 2 is released by the four writers that end the
 * window: `bo3GameStarted`, host `bo3GameStart`, guest `bo3GameStart`, and
 * `disposeMatchAdapter`.
 *
 * Conjunct 2 is an inference from a proxy, and the proxy's lifecycle has a hole:
 * nothing closes the window when the pod host's intergame orchestration
 * deadlocks. Do NOT "fix" that by latching a flag — a latch is what the phase
 * member was and what stranded the user. The deadlock's release is the viewer's,
 * via `DraftPodPage`'s local overlay dismissal, which suppresses the *rendering*
 * without touching this state.
 *
 * Keyed on `sideboardPrompt` alone, not `sideboardPrompt || playDrawPrompt`,
 * because `playDrawPrompt` is strictly nested inside it: the writers that set it
 * (`bo3ChoosePlayDraw`, host and guest) leave `sideboardPrompt` untouched, all
 * three writers that set `sideboardPrompt` null `playDrawPrompt`, and every
 * writer that clears one clears both. A `playDrawPrompt !== null` disjunct would
 * be unreachable, so no test could discriminate it. The nesting is pinned
 * directly instead — see the store tests.
 */
export function draftPodScreen(
  state: Pick<MultiplayerDraftState, "phase" | "sideboardPrompt">,
): DraftPodScreen {
  return state.phase === "matchInProgress" && state.sideboardPrompt !== null
    ? "betweenGames"
    : state.phase;
}

/**
 * Stable identity of the live intergame prompt, or `null` when none is live.
 *
 * Exists so a viewer's decision to hide the overlay can be scoped to the prompt
 * it was made about. Returning a primitive keeps it usable as a zustand selector
 * without `useShallow`.
 *
 * The third component is not redundant. One intergame window delivers TWO
 * prompts, and they carry the same `matchId` and the same `gameNumber`:
 * `transitionToPlayDraw` (`p2p-draft-host.ts`) sends the play/draw prompt with
 * `gameNumber: state.gameNumber`, and `bo3ChoosePlayDraw` sets `playDrawPrompt`
 * while leaving `sideboardPrompt` set. Without the discriminator a dismissal
 * made about sideboarding would carry straight over and suppress the play/draw
 * decision — a different decision, on a 10-second timer that auto-chooses
 * (`startPlayDrawTimer` -> `autoChoosePlayDraw`).
 *
 * Note that `playDrawPrompt`'s nesting inside `sideboardPrompt` has OPPOSITE
 * consequences at the two layers, and both are deliberate: `draftPodScreen`
 * keys on `sideboardPrompt` alone because the nesting means there is only one
 * screen answer, while this key must discriminate the prompt type precisely
 * because the nesting means the two prompts otherwise share an identity.
 */
export function intergamePromptKey(
  state: Pick<MultiplayerDraftState, "sideboardPrompt" | "playDrawPrompt">,
): string | null {
  return state.sideboardPrompt
    ? `${state.sideboardPrompt.matchId}#${state.sideboardPrompt.gameNumber}#${state.playDrawPrompt ? "pd" : "sb"}`
    : null;
}

export interface PairingInfo {
  round: number;
  table: number;
  opponentName: string;
  matchHostPeerId: string;
  matchId: string;
}

interface MultiplayerDraftState {
  role: DraftRole | null;
  phase: MultiplayerDraftPhase;
  roomCode: string | null;
  draftCode: string | null;
  seatIndex: number | null;
  view: DraftPlayerView | null;
  seats: SeatPublicView[];
  joined: number;
  total: number;
  paused: boolean;
  pauseReason: DraftPauseReason | null;
  pairing: PairingInfo | null;
  error: string | null;
  /** Recovery-only failure semantics, retained for an explicit retry CTA. */
  guestRecoveryFailure: DraftGuestRecoveryFailure | null;
  selectedCard: string | null;
  workspaceState: DraftWorkspaceState | null;
  pendingPickIntent: PendingDraftPickIntent | null;
  interactionGeneration: number;
  pickInteractionLocked: boolean;
  workspaceSyncError: string | null;
  mainDeck: string[];
  landCounts: Record<string, number>;
  timerRemainingMs: number | null;
  standings: StandingEntry[];
  currentRound: number;
  nextPairingRound: number;
  pairings: PairingView[];
  /** Full deck submitted during deckbuilding (mainDeck + lands). */
  submittedDeck: string[];
  submittedWorkspaceState: DraftWorkspaceState | null;
  submittedPartition: DraftWorkspacePartition | null;
  intergameWorkspaceState: DraftWorkspaceState | null;
  matchPairing: DraftMatchLaunch | null;
  matchAdapter: unknown | null;
  /** Bo3: sideboard prompt state between games. */
  sideboardPrompt: {
    matchId: string;
    gameNumber: number;
    score: MatchScore;
    loserSeat: number | null;
    timerMs: number;
  } | null;
  /** Bo3: play/draw choice prompt. */
  playDrawPrompt: {
    matchId: string;
    gameNumber: number;
    score: MatchScore;
    timerMs: number;
  } | null;
  /** Bo3: whether this player has submitted their sideboard. */
  sideboardSubmitted: boolean;
}

interface MultiplayerDraftActions {
  /** Dismiss the current phase-scoped error banner. */
  clearError: () => void;
  /** Host: create a new draft pod and start accepting guests. */
  /** `true` only after the current adapter initialized and remains owned. */
  hostDraft: (config: DraftPodHostConfig) => Promise<boolean>;
  /** Guest: join an existing draft pod by room code. */
  joinDraft: (config: DraftPodGuestConfig) => Promise<boolean>;
  /** Reconnect exclusively through the persisted capability, never `draft_join`. */
  resumeDraft: (options?: { routeToken?: number; signal?: AbortSignal }) => Promise<GuestDraftResumeOutcome>;
  /** Host: start the draft once the pod is ready. */
  startDraft: (botFillEmptySeats?: boolean) => Promise<void>;
  /** Both: submit a pick. */
  submitPick: (cardInstanceId: string, destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: submit one complete engine-defined pick step. */
  submitPickStep: (cardInstanceIds: readonly string[], destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: submit a pick using a drafted card's draft-time effect. */
  submitPickWithDraftEffect: (effectCardInstanceId: string, cardInstanceIds: readonly [string, string], destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: select a card (UI highlight before confirming pick). */
  selectCard: (cardInstanceId: string | null) => void;
  /** Both: confirm the currently selected card as pick. */
  confirmPick: (destination?: DraftPickDestination, placementHint?: DraftPickPlacementHint) => Promise<DraftPickOutcome>;
  /** Both: pick a card from the current pack using a deterministic draft heuristic. */
  autoPickCard: (placementHints?: DraftAutoPickPlacementHints) => Promise<DraftPickOutcome>;
  setWorkspaceState: (next: DraftWorkspaceState) => void;
  setWorkspacePlacement: (instanceId: string, placement: DraftCardPlacement) => void;
  addBasicLand: (name: string) => void;
  removeBasicLand: (name: string) => void;
  retryWorkspaceSync: () => Promise<void>;
  setIntergameWorkspaceState: (next: DraftWorkspaceState) => void;
  /** Both: submit the built deck. */
  submitDeck: (commanders?: string[]) => Promise<void>;
  /** Host: kick a player from the pod. */
  kickPlayer: (seat: number, reason?: string) => void;
  /** Host: pause the draft. */
  requestPause: () => void;
  /** Host: resume the draft. */
  requestResume: () => void;
  /** Both: tear down the connection and reset state. Lifecycle callers retain
   * recovery; an explicit leave revokes it only after host acknowledgement. */
  leave: (preserveRecovery?: boolean) => Promise<void>;
  /** Reset store to initial state (without network cleanup). */
  reset: () => void;
  /** Both: start the match for the current pairing. */
  startMatch: () => Promise<string | null>;
  /**
   * CR 903.13a: launch the completed Commander pod's multiplayer game.
   *
   * Stages the N-seat deck blob and navigates; it computes no game state. The
   * seat count comes from `view.seats`, never from the literal 4 — CR 903.13
   * fixes no pod size (CR 800.1 only requires more than two), so the pod's own
   * seat list is the authority.
   */
  launchCommanderGame: (navigate: (path: string) => void) => Promise<void>;
  /** Both: report a match result back to the pod host. */
  reportMatchResult: (matchId: string, winnerSeat: number | null) => Promise<void>;
  /** Both: report the active game result using the current draft match pairing. */
  reportActiveMatchGameResult: (gameWinner: number | null) => Promise<void>;
  /** Settles a bound match concession for the authenticated game seat. */
  reportActiveMatchConcession: (concedingGamePlayer?: number) => Promise<void>;
  /** Host: advance to the next round (Casual mode). */
  advanceRound: () => void;
  /** Host: override a match result (Casual mode). */
  overrideMatchResult: (matchId: string, winnerSeat: number | null) => void;
  /** Host: replace a disconnected player with a bot (Casual mode). */
  replaceSeatWithBot: (seat: number) => void;
  /** Both: submit sideboard between Bo3 games. */
  submitSideboard: (matchId: string, mainDeck: string[], sideboard: Array<{ name: string; count: number }>) => void;
  /** Both: choose play or draw (loser of previous game). */
  choosePlayDraw: (matchId: string, playFirst: boolean) => void;
  /** Both: handle between-games prompt from match adapter. */
  handleBetweenGamesPrompt: (prompt: { matchId: string; gameNumber: number; score: MatchScore; loserSeat: number | null; timerMs: number }) => void;
  /** Durable ingress; raw sideboard/play-draw transport is intentionally not exposed. */
  submitIntergameCommand: (payload: DraftIntergameCommandPayload) => Promise<void>;
  /** Executes only a pod-issued authorization after re-checking its immutable ack. */
  submitAuthorized: (command: DraftIntergameCommand, acknowledgement: DraftIntergameCommandAck) => Promise<void>;
}

// ── Module-level adapter refs ──────────────────────────────────────────

let activeHostAdapter: DraftPodHostAdapter | null = null;
let activeGuestAdapter: DraftPodGuestAdapter | null = null;
let activeHostEventUnsub: (() => void) | null = null;
let activeGuestEventUnsub: (() => void) | null = null;
let activeHostAbort: AbortController | null = null;
let activeGuestAbort: AbortController | null = null;
let activeHostRouteAbortListener: { signal: AbortSignal; listener: () => void } | null = null;
let activeGuestRouteAbortListener: { signal: AbortSignal; listener: () => void } | null = null;
let activeHostPersistenceId: string | null = null;
let draftAdapterEpoch = 0;
let resumeGuestDraftAttempt: {
  routeToken: number;
  signal: AbortSignal | undefined;
  promise: Promise<GuestDraftResumeOutcome>;
} | null = null;
const disposedHostAdapters = new WeakSet<DraftPodHostAdapter>();
const disposedGuestAdapters = new WeakSet<DraftPodGuestAdapter>();
const retainedDraftSessionTeardowns = new Map<string, Promise<void>>();
let activeMatchController: GameLoopController | null = null;
const intergameControllers = new Map<string, IntergameCommandController>();
const DRAFT_MATCH_FORMAT_CONFIG = FORMAT_DEFAULTS.Limited;
let lifecycleGeneration = 0;
let workspaceRevision = 0;
let exclusivePickToken: symbol | null = null;
let restoredWorkspace: { generation: number; state: DraftWorkspaceState | null } | null = null;
let pendingGuestPick: {
  generation: number;
  resolve: (view: DraftPlayerView | null) => void;
} | null = null;

function cloneWorkspace(state: DraftWorkspaceState): DraftWorkspaceState {
  return {
    ...state,
    placements: Object.fromEntries(
      Object.entries(state.placements).map(([instanceId, placement]) => [instanceId, { ...placement }]),
    ),
    virtualBasics: state.virtualBasics.map((basic) => ({ ...basic })),
  };
}

function beginDraftLifecycle(): number {
  lifecycleGeneration += 1;
  workspaceRevision = 0;
  exclusivePickToken = null;
  restoredWorkspace = null;
  pendingGuestPick?.resolve(null);
  pendingGuestPick = null;
  return lifecycleGeneration;
}

function workspaceFacades(workspace: DraftWorkspaceState, view: DraftPlayerView) {
  return {
    mainDeck: projectWorkspaceMainDeck(workspace, view.pool),
    landCounts: projectWorkspaceLandCounts(workspace),
  };
}

function activeWorkspaceAdapter(): DraftPodHostAdapter | DraftPodGuestAdapter | null {
  const role = useMultiplayerDraftStore.getState().role;
  return role === "host" ? activeHostAdapter : role === "guest" ? activeGuestAdapter : null;
}

function publishWorkspace(workspace: DraftWorkspaceState): Promise<void> {
  const adapter = activeWorkspaceAdapter();
  if (!adapter) return Promise.resolve();
  const generation = lifecycleGeneration;
  const revision = ++workspaceRevision;
  return adapter.updateWorkspace(workspace).then(
    () => {
      if (generation === lifecycleGeneration && revision === workspaceRevision
        && activeWorkspaceAdapter() === adapter) {
        useMultiplayerDraftStore.setState({ workspaceSyncError: null });
      }
    },
    (error: unknown) => {
      if (generation === lifecycleGeneration && revision === workspaceRevision
        && activeWorkspaceAdapter() === adapter) {
        useMultiplayerDraftStore.setState({
          workspaceSyncError: error instanceof Error ? error.message : String(error),
        });
      }
    },
  );
}

function installWorkspace(input: {
  view: DraftPlayerView;
  base: DraftWorkspaceState;
  publish: boolean;
  patch?: Partial<MultiplayerDraftState>;
}): DraftWorkspaceState {
  const workspace = reconcileWorkspaceState(input.base, input.view.pool);
  const current = useMultiplayerDraftStore.getState();
  const next = {
    ...input.patch,
    ...workspaceFacades(workspace, input.view),
    view: input.view,
    workspaceState: workspace,
  };
  useMultiplayerDraftStore.setState(
    next.phase !== undefined && next.phase !== current.phase
      ? { error: null, ...next }
      : next,
  );
  if (input.publish) void publishWorkspace(workspace);
  return workspace;
}

function applyDestination(
  workspace: DraftWorkspaceState,
  pool: DraftPlayerView["pool"],
  instanceIds: readonly string[],
  destination: DraftPickDestination,
  placementHint?: DraftPickPlacementHint,
): DraftWorkspaceState {
  let next = workspace;
  for (const instanceId of instanceIds) {
    const placement = next.placements[instanceId];
    if (!placement) continue;
    next = appendWorkspaceInstanceToResolvedDestination(next, pool, instanceId, {
      zone: destination,
      column: placementHint?.column ?? placement.column,
      row: placementHint?.row ?? placement.row,
    });
  }
  return next;
}

function poolMultiplicity(pool: DraftPlayerView["pool"]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const card of pool) counts.set(card.instance_id, (counts.get(card.instance_id) ?? 0) + 1);
  return counts;
}

function exactAddedIds(
  before: ReadonlyMap<string, number>,
  after: ReadonlyMap<string, number>,
): string[] | null {
  const added: string[] = [];
  for (const instanceId of new Set([...before.keys(), ...after.keys()])) {
    const previous = before.get(instanceId) ?? 0;
    const current = after.get(instanceId) ?? 0;
    if (current === previous) continue;
    if (previous !== 0 || current !== 1) return null;
    added.push(instanceId);
  }
  return added;
}

type MultiplayerPickRequest =
  | { kind: "pick"; instanceIds: readonly string[]; destination: DraftPickDestination; placementHint?: DraftPickPlacementHint }
  | { kind: "draft-effect"; effectCardInstanceId: string; instanceIds: readonly [string, string]; destination: DraftPickDestination; placementHint?: DraftPickPlacementHint }
  | {
      kind: "auto-pick";
      instanceIds: readonly string[];
      destination: "deck";
      placementHints?: DraftAutoPickPlacementHints;
    };

function intentForPick(request: MultiplayerPickRequest): PendingDraftPickIntent {
  switch (request.kind) {
    case "pick":
      return { kind: "pick", instanceIds: request.instanceIds, destination: request.destination, placementHint: request.placementHint };
    case "draft-effect":
      return { kind: "draft-effect", instanceIds: request.instanceIds, destination: request.destination, placementHint: request.placementHint };
    case "auto-pick":
      return { kind: "auto-pick", destination: "deck" };
  }
}

async function performPick(request: MultiplayerPickRequest): Promise<DraftPickOutcome> {
  if (exclusivePickToken) return { status: "ignored", reason: "busy" };
  if (request.kind === "pick" && (request.instanceIds.length === 0 || request.instanceIds.some((instanceId) => instanceId.length === 0))) {
    return { status: "rejected", reason: "invalid-request" };
  }
  if (request.kind === "auto-pick" && request.instanceIds.length === 0) {
    return { status: "rejected", reason: "invalid-request" };
  }
  if (request.kind === "draft-effect" && (request.instanceIds[0] === request.instanceIds[1]
    || request.instanceIds.some((instanceId) => instanceId.length === 0))) {
    return { status: "rejected", reason: "invalid-request" };
  }
  const state = useMultiplayerDraftStore.getState();
  const adapter = activeWorkspaceAdapter();
  if (!adapter || !state.view || !state.workspaceState) return { status: "rejected", reason: "invalid-request" };

  const token = Symbol("pick");
  exclusivePickToken = token;
  const generation = lifecycleGeneration;
  const before = poolMultiplicity(state.view.pool);
  const intent = intentForPick(request);
  useMultiplayerDraftStore.setState({ pendingPickIntent: intent, pickInteractionLocked: true });
  const isFresh = () => generation === lifecycleGeneration
    && exclusivePickToken === token
    && activeWorkspaceAdapter() === adapter;
  const cleanup = () => {
    if (exclusivePickToken !== token) return;
    exclusivePickToken = null;
    pendingGuestPick = null;
    useMultiplayerDraftStore.setState({ pendingPickIntent: null, pickInteractionLocked: false });
  };

  try {
    let acknowledgedView: DraftPlayerView | null;
    if (state.role === "host" && activeHostAdapter === adapter) {
      acknowledgedView = request.kind === "draft-effect"
        ? await adapter.submitPickWithDraftEffect(request.effectCardInstanceId, [...request.instanceIds])
        : await adapter.submitPick([...request.instanceIds]);
    } else if (state.role === "guest" && activeGuestAdapter === adapter) {
      const acknowledgement = new Promise<DraftPlayerView | null>((resolve) => {
        pendingGuestPick = { generation, resolve };
      });
      if (request.kind === "draft-effect") {
        await adapter.submitPickWithDraftEffect(request.effectCardInstanceId, [...request.instanceIds]);
      } else {
        await adapter.submitPick([...request.instanceIds]);
      }
      acknowledgedView = await acknowledgement;
    } else {
      acknowledgedView = null;
    }
    if (!isFresh()) return { status: "ignored", reason: "stale" };
    if (!acknowledgedView) {
      cleanup();
      return { status: "rejected", reason: "adapter" };
    }
    const added = exactAddedIds(before, poolMultiplicity(acknowledgedView.pool));
    const expected = request.instanceIds;
    const valid = added !== null
      && added.length === expected.length
      && expected.every((instanceId) => added.includes(instanceId));
    if (!valid) {
      cleanup();
      return { status: "rejected", reason: "unacknowledged" };
    }
    let workspace = reconcileWorkspaceState(state.workspaceState, acknowledgedView.pool);
    workspace = request.kind === "auto-pick"
      ? request.instanceIds.reduce(
        (next, instanceId) => applyDestination(
          next,
          acknowledgedView.pool,
          [instanceId],
          request.destination,
          request.placementHints?.[instanceId],
        ),
        workspace,
      )
      : applyDestination(
        workspace,
        acknowledgedView.pool,
        request.instanceIds,
        request.destination,
        request.placementHint,
      );
    exclusivePickToken = null;
    pendingGuestPick = null;
    installWorkspace({
      view: acknowledgedView,
      base: workspace,
      publish: true,
      patch: {
        phase: phaseForDraftViewStatus(acknowledgedView.status),
        selectedCard: null,
        pendingPickIntent: null,
        pickInteractionLocked: false,
      },
    });
    return { status: "acknowledged" };
  } catch {
    if (!isFresh()) return { status: "ignored", reason: "stale" };
    cleanup();
    return { status: "rejected", reason: "adapter" };
  }
}

interface DetachedDraftAdapters {
  host: DraftPodHostAdapter | null;
  guest: DraftPodGuestAdapter | null;
  hostPersistenceId: string | null;
}

function detachDraftAdapters(): DetachedDraftAdapters {
  const detached = {
    host: activeHostAdapter,
    guest: activeGuestAdapter,
    hostPersistenceId: activeHostPersistenceId,
  };
  activeHostAdapter = null;
  activeGuestAdapter = null;
  activeHostPersistenceId = null;
  activeHostAbort?.abort();
  activeGuestAbort?.abort();
  activeHostRouteAbortListener?.signal.removeEventListener("abort", activeHostRouteAbortListener.listener);
  activeGuestRouteAbortListener?.signal.removeEventListener("abort", activeGuestRouteAbortListener.listener);
  activeHostAbort = null;
  activeGuestAbort = null;
  activeHostRouteAbortListener = null;
  activeGuestRouteAbortListener = null;
  activeHostEventUnsub?.();
  activeGuestEventUnsub?.();
  activeHostEventUnsub = null;
  activeGuestEventUnsub = null;
  return detached;
}

async function disposeHostAdapter(adapter: DraftPodHostAdapter, preserveSession: boolean): Promise<void> {
  if (disposedHostAdapters.has(adapter)) return;
  disposedHostAdapters.add(adapter);
  await adapter.dispose({ preserveSession });
}

async function disposeGuestAdapter(adapter: DraftPodGuestAdapter, preserveRecovery = true): Promise<void> {
  if (disposedGuestAdapters.has(adapter)) return;
  disposedGuestAdapters.add(adapter);
  await adapter.dispose({ preserveRecovery });
}

async function disposeDetachedDraftAdapters(
  detached: DetachedDraftAdapters,
  preserveSession: boolean,
): Promise<void> {
  await Promise.allSettled([
    ...(detached.host ? [disposeHostAdapter(detached.host, preserveSession)] : []),
    ...(detached.guest ? [disposeGuestAdapter(detached.guest, preserveSession)] : []),
  ]);
}

/** Retains teardown after the active adapter ref has been detached on route abort. */
function retainDraftSessionTeardown(
  persistenceId: string | null,
  teardown: Promise<void>,
): void {
  if (!persistenceId) return;
  const previous = retainedDraftSessionTeardowns.get(persistenceId);
  const retained = previous
    ? previous.then(() => teardown, () => teardown)
    : teardown;
  retainedDraftSessionTeardowns.set(persistenceId, retained);
  void retained.then(
    () => {
      if (retainedDraftSessionTeardowns.get(persistenceId) === retained) {
        retainedDraftSessionTeardowns.delete(persistenceId);
      }
    },
    () => {
      if (retainedDraftSessionTeardowns.get(persistenceId) === retained) {
        retainedDraftSessionTeardowns.delete(persistenceId);
      }
    },
  );
}

/** Waits for the previous local owner of this persisted draft session to finish. */
async function claimDraftSessionOwner(persistenceId: string | undefined): Promise<void> {
  if (!persistenceId) return;
  await retainedDraftSessionTeardowns.get(persistenceId);
}

function lifecycleSignal(controller: AbortController): AbortSignal {
  return controller.signal;
}

function intergameAction(payload: DraftIntergameCommandPayload): GameAction {
  switch (payload.type) {
    case "SubmitSideboard":
      return { type: "SubmitSideboard", data: { main: payload.main, sideboard: payload.sideboard } };
    case "ChoosePlayDraw":
      return { type: "ChoosePlayDraw", data: { play_first: payload.playFirst } };
  }
}

const RARITY_SCORE: Record<string, number> = {
  mythic: 4,
  rare: 3,
  uncommon: 2,
  common: 1,
};

function preferredColors(pool: DraftCardInstance[]): Set<string> {
  const counts = new Map<string, number>();
  for (const card of pool) {
    for (const color of card.colors) {
      counts.set(color, (counts.get(color) ?? 0) + 1);
    }
  }

  return new Set(
    [...counts.entries()]
      .sort(([, a], [, b]) => b - a)
      .slice(0, 2)
      .map(([color]) => color),
  );
}

function curveScore(cmc: number, poolSize: number): number {
  if (poolSize < 5) {
    if (cmc <= 2) return 1;
    if (cmc >= 6) return -1;
    return 0;
  }

  if (cmc >= 2 && cmc <= 4) return 2;
  if (cmc >= 6) return -1;
  return 0;
}

function scoreDraftCard(card: DraftCardInstance, colors: Set<string>, poolSize: number): number {
  const rarityScore = (RARITY_SCORE[card.rarity.toLowerCase()] ?? 0) * 2;
  let colorScore = 0;
  if (card.colors.length === 0) {
    colorScore = 1;
  } else if (colors.size > 0) {
    colorScore = card.colors.some((color) => colors.has(color)) ? 3 : -1;
  }

  return rarityScore + colorScore + curveScore(card.cmc, poolSize);
}

/**
 * One whole CR 903.13b pick step: the top `view.required_pick_count` cards of
 * the current pack by the existing `scoreDraftCard` heuristic.
 *
 * The count is the engine's — never re-derived from `view.kind`, which cannot
 * see an odd pack's final one-card step. `Array.prototype.sort` is stable in
 * ES2019+ and sorts a COPY, so ties keep pack order and the N = 1 result is
 * identical to the previous first-best-wins linear scan. `preferredColors` and
 * the pool size are computed once, outside the comparator, exactly as before.
 *
 * Pick *quality* for a multi-card step is out of scope: these are the top N
 * independently scored cards, not a good pair.
 */
function chooseAutoPickCards(view: DraftPlayerView | null): string[] {
  const pack = view?.current_pack;
  if (!pack || pack.length === 0) return [];

  const colors = preferredColors(view.pool);
  const poolSize = view.pool.length;
  const scored = pack.map((card) => ({
    instanceId: card.instance_id,
    score: scoreDraftCard(card, colors, poolSize),
  }));
  scored.sort((a, b) => b.score - a.score);

  return scored.slice(0, view.required_pick_count).map((entry) => entry.instanceId);
}

function seatForLaunchGamePlayer(launch: DraftMatchLaunch, gamePlayer: number): number {
  switch (launch.type) {
    case "HumanHost":
      return gamePlayer === 0 ? launch.localSeat : launch.opponentSeat;
    case "HumanGuest":
      return gamePlayer === 1 ? launch.localSeat : launch.opponentSeat;
    case "Bot":
      return gamePlayer === 0 ? launch.localSeat : launch.botSeat;
  }
}

function winnerSeatForLaunch(launch: DraftMatchLaunch, gameWinner: number | null): number | null {
  return gameWinner === null ? null : seatForLaunchGamePlayer(launch, gameWinner);
}

function winnerSeatForGameResult(launch: DraftMatchLaunch, gameWinner: number | null): number | null {
  return gameWinner === null ? null : seatForLaunchGamePlayer(launch, gameWinner);
}

function localLaunchDeck(launch: DraftMatchLaunch) {
  switch (launch.type) {
    case "HumanHost":
    case "Bot":
      return launch.deckPayload.player;
    case "HumanGuest":
      return launch.localDeck;
  }
}

function nameMultiset(names: readonly string[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const name of names) counts.set(name, (counts.get(name) ?? 0) + 1);
  return counts;
}

function sameNameMultiset(left: readonly string[], right: readonly string[]): boolean {
  const leftCounts = nameMultiset(left);
  const rightCounts = nameMultiset(right);
  return leftCounts.size === rightCounts.size
    && [...leftCounts].every(([name, count]) => rightCounts.get(name) === count);
}

function disposeMatchController(): void {
  activeMatchController?.dispose();
  activeMatchController = null;
}

async function retryDraftSettlement(launch: DraftMatchLaunch, role: DraftRole): Promise<void> {
  if (launch.binding.matchAuthoritySeat !== launch.localSeat) return;
  const settlement = await loadDraftSettlementOutbox(launch.binding);
  if (!settlement) return;
  if (role === "host" && activeHostAdapter) {
    await activeHostAdapter.submitMatchSettlement(settlement);
    await clearDraftSettlementOutbox(launch.binding);
  } else if (role === "guest" && activeGuestAdapter) {
    activeGuestAdapter.sendMatchSettlement(settlement);
  }
}

/** The engine-side AI seat for a Bot draft match (the local player is game
 *  player 0, the bot is 1). Single authority shared by the live game-loop
 *  controller and the Resolve All batch drain, so the drain can never play
 *  the bot at a different difficulty than the controller driving it. */
export const DRAFT_BOT_AI_SEAT: AISeatBinding = { playerId: 1, difficulty: "Medium" };

async function installMatchRuntime(
  gameId: string,
  adapter: EngineAdapter,
  initResult: SubmitResult,
  controllerMode: "ai" | "online",
): Promise<void> {
  // Fetched after this match's engine is up, so the snapshot is
  // newest-by-construction under the global seq counter: it always passes the
  // commit gate, and it inherently drops any commit still in flight from the
  // previous game of a Bo3 (whose stamps are strictly lower).
  const snapshot = await adapter.getSnapshot();
  const initLogEntries: GameLogEntry[] = (initResult.log_entries ?? []).map((entry, i) => ({
    ...entry,
    seq: i,
  }));

  useGameStore.getState().commitEngineSnapshot(snapshot, {
    extraState: {
      gameId,
      gameMode: "draft-match",
      adapter,
      events: [] as GameEvent[],
      eventHistory: [] as GameEvent[],
      logHistory: initLogEntries,
      nextLogSeq: initLogEntries.length,
      stateHistory: [],
      turnCheckpoints: [],
      lobbyProgress: null,
    },
  });

  disposeMatchController();
  activeMatchController = createGameLoopController({
    mode: controllerMode,
    difficulty: DRAFT_BOT_AI_SEAT.difficulty,
    aiSeats: controllerMode === "ai" ? [DRAFT_BOT_AI_SEAT] : undefined,
    playerCount: 2,
  });
  activeMatchController.start();
}

function saveDraftPodProgress(phase: ActiveDraftPodPhase, view?: DraftPlayerView | null): void {
  const meta = loadActiveDraftPod();
  if (!meta) return;
  saveActiveDraftPod({
    ...meta,
    phase,
    pickCount: view?.pool.length ?? meta.pickCount,
    updatedAt: Date.now(),
  });
}

function updateActiveDraftPod(patch: Partial<ActiveDraftPodMeta>): void {
  const meta = loadActiveDraftPod();
  if (!meta) return;
  saveActiveDraftPod({ ...meta, ...patch, updatedAt: Date.now() });
}

function activePhaseForHostStatus(status: DraftPodHostStatus): ActiveDraftPodPhase | null {
  switch (status) {
    case "lobby":
    case "drafting":
    case "deckbuilding":
    case "pairing":
    case "matchInProgress":
    case "complete":
      return status;
    case "roundComplete":
      return "pairing";
    case "idle":
    case "connecting":
    case "error":
      return null;
  }
}

function phaseForDraftViewStatus(status: DraftPlayerView["status"]): MultiplayerDraftPhase {
  switch (status) {
    case "Lobby":
      return "lobby";
    case "Drafting":
    case "Paused":
      return "drafting";
    case "Deckbuilding":
      return "deckbuilding";
    case "Pairing":
      return "pairing";
    case "MatchInProgress":
      return "matchInProgress";
    case "RoundComplete":
      return "roundComplete";
    case "Complete":
      return "complete";
    case "Abandoned":
      return "error";
  }
}

function activePhaseForDraftViewStatus(status: DraftPlayerView["status"]): ActiveDraftPodPhase | null {
  switch (status) {
    case "Lobby":
      return "lobby";
    case "Drafting":
    case "Paused":
      return "drafting";
    case "Deckbuilding":
      return "deckbuilding";
    case "Pairing":
    case "RoundComplete":
      return "pairing";
    case "MatchInProgress":
      return "matchInProgress";
    case "Complete":
      return "complete";
    case "Abandoned":
      return null;
  }
}

/**
 * Dispose the active match adapter (P2PHostAdapter or P2PGuestAdapter).
 *
 * Documented exemption from the `commitEngineSnapshot` single-writer invariant:
 * this is a teardown clear, not a live-game commit. It has no snapshot to gate
 * on, and any subsequent live commit arrives newest-by-construction (a fresh
 * post-init fetch), so it cannot be resurrected by a stale pair.
 */
function disposeMatchAdapter(set: SetFn): void {
  const state = useMultiplayerDraftStore.getState();
  disposeMatchController();
  if (state.matchAdapter) {
    const adapter = state.matchAdapter as { dispose?: () => void };
    adapter.dispose?.();
    if (useGameStore.getState().adapter === state.matchAdapter) {
      useGameStore.setState({
        gameId: null,
        gameState: null,
        events: [],
        eventHistory: [],
        logHistory: [],
        nextLogSeq: 0,
        adapter: null,
        waitingFor: null,
        legalActions: [],
        autoPassRecommended: false,
        endContinuousEffectOffers: [],
        spellCosts: {},
        legalActionsByObject: {},
        stateHistory: [],
        turnCheckpoints: [],
      });
    }
    set({ matchAdapter: null, matchPairing: null, sideboardPrompt: null, playDrawPrompt: null, sideboardSubmitted: false });
  }
}

// ── Initial state ──────────────────────────────────────────────────────

const initialState: MultiplayerDraftState = {
  role: null,
  phase: "idle",
  roomCode: null,
  draftCode: null,
  seatIndex: null,
  view: null,
  seats: [],
  joined: 0,
  total: 0,
  paused: false,
  pauseReason: null,
  pairing: null,
  error: null,
  guestRecoveryFailure: null,
  selectedCard: null,
  workspaceState: null,
  pendingPickIntent: null,
  interactionGeneration: 0,
  pickInteractionLocked: false,
  workspaceSyncError: null,
  mainDeck: [],
  landCounts: {},
  timerRemainingMs: null,
  standings: [],
  currentRound: 0,
  nextPairingRound: 1,
  pairings: [],
  submittedDeck: [],
  submittedWorkspaceState: null,
  submittedPartition: null,
  intergameWorkspaceState: null,
  matchPairing: null,
  matchAdapter: null,
  sideboardPrompt: null,
  playDrawPrompt: null,
  sideboardSubmitted: false,
};

/**
 * Single authority for how long a pod error lives.
 *
 * An error belongs to the phase it was raised in; a *change* of phase retires
 * it. Wrapping the store's setter — rather than writing `error: null` into each
 * success arm — gives one rule for both roles: guests write `phase` from many
 * arms of `handleGuestEvent` and cleared it at none, while the host had a
 * single ad-hoc supersession on `pairingsGenerated`.
 *
 * Two properties are load-bearing and neither is incidental:
 *
 * - It fires only when `phase` actually *changes*. `viewUpdated` writes `phase`
 *   on every broadcast — a pick, a seat connecting, a seat dropping — so clearing
 *   on the mere presence of the key would erase an error the user has not read.
 *   That form was considered and rejected when this banner was introduced.
 * - The incoming payload wins. `kicked` and `hostLeft` write `phase` and `error`
 *   in one `set()`; spreading the payload last preserves the reason they carry.
 *
 * Not covered, deliberately: a retry that does not change phase — `startMatch`
 * fails and succeeds while `phase` is already `matchInProgress`. Dismissal
 * (`clearError`) remains the clearing path for that case.
 *
 * The Bo3 intergame window is no longer a phase at all — it is derived by
 * `draftPodScreen` from (`phase`, `sideboardPrompt`), and the three enter sites
 * (`handleBetweenGamesPrompt` and the two `bo3SideboardPrompt` arms) write no
 * `phase`. Three consequences for this rule, all intended:
 *
 * - Entering the window is not a transition, so an unread error survives it.
 * - Neither is any `viewUpdated`/`statusChanged`/`draftStarted` broadcast
 *   *during* the window: the phase is already `matchInProgress`, so a seat
 *   dropping mid-window no longer retires the banner. That narrowing was this
 *   block's own complaint and is now gone.
 * - Nor is the Bo3 game *boundary*: `bo3GameStarted` and the two `bo3GameStart`
 *   arms write `phase: "matchInProgress"` into a state whose phase is already
 *   `matchInProgress`, so the equal-phase short-circuit below returns `next`
 *   unchanged. An error raised earlier in the match therefore rides into game 2
 *   until the pod phase actually moves. This is a deliberate delta, not a bug;
 *   `clearError` remains the user's dismissal path.
 *
 * Scope: this wraps the *initializer's* setter, so it covers every write made
 * inside this module. It is not zustand middleware and does not rebind
 * `api.setState`, so `useMultiplayerDraftStore.setState(…)` bypasses the rule.
 * That is fine today — production has no such call site (only tests do) — but a
 * future production write through `setState` would not be phase-scoped.
 */
function clearErrorOnPhaseChange(set: SetFn): SetFn {
  return (partial) =>
    set((state) => {
      const next = typeof partial === "function" ? partial(state) : partial;
      return next.phase === undefined || next.phase === state.phase
        ? next
        : { error: null, ...next };
    });
}

/** Applies {@link clearErrorOnPhaseChange} to the store's setter.
 *
 * Borrows the `create<T>()(middleware(initializer))` *shape* from `gameStore`,
 * but it is not zustand middleware: it wraps only the initializer's `set`, so
 * external `useMultiplayerDraftStore.setState(…)` calls are not phase-scoped.
 * No production code does that (tests do); see `clearErrorOnPhaseChange`. */
function phaseScopedError(
  initializer: (
    set: SetFn,
    get: () => MultiplayerDraftState & MultiplayerDraftActions,
  ) => MultiplayerDraftState & MultiplayerDraftActions,
): StateCreator<MultiplayerDraftState & MultiplayerDraftActions> {
  return (set, get) => initializer(clearErrorOnPhaseChange(set), get);
}

// ── Store ──────────────────────────────────────────────────────────────

export const useMultiplayerDraftStore = create<
  MultiplayerDraftState & MultiplayerDraftActions
>()(phaseScopedError((set, get) => ({
  ...initialState,

  clearError: () => set({ error: null }),

  hostDraft: async (config) => {
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return false;
    }
    const epoch = ++draftAdapterEpoch;
    const previous = detachDraftAdapters();
    const previousTeardown = disposeDetachedDraftAdapters(previous, true);
    retainDraftSessionTeardown(previous.hostPersistenceId, previousTeardown);
    if (previous.host || previous.guest) await previousTeardown;
    if (config.persistenceId) await claimDraftSessionOwner(config.persistenceId);
    if (epoch !== draftAdapterEpoch || config.signal?.aborted) return false;
    if (getEffectiveOffline()) {
      // Replacement teardown was authorized before connectivity changed. This
      // epoch now owns the detached lifecycle, so it must not leave the prior
      // role/phase live after declining to construct its successor.
      set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
      return false;
    }

    const generation = beginDraftLifecycle();
    const adapter = new DraftPodHostAdapter();
    const controller = new AbortController();
    activeHostAdapter = adapter;
    activeHostAbort = controller;
    activeHostPersistenceId = config.persistenceId ?? null;

    activeHostEventUnsub = adapter.onEvent((event) => {
      if (generation === lifecycleGeneration && activeHostAdapter === adapter && epoch === draftAdapterEpoch) {
        handleHostEvent(event, set);
      }
    });

    const abortOwner = () => {
      controller.abort();
      if (activeHostAdapter !== adapter || epoch !== draftAdapterEpoch) return;
      ++draftAdapterEpoch;
      const detached = detachDraftAdapters();
      const teardown = disposeDetachedDraftAdapters(detached, true);
      retainDraftSessionTeardown(detached.hostPersistenceId, teardown);
      void teardown;
      set(initialState);
    };
    config.signal?.addEventListener("abort", abortOwner, { once: true });
    if (config.signal) activeHostRouteAbortListener = { signal: config.signal, listener: abortOwner };

    set({
      ...initialState,
      role: "host",
      phase: "connecting",
      seatIndex: 0,
      interactionGeneration: generation,
    });

    let initialized = false;
    try {
      await adapter.initialize({ ...config, signal: lifecycleSignal(controller) });
      initialized = true;
      if (activeHostAdapter !== adapter || epoch !== draftAdapterEpoch) return false;
      if (config.persistenceId) {
        const view = get().view;
        const phase = view ? activePhaseForDraftViewStatus(view.status) ?? "lobby" : "lobby";
        saveActiveDraftPod({
          id: config.persistenceId,
          roomCode: adapter.roomCode ?? config.preferredRoomCode ?? "",
          kind: config.kind,
          podSize: config.podSize,
          hostDisplayName: config.hostDisplayName,
          tournamentFormat: config.tournamentFormat,
          podPolicy: config.podPolicy,
          phase,
          pickCount: view?.pool.length ?? 0,
          updatedAt: Date.now(),
        });
      }
      return true;
    } catch {
      // The adapter reports the error while it is current. A late failure is
      // deliberately silent: its event gate was detached by the new owner.
    } finally {
      if (activeHostAdapter !== adapter || epoch !== draftAdapterEpoch) {
        config.signal?.removeEventListener("abort", abortOwner);
        await disposeHostAdapter(adapter, true);
      } else if (!initialized || adapter.status === "error") {
        activeHostAdapter = null;
        activeHostAbort = null;
        activeHostPersistenceId = null;
        activeHostEventUnsub?.();
        activeHostEventUnsub = null;
        config.signal?.removeEventListener("abort", abortOwner);
        activeHostRouteAbortListener = null;
        await disposeHostAdapter(adapter, true);
        if (epoch === draftAdapterEpoch && getEffectiveOffline()) {
          set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
        }
      }
    }
    return false;
  },

  joinDraft: async (config) => {
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return false;
    }
    const epoch = ++draftAdapterEpoch;
    const previous = detachDraftAdapters();
    const previousTeardown = disposeDetachedDraftAdapters(previous, true);
    retainDraftSessionTeardown(previous.hostPersistenceId, previousTeardown);
    if (previous.host || previous.guest) await previousTeardown;
    if (epoch !== draftAdapterEpoch || config.signal?.aborted) return false;
    if (getEffectiveOffline()) {
      // See hostDraft: this current replacement owns the already-detached
      // lifecycle and must publish an idle offline state rather than a phantom
      // connecting/lobby owner with no adapter.
      set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
      return false;
    }

    const generation = beginDraftLifecycle();
    const adapter = new DraftPodGuestAdapter();
    const controller = new AbortController();
    activeGuestAdapter = adapter;
    activeGuestAbort = controller;

    activeGuestEventUnsub = adapter.onEvent((event) => {
      if (generation === lifecycleGeneration && activeGuestAdapter === adapter && epoch === draftAdapterEpoch) {
        handleGuestEvent(event, set);
      }
    });

    const abortOwner = () => {
      controller.abort();
      if (activeGuestAdapter !== adapter || epoch !== draftAdapterEpoch) return;
      ++draftAdapterEpoch;
      const detached = detachDraftAdapters();
      const teardown = disposeDetachedDraftAdapters(detached, true);
      retainDraftSessionTeardown(detached.hostPersistenceId, teardown);
      void teardown;
      set(initialState);
    };
    config.signal?.addEventListener("abort", abortOwner, { once: true });
    if (config.signal) activeGuestRouteAbortListener = { signal: config.signal, listener: abortOwner };

    set({
      ...initialState,
      role: "guest",
      phase: "connecting",
      interactionGeneration: generation,
    });

    let initialized = false;
    try {
      await adapter.initialize({ ...config, signal: lifecycleSignal(controller) });
      initialized = true;
      if (activeGuestAdapter === adapter && epoch === draftAdapterEpoch) return true;
    } catch {
      // See hostDraft: only the current owner is allowed to project errors.
    } finally {
      if (activeGuestAdapter !== adapter || epoch !== draftAdapterEpoch) {
        config.signal?.removeEventListener("abort", abortOwner);
        await disposeGuestAdapter(adapter);
      } else if (!initialized || adapter.status === "error") {
        activeGuestAdapter = null;
        activeGuestAbort = null;
        activeGuestEventUnsub?.();
        activeGuestEventUnsub = null;
        config.signal?.removeEventListener("abort", abortOwner);
        activeGuestRouteAbortListener = null;
        await disposeGuestAdapter(adapter);
        if (epoch === draftAdapterEpoch && getEffectiveOffline()) {
          set({ ...initialState, error: DRAFT_OFFLINE_ERROR });
        }
      }
    }
    return false;
  },

  resumeDraft: async (options = {}) => {
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return "offline";
    }
    const routeToken = options.routeToken ?? 0;
    if (
      resumeGuestDraftAttempt
      && resumeGuestDraftAttempt.routeToken === routeToken
      && resumeGuestDraftAttempt.signal === options.signal
    ) {
      return resumeGuestDraftAttempt.promise;
    }

    const attempt: NonNullable<typeof resumeGuestDraftAttempt> = {
      routeToken,
      signal: options.signal,
      promise: Promise.resolve("superseded"),
    };
    const isCurrent = () => resumeGuestDraftAttempt === attempt && !options.signal?.aborted;
    attempt.promise = (async (): Promise<GuestDraftResumeOutcome> => {
      if (options.signal?.aborted) return "superseded";
      const active = inspectActiveDraftGuest();
      if (active.type === "absent") return "absent";
      if (active.type === "invalid") {
        if (active.capture) clearActiveDraftGuestIfCurrent(active.capture);
        else clearActiveDraftGuest();
        return "invalid";
      }
      const { meta: locator, capture } = active;
      const startingEpoch = draftAdapterEpoch;
      const locatorIsCurrent = () => {
        const current = inspectActiveDraftGuest();
        return current.type === "present"
          && current.capture.roomCode === capture.roomCode
          && current.capture.displayName === capture.displayName
          && current.capture.hostPeerId === capture.hostPeerId
          && current.capture.timestamp === capture.timestamp;
      };
        let session: Awaited<ReturnType<typeof loadDraftGuestSession>>;
      try {
        session = await loadDraftGuestSession(locator.hostPeerId, locator);
      } catch {
        if (!isCurrent() || draftAdapterEpoch !== startingEpoch || !locatorIsCurrent()) return "superseded";
        if (getEffectiveOffline()) {
          set({ error: DRAFT_OFFLINE_ERROR });
          return "offline";
        }
        return "failed";
      }
      if (!isCurrent() || draftAdapterEpoch !== startingEpoch || !locatorIsCurrent()) return "superseded";
      if (getEffectiveOffline()) {
        set({ error: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      if (!session) {
        clearActiveDraftGuestIfCurrent(capture);
        return "invalid";
      }

      if (!isCurrent() || draftAdapterEpoch !== startingEpoch || !locatorIsCurrent()) return "superseded";
      if (getEffectiveOffline()) {
        set({ error: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      const joined = await get().joinDraft({
        kind: "reconnect",
        roomCode: locator.roomCode,
        displayName: locator.displayName,
        hostPeerId: locator.hostPeerId,
        draftToken: session.draftToken,
        signal: options.signal,
      });
      if (!isCurrent()) return "superseded";
      if (joined) return "resumed";
      if (getEffectiveOffline()) {
        set({ error: DRAFT_OFFLINE_ERROR });
        return "offline";
      }
      if (get().guestRecoveryFailure?.kind === "invalid") {
        clearActiveDraftGuestIfCurrent(capture);
        return "invalid";
      }
      return "failed";
    })();
    resumeGuestDraftAttempt = attempt;
    try {
      return await attempt.promise;
    } finally {
      if (resumeGuestDraftAttempt === attempt) resumeGuestDraftAttempt = null;
    }
  },

  startDraft: async (botFillEmptySeats = true) => {
    if (!activeHostAdapter) return;
    if (getEffectiveOffline()) {
      set({ error: DRAFT_OFFLINE_ERROR });
      return;
    }
    await activeHostAdapter.startDraft(botFillEmptySeats);
  },

  submitPick: (cardInstanceId, destination = "deck", placementHint) => performPick({
    kind: "pick", instanceIds: [cardInstanceId], destination, placementHint,
  }),

  submitPickStep: (cardInstanceIds, destination = "deck", placementHint) => {
    const { view } = get();
    const selected = [...cardInstanceIds];
    const pack = view?.current_pack ?? [];
    if (
      !view
      || view.required_pick_count <= 0
      || selected.length !== view.required_pick_count
      || new Set(selected).size !== selected.length
      || selected.some((instanceId) => !pack.some((card) => card.instance_id === instanceId))
    ) {
      return Promise.resolve({ status: "rejected", reason: "invalid-request" });
    }
    return performPick({ kind: "pick", instanceIds: selected, destination, placementHint });
  },

  submitPickWithDraftEffect: (effectCardInstanceId, cardInstanceIds, destination = "deck", placementHint) => performPick({
    kind: "draft-effect", effectCardInstanceId, instanceIds: cardInstanceIds, destination, placementHint,
  }),

  selectCard: (cardInstanceId) => {
    if (get().pickInteractionLocked) return;
    set({ selectedCard: cardInstanceId });
  },

  confirmPick: (destination = "deck", placementHint) => {
    const { selectedCard } = get();
    if (!selectedCard) return Promise.resolve({ status: "rejected", reason: "invalid-request" });
    return performPick({ kind: "pick", instanceIds: [selectedCard], destination, placementHint });
  },

  autoPickCard: (placementHints) => {
    const { view } = get();
    const instanceIds = chooseAutoPickCards(view);
    if (instanceIds.length === 0) return Promise.resolve({ status: "rejected", reason: "invalid-request" });
    return performPick({
      kind: "auto-pick",
      instanceIds,
      destination: "deck",
      placementHints,
    });
  },

  setWorkspaceState: (next) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState) return;
    const reconciled = reconcileWorkspaceState(next, state.view.pool);
    if (reconciled === state.workspaceState) return;
    installWorkspace({ view: state.view, base: reconciled, publish: true });
  },

  setWorkspacePlacement: (instanceId, placement) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState) return;
    const next = updateWorkspacePlacement(state.workspaceState, state.view.pool, instanceId, placement);
    if (next === state.workspaceState) return;
    installWorkspace({ view: state.view, base: next, publish: true });
  },

  addBasicLand: (name) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState
      || state.workspaceState.virtualBasics.length >= MAX_MATERIALIZED_VIRTUAL_BASICS) return;
    const instanceId = makeInteractiveVirtualBasicInstanceId(state.workspaceState, state.view.pool);
    installWorkspace({
      view: state.view,
      base: addVirtualBasic(state.workspaceState, state.view.pool, { instanceId, name }),
      publish: true,
    });
  },

  removeBasicLand: (name) => {
    const state = get();
    if (state.pickInteractionLocked || !state.view || !state.workspaceState) return;
    const target = [...state.workspaceState.virtualBasics].reverse().find(
      (basic) => basic.name === name && state.workspaceState!.placements[basic.instanceId]?.zone === "deck",
    );
    if (!target) return;
    installWorkspace({
      view: state.view,
      base: removeVirtualBasic(state.workspaceState, target.instanceId),
      publish: true,
    });
  },

  retryWorkspaceSync: async () => {
    const state = get();
    if (!state.view || !state.workspaceState) return;
    await publishWorkspace(reconcileWorkspaceState(state.workspaceState, state.view.pool));
  },

  setIntergameWorkspaceState: (next) => {
    const state = get();
    if (draftPodScreen(state) !== "betweenGames" || !state.view || !state.intergameWorkspaceState) return;
    const workspace = reconcileWorkspaceState(next, state.view.pool);
    if (workspace === state.intergameWorkspaceState) return;
    set({ intergameWorkspaceState: workspace });
  },

  submitDeck: async (commanders = []) => {
    const { role, view, workspaceState } = get();
    if (!view || !workspaceState) return;
    const workspace = reconcileWorkspaceState(workspaceState, view.pool);
    const partition = projectWorkspacePartition(workspace, view.pool);

    if (role === "host" && activeHostAdapter) {
      const nextView = await activeHostAdapter.submitDeck(partition.mainDeck, commanders);
      installWorkspace({
        view: nextView,
        base: workspace,
        publish: false,
        patch: {
          submittedDeck: partition.mainDeck,
          submittedWorkspaceState: cloneWorkspace(workspace),
          submittedPartition: partition,
        },
      });
    } else if (role === "guest" && activeGuestAdapter) {
      await activeGuestAdapter.submitDeck(partition.mainDeck, commanders);
      set({
        submittedDeck: partition.mainDeck,
        submittedWorkspaceState: cloneWorkspace(workspace),
        submittedPartition: partition,
      });
    }
  },

  kickPlayer: (seat, reason) => {
    if (!activeHostAdapter) return;
    activeHostAdapter.kickPlayer(seat, reason);
  },

  requestPause: () => {
    if (!activeHostAdapter) return;
    activeHostAdapter.requestPause();
  },

  requestResume: () => {
    if (!activeHostAdapter) return;
    activeHostAdapter.requestResume();
  },

  launchCommanderGame: async (navigate) => {
    const { role, view, seatIndex } = get();
    if (
      role !== "host" ||
      !view ||
      view.launch_capability !== "CommanderMultiplayer" ||
      view.status !== "Complete" ||
      seatIndex === null ||
      !activeHostAdapter
    ) {
      return;
    }

    let payload: DraftMatchDeckPayload;
    try {
      payload = await activeHostAdapter.podCommanderDeckPayload(view, seatIndex);
    } catch (err) {
      // A refusal from draft-wasm reaches here: `get_bot_deck_inner` returns
      // `Err` when it cannot judge a bot deck's legality (no card database) or
      // when the deck it built is under the session's floor. Surface it and do
      // NOT navigate — the same shape `startMatch`'s own catch uses.
      console.error("[multiplayerDraftStore] launchCommanderGame failed:", err);
      set({ error: err instanceof Error ? err.message : String(err) });
      return;
    }

    const gameId = crypto.randomUUID();
    sessionStorage.setItem(`${DRAFT_DECK_SESSION_KEY}:${gameId}`, JSON.stringify(payload));
    useGameStore.setState({ gameId });
    // No `source=draft`/`draftId=`: those bind a game to a LOCAL Quick-Draft
    // run's bookkeeping, and a pod has neither a `DraftRun` nor active-quick-
    // draft meta. The pod is already `Complete`, so there is nothing to report
    // back to it.
    navigate(
      `/game/${gameId}?mode=ai&difficulty=${DRAFT_BOT_AI_SEAT.difficulty}` +
        `&format=CommanderDraft&players=${view.seats.length}&match=bo1`,
    );
  },

  startMatch: async () => {
    const { matchPairing, matchAdapter } = get();
    if (!matchPairing) return null;
    const gameId = `draft-match-${matchPairing.matchId}`;
    if (matchAdapter) return gameId;

    try {
      if (matchPairing.type === "HumanHost") {
        // Lower seat# hosts the match (D-09).
        const [{ hostRoom }, { P2PHostAdapter }] = await Promise.all([
          import("../network/connection"),
          import("../adapter/p2p-adapter"),
        ]);

        const host = await hostRoom(undefined, {
          preferredRoomCode: matchPairing.matchRoomCode,
        });

        const matchAdapter = new P2PHostAdapter(
          matchPairing.deckPayload,
          host.peer,
          host.onGuestConnected,
          2, // 1v1 match
          DRAFT_MATCH_FORMAT_CONFIG,
          matchPairing.matchConfig,
          undefined,
          undefined,
          undefined,
          undefined,
          undefined,
          undefined,
          {
            onConcede: (concedingGamePlayer) => get().reportActiveMatchConcession(concedingGamePlayer),
          },
        );

        let resolveRoomFull!: () => void;
        const roomFull = new Promise<void>((resolve) => {
          resolveRoomFull = resolve;
        });
        matchAdapter.onEvent((event) => {
          if (event.type === "roomFull") {
            resolveRoomFull();
          }
          if (event.type === "stateChanged") {
            processRemoteUpdate(event.snapshot, event.events, event.logEntries).catch((err) => {
              // A rejected delivery is otherwise gone and the screen freezes
              // on the previous state — surface it and re-sync immediately.
              debugLog(`draft-match remote update failed: ${err instanceof Error ? err.message : String(err)}`);
              resyncFromAdapterSafely("delivery rejected");
            });
          }
          if (event.type === "stateChanged") {
            const wf = event.snapshot.state?.waiting_for;
            if (!wf) return;

            if (wf.type === "GameOver") {
              // Match is complete — report result to pod host
              const winnerSeat = winnerSeatForLaunch(matchPairing, wf.data.winner);
              void get().reportMatchResult(matchPairing.matchId, winnerSeat);
            } else if (wf.type === "BetweenGamesSideboard") {
              // Between games in Bo3 — bridge to draft pod host for sideboard orchestration.
              const score = wf.data.score;
              const gameNumber = wf.data.game_number;
              // Determine loser: the player whose wins are fewer
              const loserSeat = score.p0_wins > score.p1_wins
                ? matchPairing.opponentSeat
                : score.p1_wins > score.p0_wins
                  ? matchPairing.localSeat
                  : null; // draw
              if (activeHostAdapter) {
                activeHostAdapter.handleMatchBetweenGames(
                  matchPairing.matchId,
                  gameNumber,
                  score,
                  loserSeat,
                  matchPairing.localSeat,
                  matchPairing.opponentSeat,
                );
              } else {
                activeGuestAdapter?.handleMatchBetweenGames(
                  matchPairing.matchId,
                  gameNumber,
                  score,
                  loserSeat,
                );
              }
              // Also transition the host's own UI to betweenGames
              get().handleBetweenGamesPrompt({
                matchId: matchPairing.matchId,
                gameNumber,
                score,
                loserSeat,
                timerMs: 0, // Host determines timer internally via podPolicy
              });
            }
          }
          if (event.type === "gameOver") {
            // Connection-level failure — report as match loss
            const winnerSeat = winnerSeatForLaunch(matchPairing, event.winner);
            void get().reportMatchResult(matchPairing.matchId, winnerSeat);
          }
        });

        await matchAdapter.initialize();
        await roomFull;
        const initResult = await matchAdapter.startPregameGame();
        await installMatchRuntime(gameId, matchAdapter, initResult, "online");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      } else if (matchPairing.type === "HumanGuest") {
        // Higher seat# joins as guest.
        const [{ joinRoom }, { P2PGuestAdapter }] = await Promise.all([
          import("../network/connection"),
          import("../adapter/p2p-adapter"),
        ]);

        const { conn, peer } = await joinRoom(matchPairing.matchRoomCode);

        const matchAdapter = new P2PGuestAdapter(
          {
            player: matchPairing.localDeck,
          },
          peer,
          conn.peer,
          conn,
          undefined,
          undefined,
          undefined,
          undefined,
          undefined,
          true,
        );

        matchAdapter.onEvent((event) => {
          if (event.type === "stateChanged") {
            processRemoteUpdate(event.snapshot, event.events, event.logEntries).catch((err) => {
              // A rejected delivery is otherwise gone and the screen freezes
              // on the previous state — surface it and re-sync immediately.
              debugLog(`draft-match remote update failed: ${err instanceof Error ? err.message : String(err)}`);
              resyncFromAdapterSafely("delivery rejected");
            });
          }
          if (event.type === "stateChanged") {
            const wf = event.snapshot.state?.waiting_for;
            if (!wf) return;

            if (wf.type === "GameOver") {
              // Guest reports as backup (host's report is authoritative)
              const winnerSeat = winnerSeatForLaunch(matchPairing, wf.data.winner);
              void get().reportMatchResult(matchPairing.matchId, winnerSeat);
            }
            // BetweenGamesSideboard: guest receives sideboard prompt via draft pod channel
            // (handled by bo3SideboardPrompt event from P2PDraftGuest), not here.
          }
          if (event.type === "gameOver") {
            // Connection failure — report as match loss
            const winnerSeat = winnerSeatForLaunch(matchPairing, event.winner);
            void get().reportMatchResult(matchPairing.matchId, winnerSeat);
          }
        });

        await matchAdapter.initialize();
        const initResult = await matchAdapter.initializeGame();
        await installMatchRuntime(gameId, matchAdapter, initResult, "online");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      } else {
        const { WasmAdapter } = await import("../adapter/wasm-adapter");
        const matchAdapter = new WasmAdapter();
        // #7920: a bot match installs no transport-side whole-match concede,
        // so the menu's Concede was refused as unbound. Bind the capability
        // to a plain game-level Concede for the local seat (game player 0 —
        // the DRAFT_BOT_AI_SEAT authority): CR 104.3a ends the game as a
        // loss, the game-over screen's existing pod effect settles the match,
        // and "Back to pod" returns to the standings with any next round
        // intact.
        matchAdapter.bindMatchConcede(() => {
          void useGameStore
            .getState()
            .dispatch({ type: "Concede", data: { player_id: 0 } })
            .catch((err) => {
              console.error("[multiplayerDraftStore] bot-match concede failed:", err);
            });
        });
        await matchAdapter.initialize();
        const initResult = await matchAdapter.initializeGame(
          matchPairing.deckPayload,
          DRAFT_MATCH_FORMAT_CONFIG,
          2,
          matchPairing.matchConfig,
        );
        await installMatchRuntime(gameId, matchAdapter, initResult, "ai");
        set({ matchAdapter, phase: "matchInProgress" });
        return gameId;
      }
    } catch (err) {
      console.error("[multiplayerDraftStore] startMatch failed:", err);
      set({ error: err instanceof Error ? err.message : String(err) });
      return null;
    }
  },

  reportMatchResult: (matchId, winnerSeat) => {
    const { role, matchPairing } = get();
    if (!matchPairing || matchPairing.matchId !== matchId) return Promise.resolve();
    if (matchPairing.binding.matchAuthoritySeat !== matchPairing.localSeat) return Promise.resolve();
    return (async () => {
      const existing = await loadDraftSettlementOutbox(matchPairing.binding);
      const settlement: DraftMatchSettlement = existing ?? {
        binding: matchPairing.binding,
        receiptId: crypto.randomUUID(),
        winnerSeat,
      };
      await saveDraftSettlementOutbox(settlement);
      if (role === "host" && activeHostAdapter) {
        await activeHostAdapter.submitMatchSettlement(settlement);
        await clearDraftSettlementOutbox(matchPairing.binding);
      } else if (role === "guest" && activeGuestAdapter) {
        activeGuestAdapter.sendMatchSettlement(settlement);
      }
    })();
  },

  reportActiveMatchGameResult: async (gameWinner) => {
    const { matchPairing, reportMatchResult } = get();
    if (!matchPairing) return;
    await reportMatchResult(
      matchPairing.matchId,
      winnerSeatForGameResult(matchPairing, gameWinner),
    );
  },

  reportActiveMatchConcession: async (concedingGamePlayer = 0) => {
    const { matchPairing, reportMatchResult } = get();
    if (!matchPairing) return;
    await reportMatchResult(
      matchPairing.matchId,
      seatForLaunchGamePlayer(matchPairing, concedingGamePlayer === 0 ? 1 : 0),
    );
  },

  advanceRound: () => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.advanceRound();
  },

  overrideMatchResult: (matchId, winnerSeat) => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.overrideMatchResult(matchId, winnerSeat);
  },

  replaceSeatWithBot: (seat) => {
    if (!activeHostAdapter) return;
    void activeHostAdapter.replaceSeatWithBot(seat);
  },

  submitSideboard: (matchId, mainDeck, sideboard) => {
    const launch = get().matchPairing;
    if (!launch || launch.matchId !== matchId) return;
    const submittedNames = [
      ...mainDeck,
      ...sideboard.flatMap(({ name, count }) => (
        Number.isSafeInteger(count) && count >= 0 ? Array<string>(count).fill(name) : []
      )),
    ];
    const registered = localLaunchDeck(launch);
    if (!sameNameMultiset(submittedNames, [...registered.main_deck, ...registered.sideboard])) {
      set({ error: "Sideboard submission does not match the registered match pool" });
      return;
    }
    void get().submitIntergameCommand({
      type: "SubmitSideboard",
      main: countProjectedNames(mainDeck),
      sideboard,
    });
  },

  choosePlayDraw: (matchId, playFirst) => {
    void matchId;
    void get().submitIntergameCommand({ type: "ChoosePlayDraw", playFirst });
  },

  submitIntergameCommand: async (payload) => {
    const state = get();
    const launch = state.matchPairing;
    const gameNumber = state.sideboardPrompt?.gameNumber ?? state.playDrawPrompt?.gameNumber;
    if (!launch || gameNumber === undefined || state.seatIndex === null) return;
    let controller = intergameControllers.get(launch.matchId);
    if (!controller) {
      controller = new IntergameCommandController(await loadDraftIntergameCommands(launch.matchId));
      controller.recover();
      intergameControllers.set(launch.matchId, controller);
    }
    const command = controller.hold({
      commandId: crypto.randomUUID(), matchId: launch.matchId, gameNumber,
      seat: state.seatIndex, payload, launchPayload: launch, launchDigest: draftIntergameDigest(launch),
    });
    await saveDraftIntergameCommands(launch.matchId, controller.snapshot());
    if (state.role === "host") activeHostAdapter?.submitAuthorized(state.seatIndex, command);
    else activeGuestAdapter?.submitAuthorized(command);
    if (payload.type === "SubmitSideboard") set({ sideboardSubmitted: true });
  },

  submitAuthorized: async (command, acknowledgement) => {
    const state = get();
    const launch = state.matchPairing;
    if (!launch || command.matchId !== launch.matchId || command.launchDigest !== draftIntergameDigest(launch)) return;
    let controller = intergameControllers.get(command.matchId);
    if (!controller) {
      controller = new IntergameCommandController(await loadDraftIntergameCommands(command.matchId));
      controller.recover();
      intergameControllers.set(command.matchId, controller);
    }
    const known = controller.snapshot().find((candidate) => candidate.commandId === command.commandId);
    if (known?.status === "Pending") controller.authorize(command.commandId, acknowledgement);
    else if (!known && command.status === "Authorized") {
      controller = new IntergameCommandController([...controller.snapshot(), command]);
      intergameControllers.set(command.matchId, controller);
    }
    const permit = controller.begin(command.commandId, acknowledgement);
    if (!permit || !consumeIntergamePermit(permit, acknowledgement)) return;
    const adapter = state.matchAdapter as EngineAdapter | null;
    if (!adapter) return;
    // Re-check immediately before crossing the host, guest, native, or WASM sink.
    const actor = launch.type === "HumanGuest" ? 1 : 0;
    try {
      await adapter.submitAction(intergameAction(command.payload), actor);
    } catch (err) {
      if (reportStructuredActionRejection(err) !== "not-structured") {
        const message = err instanceof Error ? err.message : String(err);
        controller.reject(command.commandId, acknowledgement, message);
        await saveDraftIntergameCommands(command.matchId, controller.snapshot());
        if (command.payload.type === "SubmitSideboard") set({ sideboardSubmitted: false });
        return;
      }
      throw err;
    }
    const receipted = controller.receipt(command.commandId, acknowledgement, crypto.randomUUID());
    if (!receipted) return;
    await saveDraftIntergameCommands(command.matchId, controller.snapshot());
    if (state.role === "host") {
      activeHostAdapter?.submitAuthorized(state.seatIndex!, receipted);
    } else {
      activeGuestAdapter?.acknowledgeAuthorized(commandAcknowledgement(receipted), receipted.receiptId!);
    }
  },

  handleBetweenGamesPrompt: (prompt) => {
    const state = get();
    const source = state.intergameWorkspaceState ?? state.submittedWorkspaceState;
    const launch = state.matchPairing;
    const view = state.view;
    let intergameWorkspaceState: DraftWorkspaceState | null = null;
    let error = state.error;
    if (source && launch && view) {
      const candidate = reconcileWorkspaceState(cloneWorkspace(source), view.pool);
      const partition = projectWorkspacePartition(candidate, view.pool);
      const registered = localLaunchDeck(launch);
      if (sameNameMultiset(
        [...partition.mainDeck, ...partition.sideboard],
        [...registered.main_deck, ...registered.sideboard],
      )) {
        intergameWorkspaceState = candidate;
      } else {
        error = "Submitted deck partition does not match the registered match pool";
      }
    }
    set({
      phase: "matchInProgress",
      intergameWorkspaceState,
      error,
      sideboardPrompt: {
        matchId: prompt.matchId,
        gameNumber: prompt.gameNumber,
        score: prompt.score,
        loserSeat: prompt.loserSeat,
        timerMs: prompt.timerMs,
      },
      sideboardSubmitted: false,
      playDrawPrompt: null,
      timerRemainingMs: prompt.timerMs > 0 ? prompt.timerMs : null,
    });
  },

  leave: async (preserveRecovery = false) => {
    const host = activeHostAdapter;
    const guest = activeGuestAdapter;

    // An explicit guest leave is host-acknowledged. Until that completes, the
    // live adapter remains the recovery owner; tearing down the lifecycle here
    // would discard the session that must reconnect after a dropped ACK.
    if (host) {
      await host.dispose({ preserveSession: preserveRecovery });
    }
    if (guest) {
      await guest.dispose({ preserveRecovery });
    }

    beginDraftLifecycle();
    // The pod session has now ended, so its game transport can follow it.
    disposeMatchAdapter(set);

    if (activeHostAdapter === host) {
      activeHostAdapter = null;
      if (!preserveRecovery) {
        clearActiveDraftPod();
      }
    }
    if (activeGuestAdapter === guest) {
      activeGuestAdapter = null;
    }
    set({ ...initialState, interactionGeneration: lifecycleGeneration });
  },

  reset: () => {
    beginDraftLifecycle();
    disposeMatchAdapter(set);
    set({ ...initialState, interactionGeneration: lifecycleGeneration });
  },
})));

// ── Event handlers ─────────────────────────────────────────────────────

function hostStatusToPhase(status: DraftPodHostStatus): MultiplayerDraftPhase {
  switch (status) {
    case "idle":
      return "idle";
    case "connecting":
      return "connecting";
    case "lobby":
      return "lobby";
    case "drafting":
      return "drafting";
    case "deckbuilding":
      return "deckbuilding";
    case "pairing":
      return "pairing";
    case "matchInProgress":
      return "matchInProgress";
    case "roundComplete":
      return "roundComplete";
    case "complete":
      return "complete";
    case "error":
      return "error";
  }
}

function guestStatusToPhase(status: DraftPodGuestStatus): MultiplayerDraftPhase {
  switch (status) {
    case "idle":
      return "idle";
    case "connecting":
      return "connecting";
    case "lobby":
      return "lobby";
    case "drafting":
      return "drafting";
    case "deckbuilding":
      return "deckbuilding";
    case "matchInProgress":
      return "matchInProgress";
    case "complete":
      return "complete";
    case "kicked":
      return "kicked";
    case "hostLeft":
      return "hostLeft";
    case "error":
      return "error";
  }
}

type SetFn = (
  partial:
    | Partial<MultiplayerDraftState>
    | ((state: MultiplayerDraftState) => Partial<MultiplayerDraftState>),
) => void;

function installEventView(view: DraftPlayerView): void {
  if (exclusivePickToken) return;
  const state = useMultiplayerDraftStore.getState();
  const restored = restoredWorkspace?.generation === lifecycleGeneration ? restoredWorkspace : null;
  restoredWorkspace = null;
  const base = restored?.state ?? state.workspaceState ?? createDraftWorkspaceState();
  const workspace = reconcileWorkspaceState(base, view.pool);
  const publish = restored !== null
    ? (restored.state === null ? view.pool.length > 0 : workspace !== base)
    : workspace !== state.workspaceState;
  installWorkspace({
    view,
    base: workspace,
    publish,
    patch: {
      phase: phaseForDraftViewStatus(view.status),
      timerRemainingMs: view.timer_remaining_ms ?? null,
      standings: view.standings ?? [],
      currentRound: view.current_round ?? 0,
      pairings: view.pairings ?? [],
    },
  });
}

function handleHostEvent(event: DraftPodHostEvent, set: SetFn): void {
  switch (event.type) {
    case "workspaceRestored":
      restoredWorkspace = {
        generation: lifecycleGeneration,
        state: event.workspaceState ? cloneWorkspace(event.workspaceState) : null,
      };
      break;
    case "statusChanged":
      set({ phase: hostStatusToPhase(event.status) });
      {
        const activePhase = activePhaseForHostStatus(event.status);
        if (activePhase) saveDraftPodProgress(activePhase);
      }
      break;
    case "roomCreated":
      set({ roomCode: event.roomCode });
      updateActiveDraftPod({ roomCode: event.roomCode });
      break;
    case "viewUpdated":
      installEventView(event.view);
      {
        const activePhase = activePhaseForDraftViewStatus(event.view.status);
        if (activePhase) saveDraftPodProgress(activePhase, event.view);
      }
      break;
    case "lobbyUpdate":
      set({ joined: event.joined, total: event.total, seats: event.seats });
      break;
    case "lobbyFull":
      break;
    case "draftStarted": {
      const activePhase = activePhaseForDraftViewStatus(event.view.status);
      installEventView(event.view);
      if (activePhase) saveDraftPodProgress(activePhase, event.view);
      break;
    }
    case "draftComplete":
      set({ phase: "deckbuilding" });
      saveDraftPodProgress("deckbuilding");
      break;
    case "allDecksSubmitted":
      // Shape B: a documented no-op. This event fires for EVERY pod kind, so it
      // cannot know where the pod went — a `PostDraftPlay::CompleteImmediately`
      // pod is already `Complete` (draft-core session.rs:902), and writing
      // "pairing" here overwrote the reducer's own answer. The `viewUpdated`
      // that follows establishes BOTH: the phase via `phaseForDraftViewStatus`
      // and the persisted record via `activePhaseForDraftViewStatus`.
      //
      // The arm itself stays for the reader, not for the compiler: this
      // `switch` returns `void` and has no `default`/`assertNever`, so dropping
      // the case would still compile. Written out, the no-op is a decision on
      // the record; deleted, it reads as an event nobody considered.
      break;
    case "draftPaused":
      set({ paused: true, pauseReason: event.reason });
      break;
    case "draftResumed":
      set({ paused: false, pauseReason: null });
      break;
    case "pairingsGenerated":
      // No `error: null` here. `clearErrorOnPhaseChange` retires the banner one
      // step earlier, when `roundAdvanced` / `statusChanged("pairing")` moves the
      // phase into `pairing` — off `roundComplete` at a round boundary, off
      // `deckbuilding` at round 0 — and it does the same for guests, which this
      // host-only arm never could.
      set({
        phase: "matchInProgress",
        currentRound: event.round,
        pairings: event.pairings,
      });
      saveDraftPodProgress("matchInProgress");
      break;
    case "matchStart":
      set({ matchPairing: event.launch, phase: "matchInProgress" });
      void retryDraftSettlement(event.launch, "host");
      break;
    case "roundAdvanced":
      disposeMatchAdapter(set);
      // `currentRound` is engine-owned: `viewUpdated` and `pairingsGenerated`
      // both write it from engine state moments later.
      set({ phase: "pairing" });
      saveDraftPodProgress("pairing");
      break;
    case "roundComplete":
      disposeMatchAdapter(set);
      break;
    case "matchResultReceived":
      // Informational — standings update comes via viewUpdated
      break;
    case "timerExpired":
      break;
    case "error":
      set({ error: event.message });
      break;
    // Seat events are informational — the lobby update carries the authoritative seat list
    case "seatJoined":
    case "seatReconnected":
    case "seatDisconnected":
    case "seatKicked":
    case "pickReceived":
    case "deckSubmitted":
      break;
    case "bo3SideboardPromptSent":
      // Host UI transition handled by the stateChanged bridge in startMatch.
      break;
    case "bo3BothSideboardsSubmitted":
      // Informational — play/draw prompt or game start follows automatically.
      break;
    case "bo3GameStarted":
      set({ phase: "matchInProgress", sideboardPrompt: null, playDrawPrompt: null, sideboardSubmitted: false });
      saveDraftPodProgress("matchInProgress");
      break;
    case "bo3SideboardPrompt":
      useMultiplayerDraftStore.getState().handleBetweenGamesPrompt(event);
      break;
    case "bo3ChoosePlayDraw":
      set({
        playDrawPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        },
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3GameStart":
      set({
        phase: "matchInProgress",
        sideboardPrompt: null,
        playDrawPrompt: null,
        sideboardSubmitted: false,
      });
      break;
    case "bo3AuthorizedCommand":
      void useMultiplayerDraftStore.getState().submitAuthorized(event.command, event.acknowledgement);
      break;
  }
}

function handleGuestEvent(event: DraftPodGuestEvent, set: SetFn): void {
  switch (event.type) {
    case "workspaceRestored":
      restoredWorkspace = {
        generation: lifecycleGeneration,
        state: event.workspaceState ? cloneWorkspace(event.workspaceState) : null,
      };
      break;
    case "statusChanged":
      set({ phase: guestStatusToPhase(event.status) });
      break;
    case "joined":
      set({
        seatIndex: event.seatIndex,
        draftCode: event.draftCode,
        phase: "lobby",
      });
      break;
    case "reconnected":
      set({ seatIndex: event.seatIndex });
      break;
    case "viewUpdated":
      installEventView(event.view);
      break;
    case "pickAcknowledged":
      if (pendingGuestPick?.generation === lifecycleGeneration) {
        const pending = pendingGuestPick;
        pendingGuestPick = null;
        pending.resolve(event.view);
      }
      break;
    case "lobbyUpdate":
      set({ seats: event.seats, joined: event.joined, total: event.total });
      break;
    case "draftPaused":
      set({ paused: true, pauseReason: event.reason });
      break;
    case "draftResumed":
      set({ paused: false, pauseReason: null });
      break;
    case "pairing":
      set({
        pairing: {
          round: event.round,
          table: event.table,
          opponentName: event.opponentName,
          matchHostPeerId: event.matchHostPeerId,
          matchId: event.matchId,
        },
      });
      break;
    case "matchResult":
      break;
    case "timerSync":
      set({ timerRemainingMs: event.remainingMs });
      break;
    case "matchStart":
      set({
        matchPairing: event.launch,
        phase: "matchInProgress",
      });
      void retryDraftSettlement(event.launch, "guest");
      break;
    case "matchSettlementAcknowledged": {
      const binding = useMultiplayerDraftStore.getState().matchPairing?.binding;
      if (binding?.matchId === event.matchId && binding.revision === event.revision) {
        void clearDraftSettlementOutbox(binding);
      }
      break;
    }
    case "kicked":
      set({ phase: "kicked", error: event.reason });
      break;
    case "hostLeft":
      set({ phase: "hostLeft", error: event.reason });
      break;
    case "error":
      if (pendingGuestPick?.generation === lifecycleGeneration) {
        const pending = pendingGuestPick;
        pendingGuestPick = null;
        pending.resolve(null);
      }
      set({ error: event.message });
      break;
    case "reconnecting":
      break;
    case "reconnectFailed":
      set({ error: event.failure.message, guestRecoveryFailure: event.failure });
      break;
    case "bo3SideboardPrompt":
      useMultiplayerDraftStore.getState().handleBetweenGamesPrompt(event);
      break;
    case "bo3ChoosePlayDraw":
      set({
        playDrawPrompt: {
          matchId: event.matchId,
          gameNumber: event.gameNumber,
          score: event.score,
          timerMs: event.timerMs,
        },
        timerRemainingMs: event.timerMs > 0 ? event.timerMs : null,
      });
      break;
    case "bo3GameStart":
      set({
        phase: "matchInProgress",
        sideboardPrompt: null,
        playDrawPrompt: null,
        sideboardSubmitted: false,
      });
      break;
    case "bo3AuthorizedCommand":
      void useMultiplayerDraftStore.getState().submitAuthorized(event.command, event.acknowledgement);
      break;
    case "bo3ScoreUpdate":
      // Informational — standings update comes via viewUpdated
      break;
  }
}
