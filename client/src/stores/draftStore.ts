import { create } from "zustand";
import i18n from "i18next";

export const DRAFT_DECK_SESSION_KEY = "phase:draft-deck";

import {
  DraftAdapter,
  distinctJoined,
  drainDraftEngineOperations,
  draftSelectionPickCount,
  draftViewPresentation,
  draftViewPrompt,
  draftViewSessionId,
  sameDraftPrompt,
  withDraftEngineOperation,
  type CubeDraftSettings,
  type DraftEngineOperationLease,
  type DraftPlayerView,
  type FifoPromptId,
  type LlmDraftResponsePayload,
  type LocalDraftRuntime,
  type LocalFifoLlmDrafting,
  type LocalFifoLlmPickRequest,
  type LocalFifoOwner,
  type SetPackSequence,
  type SuggestedDeck,
} from "../adapter/draft-adapter";
import { CUSTOM_CUBE_SET_CODE } from "../adapter/draftKinds";
import {
  buildLocalFifoLlmRequests,
  callLlmDrafter,
  cancelLlmDraftRun,
  recordLlmDraftSeatUsage,
  recordLlmDraftUsage,
  reportLlmDraftOutcomes,
  startLlmDraftRound,
  type LlmDraftRound,
  type LlmDrafterState,
  type LlmDrafterStatus,
} from "../services/llm/draftLlm";
import { loadProviderCatalog } from "../services/llm/catalog";
import { reportLlmFailure } from "../services/llm/diagnostics";
import type { LlmDraftOutcome, LlmProfile } from "../services/llm/types";
import { draftProfile, useLlmStore } from "./llmStore";
import {
  MAX_MATERIALIZED_VIRTUAL_BASICS,
  migrateLegacyWorkspace,
  normalizeVirtualBasicCount,
} from "../components/draft/workspace/workspaceMigration";
import {
  appendWorkspaceInstanceToResolvedDestination,
  createDraftWorkspaceState,
  makeInteractiveVirtualBasicInstanceId,
  placeArrivingPoolCards,
  reconcileWorkspaceState,
  unplacedPoolIds,
  updateWorkspacePlacement,
} from "../components/draft/workspace/workspacePlacement";
import { getArrivingCardBoardPreferences } from "../components/draft/workspace/workspacePreferences";
import {
  addVirtualBasic,
  projectDeckNames,
  projectWorkspaceMainDeck,
  projectWorkspacePartition,
  removeVirtualBasic,
} from "../components/draft/workspace/workspaceProjection";
import type {
  DraftCardPlacement,
  DraftWorkspaceState,
  DraftZone,
} from "../components/draft/workspace/types";
import { BASIC_LAND_NAMES } from "../constants/game";
import { autosaveDraftDeck } from "../services/draftDeckAutosave";
import {
  cleanupQuickDraftLifecycle,
  drainQuickDraftPersistence,
  inspectActiveQuickDraftLifecycle,
  loadDraftRun,
  loadQuickDraftSession,
  persistQuickDraftSnapshot,
  publishInitialDraftMatch,
  publishStagedDraftMatch,
  recordDraftMatchResult,
  resolveDraftRunOpponentSeat,
  saveDraftRun,
  runLimits,
  type ActiveQuickDraftMeta,
  type DraftMatchPayload,
  type DraftMatchResult,
  type DraftRunFormat,
  type DraftRunState,
} from "../services/quickDraftPersistence";
import { usePreferencesStore } from "./preferencesStore";
import { useGameStore } from "./gameStore";
import { getSharedAdapter } from "../adapter/wasm-adapter";
import type { GameFormat, MatchType } from "../adapter/types";

export type DraftPhase = "setup" | "drafting" | "opening" | "deckbuilding" | "launching" | "playing" | "complete";
export type DraftResumeOutcome =
  | { status: "resumed"; draftId: string }
  | { status: "none" }
  | { status: "unavailable"; draftId: string; reason: string };

/** One booster of a local set draft: which set fills it, and that set's name. */
export interface DraftPackChoice {
  code: string;
  name: string;
}

/** The set-backed boosters selected for a local draft. */
export interface DraftSetSelection {
  packs: DraftPackChoice[];
  pools: unknown[];
}

type LegacyDraftStart = {
  (selection: DraftSetSelection, difficulty: number): Promise<void>;
  (setPoolJson: string, setCode: string, setName: string, difficulty: number): Promise<void>;
};

function packSequence(selection: DraftSetSelection): SetPackSequence {
  return {
    pools: selection.pools,
    sequence: selection.packs.map((pack) => pack.code),
  };
}

export type LocalDraftKind = "Quick" | "Sealed";
export type PoolSortMode = "color" | "type" | "cmc";
export type DraftPickDestination = DraftZone;

export interface DraftPickPlacementHint {
  readonly column: number;
  readonly row?: number;
}

export type DraftAutoPickPlacementHints = Readonly<Record<string, DraftPickPlacementHint>>;

export type DraftPickOutcome =
  | { readonly status: "acknowledged" }
  | { readonly status: "rejected"; readonly reason: "adapter" | "invalid-request" | "unacknowledged" }
  | { readonly status: "ignored"; readonly reason: "busy" | "stale" };

export type PendingDraftPickIntent =
  | {
      kind: "pick";
      instanceIds: readonly string[];
      destination: DraftPickDestination;
      placementHint?: DraftPickPlacementHint;
    }
  | {
      kind: "draft-effect";
      instanceIds: readonly string[];
      destination: DraftPickDestination;
      placementHint?: DraftPickPlacementHint;
    }
  | { kind: "auto-pick"; destination: "deck" };

interface DraftStoreState {
  draftId: string | null;
  adapter: DraftAdapter | null;
  /** The engine runtime holding this draft's session; set together with `adapter`. */
  runtime: LocalDraftRuntime | null;
  /** The saved record's declared local FIFO session id, mirrored from its meta. */
  localFifoSessionId: string | null;
  view: DraftPlayerView | null;
  selectedCard: string | null;
  phase: DraftPhase;
  difficulty: number;
  selectedSet: string | null;
  selectedSetName: string | null;
  kind: LocalDraftKind;
  workspaceState: DraftWorkspaceState | null;
  pendingPickIntent: PendingDraftPickIntent | null;
  interactionGeneration: number;
  pickInteractionLocked: boolean;
  poolSortMode: PoolSortMode;
  poolPanelOpen: boolean;
  runFormat: DraftRunFormat;
  runState: DraftRunState | null;
  /**
   * Each LLM drafter's progress on the pack in front of the pod. Empty when no
   * LLM is drafting this step (not opted in, or every pick is forced).
   */
  llmDrafters: readonly LlmDrafterStatus[];
  /** The player has picked and the pod is waiting on LLM drafters to finish. */
  awaitingDrafters: boolean;
}

interface DraftStoreActions {
  startDraft: LegacyDraftStart;
  startSealedDraft: LegacyDraftStart;
  startCubeDraft(cubeListText: string, cubeName: string, settings: CubeDraftSettings, difficulty: number): Promise<void>;
  completeSealedOpening(): void;
  resumeDraft(): Promise<DraftResumeOutcome>;
  abandonDraft(): Promise<void>;
  pickCard(
    cardInstanceId: string,
    destination?: DraftPickDestination,
    placementHint?: DraftPickPlacementHint,
  ): Promise<DraftPickOutcome>;
  confirmPick(
    destination?: DraftPickDestination,
    placementHint?: DraftPickPlacementHint,
  ): Promise<DraftPickOutcome>;
  pickCardWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: readonly string[],
    destination?: DraftPickDestination,
    placementHint?: DraftPickPlacementHint,
  ): Promise<DraftPickOutcome>;
  autoPickCard(destination?: "deck", placementHints?: DraftAutoPickPlacementHints): Promise<DraftPickOutcome>;
  setWorkspaceState(next: DraftWorkspaceState): void;
  setWorkspacePlacement(instanceId: string, placement: DraftCardPlacement): void;
  selectCard(cardInstanceId: string | null): void;
  addBasicLand(name: string): void;
  removeBasicLand(name: string): void;
  autoSuggestDeck(): Promise<void>;
  autoSuggestLands(): Promise<void>;
  submitDeck(): Promise<void>;
  setPoolSortMode(mode: PoolSortMode): void;
  togglePoolPanel(): void;
  setDifficulty(difficulty: number): void;
  setSelectedSet(setCode: string | null): void;
  setRunFormat(format: DraftRunFormat): void;
  launchMatch(navigate: (path: string) => void): Promise<void>;
  recordMatchResult(gameId: string, result: DraftMatchResult): Promise<void>;
  launchNextMatch(navigate: (path: string) => void): Promise<void>;
  endRun(draftId?: string): Promise<void>;
  reset(): void;
}

const initialState: DraftStoreState = {
  draftId: null,
  adapter: null,
  runtime: null,
  localFifoSessionId: null,
  view: null,
  selectedCard: null,
  phase: "setup",
  difficulty: 2,
  selectedSet: null,
  selectedSetName: null,
  kind: "Quick",
  workspaceState: null,
  pendingPickIntent: null,
  interactionGeneration: 0,
  pickInteractionLocked: false,
  poolSortMode: "color",
  poolPanelOpen: true,
  runFormat: "run",
  runState: null,
  llmDrafters: [],
  awaitingDrafters: false,
};

export const DIFFICULTY_NAMES = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"] as const;

function validDifficulty(value: number): boolean {
  return Number.isFinite(value) && Number.isInteger(value)
    && value >= 0 && value < DIFFICULTY_NAMES.length;
}

let lifecycleGeneration = 0;
/**
 * Source of every published `interactionGeneration`: one monotone counter for
 * lifecycle resets and prompt moves alike, so a value is never issued twice.
 * Separate from `lifecycleGeneration`, which gates in-flight operations and
 * must not move when an accepted selection installs a new prompt.
 */
let interactionGenerationCounter = 0;
let workspaceRevision = 0;
let persistenceGeneration = 0;
let suggestionToken = 0;
type ExclusiveKind = "pick" | "submit" | "launch" | "end";

let exclusiveToken: { identity: symbol; kind: ExclusiveKind } | null = null;
let endingRun: { draftId: string; promise: Promise<void> } | null = null;
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

function cancelScheduledPersistence(): void {
  persistenceGeneration += 1;
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = null;
}

function invalidateWorkspaceDependents(): void {
  workspaceRevision += 1;
  suggestionToken += 1;
}

function invalidateLifecycle(): number {
  lifecycleGeneration += 1;
  exclusiveToken = null;
  invalidateWorkspaceDependents();
  cancelScheduledPersistence();
  // Abandoning or replacing a draft must take its LLM work with it. Without
  // this, a provider call started for the old draft runs to its full timeout
  // holding a socket, and its reply lands against a pod that no longer exists.
  cancelLlmDraftRun();
  fifoLlm?.controller.abort();
  fifoLlm = null;
  return lifecycleGeneration;
}

function publishInitialState(generation: number): void {
  useDraftStore.setState((state) => ({
    ...initialState,
    // Bot difficulty is the player's setup choice, not per-draft state. The
    // setup screen stays mounted while a start loads (the card database fetch
    // for Sealed/Hard+ can take seconds), so resetting it here would show the
    // selector snap back to Medium even though the chosen value was already
    // captured and forwarded to the engine.
    difficulty: state.difficulty,
    interactionGeneration: generation,
  }));
}

function nextInteractionGeneration(): number {
  interactionGenerationCounter += 1;
  return interactionGenerationCounter;
}

function beginLifecycle(): number {
  const generation = invalidateLifecycle();
  publishInitialState(nextInteractionGeneration());
  return generation;
}

function admitExclusive(kind: ExclusiveKind): symbol | null {
  if (exclusiveToken) return null;
  const identity = Symbol(kind);
  exclusiveToken = { identity, kind };
  return identity;
}

