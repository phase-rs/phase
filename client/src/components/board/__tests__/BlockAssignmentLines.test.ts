import { describe, expect, it } from "vitest";

import { filterVisibleBlockerPairs } from "../blockAssignmentVisibility.ts";

describe("filterVisibleBlockerPairs", () => {
  it("keeps blocker pairs controlled by visible players", () => {
    const pairs: [number, number][] = [
      [10, 100],
      [20, 200],
      [30, 300],
    ];
    const objects = {
      10: { controller: 1 },
      20: { controller: 2 },
      30: { controller: 3 },
    };

    expect(filterVisibleBlockerPairs(pairs, objects, new Set([0, 2]))).toEqual([
      [20, 200],
    ]);
  });

  it("keeps all live split-mode opponent pairs when every opponent is visible", () => {
    const pairs: [number, number][] = [
      [10, 100],
      [20, 200],
      [30, 300],
    ];
    const objects = {
      10: { controller: 1 },
      20: { controller: 2 },
      30: { controller: 3 },
    };

    expect(filterVisibleBlockerPairs(pairs, objects, new Set([0, 1, 2, 3]))).toEqual([
      [10, 100],
      [20, 200],
      [30, 300],
    ]);
  });

  it("retains every pair when one visible blocker blocks multiple attackers", () => {
    const pairs: [number, number][] = [
      [10, 100],
      [10, 200],
    ];
    const objects = { 10: { controller: 2 } };

    expect(filterVisibleBlockerPairs(pairs, objects, new Set([0, 2]))).toEqual([
      [10, 100],
      [10, 200],
    ]);
  });
});
