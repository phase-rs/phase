import type { AttackTarget, GameObject, GameState, ObjectId } from "../adapter/types";
import { groupByName } from "../viewmodel/battlefieldProps";

/**
 * Assemble the `[attacker, chosen-target]` pairs the engine expects, using only
 * engine-provided legal targets — no client-side default-opponent injection
 * (the engine is the sole authority on target legality, CR 508.1a–d).
 *
 * Used for the single-target confirmation path (2-player / one common target):
 * each selected attacker is paired with its sole engine-provided legal target.
 * For multi-target declarations the {@link AttackTargetPicker} builds the pairs
 * from explicit per-attacker choices instead.
 */
export function buildAttacks(
  attackerIds: ObjectId[],
  byAttacker: Record<string, AttackTarget[]> | undefined,
  aggregate: AttackTarget[],
): [ObjectId, AttackTarget][] {
  return attackerIds.flatMap((id): [ObjectId, AttackTarget][] => {
    const target = attackTargetsForAttacker(id, byAttacker, aggregate)[0];
    return target ? [[id, target]] : [];
  });
}

/** Stable key for an AttackTarget (`"Player-1"`, `"Planeswalker-42"`). */
export function attackTargetKey(target: AttackTarget): string {
  return `${target.type}-${target.data}`;
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

/** Get the aggregate compatibility target list from the current WaitingFor. */
export function getValidAttackTargets(
  state: GameState | null,
): AttackTarget[] {
  if (!state) return [];
  const wf = state.waiting_for;
  if (wf.type !== "DeclareAttackers") return [];
  return wf.data.valid_attack_targets ?? [];
}

/**
 * The engine-authoritative per-attacker legal-target map from the current
 * `DeclareAttackers` prompt, or `undefined` for a legacy payload that predates
 * the field. `undefined` means "fall back to the aggregate list"; a present map
 * (even `{}`) is authoritative.
 */
export function getValidAttackTargetsByAttacker(
  state: GameState | null,
): Record<string, AttackTarget[]> | undefined {
  if (!state) return undefined;
  const wf = state.waiting_for;
  if (wf.type !== "DeclareAttackers") return undefined;
  return wf.data.valid_attack_targets_by_attacker;
}

/**
 * Legal attack targets for one attacker — presentation over engine choices only,
 * no client legality computed here. When the engine provides the per-attacker
 * map (`byAttacker` present, authoritative), a present key gives that attacker's
 * exact legal targets and a MISSING key means it has none (no fallback). Only a
 * legacy payload (`byAttacker === undefined`) falls back to the aggregate
 * compatibility list.
 */
export function attackTargetsForAttacker(
  attackerId: ObjectId,
  byAttacker: Record<string, AttackTarget[]> | undefined,
  aggregate: AttackTarget[],
): AttackTarget[] {
  if (byAttacker) return byAttacker[attackerId] ?? [];
  return aggregate;
}

/**
 * The set of targets every one of `attackerIds` can legally attack — the
 * intersection of their per-attacker legal sets, in `aggregate` display order.
 * Drives "Attack All": a single target is offered only when all selected
 * attackers may legally attack it. With a legacy payload the intersection is the
 * aggregate list (every attacker shares it).
 */
export function commonAttackTargets(
  attackerIds: ObjectId[],
  byAttacker: Record<string, AttackTarget[]> | undefined,
  aggregate: AttackTarget[],
): AttackTarget[] {
  if (attackerIds.length === 0) return [];
  const perAttackerKeys = attackerIds.map(
    (id) => new Set(attackTargetsForAttacker(id, byAttacker, aggregate).map(attackTargetKey)),
  );
  return aggregate.filter((target) => {
    const key = attackTargetKey(target);
    return perAttackerKeys.every((set) => set.has(key));
  });
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
  /**
   * The engine-provided legal attack targets shared by every member of this
   * stack. All members have identical legal sets (that is the stack invariant),
   * so distribution steppers only ever offer these targets. Empty when no
   * per-attacker map is supplied (legacy callers that don't distribute per
   * bucket).
   */
  targets: AttackTarget[];
}

/**
 * Group selected attackers into stacks of identical creatures, reusing the same
 * `groupByName`/`groupKey` building block the battlefield uses to collapse
 * identical permanents — so the picker's grouping always matches the board's.
 *
 * When `targetsFor` is supplied, each name-group is further subdivided by its
 * members' engine-provided legal-target set: two identically-named attackers
 * with *different* legal options (CR 508.1c scoped restrictions) are NOT
 * interchangeable, so they land in separate stacks. Every member of a returned
 * stack shares one legal-target set, exposed as `stack.targets`.
 *
 * Ring-bearers (CR 701.54) are grouped solo by that building block, which is
 * the correct behavior here too. Stacks and their members are returned in
 * ascending-id order for a deterministic, stable layout.
 */
export function groupAttackers(
  attackerIds: ObjectId[],
  state: GameState | null,
  targetsFor?: (id: ObjectId) => AttackTarget[],
): AttackerStack[] {
  if (!state) {
    // Defensive: with no state we can't group by identity — treat each attacker
    // as its own singleton stack so the UI still renders something usable.
    return [...attackerIds]
      .sort((a, b) => a - b)
      .map((id) => ({
        key: String(id),
        name: `#${id}`,
        ids: [id],
        count: 1,
        representative: null,
        targets: targetsFor?.(id) ?? [],
      }));
  }

  const objects = attackerIds
    .map((id) => state.objects[id])
    .filter((o): o is GameObject => o != null);

  // CR 701.54: keep the Ring-bearer as its own stack (mirrors the battlefield).
  const ringBearerIds = new Set(
    Object.values(state.ring_bearer ?? {}).filter((id): id is ObjectId => id != null),
  );

  return groupByName(objects, ringBearerIds)
    .flatMap((group) => subdivideByTargets([...group.ids], group.name, state, targetsFor))
    .sort((a, b) => a.ids[0] - b.ids[0]);
}

/**
 * Split one name-group into stacks sharing an identical legal-target set. With
 * no `targetsFor` the group stays whole (legacy). Sub-stacks and their members
 * are ascending-id sorted so stepper moves stay deterministic.
 */
function subdivideByTargets(
  ids: ObjectId[],
  name: string,
  state: GameState,
  targetsFor?: (id: ObjectId) => AttackTarget[],
): AttackerStack[] {
  const sorted = [...ids].sort((a, b) => a - b);
  if (!targetsFor) {
    return [{ key: String(sorted[0]), name, ids: sorted, count: sorted.length, representative: state.objects[sorted[0]] ?? null, targets: [] }];
  }

  // Bucket members by the canonical signature of their legal-target set.
  const bySignature = new Map<string, { targets: AttackTarget[]; ids: ObjectId[] }>();
  for (const id of sorted) {
    const targets = targetsFor(id);
    const signature = targets.map(attackTargetKey).sort().join("|");
    const bucket = bySignature.get(signature);
    if (bucket) bucket.ids.push(id);
    else bySignature.set(signature, { targets, ids: [id] });
  }

  return Array.from(bySignature.values())
    .map(({ targets, ids: bucketIds }) => ({
      key: String(bucketIds[0]),
      name,
      ids: bucketIds,
      count: bucketIds.length,
      representative: state.objects[bucketIds[0]] ?? null,
      targets,
    }))
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
