import { create } from "zustand";

export const DRAFT_DECK_SESSION_KEY = "phase:draft-deck";

import {
  DraftAdapter,
  distinctJoined,
  drainDraftEngineOperations,
  withDraftEngineOperation,
  type CubeDraftSettings,
  type DraftPlayerView,
  type SetPackSequence,
  type SuggestedDeck,
} from "../adapter/draft-adapter";
import {
  MAX_MATERIALIZED_VIRTUAL_BASICS,
  migrateLegacyWorkspace,
  normalizeVirtualBasicCount,
} from "../components/draft/workspace/workspaceMigration";
import {
  appendWorkspaceInstanceToResolvedDestination,
  createDraftWorkspaceState,
  makeInteractiveVirtualBasicInstanceId,
  reconcileWorkspaceState,
  updateWorkspacePlacement,
} from "../components/draft/workspace/workspacePlacement";
import {
  addVirtualBasic,
  projectDeckNames,
  projectWorkspaceMainDeck,
  removeVirtualBasic,
} from "../components/draft/workspace/workspaceProjection";
import type {
  DraftCardPlacement,
  DraftWorkspaceState,
  DraftZone,
} from "../components/draft/workspace/types";
import { BASIC_LAND_NAMES } from "../constants/game";
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
  runLimits,
  type ActiveQuickDraftMeta,
  type DraftMatchPayload,
  type DraftMatchResult,
  type DraftRunFormat,
  type DraftRunState,
} from "../services/quickDraftPersistence";
import { useGameStore } from "./gameStore";

export type DraftPhase = "setup" | "drafting" | "opening" | "deckbuilding" | "launching" | "playing" | "complete";

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
      instanceIds: readonly [string, string];
      destination: DraftPickDestination;
      placementHint?: DraftPickPlacementHint;
    }
  | { kind: "auto-pick"; destination: "deck" };

interface DraftStoreState {
  draftId: string | null;
  adapter: DraftAdapter | null;
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
}

interface DraftStoreActions {
  startDraft: LegacyDraftStart;
  startSealedDraft: LegacyDraftStart;
  startCubeDraft(cubeListText: string, cubeName: string, settings: CubeDraftSettings, difficulty: number): Promise<void>;
  completeSealedOpening(): void;
  resumeDraft(): Promise<void>;
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
    cardInstanceIds: readonly [string, string],
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
  endRun(): Promise<void>;
  reset(): void;
}

const initialState: DraftStoreState = {
  draftId: null,
  adapter: null,
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
};

export const DIFFICULTY_NAMES = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"] as const;

let lifecycleGeneration = 0;
let workspaceRevision = 0;
let persistenceGeneration = 0;
let suggestionToken = 0;
let exclusiveToken: { identity: symbol; kind: "pick" | "submit" | "launch" } | null = null;
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

function beginLifecycle(): number {
  lifecycleGeneration += 1;
  exclusiveToken = null;
  invalidateWorkspaceDependents();
  cancelScheduledPersistence();
  useDraftStore.setState({
    ...initialState,
    interactionGeneration: lifecycleGeneration,
  });
  return lifecycleGeneration;
}

function admitExclusive(kind: "pick" | "submit" | "launch"): symbol | null {
  if (exclusiveToken) return null;
  const identity = Symbol(kind);
  exclusiveToken = { identity, kind };
  return identity;
}

function isExclusive(identity: symbol, kind?: "pick" | "submit" | "launch"): boolean {
  return exclusiveToken?.identity === identity && (!kind || exclusiveToken.kind === kind);
}

function retireExclusive(identity: symbol): void {
  if (exclusiveToken?.identity === identity) exclusiveToken = null;
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
  const generation = ++persistenceGeneration;
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = setTimeout(() => {
    debounceTimer = null;
    void persistDraft(generation);
  }, delay);
}

