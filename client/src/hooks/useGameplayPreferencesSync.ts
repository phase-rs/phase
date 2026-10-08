import { useEffect } from "react";

import type {
  EngineAdapter,
  PhaseStop,
  PriorityPassingMode,
} from "../adapter/types";
import { dispatchActionForGameSession } from "../game/dispatch";
import { useGameStore } from "../stores/gameStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { useUiStore } from "../stores/uiStore";

/**
 * The mode the engine must hold for this player.
 *
 * CR 117.1: Full Control is a standing refusal to give up any priority window,
 * so it has to be engine state, not a frontend flag. An auto-pass session
 * another player installed (Resolve All) is driven inside the engine's own
 * priority loop and never consults a client, so a purely local toggle could not
 * stop it. It stays a per-session `uiStore` toggle in the UI — this only
 * projects it onto the synced preference while it is on.
 */
function effectivePriorityPassingMode(): PriorityPassingMode {
  return useUiStore.getState().fullControl
    ? "FullControl"
    : usePreferencesStore.getState().priorityPassingMode;
}

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

      const stops = usePreferencesStore.getState().phaseStops;
      const mode = effectivePriorityPassingMode();
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

      const currentMode = effectivePriorityPassingMode();
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
          && effectivePriorityPassingMode() === mode
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
    // Full Control lives in `uiStore` (a per-session toggle, not a persisted
    // preference), so it needs its own subscription to reach the engine.
    // `uiStore` is a plain zustand store with no `subscribeWithSelector`
    // middleware, so the selector overload is unavailable — hence the explicit
    // previous-value guard, which also keeps unrelated UI state changes from
    // re-dispatching the preference.
    let lastFullControl = useUiStore.getState().fullControl;
    const unsubFullControl = useUiStore.subscribe((state) => {
      if (state.fullControl === lastFullControl) return;
      lastFullControl = state.fullControl;
      sendGameplayPreferences();
    });

    return () => {
      unsubGame();
      unsubPreferences();
      unsubFullControl();
    };
  }, []);
}
