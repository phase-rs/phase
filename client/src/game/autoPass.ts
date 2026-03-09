import type { GameAction, GameState, Phase, WaitingFor } from "../adapter/types";
import { PLAYER_ID } from "../constants/game";

/**
 * Determines whether the current priority window should be auto-passed.
 * Uses legal actions from the engine (like Alchemy/MTGA) for accurate decisions.
 *
 * Rules (in order):
 * 1. Full control mode disables auto-pass
 * 2. Only auto-pass Priority prompts for the local player
 * 3. Never auto-pass with items on the stack
 * 4. Never auto-pass phases the player has stops set for
 * 5. Auto-pass if no meaningful actions exist (only PassPriority available)
 */
export function shouldAutoPass(
  state: GameState,
  waitingFor: WaitingFor,
  phaseStops: Phase[],
  fullControl: boolean,
  legalActions: GameAction[],
): boolean {
  if (fullControl) return false;
  if (waitingFor.type !== "Priority") return false;
  if (waitingFor.data.player !== PLAYER_ID) return false;
  if (state.stack.length > 0) return false;
  if (phaseStops.includes(state.phase)) return false;

  // If the only legal action is PassPriority, auto-pass
  const hasPlayableAction = legalActions.some(
    (a) => a.type !== "PassPriority",
  );

  return !hasPlayableAction;
}
