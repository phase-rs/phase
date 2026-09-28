import type {
  AttackerInfo,
  AttackTarget,
  BlockRequirementInfo,
  CombatRequirement,
  GameObject,
  GameState,
  ObjectId,
} from "../adapter/types";
import { groupByName } from "../viewmodel/battlefieldProps";

/**
 * Assemble the `[attacker, chosen-target]` pairs the engine expects, using only
 * engine-provided legal targets — no client-side default-opponent injection
 * (the engine is the sole authority on target legality, CR 508.1a–d).
 *
 * Used for the single-target confirmation path (2-player / one common target):
 * each selected attacker is paired with its sole engine-provided selectable
 * target. Returns `null` when the authoritative support map cannot pair every
 * selected attacker, so callers can keep the declaration unsubmitted rather
 * than silently omitting an attacker.
 * For multi-target declarations the {@link AttackTargetPicker} builds the pairs
 * from explicit per-attacker choices instead.
 */
export function buildAttacks(
  attackerIds: ObjectId[],
  byAttacker: Record<string, AttackTarget[]> | undefined,
  aggregate: AttackTarget[],
): [ObjectId, AttackTarget][] | null {
  const attacks: [ObjectId, AttackTarget][] = [];
  for (const id of attackerIds) {
    const target = attackTargetsForAttacker(id, byAttacker, aggregate)[0];
    if (!target) return null;
    attacks.push([id, target]);
  }
  return attacks;
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
 * The engine-authoritative per-attacker selectable-target-support map from the current
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
 * Selectable attack targets for one attacker — presentation over engine choices
 * only, with no client legality computed here. The map proves each pair belongs
 * to at least one accepted full declaration; it is not a standalone legality
 * check. When `byAttacker` is present (authoritative), a present key gives that
 * attacker's exact selectable support and a MISSING key means it has none (no
 * fallback). Only a legacy payload (`byAttacker === undefined`) falls back to
 * the aggregate compatibility list.
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
 * The common selectable targets for every one of `attackerIds` — the intersection
 * of their engine-provided support sets, in `aggregate` display order. Drives
 * "Attack All" without attempting client-side rules evaluation. With a legacy
 * payload the intersection is the aggregate list (every attacker shares it).
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
   * The engine-provided selectable attack targets shared by every member of this
   * stack. All members have identical selectable sets (that is the stack invariant),
   * so distribution steppers only ever offer these targets. Empty when no
   * per-attacker map is supplied (legacy callers that don't distribute per
   * bucket).
   */
  targets: AttackTarget[];
  /**
   * CR 732.2a: every member of this stack is in an accepted object-growth loop's
   * engine-authored "∞ pile", so the picker renders `∞` instead of `×N`. Carried
   * through from `groupByName`'s `isUnboundedPile` (see `derived.unbounded_pile`).
   * Reachable: the pile is a persistent object-id snapshot (`derived_views.rs`
   * re-filters only by battlefield membership, not `tapped`), so a member that
   * untaps on a later turn stays in the pile and can be declared an attacker.
   */
  isUnboundedPile: boolean;
}

