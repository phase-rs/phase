import type { PlayerId } from "../adapter/types";
import { PLAYER_ID } from "../constants/game";
import { useGameStore } from "../stores/gameStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";

function currentLocalPlayerId(): PlayerId {
  const gameMode = useGameStore.getState().gameMode;
  if (gameMode && (gameMode === "online" || gameMode === "p2p-host" || gameMode === "p2p-join")) {
    return useMultiplayerStore.getState().activePlayerId ?? PLAYER_ID;
  }

  return PLAYER_ID;
}

/** React hook: returns the current player's game-assigned ID (0 or 1). Falls back to PLAYER_ID (0) for AI/local mode. */
export function usePlayerId(): PlayerId {
  const gameMode = useGameStore((s) => s.gameMode);
  const activePlayerId = useMultiplayerStore((s) => s.activePlayerId);

  if (gameMode && (gameMode === "online" || gameMode === "p2p-host" || gameMode === "p2p-join")) {
    return activePlayerId ?? PLAYER_ID;
  }

  return PLAYER_ID;
}

/** Non-React getter for use in plain functions (autoPass, gameLoopController). */
export function getPlayerId(): PlayerId {
  return currentLocalPlayerId();
}

function waitingPlayer(waitingFor: ReturnType<typeof useGameStore.getState>["waitingFor"]): PlayerId | null {
  if (!waitingFor || waitingFor.type === "GameOver") return null;
  // `VoteChoice.delegate_chooser`, when set, names the ACTOR who submits
  // the next `ChooseOption`. Battlebond's friend-or-foe cards (no explicit
  // CR section) pin this to the spell controller so the labeling cycle's
  // `player` field (the subject being labeled) does not gate submission.
  // Returning the delegate makes `useCanActForWaitingState` resolve to the
  // correct seat.
  if (waitingFor.type === "VoteChoice") {
    return waitingFor.data.delegate_chooser ?? waitingFor.data.player;
  }
  return "player" in waitingFor.data ? waitingFor.data.player : null;
}

export function usePerspectivePlayerId(): PlayerId {
  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);
  if (!gameState) return playerId;
  return gameState.turn_decision_controller === playerId ? gameState.active_player : playerId;
}

export function useCanActForWaitingState(): boolean {
  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const semanticPlayer = waitingPlayer(waitingFor);
  if (!gameState || semanticPlayer == null) return false;
  if (semanticPlayer === playerId) return true;
  return gameState.turn_decision_controller === playerId && semanticPlayer === gameState.active_player;
}
