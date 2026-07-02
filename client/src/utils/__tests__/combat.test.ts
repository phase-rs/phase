import { describe, expect, it } from "vitest";

import type { GameObject, GameState, ObjectId } from "../../adapter/types";
import { evenSplit, groupAttackers } from "../combat";

function makeObject(overrides: Partial<GameObject> & { id: ObjectId }): GameObject {
  return {
    card_id: 100,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Goblin",
    power: 1,
    toughness: 1,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Red"],
    base_power: 1,
    base_toughness: 1,
    base_keywords: [],
    base_color: ["Red"],
    timestamp: 1,
    entered_battlefield_turn: 1,
    ...overrides,
  };
}

function makeState(
  objects: GameObject[],
  ringBearer?: Record<string, ObjectId | null>,
): GameState {
  const map: Record<string, GameObject> = {};
  for (const obj of objects) map[obj.id] = obj;
  return { objects: map, ring_bearer: ringBearer } as unknown as GameState;
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
});
