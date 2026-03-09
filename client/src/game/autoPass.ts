import type { GameAction, GameState, Phase, WaitingFor } from "../adapter/types";
import { PLAYER_ID } from "../constants/game";

/**
 * Determines whether the current priority window should be auto-passed.
 * Uses legal actions from the engine (like Alchemy/MTGA) for accurate decisions.
 *
 * Rules (in order):
 * 1. Full control mode disables auto-pass
 * 2. Only auto-pass Priority prompts for the local player
 * 3. If stack is empty, respect phase stops (initial priority in that phase)
 * 4. Auto-pass if no meaningful actions exist (only PassPriority available)
 *
 * Phase stops only apply to initial priority (empty stack). When responding
 * to a spell/ability on the stack, only legal actions matter — matching
 * MTGA behavior where your own creature resolves without a click.
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

  // Phase stops only gate initial priority (empty stack)
  if (state.stack.length === 0 && phaseStops.includes(state.phase)) return false;

  // If the only legal action is PassPriority, auto-pass
  return !legalActions.some((a) => a.type !== "PassPriority");
}