async function persistDraft(generation: number): Promise<void> {
  const state = useDraftStore.getState();
  const { adapter, draftId, view, workspaceState, selectedSet, phase } = state;
  if (!adapter || !draftId || !view || !workspaceState || !selectedSet
    || phase === "setup" || phase === "playing" || phase === "complete") return;
  const lifecycle = lifecycleGeneration;
  const revision = workspaceRevision;
  try {
    const sessionJson = await withDraftEngineOperation((lease) => {
      if (generation !== persistenceGeneration || lifecycle !== lifecycleGeneration
        || revision !== workspaceRevision || useDraftStore.getState().adapter !== adapter) {
        throw new Error("Stale draft persistence request");
      }
      return lease.exportSession();
    });
    if (generation !== persistenceGeneration || lifecycle !== lifecycleGeneration) return;
    await persistQuickDraftSnapshot(draftId, sessionJson, {
      phase,
      ...workspaceFacades(workspaceState, view),
      poolSortMode: state.poolSortMode,
      poolPanelOpen: state.poolPanelOpen,
      workspace: workspaceState,
    }, makeMeta(state, phase));
  } catch (error) {
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
      readonly placeInstanceIds: readonly [string] | readonly [string, string];
      readonly destination: DraftPickDestination;
      readonly placementHint?: DraftPickPlacementHint;
      readonly patch: WorkspaceInstallPatch;
      readonly persistence: "schedule";
    }
  | {
      readonly kind: "acknowledged-auto-pick";
      readonly authoritativeView: DraftPlayerView;
      readonly baseWorkspace: DraftWorkspaceState;
      readonly addedInstanceId: string;
      readonly placementHint?: DraftPickPlacementHint;
      readonly patch: WorkspaceInstallPatch;
      readonly persistence: "schedule";
    };

function installWorkspace(operation: WorkspaceInstallOperation): void {
  let workspace = reconcileWorkspaceState(
    operation.baseWorkspace,
    operation.authoritativeView.pool,
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
      workspace = applyDestination(
        workspace,
        operation.authoritativeView.pool,
        [operation.addedInstanceId],
        "deck",
        operation.placementHint,
      );
      break;
  }
  invalidateWorkspaceDependents();
  useDraftStore.setState({
    ...operation.patch,
    view: operation.authoritativeView,
    workspaceState: workspace,
  });
  if (operation.persistence === "schedule") schedulePersistence(0);
}

async function prepareCardDatabase(required: boolean): Promise<string | null> {
  if (!required) return null;
  const response = await fetch(__CARD_DATA_URL__);
  return response.text();
}

async function startLocalDraft(input: {
  setCode: string;
  setName: string;
  difficulty: number;
  kind: LocalDraftKind;
  prepareDatabase: boolean;
  initialize: Parameters<typeof withDraftEngineOperation<DraftPlayerView>>[0];
}): Promise<void> {
  const lifecycle = beginLifecycle();
  try {
    await Promise.all([drainDraftEngineOperations(), drainQuickDraftPersistence()]);
    if (lifecycle !== lifecycleGeneration) return;
    await inspectActiveQuickDraftLifecycle("consume");
    if (lifecycle !== lifecycleGeneration) return;
    const database = await prepareCardDatabase(input.prepareDatabase);
    const adapter = new DraftAdapter();
    const view = await withDraftEngineOperation((lease) => {
      if (lifecycle !== lifecycleGeneration) throw new Error("Stale draft start");
      if (database !== null) lease.loadCardDatabase(database);
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
      readonly instanceIds: readonly [string, string];
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
      if (!Array.isArray(request.instanceIds) || request.instanceIds.length !== 2
        || typeof request.effectCardInstanceId !== "string"
        || request.effectCardInstanceId.length === 0
        || !validPlacementHint(request.placementHint)) return false;
      const [first, second] = request.instanceIds;
      return typeof first === "string" && typeof second === "string"
        && first.length > 0 && second.length > 0 && first !== second
        && first !== request.effectCardInstanceId && second !== request.effectCardInstanceId;
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

function acknowledgedAutoPickId(
  before: ReadonlyMap<string, number>,
  after: ReadonlyMap<string, number>,
): string | null {
  const ids = new Set([...before.keys(), ...after.keys()]);
  let addedInstanceId: string | null = null;
  for (const instanceId of ids) {
    const previous = before.get(instanceId) ?? 0;
    const current = after.get(instanceId) ?? 0;
    const delta = current - previous;
    if (delta === 0) continue;
    if (delta !== 1 || previous !== 0 || addedInstanceId !== null) return null;
    addedInstanceId = instanceId;
  }
  return addedInstanceId;
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

async function performPick(request: PickRequest): Promise<DraftPickOutcome> {
  if (!validPickRequest(request)) return { status: "rejected", reason: "invalid-request" };
  if (request.kind === "draft-effect") {
    request = {
      ...request,
      instanceIds: [request.instanceIds[0], request.instanceIds[1]],
    };
  }
  const token = admitExclusive("pick");
  if (!token) return { status: "ignored", reason: "busy" };
  const state = useDraftStore.getState();
  const { adapter, draftId, workspaceState, view } = state;
  if (!adapter || !draftId || !workspaceState || !view) {
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
    const nextView = await withDraftEngineOperation((lease) => {
      if (!isFresh()) {
        throw new Error("Stale draft pick request");
      }
      switch (request.kind) {
        case "pick":
          return lease.submitPick(request.instanceId);
        case "draft-effect": {
          const adapterInstanceIds = [...request.instanceIds];
          return lease.submitPickWithDraftEffect(request.effectCardInstanceId, adapterInstanceIds);
        }
        case "auto-pick":
          return lease.autoPick();
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
        const addedInstanceId = acknowledgedAutoPickId(before, after);
        if (addedInstanceId === null) {
          cleanup();
          return { status: "rejected", reason: "unacknowledged" };
        }
        operation = {
          kind: "acknowledged-auto-pick",
          authoritativeView: nextView,
          baseWorkspace: workspaceState,
          addedInstanceId,
          placementHint: request.placementHints?.[addedInstanceId],
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

function unresolvedStageMatches(
  run: DraftRunState,
  draftId: string,
  format: DraftRunFormat,
  playerDeck: string[],
): boolean {
  const stage = run.activeMatch;
  return !!stage
    && stage.draftId === draftId
    && stage.format === run.format && run.format === format
    && arraysEqual(run.playerDeck, playerDeck)
    && arraysEqual(run.opponentDeck, stage.opponentDeck)
    && run.usedBotSeats.includes(stage.botSeat)
    && !run.results.some((entry) => entry.gameId === stage.gameId)
    && stage.resultCountAtLaunch === run.results.length;
}

function matchPayload(playerDeck: string[], opponentDeck: string[]): DraftMatchPayload {
  return {
    player: { main_deck: playerDeck, sideboard: [], commander: [] },
    opponent: { main_deck: opponentDeck, sideboard: [], commander: [] },
    ai_decks: [],
  };
}

function pickBotSeat(usedSeats: number[], view: DraftPlayerView): number {
  const botSeats = view.seats.filter((seat) => seat.is_bot).map((seat) => seat.seat_index);
  const candidates = botSeats.length > 0 ? botSeats : [1, 2, 3, 4, 5, 6, 7];
  const available = candidates.filter((seat) => !usedSeats.includes(seat));
  const choices = available.length > 0 ? available : candidates;
  return choices[Math.floor(Math.random() * choices.length)] ?? 1;
}

function expandSuggestedDeck(deck: SuggestedDeck): string[] {
  return [...deck.main_deck, ...Object.entries(deck.lands).flatMap(([name, count]) =>
    Array<string>(normalizeVirtualBasicCount(count)).fill(name))];
}

function navigateToMatch(
  state: DraftStoreState,
  gameId: string,
  navigate: (path: string) => void,
): void {
  const matchType = state.view?.match_config.match_type === "Bo3" && state.runFormat === "bo3" ? "bo3" : "bo1";
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
    prepareDatabase: difficulty >= 3,
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
    prepareDatabase: true,
    initialize: (lease) => lease.initializeSealed(setPoolJson, difficulty, Math.floor(Math.random() * 0xffffffff)),
    });
  },

  startCubeDraft: (cubeListText, cubeName, settings, difficulty) => startLocalDraft({
    setCode: "custom-cube",
    setName: cubeName,
    difficulty,
    kind: "Quick",
    prepareDatabase: true,
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
    let resumeId: string | null = null;
    try {
      await Promise.all([drainDraftEngineOperations(), drainQuickDraftPersistence()]);
      const meta = await inspectActiveQuickDraftLifecycle("inspect");
      if (!meta || lifecycle !== lifecycleGeneration) return;
      resumeId = meta.id;
      const [saved, run] = await Promise.all([loadQuickDraftSession(meta.id), loadDraftRun(meta.id)]);
      if (!saved || ((meta.phase === "playing" || meta.phase === "complete") && !run)) {
        await cleanupQuickDraftLifecycle(meta.id);
        return;
      }
      const database = await prepareCardDatabase(meta.difficulty >= 3 || meta.kind === "Sealed");
      const adapter = new DraftAdapter();
      const view = await withDraftEngineOperation((lease) => {
        if (lifecycle !== lifecycleGeneration) throw new Error("Stale draft resume");
        if (database !== null) lease.loadCardDatabase(database);
        if (lifecycle !== lifecycleGeneration) throw new Error("Stale draft resume");
        return lease.importSession(saved.sessionJson, meta.difficulty);
      });
      if (lifecycle !== lifecycleGeneration) return;
      // The durable run is authoritative for run phases: the run and the meta
      // are persisted as separate writes (publishInitialDraftMatch /
      // recordDraftMatchResult save the run first), so a crash between them
      // leaves the meta stale — it may still say "launching" for an already
      // active run, or "playing" for a terminal one. The run's results vs its
      // limits decide; the meta's phase only matters before a run exists.
      const resumedPhase: DraftPhase = run ? draftRunPhase(run) : meta.phase;
      installWorkspace({
        kind: "state",
        authoritativeView: view,
        baseWorkspace: saved.workspace ?? migrateLegacyWorkspace(view.pool, saved),
        patch: {
          draftId: meta.id,
          adapter,
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
    } catch (error) {
      if (lifecycle !== lifecycleGeneration) return;
      if (lifecycle === lifecycleGeneration) {
        if (resumeId) await cleanupQuickDraftLifecycle(resumeId);
      }
      throw error;
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
    if (get().pickInteractionLocked) return;
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
    if (workspaceMutationBlocked(state) || !state.adapter || !state.workspaceState || !state.view) return;
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
        return lease.suggestDeck();
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
    if (workspaceMutationBlocked(state) || !state.adapter || !state.workspaceState || !state.view) return;
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
        return lease.suggestLands(workspaceFacades(state.workspaceState!, state.view!).mainDeck);
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
    if (!state.adapter || !state.workspaceState || !state.view) {
      retireExclusive(token);
      return;
    }
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    try {
      const view = await withDraftEngineOperation((lease) => {
        if (!isExclusive(token, "submit") || lifecycle !== lifecycleGeneration || revision !== workspaceRevision) {
          throw new Error("Stale draft deck submission");
        }
        return lease.submitDeck(projectDeckNames(state.workspaceState!, state.view!.pool), []);
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
  setRunFormat: (runFormat) => { set({ runFormat }); schedulePersistence(); },

  launchMatch: async (navigate) => {
    const token = admitExclusive("launch");
    if (!token) return;
    cancelScheduledPersistence();
    const state = get();
    if (!state.adapter || !state.draftId || !state.selectedSet || !state.workspaceState || !state.view) {
      retireExclusive(token);
      return;
    }
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    const playerDeck = projectDeckNames(state.workspaceState, state.view.pool);
    const legacyFacades = workspaceFacades(state.workspaceState, state.view);
    try {
      const durableRun = await loadDraftRun(state.draftId);
      if (!isExclusive(token, "launch") || lifecycle !== lifecycleGeneration || revision !== workspaceRevision) return;
      let run: DraftRunState;
      let sessionJson: string | null = null;
      if (durableRun) {
        if (!unresolvedStageMatches(durableRun, state.draftId, state.runFormat, playerDeck)
          || durableRun.results.length !== 0) throw new Error("Conflicting staged draft match");
        run = durableRun;
      } else {
        const botSeat = pickBotSeat([], state.view);
        const prepared = await withDraftEngineOperation((lease) => {
          if (!isExclusive(token, "launch") || lifecycle !== lifecycleGeneration || revision !== workspaceRevision) {
            throw new Error("Stale draft match launch");
          }
          return { sessionJson: lease.exportSession(), botDeck: lease.getBotDeck(botSeat) };
        });
        sessionJson = prepared.sessionJson;
        const opponentDeck = expandSuggestedDeck(prepared.botDeck);
        const gameId = crypto.randomUUID();
        run = {
          format: state.runFormat,
          results: [],
          playerDeck,
          opponentDeck,
          usedBotSeats: [botSeat],
          activeMatch: {
            draftId: state.draftId,
            gameId,
            format: state.runFormat,
            resultCountAtLaunch: 0,
            botSeat,
            opponentDeck,
          },
        };
      }
      const gameId = run.activeMatch!.gameId;
      const localState = { ...state, runState: run };
      const meta = makeMeta(localState, "playing", gameId);
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
          payload: matchPayload(run.playerDeck, run.opponentDeck),
          meta,
        });
      } else {
        await publishStagedDraftMatch({
          draftId: state.draftId,
          gameId,
          payload: matchPayload(run.playerDeck, run.opponentDeck),
          meta,
        });
      }
      if (!isExclusive(token, "launch") || lifecycle !== lifecycleGeneration || revision !== workspaceRevision) return;
      set({ phase: "playing", runState: run });
      navigateToMatch({ ...get(), runState: run }, gameId, navigate);
      retireExclusive(token);
    } catch (error) {
      retireExclusive(token);
      throw error;
    }
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
    if (!state.adapter || !state.draftId || !state.selectedSet || !state.workspaceState || !state.view) {
      retireExclusive(token);
      return;
    }
    const lifecycle = lifecycleGeneration;
    const revision = workspaceRevision;
    try {
      const durableRun = await loadDraftRun(state.draftId);
      if (!durableRun) throw new Error("Missing durable draft run");
      const playerDeck = projectDeckNames(state.workspaceState, state.view.pool);
      if (draftRunPhase(durableRun) === "complete") throw new Error("Draft run is complete");
      let run = durableRun;
      let saveRun = false;
      if (durableRun.activeMatch) {
        if (!unresolvedStageMatches(durableRun, state.draftId, state.runFormat, playerDeck)) {
          throw new Error("Conflicting staged draft match");
        }
      } else {
        const botSeat = pickBotSeat(durableRun.usedBotSeats, state.view);
        const botDeck = await withDraftEngineOperation((lease) => {
          if (!isExclusive(token, "launch") || lifecycle !== lifecycleGeneration || revision !== workspaceRevision) {
            throw new Error("Stale next match launch");
          }
          return lease.getBotDeck(botSeat);
        });
        const opponentDeck = expandSuggestedDeck(botDeck);
        const gameId = crypto.randomUUID();
        const usedBotSeats = durableRun.usedBotSeats.includes(botSeat)
          ? durableRun.usedBotSeats
          : [...durableRun.usedBotSeats, botSeat];
        run = {
          ...durableRun,
          opponentDeck,
          usedBotSeats,
          activeMatch: {
            draftId: state.draftId,
            gameId,
            format: state.runFormat,
            resultCountAtLaunch: durableRun.results.length,
            botSeat,
            opponentDeck,
          },
        };
        saveRun = true;
      }
      const gameId = run.activeMatch!.gameId;
      const meta = makeMeta({ ...state, runState: run }, "playing", gameId);
      await publishStagedDraftMatch({
        draftId: state.draftId,
        run: saveRun ? run : undefined,
        gameId,
        payload: matchPayload(run.playerDeck, run.opponentDeck),
        meta,
      });
      if (!isExclusive(token, "launch") || lifecycle !== lifecycleGeneration || revision !== workspaceRevision) return;
      set({ phase: "playing", runState: run });
      navigateToMatch({ ...get(), runState: run }, gameId, navigate);
      retireExclusive(token);
    } catch (error) {
      retireExclusive(token);
      throw error;
    }
  },

  endRun: async () => {
    const id = get().draftId;
    beginLifecycle();
    if (id) await cleanupQuickDraftLifecycle(id);
  },

  reset: () => {
    beginLifecycle();
  },
}));
