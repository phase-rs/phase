import { describe, it, expect } from "vitest";
import type { GameObject, ManaCost, ObjectId } from "../../../../adapter/types.ts";
import {
  orderCards,
  groupCards,
  applyBulk,
  rangeAdd,
} from "../gridSelection.ts";

const cost = (generic: number): ManaCost => ({ type: "Cost", shards: [], generic } as ManaCost);

function obj(id: number, name: string, types: string[], generic: number, color: string[]): GameObject {
  return {
    id,
    name,
    card_types: { supertypes: [], core_types: types, subtypes: [] },
    mana_cost: cost(generic),
    color,
  } as unknown as GameObject;
}

const objects: Record<ObjectId, GameObject> = {
  1: obj(1, "Zealot", ["Creature"], 3, ["Red"]),
  2: obj(2, "Island", ["Land"], 0, []),
  3: obj(3, "Ancestral", ["Instant"], 1, ["Blue"]),
  4: obj(4, "Bear", ["Creature"], 2, ["Green"]),
};
const all: ObjectId[] = [1, 2, 3, 4];

describe("orderCards", () => {
  it("sorts by name A->Z", () => {
    expect(orderCards(all, objects, "name")).toEqual([3, 4, 2, 1]);
  });
  it("sorts by ascending cmc, stable on ties", () => {
    expect(orderCards(all, objects, "cmc")).toEqual([2, 3, 4, 1]);
  });
  it("returns input order for 'none'", () => {
    expect(orderCards(all, objects, "none")).toEqual([1, 2, 3, 4]);
  });
});

describe("groupCards", () => {
  it("groups by primary core type with per-group ids", () => {
    const groups = groupCards([1, 4, 2, 3], objects, "type");
    expect(groups).toEqual([
      { key: "Creature", ids: [1, 4] },
      { key: "Land", ids: [2] },
      { key: "Instant", ids: [3] },
    ]);
  });
  it("returns a single unnamed group for 'none'", () => {
    expect(groupCards(all, objects, "none")).toEqual([{ key: "", ids: [1, 2, 3, 4] }]);
  });
});

describe("applyBulk", () => {
  it("select-all fills to cap in display order", () => {
    expect(applyBulk("all", all, new Set(), 2)).toEqual(new Set([1, 2]));
  });
  it("clear empties", () => {
    expect(applyBulk("clear", all, new Set([1, 2]), 2)).toEqual(new Set());
  });
  it("invert takes the complement, truncated to cap in display order", () => {
    // value = {1}; complement = {2,3,4}; cap 2 -> first two of display order = {2,3}
    expect(applyBulk("invert", all, new Set([1]), 2)).toEqual(new Set([2, 3]));
  });
});

describe("rangeAdd", () => {
  it("adds the inclusive index range, respecting cap", () => {
    expect(rangeAdd(all, 0, 3, new Set(), 3)).toEqual(new Set([1, 2, 3]));
  });
  it("handles reversed indices", () => {
    expect(rangeAdd(all, 3, 1, new Set(), 10)).toEqual(new Set([2, 3, 4]));
  });
  it("clamps out-of-bounds endpoints and never adds undefined", () => {
    // A stale shift anchor past the end (10) plus an unmapped index (-1) must
    // clamp into [0, len-1] rather than indexing past the dense list and
    // polluting the set with `undefined` (which fails engine set-membership).
    const result = rangeAdd(all, 10, -1, new Set(), 10);
    expect(result).toEqual(new Set([1, 2, 3, 4]));
    expect([...result]).not.toContain(undefined);
  });
  it("returns the existing value unchanged for an empty list", () => {
    expect(rangeAdd([], 0, 5, new Set([1]), 10)).toEqual(new Set([1]));
  });
});
