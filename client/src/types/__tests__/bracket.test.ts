import { describe, expect, it } from "vitest";

import {
  BRACKET_LABEL,
  BRACKET_NUMERIC_TIER,
  BRACKET_TIER_NUMERIC,
  COMMANDER_BRACKETS,
  effectiveBracket,
  isBracketMismatch,
  type CommanderBracket,
} from "../bracket";

describe("CommanderBracket constants", () => {
  it("COMMANDER_BRACKETS lists 1..5 in order", () => {
    expect(COMMANDER_BRACKETS).toEqual([1, 2, 3, 4, 5]);
  });

  it("BRACKET_LABEL covers every bracket", () => {
    for (const b of COMMANDER_BRACKETS) {
      expect(BRACKET_LABEL[b]).toEqual(expect.stringMatching(/.+/));
    }
  });

  it("BRACKET_LABEL uses the WotC names", () => {
    const expected: Record<CommanderBracket, string> = {
      1: "Exhibition",
      2: "Core",
      3: "Upgraded",
      4: "Optimized",
      5: "cEDH",
    };
    expect(BRACKET_LABEL).toEqual(expected);
  });
});

describe("BRACKET_NUMERIC_TIER", () => {
  it("is the exact inverse of BRACKET_TIER_NUMERIC", () => {
    for (const b of COMMANDER_BRACKETS) {
      expect(BRACKET_TIER_NUMERIC[BRACKET_NUMERIC_TIER[b]]).toBe(b);
    }
  });
});

describe("effectiveBracket", () => {
  it("returns the manual tag when one exists, even above the estimate", () => {
    // A 0-Game Changer deck the pilot calls B3: the pilot's word is final.
    expect(effectiveBracket(3, "core")).toBe(3);
    // A B4 estimate the pilot calls cEDH.
    expect(effectiveBracket(5, "optimized")).toBe(5);
  });

  it("returns the manual tag as-is even below the estimate", () => {
    // Under-reporting is flagged by isBracketMismatch, but the declared tag
    // remains the effective value.
    expect(effectiveBracket(2, "optimized")).toBe(2);
  });

  it("falls back to the estimate for untagged decks", () => {
    expect(effectiveBracket(null, "upgraded")).toBe(3);
    expect(effectiveBracket(null, "core")).toBe(2);
  });

  it("returns null when there is neither a tag nor an estimate", () => {
    expect(effectiveBracket(null, null)).toBeNull();
  });
});

describe("isBracketMismatch", () => {
  it("flags a manual tag below the estimate (under-reporting)", () => {
    expect(isBracketMismatch(2, "upgraded")).toBe(true);
    expect(isBracketMismatch(3, "optimized")).toBe(true);
    expect(isBracketMismatch(1, "upgraded")).toBe(true);
  });

  it("allows declaring at or above the estimate", () => {
    expect(isBracketMismatch(3, "upgraded")).toBe(false);
    expect(isBracketMismatch(4, "upgraded")).toBe(false);
    expect(isBracketMismatch(5, "optimized")).toBe(false);
  });

  it("allows B1 Exhibition on a B2-floor estimate", () => {
    // The estimator never returns B1, so a B1 tag on a Core estimate is a
    // valid self-declaration, not a mismatch.
    expect(isBracketMismatch(1, "core")).toBe(false);
  });

  it("never mismatches an untagged deck", () => {
    expect(isBracketMismatch(null, "optimized")).toBe(false);
  });
});
