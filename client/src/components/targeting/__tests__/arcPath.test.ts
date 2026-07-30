import { describe, expect, it } from "vitest";
import { getArcPath } from "../arcPath";

describe("getArcPath", () => {
  it("draws a quadratic arc between two distinct points with a finite control point", () => {
    const d = getArcPath({ x: 0, y: 0 }, { x: 100, y: 0 });
    expect(d.startsWith("M 0 0 Q ")).toBe(true);
    expect(d.endsWith("100 0")).toBe(true);
    // Every coordinate in the path must be a finite number (no NaN/Infinity).
    for (const n of d.match(/-?\d+(?:\.\d+)?|NaN|Infinity/g) ?? []) {
      expect(Number.isFinite(Number(n))).toBe(true);
    }
  });

  it("does not emit NaN when the endpoints are coincident (dist === 0)", () => {
    // A self-target or two anchors overlapping mid-layout gives from === to. The
    // perpendicular unit vector -dy/dist is 0/0 = NaN without a guard, which would
    // produce an invalid SVG `d` like "M 100 100 Q NaN NaN 100 100".
    const d = getArcPath({ x: 100, y: 100 }, { x: 100, y: 100 });
    expect(d).not.toContain("NaN");
    expect(d).toBe("M 100 100 L 100 100");
  });
});
