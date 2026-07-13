import type { PlayerId, WaitingFor } from "../adapter/types";
import { PLAYER_ID, SPECTATOR_PLAYER_ID } from "../constants/game";
import { useGameStore } from "../stores/gameStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";

function currentLocalPlayerId(): PlayerId {
  const gameMode = useGameStore.getState().gameMode;
  if (gameMode === "spectate") {
    return SPECTATOR_PLAYER_ID;
  }
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

/**
 * The seat that must act next for `waitingFor` — the *semantic* actor, which
 * differs from the engine's `priority_player` (the re-derived authorized
 * submitter). Resolves Vote delegation and Assist's chosen helper; otherwise
 * the variant's `player`. Exported so display surfaces (e.g. `useTurnStatus`)
 * read this single authority instead of cloning the logic.
 */
export function waitingPlayer(waitingFor: WaitingFor | null): PlayerId | null {
  if (!waitingFor || waitingFor.type === "GameOver") return null;
  // `VoteChoice.actor` names who submits the next `ChooseOption`. Classic
  // Council's-dilemma votes carry `{ type: "SubjectActs" }` so the current
  // subject (`player`) acts for themselves. Battlebond friend-or-foe (no
  // explicit CR section) carries `{ type: "Delegated", data: <controller> }`
  // so the spell controller is the authorized submitter while `player`
  // cycles through subjects. Resolving here makes
  // `useCanActForWaitingState` route the action to the correct seat.
  if (waitingFor.type === "VoteChoice") {
    const { actor, player } = waitingFor.data;
    return actor.type === "Delegated" ? actor.data : player;
  }
  // CR 702.132a: Assist payment — the CHOSEN helper acts on this step, not the
  // caster. The prompt carries `caster`/`chosen` (no `player` field), so route
  // authorization to `chosen` here. (`AssistChoosePlayer` carries `player` =
  // caster and falls through to the default below.)
  if (waitingFor.type === "AssistPayment") {
    return waitingFor.data.chosen;
  }
  // CR 732.2a: LoopShortcut routes authorization to the proposer, whose data
  // field is `proposer` (not `player`); mirror engine `acting_player()`
  // (game_state.rs). Without this the declare modal's actor gate returns false
  // and it never renders. `RespondToShortcut` carries `player` → default below.
  if (waitingFor.type === "LoopShortcut") {
    return waitingFor.data.proposer;
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
  const gameMode = useGameStore((s) => s.gameMode);
  const isSpectator = useMultiplayerStore((s) => s.isSpectator);
  const playerId = usePlayerId();
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (gameMode === "spectate" || isSpectator) return false;

  const semanticPlayer = waitingPlayer(waitingFor);
  if (!gameState || semanticPlayer == null) return false;
  if (playerId === SPECTATOR_PLAYER_ID) return false;
  if (semanticPlayer === playerId) return true;
  return gameState.turn_decision_controller === playerId && semanticPlayer === gameState.active_player;
}
