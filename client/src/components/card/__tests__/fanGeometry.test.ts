import { describe, expect, it } from "vitest";

import { fanGeometry, spreadFactor } from "../fanGeometry.ts";

describe("fanGeometry profiles", () => {
  it("gives a normal desktop hand substantially more horizontal room", () => {
    expect(spreadFactor(8, "wide")).toBeCloseTo(5.5);
    expect(spreadFactor(8, "wide")).toBeGreaterThan(spreadFactor(8, "compact"));
  });

  it("holds the wide profile near its target width as Commander hands grow", () => {
    expect(spreadFactor(12, "wide")).toBeCloseTo(5.5);
    expect(spreadFactor(20, "wide")).toBeCloseTo(5.5);
  });

  it("uses a flatter curve and gentler tilt than the compact profile", () => {
    const compact = fanGeometry(8, "--hand-card-w", "compact");
    const wide = fanGeometry(8, "--hand-card-w", "wide");

    expect(wide.rotation(0)).toBeCloseTo(-12);
    expect(wide.rotation(7)).toBeCloseTo(12);
    expect(wide.arc(0)).toBeCloseTo(32);
    expect(Math.abs(wide.rotation(0))).toBeLessThan(Math.abs(compact.rotation(0)));
    expect(wide.arc(0)).toBeLessThan(compact.arc(0));
  });

  it("keeps compact geometry as the default for constrained surfaces", () => {
    expect(fanGeometry(8).overlap).toBe(
      fanGeometry(8, "--hand-card-w", "compact").overlap,
    );
    expect(spreadFactor(8)).toBe(spreadFactor(8, "compact"));
  });
});
