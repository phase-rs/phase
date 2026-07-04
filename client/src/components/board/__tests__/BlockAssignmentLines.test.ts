import { describe, expect, it } from "vitest";

import { filterVisibleBlockerPairs } from "../blockAssignmentVisibility.ts";

describe("filterVisibleBlockerPairs", () => {
  it("keeps blocker pairs controlled by visible players", () => {
    const pairs = new Map([
      [10, 100],
      [20, 200],
      [30, 300],
    ]);
    const objects = {
      10: { controller: 1 },
      20: { controller: 2 },
      30: { controller: 3 },
    };

    expect([...filterVisibleBlockerPairs(pairs, objects, new Set([0, 2])).entries()]).toEqual([
      [20, 200],
    ]);
  });

  it("keeps all live split-mode opponent pairs when every opponent is visible", () => {
    const pairs = new Map([
      [10, 100],
      [20, 200],
      [30, 300],
    ]);
    const objects = {
      10: { controller: 1 },
      20: { controller: 2 },
      30: { controller: 3 },
    };

    expect([...filterVisibleBlockerPairs(pairs, objects, new Set([0, 1, 2, 3])).entries()]).toEqual([
      [10, 100],
      [20, 200],
      [30, 300],
    ]);
  });
});
