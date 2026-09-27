import { describe, expect, it } from "vitest";

import type { CardMotionTarget } from "../../../stores/animationStore.ts";
import {
  cardFlightControl,
  projectedCardMotionTarget,
  stackCardMotionTarget,
} from "../cardMotion.ts";

describe("card motion continuity", () => {
  it("preserves projected card scale and orientation in its DOM pose", () => {
    const target = projectedCardMotionTarget({
      center: { x: 400, y: 300 },
      left: { x: 350, y: 300 },
      right: { x: 450, y: 300 },
      top: { x: 410, y: 230 },
      bottom: { x: 390, y: 370 },
    });

    expect(target.rect.x).toBe(350);
    expect(target.rect.width).toBe(100);
    expect(target.rect.height).toBeCloseTo(141.42, 2);
    expect(target.rotation).toBeCloseTo(8.13, 2);
  });

  it("moves the predicted stack landing pose to the selected dock", () => {
    const left = stackCardMotionTarget(
      { width: 1440, height: 900 },
      "left",
    );
    const right = stackCardMotionTarget(
      { width: 1440, height: 900 },
      "right",
    );

    expect(left.rect.width).toBe(right.rect.width);
    expect(left.rect.x).toBeLessThan(right.rect.x);
    expect(
      left.rect.x + left.rect.width / 2,
    ).toBeCloseTo(
      1440 - (right.rect.x + right.rect.width / 2),
    );
  });

  it("carries release momentum into the flight control point", () => {
    const from: CardMotionTarget = {
      rect: new DOMRect(200, 700, 120, 168),
      rotation: -4,
    };
    const to: CardMotionTarget = {
      rect: new DOMRect(1000, 220, 150, 210),
      rotation: 0,
    };
    const neutral = cardFlightControl(from, to, { x: 0, y: 0 });
    const thrownRight = cardFlightControl(
      from,
      to,
      { x: 900, y: -600 },
    );

    expect(thrownRight.x).toBeGreaterThan(neutral.x);
    expect(thrownRight.y).toBeLessThan(neutral.y);
    expect(thrownRight.rotation).toBeGreaterThan(neutral.rotation);
  });
});