function isExclusive(identity: symbol, kind?: ExclusiveKind): boolean {
  return exclusiveToken?.identity === identity && (!kind || exclusiveToken.kind === kind);
}

function retireExclusive(identity: symbol): void {
  if (exclusiveToken?.identity === identity) exclusiveToken = null;
}

function endRunOwnsExclusive(): boolean {
  return exclusiveToken?.kind === "end";
}

function workspaceFacades(workspace: DraftWorkspaceState, view: DraftPlayerView) {
  const mainDeck = projectWorkspaceMainDeck(workspace, view.pool);
  const landCounts: Record<string, number> = {};
  for (const card of workspace.virtualBasics) {
    if (workspace.placements[card.instanceId]?.zone !== "deck") continue;
    if (BASIC_LAND_NAMES.has(card.name)) {
      landCounts[card.name] = (landCounts[card.name] ?? 0) + 1;
    } else {
      mainDeck.push(card.name);
    }
  }
  return {
    mainDeck,
    landCounts,
  };
}

function workspaceMutationBlocked(state: Pick<
  DraftStoreState,
  "pickInteractionLocked" | "pendingPickIntent"
>): boolean {
  return exclusiveToken !== null
    || state.pickInteractionLocked
    || state.pendingPickIntent !== null;
}

function schedulePersistence(delay = 500): void {
  if (endRunOwnsExclusive()) return;
  const generation = ++persistenceGeneration;
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = setTimeout(() => {
    debounceTimer = null;
    void persistDraft(generation);
  }, delay);
}

async function persistDraft(
  generation: number,
  options: { propagateFailure?: boolean; canPersist?: () => boolean } = {},
): Promise<void> {
  const state = useDraftStore.getState();
  const { adapter, runtime, draftId, view, workspaceState, selectedSet, phase } = state;
  if (!adapter || !runtime || !draftId || !view || !workspaceState || !selectedSet
    || phase === "setup" || phase === "playing" || phase === "complete"
    || endRunOwnsExclusive()
    || options.canPersist?.() === false) return;
  const lifecycle = lifecycleGeneration;
  const revision = workspaceRevision;
  try {
    const sessionJson = await withDraftEngineOperation((lease) => {
      if (generation !== persistenceGeneration || lifecycle !== lifecycleGeneration
        || revision !== workspaceRevision || useDraftStore.getState().adapter !== adapter) {
        throw new Error("Stale draft persistence request");
      }
      return lease.quickSession(runtime).exportSession();
    });
    if (options.canPersist?.() === false || endRunOwnsExclusive()) return;
    if (generation !== persistenceGeneration || lifecycle !== lifecycleGeneration
      || revision !== workspaceRevision || useDraftStore.getState().adapter !== adapter) {
      if (options.propagateFailure) throw new Error("Stale draft persistence request");
      return;
    }
    await persistQuickDraftSnapshot(draftId, sessionJson, {
      phase,
      ...workspaceFacades(workspaceState, view),
      poolSortMode: state.poolSortMode,
      poolPanelOpen: state.poolPanelOpen,
      workspace: workspaceState,
    }, makeMeta(state, phase));
  } catch (error) {
    if (options.propagateFailure) throw error;
    if (generation === persistenceGeneration && lifecycle === lifecycleGeneration) {
      console.warn("[persistDraft] failed:", error);
    }
  }
}

function makeMeta(state: DraftStoreState, phase: ActiveQuickDraftMeta["phase"], gameId?: string): ActiveQuickDraftMeta {
  return {
    id: state.draftId!,
    setCode: state.selectedSet!,
    setName: state.selectedSetName ?? undefined,
    difficulty: state.difficulty,
    kind: state.kind,
    phase,
    pickCount: state.view?.pool.length ?? 0,
    updatedAt: Date.now(),
    runFormat: state.runFormat,
    runWins: state.runState?.results.filter((entry) => entry.result === "win").length ?? 0,
    runLosses: state.runState?.results.filter((entry) => entry.result === "loss").length ?? 0,
    runDraws: state.runState?.results.filter((entry) => entry.result === "draw").length ?? 0,
    currentGameId: gameId,
    ...(state.localFifoSessionId === null ? {} : { localFifoSessionId: state.localFifoSessionId }),
  };
}

function phaseForView(view: DraftPlayerView, persistedPhase: DraftPhase): DraftPhase {
  // Run phases are owned by the run record, not the draft session's pairing
  // status. A Sealed session sits idle in `Pairing` once its deck is in, so
  // resuming BETWEEN run games (or after the last game) must keep the run
  // phase (`playing` → BetweenMatches, `complete` → RunComplete) — mapping
  // `Pairing` to `launching` there bounces the player back to the format
  // picker mid-run.
  if (persistedPhase === "playing" || persistedPhase === "complete") return persistedPhase;
  if (view.status === "Deckbuilding") {
    return view.kind === "Sealed" && persistedPhase === "opening" ? "opening" : "deckbuilding";
  }
  return view.status === "Pairing" ? "launching" : persistedPhase;
}

/** The run phase the durable record dictates: `complete` once the run hits its
 * win/loss limits, otherwise `playing`. Single authority for run terminality —
 * recordMatchResult's meta, launchNextMatch's guard, and resumeDraft all read
 * this. The run is authoritative over the persisted metadata: the run and the
 * meta are written as separate operations (publishInitialDraftMatch /
 * recordDraftMatchResult save the run before the meta), so a crash between
 * them leaves stale metadata that resume must not trust. Mirrors CR-adjacent
 * ladder semantics (7 wins / 3 losses) owned by `runLimits` in
 * quickDraftPersistence. */
function draftRunPhase(run: DraftRunState): "playing" | "complete" {
  const limits = runLimits(run.format);
  const wins = run.results.filter((entry) => entry.result === "win").length;
  const losses = run.results.filter((entry) => entry.result === "loss").length;
  return wins >= limits.maxWins || losses >= limits.maxLosses ? "complete" : "playing";
}

type WorkspaceInstallPatch = Partial<Omit<
  DraftStoreState,
  | "view"
  | "workspaceState"
  | "interactionGeneration"
>>;

type WorkspaceInstallOperation =
  | {
      readonly kind: "state";
      readonly authoritativeView: DraftPlayerView;
      readonly baseWorkspace: DraftWorkspaceState;
      readonly patch: WorkspaceInstallPatch;
      readonly persistence: "schedule" | "skip";
    }
  | {
      readonly kind: "acknowledged-pick";
      readonly authoritativeView: DraftPlayerView;
      readonly baseWorkspace: DraftWorkspaceState;
      readonly placeInstanceIds: readonly string[];
      readonly destination: DraftPickDestination;
      readonly placementHint?: DraftPickPlacementHint;
      readonly patch: WorkspaceInstallPatch;
      readonly persistence: "schedule";
    }
  | {
      readonly kind: "acknowledged-auto-pick";
      readonly authoritativeView: DraftPlayerView;
      readonly baseWorkspace: DraftWorkspaceState;
      readonly addedInstanceIds: readonly string[];
      readonly placementHints?: DraftAutoPickPlacementHints;
      readonly patch: WorkspaceInstallPatch;
      readonly persistence: "schedule";
    };

/**
 * Ids this operation resolves a placement for ITSELF, which the arriving pass
 * must leave alone.
 *
 * A `sideboard` destination, because the pass is deck-only: the card still
 * carries reconcile's `"deck"` default when the pass runs, so the pass would
 * stamp a deck-geometry column that `applyDestination` then carries into the
 * sideboard, to be clamped by `normalizeWorkspaceForBoardGeometry` to that
 * zone's last column once it overflows the narrower sideboard.
 *
 * A `placementHint`, because `applyDestination` falls back per FIELD:
 * `placementHint?.row ?? placement.row`. `DraftPickPlacementHint.row` is
 * optional, and `useDraftWorkspaceDrag` omits it whenever the drop hit a column
 * but no row band. On a two-row board the pass would then decide that card's
 * row through the engine classification, where the hint path has always fallen
 * back to reconcile's default — a drag-behaviour change this change has no
 * business making. That card's own COLUMN is unaffected either way, since a
 * hint always wins there — `row` is the whole of what this arm protects.
 *
 * `acknowledged-auto-pick` installs to `"deck"` unconditionally below, so only
 * its hint can exclude it.
 */
function operationResolvesOwnPlacement(operation: WorkspaceInstallOperation): readonly string[] {
  switch (operation.kind) {
    case "state":
      return [];
    case "acknowledged-pick":
      return operation.placementHint !== undefined || operation.destination !== "deck"
        ? operation.placeInstanceIds
        : [];
    case "acknowledged-auto-pick":
      return operation.addedInstanceIds.filter(
        (instanceId) => operation.placementHints?.[instanceId] !== undefined,
      );
  }
}

function installWorkspace(operation: WorkspaceInstallOperation): void {
  const ownPlacement = operationResolvesOwnPlacement(operation);
  // Against `operation.baseWorkspace`, the PRE-reconcile workspace, so a card
  // that entered the pool on this install still counts as arriving. Asked after
  // the reconcile below it would already hold the column-0 default and be
  // filtered out, which is why the id list is computed here and not inside the
  // placement call.
  const arriving = unplacedPoolIds(operation.baseWorkspace, operation.authoritativeView.pool)
    .filter((instanceId) => !ownPlacement.includes(instanceId));
  // Sorted placement for cards that reach the pool with no hint resolved for
  // them: the `kind: "state"` installs `startLocalDraft` and `resumeDraft` make,
  // plus the hint-less deck picks `PackDisplay.request` dispatches through
  // `pickCard`, `pickCardStep` and `pickCardWithDraftEffect`. Without this they
  // stack in the board's first column whatever the sort says.
  // BEFORE the switch, and the order is load-bearing — do not move this below
  // it. For a multi-id hint-less DECK pick (`pickCardWithDraftEffect` from
  // `PackDisplay.request`) both calls write the same two ids' placements: this
  // pass appends them in POOL order, `applyDestination` appends them in REQUEST
  // order and re-appends an id it finds already placed (`if (!placement)
  // continue` is its only skip). Whichever runs last decides the stack order.
  // Pinned by `appends_a_hint_less_deck_draft_effect_pick_in_request_order`,
  // which was the single placement failure of a full `npx vitest run` with this
  // call moved below the switch — it failed there on `second.order`, expecting
  // 0 and getting 1, the pool-order result.
  let workspace = placeArrivingPoolCards(
    reconcileWorkspaceState(operation.baseWorkspace, operation.authoritativeView.pool),
    arriving,
    operation.authoritativeView.pool,
    operation.authoritativeView.pool_groups,
    getArrivingCardBoardPreferences(),
  );
  switch (operation.kind) {
    case "state":
      break;
    case "acknowledged-pick":
      workspace = applyDestination(
        workspace,
        operation.authoritativeView.pool,
        operation.placeInstanceIds,
        operation.destination,
        operation.placementHint,
      );
      break;
    case "acknowledged-auto-pick":
      for (const instanceId of operation.addedInstanceIds) {
        workspace = applyDestination(
          workspace,
          operation.authoritativeView.pool,
          [instanceId],
          "deck",
          operation.placementHints?.[instanceId],
        );
      }
      break;
  }
  invalidateWorkspaceDependents();
  // The interaction generation follows the engine's prompt, never card ids: a
  // FIFO view's new prompt is a new interaction even when its head shows the
  // same cards, and a legacy view carries no prompt. Published in the same
  // update as the view, so an observer sees the unlock and the move together.
  const previous = useDraftStore.getState().view;
  const promptMoved = !sameDraftPrompt(
    previous === null ? null : draftViewPrompt(previous),
    draftViewPrompt(operation.authoritativeView),
  );
  useDraftStore.setState({
    ...operation.patch,
    view: operation.authoritativeView,
    workspaceState: workspace,
    ...(promptMoved ? { interactionGeneration: nextInteractionGeneration() } : {}),
  });
  if (operation.persistence === "schedule") schedulePersistence(0);
}

