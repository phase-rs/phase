import { isMultiplayerMode, useGameStore } from "../stores/gameStore";

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
  const { gameMode, gameState, adapter } = useGameStore.getState();
  if (!isMultiplayerMode(gameMode)) return false;
  if (!adapter) return false;
  if (gameState?.waiting_for?.type === "GameOver") return false;
  return true;
}

/**
 * Register a one-shot callback that fires once `isMultiplayerGameLive()`
 * transitions from true to false. Returns the unsubscribe function so
 * callers can cancel if the deferred action is no longer needed.
 *
 * Selector-based subscribe so the listener only fires when the derived
 * liveness boolean flips, not on every unrelated store mutation (actions,
 * log entries, animation ticks).
 *
 * Immediate re-check after subscribe closes a TOCTOU window: the state
 * may have transitioned out of "live" between the caller's guard and
 * our subscribe, and Zustand only fires on *subsequent* changes.
 */
export function whenMultiplayerGameEnds(callback: () => void): () => void {
  let fired = false;
  const fire = () => {
    if (fired) return;
    fired = true;
    unsub();
    callback();
  };
  const unsub = useGameStore.subscribe(
    (s) => {
      if (!isMultiplayerMode(s.gameMode)) return false;
      if (!s.adapter) return false;
      if (s.gameState?.waiting_for?.type === "GameOver") return false;
      return true;
    },
    (live) => { if (!live) fire(); },
  );
  if (!isMultiplayerGameLive()) fire();
  return unsub;
}
