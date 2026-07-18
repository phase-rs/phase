import { describe, expect, it } from "vitest";

import type { AttackTarget, GameObject, ObjectId } from "../../adapter/types";
import {
  buildGameObjectWithCoreTypes,
  buildObjectMap,
} from "../../test/factories/gameObjectFactory";
import { buildGameState } from "../../test/factories/gameStateFactory";
import {
  attackTargetsForAttacker,
  buildAttacks,
  commonAttackTargets,
  evenSplit,
  groupAttackers,
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
) {
  return buildGameState({ objects: buildObjectMap(...objects), ring_bearer: ringBearer });
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

  it("drops an attacker the engine gives no legal target (present map, missing key)", () => {
    expect(buildAttacks([101, 102], { "101": [P1] }, [P1])).toEqual([[101, P1]]);
  });

  it("uses the aggregate for a legacy payload", () => {
    expect(buildAttacks([101, 102], undefined, [P1])).toEqual([
      [101, P1],
      [102, P1],
    ]);
  });
});
