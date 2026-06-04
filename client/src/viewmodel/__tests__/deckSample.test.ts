import { describe, expect, it } from "vitest";

import { sampleOpeningHand } from "../deckSample";

describe("deckSample", () => {
  it("returns up to seven card names", () => {
    const hand = sampleOpeningHand([
      { name: "Lightning Bolt", count: 4 },
      { name: "Island", count: 10 },
    ]);
    expect(hand).toHaveLength(7);
    expect(hand.every((name) => ["Lightning Bolt", "Island"].includes(name))).toBe(true);
  });
});
