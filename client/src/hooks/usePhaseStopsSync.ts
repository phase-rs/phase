import { useEffect } from "react";

import type { EngineAdapter, Phase } from "../adapter/types";
import { dispatchAction } from "../game/dispatch";
import { useGameStore } from "../stores/gameStore";
import { usePreferencesStore } from "../stores/preferencesStore";

/**
 * Keeps the engine's per-player `phase_stops` field in sync with the client's
 * persistent `phaseStops` preference. The engine is the single authority for
 * auto-pass and empty-blocker-auto-submission decisions, so the preference
 * must be pushed on game start and on every subsequent change.
 *
 * Mount once per active game (e.g. in `GameProvider`). The hook is a no-op
 * while no adapter is attached.
 */
export function usePhaseStopsSync(): void {
  useEffect(() => {
    // Dedup against the last-sent array, keyed by adapter identity so a new
    // game (fresh adapter) always gets a dispatch even if the preference is
    // unchanged since the previous game.
    let lastSent: { adapter: EngineAdapter; stops: readonly Phase[] } | null = null;

    const send = (stops: readonly Phase[]): void => {
      const adapter = useGameStore.getState().adapter;
      if (!adapter) return;
      if (
        lastSent !== null &&
        lastSent.adapter === adapter &&
        lastSent.stops.length === stops.length &&
        lastSent.stops.every((v, i) => v === stops[i])
      ) {
        return;
      }
      lastSent = { adapter, stops: stops.slice() };
      dispatchAction({ type: "SetPhaseStops", data: { stops: [...stops] } });
    };

    // Push the current preference once the adapter is ready. Subscribing to
    // both stores avoids a race between game init and first preference read.
    const unsubAdapter = useGameStore.subscribe(
      (s) => s.adapter,
      (adapter) => {
        if (adapter) send(usePreferencesStore.getState().phaseStops);
      },
      { fireImmediately: true },
    );

    // `usePreferencesStore` uses plain `persist` (no `subscribeWithSelector`),
    // so we fire on every change and let `send()` dedupe by comparing arrays.
    const unsubPrefs = usePreferencesStore.subscribe((state) => {
      send(state.phaseStops);
    });

    return () => {
      unsubAdapter();
      unsubPrefs();
    };
  }, []);
}
