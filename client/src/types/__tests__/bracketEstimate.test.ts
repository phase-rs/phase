import { describe, expect, it } from "vitest";

import { isBracketEstimate, isComboDeclaration } from "../bracketEstimate";

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
    declaration: null,
    combo_barometer: { declaration: { kind: "undeclared" }, floor: null },
    barometers: {
      game_changers: "engine",
      extra_turns: "engine",
      mass_land_denial: "engine",
      two_card_combos: "unanswered",
    },
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

  it("accepts a null declaration", () => {
    expect(isBracketEstimate({ ...estimate(), declaration: null })).toBe(true);
  });

  it("accepts a valid below-floor declaration", () => {
    expect(
      isBracketEstimate({
        ...estimate(),
        declaration: {
          kind: "below_floor",
          floor: "optimized",
          raised_by: ["mass_land_denial"],
        },
      }),
    ).toBe(true);
  });

  it("accepts a payload with the declaration field absent", () => {
    const value = estimate();
    delete (value as Partial<typeof value>).declaration;
    expect(isBracketEstimate(value)).toBe(true);
  });

  it("rejects an unknown declaration kind", () => {
    expect(
      isBracketEstimate({ ...estimate(), declaration: { kind: "nonsense" } }),
    ).toBe(false);
  });

  it("accepts the combo barometer fields and their absence", () => {
    expect(isBracketEstimate(estimate())).toBe(true);
    const legacy = estimate();
    delete (legacy as Partial<typeof legacy>).combo_barometer;
    delete (legacy as Partial<typeof legacy>).barometers;
    expect(isBracketEstimate(legacy)).toBe(true);
  });

  it("rejects an invalid barometer authority", () => {
    expect(
      isBracketEstimate({
        ...estimate(),
        barometers: { two_card_combos: "guessed" },
      }),
    ).toBe(false);
  });

  it("rejects an invalid intended combo window", () => {
    expect(isComboDeclaration({ kind: "intended", window: "soon" })).toBe(false);
  });
});
