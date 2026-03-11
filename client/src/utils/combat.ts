import type { AttackTarget, GameState, ObjectId, PlayerId } from "../adapter/types";

/**
 * Build attacks array from selected attacker IDs, defaulting to the first
 * non-eliminated opponent as the attack target. In N-player games with
 * attack target selection UI (Plan 29-08), callers can provide explicit targets.
 */
export function buildAttacks(
  attackerIds: ObjectId[],
  state: GameState | null,
  myId: PlayerId,
): [ObjectId, AttackTarget][] {
  const defaultTarget = getDefaultAttackTarget(state, myId);
  return attackerIds.map((id) => [id, defaultTarget]);
}

/** Returns the default attack target: first non-eliminated opponent. */
function getDefaultAttackTarget(state: GameState | null, myId: PlayerId): AttackTarget {
  if (!state) return { Player: myId === 0 ? 1 : 0 };

  const seatOrder = state.seat_order ?? state.players.map((p) => p.id);
  const eliminated = state.eliminated_players ?? [];

  const opponent = seatOrder.find(
    (id) => id !== myId && !eliminated.includes(id),
  );

  return { Player: opponent ?? (myId === 0 ? 1 : 0) };
}
