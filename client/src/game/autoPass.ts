import type { GameState, WaitingFor } from "../adapter/types";
import { getPlayerId } from "../hooks/usePlayerId";

/**
 * Determines whether the current priority window should be auto-passed.
 *
 * The engine computes `autoPassRecommended` which classifies whether the player
 * has meaningful actions (spells, abilities, lands) beyond PassPriority. This
 * function only gates on frontend-specific UI preferences: full control mode
 * and phase stops. All game-logic classification lives in the engine.
 *
 * Phase stops are read from `state.phase_stops[playerId]` — the engine is the
 * single source of truth, kept in sync with the user's persistent preference by
 * `usePhaseStopsSync`.
 *
 * Rules (in order):
 * 1. Full control mode disables auto-pass
 * 2. Only auto-pass Priority prompts for the local player
 * 3. If stack is empty, respect phase stops (initial priority in that phase)
 * 4. Defer to engine's auto-pass recommendation
 */
export function shouldAutoPass(
  state: GameState,
  waitingFor: WaitingFor,
  fullControl: boolean,
  autoPassRecommended: boolean,
): boolean {
  if (fullControl) return false;
  if (waitingFor.type !== "Priority") return false;
  const player = waitingFor.data.player;
  if (player !== getPlayerId()) return false;

  // Don't auto-pass an invalid/empty game state (e.g. no cards loaded yet)
  if (state.players.length === 0 || Object.keys(state.objects).length === 0) return false;

  // Phase stops only gate initial priority (empty stack).
  const stops = state.phase_stops?.[player] ?? [];
  if (state.stack.length === 0 && stops.includes(state.phase)) return false;

  return autoPassRecommended;
}
