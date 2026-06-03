import { describe, expect, it } from "vitest";

import type { GameLogEntry } from "../../adapter/types";
import { filterLogEntries, segmentsToPlainText } from "../logSearch";

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
});
