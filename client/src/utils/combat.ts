import type { AttackTarget, GameObject, GameState, ObjectId, PlayerId } from "../adapter/types";
import { groupByName } from "../viewmodel/battlefieldProps";

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

/**
 * A stack of identical attackers (e.g. 30 token "ants" → one stack of count 30),
 * used by the attack-distribution UI to assign many attackers at once.
 *
 * `ids` is sorted ascending so per-target stepper moves are deterministic:
 * "+1 to target T" claims the lowest-id unassigned member, "-1" releases the
 * highest-id member currently on T (see {@link AttackTargetPicker}).
 */
export interface AttackerStack {
  /** Stable key for the stack (the representative/lowest member id, stringified). */
  key: string;
  /** Display name shared by every member of the stack. */
  name: string;
  /** Member object ids, sorted ascending for deterministic assignment. */
  ids: ObjectId[];
  /** Convenience for `ids.length`. */
  count: number;
  /** Representative object for rendering P/T and counter chips (null only if state is missing). */
  representative: GameObject | null;
}

/**
 * Group selected attackers into stacks of identical creatures, reusing the same
 * `groupByName`/`groupKey` building block the battlefield uses to collapse
 * identical permanents — so the picker's grouping always matches the board's.
 *
 * Ring-bearers (CR 701.54) are grouped solo by that building block, which is
 * the correct behavior here too. Stacks and their members are returned in
 * ascending-id order for a deterministic, stable layout.
 */
export function groupAttackers(
  attackerIds: ObjectId[],
  state: GameState | null,
): AttackerStack[] {
  if (!state) {
    // Defensive: with no state we can't group by identity — treat each attacker
    // as its own singleton stack so the UI still renders something usable.
    return [...attackerIds]
      .sort((a, b) => a - b)
      .map((id) => ({ key: String(id), name: `#${id}`, ids: [id], count: 1, representative: null }));
  }

  const objects = attackerIds
    .map((id) => state.objects[id])
    .filter((o): o is GameObject => o != null);

  // CR 701.54: keep the Ring-bearer as its own stack (mirrors the battlefield).
  const ringBearerIds = new Set(
    Object.values(state.ring_bearer ?? {}).filter((id): id is ObjectId => id != null),
  );

  return groupByName(objects, ringBearerIds)
    .map((group) => {
      const ids = [...group.ids].sort((a, b) => a - b);
      return {
        key: String(ids[0]),
        name: group.name,
        ids,
        count: ids.length,
        representative: state.objects[ids[0]] ?? null,
      };
    })
    .sort((a, b) => a.ids[0] - b.ids[0]);
}

/**
 * Distribute `count` items as evenly as possible across `buckets` slots,
 * handing the remainder to the earliest buckets in order. e.g. `evenSplit(31, 3)`
 * → `[11, 10, 10]`. Returns an array of length `buckets` (all zeros when
 * `count <= 0`; empty when `buckets <= 0`).
 */
export function evenSplit(count: number, buckets: number): number[] {
  if (buckets <= 0) return [];
  const total = Math.max(0, count);
  const base = Math.floor(total / buckets);
  const remainder = total % buckets;
  return Array.from({ length: buckets }, (_, i) => base + (i < remainder ? 1 : 0));
}
