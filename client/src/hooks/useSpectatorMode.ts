import { SPECTATOR_PLAYER_ID } from "../constants/game";
import { useGameStore } from "../stores/gameStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";
import { usePlayerId } from "./usePlayerId";

/** True when the local client must not submit game actions (live or eliminated spectator). */
export function useSpectatorMode(): boolean {
  const isSpectator = useMultiplayerStore((s) => s.isSpectator);
  const gameMode = useGameStore((s) => s.gameMode);
  const playerId = usePlayerId();
  if (gameMode === "spectate") return true;
  if (isSpectator) return true;
  return playerId === SPECTATOR_PLAYER_ID;
}
