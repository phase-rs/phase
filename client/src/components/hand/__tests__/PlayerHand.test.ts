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
  // centers: card1 (50,80), card2 (130,70), card3 (210,80)
  { objectId: 1, left: 0, width: 100, top: 10, height: 140 },
  { objectId: 2, left: 80, width: 100, top: 0, height: 140 },
  { objectId: 3, left: 160, width: 100, top: 10, height: 140 },
];

describe("computeHandInsertionMarker", () => {
  it("returns the midpoint of the two flanking cards' CENTERS for an interior slot", () => {
    // dragging id 2 -> remaining [card1 center (50,80), card3 center (210,80)];
    // slot 1 -> midpoint of the centers = (130, 80). Tilt-proof: centers, not edges.
    expect(computeHandInsertionMarker(markerRects, 1, 2)).toEqual({ x: 130, y: 80 });
  });

  it("extrapolates half a step BEFORE the first card's center for slot 0", () => {
    // remaining centers c0 (50,80), c1 (210,80); step = (160,0);
    // slot 0 -> c0 - step/2 = (50-80, 80-0) = (-30, 80). Follows the fan's spacing/arc.
    expect(computeHandInsertionMarker(markerRects, 0, 2)).toEqual({ x: -30, y: 80 });
  });

  it("extrapolates half a step AFTER the last card's center for the append slot", () => {
    // remaining centers c0 (50,80), cLast (210,80); step = (160,0);
    // append -> cLast + step/2 = (210+80, 80) = (290, 80).
    expect(computeHandInsertionMarker(markerRects, 2, 2)).toEqual({ x: 290, y: 80 });
  });

  it("carries the fan's vertical arc into the extrapolated edge point", () => {
    // Drag id 1: remaining c0=card2 (130,70), c1=card3 (210,80); step=(80,10).
    // append -> cLast(210,80) + step/2 (40,5) = (250, 85): the arc tilts the point down.
    expect(computeHandInsertionMarker(markerRects, 2, 1)).toEqual({ x: 250, y: 85 });
  });

  it("clamps an out-of-range slot to the append position", () => {
    expect(computeHandInsertionMarker(markerRects, 99, 2)).toEqual({ x: 290, y: 80 });
  });

  it("returns null when no cards remain after excluding the dragged card", () => {
    expect(
      computeHandInsertionMarker([{ objectId: 5, left: 0, width: 100, top: 0, height: 10 }], 0, 5),
    ).toBeNull();
  });

  it("returns the lone remaining card's center (no neighbor to extrapolate from)", () => {
    expect(computeHandInsertionMarker([{ objectId: 1, left: 40, width: 100 }, { objectId: 9, left: 0, width: 100 }], 0, 9))
      .toEqual({ x: 90, y: 0 });
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