/**
 * Group selected attackers into stacks of identical creatures, reusing the same
 * `groupByName`/`groupKey` building block the battlefield uses to collapse
 * identical permanents — so the picker's grouping always matches the board's.
 *
 * When `targetsFor` is supplied, each name-group is further subdivided by its
 * members' engine-provided selectable-target set: two identically-named attackers
 * with *different* selectable options (CR 508.1c scoped restrictions) are NOT
 * interchangeable, so they land in separate stacks. Every member of a returned
 * stack shares one selectable-target set, exposed as `stack.targets`.
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
        isUnboundedPile: false,
      }));
  }

  const objects = attackerIds
    .map((id) => state.objects[id])
    .filter((o): o is GameObject => o != null);

  // CR 701.54: keep the Ring-bearer as its own stack (mirrors the battlefield).
  const ringBearerIds = new Set(
    Object.values(state.ring_bearer ?? {}).filter((id): id is ObjectId => id != null),
  );
  // CR 732.2a: engine-authored ∞-pile membership, threaded so the picker renders
  // `∞` like the battlefield (mirrors buildPlayerBattlefieldView in gameStateView.ts).
  const unboundedPileIds = new Set(state.derived?.unbounded_pile ?? []);

  // CR 122.1: the engine's counter-display projection is part of the group IDENTITY, so attacker
  // stacks split exactly where the battlefield does (mirrors buildPlayerBattlefieldView).
  return groupByName(objects, ringBearerIds, unboundedPileIds, state.derived?.counter_display)
    .flatMap((group) =>
      subdivideByTargets(
        [...group.ids],
        group.name,
        state,
        targetsFor,
        group.isUnboundedPile,
      ),
    )
    .sort((a, b) => a.ids[0] - b.ids[0]);
}

/**
 * Split one name-group into stacks sharing an identical legal-target set. With
 * no `targetsFor` the group stays whole (legacy). Sub-stacks and their members
 * are ascending-id sorted so stepper moves stay deterministic. Each returned
 * stack inherits the group's `isUnboundedPile` (CR 732.2a ∞-pile membership is a
 * per-name property, independent of the legal-target subdivision).
 */
