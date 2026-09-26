import { describe, expect, it } from "vitest";

import { isBracketEstimate } from "../bracketEstimate";

function estimate() {
  return {
    tier: "upgraded",
    axes: {
      game_changers: { count: 1, contributing: ["Smothering Tithe"] },
      mass_land_denial: { count: 0, contributing: [] },
      extra_turns: { count: 0, contributing: [] },
      efficient_tutors: { count: 0, contributing: [] },
    },
    checks: [
      {
        axis: "game_changers",
        comparator: "GE",
        threshold: 1,
        floor: "upgraded",
        observed: 1,
        outcome: { kind: "fired" },
        official_line: "Bracket 1 and 2 decks exclude Game Changers.",
        source_document: "MTG Commander Format — Game Changers",
        source_published: "2026-02-09",
        source_url: "https://magic.wizards.com/en/formats/commander",
        evidence: ["Smothering Tithe"],
      },
    ],
    coverage: { counted: 2, resolved: 2, unresolved: [], confidence: "complete" },
    data_version: "test-1",
  };
}

describe("isBracketEstimate", () => {
  it("accepts the new shape and rejects the old", () => {
    const current = estimate();
    expect(isBracketEstimate(current)).toBe(true);
    expect(
      isBracketEstimate({
        tier: "upgraded",
        axes: {
          game_changers: { count: 1, cap_at_tier: 3, contributing: ["Smothering Tithe"] },
          mass_land_denial: { count: 0, cap_at_tier: 0, contributing: [] },
          extra_turns: { count: 0, cap_at_tier: null, contributing: [] },
          efficient_tutors: { count: 0, cap_at_tier: null, contributing: [] },
        },
        violations: {},
        data_version: "test-1",
      }),
    ).toBe(false);
  });

  it("rejects a missing axis", () => {
    const value = estimate();
    delete (value.axes as Partial<typeof value.axes>).mass_land_denial;
    expect(isBracketEstimate(value)).toBe(false);
  });

  it("rejects an invalid check outcome", () => {
    const value = estimate();
    (value.checks[0] as { outcome: unknown }).outcome = { kind: "unknown" };
    expect(isBracketEstimate(value)).toBe(false);
  });

  it("rejects a non-string contributing card", () => {
    const value = estimate();
    (value.axes.game_changers as { contributing: unknown[] }).contributing = [1];
    expect(isBracketEstimate(value)).toBe(false);
  });
});
