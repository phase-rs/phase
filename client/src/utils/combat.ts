import type { AttackTarget, GameState, ObjectId, PlayerId } from "../adapter/types";

/**
 * Build attacks array from selected attacker IDs, defaulting to the first
 * non-eliminated opponent as the attack target. In N-player games, callers
 * can provide explicit per-creature targets via the overrides map.
 */
export function buildAttacks(
  attackerIds: ObjectId[],
  state: GameState | null,
  myId: PlayerId,
  targetOverrides?: Map<ObjectId, AttackTarget>,
): [ObjectId, AttackTarget][] {
  const defaultTarget = getDefaultAttackTarget(state, myId);
  return attackerIds.map((id) => [id, targetOverrides?.get(id) ?? defaultTarget]);
}

/** Returns the default attack target: first non-eliminated opponent. */
export function getDefaultAttackTarget(state: GameState | null, myId: PlayerId): AttackTarget {
  if (!state) return { type: "Player", data: myId === 0 ? 1 : 0 };

  const seatOrder = state.seat_order ?? state.players.map((p) => p.id);
  const eliminated = state.eliminated_players ?? [];

  const opponent = seatOrder.find(
    (id) => id !== myId && !eliminated.includes(id),
  );

  return { type: "Player", data: opponent ?? (myId === 0 ? 1 : 0) };
}

/** Check if there are multiple valid attack targets (multiplayer or planeswalkers). */
export function hasMultipleAttackTargets(
  state: GameState | null,
): boolean {
  if (!state) return false;
  const wf = state.waiting_for;
  if (wf.type !== "DeclareAttackers") return false;
  const targets = wf.data.valid_attack_targets;
  return targets != null && targets.length > 1;
}

/** Get valid attack targets from the current WaitingFor state. */
export function getValidAttackTargets(
  state: GameState | null,
): AttackTarget[] {
  if (!state) return [];
  const wf = state.waiting_for;
  if (wf.type !== "DeclareAttackers") return [];
  return wf.data.valid_attack_targets ?? [];
}
