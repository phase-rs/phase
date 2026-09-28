import { describe, expect, it } from "vitest";

import type { AttackerInfo, AttackTarget, GameObject, ObjectId } from "../../adapter/types";
import {
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../test/factories/gameObjectFactory";
import { buildGameState } from "../../test/factories/gameStateFactory";
import {
  attackTargetsForAttacker,
  blockersByAttacker,
  blockTargetSelection,
  type BlockTargetStack,
  buildAttacks,
  commonAttackTargets,
  evenSplit,
  groupAttackers,
  partitionBlockTargets,
} from "../combat";

const P1: AttackTarget = { type: "Player", data: 1 };
const P2: AttackTarget = { type: "Player", data: 2 };
const PW: AttackTarget = { type: "Planeswalker", data: 50 };

function makeObject(overrides: Partial<GameObject> & { id: ObjectId }): GameObject {
  return buildGameObjectWithCoreTypes(["Creature"], {
    card_id: 100,
    zone: "Battlefield",
    name: "Goblin",
    power: 1,
    toughness: 1,
    color: ["Red"],
    base_power: 1,
    base_toughness: 1,
    base_color: ["Red"],
    ...overrides,
  });
}

function makeState(
  objects: GameObject[],
  ringBearer?: Record<string, ObjectId | null>,
  unboundedPile?: ObjectId[],
) {
  return buildGameState({
    objects: buildObjectMap(...objects),
    ring_bearer: ringBearer,
    // CR 732.2a: engine-authored ∞-pile membership (mirrors DerivedViews::unbounded_pile).
    derived: unboundedPile ? { unbounded_pile: unboundedPile } : undefined,
  });
}

describe("evenSplit", () => {
  it("distributes evenly with no remainder", () => {
    expect(evenSplit(30, 3)).toEqual([10, 10, 10]);
  });

  it("front-loads the remainder onto the earliest buckets", () => {
    expect(evenSplit(31, 3)).toEqual([11, 10, 10]);
    expect(evenSplit(2, 5)).toEqual([1, 1, 0, 0, 0]);
  });

  it("returns all zeros for a non-positive count", () => {
    expect(evenSplit(0, 3)).toEqual([0, 0, 0]);
    expect(evenSplit(-4, 2)).toEqual([0, 0]);
  });

  it("returns an empty array when there are no buckets", () => {
    expect(evenSplit(5, 0)).toEqual([]);
    expect(evenSplit(5, -1)).toEqual([]);
  });

  it("always sums back to the (clamped) count and has the right length", () => {
    for (const [count, buckets] of [[31, 3], [7, 7], [1, 4], [100, 6]] as const) {
      const split = evenSplit(count, buckets);
      expect(split).toHaveLength(buckets);
      expect(split.reduce((a, b) => a + b, 0)).toBe(count);
    }
  });
});

describe("groupAttackers", () => {
  it("groups identical creatures into one stack and distinct ones separately", () => {
    const state = makeState([
      makeObject({ id: 200, name: "Elf", power: 2, toughness: 2 }),
      makeObject({ id: 103 }),
      makeObject({ id: 101 }),
      makeObject({ id: 102 }),
    ]);

    const stacks = groupAttackers([200, 103, 101, 102], state);

    expect(stacks).toHaveLength(2);
    // Stacks are sorted by their lowest member id.
    expect(stacks[0]).toMatchObject({ name: "Goblin", count: 3, ids: [101, 102, 103] });
    expect(stacks[1]).toMatchObject({ name: "Elf", count: 1, ids: [200] });
    expect(stacks[0].key).toBe("101");
    expect(stacks[0].representative?.id).toBe(101);
  });

  it("sorts member ids ascending regardless of input order", () => {
    const state = makeState([
      makeObject({ id: 5 }),
      makeObject({ id: 1 }),
      makeObject({ id: 9 }),
    ]);
    const [stack] = groupAttackers([9, 1, 5], state);
    expect(stack.ids).toEqual([1, 5, 9]);
  });

  it("keeps the Ring-bearer as its own stack (CR 701.54)", () => {
    const state = makeState(
      [makeObject({ id: 101 }), makeObject({ id: 102 }), makeObject({ id: 103 })],
      { "0": 102 },
    );

    const stacks = groupAttackers([101, 102, 103], state);

    expect(stacks).toHaveLength(2);
    expect(stacks[0]).toMatchObject({ count: 2, ids: [101, 103] });
    expect(stacks[1]).toMatchObject({ count: 1, ids: [102] });
  });

  it("falls back to singleton stacks (sorted) when state is missing", () => {
    const stacks = groupAttackers([3, 1, 2], null);
    expect(stacks.map((s) => s.ids)).toEqual([[1], [2], [3]]);
    expect(stacks.every((s) => s.count === 1 && s.representative === null)).toBe(true);
    // The ∞-pile flag defaults false when there is no state to consult.
    expect(stacks.every((s) => s.isUnboundedPile === false)).toBe(true);
  });

  // CR 732.2a: the rebuilt AttackerStack must carry `isUnboundedPile` from
  // groupByName so the picker renders `∞`. Reachable because the ∞ pile is a
  // persistent object-id snapshot (derived_views.rs re-filters only on battlefield
  // membership, not `tapped`), so a member that untaps on a later turn can attack.
  // Discriminating: the pile stack (Goblin) and the non-pile stack (Elf) differ
  // only by membership — dropping the `unbounded_pile` thread in groupAttackers
  // flips the Goblin stack to `false` and this assertion fails.
  it("carries ∞-pile membership onto the rebuilt stack shape", () => {
    const state = makeState(
      [
        makeObject({ id: 101 }),
        makeObject({ id: 102 }),
        makeObject({ id: 200, name: "Elf", power: 2, toughness: 2 }),
      ],
      undefined,
      [101, 102],
    );

    const stacks = groupAttackers([200, 101, 102], state);
    const goblins = stacks.find((s) => s.name === "Goblin");
    const elf = stacks.find((s) => s.name === "Elf");

    expect(goblins?.isUnboundedPile).toBe(true);
    expect(elf?.isUnboundedPile).toBe(false);
  });

  it("splits identically-named attackers with different legal-target sets into separate stacks", () => {
    const state = makeState([
      makeObject({ id: 101 }),
      makeObject({ id: 102 }),
      makeObject({ id: 103 }),
    ]);
    // 101/102 share [P1, P2]; 103 can only attack P1 — it must not stack with them.
    const targetsFor = (id: ObjectId): AttackTarget[] =>
      id === 103 ? [P1] : [P1, P2];

    const stacks = groupAttackers([101, 102, 103], state, targetsFor);

    expect(stacks).toHaveLength(2);
    expect(stacks[0]).toMatchObject({ name: "Goblin", ids: [101, 102], targets: [P1, P2] });
    expect(stacks[1]).toMatchObject({ name: "Goblin", ids: [103], targets: [P1] });
  });
});

describe("attackTargetsForAttacker", () => {
  it("returns the attacker's own bucket when the engine map is present", () => {
    expect(attackTargetsForAttacker(101, { "101": [P1, PW], "102": [P2] }, [P1, P2, PW])).toEqual([P1, PW]);
  });

  it("treats a missing key in a present map as no legal targets (no fallback)", () => {
    expect(attackTargetsForAttacker(999, { "101": [P1] }, [P1, P2])).toEqual([]);
  });

  it("falls back to the aggregate only for a legacy payload (undefined map)", () => {
    expect(attackTargetsForAttacker(101, undefined, [P1, P2])).toEqual([P1, P2]);
  });
});

describe("commonAttackTargets", () => {
  it("returns the intersection of every selected attacker's legal set, in aggregate order", () => {
    expect(
      commonAttackTargets([101, 102], { "101": [P1, P2], "102": [P1] }, [P1, P2]),
    ).toEqual([P1]);
  });

  it("is empty when the selected attackers share no legal target", () => {
    expect(
      commonAttackTargets([101, 102], { "101": [P1], "102": [P2] }, [P1, P2]),
    ).toEqual([]);
  });

  it("is the whole aggregate for a legacy payload (every attacker shares it)", () => {
    expect(commonAttackTargets([101, 102], undefined, [P1, P2])).toEqual([P1, P2]);
  });
});

describe("buildAttacks", () => {
  it("pairs each attacker with its sole engine-provided target (no default injection)", () => {
    expect(buildAttacks([101, 102], { "101": [P1], "102": [P2] }, [P1, P2])).toEqual([
      [101, P1],
      [102, P2],
    ]);
  });

  it("returns null instead of dropping an attacker with empty authoritative support", () => {
    expect(buildAttacks([101, 102], { "101": [P1], "102": [] }, [P1])).toBeNull();
  });

  it("uses the aggregate for a legacy payload", () => {
    expect(buildAttacks([101, 102], undefined, [P1])).toEqual([
      [101, P1],
      [102, P1],
    ]);
  });
});

function attacker(
  objectId: ObjectId,
  attackTarget: AttackTarget,
  bandId?: number | null,
): AttackerInfo {
  return { object_id: objectId, defending_player: 0, attack_target: attackTarget, band_id: bandId };
}

describe("partitionBlockTargets", () => {
  const noAssignments = new Map<ObjectId, Set<ObjectId>>();

  it("splits by attack target (player vs planeswalker)", () => {
    const stacks = partitionBlockTargets([11, 12, 13, 14], 100, {
      attackers: [attacker(11, P1), attacker(12, P1), attacker(13, PW), attacker(14, PW)],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(2);
    expect(stacks[0]).toMatchObject({ ids: [11, 12], attackTarget: P1 });
    expect(stacks[1]).toMatchObject({ ids: [13, 14], attackTarget: PW });
  });

  it("splits by other UI-assigned blockers, excluding the pending blocker itself", () => {
    const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([
      [200, new Set([11])],
      [100, new Set([11, 12, 13])],
    ]);
    const stacks = partitionBlockTargets([11, 12, 13], 100, {
      attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1)],
      blockerAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(2);
    expect(stacks.find((s) => s.ids.includes(11))).toMatchObject({ ids: [11], otherBlockerIds: [200] });
    expect(stacks.find((s) => s.ids.includes(12))).toMatchObject({ ids: [12, 13], otherBlockerIds: [] });
  });

  it("splits by minimum-blocker count", () => {
    const stacks = partitionBlockTargets([11, 12], 100, {
      attackers: [attacker(11, P1), attacker(12, P1)],
      blockerAssignments: noAssignments,
      blockRequirements: { 12: { count: 2 } },
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(2);
    expect(stacks.find((s) => s.ids.includes(11))).toMatchObject({ minBlockers: 0 });
    expect(stacks.find((s) => s.ids.includes(12))).toMatchObject({ minBlockers: 2 });
  });

  it("splits out the pending blocker's own must-block attackers", () => {
    const blockerConstraints = { 100: { kind: "MustBlock" as const, attackers: [12] } };
    const stacks = partitionBlockTargets([11, 12], 100, {
      attackers: [attacker(11, P1), attacker(12, P1)],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(2);
    expect(stacks.find((s) => s.ids.includes(11))).toMatchObject({ mustBlock: false });
    expect(stacks.find((s) => s.ids.includes(12))).toMatchObject({ mustBlock: true });
  });

  it("does not apply a must-block constraint named for a DIFFERENT blocker", () => {
    const blockerConstraints = { 100: { kind: "MustBlock" as const, attackers: [12] } };
    const stacks = partitionBlockTargets([11, 12], 200, {
      attackers: [attacker(11, P1), attacker(12, P1)],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(1);
    expect(stacks[0]).toMatchObject({ ids: [11, 12], mustBlock: false });
  });

  it("splits out members listed under the pending blocker in mustBeBlockedTargets, and does not split them for a different pending blocker whose entry omits them", () => {
    const mustBeBlockedTargets = { 100: [12] };
    const forPending = partitionBlockTargets([11, 12], 100, {
      attackers: [attacker(11, P1), attacker(12, P1)],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets,
      blockCapacities: undefined,
    });
    expect(forPending).toHaveLength(2);
    expect(forPending.find((s) => s.ids.includes(11))).toMatchObject({ mustBeBlocked: false });
    expect(forPending.find((s) => s.ids.includes(12))).toMatchObject({ mustBeBlocked: true });

    const forOtherBlocker = partitionBlockTargets([11, 12], 200, {
      attackers: [attacker(11, P1), attacker(12, P1)],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets,
      blockCapacities: undefined,
    });
    expect(forOtherBlocker).toHaveLength(1);
    expect(forOtherBlocker[0]).toMatchObject({ ids: [11, 12], mustBeBlocked: false });
  });

  it("splits by band membership (CR 702.22c), and does not split a bandless member from another bandless one", () => {
    const stacks = partitionBlockTargets([11, 12, 13, 14], 100, {
      attackers: [
        attacker(11, P1, 1),
        attacker(12, P1, 2),
        attacker(13, P1, null),
        attacker(14, P1),
      ],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(3);
    expect(stacks.find((s) => s.ids.includes(11))).toMatchObject({ ids: [11], bandId: 1 });
    expect(stacks.find((s) => s.ids.includes(12))).toMatchObject({ ids: [12], bandId: 2 });
    expect(stacks.find((s) => s.ids.includes(13))).toMatchObject({ ids: [13, 14], bandId: null });
  });

  it("keeps members already assigned to the pending blocker in their stack and reports them", () => {
    const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[100, new Set([11])]]);
    const stacks = partitionBlockTargets([11, 12, 13], 100, {
      attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1)],
      blockerAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(1);
    expect(stacks[0]).toMatchObject({ ids: [11, 12, 13], assignedIds: [11] });
  });

  it("sorts stacks and members ascending, keying each stack by its lowest member id", () => {
    const stacks = partitionBlockTargets([13, 11, 12], 100, {
      attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1)],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(1);
    expect(stacks[0]).toMatchObject({ key: "11", ids: [11, 12, 13] });
  });

  it("drops a candidate with no attacker record instead of guessing its target", () => {
    const stacks = partitionBlockTargets([11, 99], 100, {
      attackers: [attacker(11, P1)],
      blockerAssignments: noAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(1);
    expect(stacks[0].ids).toEqual([11]);
  });

  it("reports only direct assignments in otherBlockerIds, so a band-mate of an assigned attacker stays in its own stack", () => {
    const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[101, new Set([14])]]);
    const stacks = partitionBlockTargets([14, 15], 100, {
      attackers: [attacker(14, P1, 5), attacker(15, P1, 5)],
      blockerAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(2);
    // Reach guard: the [14] stack's [101] shows the direct-blockers lookup fires.
    expect(stacks.find((s) => s.ids.includes(14))).toMatchObject({
      ids: [14],
      otherBlockerIds: [101],
      bandId: 5,
    });
    expect(stacks.find((s) => s.ids.includes(15))).toMatchObject({
      ids: [15],
      otherBlockerIds: [],
      bandId: 5,
    });
  });

  it("keeps bandless attackers in separate stacks by their own direct assignments", () => {
    const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[101, new Set([14])]]);
    const stacks = partitionBlockTargets([14, 15], 100, {
      attackers: [attacker(14, P1, null), attacker(15, P1, null)],
      blockerAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: undefined,
    });

    expect(stacks).toHaveLength(2);
    expect(stacks.find((s) => s.ids.includes(14))).toMatchObject({ otherBlockerIds: [101] });
    expect(stacks.find((s) => s.ids.includes(15))).toMatchObject({ otherBlockerIds: [] });
  });

  it("caps a banded stack by the capacity the pending blocker spent on a band-mate outside the candidates", () => {
    const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[100, new Set([14])]]);
    const board = {
      attackers: [attacker(14, P1, 5), attacker(15, P1, 5), attacker(16, P1, 5)],
      blockerAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
    };
    const capped = partitionBlockTargets([15, 16], 100, { ...board, blockCapacities: { 100: 1 } });
    expect(capped).toHaveLength(1);
    expect(capped[0]).toMatchObject({
      ids: [15, 16],
      assignedIds: [],
      otherBlockerIds: [],
      bandId: 5,
      maxAssignable: 0,
    });

    const unlimited = partitionBlockTargets([15, 16], 100, { ...board, blockCapacities: { 100: null } });
    expect(unlimited[0]).toMatchObject({
      maxAssignable: 2,
    });
  });

  it("keeps the pending blocker's own band pick in one stack with its unpicked band-mates", () => {
    const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[100, new Set([14])]]);
    const stacks = partitionBlockTargets([14, 15, 16], 100, {
      attackers: [attacker(14, P1, 5), attacker(15, P1, 5), attacker(16, P1, 5)],
      blockerAssignments,
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: undefined,
      blockCapacities: { 100: null },
    });

    expect(stacks).toHaveLength(1);
    expect(stacks[0]).toMatchObject({
      ids: [14, 15, 16],
      assignedIds: [14],
      otherBlockerIds: [],
      maxAssignable: 3,
    });
  });

  it("lets a capacity-2 blocker take two direct picks in a must-be-blocked band (CR 509.1c)", () => {
    const board = {
      attackers: [attacker(14, P1, 5), attacker(15, P1, 5), attacker(16, P1, 5)],
      blockRequirements: undefined,
      blockerConstraints: undefined,
      mustBeBlockedTargets: { 100: [14, 15, 16] },
      blockCapacities: { 100: 2 },
    };

    const none = partitionBlockTargets([14, 15, 16], 100, {
      ...board,
      blockerAssignments: new Map<ObjectId, Set<ObjectId>>(),
    });
    expect(none).toHaveLength(1);
    expect(none[0]).toMatchObject({
      ids: [14, 15, 16],
      mustBeBlocked: true,
      maxAssignable: 2,
    });

    const withOnePick = partitionBlockTargets([14, 15, 16], 100, {
      ...board,
      blockerAssignments: new Map<ObjectId, Set<ObjectId>>([[100, new Set([14])]]),
    });
    expect(withOnePick).toHaveLength(1);
    expect(withOnePick[0]).toMatchObject({
      ids: [14, 15, 16],
      assignedIds: [14],
      mustBeBlocked: true,
      maxAssignable: 2,
    });
  });

  describe("maxAssignable ceiling", () => {
    it("caps a bandless stack at the pending blocker's remaining capacity", () => {
      const [stack] = partitionBlockTargets([11, 12, 13, 14], 100, {
        attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1), attacker(14, P1)],
        blockerAssignments: noAssignments,
        blockRequirements: undefined,
        blockerConstraints: undefined,
        mustBeBlockedTargets: undefined,
        blockCapacities: { 100: 1 },
      });
      expect(stack).toMatchObject({ maxAssignable: 1 });
    });

    it("subtracts the pending blocker's assignment OUTSIDE the stack, even to a non-candidate id", () => {
      const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[100, new Set([99])]]);
      const [stack] = partitionBlockTargets([11, 12, 13], 100, {
        attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1)],
        blockerAssignments,
        blockRequirements: undefined,
        blockerConstraints: undefined,
        mustBeBlockedTargets: undefined,
        blockCapacities: { 100: 2 },
      });
      expect(stack).toMatchObject({ maxAssignable: 1 });
    });

    it("does not double-count the stack's own assignments as outside", () => {
      const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[100, new Set([11])]]);
      const [stack] = partitionBlockTargets([11, 12, 13], 100, {
        attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1)],
        blockerAssignments,
        blockRequirements: undefined,
        blockerConstraints: undefined,
        mustBeBlockedTargets: undefined,
        blockCapacities: { 100: 2 },
      });
      expect(stack).toMatchObject({ maxAssignable: 2 });
    });

    it("imposes no ceiling for a null capacity or an absent capacities map", () => {
      const forNull = partitionBlockTargets([11, 12, 13, 14], 100, {
        attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1), attacker(14, P1)],
        blockerAssignments: noAssignments,
        blockRequirements: undefined,
        blockerConstraints: undefined,
        mustBeBlockedTargets: undefined,
        blockCapacities: { 100: null },
      });
      expect(forNull[0]).toMatchObject({ maxAssignable: 4 });

      const forUndefined = partitionBlockTargets([11, 12, 13, 14], 100, {
        attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1), attacker(14, P1)],
        blockerAssignments: noAssignments,
        blockRequirements: undefined,
        blockerConstraints: undefined,
        mustBeBlockedTargets: undefined,
        blockCapacities: undefined,
      });
      expect(forUndefined[0]).toMatchObject({ maxAssignable: 4 });
    });

    it("sizes a banded stack's ceiling by stack size alone under an unlimited capacity — no band-specific cap", () => {
      const [stack] = partitionBlockTargets([14, 15, 16], 100, {
        attackers: [attacker(14, P1, 5), attacker(15, P1, 5), attacker(16, P1, 5)],
        blockerAssignments: noAssignments,
        blockRequirements: undefined,
        blockerConstraints: undefined,
        mustBeBlockedTargets: undefined,
        blockCapacities: { 100: null },
      });
      expect(stack).toMatchObject({ maxAssignable: 3 });
    });

    it("clamps to the capacity even when it is below the stack's already-assigned count", () => {
      const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([[100, new Set([11, 12])]]);
      const [stack] = partitionBlockTargets([11, 12, 13], 100, {
        attackers: [attacker(11, P1), attacker(12, P1), attacker(13, P1)],
        blockerAssignments,
        blockRequirements: undefined,
        blockerConstraints: undefined,
        mustBeBlockedTargets: undefined,
        blockCapacities: { 100: 1 },
      });
      expect(stack).toMatchObject({ maxAssignable: 1, assignedIds: [11, 12] });
    });
  });
});

