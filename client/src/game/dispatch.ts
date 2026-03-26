import type { GameAction, GameEvent, GameState } from "../adapter/types";
import { normalizeEvents } from "../animation/eventNormalizer";
import { getPlayerId } from "../hooks/usePlayerId";
import type { AnimationStep } from "../animation/types";
import { SPEED_MULTIPLIERS } from "../animation/types";
import { audioManager } from "../audio/AudioManager";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { useAnimationStore } from "../stores/animationStore";
import { useGameStore, saveGame, saveCheckpoints } from "../stores/gameStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";
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
  resolve: () => void;
  reject: (err: unknown) => void;
}

interface PendingRemoteUpdate {
  kind: "remote";
  state: GameState;
  events: GameEvent[];
  legalActions: GameAction[];
  resolve: () => void;
  reject: (err: unknown) => void;
}

type PendingWork = PendingLocalAction | PendingRemoteUpdate;

/** Module-level mutex — replaces useRef from the hook version. */
let isAnimating = false;

/** Unified queue for local actions and remote state updates. */
const pendingQueue: PendingWork[] = [];

async function processAction(action: GameAction): Promise<void> {
  const { adapter, gameState } = useGameStore.getState();
  if (!adapter || !gameState) {
    throw new Error("Game not initialized");
  }

  // 1. Capture snapshot before WASM call
  const snapshot = useAnimationStore.getState().captureSnapshot();
  currentSnapshot = snapshot;

  // 2. Save undo history if applicable
  const shouldSaveHistory = UNDOABLE_ACTIONS.has(action.type);

  // 3. Call WASM — get events without updating state yet
  const result = await adapter.submitAction(action);
  const events: GameEvent[] = result.events;

  // 3b. Fetch new state eagerly and persist before animations so a mid-animation
  //     page reload (e.g. PWA service-worker update) doesn't lose the latest state.
  const newState = await adapter.getState();
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
    const gamePlayerCount = useGameStore.getState().gameState?.players.length ?? 2;
    let bannerText: string;
    if (turnPlayerId === myId) {
      bannerText = "YOUR TURN";
    } else if (gamePlayerCount > 2) {
      const oppName = useMultiplayerStore.getState().opponentDisplayName;
      bannerText = `${oppName ?? `OPP ${turnPlayerId + 1}`}'S TURN`;
    } else {
      const oppName = useMultiplayerStore.getState().opponentDisplayName;
      bannerText = oppName ? `${oppName}'S TURN` : "THEIR TURN";
    }
    useUiStore.getState().flashTurnBanner(bannerText);
  }

  // 6. Normalize events into animation steps
  const combatPacing = usePreferencesStore.getState().combatPacing;
  const steps = normalizeEvents(events, { combatPacing });

  // 7. Play animations (unless instant)
  const speed = usePreferencesStore.getState().animationSpeed;
  const multiplier = SPEED_MULTIPLIERS[speed];

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

  // 8. Update game state (deferred after animations — state already fetched in step 3b)
  const legalActions = await adapter.getLegalActions();

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
      legalActions,
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
        await processAction(next.action);
      } else {
        await processRemoteUpdateInner(next.state, next.events, next.legalActions);
      }
      next.resolve();
    } catch (err) {
      next.reject(err);
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
export async function dispatchAction(action: GameAction): Promise<void> {
  if (isAnimating) {
    return new Promise<void>((resolve, reject) => {
      pendingQueue.push({ kind: "local", action, resolve, reject });
    });
  }

  isAnimating = true;
  try {
    await processAction(action);
  } finally {
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
  legalActions: GameAction[],
): Promise<void> {
  // 1. Capture snapshot before updating state (for position lookups during animation)
  const snapshot = useAnimationStore.getState().captureSnapshot();
  currentSnapshot = snapshot;

  // 2. Flash turn banner
  const turnEvent = events.find((e) => e.type === "TurnStarted");
  if (turnEvent && "data" in turnEvent) {
    const turnPlayerId = (turnEvent.data as { player_id: number }).player_id;
    const myId = getPlayerId();
    const gamePlayerCount = useGameStore.getState().gameState?.players.length ?? 2;
    let bannerText: string;
    if (turnPlayerId === myId) {
      bannerText = "YOUR TURN";
    } else if (gamePlayerCount > 2) {
      const oppName = useMultiplayerStore.getState().opponentDisplayName;
      bannerText = `${oppName ?? `OPP ${turnPlayerId + 1}`}'S TURN`;
    } else {
      const oppName = useMultiplayerStore.getState().opponentDisplayName;
      bannerText = oppName ? `${oppName}'S TURN` : "THEIR TURN";
    }
    useUiStore.getState().flashTurnBanner(bannerText);
  }

  // 3. Normalize events into animation steps
  const combatPacing = usePreferencesStore.getState().combatPacing;
  const steps = normalizeEvents(events, { combatPacing });

  // 4. Play animations (unless instant)
  const speed = usePreferencesStore.getState().animationSpeed;
  const multiplier = SPEED_MULTIPLIERS[speed];

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
    legalActions,
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
  legalActions: GameAction[],
): Promise<void> {
  if (isAnimating) {
    return new Promise<void>((resolve, reject) => {
      pendingQueue.push({ kind: "remote", state, events, legalActions, resolve, reject });
    });
  }

  isAnimating = true;
  try {
    await processRemoteUpdateInner(state, events, legalActions);
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

  const legalActions = await adapter.getLegalActions();
  useGameStore.setState({
    gameState: state,
    waitingFor: state.waiting_for,
    legalActions,
    events: [],
  });

  return null;
}
