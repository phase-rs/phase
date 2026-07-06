import { describe, expect, it } from "vitest";

import { ratioFromPointerX, resizeBand } from "../gridBandMath.ts";

const VH = 1000; // round viewport height for easy arithmetic

describe("resizeBand", () => {
  it("grows the band by the drag delta and keeps pct/pxCap in agreement", () => {
    // Default top band: min(12% of 1000 = 120, cap 100) => 100px effective.
    const next = resizeBand({ pct: 12, pxCap: 100 }, 50, VH);
    expect(next.pxCap).toBe(150); // 100 + 50
    expect(next.pct).toBeCloseTo(15, 5); // 150 / 1000 * 100
  });

  it("shrinks the band on a negative delta", () => {
    const next = resizeBand({ pct: 18, pxCap: 150 }, -50, VH);
    expect(next.pxCap).toBe(100); // 150 - 50
    expect(next.pct).toBeCloseTo(10, 5);
  });

  it("never shrinks a band below the minimum pixel floor", () => {
    const next = resizeBand({ pct: 12, pxCap: 100 }, -500, VH);
    expect(next.pxCap).toBe(40); // MIN_PX floor
  });

  it("never grows a band past 40% of the viewport (battlefield protection)", () => {
    const next = resizeBand({ pct: 18, pxCap: 150 }, 5000, VH);
    expect(next.pxCap).toBe(400); // MAX_FRACTION * VH
    expect(next.pct).toBeCloseTo(35, 5); // clamped to MAX_PCT
  });

  it("clamps the percentage to its ceiling even when px would allow more", () => {
    // On a very tall viewport, 40% px would imply >35% — pct must still cap.
    const next = resizeBand({ pct: 18, pxCap: 150 }, 100, VH);
    expect(next.pct).toBeLessThanOrEqual(35);
  });

  it("magnetically snaps back to the default band when released near it", () => {
    const home = { pct: 18, pxCap: 150 }; // default bottom band (150px @ VH=1000)
    // Drag a custom 200px band down by ~8px → lands within 12px of home (150).
    const next = resizeBand({ pct: 20, pxCap: 200 }, -42, VH, home);
    expect(next).toEqual(home); // restored verbatim
  });

  it("does not snap when released outside the snap window", () => {
    const home = { pct: 18, pxCap: 150 };
    const next = resizeBand({ pct: 20, pxCap: 200 }, -20, VH, home); // 180px, 30px off
    expect(next.pxCap).toBe(180);
  });

  it("is a no-op on a degenerate (zero/negative/NaN) viewport, never persisting a NaN track", () => {
    // window.innerHeight can be 0 / NaN transiently (collapsed or pre-layout
    // container); the result is written to the persisted preferences store, so a
    // NaN would serialize to null and corrupt the stored track permanently.
    const track = { pct: 12, pxCap: 100 };
    expect(resizeBand(track, 50, 0)).toEqual(track);
    expect(Number.isNaN(resizeBand(track, 50, 0).pct)).toBe(false);
    expect(resizeBand(track, 50, Number.NaN)).toEqual(track);
    // A negative viewport would otherwise yield a negative pxCap.
    expect(resizeBand(track, 50, -100).pxCap).toBeGreaterThan(0);
  });
});

describe("ratioFromPointerX", () => {
  it("returns lands' share from the pointer position in the region", () => {
    // Region [200, 1000]: a pointer at 600 is exactly halfway.
    expect(ratioFromPointerX(600, 200, 1000)).toBeCloseTo(0.5, 5);
    // Three-quarters across ⇒ lands take 0.75 (support 0.25).
    expect(ratioFromPointerX(800, 200, 1000)).toBeCloseTo(0.75, 5);
  });

  it("is drift-free (depends only on absolute position, not a delta)", () => {
    // Same pointer X always yields the same ratio regardless of call history.
    expect(ratioFromPointerX(450, 200, 1000)).toEqual(ratioFromPointerX(450, 200, 1000));
  });

  it("falls back to an even split for a degenerate region", () => {
    expect(ratioFromPointerX(500, 600, 600)).toBe(0.5);
    expect(ratioFromPointerX(500, 700, 600)).toBe(0.5);
  });
});