describe("blockersByAttacker", () => {
  it("maps each attacker to the blockers directly assigned to it, ascending", () => {
    const blockerAssignments = new Map<ObjectId, Set<ObjectId>>([
      [102, new Set([11])],
      [101, new Set([11, 12])],
    ]);
    const map = blockersByAttacker(blockerAssignments);
    expect(map.get(11)).toEqual([101, 102]);
    expect(map.get(12)).toEqual([101]);
    expect(map.size).toBe(2);
  });

  it("omits an attacker no blocker is assigned to", () => {
    const map = blockersByAttacker(new Map());
    expect(map.size).toBe(0);
  });
});

describe("blockTargetSelection", () => {
  function makeStack(overrides: Partial<BlockTargetStack> = {}): BlockTargetStack {
    return {
      key: "11",
      ids: [11, 12, 13, 14],
      count: 4,
      attackTarget: P1,
      otherBlockerIds: [],
      minBlockers: 0,
      mustBlock: false,
      mustBeBlocked: false,
      bandId: null,
      assignedIds: [11, 12],
      maxAssignable: 4,
      ...overrides,
    };
  }

  it("grows by adding the lowest-id unassigned members", () => {
    expect(blockTargetSelection(makeStack(), 3)).toEqual([11, 12, 13]);
    expect(blockTargetSelection(makeStack(), 4)).toEqual([11, 12, 13, 14]);
  });

  it("shrinks by dropping the highest-id already-assigned members", () => {
    expect(blockTargetSelection(makeStack(), 1)).toEqual([11]);
    expect(blockTargetSelection(makeStack(), 0)).toEqual([]);
  });

  it("clamps below 0 and above maxAssignable", () => {
    expect(blockTargetSelection(makeStack(), -5)).toEqual([]);
    expect(blockTargetSelection(makeStack(), 99)).toEqual([11, 12, 13, 14]);
  });

  it("is the identity at the current assigned count", () => {
    expect(blockTargetSelection(makeStack(), 2)).toEqual([11, 12]);
  });

  it("grows no further than maxAssignable when it is below the stack size", () => {
    expect(
      blockTargetSelection(makeStack({ assignedIds: [11], maxAssignable: 2 }), 4),
    ).toEqual([11, 12]);
  });

  it("still shrinks correctly when already-assigned exceeds maxAssignable (over capacity)", () => {
    expect(
      blockTargetSelection(makeStack({ assignedIds: [11, 12, 13], maxAssignable: 1 }), 2),
    ).toEqual([11, 12]);
  });
});
