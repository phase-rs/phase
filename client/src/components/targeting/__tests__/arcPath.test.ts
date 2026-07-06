import { describe, expect, it } from "vitest";

import { getArcPath } from "../arcPath";

describe("getArcPath", () => {
  it("produces a valid path for a zero-length arrow (from === to)", () => {
    // When the source and target anchors coincide, `dist` is 0; without a guard
    // the perpendicular normal is `0/0 = NaN` and the path is unrenderable.
    const path = getArcPath({ x: 100, y: 100 }, { x: 100, y: 100 });

    expect(path).not.toContain("NaN");
    expect(path).toBe("M 100 100 Q 100 100 100 100");
  });

  it("builds a curved path with finite control points for a normal arrow", () => {
    const path = getArcPath({ x: 0, y: 0 }, { x: 100, y: 0 });

    expect(path).not.toContain("NaN");
    expect(path.startsWith("M 0 0 Q ")).toBe(true);
    expect(path.endsWith(" 100 0")).toBe(true);
    // Every coordinate emitted must be a finite number.
    const numbers = path.match(/-?\d+(\.\d+)?/g)?.map(Number) ?? [];
    expect(numbers.every((n) => Number.isFinite(n))).toBe(true);
  });
});
