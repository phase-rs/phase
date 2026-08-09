import { describe, expect, it } from "vitest";

import type { GameLogEntry } from "../../adapter/types";
import { filterLogEntries, segmentsToPlainText, uniqueTurns } from "../logSearch";

function entry(
  category: GameLogEntry["category"],
  text: string,
  turn = 1,
): GameLogEntry {
  return {
    seq: turn,
    turn,
    phase: "PreCombatMain",
    category,
    segments: [{ type: "Text", value: text }],
  };
}

describe("logSearch", () => {
  it("filters by query and category", () => {
    const entries = [
      entry("Combat", "deals damage"),
      entry("Life", "gains life"),
    ];
    const result = filterLogEntries(entries, {
      query: "damage",
      categories: new Set(["Combat"]),
      turn: null,
    });
    expect(result).toHaveLength(1);
    expect(segmentsToPlainText(result[0].segments)).toContain("damage");
  });

  it("does not expose pregame turn zero as a turn filter", () => {
    const turns = uniqueTurns([entry("Game", "setup", 0), entry("Stack", "cast", 1)]);
    expect(turns).toEqual([1]);
  });
});
