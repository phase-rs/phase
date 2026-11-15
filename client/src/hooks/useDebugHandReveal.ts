import { useEffect } from "react";

import { supportsDebugHandReveal } from "../adapter/types";
import type { DebugHandRevealCapability, EngineAdapter } from "../adapter/types";
import { isBoundGameSessionCurrent } from "../game/dispatch";
import type { BoundGameSession } from "../game/dispatch";
import { useGameStore } from "../stores/gameStore";

/**
 * Send one hand-disclosure request to the engine, then publish the engine's
 * fresh snapshot for the game session the request was made in. A request that
 * outlives its session (a rematch or import installed a new game, or the page
 * left the game) publishes nothing.
 */
async function applyDebugHandReveal(
  adapter: EngineAdapter & DebugHandRevealCapability,
  enabled: boolean,
  session: BoundGameSession,
  isCurrent: () => boolean,
): Promise<void> {
  const stillBound = () => isCurrent() && isBoundGameSessionCurrent(session);
  try {
    await adapter.setDebugHandReveal(enabled);
    if (!stillBound()) return;
    const snapshot = await adapter.getSnapshot();
    if (!stillBound()) return;
    useGameStore.getState().commitEngineSnapshot(snapshot);
  } catch (err) {
    console.warn("Show AI Hand: engine projection refresh failed", err);
  }
}

/**
 * "Show AI Hand": the engine owns hidden-card disclosure. While `enabled`, ask
 * it for the revealed projection of the current game session; turning it off
 * or leaving the game withdraws the request. Every engine game install starts
 * hidden, so nothing is sent while the toggle is off, and keying on the session
 * generation re-sends the request for each newly installed game (a rematch on
 * the shared adapter, or an imported state).
 */
export function useDebugHandReveal(enabled: boolean): void {
  const adapter = useGameStore((s) => s.adapter);
  const hasGameState = useGameStore((s) => s.gameState != null);
  const gameSessionGeneration = useGameStore((s) => s.gameSessionGeneration);
  useEffect(() => {
    if (!enabled || !hasGameState || !supportsDebugHandReveal(adapter)) return;
    const session: BoundGameSession = { adapter, generation: gameSessionGeneration };
    let cancelled = false;
    void applyDebugHandReveal(adapter, true, session, () => !cancelled);
    return () => {
      cancelled = true;
      void applyDebugHandReveal(adapter, false, session, () => true);
    };
  }, [adapter, enabled, gameSessionGeneration, hasGameState]);
}
