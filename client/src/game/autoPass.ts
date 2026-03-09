import type { GameState, Phase, WaitingFor } from "../adapter/types";
import { PLAYER_ID } from "../constants/game";

/**
 * Check if the player has relevant instant-speed actions available.
 * Conservative: returns true (don't auto-pass) when unsure.
 */
function hasRelevantActions(state: GameState): boolean {
  const player = state.players[PLAYER_ID];
  if (!player) return false;

  const hasMana = player.mana_pool.mana.length > 0;
  if (!hasMana) return false;

  // Check for instants or flash cards in hand
  const hasInstants = player.hand.some((id) => {
    const obj = state.objects[id];
    if (!obj) return false;
    return (
      obj.card_types.core_types.includes("Instant") ||
      obj.keywords.includes("Flash")
    );
  });

  if (hasInstants) return true;

  // Check for activated abilities on controlled battlefield permanents
  const hasActivatedAbilities = state.battlefield.some((id) => {
    const obj = state.objects[id];
    if (!obj) return false;
    return obj.controller === PLAYER_ID && obj.abilities.length > 0;
  });

  return hasActivatedAbilities;
}

/**
 * Determines whether the current priority window should be auto-passed.
 * Uses MTGA-style aggressive auto-pass heuristics.
 *
 * Rules (in order):
 * 1. Full control mode disables auto-pass
 * 2. Only auto-pass Priority prompts for the local player
 * 3. Never auto-pass with items on the stack
 * 4. Never auto-pass phases the player has stops set for
 * 5. Never auto-pass if player has relevant instant-speed actions
 * 6. Otherwise, auto-pass
 */
export function shouldAutoPass(
  state: GameState,
  waitingFor: WaitingFor,
  phaseStops: Phase[],
  fullControl: boolean,
): boolean {
  if (fullControl) return false;
  if (waitingFor.type !== "Priority") return false;
  if (waitingFor.data.player !== PLAYER_ID) return false;
  if (state.stack.length > 0) return false;
  if (phaseStops.includes(state.phase)) return false;
  if (hasRelevantActions(state)) return false;

  return true;
}
