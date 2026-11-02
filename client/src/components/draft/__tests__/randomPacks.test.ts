import { describe, expect, it } from "vitest";

import { randomPackLineup } from "../randomPacks";

const SETS = [
  { code: "AAA", name: "Alpha" },
  { code: "BBB", name: "Beta" },
  { code: "CCC", name: "Gamma" },
];

/** A random source that replays `draws`, so each test fixes the outcome. */
function replay(...draws: number[]) {
  let next = 0;
  return () => {
    if (next >= draws.length) throw new Error("random source drawn more often than expected");
    return draws[next++];
  };
}

describe("randomPackLineup", () => {
  it("fills every booster from one drawn set", () => {
    // One draw only: the set is chosen once, not per booster.
    expect(randomPackLineup(SETS, 3, "OneSet", replay(0.5))).toEqual([
      { code: "BBB", name: "Beta" },
      { code: "BBB", name: "Beta" },
      { code: "BBB", name: "Beta" },
    ]);
  });

  it("draws a set for each booster, allowing repeats", () => {
    expect(randomPackLineup(SETS, 4, "EachPack", replay(0.9, 0.0, 0.9, 0.4))).toEqual([
      { code: "CCC", name: "Gamma" },
      { code: "AAA", name: "Alpha" },
      { code: "CCC", name: "Gamma" },
      { code: "BBB", name: "Beta" },
    ]);
  });

  it("draws distinct sets for a candidate pool", () => {
    // 0.9 of three slots takes Gamma; 0.0 of the remaining two takes the
    // set now in slot 1. Drawing the same value twice still cannot repeat.
    const lineup = randomPackLineup(SETS, 2, "DistinctSets", replay(0.9, 0.0));
    expect(lineup).toEqual([
      { code: "CCC", name: "Gamma" },
      { code: "BBB", name: "Beta" },
    ]);
    expect(new Set(lineup.map((pack) => pack.code)).size).toBe(2);
  });

  it("caps a candidate pool at the sets that exist", () => {
    const lineup = randomPackLineup(SETS, 5, "DistinctSets", replay(0.0, 0.0, 0.0));
    expect(lineup.map((pack) => pack.code).sort()).toEqual(["AAA", "BBB", "CCC"]);
  });

  it("never indexes past the last set on a draw at the top of the range", () => {
    // `Math.random` excludes 1, but a source at the boundary must not yield
    // an undefined pack.
    expect(randomPackLineup(SETS, 1, "OneSet", () => 1)).toEqual([{ code: "CCC", name: "Gamma" }]);
  });

  it("keeps only the pack fields of the offered sets", () => {
    const decorated = [{ code: "AAA", name: "Alpha", icon: "a.svg", releasedAt: "2020-01-01" }];
    expect(randomPackLineup(decorated, 1, "EachPack", replay(0.3))).toEqual([
      { code: "AAA", name: "Alpha" },
    ]);
  });

  it("returns no packs when there is nothing to draw from or nothing to fill", () => {
    expect(randomPackLineup([], 3, "OneSet", replay())).toEqual([]);
    expect(randomPackLineup(SETS, 0, "EachPack", replay())).toEqual([]);
  });
});