async function prepareCardDatabase(): Promise<string> {
  const response = await fetch(__CARD_DATA_URL__);
  return response.text();
}

async function startLocalDraft(input: {
  setCode: string;
  setName: string;
  difficulty: number;
  kind: LocalDraftKind;
  initialize: Parameters<typeof withDraftEngineOperation<DraftPlayerView>>[0];
}): Promise<void> {
  const lifecycle = beginLifecycle();
  try {
    await Promise.all([drainDraftEngineOperations(), drainQuickDraftPersistence()]);
    if (lifecycle !== lifecycleGeneration) return;
    await inspectActiveQuickDraftLifecycle("consume");
    if (lifecycle !== lifecycleGeneration) return;
    const database = await prepareCardDatabase();
    const adapter = new DraftAdapter();
    const view = await withDraftEngineOperation((lease) => {
      if (lifecycle !== lifecycleGeneration) throw new Error("Stale draft start");
      lease.loadCardDatabase(database);
      lease.setPickEnhancements(usePreferencesStore.getState().draftPickEnhancements);
      if (lifecycle !== lifecycleGeneration) throw new Error("Stale draft start");
      return input.initialize(lease);
    });
    if (lifecycle !== lifecycleGeneration) return;
    installWorkspace({
      kind: "state",
      authoritativeView: view,
      baseWorkspace: createDraftWorkspaceState(),
      patch: {
        draftId: crypto.randomUUID(),
        adapter,
        runtime: { kind: "legacy" },
        localFifoSessionId: null,
        phase: input.kind === "Sealed" ? "opening" : "drafting",
        difficulty: input.difficulty,
        selectedSet: input.setCode,
        selectedSetName: input.setName,
        kind: input.kind,
        selectedCard: null,
        pendingPickIntent: null,
        pickInteractionLocked: false,
        runFormat: input.kind === "Sealed" ? "single" : "run",
        runState: null,
      },
      persistence: "schedule",
    });
  } catch (error) {
    if (lifecycle !== lifecycleGeneration) return;
    throw error;
  }
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

type PickRequest =
  | {
      readonly kind: "pick";
      readonly instanceId: string;
      readonly destination: DraftPickDestination;
      readonly placementHint?: DraftPickPlacementHint;
    }
  | {
      readonly kind: "draft-effect";
      readonly effectCardInstanceId: string;
      readonly instanceIds: readonly string[];
      readonly destination: DraftPickDestination;
      readonly placementHint?: DraftPickPlacementHint;
    }
  | {
      readonly kind: "auto-pick";
      readonly destination: "deck";
      readonly placementHints?: DraftAutoPickPlacementHints;
    };

function validDestination(destination: unknown): destination is DraftPickDestination {
  return destination === "deck" || destination === "sideboard";
}

function validPlacementHint(placementHint: unknown): placementHint is DraftPickPlacementHint | undefined {
  return placementHint === undefined
    || (typeof placementHint === "object"
      && placementHint !== null
      && "column" in placementHint
      && typeof placementHint.column === "number"
      && Number.isFinite(placementHint.column)
      && Number.isInteger(placementHint.column)
      && placementHint.column >= 0
      && (!("row" in placementHint)
        || (typeof placementHint.row === "number"
          && Number.isInteger(placementHint.row)
          && (placementHint.row === 0 || placementHint.row === 1))));
}

function validAutoPickPlacementHints(placementHints: unknown): placementHints is DraftAutoPickPlacementHints | undefined {
  return placementHints === undefined
    || (typeof placementHints === "object"
      && placementHints !== null
      && !Array.isArray(placementHints)
      && Object.entries(placementHints).every(([instanceId, hint]) => (
        instanceId.length > 0 && hint !== undefined && validPlacementHint(hint)
      )));
}

function validPickRequest(request: PickRequest): boolean {
  if (!validDestination(request.destination)) return false;
  switch (request.kind) {
    case "pick":
      return typeof request.instanceId === "string"
        && request.instanceId.length > 0
        && validPlacementHint(request.placementHint);
    case "draft-effect": {
      if (!Array.isArray(request.instanceIds)
        || typeof request.effectCardInstanceId !== "string"
        || request.effectCardInstanceId.length === 0
        || !validPlacementHint(request.placementHint)) return false;
      // The selection's size is the view's count for this effect card; a FIFO
      // view that publishes none for it offers no such selection.
      const view = useDraftStore.getState().view;
      const count = view === null ? null : draftSelectionPickCount(view, request.effectCardInstanceId);
      return count !== null
        && request.instanceIds.length === count
        && request.instanceIds.every((instanceId) => typeof instanceId === "string"
          && instanceId.length > 0
          && instanceId !== request.effectCardInstanceId)
        && new Set(request.instanceIds).size === request.instanceIds.length;
    }
    case "auto-pick":
      return request.destination === "deck" && validAutoPickPlacementHints(request.placementHints);
  }
}

function poolMultiplicity(pool: DraftPlayerView["pool"]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const card of pool) counts.set(card.instance_id, (counts.get(card.instance_id) ?? 0) + 1);
  return counts;
}

function acknowledgesRequestedId(
  before: ReadonlyMap<string, number>,
  after: ReadonlyMap<string, number>,
  instanceId: string,
): boolean {
  return (before.get(instanceId) ?? 0) === 0 && (after.get(instanceId) ?? 0) === 1;
}

/**
 * The ids the pool gained, each going from absent to exactly one copy; `null`
 * when any id changed in another way.
 */
