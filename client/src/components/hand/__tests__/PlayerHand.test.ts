import { describe, expect, it } from "vitest";

import { computeHandInsertionSlot } from "../PlayerHand.tsx";

const cardRects = [
  { objectId: 1, left: 0, width: 100 },
  { objectId: 2, left: 100, width: 100 },
  { objectId: 3, left: 200, width: 100 },
];

describe("computeHandInsertionSlot", () => {
  it("returns the slot after the final remaining card", () => {
    expect(computeHandInsertionSlot(cardRects, 280, 1)).toBe(2);
  });

  it("returns the slot before the first remaining card", () => {
    expect(computeHandInsertionSlot(cardRects, 25, 3)).toBe(0);
  });

  it("returns middle insertion slots around remaining card centers", () => {
    expect(computeHandInsertionSlot(cardRects, 125, 3)).toBe(1);
  });
});
