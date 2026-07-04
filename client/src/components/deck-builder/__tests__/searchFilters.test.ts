import { describe, expect, it } from "vitest";

import type { CardSearchFilters } from "../CardSearch";
import { hasSearchCriteria } from "../searchFilters";

function makeFilters(overrides: Partial<CardSearchFilters> = {}): CardSearchFilters {
  return {
    text: "",
    colors: [],
    type: "",
    cmcMax: undefined,
    sets: [],
    browseFormat: "all",
    ...overrides,
  };
}

describe("hasSearchCriteria", () => {
  it("is false for fully empty filters", () => {
    expect(hasSearchCriteria(makeFilters())).toBe(false);
  });

  it("is true for a real text query", () => {
    expect(hasSearchCriteria(makeFilters({ text: "bolt" }))).toBe(true);
  });

  it("is false for a whitespace-only query (not real criteria)", () => {
    // The text box is stored raw/untrimmed, so a spaces-only value must not
    // register as an active search — otherwise the canvas swaps to (empty)
    // results and a no-op search fires.
    expect(hasSearchCriteria(makeFilters({ text: "   " }))).toBe(false);
    expect(hasSearchCriteria(makeFilters({ text: "\t\n " }))).toBe(false);
  });

  it("still detects a query that has surrounding whitespace", () => {
    expect(hasSearchCriteria(makeFilters({ text: "  bolt  " }))).toBe(true);
  });

  it("detects non-text criteria regardless of text", () => {
    expect(hasSearchCriteria(makeFilters({ colors: ["R"] }))).toBe(true);
    expect(hasSearchCriteria(makeFilters({ type: "Creature" }))).toBe(true);
    expect(hasSearchCriteria(makeFilters({ cmcMax: 0 }))).toBe(true);
    expect(hasSearchCriteria(makeFilters({ sets: ["neo"] }))).toBe(true);
  });
});