function acknowledgedAddedIds(
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

/**
 * Where an admitted selection goes: the legacy runtime, or the local FIFO
 * runtime's owner and the prompt the selection was composed on.
 */
type PickEngineTarget =
  | { readonly kind: "legacy" }
  | { readonly kind: "local-fifo"; readonly owner: LocalFifoOwner; readonly prompt: FifoPromptId };

/** `null` when a FIFO view publishes no prompt: there is nothing to select on. */
function pickEngineTarget(runtime: LocalDraftRuntime, view: DraftPlayerView): PickEngineTarget | null {
  switch (runtime.kind) {
    case "legacy":
      return { kind: "legacy" };
    case "local-fifo": {
      const prompt = draftViewPrompt(view);
      return prompt === null ? null : { kind: "local-fifo", owner: runtime.owner, prompt };
    }
  }
}

/**
 * One admitted selection through the local FIFO exports, bound to the prompt
 * it was composed on. The time is read here, immediately before the export.
 */
function submitLocalFifoSelection(
  lease: DraftEngineOperationLease,
  target: Extract<PickEngineTarget, { readonly kind: "local-fifo" }>,
  request: PickRequest,
): DraftPlayerView {
  const nowMs = Date.now();
  switch (request.kind) {
    case "pick":
      return lease.localFifoSelect(target.owner, {
        prompt_id: target.prompt,
        picked_card_instance_ids: [request.instanceId],
        burned_card_instance_ids: [],
        effect_card_instance_ids: [],
      }, nowMs);
    case "draft-effect":
      return lease.localFifoSelect(target.owner, {
        prompt_id: target.prompt,
        picked_card_instance_ids: [...request.instanceIds],
        burned_card_instance_ids: [],
        effect_card_instance_ids: [request.effectCardInstanceId],
      }, nowMs);
    case "auto-pick":
      return lease.localFifoAutoSelect(target.owner, target.prompt, nowMs);
  }
}

function pendingIntentFor(request: PickRequest): PendingDraftPickIntent {
  switch (request.kind) {
    case "pick":
      return {
        kind: "pick",
        instanceIds: [request.instanceId],
        destination: request.destination,
        placementHint: request.placementHint,
      };
    case "draft-effect":
      return {
        kind: "draft-effect",
        instanceIds: request.instanceIds,
        destination: request.destination,
        placementHint: request.placementHint,
      };
    case "auto-pick":
      return { kind: "auto-pick", destination: "deck" };
  }
}

/**
 * The LLM pick round for the pick step a view shows.
 *
 * Keyed by the VIEW OBJECT, not by pack and pick numbers: a new view is
 * installed for every step, so a round can never outlive the step it was
 * started for, and its replies are submitted at most once.
 */
let llmRound: {
  view: DraftPlayerView;
  lifecycle: number;
  round: Promise<LlmDraftRound | null>;
} | null = null;

/**
 * The round of LLM picks for `view`, started on first request.
 *
 * Called when a pack opens, so the pod's LLM drafters think while the player
 * does, and again when the player picks, which then only waits on whatever is
 * still in flight. LLM drafters are opt-in twice over: a profile must be
 * configured AND drafting must be switched on for it. Anything else — including
 * a pod with no bot seats — yields no round, and the pick takes the ordinary engine-bot path.
 */
function ensureLlmDraftRound(view: DraftPlayerView): Promise<LlmDraftRound | null> {
  // Not remembered while drafting is off, so switching it on mid-step starts
  // this step's round when the player picks.
  if (!useLlmStore.getState().draftEnabled) return Promise.resolve(null);
  if (llmRound && llmRound.view === view && llmRound.lifecycle === lifecycleGeneration) {
    return llmRound.round;
  }
  const lifecycle = lifecycleGeneration;
  const isCurrent = (): boolean =>
    lifecycle === lifecycleGeneration && useDraftStore.getState().view === view;
  const round = (async (): Promise<LlmDraftRound | null> => {
    const catalog = await loadProviderCatalog();
    if (!isCurrent()) return null;
    const profile = draftProfile(useLlmStore.getState(), catalog);
    if (!profile) return null;
    return startLlmDraftRound(profile, isCurrent, (llmDrafters) => {
      useDraftStore.setState({ llmDrafters });
    });
  })();
  llmRound = { view, lifecycle, round };
  return round;
}

/** The shape of `fifoLlm`. */
interface LocalFifoLlm {
  readonly lifecycle: number;
  readonly owner: LocalFifoOwner;
  /** What the engine was last told. */
  drafting: LocalFifoLlmDrafting;
  /**
   * `"unavailable"` once requests could not be built: the bot seats are handed
   * to the engine until the player's next accepted selection.
   */
  availability: "available" | "unavailable";
  /** Cuts every call in flight loose; replaced when the engine bots take over. */
  controller: AbortController;
  readonly inFlight: Map<number, FifoPromptId>;
  readonly statuses: Map<number, LlmDrafterState>;
  /** A view the engine answered while the player's selection was pending. */
  viewOwed: boolean;
  /** One sync runs at a time; one asked for meanwhile runs once more after it. */
  syncing: boolean;
  syncAgain: boolean;
}

/** LLM drafting on the local FIFO runtime for one lifecycle: what the engine was told, and each seat's call in flight with the prompt it answers. */
let fifoLlm: LocalFifoLlm | null = null;

/** Whether the bot seats should be LLM drafters, and through which profile. */
type LocalFifoLlmDesire =
  | { readonly drafting: "EngineBots" }
  | { readonly drafting: "LlmSeats"; readonly profile: LlmProfile };

/**
 * LLM drafting is opt-in twice over, as for the barrier round: switched on,
 * and a usable profile configured. With it switched off no catalog is loaded.
 */
async function localFifoLlmDrafting(): Promise<LocalFifoLlmDesire> {
  if (!useLlmStore.getState().draftEnabled) return { drafting: "EngineBots" };
  const profile = draftProfile(useLlmStore.getState(), await loadProviderCatalog());
  return profile ? { drafting: "LlmSeats", profile } : { drafting: "EngineBots" };
}

/** Whether `llm` still belongs to the local draft the store holds. */
function localFifoLlmOwnsDraft(llm: LocalFifoLlm): boolean {
  const runtime = useDraftStore.getState().runtime;
  return fifoLlm === llm
    && llm.lifecycle === lifecycleGeneration
    && runtime?.kind === "local-fifo"
    && runtime.owner === llm.owner;
}

/**
 * Each drafter's status, by seat. The labels keep their meaning: picking while
 * a call is in flight, picked when the engine used the reply, engine pick when
 * the seat fell back. The player never waits on a drafter on this runtime, so
 * `awaitingDrafters` is never set here.
 */
function publishLocalFifoLlmStatuses(llm: LocalFifoLlm): void {
  if (!localFifoLlmOwnsDraft(llm)) return;
  const llmDrafters = [...llm.statuses]
    .sort(([left], [right]) => left - right)
    .map(([seat, state]) => ({ seat, state }));
  useDraftStore.setState({ llmDrafters });
}

/**
 * Install a view the engine answered outside the player's own selection. While
 * that selection is pending, installing would make it stale, so the latest
 * view is owed instead and fetched once the selection ends.
 */
function installLocalFifoView(llm: LocalFifoLlm, view: DraftPlayerView): void {
  if (!localFifoLlmOwnsDraft(llm)) return;
  if (exclusiveToken !== null) {
    llm.viewOwed = true;
    return;
  }
  const state = useDraftStore.getState();
  if (state.workspaceState === null) return;
  llm.viewOwed = false;
  installWorkspace({
    kind: "state",
    authoritativeView: view,
    baseWorkspace: state.workspaceState,
    patch: { phase: view.status === "Deckbuilding" ? "deckbuilding" : state.phase },
    persistence: "schedule",
  });
}

/**
 * Tell the engine whether the bot seats are LLM drafters, under a lease of its
 * own; the engine runs the bots the declaration leaves to it. Handing the
 * seats to the engine bots first cuts every call loose, so no late reply
 * reaches the engine. `false` when the declaration did not land.
 */
async function declareLocalFifoLlmDrafting(
  llm: LocalFifoLlm,
  drafting: LocalFifoLlmDrafting,
): Promise<boolean> {
  if (drafting === "EngineBots") {
    llm.controller.abort();
    llm.controller = new AbortController();
    llm.inFlight.clear();
    llm.statuses.clear();
    publishLocalFifoLlmStatuses(llm);
  }
  let view: DraftPlayerView;
  try {
    view = await withDraftEngineOperation((lease) => {
      if (!localFifoLlmOwnsDraft(llm)) throw new Error("Stale LLM drafting declaration");
      return lease.localFifoSetLlmDrafting(llm.owner, drafting, Date.now());
    });
  } catch (error) {
    if (localFifoLlmOwnsDraft(llm)) console.warn("[draftStore] LLM drafting declaration failed:", error);
    return false;
  }
  if (!localFifoLlmOwnsDraft(llm)) return false;
  llm.drafting = drafting;
  installLocalFifoView(llm, view);
  return true;
}

/**
 * One pass of `syncLocalFifoLlm`: install an owed view, declare the drafting
 * the player's settings now ask for when it changed, then build the requests
 * for every held head under one lease and start a call for each head no call
 * is answering yet.
 */
async function syncLocalFifoLlmOnce(llm: LocalFifoLlm): Promise<void> {
  const current = (): boolean => localFifoLlmOwnsDraft(llm)
    && useDraftStore.getState().view?.status === "Drafting";
  if (!current()) return;
  if (llm.viewOwed && exclusiveToken === null) {
    const view = await withDraftEngineOperation((lease) => {
      if (!localFifoLlmOwnsDraft(llm)) throw new Error("Stale local draft view");
      return lease.localFifoView(llm.owner);
    });
    installLocalFifoView(llm, view);
    if (!current()) return;
  }
  const desired: LocalFifoLlmDesire = llm.availability === "unavailable"
    ? { drafting: "EngineBots" }
    : await localFifoLlmDrafting();
  if (!current()) return;
  if (desired.drafting !== llm.drafting && !(await declareLocalFifoLlmDrafting(llm, desired.drafting))) return;
  if (desired.drafting !== "LlmSeats" || !current()) return;
  let entries: LocalFifoLlmPickRequest[];
  try {
    entries = await buildLocalFifoLlmRequests(llm.owner, desired.profile);
  } catch (error) {
    if (!current()) return;
    // Once, with no loop: the bot seats go to the engine until the player's
    // next accepted selection makes the drafters available again.
    reportLlmFailure("LLM drafters unavailable; using the engine bots", error);
    llm.availability = "unavailable";
    await declareLocalFifoLlmDrafting(llm, "EngineBots");
    return;
  }
  if (!current() || llm.drafting !== "LlmSeats") return;
  for (const entry of entries) {
    if (sameDraftPrompt(llm.inFlight.get(entry.seat) ?? null, entry.promptId)) continue;
    llm.inFlight.set(entry.seat, entry.promptId);
    llm.statuses.set(entry.seat, "picking");
    void resolveLocalFifoLlmSeat(llm, desired.profile, entry);
  }
  publishLocalFifoLlmStatuses(llm);
}

/**
 * Keep the local FIFO runtime's LLM seats in step with the store. Runs after
 * every view change, every player selection and every resolution; one sync
 * runs at a time, and one asked for while it runs runs once more after it.
 */
function syncLocalFifoLlm(): void {
  const llm = fifoLlm;
  if (llm === null) return;
  if (llm.syncing) {
    llm.syncAgain = true;
    return;
  }
  llm.syncing = true;
  void (async () => {
    try {
      do {
        llm.syncAgain = false;
        try {
          await syncLocalFifoLlmOnce(llm);
        } catch (error) {
          if (localFifoLlmOwnsDraft(llm)) console.warn("[draftStore] LLM seat sync failed:", error);
        }
      } while (llm.syncAgain && localFifoLlmOwnsDraft(llm));
    } finally {
      llm.syncing = false;
    }
  })();
}

/**
 * One held head's drafter: the provider call with no lease held, then the
 * reply handed to the engine under a lease of its own. The engine alone
 * decides whether the reply is used or the seat falls back to the engine bot.
 * A reply whose draft, call or prompt has been superseded is dropped unsent.
 */
async function resolveLocalFifoLlmSeat(
  llm: LocalFifoLlm,
  profile: LlmProfile,
  entry: LocalFifoLlmPickRequest,
): Promise<void> {
  const signal = llm.controller.signal;
  const stale = (): boolean => !localFifoLlmOwnsDraft(llm)
    || signal.aborted
    || !sameDraftPrompt(llm.inFlight.get(entry.seat) ?? null, entry.promptId);
  let reply: { status: number; body: string } | null = null;
  switch (entry.kind) {
    case "request":
      reply = await callLlmDrafter(entry, signal);
      break;
    case "unbuildable":
      // The engine's reason goes to the console only; the game log names the seat.
      if (!stale()) reportLlmFailure(`LLM drafter (seat ${entry.seat}) unavailable`, entry.error);
      break;
  }
  if (stale()) return;
  let resolved: { view: DraftPlayerView; llmOutcome: LlmDraftOutcome };
  try {
    resolved = await withDraftEngineOperation((lease) => {
      if (stale()) throw new Error("Stale LLM drafter reply");
      return lease.localFifoResolveLlmPick(llm.owner, {
        promptId: entry.promptId,
        reply: entry.kind === "request" && reply !== null
          ? { fingerprint: entry.fingerprint, provider: profile.provider, status: reply.status, body: reply.body }
          : null,
      }, Date.now());
    });
  } catch (error) {
    if (stale()) return;
    llm.inFlight.delete(entry.seat);
    llm.statuses.delete(entry.seat);
    publishLocalFifoLlmStatuses(llm);
    reportLlmFailure(`LLM drafter (seat ${entry.seat}) failed`, error);
    syncLocalFifoLlm();
    return;
  }
  if (stale()) return;
  const { view, llmOutcome } = resolved;
  reportLlmDraftOutcomes([llmOutcome]);
  recordLlmDraftSeatUsage(llmOutcome, entry.kind === "request" ? entry.promptChars : undefined);
  llm.inFlight.delete(entry.seat);
  llm.statuses.set(entry.seat, llmOutcome.used ? "ready" : "fallback");
  publishLocalFifoLlmStatuses(llm);
  installLocalFifoView(llm, view);
  syncLocalFifoLlm();
}

/**
 * One player selection. On the local FIFO runtime the LLM seats sync after it:
 * an accepted selection makes drafters available again after requests could
 * not be built, and a view owed while the selection was pending is installed.
 */
async function performPick(request: PickRequest): Promise<DraftPickOutcome> {
  const outcome = await performPickOnce(request);
  const llm = fifoLlm;
  if (llm !== null && localFifoLlmOwnsDraft(llm)) {
    if (outcome.status === "acknowledged") llm.availability = "available";
    syncLocalFifoLlm();
  }
  return outcome;
}

async function performPickOnce(request: PickRequest): Promise<DraftPickOutcome> {
  if (!validPickRequest(request)) return { status: "rejected", reason: "invalid-request" };
  if (request.kind === "draft-effect") {
    request = {
      ...request,
      instanceIds: [...request.instanceIds],
    };
  }
  const token = admitExclusive("pick");
  if (!token) return { status: "ignored", reason: "busy" };
  const state = useDraftStore.getState();
  const { adapter, runtime, draftId, workspaceState, view } = state;
  if (!adapter || !runtime || !draftId || !workspaceState || !view) {
    retireExclusive(token);
    return { status: "rejected", reason: "invalid-request" };
  }
  if (draftViewPresentation(view).isReadOnly) {
    retireExclusive(token);
    return { status: "rejected", reason: "invalid-request" };
  }
  const target = pickEngineTarget(runtime, view);
  if (target === null) {
    retireExclusive(token);
    return { status: "rejected", reason: "invalid-request" };
  }
  const lifecycle = lifecycleGeneration;
  const intent = pendingIntentFor(request);
  const before = poolMultiplicity(view.pool);
  suggestionToken += 1;
  useDraftStore.setState({ pendingPickIntent: intent, pickInteractionLocked: true });
  const isFresh = (): boolean => {
    const current = useDraftStore.getState();
    return lifecycle === lifecycleGeneration
      && current.adapter === adapter
      && current.draftId === draftId
      && current.view === view
      && current.workspaceState === workspaceState
      && current.pendingPickIntent === intent
      && current.pickInteractionLocked
      && isExclusive(token, "pick");
  };
  const cleanup = (): void => {
    retireExclusive(token);
    useDraftStore.setState({ pendingPickIntent: null, pickInteractionLocked: false });
  };
  try {
    // The LLM round was started when this pack opened (see the subscription
    // below the store); if it was not — LLM drafting switched on mid-step —
    // this starts it. `draftEnabled` is read synchronously first so a draft
    // with no LLM drafting takes the ordinary path without yielding.
    //
    // The round's requests are built under a lease of their own and its
    // provider I/O runs with none held, so the singleton draft engine queue is
    // never blocked across a network round trip. Each reply carries the pack
    // fingerprint it was built from and the engine re-validates it against the
    // live pack below, so a pack that moved on is refused per seat rather than
    // mis-picked.
    //
    // The barrier round serves the legacy runtime; a local FIFO draft's LLM
    // seats answer their own heads (see `syncLocalFifoLlm`). The round's
    // exports read the legacy session, which a local FIFO session never is.
    const llmRoundForPick = target.kind === "legacy" && request.kind === "pick"
      && useLlmStore.getState().draftEnabled
      ? await ensureLlmDraftRound(view)
      : null;
    if (!isFresh()) {
      return { status: "ignored", reason: "stale" };
    }
    let llmResponses: LlmDraftResponsePayload[] = [];
    if (llmRoundForPick) {
      // The player's pick is accepted now; the pack does not pass until every
      // LLM drafter has picked too (or fallen back to the engine bot).
      if (!llmRoundForPick.settled) useDraftStore.setState({ awaitingDrafters: true });
      llmResponses = await llmRoundForPick.responses;
      // Spent: replies are submitted at most once. A pick the engine does not
      // acknowledge leaves the same view in place, and the next pick from it
      // starts a fresh round rather than replaying these replies.
      if (llmRound?.view === view) llmRound = null;
      if (useDraftStore.getState().awaitingDrafters) useDraftStore.setState({ awaitingDrafters: false });
    }
    if (!isFresh()) {
      // The pick was superseded while the providers were answering. Cut the
      // round loose rather than letting it run to its timeout holding sockets.
      cancelLlmDraftRun();
      return { status: "ignored", reason: "stale" };
    }

    const nextView = await withDraftEngineOperation((lease) => {
      if (!isFresh()) {
        throw new Error("Stale draft pick request");
      }
      switch (target.kind) {
        case "local-fifo":
          return submitLocalFifoSelection(lease, target, request);
        case "legacy":
          switch (request.kind) {
            case "pick": {
              if (llmResponses.length > 0 && llmRoundForPick) {
                const outcome = lease.submitPickWithLlmBotPicks(request.instanceId, llmResponses);
                reportLlmDraftOutcomes(outcome.llmOutcomes);
                recordLlmDraftUsage(llmRoundForPick, outcome.llmOutcomes);
                return outcome.view;
              }
              return lease.submitPick(request.instanceId);
            }
            case "draft-effect": {
              const adapterInstanceIds = [...request.instanceIds];
              return lease.submitPickWithDraftEffect(request.effectCardInstanceId, adapterInstanceIds);
            }
            case "auto-pick":
              return lease.autoPick();
          }
      }
    });
    if (!isFresh()) return { status: "ignored", reason: "stale" };
    const after = poolMultiplicity(nextView.pool);
    let operation: WorkspaceInstallOperation;
    switch (request.kind) {
      case "pick":
        if (!acknowledgesRequestedId(before, after, request.instanceId)) {
          cleanup();
          return { status: "rejected", reason: "unacknowledged" };
        }
        operation = {
          kind: "acknowledged-pick",
          authoritativeView: nextView,
          baseWorkspace: workspaceState,
          placeInstanceIds: [request.instanceId],
          destination: request.destination,
          placementHint: request.placementHint,
          patch: {
            phase: nextView.status === "Deckbuilding" ? "deckbuilding" : "drafting",
            selectedCard: null,
            pendingPickIntent: null,
            pickInteractionLocked: false,
          },
          persistence: "schedule",
        };
        break;
      case "draft-effect":
        if (!request.instanceIds.every((instanceId) => acknowledgesRequestedId(before, after, instanceId))) {
          cleanup();
          return { status: "rejected", reason: "unacknowledged" };
        }
        operation = {
          kind: "acknowledged-pick",
          authoritativeView: nextView,
          baseWorkspace: workspaceState,
          placeInstanceIds: request.instanceIds,
          destination: request.destination,
          placementHint: request.placementHint,
          patch: {
            phase: nextView.status === "Deckbuilding" ? "deckbuilding" : "drafting",
            selectedCard: null,
            pendingPickIntent: null,
            pickInteractionLocked: false,
          },
          persistence: "schedule",
        };
        break;
      case "auto-pick": {
        // Acknowledged when the pool gains exactly the selection's published
        // count of new cards: one for every legacy Quick view.
        const addedInstanceIds = acknowledgedAddedIds(before, after);
        if (addedInstanceIds === null || addedInstanceIds.length !== draftSelectionPickCount(view, null)) {
          cleanup();
          return { status: "rejected", reason: "unacknowledged" };
        }
        operation = {
          kind: "acknowledged-auto-pick",
          authoritativeView: nextView,
          baseWorkspace: workspaceState,
          addedInstanceIds,
          placementHints: request.placementHints,
          patch: {
            phase: nextView.status === "Deckbuilding" ? "deckbuilding" : "drafting",
            selectedCard: null,
            pendingPickIntent: null,
            pickInteractionLocked: false,
          },
          persistence: "schedule",
        };
        break;
      }
    }
    retireExclusive(token);
    installWorkspace(operation);
    return { status: "acknowledged" };
  } catch {
    if (!isFresh()) return { status: "ignored", reason: "stale" };
    cleanup();
    return { status: "rejected", reason: "adapter" };
  }
}

function replaceDeckVirtualBasics(
  workspace: DraftWorkspaceState,
  pool: DraftPlayerView["pool"],
  counts: Readonly<Record<string, unknown>>,
  removeSideboard: boolean,
  preserveCustom: boolean,
): DraftWorkspaceState {
  let next = workspace;
  for (const basic of workspace.virtualBasics) {
    if (preserveCustom && !BASIC_LAND_NAMES.has(basic.name)) continue;
    if (removeSideboard || workspace.placements[basic.instanceId]?.zone === "deck") {
      next = removeVirtualBasic(next, basic.instanceId);
    }
  }
  let remaining = MAX_MATERIALIZED_VIRTUAL_BASICS - next.virtualBasics.length;
  for (const name of Object.keys(counts).sort()) {
    const count = Math.min(normalizeVirtualBasicCount(counts[name]), remaining);
    for (let index = 0; index < count; index += 1) {
      const instanceId = makeInteractiveVirtualBasicInstanceId(next, pool);
      next = addVirtualBasic(next, pool, { instanceId, name });
    }
    remaining -= count;
    if (remaining === 0) break;
  }
  return next;
}

function arraysEqual(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function validPersistedStage(run: DraftRunState, draftId: string): boolean {
  return run.activeMatch === undefined
    || (typeof run.activeMatch === "object" && run.activeMatch !== null
      && isCoherentUnresolvedDraftStage(run, draftId, run.activeMatch.gameId));
}

function validRunAuthority(run: DraftRunState): boolean {
  return (run.draft_set_codes === undefined || (Array.isArray(run.draft_set_codes)
      && run.draft_set_codes.every((code) => typeof code === "string")))
    && (run.lastOpponentSeat === undefined || (Number.isInteger(run.lastOpponentSeat)
      && run.lastOpponentSeat > 0 && Array.isArray(run.usedBotSeats)
      && run.usedBotSeats.includes(run.lastOpponentSeat)));
}

function validRunFields(run: DraftRunState, draftId: string): boolean {
  return (run.format === "single" || run.format === "bo3" || run.format === "run")
    && Array.isArray(run.results)
    && run.results.every((entry) => typeof entry?.gameId === "string"
      && (entry.result === "win" || entry.result === "loss" || entry.result === "draw"))
    && Array.isArray(run.playerDeck) && run.playerDeck.length > 0
    && run.playerDeck.every((card) => typeof card === "string")
    && Array.isArray(run.opponentDeck) && run.opponentDeck.length > 0
    && run.opponentDeck.every((card) => typeof card === "string")
    && Array.isArray(run.usedBotSeats)
    && run.usedBotSeats.length > 0
    && run.usedBotSeats.every((seat) => Number.isInteger(seat) && seat > 0)
    && validRunAuthority(run)
    && validPersistedStage(run, draftId);
}

function validRun(run: DraftRunState, draftId: string, setCode: string): boolean {
  return validRunFields(run, draftId)
    && (setCode !== "custom-cube" || Array.isArray(run.booster_pack_pool));
}

/** Transport identity for a submitted, unresolved match. The run is the authority. */
export function isCoherentUnresolvedDraftStage(run: DraftRunState, draftId: string, gameId: string): boolean {
  const stage = run.activeMatch;
  return !!stage
    && validRunAuthority(run)
    && (run.lastOpponentSeat === undefined || run.lastOpponentSeat === stage.botSeat)
    && (run.format === "single" || run.format === "bo3" || run.format === "run")
    && Array.isArray(run.results)
    && run.results.every((entry) => typeof entry?.gameId === "string"
      && (entry.result === "win" || entry.result === "loss" || entry.result === "draw"))
    && Array.isArray(run.playerDeck) && run.playerDeck.length > 0
    && run.playerDeck.every((card) => typeof card === "string")
    && Array.isArray(run.opponentDeck) && run.opponentDeck.length > 0
    && run.opponentDeck.every((card) => typeof card === "string")
    && Array.isArray(run.usedBotSeats)
    && typeof stage.draftId === "string" && stage.draftId === draftId
    && typeof stage.gameId === "string" && stage.gameId.length > 0 && stage.gameId === gameId
    && stage.format === run.format
    && Number.isInteger(stage.resultCountAtLaunch)
    && stage.resultCountAtLaunch === run.results.length
    && Number.isInteger(stage.botSeat) && stage.botSeat > 0
    && run.usedBotSeats.includes(stage.botSeat)
    && Array.isArray(stage.opponentDeck)
    && stage.opponentDeck.every((card) => typeof card === "string")
    && arraysEqual(run.opponentDeck, stage.opponentDeck)
    && !run.results.some((entry) => entry.gameId === gameId);
}

function unresolvedStageMatches(
  run: DraftRunState,
  draftId: string,
  format: DraftRunFormat,
  playerDeck: string[],
): boolean {
  const stage = run.activeMatch;
  return !!stage
    && isCoherentUnresolvedDraftStage(run, draftId, stage.gameId)
    && run.format === format
    && arraysEqual(run.playerDeck, playerDeck);
}

function draftSetCodes(run: DraftRunState | null, view: DraftPlayerView | null): string[] {
  if (run && !validRunAuthority(run)) throw new Error(i18n.t("draft:run.resumeUnavailable"));
  return [...(run?.draft_set_codes ?? view?.draft_set_codes ?? [])];
}

function withBoosterPackPool(
  run: DraftRunState,
  boosterPackPool: string[] | null | undefined,
  view: DraftPlayerView | null,
  draftId: string,
): DraftRunState {
  const codes = draftSetCodes(run, view);
  const seat = resolveDraftRunOpponentSeat(run, draftId);
  const needsPool = run.booster_pack_pool === undefined && boosterPackPool !== undefined;
  const needsSeat = run.lastOpponentSeat === undefined && seat !== undefined;
  if (!needsPool && !needsSeat && run.draft_set_codes !== undefined) return run;
  return {
    ...run,
    ...(needsPool ? { booster_pack_pool: boosterPackPool } : {}),
    ...(needsSeat ? { lastOpponentSeat: seat } : {}),
    ...(run.draft_set_codes === undefined ? { draft_set_codes: codes } : {}),
  };
}

function matchPayload(run: DraftRunState): DraftMatchPayload {
  return {
    player: { main_deck: run.playerDeck, sideboard: [], commander: [] },
    opponent: { main_deck: run.opponentDeck, sideboard: [], commander: [] },
    ai_decks: [],
    booster_pack_pool: run.booster_pack_pool,
  };
}

type FormatGateVerdict = { compatible: boolean; reasons?: string[] };

async function evaluateLimitedDeck(
  deck: DraftMatchPayload["player"],
  draftSetCodes: readonly string[],
  selectedMatchType: MatchType,
): Promise<FormatGateVerdict> {
  const selectedFormat: GameFormat = "Limited";
  const result = await getSharedAdapter().evaluateDeckFormatGate({
    main_deck: deck.main_deck,
    sideboard: deck.sideboard,
    commander: deck.commander,
    companion: [],
    planar_deck: [],
    scheme_deck: [],
    signature_spell: [],
    draft_set_codes: [...draftSetCodes],
    selected_format: selectedFormat,
    selected_match_type: selectedMatchType,
    player_count: 2,
  });
  if (result === null || typeof result !== "object"
    || typeof (result as FormatGateVerdict).compatible !== "boolean") {
    throw new Error(i18n.t("draft:limitedDeck.compatibilityUnavailable"));
  }
  return result as FormatGateVerdict;
}

function gateReason(verdict: FormatGateVerdict): string | null {
  if (verdict.compatible === true) return null;
  return verdict.reasons?.find((reason) => typeof reason === "string" && reason.length > 0)
    ?? i18n.t("draft:limitedDeck.validationTitle");
}

async function preflightMatchPayload(
  payload: DraftMatchPayload,
  draftSetCodes: readonly string[],
  selectedMatchType: MatchType,
): Promise<void> {
  const results = await Promise.allSettled([
    evaluateLimitedDeck(payload.player, draftSetCodes, selectedMatchType),
    evaluateLimitedDeck(payload.opponent, draftSetCodes, selectedMatchType),
  ]);
  for (const result of results) {
    if (result.status === "rejected") {
      const message = result.reason instanceof Error ? result.reason.message : String(result.reason);
      throw new Error(message || i18n.t("draft:limitedDeck.compatibilityUnavailable"));
    }
    const reason = gateReason(result.value);
    if (reason) throw new Error(reason);
  }
}

function orderedBotSeats(usedSeats: readonly number[], view: DraftPlayerView): number[] {
  const roster = [...new Set(view.seats.filter((seat) => seat.is_bot).map((seat) => seat.seat_index))];
  const candidates = roster.length > 0 ? roster : [1, 2, 3, 4, 5, 6, 7];
  const unused = candidates.filter((seat) => !usedSeats.includes(seat));
  const used = candidates.filter((seat) => usedSeats.includes(seat));
  const preferred = unused.length > 0 ? unused : used;
  const start = Math.floor(Math.random() * preferred.length);
  return [...preferred.slice(start), ...preferred.slice(0, start), ...(unused.length > 0 ? used : [])];
}

function expandSuggestedDeck(deck: SuggestedDeck): string[] {
  return [...deck.main_deck, ...Object.entries(deck.lands).flatMap(([name, count]) =>
    Array<string>(normalizeVirtualBasicCount(count)).fill(name))];
}

async function selectViableOpponent(
  runtime: LocalDraftRuntime,
  playerDeck: string[],
  usedSeats: readonly number[],
  view: DraftPlayerView,
  draftSetCodes: readonly string[],
  selectedMatchType: MatchType,
  fresh: () => boolean,
): Promise<{ botSeat: number; opponentDeck: string[] }> {
  const player = { main_deck: [...playerDeck], sideboard: [], commander: [] };
  let playerAccepted = false;
  let lastOpponentReason = i18n.t("draft:run.startUnavailable");

  for (const botSeat of orderedBotSeats(usedSeats, view)) {
    if (!fresh()) throw new Error("Stale draft match launch");
    let botDeck: SuggestedDeck;
    try {
      botDeck = await withDraftEngineOperation((lease) => {
        if (!fresh()) throw new Error("Stale draft match launch");
        return lease.quickSession(runtime).botDeck(botSeat);
      });
    } catch (error) {
      if (!fresh()) throw new Error("Stale draft match launch");
      lastOpponentReason = error instanceof Error ? error.message : String(error);
      continue;
    }
    if (!fresh()) throw new Error("Stale draft match launch");
    const opponentDeck = expandSuggestedDeck(botDeck);
    const opponent = { main_deck: opponentDeck, sideboard: [], commander: [] };
    if (!playerAccepted) {
      const [playerResult, opponentResult] = await Promise.allSettled([
        evaluateLimitedDeck(player, draftSetCodes, selectedMatchType),
        evaluateLimitedDeck(opponent, draftSetCodes, selectedMatchType),
      ]);
      if (!fresh()) throw new Error("Stale draft match launch");
      if (playerResult.status === "rejected") {
        const message = playerResult.reason instanceof Error ? playerResult.reason.message : String(playerResult.reason);
        throw new Error(message || i18n.t("draft:limitedDeck.compatibilityUnavailable"));
      }
      if (opponentResult.status === "rejected") {
        const message = opponentResult.reason instanceof Error ? opponentResult.reason.message : String(opponentResult.reason);
        throw new Error(message || i18n.t("draft:limitedDeck.compatibilityUnavailable"));
      }
      const playerReason = gateReason(playerResult.value);
      if (playerReason) throw new Error(playerReason);
      playerAccepted = true;
      const opponentReason = gateReason(opponentResult.value);
      if (opponentReason) {
        lastOpponentReason = opponentReason;
        continue;
      }
    } else {
      const verdict = await evaluateLimitedDeck(opponent, draftSetCodes, selectedMatchType);
      if (!fresh()) throw new Error("Stale draft match launch");
      const opponentReason = gateReason(verdict);
      if (opponentReason) {
        lastOpponentReason = opponentReason;
        continue;
      }
    }
    return { botSeat, opponentDeck };
  }
  throw new Error(lastOpponentReason);
}

function navigateToMatch(
  state: DraftStoreState,
  gameId: string,
  selectedMatchType: MatchType,
  navigate: (path: string) => void,
): void {
  const matchType = selectedMatchType === "Bo3" ? "bo3" : "bo1";
  const difficulty = DIFFICULTY_NAMES[state.difficulty] ?? "Medium";
  useGameStore.setState({ gameId });
  navigate(`/game/${gameId}?mode=ai&difficulty=${difficulty}&format=Limited&match=${matchType}&source=draft&draftId=${state.draftId}`);
}

export const useDraftStore = create<DraftStoreState & DraftStoreActions>()((set, get) => ({
  ...initialState,

  startDraft: (selectionOrJson: DraftSetSelection | string, ...args: [number] | [string, string, number]) => {
    const [setCode, setName, difficulty] = typeof selectionOrJson === "string"
      ? args as [string, string, number]
      : [
          distinctJoined(selectionOrJson.packs.map((pack) => pack.code), "+"),
          distinctJoined(selectionOrJson.packs.map((pack) => pack.name), " · "),
          args[0] as number,
        ];
    const setPoolJson = typeof selectionOrJson === "string"
      ? selectionOrJson
      : JSON.stringify(packSequence(selectionOrJson));
    return startLocalDraft({
    setCode,
    setName,
    difficulty,
    kind: "Quick",
    initialize: (lease) => lease.initialize(setPoolJson, difficulty, Math.floor(Math.random() * 0xffffffff)),
    });
  },

  startSealedDraft: (selectionOrJson: DraftSetSelection | string, ...args: [number] | [string, string, number]) => {
    const [setCode, setName, difficulty] = typeof selectionOrJson === "string"
      ? args as [string, string, number]
      : [
          distinctJoined(selectionOrJson.packs.map((pack) => pack.code), "+"),
          distinctJoined(selectionOrJson.packs.map((pack) => pack.name), " · "),
          args[0] as number,
        ];
    const setPoolJson = typeof selectionOrJson === "string"
      ? selectionOrJson
      : JSON.stringify(packSequence(selectionOrJson));
    return startLocalDraft({
    setCode,
    setName,
    difficulty,
    kind: "Sealed",
    initialize: (lease) => lease.initializeSealed(setPoolJson, difficulty, Math.floor(Math.random() * 0xffffffff)),
    });
  },

  startCubeDraft: (cubeListText, cubeName, settings, difficulty) => startLocalDraft({
    setCode: CUSTOM_CUBE_SET_CODE,
    setName: cubeName,
    difficulty,
    kind: "Quick",
    initialize: (lease) => lease.initializeCube(
      cubeListText, cubeName, settings, difficulty, Math.floor(Math.random() * 0xffffffff),
    ),
  }),

  completeSealedOpening: () => {
    if (exclusiveToken) return;
    set({ phase: "deckbuilding" });
    schedulePersistence(0);
  },

  resumeDraft: async () => {
    const lifecycle = beginLifecycle();
    await Promise.all([drainDraftEngineOperations(), drainQuickDraftPersistence()]);
    const meta = await inspectActiveQuickDraftLifecycle("inspect");
    if (!meta || lifecycle !== lifecycleGeneration) return { status: "none" };
    const unavailable = (reason: string): DraftResumeOutcome => {
      if (lifecycle === lifecycleGeneration) set({ draftId: meta.id });
      return { status: "unavailable", draftId: meta.id, reason };
    };
    let run: DraftRunState | null;
    try {
      run = await loadDraftRun(meta.id);
    } catch (error) {
      return unavailable(error instanceof Error ? error.message : String(error));
    }
    if (lifecycle !== lifecycleGeneration) return { status: "none" };
    const submitted = meta.phase === "playing" || meta.phase === "complete" || !!run;
    if (submitted && !run) return unavailable(i18n.t("draft:run.resumeUnavailable"));
    const installRunOnly = (): DraftResumeOutcome => {
      if (!run || lifecycle !== lifecycleGeneration) return unavailable(i18n.t("draft:run.resumeUnavailable"));
      if (!validDifficulty(meta.difficulty) || !validRun(run, meta.id, meta.setCode)) {
        return unavailable(i18n.t("draft:run.resumeUnavailable"));
      }
      set({
        draftId: meta.id, adapter: null, runtime: null, view: null, workspaceState: null,
        localFifoSessionId: meta.localFifoSessionId ?? null,
        phase: draftRunPhase(run), difficulty: meta.difficulty,
        selectedSet: meta.setCode, selectedSetName: meta.setName ?? null,
        kind: meta.kind ?? "Quick", runFormat: run.format, runState: run,
      });
      return { status: "resumed", draftId: meta.id };
    };
    // A historical Cube run may acquire its source from the restored adapter,
    // but its submitted decks and other durable fields must already be sound.
    if (run && !validRunFields(run, meta.id)) {
      return unavailable(i18n.t("draft:run.resumeUnavailable"));
    }
    let saved: Awaited<ReturnType<typeof loadQuickDraftSession>>;
    try {
      saved = await loadQuickDraftSession(meta.id);
    } catch (error) {
      return submitted ? installRunOnly() : unavailable(error instanceof Error ? error.message : String(error));
    }
    if (!saved) return submitted ? installRunOnly() : unavailable(i18n.t("draft:run.resumeUnavailable"));
    try {
      const database = await prepareCardDatabase();
      const adapter = new DraftAdapter();
      // The engine keeps no LLM drafting across installs: the store declares
      // it, which also runs any bot the saved session left waiting.
      const fifoResume = meta.localFifoSessionId === undefined
        ? null
        : { sessionId: meta.localFifoSessionId, llm: await localFifoLlmDrafting() };
      if (lifecycle !== lifecycleGeneration) return { status: "none" };
      const restored = await withDraftEngineOperation((lease) => {
        if (lifecycle !== lifecycleGeneration) throw new Error("Stale draft resume");
        lease.loadCardDatabase(database);
        lease.setPickEnhancements(usePreferencesStore.getState().draftPickEnhancements);
        if (lifecycle !== lifecycleGeneration) throw new Error("Stale draft resume");
        // The record's declaration routes it; the engine's import is the check.
        // An undeclared record keeps the legacy import it was saved for.
        let runtime: LocalDraftRuntime;
        let view: DraftPlayerView;
        if (fifoResume === null) {
          runtime = { kind: "legacy" };
          view = lease.importSession(saved.sessionJson, meta.difficulty);
        } else {
          const local = lease.importLocalFifoSession(saved.sessionJson, meta.difficulty);
          if (draftViewSessionId(local.view) !== fifoResume.sessionId) {
            throw new Error(i18n.t("draft:run.resumeUnavailable"));
          }
          runtime = { kind: "local-fifo", owner: local.owner };
          view = lease.localFifoSetLlmDrafting(local.owner, fifoResume.llm.drafting, Date.now());
        }
        return {
          runtime,
          view,
          boosterPackPool: lease.quickSession(runtime).boosterPackPoolForGame(),
        };
      });
      if (lifecycle !== lifecycleGeneration) return { status: "none" };
      const { runtime, view } = restored;
      if (run) {
        const upgraded = withBoosterPackPool(run, restored.boosterPackPool, view, meta.id);
        if (!validRun(upgraded, meta.id, meta.setCode)) {
          return unavailable(i18n.t("draft:run.resumeUnavailable"));
        }
        if (upgraded !== run) {
          await saveDraftRun(meta.id, upgraded);
          if (lifecycle !== lifecycleGeneration) return { status: "none" };
          run = upgraded;
        }
      }
      // The durable run is authoritative for run phases: the run and the meta
      // are persisted as separate writes (publishInitialDraftMatch /
      // recordDraftMatchResult save the run first), so a crash between them
      // leaves the meta stale — it may still say "launching" for an already
      // active run, or "playing" for a terminal one. The run's results vs its
      // limits decide; the meta's phase only matters before a run exists.
      const resumedPhase: DraftPhase = run ? draftRunPhase(run) : meta.phase;
      if (runtime.kind === "local-fifo" && fifoResume !== null) {
        fifoLlm = {
          lifecycle,
          owner: runtime.owner,
          drafting: fifoResume.llm.drafting,
          availability: "available",
          controller: new AbortController(),
          inFlight: new Map(),
          statuses: new Map(),
          viewOwed: false,
          syncing: false,
          syncAgain: false,
        };
      }
      installWorkspace({
        kind: "state",
        authoritativeView: view,
        baseWorkspace: saved.workspace ?? migrateLegacyWorkspace(view.pool, saved),
        patch: {
          draftId: meta.id,
          adapter,
          runtime,
          localFifoSessionId: meta.localFifoSessionId ?? null,
          phase: phaseForView(view, resumedPhase),
          difficulty: meta.difficulty,
          selectedSet: meta.setCode,
          selectedSetName: meta.setName ?? null,
          kind: meta.kind ?? "Quick",
          selectedCard: null,
          pendingPickIntent: null,
          pickInteractionLocked: false,
          poolSortMode: saved.poolSortMode,
          poolPanelOpen: saved.poolPanelOpen,
          // The persisted run's format is authoritative once a run exists — a
          // Sealed run may be a 7W/3L ladder even though the event's picker
          // default is a single match. Before the first match there is no run,
          // so fall back to the meta's remembered choice, then the kind default.
          runFormat: run?.format ?? meta.runFormat ?? (meta.kind === "Sealed" ? "single" : "run"),
          runState: run,
        },
        persistence: "skip",
      });
      return { status: "resumed", draftId: meta.id };
    } catch (error) {
      if (lifecycle !== lifecycleGeneration) return { status: "none" };
      return submitted ? installRunOnly() : unavailable(error instanceof Error ? error.message : String(error));
    }
  },

  abandonDraft: async () => {
    const id = get().draftId;
    beginLifecycle();
    if (id) await cleanupQuickDraftLifecycle(id);
  },

  pickCard: (cardInstanceId, destination = "deck", placementHint) => performPick({
    kind: "pick",
    instanceId: cardInstanceId,
    destination,
    placementHint,
  }),

  confirmPick: (destination = "deck", placementHint) => {
    const selectedCard = get().selectedCard;
    if (!selectedCard) {
      return Promise.resolve({ status: "rejected", reason: "invalid-request" });
    }
    return performPick({
      kind: "pick",
      instanceId: selectedCard,
      destination,
      placementHint,
    });
  },

  pickCardWithDraftEffect: (
    effectCardInstanceId,
    instanceIds,
    destination = "deck",
    placementHint,
  ) => performPick({
    kind: "draft-effect",
    effectCardInstanceId,
    instanceIds,
    destination,
    placementHint,
  }),

  autoPickCard: (destination = "deck", placementHints) => performPick({
    kind: "auto-pick",
    destination,
    placementHints,
  }),

  setWorkspaceState: (next) => {
    const state = get();
    if (workspaceMutationBlocked(state) || !state.view) return;
    installWorkspace({
      kind: "state",
      authoritativeView: state.view,
      baseWorkspace: next,
      patch: {},
      persistence: "schedule",
    });
  },

  setWorkspacePlacement: (instanceId, placement) => {
    const state = get();
    if (workspaceMutationBlocked(state) || !state.workspaceState || !state.view) return;
    const workspace = updateWorkspacePlacement(
      state.workspaceState,
      state.view.pool,
      instanceId,
      placement,
    );
    if (workspace === state.workspaceState) return;
    installWorkspace({
      kind: "state",
      authoritativeView: state.view,
      baseWorkspace: workspace,
      patch: {},
      persistence: "schedule",
    });
  },

  selectCard: (selectedCard) => {
    const state = get();
    if (state.pickInteractionLocked) return;
    if (state.view !== null && draftViewPresentation(state.view).isReadOnly) return;
    set({ selectedCard });
  },

  addBasicLand: (name) => {
    const state = get();
    if (workspaceMutationBlocked(state) || !state.workspaceState || !state.view) return;
    if (state.workspaceState.virtualBasics.length >= MAX_MATERIALIZED_VIRTUAL_BASICS) return;
    const instanceId = makeInteractiveVirtualBasicInstanceId(state.workspaceState, state.view.pool);
    installWorkspace({
      kind: "state",
      authoritativeView: state.view,
      baseWorkspace: addVirtualBasic(state.workspaceState, state.view.pool, { instanceId, name }),
      patch: {},
      persistence: "schedule",
    });
  },

  removeBasicLand: (name) => {
    const state = get();
    if (workspaceMutationBlocked(state) || !state.workspaceState || !state.view) return;
    const target = [...state.workspaceState.virtualBasics].reverse().find(
      (basic) => basic.name === name
        && state.workspaceState!.placements[basic.instanceId]?.zone === "deck",
    );
    if (!target) return;
    installWorkspace({
      kind: "state",
      authoritativeView: state.view,
      baseWorkspace: removeVirtualBasic(state.workspaceState, target.instanceId),
      patch: {},
      persistence: "schedule",
    });
  },

  autoSuggestDeck: async () => {
    const state = get();
    const runtime = state.runtime;
    if (workspaceMutationBlocked(state) || !state.adapter || !runtime || !state.workspaceState || !state.view) return;
    const token = ++suggestionToken;
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    const fresh = (): boolean => {
      const current = useDraftStore.getState();
      return token === suggestionToken
        && lifecycle === lifecycleGeneration
        && revision === workspaceRevision
        && current.adapter === state.adapter
        && current.view === state.view
        && current.workspaceState === state.workspaceState
        && !workspaceMutationBlocked(current);
    };
    let result: SuggestedDeck;
    try {
      result = await withDraftEngineOperation((lease) => {
        if (!fresh()) throw new Error("Stale draft suggestion");
        return lease.quickSession(runtime).suggestDeck();
      });
    } catch (error) {
      if (!fresh()) return;
      throw error;
    }
    if (!fresh()) return;
    const remaining = new Map<string, number>();
    for (const name of result.main_deck) remaining.set(name, (remaining.get(name) ?? 0) + 1);
    let workspace = state.workspaceState;
    for (const card of state.view.pool) {
      const count = remaining.get(card.name) ?? 0;
      const placement = workspace.placements[card.instance_id];
      workspace = updateWorkspacePlacement(workspace, state.view.pool, card.instance_id, {
        ...placement, zone: count > 0 ? "deck" : "sideboard",
      });
      if (count > 0) remaining.set(card.name, count - 1);
    }
    workspace = replaceDeckVirtualBasics(workspace, state.view.pool, result.lands, true, false);
    installWorkspace({
      kind: "state",
      authoritativeView: state.view,
      baseWorkspace: workspace,
      patch: {},
      persistence: "schedule",
    });
  },

  autoSuggestLands: async () => {
    const state = get();
    const runtime = state.runtime;
    if (workspaceMutationBlocked(state) || !state.adapter || !runtime || !state.workspaceState || !state.view) return;
    const token = ++suggestionToken;
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    const fresh = (): boolean => {
      const current = useDraftStore.getState();
      return token === suggestionToken
        && lifecycle === lifecycleGeneration
        && revision === workspaceRevision
        && current.adapter === state.adapter
        && current.view === state.view
        && current.workspaceState === state.workspaceState
        && !workspaceMutationBlocked(current);
    };
    let lands: Record<string, number>;
    try {
      lands = await withDraftEngineOperation((lease) => {
        if (!fresh()) throw new Error("Stale draft suggestion");
        return lease.quickSession(runtime).suggestLands(workspaceFacades(state.workspaceState!, state.view!).mainDeck);
      });
    } catch (error) {
      if (!fresh()) return;
      throw error;
    }
    if (!fresh()) return;
    installWorkspace({
      kind: "state",
      authoritativeView: state.view,
      baseWorkspace: replaceDeckVirtualBasics(state.workspaceState, state.view.pool, lands, true, true),
      patch: {},
      persistence: "schedule",
    });
  },

  submitDeck: async () => {
    const token = admitExclusive("submit");
    if (!token) return;
    const state = get();
    const runtime = state.runtime;
    if (!state.adapter || !runtime || !state.workspaceState || !state.view) {
      retireExclusive(token);
      return;
    }
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    const partition = projectWorkspacePartition(state.workspaceState, state.view.pool);
    const fresh = () => isExclusive(token, "submit")
      && lifecycle === lifecycleGeneration && revision === workspaceRevision
      && get().adapter === state.adapter && get().draftId === state.draftId
      && get().view === state.view && get().workspaceState === state.workspaceState;
    try {
      const verdict = await evaluateLimitedDeck(
        { main_deck: partition.mainDeck, sideboard: [], commander: [] },
        draftSetCodes(state.runState, state.view),
        "Bo1",
      );
      const reason = gateReason(verdict);
      if (reason) throw new Error(reason);
      if (!fresh()) throw new Error("Stale draft deck submission");
      const view = await withDraftEngineOperation((lease) => {
        if (!fresh()) throw new Error("Stale draft deck submission");
        return lease.quickSession(runtime).submitDeck(partition.mainDeck, [], Date.now());
      });
      if (!isExclusive(token, "submit") || lifecycle !== lifecycleGeneration) return;
      retireExclusive(token);
      installWorkspace({
        kind: "state",
        authoritativeView: view,
        baseWorkspace: state.workspaceState,
        patch: {
          phase: view.status === "Deckbuilding" ? "deckbuilding" : "launching",
        },
        persistence: "schedule",
      });
      void autosaveDraftDeck({ view: state.view, setCode: state.selectedSet, partition, commanders: [] });
    } catch (error) {
      retireExclusive(token);
      throw error;
    }
  },

  setPoolSortMode: (poolSortMode) => { set({ poolSortMode }); schedulePersistence(); },
  togglePoolPanel: () => { set((state) => ({ poolPanelOpen: !state.poolPanelOpen })); schedulePersistence(); },
  setDifficulty: (difficulty) => set({ difficulty }),
  setSelectedSet: (selectedSet) => set({ selectedSet }),
  // The picker selection is the resume authority before the first match (the
  // run record only appears at launch), so persist it — otherwise reloading on
  // the launching screen restores the stale default. Mirrors setPoolSortMode.
  setRunFormat: (runFormat) => {
    if (exclusiveToken?.kind === "launch" || exclusiveToken?.kind === "end") return;
    set({ runFormat });
    schedulePersistence();
  },

  launchMatch: async (navigate) => {
    const token = admitExclusive("launch");
    if (!token) return;
    cancelScheduledPersistence();
    const state = get();
    const runtime = state.runtime;
    if (!state.adapter || !runtime || !state.draftId || !state.selectedSet || !state.workspaceState || !state.view) {
      retireExclusive(token);
      return;
    }
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    const selectedRunFormat = state.runFormat;
    const selectedMatchType: MatchType = state.view.match_config.match_type === "Bo3"
      && selectedRunFormat === "bo3" ? "Bo3" : "Bo1";
    const fresh = () => isExclusive(token, "launch")
      && lifecycle === lifecycleGeneration && revision === workspaceRevision
      && get().draftId === state.draftId && get().adapter === state.adapter
      && get().runFormat === selectedRunFormat;
    const samePreRunSession = () => exclusiveToken === null
      && lifecycle === lifecycleGeneration
      && get().draftId === state.draftId && get().adapter === state.adapter
      && get().selectedSet === state.selectedSet && get().phase === "launching"
      && get().runFormat === selectedRunFormat && get().runState === null;
    const playerDeck = projectDeckNames(state.workspaceState, state.view.pool);
    const legacyFacades = workspaceFacades(state.workspaceState, state.view);
    let durableRun: DraftRunState | null | undefined;
    let publicationStarted = false;
    let publicationSucceeded = false;
    let launchError: unknown = null;
    let launchFailed = false;
    try {
      durableRun = await loadDraftRun(state.draftId);
      if (!fresh()) throw new Error("Stale draft match launch");
      const codes = draftSetCodes(durableRun, state.view);
      let run: DraftRunState;
      let sessionJson: string | null = null;
      if (durableRun) {
        if (!unresolvedStageMatches(durableRun, state.draftId, selectedRunFormat, playerDeck)
          || durableRun.results.length !== 0) throw new Error(i18n.t("draft:run.startUnavailable"));
        const boosterPackPool = await withDraftEngineOperation(
          (lease) => lease.quickSession(runtime).boosterPackPoolForGame(),
        );
        if (!fresh()) throw new Error("Stale draft match launch");
        run = withBoosterPackPool(durableRun, boosterPackPool, state.view, state.draftId);
      } else {
        const prepared = await withDraftEngineOperation((lease) => {
          if (!fresh()) throw new Error("Stale draft match launch");
          const session = lease.quickSession(runtime);
          return {
            sessionJson: session.exportSession(),
            boosterPackPool: session.boosterPackPoolForGame(),
          };
        });
        sessionJson = prepared.sessionJson;
        const { botSeat, opponentDeck } = await selectViableOpponent(
          runtime, playerDeck, [], state.view, codes, selectedMatchType, fresh,
        );
        if (!fresh()) throw new Error("Stale draft match launch");
        const gameId = crypto.randomUUID();
        run = {
          format: selectedRunFormat,
          booster_pack_pool: prepared.boosterPackPool,
          results: [],
          playerDeck,
          opponentDeck,
          usedBotSeats: [botSeat],
          lastOpponentSeat: botSeat,
          draft_set_codes: codes,
          activeMatch: {
            draftId: state.draftId,
            gameId,
            format: selectedRunFormat,
            resultCountAtLaunch: 0,
            botSeat,
            opponentDeck,
          },
        };
      }
      const gameId = run.activeMatch!.gameId;
      const localState = { ...state, runState: run };
      const meta = makeMeta(localState, "playing", gameId);
      const payload = matchPayload(run);
      if (durableRun) {
        await preflightMatchPayload(payload, codes, selectedMatchType);
      }
      if (!fresh()) throw new Error("Stale draft match launch");
      publicationStarted = true;
      if (sessionJson !== null) {
        await publishInitialDraftMatch({
          draftId: state.draftId,
          sessionJson,
          snapshot: {
            phase: state.phase,
            ...legacyFacades,
            poolSortMode: state.poolSortMode,
            poolPanelOpen: state.poolPanelOpen,
            workspace: state.workspaceState,
          },
          run,
          gameId,
          payload,
          meta,
        });
      } else {
        await publishStagedDraftMatch({
          draftId: state.draftId,
          run: run !== durableRun ? run : undefined,
          gameId,
          payload,
          meta,
        });
      }
      publicationSucceeded = true;
      if (!fresh()) return;
      set({ phase: "playing", runState: run });
      navigateToMatch({ ...get(), runState: run }, gameId, selectedMatchType, navigate);
    } catch (error) {
      launchError = error;
      launchFailed = true;
    } finally {
      const owned = isExclusive(token, "launch");
      retireExclusive(token);
      if (owned && durableRun === null && !publicationSucceeded && samePreRunSession()) {
        try {
          // The initial publication writes the session before the run. A
          // rejected write may therefore have cancelled the format debounce
          // without leaving any durable run for resume to read.
          const publishedRun = publicationStarted ? await loadDraftRun(state.draftId) : null;
          if (publishedRun === null && samePreRunSession()) {
            await persistDraft(persistenceGeneration, {
              propagateFailure: true,
              canPersist: samePreRunSession,
            });
          }
        } catch (saveError) {
          const cause = launchError instanceof Error ? launchError.message
            : launchError === null ? i18n.t("draft:run.startUnavailable") : String(launchError);
          const saveReason = saveError instanceof Error ? saveError.message : String(saveError);
          launchError = new Error(`${cause}\n${saveReason}`);
          launchFailed = true;
        }
      }
    }
    if (launchFailed) throw launchError;
  },

  recordMatchResult: async (gameId, result) => {
    const meta = await inspectActiveQuickDraftLifecycle("inspect");
    if (!meta) return;
    const persisted = await recordDraftMatchResult({
      draftId: meta.id,
      gameId,
      result,
      makeMeta: (run) => {
        const wins = run.results.filter((entry) => entry.result === "win").length;
        const losses = run.results.filter((entry) => entry.result === "loss").length;
        const draws = run.results.filter((entry) => entry.result === "draw").length;
        return {
          ...meta,
          // Legacy metadata may predate the runFormat field; the durable run
          // is the only authoritative source for it. Without this, a later
          // recordMatchResult would gate out on the absent field and drop
          // the next match's result too.
          runFormat: run.format,
          phase: draftRunPhase(run),
          updatedAt: Date.now(),
          runWins: wins,
          runLosses: losses,
          runDraws: draws,
          currentGameId: undefined,
        };
      },
    });
    if (persisted && get().draftId === meta.id) {
      set({ runState: persisted.run, phase: persisted.meta.phase });
    }
  },

  launchNextMatch: async (navigate) => {
    const token = admitExclusive("launch");
    if (!token) return;
    const state = get();
    if (!state.draftId || !state.selectedSet || (!state.adapter && !state.runState)) {
      retireExclusive(token);
      throw new Error(i18n.t("draft:run.resumeUnavailable"));
    }
    const runOnly = !state.adapter || !state.runtime || !state.view || !state.workspaceState;
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    const selectedRunFormat = state.runFormat;
    const selectedMatchType: MatchType = (runOnly || state.view?.match_config.match_type === "Bo3")
      && selectedRunFormat === "bo3" ? "Bo3" : "Bo1";
    const fresh = () => isExclusive(token, "launch")
      && lifecycle === lifecycleGeneration && revision === workspaceRevision
      && get().draftId === state.draftId && get().adapter === state.adapter
      && get().runFormat === selectedRunFormat;
    try {
      const savedRun = await loadDraftRun(state.draftId);
      if (!savedRun) throw new Error(i18n.t("draft:run.resumeUnavailable"));
      const boosterPackPool = runOnly ? undefined
        : await withDraftEngineOperation((lease) => lease.quickSession(state.runtime!).boosterPackPoolForGame());
      if (!fresh()) return;
      const durableRun = withBoosterPackPool(savedRun, boosterPackPool, state.view, state.draftId);
      if (!validRun(durableRun, state.draftId, state.selectedSet)
        || (runOnly && !validDifficulty(state.difficulty))) {
        throw new Error(i18n.t("draft:run.resumeUnavailable"));
      }
      const codes = draftSetCodes(durableRun, state.view);
      const playerDeck = runOnly ? durableRun.playerDeck
        : projectDeckNames(state.workspaceState!, state.view!.pool);
      if (draftRunPhase(durableRun) === "complete") throw new Error(i18n.t("draft:run.runComplete"));
      if (durableRun.format !== selectedRunFormat) throw new Error(i18n.t("draft:run.startUnavailable"));
      let run = durableRun;
      let saveRun = durableRun !== savedRun;
      if (durableRun.activeMatch) {
        if (!unresolvedStageMatches(durableRun, state.draftId, selectedRunFormat, playerDeck)) {
          throw new Error(i18n.t("draft:run.startUnavailable"));
        }
      } else {
        const retainedSeat = resolveDraftRunOpponentSeat(durableRun, state.draftId);
        if (runOnly && retainedSeat === undefined) throw new Error(i18n.t("draft:run.resumeUnavailable"));
        const { botSeat, opponentDeck } = runOnly
          ? { botSeat: retainedSeat!, opponentDeck: durableRun.opponentDeck }
          : await selectViableOpponent(
              state.runtime!, playerDeck, durableRun.usedBotSeats, state.view!, codes, selectedMatchType, fresh,
            );
        if (!fresh()) throw new Error("Stale next match launch");
        const gameId = crypto.randomUUID();
        const usedBotSeats = durableRun.usedBotSeats.includes(botSeat)
          ? durableRun.usedBotSeats
          : [...durableRun.usedBotSeats, botSeat];
        run = {
          ...durableRun,
          opponentDeck,
          lastOpponentSeat: botSeat,
          usedBotSeats,
          activeMatch: {
            draftId: state.draftId,
            gameId,
            format: selectedRunFormat,
            resultCountAtLaunch: durableRun.results.length,
            botSeat,
            opponentDeck,
          },
        };
        saveRun = true;
      }
      const gameId = run.activeMatch!.gameId;
      const meta = makeMeta({ ...state, runState: run }, "playing", gameId);
      const payload = matchPayload(run);
      if (durableRun.activeMatch || runOnly || !arraysEqual(run.playerDeck, playerDeck)) {
        await preflightMatchPayload(payload, codes, selectedMatchType);
      }
      if (!fresh()) return;
      await publishStagedDraftMatch({
        draftId: state.draftId,
        run: saveRun ? run : undefined,
        gameId,
        payload,
        meta,
      });
      if (!fresh()) return;
      set({ phase: "playing", runState: run });
      navigateToMatch({ ...get(), runState: run }, gameId, selectedMatchType, navigate);
    } finally {
      retireExclusive(token);
    }
  },

  endRun: (draftId) => {
    const id = draftId ?? get().draftId;
    if (endingRun) {
      return endingRun.draftId === id
        ? endingRun.promise
        : Promise.reject(new Error("Another draft run is ending"));
    }
    if (!id) {
      beginLifecycle();
      return Promise.resolve();
    }

    let resolve!: () => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<void>((resolvePromise, rejectPromise) => {
      resolve = resolvePromise;
      reject = rejectPromise;
    });
    const generation = invalidateLifecycle();
    const interaction = nextInteractionGeneration();
    const token = admitExclusive("end")!;
    const operation = { draftId: id, promise };
    endingRun = operation;
    useDraftStore.setState({ interactionGeneration: interaction });

    void Promise.resolve().then(() => cleanupQuickDraftLifecycle(id)).then(
      () => {
        if (generation === lifecycleGeneration && isExclusive(token, "end")) {
          retireExclusive(token);
          publishInitialState(interaction);
        }
        if (endingRun === operation) endingRun = null;
        resolve();
      },
      (error: unknown) => {
        retireExclusive(token);
        if (endingRun === operation) endingRun = null;
        reject(error);
      },
    );
    return promise;
  },

  reset: () => {
    beginLifecycle();
  },
}));

// When a pack opens, start that step's LLM picks at once rather than on the
// player's click: the pod's LLM drafters think while the player does. A new
// step also clears the previous step's drafter statuses, so a chip never
// reports a pick for a pack that has already passed.
useDraftStore.subscribe((state, previous) => {
  if (state.view === previous.view) return;
  // A local FIFO draft's LLM seats answer their own heads, each at its own
  // pace, so a new view neither clears their statuses nor starts a round: it
  // syncs them. The barrier round reads the legacy session only.
  if (state.runtime?.kind === "local-fifo") {
    syncLocalFifoLlm();
    return;
  }
  if (state.llmDrafters.length > 0 || state.awaitingDrafters) {
    useDraftStore.setState({ llmDrafters: [], awaitingDrafters: false });
  }
  const view = state.view;
  if (!view || state.phase !== "drafting" || view.status !== "Drafting") return;
  if (!view.current_pack || view.current_pack.length === 0) return;
  void ensureLlmDraftRound(view);
});
