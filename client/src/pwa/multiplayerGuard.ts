import { isAuthorityRemote, useGameStore } from "../stores/gameStore";
import { isMultiplayerDraftPodLive, useMultiplayerDraftStore } from "../stores/multiplayerDraftStore";

function isRemoteGameLive(): boolean {
  const { gameMode, gameState, adapter } = useGameStore.getState();
  return isAuthorityRemote(gameMode) && adapter !== null && gameState?.waiting_for?.type !== "GameOver";
}

function isDraftPodLive(): boolean {
  return isMultiplayerDraftPodLive(useMultiplayerDraftStore.getState());
}

/**
 * True when a multiplayer game is live in this tab and reloading would
 * drop the P2P/WebSocket connection mid-game.
 *
 * Covers:
 * - Active MP game with a `gameState` (waiting_for !== GameOver).
 * - Pre-game P2P lobby (adapter attached, no gameState yet) — reloading
 *   here drops the user from the lobby.
 *
 * Used by both the service-worker updater (web) and the Tauri updater
 * (desktop) to defer activation/relaunch until the game ends.
 */
export function isMultiplayerGameLive(): boolean {
  return isRemoteGameLive() || isDraftPodLive();
}

export type DeferredMultiplayerActionKind = "activation" | "reload" | "install" | "observer";

interface QueuedMultiplayerAction {
  id: number;
  kind: DeferredMultiplayerActionKind;
  action: () => void;
}

const ACTION_PRIORITY: Record<DeferredMultiplayerActionKind, number> = {
  activation: 0,
  reload: 1,
  install: 2,
  observer: 3,
};

let nextActionId = 0;
let queuedActions: QueuedMultiplayerAction[] = [];
let gameUnsubscribe: (() => void) | null = null;
let draftUnsubscribe: (() => void) | null = null;

function stopLivenessSubscription(): void {
  gameUnsubscribe?.();
  draftUnsubscribe?.();
  gameUnsubscribe = null;
  draftUnsubscribe = null;
}

function releaseQueuedActions(): void {
  if (isMultiplayerGameLive() || queuedActions.length === 0) return;

  // Stop watching before actions run: an activation can synchronously trigger
  // a controller-change event, which must either run immediately (the pod is
  // over) or create a fresh wait for a newly-live session.
  stopLivenessSubscription();
  const ready = queuedActions
    .sort((left, right) => ACTION_PRIORITY[left.kind] - ACTION_PRIORITY[right.kind] || left.id - right.id);
  queuedActions = [];
  for (const queued of ready) queued.action();
}

function ensureLivenessSubscription(): void {
  if (gameUnsubscribe || draftUnsubscribe) return;

  // Both stores use plain Zustand subscriptions. Re-evaluating the combined
  // predicate prevents a game-to-pod handoff from applying an update between
  // the two lifecycles.
  const recheck = () => releaseQueuedActions();
  gameUnsubscribe = useGameStore.subscribe(recheck);
  draftUnsubscribe = useMultiplayerDraftStore.subscribe(recheck);

  // Close the guard/check → subscribe TOCTOU window after *both* listeners
  // exist. `releaseQueuedActions` tears both down before it invokes work.
  releaseQueuedActions();
}

function queueUntilMultiplayerSessionEnds(
  action: () => void,
  kind: DeferredMultiplayerActionKind,
): { deferred: boolean; cancel: () => void } {
  if (!isMultiplayerGameLive()) {
    action();
    return { deferred: false, cancel: NOOP };
  }

  const id = nextActionId++;
  queuedActions.push({ id, kind, action });
  ensureLivenessSubscription();

  let active = queuedActions.some((queued) => queued.id === id);
  const cancel = () => {
    if (!active) return;
    active = false;
    queuedActions = queuedActions.filter((queued) => queued.id !== id);
    if (queuedActions.length === 0) stopLivenessSubscription();
  };
  return { deferred: active, cancel };
}

/**
 * Register a one-shot callback for the end of every live remote game and
 * draft-pod session. Kept for callers that only need an observer; update
 * actions should use `deferUntilMultiplayerSessionEnds` with an explicit kind.
 */
export function whenMultiplayerGameEnds(callback: () => void): () => void {
  return queueUntilMultiplayerSessionEnds(callback, "observer").cancel;
}

export interface DeferredMultiplayerAction {
  /** True when the action was parked behind a live remote game or draft pod. */
  readonly deferred: boolean;
  /** Cancels a parked action. It is safe to call after the action has fired. */
  cancel(): void;
}

const NOOP = () => {};

/**
 * Run `action` immediately when no remote session is live, otherwise once
 * the last live game or draft pod ends. The returned cancellation handle and
 * the one-shot subscription form a single ownership unit, so consumers never
 * retain a stale callback after it executes or is superseded.
 */
export function deferUntilMultiplayerSessionEnds(
  action: () => void,
  kind: DeferredMultiplayerActionKind = "reload",
): DeferredMultiplayerAction {
  return queueUntilMultiplayerSessionEnds(action, kind);
}
