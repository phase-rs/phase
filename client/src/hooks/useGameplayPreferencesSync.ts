import { useEffect } from "react";

import type {
  EngineAdapter,
  PhaseStop,
  PriorityPassingMode,
} from "../adapter/types";
import { dispatchActionForGameSession } from "../game/dispatch";
import { useGameStore } from "../stores/gameStore";
import { usePreferencesStore } from "../stores/preferencesStore";

type LastSent = {
  adapter: EngineAdapter;
  generation: number;
  stops?: readonly PhaseStop[];
  mode?: PriorityPassingMode;
};

// Module-scoped so React StrictMode remounts cannot resend preferences for the
// same live engine lifecycle. `gameSessionGeneration` is monotonically unique,
// so a genuine init/resume/reset always invalidates this cache even when both
// the adapter object and game id are reused.
let lastSent: LastSent | null = null;
let syncRequested = false;
let syncInFlight = false;

function phaseStopsEqual(a: readonly PhaseStop[], b: readonly PhaseStop[]): boolean {
  return a.length === b.length
    && a.every((value, index) =>
      value.phase === b[index]?.phase && value.scope === b[index]?.scope,
    );
}

function isCurrentSession(adapter: EngineAdapter, generation: number): boolean {
  const game = useGameStore.getState();
  return (
    game.adapter === adapter
    && game.gameSessionGeneration === generation
    && game.gameState !== null
  );
}

function successfulSendFor(adapter: EngineAdapter, generation: number): LastSent {
  if (lastSent?.adapter === adapter && lastSent.generation === generation) {
    return lastSent;
  }
  return { adapter, generation };
}

async function drainGameplayPreferenceSync(): Promise<void> {
  if (syncInFlight) return;
  syncInFlight = true;

  try {
    while (syncRequested) {
      syncRequested = false;

      const {
        adapter,
        gameSessionGeneration: generation,
        gameState,
      } = useGameStore.getState();
      if (!adapter || !gameState) continue;

      const { phaseStops: stops, priorityPassingMode: mode } = usePreferencesStore.getState();
      const sent = successfulSendFor(adapter, generation);

      if (!sent.stops || !phaseStopsEqual(sent.stops, stops)) {
        try {
          await dispatchActionForGameSession(
            { type: "SetPhaseStops", data: { stops: [...stops] } },
            adapter,
            generation,
          );
        } catch {
          // dispatchAction reports engine failures. Leave this value unsent so
          // the next store notification can retry it.
          continue;
        }

        const currentStops = usePreferencesStore.getState().phaseStops;
        if (isCurrentSession(adapter, generation) && phaseStopsEqual(currentStops, stops)) {
          lastSent = {
            ...successfulSendFor(adapter, generation),
            stops: stops.slice(),
          };
        } else {
          syncRequested = true;
          continue;
        }
      }

      if (!isCurrentSession(adapter, generation)) {
        syncRequested = true;
        continue;
      }

      const currentMode = usePreferencesStore.getState().priorityPassingMode;
      if (currentMode !== mode) {
        syncRequested = true;
        continue;
      }

      const modeSent = successfulSendFor(adapter, generation);
      if (modeSent.mode !== mode) {
        try {
          await dispatchActionForGameSession(
            { type: "SetPriorityPassingMode", data: { mode } },
            adapter,
            generation,
          );
        } catch {
          // As above, a rejected dispatch must remain retryable.
          continue;
        }

        if (
          isCurrentSession(adapter, generation)
          && usePreferencesStore.getState().priorityPassingMode === mode
        ) {
          lastSent = { ...successfulSendFor(adapter, generation), mode };
        } else {
          syncRequested = true;
        }
      }
    }
  } finally {
    syncInFlight = false;
    // A notification can land after the loop condition but before the flag is
    // cleared. Make sure that request is not stranded.
    if (syncRequested) void drainGameplayPreferenceSync();
  }
}

function sendGameplayPreferences(): void {
  syncRequested = true;
  void drainGameplayPreferenceSync();
}

/** Push engine-owned gameplay preferences once per game lifecycle and whenever
 * either preference changes. Mount exactly once in `GameProvider`. */
export function useGameplayPreferencesSync(): void {
  useEffect(() => {
    const unsubGame = useGameStore.subscribe(
      (state) => [
        state.adapter,
        state.gameSessionGeneration,
        state.gameState !== null,
        state.engineCommitEpoch,
      ] as const,
      sendGameplayPreferences,
      { fireImmediately: true },
    );
    const unsubPreferences = usePreferencesStore.subscribe(sendGameplayPreferences);

    return () => {
      unsubGame();
      unsubPreferences();
    };
  }, []);
}
