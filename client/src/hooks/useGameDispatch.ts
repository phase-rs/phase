import { useCallback, useRef } from "react";

import type { GameAction, GameEvent } from "../adapter/types.ts";
import { normalizeEvents } from "../animation/eventNormalizer.ts";
import { SPEED_MULTIPLIERS } from "../animation/types.ts";
import { MAX_UNDO_HISTORY, UNDOABLE_ACTIONS } from "../constants/game.ts";
import { useAnimationStore } from "../stores/animationStore.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { usePreferencesStore } from "../stores/preferencesStore.ts";

/**
 * Module-level ref for the current position snapshot.
 * AnimationOverlay (Plan 04) reads this during playback for position lookups.
 */
export let currentSnapshot = useAnimationStore.getState().captureSnapshot();

interface PendingAction {
  action: GameAction;
  resolve: () => void;
  reject: (err: unknown) => void;
}

/**
 * Wraps adapter calls with snapshot-animate-update flow.
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
export function useGameDispatch(): (action: GameAction) => Promise<void> {
  const isAnimatingRef = useRef(false);
  const pendingQueueRef = useRef<PendingAction[]>([]);

  const processAction = useCallback(async (action: GameAction) => {
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

    // 4. Normalize events into animation steps
    const steps = normalizeEvents(events);

    // 5. Play animations (unless instant)
    const speed = usePreferencesStore.getState().animationSpeed;
    const multiplier = SPEED_MULTIPLIERS[speed];

    if (steps.length > 0 && multiplier > 0) {
      useAnimationStore.getState().enqueueSteps(steps);

      // Wait for total animation duration
      const totalDuration = steps.reduce(
        (sum, step) => sum + step.duration * multiplier,
        0,
      );
      await new Promise<void>((resolve) => setTimeout(resolve, totalDuration));
    }

    // 6. Update game state (deferred after animations)
    const newState = await adapter.getState();

    useGameStore.setState((prev) => {
      const newHistory = shouldSaveHistory
        ? [...prev.stateHistory, gameState].slice(-MAX_UNDO_HISTORY)
        : prev.stateHistory;

      return {
        gameState: newState,
        events,
        eventHistory: [...prev.eventHistory, ...events].slice(-1000),
        waitingFor: newState.waiting_for,
        stateHistory: newHistory,
      };
    });
  }, []);

  const processQueue = useCallback(async () => {
    while (pendingQueueRef.current.length > 0) {
      const next = pendingQueueRef.current.shift()!;
      try {
        await processAction(next.action);
        next.resolve();
      } catch (err) {
        next.reject(err);
      }
    }
    isAnimatingRef.current = false;
  }, [processAction]);

  return useCallback(
    async (action: GameAction) => {
      if (isAnimatingRef.current) {
        // Queue the action and return a promise that resolves when processed
        return new Promise<void>((resolve, reject) => {
          pendingQueueRef.current.push({ action, resolve, reject });
        });
      }

      isAnimatingRef.current = true;
      try {
        await processAction(action);
      } finally {
        // Process any queued actions, then release mutex
        if (pendingQueueRef.current.length > 0) {
          processQueue();
        } else {
          isAnimatingRef.current = false;
        }
      }
    },
    [processAction, processQueue],
  );
}
