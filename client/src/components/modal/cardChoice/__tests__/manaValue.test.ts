import { describe, it, expect } from "vitest";
import type { ManaCost } from "../../../../adapter/types.ts";
import { manaValueOfObject } from "../manaValue.ts";

const cost = (shards: string[], generic: number): ManaCost => ({
  type: "Cost",
  shards,
  generic,
} as ManaCost);

describe("manaValueOfObject", () => {
  it("sums generic plus per-shard weights (hybrid two-X shards weigh 2)", () => {
    // {3}{U}{2/W} -> 3 + 1 + 2 = 6
    expect(manaValueOfObject({ mana_cost: cost(["Blue", "TwoWhite"], 3) })).toBe(6);
  });

  it("treats X shards as 0", () => {
    expect(manaValueOfObject({ mana_cost: cost(["X"], 0) })).toBe(0);
  });

  it("returns 0 for NoCost / SelfManaCost / SelfManaValue", () => {
    expect(manaValueOfObject({ mana_cost: { type: "NoCost" } as ManaCost })).toBe(0);
    expect(manaValueOfObject({ mana_cost: { type: "SelfManaCost" } as ManaCost })).toBe(0);
    expect(manaValueOfObject({ mana_cost: { type: "SelfManaValue" } as ManaCost })).toBe(0);
  });
});
