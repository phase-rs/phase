import type { PlayerId } from "../adapter/types";
import { PLAYER_ID } from "../constants/game";
import { useMultiplayerStore } from "../stores/multiplayerStore";

/** React hook: returns the current player's game-assigned ID (0 or 1). Falls back to PLAYER_ID (0) for AI/local mode. */
export function usePlayerId(): PlayerId {
  return useMultiplayerStore((s) => s.activePlayerId) ?? PLAYER_ID;
}

/** Non-React getter for use in plain functions (autoPass, gameLoopController). */
export function getPlayerId(): PlayerId {
  return useMultiplayerStore.getState().activePlayerId ?? PLAYER_ID;
}
