import { describe, expect, it } from "vitest";

import type { TabletopPlacement, TabletopSeatAssignment } from "../tabletopLayout.ts";
import {
  tabletopAttackStrikeTargets,
  tabletopAttackStrikeTransform,
} from "../tabletopAttackAnimation.ts";

describe("tabletopAttackStrikeTransform", () => {
  it("lifts, strikes toward the defender, and returns exactly home", () => {
    const origin = [0, 1] as const;
    const target = [0, -4.75] as const;
    const start = tabletopAttackStrikeTransform(0, origin, target);
    const impact = tabletopAttackStrikeTransform(0.58, origin, target);
    const end = tabletopAttackStrikeTransform(1, origin, target);

    expect(start).toEqual({ offsetX: -0, offsetZ: 0, lift: 0, scale: 1 });
    expect(impact.offsetZ).toBeLessThan(-4);
    expect(impact.lift).toBeGreaterThan(0.5);
    expect(impact.scale).toBeGreaterThan(1);
    expect(end.offsetX).toBeCloseTo(0);
    expect(end.offsetZ).toBeCloseTo(0);
    expect(end.lift).toBeCloseTo(0);
    expect(end.scale).toBeCloseTo(1);
  });

  it("winds away from the target before surging forward", () => {
    const windup = tabletopAttackStrikeTransform(0.22, [1, 0], [5, 0]);
    const surge = tabletopAttackStrikeTransform(0.48, [1, 0], [5, 0]);

    expect(windup.offsetX).toBeLessThan(0);
    expect(windup.lift).toBeGreaterThan(0.5);
    expect(surge.offsetX).toBeGreaterThan(0);
    expect(surge.lift).toBeGreaterThan(windup.lift);
  });
});

describe("tabletopAttackStrikeTargets", () => {
  const placements: TabletopPlacement[] = [
    {
      objectId: 10,
      pileCount: 1,
      lane: "creatures",
      position: [0, 0.16, 1],
      faceAngle: 0,
      attackVector: [0, -1],
      cardScale: 1,
    },
    {
      objectId: 42,
      pileCount: 1,
      lane: "support",
      position: [2.4, 0.16, -4.2],
      faceAngle: Math.PI,
      attackVector: [0, 1],
      cardScale: 1,
    },
  ];
  const seats: TabletopSeatAssignment[] = [{ playerId: 1, seat: "far" }];

  it("aims player attacks at the engine-authored defending seat", () => {
    const targets = tabletopAttackStrikeTargets(
      [{
        object_id: 10,
        defending_player: 1,
        attack_target: { type: "Player", data: 1 },
      }],
      placements,
      0,
      seats,
      "duel",
    );

    expect(targets.get(10)).toEqual({
      key: "Player:1",
      x: 0,
      z: -4.75,
    });
  });

  it("aims planeswalker attacks at that permanent instead of the seat edge", () => {
    const targets = tabletopAttackStrikeTargets(
      [{
        object_id: 10,
        defending_player: 1,
        attack_target: { type: "Planeswalker", data: 42 },
      }],
      placements,
      0,
      seats,
      "duel",
    );

    expect(targets.get(10)).toEqual({
      key: "Planeswalker:42",
      x: 2.4,
      z: -4.2,
    });
  });
});
