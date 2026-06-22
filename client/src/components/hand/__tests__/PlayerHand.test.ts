import { describe, expect, it } from "vitest";

import { computeHandInsertionSlot, computeHandInsertionMarker } from "../handInsertionSlot.ts";

const cardRects = [
  { objectId: 1, left: 0, width: 100 },
  { objectId: 2, left: 100, width: 100 },
  { objectId: 3, left: 200, width: 100 },
];

const markerRects = [
  { objectId: 1, left: 0, width: 100, top: 10, height: 140 },
  { objectId: 2, left: 80, width: 100, top: 0, height: 140 },
  { objectId: 3, left: 160, width: 100, top: 10, height: 140 },
];

describe("computeHandInsertionMarker", () => {
  it("places the caret at the leading edge of the card now at the slot (drag-excluded space)", () => {
    // dragging id 2 -> remaining [card1, card3]; slot 1 -> remaining[1] = card3 left edge
    expect(computeHandInsertionMarker(markerRects, 1, 2)).toEqual({ x: 160, top: 10, height: 140 });
  });

  it("places the caret before the first remaining card for slot 0", () => {
    expect(computeHandInsertionMarker(markerRects, 0, 2)).toEqual({ x: 0, top: 10, height: 140 });
  });

  it("places the caret after the last remaining card for the append slot", () => {
    // dragging id 2 -> remaining [card1, card3] (len 2); slot 2 -> append = card3.left + card3.width
    expect(computeHandInsertionMarker(markerRects, 2, 2)).toEqual({ x: 260, top: 10, height: 140 });
  });

  it("clamps an out-of-range slot to the append position", () => {
    expect(computeHandInsertionMarker(markerRects, 99, 2)).toEqual({ x: 260, top: 10, height: 140 });
  });

  it("returns null when no cards remain after excluding the dragged card", () => {
    expect(
      computeHandInsertionMarker([{ objectId: 5, left: 0, width: 100, top: 0, height: 10 }], 0, 5),
    ).toBeNull();
  });

  it("defaults missing top/height to 0", () => {
    expect(computeHandInsertionMarker([{ objectId: 1, left: 40, width: 100 }], 0, 9)).toEqual({
      x: 40,
      top: 0,
      height: 0,
    });
  });
});

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
