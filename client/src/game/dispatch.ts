import type { GameAction, GameEvent, GameState, LegalActionsResult } from "../adapter/types";
import { AdapterError, AdapterErrorCode } from "../adapter/types";
import { attemptStateRehydrate, isEnginePanic, notifyEngineLost, routePanic } from "./engineRecovery";
import { normalizeEvents } from "../animation/eventNormalizer";
import { getPlayerId } from "../hooks/usePlayerId";
import type { AnimationStep } from "../animation/types";
import { audioManager } from "../audio/AudioManager";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { debugLog } from "./debugLog";
import { useAnimationStore } from "../stores/animationStore";
import { isMultiplayerMode, useGameStore, legalResultState, saveGame, saveCheckpoints } from "../stores/gameStore";
import { getOpponentDisplayName } from "../stores/multiplayerStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { useUiStore } from "../stores/uiStore";

/**
 * Event types whose SFX is deferred to the card slam onImpact callback
 * in AnimationOverlay, so sound aligns with the visual impact moment.
 */
const SLAM_DEFERRED_SFX = new Set(["DamageDealt"]);

/** Schedule SFX for each animation step, offset to sync with visual timing. */
function scheduleSfxForSteps(steps: AnimationStep[], multiplier: number): void {
  let offset = 0;
  for (const step of steps) {
    // Filter out slam-deferred events — their SFX fires at impact time instead
    const immediate = step.effects.filter((e) => !SLAM_DEFERRED_SFX.has(e.event.type));
    if (immediate.length > 0) {
      if (offset === 0) {
        audioManager.playSfxForStep(immediate);
      } else {
        const delay = offset;
        setTimeout(() => audioManager.playSfxForStep(immediate), delay);
      }
    }
    offset += step.duration * multiplier;
  }
}

/**
 * Module-level position snapshot for AnimationOverlay position lookups.
 */
export let currentSnapshot = useAnimationStore.getState().captureSnapshot();

interface PendingLocalAction {
  kind: "local";
  action: GameAction;
  actor: number;
  resolve: () => void;
  reject: (err: unknown) => void;
}

interface PendingRemoteUpdate {
  kind: "remote";
  state: GameState;
  events: GameEvent[];
  legalResult: LegalActionsResult;
  resolve: () => void;
  reject: (err: unknown) => void;
}

type PendingWork = PendingLocalAction | PendingRemoteUpdate;

/** Module-level mutex — replaces useRef from the hook version. */
let isAnimating = false;

/** Unified queue for local actions and remote state updates. */
const pendingQueue: PendingWork[] = [];

/**
 * The local action currently being processed (set while inside processAction).
 * Used alongside pendingQueue to deduplicate rapid double-clicks: if the same
 * action is already in flight, a second dispatch is a silent no-op rather than
 * a queued duplicate that would fail against a transitioned engine state.
 */
let inFlightLocalAction: GameAction | null = null;

/** Structural equality for GameAction — action objects are small plain JSON. */
function actionsEqual(a: GameAction, b: GameAction): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function isStateLost(err: unknown): boolean {
  return err instanceof AdapterError && err.code === AdapterErrorCode.STATE_LOST;
}