function subdivideByTargets(
  ids: ObjectId[],
  name: string,
  state: GameState,
  targetsFor: ((id: ObjectId) => AttackTarget[]) | undefined,
  isUnboundedPile: boolean,
): AttackerStack[] {
  const sorted = [...ids].sort((a, b) => a - b);
  if (!targetsFor) {
    return [
      {
        key: String(sorted[0]),
        name,
        ids: sorted,
        count: sorted.length,
        representative: state.objects[sorted[0]] ?? null,
        targets: [],
        isUnboundedPile,
      },
    ];
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
      isUnboundedPile,
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

/**
 * A stack of the pending blocker's legal candidate attackers that are
 * interchangeable for that blocker: every rule-relevant axis is identical, so
 * a single count stepper can stand in for a #1..#N list. The axes:
 * CR 509.1a (the attack target — blocking the one on a planeswalker is a
 * different choice than blocking the one on the defending player);
 * CR 509.1b/CR 702.111b (the minimum-blocker count another static or Menace
 * imposes on the attacker); CR 509.1c (a requirement carried by either
 * combatant — the pending blocker's own "block X if able", or the attacker's
 * "must be blocked" static); CR 702.22c (band membership — a member of one
 * band is not interchangeable with a bandless member or a member of a
 * different band). Legality itself is not an axis: the caller already
 * filters to `valid_block_targets[pendingBlocker]` before stacking.
 */
export interface BlockTargetStack {
  /** Stable key for the stack (the lowest member id, stringified). */
  key: string;
  /** Member object ids, sorted ascending for deterministic stepper moves. */
  ids: ObjectId[];
  /** Convenience for `ids.length`. */
  count: number;
  /** CR 509.1a: the attack target shared by every member of this stack. */
  attackTarget: AttackTarget;
  /** Other blockers (never the pending one) directly assigned to every
   *  member of this stack, ascending. A double block onto an already-blocked
   *  attacker is a different choice than a fresh block (CR 702.111b menace
   *  needs two or more on the SAME attacker). */
  otherBlockerIds: ObjectId[];
  /** CR 509.1b / CR 702.111b: the minimum-blocker count this stack's members
   *  require, 0 when none applies. */
  minBlockers: number;
  /** CR 509.1c: the pending blocker's own "block this creature if able"
   *  requirement names every member of this stack. */
  mustBlock: boolean;
  /** CR 509.1c: every member of this stack carries a "must be blocked"
   *  static that the pending blocker's block would obey. */
  mustBeBlocked: boolean;
  /** CR 702.22c: the band every member of this stack shares, or `null` when
   *  none of them is banded. */
  bandId: number | null;
  /** Members of this stack already assigned to the pending blocker,
   *  ascending — kept in their stack rather than split out, so growing or
   *  shrinking the count can find them. */
  assignedIds: ObjectId[];
  /** The most of this stack's members the pending blocker may hold directly,
   *  from its published block capacity minus its direct assignments outside
   *  this stack (`partitionBlockTargets`'s `blockCapacities` option); `ids.length`
   *  when its capacity is absent or `null` (any number). Bounds the stepper's
   *  `max`. No band-specific ceiling. */
  maxAssignable: number;
}

/**
 * Maps each attacker's object id to every blocker directly UI-assigned to
 * it, ascending; an attacker no blocker is assigned to is absent. Band blocks
 * are not previewed here — the engine applies them when it processes the
 * declaration (`combat.rs::propagate_banding_block_state`, called from
 * `combat.rs::declare_blockers_for_player`).
 */
export function blockersByAttacker(
  blockerAssignments: ReadonlyMap<ObjectId, ReadonlySet<ObjectId>>,
): ReadonlyMap<ObjectId, readonly ObjectId[]> {
  const byId = new Map<ObjectId, ObjectId[]>();
  for (const [blockerId, attackerIds] of blockerAssignments) {
    for (const attackerId of attackerIds) {
      const bucket = byId.get(attackerId);
      if (bucket) bucket.push(blockerId);
      else byId.set(attackerId, [blockerId]);
    }
  }
  const result = new Map<ObjectId, readonly ObjectId[]>();
  for (const [attackerId, blockerIds] of byId) {
    result.set(attackerId, blockerIds.sort((a, b) => a - b));
  }
  return result;
}

/**
 * Split the pending blocker's legal candidate attackers into
 * {@link BlockTargetStack}s. Every input is engine-provided or UI-assigned
 * state; no legality is (re)computed here (CLAUDE.md: the frontend is a
 * display layer).
 */
export function partitionBlockTargets(
  candidateIds: ObjectId[],
  pendingBlocker: ObjectId,
  {
    attackers,
    blockerAssignments,
    blockRequirements,
    blockerConstraints,
    mustBeBlockedTargets,
    blockCapacities,
  }: {
    attackers: AttackerInfo[] | undefined;
    blockerAssignments: ReadonlyMap<ObjectId, ReadonlySet<ObjectId>>;
    blockRequirements: Record<string, BlockRequirementInfo> | undefined;
    blockerConstraints: Record<string, CombatRequirement> | undefined;
    mustBeBlockedTargets: Record<string, ObjectId[]> | undefined;
    blockCapacities: Record<string, number | null> | undefined;
  },
): BlockTargetStack[] {
  const attackTargetById = new Map<ObjectId, AttackTarget>();
  const bandIdById = new Map<ObjectId, number | null>();
  for (const attacker of attackers ?? []) {
    attackTargetById.set(attacker.object_id, attacker.attack_target);
    bandIdById.set(attacker.object_id, attacker.band_id ?? null);
  }
  const directBlockers = blockersByAttacker(blockerAssignments);
  const pendingConstraint = blockerConstraints?.[pendingBlocker];
  const mustBlockIds = new Set(
    pendingConstraint?.kind === "MustBlock" ? pendingConstraint.attackers ?? [] : [],
  );
  const mustBeBlockedIds = new Set(mustBeBlockedTargets?.[pendingBlocker] ?? []);
  const pendingAssignments = blockerAssignments.get(pendingBlocker);
  const capacity = blockCapacities?.[pendingBlocker];

  interface Bucket {
    ids: ObjectId[];
    attackTarget: AttackTarget;
    otherBlockerIds: ObjectId[];
    minBlockers: number;
    mustBlock: boolean;
    mustBeBlocked: boolean;
    bandId: number | null;
    assignedIds: ObjectId[];
  }
  const buckets = new Map<string, Bucket>();

  for (const id of [...candidateIds].sort((a, b) => a - b)) {
    const attackTarget = attackTargetById.get(id);
    // Every candidate is drawn from `valid_block_targets[pendingBlocker]`,
    // which only ever names an attacker the engine also reports in
    // `combat.attackers` — a missing record means the two payloads disagree,
    // and this stack cannot label the member, so it is dropped defensively
    // rather than guessing a target.
    if (!attackTarget) continue;
    const isAssigned = pendingAssignments?.has(id) ?? false;
    const otherBlockerIds = (directBlockers.get(id) ?? []).filter(
      (blockerId) => blockerId !== pendingBlocker,
    );
    const minBlockers = blockRequirements?.[id]?.count ?? 0;
    const mustBlock = mustBlockIds.has(id);
    const mustBeBlocked = mustBeBlockedIds.has(id);
    const bandId = bandIdById.get(id) ?? null;
    const signature = [
      attackTargetKey(attackTarget),
      otherBlockerIds.join(","),
      minBlockers,
      mustBlock,
      mustBeBlocked,
      bandId,
    ].join("|");

    const bucket = buckets.get(signature);
    if (bucket) {
      bucket.ids.push(id);
      if (isAssigned) bucket.assignedIds.push(id);
    } else {
      buckets.set(signature, {
        ids: [id],
        attackTarget,
        otherBlockerIds,
        minBlockers,
        mustBlock,
        mustBeBlocked,
        bandId,
        assignedIds: isAssigned ? [id] : [],
      });
    }
  }

  return Array.from(buckets.values())
    .map((bucket) => {
      // The most of this stack the pending blocker may hold directly: its
      // published capacity (`null`/absent = any number) minus its direct
      // assignments OUTSIDE this stack. No band-specific ceiling.
      const outside = (pendingAssignments?.size ?? 0) - bucket.assignedIds.length;
      const maxAssignable =
        capacity == null ? bucket.ids.length : Math.min(bucket.ids.length, Math.max(0, capacity - outside));
      return {
        key: String(bucket.ids[0]),
        ids: bucket.ids,
        count: bucket.ids.length,
        attackTarget: bucket.attackTarget,
        otherBlockerIds: bucket.otherBlockerIds,
        minBlockers: bucket.minBlockers,
        mustBlock: bucket.mustBlock,
        mustBeBlocked: bucket.mustBeBlocked,
        bandId: bucket.bandId,
        assignedIds: bucket.assignedIds,
        maxAssignable,
      };
    })
    .sort((a, b) => a.ids[0] - b.ids[0]);
}

/**
 * Deterministic grow/shrink for one {@link BlockTargetStack} (mirrors
 * {@link AttackTargetPicker}'s `lowestUnassigned` / `highestOnTarget`):
 * growing claims the lowest-id members not yet assigned to the pending
 * blocker, up to `stack.maxAssignable` and never past `stack.assignedIds`
 * (so an over-capacity stack a mutation elsewhere over-assigned can still
 * shrink); shrinking releases the highest-id already-assigned members and is
 * never bounded by `maxAssignable`. Returns the full set of this stack's
 * members that should be assigned to the pending blocker once the count
 * becomes `n`.
 */
export function blockTargetSelection(stack: BlockTargetStack, n: number): ObjectId[] {
  const requested = Math.max(0, Math.min(n, stack.ids.length));
  if (requested <= stack.assignedIds.length) {
    return stack.assignedIds.slice(0, requested);
  }
  const target = Math.max(stack.assignedIds.length, Math.min(requested, stack.maxAssignable));
  const assignedSet = new Set(stack.assignedIds);
  const unassigned = stack.ids.filter((id) => !assignedSet.has(id));
  return [...stack.assignedIds, ...unassigned.slice(0, target - stack.assignedIds.length)];
}
