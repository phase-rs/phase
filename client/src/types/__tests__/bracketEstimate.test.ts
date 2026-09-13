import { describe, expect, it } from "vitest";

import { isBracketEstimate } from "../bracketEstimate";

function estimate() {
  return {
    tier: "upgraded",
    axes: {
      game_changers: { count: 1, cap_at_tier: 3, contributing: ["Smothering Tithe"] },
      mass_land_denial: { count: 0, cap_at_tier: 0, contributing: [] },
      extra_turns: { count: 0, cap_at_tier: null, contributing: [] },
      efficient_tutors: { count: 0, cap_at_tier: null, contributing: [] },
    },
    violations: {},
    data_version: "test-1",
  };
}

describe("isBracketEstimate", () => {
  it("accepts the complete nested reading map including an uncapped axis", () => {
    expect(isBracketEstimate(estimate())).toBe(true);
  });

  it("rejects a missing axis", () => {
    const value = estimate();
    delete (value.axes as Partial<typeof value.axes>).mass_land_denial;
    expect(isBracketEstimate(value)).toBe(false);
  });

  it("rejects a non-numeric cap", () => {
    const value = estimate();
    (value.axes.game_changers as { cap_at_tier: unknown }).cap_at_tier = "3";
    expect(isBracketEstimate(value)).toBe(false);
  });

  it("rejects a non-string contributing card", () => {
    const value = estimate();
    (value.axes.game_changers as { contributing: unknown[] }).contributing = [1];
    expect(isBracketEstimate(value)).toBe(false);
  });
});
