import { describe, expect, it } from "vitest";

import type { CardPose } from "../cardAnchors.ts";
import { SWEEP_TRAVEL_S, sweepPath } from "../sweepPath.ts";

const pose = (x: number, y: number): CardPose => ({ x, y, w: 63, h: 88, angleDeg: 0 });
const HUD_BELOW = pose(400, 700);

describe("sweepPath", () => {
  it("V15-3: the front heads away from the caster's HUD, reaching the nearer row first and a row's cards together", () => {
    const members = new Map([
      [1, pose(300, 250)],
      [2, pose(500, 250)],
      [3, pose(300, 500)],
      [4, pose(500, 500)],
    ]);
    const path = sweepPath(HUD_BELOW, members);
    const at = new Map(path.hits.map((hit) => [hit.objectId, hit.atS]));
    expect(at.get(3)).toBeCloseTo(at.get(4) ?? NaN);
    expect(at.get(1)).toBeCloseTo(at.get(2) ?? NaN);
    expect(at.get(3)).toBeLessThan(at.get(1) ?? NaN);
    for (const atS of at.values()) {
      expect(atS).toBeGreaterThan(0);
      expect(atS).toBeLessThan(SWEEP_TRAVEL_S);
    }
    // It sets out below the lower row, stops above the upper, and spans both columns.
    expect(path.from[1]).toBeGreaterThan(500 + 44);
    expect(path.to[1]).toBeLessThan(250 - 44);
    expect(path.halfWidth).toBeGreaterThan(100 + 63 / 2);
  });

  it("V15-3: with no HUD to come from, the front travels up the screen", () => {
    const path = sweepPath(null, new Map([[1, pose(300, 200)], [2, pose(300, 450)]]));
    const [upper, lower] = path.hits;
    expect(lower.atS).toBeLessThan(upper.atS);
    expect(path.to[1]).toBeLessThan(path.from[1]);
  });
});