async function processAction(action: GameAction, actor: number): Promise<void> {
  const { adapter, gameState } = useGameStore.getState();
  if (!adapter || !gameState) {
    debugLog("processAction called with no adapter or gameState");
    throw new Error("Game not initialized");
  }

  // 1. Capture snapshot before WASM call
  const snapshot = useAnimationStore.getState().captureSnapshot();
  currentSnapshot = snapshot;

  // 2. Save undo history if applicable. Gated off in multiplayer because
  // rewinding a single client's view desyncs from the authoritative game
  // state on the wire.
  const { gameMode } = useGameStore.getState();
  const shouldSaveHistory =
    UNDOABLE_ACTIONS.has(action.type) && !isMultiplayerMode(gameMode);

  // 3. Call WASM — get events without updating state yet.
  // `actor` is the authenticated seat ID of whoever initiated this dispatch
  // (local human from `getPlayerId()`, or an AI seat from `aiController`).
  // The engine's guard rejects any action whose actor doesn't match the
  // authorized submitter — so passing the local human's ID during the AI's
  // turn correctly fails instead of silently applying as the AI.
  // If the engine reports STATE_LOST (thread-local cleared between calls —
  // PWA update desync, worker restart, etc.), transparently rehydrate from
  // the store snapshot and retry once. Safe because submitAction fails
  // before mutating any engine state when the cell is None.
  let result;
  try {
    result = await adapter.submitAction(action, actor);
  } catch (err) {
    // Engine panic: re-running the same action against the same state is
    // guaranteed to re-panic (the previous "ai-getAction-retry" / similar
    // failure modes were caused by exactly this loop). Surface the captured
    // panic message immediately instead of attempting recovery.
    if (isEnginePanic(err)) {
      // Try rehydrate — if the panic was in a side path and engine state
      // survived, downgrade to a non-fatal toast and let the user keep
      // playing. Only the true state-loss path triggers the blocking modal.
      await routePanic("submitAction-panic", err.panic);
      throw err;
    }
    if (!isStateLost(err)) throw err;
    debugLog(`processAction: STATE_LOST on ${action.type}; attempting rehydrate`, "warn");
    const recovered = await attemptStateRehydrate();
    if (!recovered) {
      notifyEngineLost("submitAction");
      throw err;
    }
    // Recovery reported success but the underlying worker restoreState is
    // fire-and-forget from the adapter (void return, async worker). If the
    // restore silently failed — e.g., MULTIPLAYER_MODE refused it, the worker
    // crashed mid-restore — this retry will throw STATE_LOST again. Catch
    // that explicitly and surface via Layer 3 rather than letting the error
    // escape uncaught.
    try {
      result = await adapter.submitAction(action, actor);
    } catch (retryErr) {
      // Prefer the captured panic message over the bare retry tag — that's
      // the "diagnostic: submitAction-retry" the user reported, which told
      // them nothing actionable.
      if (isEnginePanic(retryErr)) {
        await routePanic("submitAction-retry-panic", retryErr.panic);
      } else {
        notifyEngineLost("submitAction-retry");
      }
      throw retryErr;
    }
  }
  const events: GameEvent[] = result.events;

  // 3b. Fetch new state eagerly and persist before animations so a mid-animation
  //     page reload (e.g. PWA service-worker update) doesn't lose the latest state.
  // Recover from STATE_LOST here too — a worker restart could happen between
  // submitAction and getState. Critically: if recovery fails, do NOT call
  // saveGame — earlier revisions silently wrote a default empty GameState to
  // IDB on null, corrupting the checkpoint we now rely on for Layer 3 reload.
  let newState: GameState;
  try {
    newState = await adapter.getState();
  } catch (err) {
    if (isEnginePanic(err)) {
      await routePanic("getState-panic", err.panic);
      throw err;
    }
    if (!isStateLost(err)) throw err;
    debugLog("processAction: STATE_LOST on getState; attempting rehydrate", "warn");
    const recovered = await attemptStateRehydrate();
    if (!recovered) {
      notifyEngineLost("getState");
      throw err;
    }
    try {
      newState = await adapter.getState();
    } catch (retryErr) {
      if (isEnginePanic(retryErr)) {
        notifyEngineLost("getState-retry-panic", retryErr.panic);
      } else {
        notifyEngineLost("getState-retry");
      }
      throw retryErr;
    }
  }
  const { gameId } = useGameStore.getState();
  if (gameId) saveGame(gameId, newState);

  // 4. Checkpoint: save pre-action state on turn boundaries for debug restore
  const turnEvent = events.find((e) => e.type === "TurnStarted");
  if (turnEvent) {
    const prev = useGameStore.getState();
    const updated = [...prev.turnCheckpoints, gameState].slice(-MAX_UNDO_HISTORY);
    useGameStore.setState({ turnCheckpoints: updated });
    if (prev.gameId) saveCheckpoints(prev.gameId, updated);
  }

  // 5. Flash turn banner directly (bypasses animation queue for reliability)
  if (turnEvent && "data" in turnEvent) {
    const turnPlayerId = (turnEvent.data as { player_id: number }).player_id;
    const myId = getPlayerId();
    let bannerText: string;
    if (turnPlayerId === myId) {
      bannerText = "YOUR TURN";
    } else {
      const oppName = getOpponentDisplayName(turnPlayerId);
      bannerText = `${oppName.toUpperCase()}'S TURN`;
    }
    // CR 500: per-player turn count (skipped turns excluded). Engine increments
    // turns_taken before TurnStarted fires, so newState already has the value.
    const turnNumber = newState.players[turnPlayerId]?.turns_taken ?? 1;
    useUiStore.getState().flashTurnBanner(bannerText, turnNumber);
  }

  // 6. Normalize events into animation steps
  const pacingMultipliers = usePreferencesStore.getState().pacingMultipliers;
  const steps = normalizeEvents(events, { pacingMultipliers });

  // 7. Play animations (unless instant — multiplier === 0)
  const multiplier = usePreferencesStore.getState().animationSpeedMultiplier;

  if (steps.length > 0 && multiplier > 0) {
    useAnimationStore.getState().enqueueSteps(steps);

    // Schedule SFX synced with each step's visual timing
    scheduleSfxForSteps(steps, multiplier);

    // Wait for total animation duration
    const totalDuration = steps.reduce(
      (sum, step) => sum + step.duration * multiplier,
      0,
    );
    await new Promise<void>((resolve) => setTimeout(resolve, totalDuration));
  } else if (steps.length > 0) {
    // Instant speed: fire all SFX immediately
    for (const step of steps) {
      audioManager.playSfxForStep(step.effects);
    }
  }

  // 8. Update game state (deferred after animations — state already fetched in step 3b).
  // Engine state could have been lost during the animation window; rehydrate
  // once if needed so the UI doesn't render empty legal actions.
  let legalResult;
  try {
    legalResult = await adapter.getLegalActions();
  } catch (err) {
    if (isEnginePanic(err)) {
      notifyEngineLost("getLegalActions-panic", err.panic);
      throw err;
    }
    if (!isStateLost(err)) throw err;
    const recovered = await attemptStateRehydrate();
    if (!recovered) {
      notifyEngineLost("getLegalActions");
      throw err;
    }
    try {
      legalResult = await adapter.getLegalActions();
    } catch (retryErr) {
      if (isEnginePanic(retryErr)) {
        notifyEngineLost("getLegalActions-retry-panic", retryErr.panic);
      } else {
        notifyEngineLost("getLegalActions-retry");
      }
      throw retryErr;
    }
  }

  useGameStore.setState((prev) => {
    const newHistory = shouldSaveHistory
      ? [...prev.stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
      : prev.stateHistory;

    // Assign monotonic sequence numbers to new log entries
    let seq = prev.nextLogSeq;
    const newLogEntries = (result.log_entries ?? []).map((entry) => ({
      ...entry,
      seq: seq++,
    }));

    return {
      gameState: newState,
      events,
      eventHistory: [...prev.eventHistory, ...events].slice(-1000),
      logHistory: [...prev.logHistory, ...newLogEntries].slice(-2000),
      nextLogSeq: seq,
      waitingFor: newState.waiting_for,
      ...legalResultState(legalResult),
      stateHistory: newHistory,
    };
  });

  // Play victory/defeat stinger on GameOver
  const gameOverEvent = events.find((e) => e.type === "GameOver");
  if (gameOverEvent && gameOverEvent.type === "GameOver") {
    const winner = (gameOverEvent.data as { winner: number | null }).winner;
    if (winner === null) {
      // Draw — just fade out
      audioManager.stopMusic(2.0);
    } else {
      const myId = getPlayerId();
      audioManager.playStinger(winner === myId ? "victory" : "defeat");
    }
  }
}

async function processQueue(): Promise<void> {
  while (pendingQueue.length > 0) {
    const next = pendingQueue.shift()!;
    try {
      if (next.kind === "local") {
        inFlightLocalAction = next.action;
        try {
          await processAction(next.action, next.actor);
        } finally {
          inFlightLocalAction = null;
        }
      } else {
        await processRemoteUpdateInner(next.state, next.events, next.legalResult);
      }
      next.resolve();
    } catch (err) {
      debugLog(`processQueue error (${next.kind}): ${err instanceof Error ? err.message : String(err)}`);
      next.reject(err);
      // If processAction escalated to Layer 3 (notifyEngineLost already
      // fired), drain the rest of the queue with the same error. Without
      // this, each remaining item would attempt its own recovery, each
      // one failing and re-firing notifyEngineLost — the modal is
      // de-duped but the log becomes noisy and we waste cycles on doomed
      // rehydrates. User is about to reload; nothing in this queue is
      // going to succeed.
      if (isStateLost(err) || isEnginePanic(err)) {
        // Drain on ENGINE_PANIC too: each queued action would otherwise hit
        // its own catch + (no-op) recovery + re-throw, doubling the noise
        // for an unrecoverable failure. The first item already fired
        // notifyEngineLost with the captured panic.
        while (pendingQueue.length > 0) {
          const stale = pendingQueue.shift()!;
          stale.reject(err);
        }
        break;
      }
    }
  }
  isAnimating = false;
}

/**
 * Standalone dispatch function with snapshot-animate-update flow.
 *
 * Flow per dispatch:
 * 1. Mutex gate — queue if already animating
 * 2. Capture snapshot of all card positions
 * 3. Call WASM via adapter.submitAction
 * 4. Normalize events into AnimationSteps
 * 5. Play animations (unless speed is 'instant')
 * 6. Update game state in gameStore
 * 7. Release mutex, process next queued action
 */
/**
 * Dispatch `action` on behalf of `actor`. `actor` defaults to the local
 * human's seat (`getPlayerId()`); the AI controller overrides it with the
 * AI seat's PlayerId so the engine accepts the action as coming from that
 * seat instead of the human.
 *
 * The engine itself enforces `actor === authorized_submitter(state)`, so a
 * misrouted action fails cleanly rather than silently applying as the
 * wrong player.
 */
export async function dispatchAction(
  action: GameAction,
  actor: number = getPlayerId(),
): Promise<void> {
  if (isAnimating) {
    // Enqueue-time de-dup: if the exact same action is already in flight or
    // already queued, silently resolve. Covers rapid double-clicks (e.g. a
    // planeswalker ability fired twice before the first transitions the
    // engine into TargetSelection).
    if (inFlightLocalAction && actionsEqual(inFlightLocalAction, action)) {
      return;
    }
    for (const pending of pendingQueue) {
      if (pending.kind === "local" && actionsEqual(pending.action, action)) {
        return;
      }
    }
    debugLog(`dispatch queued (mutex held): ${action.type}, queue=${pendingQueue.length}`, "warn");
    return new Promise<void>((resolve, reject) => {
      pendingQueue.push({ kind: "local", action, actor, resolve, reject });
    });
  }

  isAnimating = true;
  inFlightLocalAction = action;
  try {
    await processAction(action, actor);
  } catch (e) {
    debugLog(`dispatch error for ${action.type}: ${e instanceof Error ? e.message : String(e)}`);
    throw e;
  } finally {
    inFlightLocalAction = null;
    if (pendingQueue.length > 0) {
      processQueue().catch(() => { isAnimating = false; });
    } else {
      isAnimating = false;
    }
  }
}

/**
 * Inner implementation for remote state updates — runs the animation pipeline.
 */
async function processRemoteUpdateInner(
  state: GameState,
  events: GameEvent[],
  legalResult: LegalActionsResult,
): Promise<void> {
  // 1. Capture snapshot before updating state (for position lookups during animation)
  const snapshot = useAnimationStore.getState().captureSnapshot();
  currentSnapshot = snapshot;

  // 2. Flash turn banner
  const turnEvent = events.find((e) => e.type === "TurnStarted");
  if (turnEvent && "data" in turnEvent) {
    const turnPlayerId = (turnEvent.data as { player_id: number }).player_id;
    const myId = getPlayerId();
    let bannerText: string;
    if (turnPlayerId === myId) {
      bannerText = "YOUR TURN";
    } else {
      const oppName = getOpponentDisplayName(turnPlayerId);
      bannerText = `${oppName.toUpperCase()}'S TURN`;
    }
    // CR 500: per-player turn count from the post-update state.
    const turnNumber = state.players[turnPlayerId]?.turns_taken ?? 1;
    useUiStore.getState().flashTurnBanner(bannerText, turnNumber);
  }

  // 3. Normalize events into animation steps
  const pacingMultipliers = usePreferencesStore.getState().pacingMultipliers;
  const steps = normalizeEvents(events, { pacingMultipliers });

  // 4. Play animations (unless instant — multiplier === 0)
  const multiplier = usePreferencesStore.getState().animationSpeedMultiplier;

  if (steps.length > 0 && multiplier > 0) {
    useAnimationStore.getState().enqueueSteps(steps);
    scheduleSfxForSteps(steps, multiplier);

    const totalDuration = steps.reduce(
      (sum, step) => sum + step.duration * multiplier,
      0,
    );
    await new Promise<void>((resolve) => setTimeout(resolve, totalDuration));
  } else if (steps.length > 0) {
    for (const step of steps) {
      audioManager.playSfxForStep(step.effects);
    }
  }

  // 5. Update game state after animations complete
  useGameStore.setState((prev) => ({
    gameState: state,
    events,
    eventHistory: [...prev.eventHistory, ...events].slice(-1000),
    waitingFor: state.waiting_for,
    ...legalResultState(legalResult),
  }));

  // 6. Play victory/defeat stinger on GameOver
  const gameOverEvent = events.find((e) => e.type === "GameOver");
  if (gameOverEvent && gameOverEvent.type === "GameOver") {
    const winner = (gameOverEvent.data as { winner: number | null }).winner;
    if (winner === null) {
      audioManager.stopMusic(2.0);
    } else {
      const myId = getPlayerId();
      audioManager.playStinger(winner === myId ? "victory" : "defeat");
    }
  }
}

/**
 * Process an incoming remote state update (opponent's action in multiplayer/P2P).
 * Shares the animation mutex with dispatchAction so remote updates queue behind
 * local actions and vice versa — no overlapping animations.
 */
export async function processRemoteUpdate(
  state: GameState,
  events: GameEvent[],
  legalResult: LegalActionsResult,
): Promise<void> {
  if (isAnimating) {
    return new Promise<void>((resolve, reject) => {
      pendingQueue.push({ kind: "remote", state, events, legalResult, resolve, reject });
    });
  }

  isAnimating = true;
  try {
    await processRemoteUpdateInner(state, events, legalResult);
  } finally {
    if (pendingQueue.length > 0) {
      processQueue().catch(() => { isAnimating = false; });
    } else {
      isAnimating = false;
    }
  }
}

/**
 * Restore a previously captured GameState snapshot.
 * Returns null on success, or an error message string on failure.
 */
export async function restoreGameState(state: GameState): Promise<string | null> {
  const { adapter } = useGameStore.getState();
  if (!adapter) return "No adapter available";

  try {
    adapter.restoreState(state);
  } catch (err) {
    return err instanceof Error ? err.message : "Failed to restore state";
  }

  const legalResult = await adapter.getLegalActions();
  useGameStore.setState({
    gameState: state,
    waitingFor: state.waiting_for,
    ...legalResultState(legalResult),
    events: [],
  });

  return null;
}
