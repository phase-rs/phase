import { describe, expect, it } from "vitest";
import type { ObjectId } from "../../adapter/types";
import type { GroupedPermanent } from "../battlefieldProps";
import { sortCreaturesForBlockers } from "../blockerSorting";

// ObjectId is a number; the sorter only reads `group.ids`, so a minimal stub suffices.
const g = (...ids: ObjectId[]) => ({ ids }) as unknown as GroupedPermanent;

describe("sortCreaturesForBlockers", () => {
  it("orders blockers by their assigned attacker's opponent column", () => {
    const players = [g(1), g(2)];
    const opponents = [g(102), g(101)]; // atk 102 -> col 0, atk 101 -> col 1
    const assignments = new Map<ObjectId, ObjectId>([
      [1, 101], // col 1
      [2, 102], // col 0
    ]);
    const sorted = sortCreaturesForBlockers(players, opponents, assignments);
    expect(sorted.map((p) => p.ids[0])).toEqual([2, 1]);
  });

  it("keeps a stable order for blockers whose attacker has no opponent column", () => {
    // Both blockers are assigned to attackers that are NOT in the opponent row,
    // so both min-columns are Infinity. The raw `colA - colB` is Infinity -
    // Infinity = NaN, which makes the comparator's ordering implementation-
    // defined. The result must preserve the input order deterministically.
    const players = [g(1), g(2), g(3)];
    const opponents: GroupedPermanent[] = []; // no attacker columns at all
    const assignments = new Map<ObjectId, ObjectId>([
      [1, 901],
      [2, 902],
      [3, 903],
    ]);
    const sorted = sortCreaturesForBlockers(players, opponents, assignments);
    expect(sorted.map((p) => p.ids[0])).toEqual([1, 2, 3]);
  });

  it("sorts off-row (no-column) blockers after those with a real column", () => {
    const players = [g(10), g(11)];
    const opponents = [g(500)]; // col 0
    const assignments = new Map<ObjectId, ObjectId>([
      [10, 999], // Infinity (not on the opponent row)
      [11, 500], // col 0
    ]);
    const sorted = sortCreaturesForBlockers(players, opponents, assignments);
    expect(sorted.map((p) => p.ids[0])).toEqual([11, 10]);
  });
});
