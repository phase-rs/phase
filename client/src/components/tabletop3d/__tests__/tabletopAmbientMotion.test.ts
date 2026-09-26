import { describe, expect, it } from "vitest";

import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  TABLETOP_FLYING_BOB_AMPLITUDE,
  TABLETOP_FLYING_BOB_PERIOD_SECONDS,
  TABLETOP_FLYING_HOVER_LIFT,
  tabletopFlyingBobOffset,
  isFlyingCreature,
} from "../tabletopAmbientMotion.ts";

describe("tabletop ambient motion", () => {
  it("floats only creatures that currently have Flying", () => {
    const flyer = gameObjectFactory.creature().withKeywords("Flying").build();
    const groundedCreature = gameObjectFactory.creature().build();
    const flyingArtifact = gameObjectFactory.artifact().withKeywords("Flying").build();

    expect(isFlyingCreature(flyer)).toBe(true);
    expect(isFlyingCreature(groundedCreature)).toBe(false);
    expect(isFlyingCreature(flyingArtifact)).toBe(false);
  });

  it("bobs above the tabletop, loops cleanly, and honors disabled animation", () => {
    const start = tabletopFlyingBobOffset(0, 17, 1);
    const looped = tabletopFlyingBobOffset(TABLETOP_FLYING_BOB_PERIOD_SECONDS, 17, 1);
    const samples = Array.from({ length: 128 }, (_, index) =>
      tabletopFlyingBobOffset(
        TABLETOP_FLYING_BOB_PERIOD_SECONDS * index / 128,
        17,
        1,
      )
    );

    expect(looped).toBeCloseTo(start);
    expect(Math.min(...samples)).toBeGreaterThanOrEqual(
      TABLETOP_FLYING_HOVER_LIFT - TABLETOP_FLYING_BOB_AMPLITUDE,
    );
    expect(Math.min(...samples)).toBeGreaterThan(0);
    expect(Math.max(...samples)).toBeLessThanOrEqual(
      TABLETOP_FLYING_HOVER_LIFT + TABLETOP_FLYING_BOB_AMPLITUDE,
    );
    expect(tabletopFlyingBobOffset(1, 17, 0)).toBe(0);
  });
});
