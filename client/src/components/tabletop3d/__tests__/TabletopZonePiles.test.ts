import { describe, expect, it } from "vitest";

import {
  TABLETOP_ZONE_PILE_VISUAL_SCALE,
  tabletopZoneLayout,
} from "../tabletopLayout.ts";

describe("tabletopZoneLayout", () => {
  it("renders supporting zone piles slightly smaller than battlefield cards", () => {
    expect(TABLETOP_ZONE_PILE_VISUAL_SCALE).toBe(0.9);
  });

  it("mirrors each player's tabletop zones without entering the battlefield lanes", () => {
    const local = tabletopZoneLayout("local");
    const opponent = tabletopZoneLayout("far");

    expect(local.library).toEqual([-5.55, 0.08, 4.85]);
    expect(local.graveyard[2]).toBe(local.library[2]);
    expect(local.exile[2]).toBe(local.library[2]);
    expect(opponent.library).toEqual([5.55, 0.08, -5.2]);
    expect(opponent.graveyard[2]).toBe(opponent.library[2]);
    expect(opponent.exile[2]).toBe(opponent.library[2]);
    expect(opponent.faceAngle).toBe(Math.PI);
  });

  it("pulls duel piles into mirrored compact rows on narrow screens", () => {
    const local = tabletopZoneLayout("local", "duel", "compact");
    const opponent = tabletopZoneLayout("far", "duel", "compact");

    expect(local.library).toEqual([-1.45, 0.08, 4.85]);
    expect(local.graveyard).toEqual([0, 0.08, 4.85]);
    expect(local.exile).toEqual([1.45, 0.08, 4.85]);
    expect(opponent.library).toEqual([1.45, 0.08, -5.2]);
    expect(opponent.graveyard[0]).toBeCloseTo(0);
    expect(opponent.exile[0]).toBeCloseTo(-1.45);
  });

  it("places the right library at the near corner", () => {
    const right = tabletopZoneLayout("right", "pod");

    expect(right.library[2]).toBeGreaterThan(0);
    expect(right.graveyard[2]).toBeLessThan(right.library[2]);
    expect(right.graveyard[0]).toBeLessThanOrEqual(right.library[0]);
  });

  it("keeps side libraries perpendicular to local and far seats", () => {
    const left = tabletopZoneLayout("left", "pod");
    const right = tabletopZoneLayout("right", "pod");

    expect(left.library[2]).toBeLessThan(0);
    expect(left.library[2]).toBeLessThan(right.library[2]);
    expect(left.faceAngle).toBe(-Math.PI / 2);
    expect(right.faceAngle).toBe(Math.PI / 2);
  });

  it.each(["local", "far", "left"] as const)(
    "places the %s graveyard to that player's right",
    (seat) => {
      const layout = tabletopZoneLayout(seat, "pod");
      const deltaX = layout.graveyard[0] - layout.library[0];
      const deltaZ = layout.graveyard[2] - layout.library[2];
      const playerRelativeRight =
        deltaX * Math.cos(layout.faceAngle)
        - deltaZ * Math.sin(layout.faceAngle);

      expect(playerRelativeRight).toBeCloseTo(1.45);
    },
  );

  it("pushes the opposite player's pile row behind their hand toward the far crop", () => {
    const duel = tabletopZoneLayout("far", "duel");
    const compactDuel = tabletopZoneLayout("far", "duel", "compact");
    const pod = tabletopZoneLayout("far", "pod");

    expect(duel.library[2]).toBe(-5.2);
    expect(duel.graveyard[2]).toBe(-5.2);
    expect(compactDuel.library[2]).toBe(-5.2);
    expect(compactDuel.graveyard[2]).toBe(-5.2);
    expect(pod.library[2]).toBe(-6.15);
    expect(pod.graveyard[2]).toBe(-6.15);
  });

  it("keeps the user's pile row close to the near crop in every layout", () => {
    const duel = tabletopZoneLayout("local", "duel");
    const compactDuel = tabletopZoneLayout("local", "duel", "compact");
    const pod = tabletopZoneLayout("local", "pod");

    expect(duel.library[2]).toBe(4.85);
    expect(duel.graveyard[2]).toBe(4.85);
    expect(compactDuel.library[2]).toBe(4.85);
    expect(compactDuel.graveyard[2]).toBe(4.85);
    expect(pod.library[2]).toBe(4.85);
    expect(pod.graveyard[2]).toBe(4.85);
  });

  it("uses square side-seat orientation for the kitchen-table view", () => {
    expect(tabletopZoneLayout("left", "pod").faceAngle).toBe(
      -Math.PI / 2,
    );
    expect(tabletopZoneLayout("right", "pod").faceAngle).toBe(
      Math.PI / 2,
    );
  });

  it("anchors side libraries toward their respective corners", () => {
    const left = tabletopZoneLayout("left", "pod");
    const right = tabletopZoneLayout("right", "pod");

    expect(left.library[0]).toBeLessThan(-7.5);
    expect(left.library[2]).toBeLessThan(-4.2);
    expect(right.library[0]).toBeGreaterThan(7.5);
    expect(right.library[2]).toBeGreaterThan(2.9);
  });
});
