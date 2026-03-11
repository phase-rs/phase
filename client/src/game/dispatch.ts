import type { GameAction, GameEvent } from "../adapter/types";
import { normalizeEvents } from "../animation/eventNormalizer";
import { getPlayerId } from "../hooks/usePlayerId";
import type { AnimationStep } from "../animation/types";
import { SPEED_MULTIPLIERS } from "../animation/types";
import { audioManager } from "../audio/AudioManager";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game";
import { useAnimationStore } from "../stores/animationStore";
import { useGameStore, saveGame } from "../stores/gameStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { useUiStore } from "../stores/uiStore";

/** Schedule SFX for each animation step, offset to sync with visual timing. */
function scheduleSfxForSteps(steps: AnimationStep[], multiplier: number): void {
  let offset = 0;
  for (const step of steps) {
    if (offset === 0) {
      audioManager.playSfxForStep(step.effects);
    } else {
      const delay = offset;
      setTimeout(() => audioManager.playSfxForStep(step.effects), delay);
    }
    offset += step.duration * multiplier;
  }
}

/**
 * Module-level position snapshot for AnimationOverlay position lookups.
 */
export let currentSnapshot = useAnimationStore.getState().captureSnapshot();

interface PendingAction {
  action: GameAction;
  resolve: () => void;
  reject: (err: unknown) => void;
}

/** Module-level mutex — replaces useRef from the hook version. */
let isAnimating = false;

/** Module-level queue — replaces pendingQueueRef from the hook version. */
const pendingQueue: PendingAction[] = [];

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
  const events: GameEvent[] = await adapter.submitAction(action);

  // 4. Flash turn banner directly (bypasses animation queue for reliability)
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

  // 5. Normalize events into animation steps
  const steps = normalizeEvents(events);

  // 6. Play animations (unless instant)
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

  // 7. Update game state (deferred after animations)
  const newState = await adapter.getState();
  const legalActions = await adapter.getLegalActions();

  useGameStore.setState((prev) => {
    const newHistory = shouldSaveHistory
      ? [...prev.stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
      : prev.stateHistory;

    return {
      gameState: newState,
      events,
      eventHistory: [...prev.eventHistory, ...events].slice(-1000),
      waitingFor: newState.waiting_for,
      legalActions,
      stateHistory: newHistory,
    };
  });

  // Persist to localStorage for resume-on-refresh
  const { gameId } = useGameStore.getState();
  if (gameId) saveGame(gameId, newState);

  // Fade out music on GameOver
  if (events.some((e) => e.type === "GameOver")) {
    audioManager.stopMusic(2.0);
  }
}

async function processQueue(): Promise<void> {
  while (pendingQueue.length > 0) {
    const next = pendingQueue.shift()!;
    try {
      await processAction(next.action);
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
      pendingQueue.push({ action, resolve, reject });
    });
  }

  isAnimating = true;
  try {
    await processAction(action);
  } finally {
    if (pendingQueue.length > 0) {
      processQueue();
    } else {
      isAnimating = false;
    }
  }
}
