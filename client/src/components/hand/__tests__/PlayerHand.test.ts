import { describe, expect, it } from "vitest";

import {
  computeHandInsertionSlot,
  computeHandInsertionMarker,
  computeFlankDisplacement,
  computeGapPx,
  flankingHandIndices,
  VISIBLE_GAP_FRACTION,
} from "../handInsertionSlot.ts";

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
  it("centers the marker in the gap between the two flanking cards (drag-excluded space)", () => {
    // dragging id 2 -> remaining [card1(left0,w100,right100), card3(left160)];
    // slot 1 -> gap between card1 and card3 -> midpoint of 100 and 160 = 130.
    expect(computeHandInsertionMarker(markerRects, 1, 2)).toEqual({ x: 130, top: 10, height: 140 });
  });

  it("places the marker at the leading edge of the first remaining card for slot 0", () => {
    expect(computeHandInsertionMarker(markerRects, 0, 2)).toEqual({ x: 0, top: 10, height: 140 });
  });

  it("places the marker after the last remaining card for the append slot", () => {
    // dragging id 2 -> remaining [card1, card3] (len 2); slot 2 -> append = card3.left + card3.width = 260.
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

describe("computeGapPx", () => {
  it("opens a visible gap of exactly 2/3 the card width on top of the resting edge overlap", () => {
    // cardWidth 150, the two flanking cards overlap by 60px at rest. The total
    // displacement must cover the overlap AND open 2/3*150 = 100px of clear space.
    expect(computeGapPx(150, 60)).toBe(160);
  });

  it("equals just the visible gap when the cards do not overlap at rest", () => {
    expect(computeGapPx(150, 0)).toBe(100);
  });

  it("guarantees the post-displacement visible gap is 2/3 card width for any overlap", () => {
    // Rigid two-block model separates the flanking pair by exactly gapPx, so the
    // visible gap after sliding = gapPx - edgeOverlap. This must always be 2/3*w.
    for (const [w, overlap] of [[120, 30], [200, 170], [96, 81.6]] as const) {
      expect(computeGapPx(w, overlap) - overlap).toBeCloseTo(VISIBLE_GAP_FRACTION * w);
    }
  });

  it("exposes 2/3 as the visible-gap fraction", () => {
    expect(VISIBLE_GAP_FRACTION).toBeCloseTo(2 / 3);
  });
});

describe("computeFlankDisplacement", () => {
  it("returns 0 for every card when no insertion slot is active", () => {
    expect(computeFlankDisplacement(0, -1, 2, 32)).toBe(0);
    expect(computeFlankDisplacement(3, -1, 2, 32)).toBe(0);
  });

  it("returns 0 for the dragged card itself", () => {
    expect(computeFlankDisplacement(2, 1, 2, 32)).toBe(0);
  });

  it("shifts cards left of the boundary by -gap/2 and right by +gap/2 (rigid blocks)", () => {
    // handSize 5, dragging index 2, slot 2 -> remaining indices [0,1,(3->2),(4->3)],
    // boundary at remaining slot 2: handObjects 0,1 are left; 3,4 are right.
    expect(computeFlankDisplacement(0, 2, 2, 32)).toBe(-16);
    expect(computeFlankDisplacement(1, 2, 2, 32)).toBe(-16);
    expect(computeFlankDisplacement(3, 2, 2, 32)).toBe(16);
    expect(computeFlankDisplacement(4, 2, 2, 32)).toBe(16);
  });

  it("honors a custom gap width", () => {
    expect(computeFlankDisplacement(0, 1, 2, 40)).toBe(-20);
  });
});

describe("flankingHandIndices", () => {
  it("maps an interior slot to the two handObjects indices it sits between", () => {
    // handSize 5, dragging index 2, slot 2 -> remaining[1]=hand1, remaining[2]=hand3.
    expect(flankingHandIndices(2, 2, 5)).toEqual({ left: 1, right: 3 });
  });

  it("returns a null left at slot 0 (before all cards)", () => {
    expect(flankingHandIndices(0, 2, 5)).toEqual({ left: null, right: 0 });
  });

  it("returns a null right at the append slot", () => {
    expect(flankingHandIndices(4, 2, 5)).toEqual({ left: 4, right: null });
  });

  it("accounts for the dragged card shifting the remaining->handObjects mapping", () => {
    // dragging index 0 -> remaining are handObjects 1..4; remaining[1]=hand2, remaining[2]=hand3.
    expect(flankingHandIndices(2, 0, 5)).toEqual({ left: 2, right: 3 });
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
