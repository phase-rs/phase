import { describe, expect, it } from "vitest";

import type { GameLogEntry, LogImportance } from "../../adapter/types";
import { filterLogByView, logPresentation, timelineRows, toneClass } from "../logFormatting";

function entry(
  importance: LogImportance,
  overrides: Partial<GameLogEntry> = {},
): GameLogEntry {
  return {
    seq: 1,
    turn: 1,
    phase: "PreCombatMain",
    category: "Stack",
    segments: [{ type: "Text", value: "event" }],
    presentation: { importance, tone: "Neutral", boundary: "None", visibility: "Public" },
    ...overrides,
  };
}

describe("game log presentation", () => {
  it("uses one detail/neutral/non-boundary fallback for legacy entries", () => {
    const legacy = { ...entry("Essential"), presentation: undefined };
    expect(logPresentation(legacy)).toEqual({ importance: "Detail", tone: "Neutral", boundary: "None", visibility: "Public" });
    expect(filterLogByView([legacy], "timeline")).toEqual([]);
    expect(filterLogByView([legacy], "details")).toEqual([legacy]);
    expect(filterLogByView([legacy], "diagnostics")).toEqual([legacy]);
  });

  it("filters typed importance without interpreting English segments", () => {
    const entries = [entry("Essential"), entry("Context", { seq: 2 }), entry("Detail", { seq: 3 }), entry("Diagnostic", { seq: 4, category: "Debug" })];
    expect(filterLogByView(entries, "timeline").map((value) => value.seq)).toEqual([1, 2]);
    expect(filterLogByView(entries, "details").map((value) => value.seq)).toEqual([1, 2, 3]);
    expect(filterLogByView(entries, "diagnostics").map((value) => value.seq)).toEqual([1, 2, 3, 4]);
    expect(filterLogByView(entries, "timeline", new Set(["Debug"]))).toEqual([entries[3]]);
  });

  it("requires an explicit diagnostics opt-in for hidden information", () => {
    const hidden = entry("Detail", {
      presentation: { importance: "Detail", tone: "Neutral", boundary: "None", visibility: "HiddenInformation" },
    });

    expect(filterLogByView([hidden], "diagnostics")).toEqual([]);
    expect(filterLogByView([hidden], "diagnostics", null, true)).toEqual([hidden]);
    expect(filterLogByView([hidden], "details", null, true)).toEqual([]);
  });

  it("coalesces retained boundaries and never creates a turn-zero divider", () => {
    const rows = timelineRows([
      entry("Context", { seq: 1, turn: 1, phase: "Untap", category: "Turn", presentation: { importance: "Context", tone: "Neutral", boundary: "Turn", visibility: "Public" } }),
      entry("Context", { seq: 2, turn: 1, phase: "Upkeep", category: "Turn", presentation: { importance: "Context", tone: "Neutral", boundary: "Phase", visibility: "Public" } }),
      entry("Essential", { seq: 3, turn: 1 }),
      entry("Context", { seq: 4, turn: 0, category: "Turn", presentation: { importance: "Context", tone: "Neutral", boundary: "Turn", visibility: "Public" } }),
      entry("Essential", { seq: 5, turn: 0 }),
      entry("Essential", { seq: 6, turn: 1 }),
    ]);
    expect(rows).toHaveLength(4);
    expect(rows[0]).toMatchObject({ type: "divider", divider: { seq: 2, turn: 1, phase: "Upkeep" } });
    expect(rows.filter((row) => row.type === "divider")).toHaveLength(1);
  });

  it("retains each nonzero standalone boundary in order only for an explicit drill-down", () => {
    const firstBoundary = entry("Context", {
      category: "Turn",
      phase: "Upkeep",
      presentation: { importance: "Context", tone: "Neutral", boundary: "Turn", visibility: "Public" },
    });
    const secondBoundary = entry("Context", {
      seq: 2,
      turn: 2,
      category: "Turn",
      phase: "Draw",
      presentation: { importance: "Context", tone: "Neutral", boundary: "Turn", visibility: "Public" },
    });

    expect(timelineRows([firstBoundary, secondBoundary])).toEqual([]);
    expect(timelineRows([firstBoundary, secondBoundary], true)).toEqual([
      { type: "divider", divider: { seq: 1, turn: 1, phase: "Upkeep", boundary: "Turn" } },
      { type: "divider", divider: { seq: 2, turn: 2, phase: "Draw", boundary: "Turn" } },
    ]);
    expect(timelineRows([{ ...firstBoundary, turn: 0 }], true)).toEqual([]);
  });

  it("uses typed tones for non-color style cues", () => {
    expect(toneClass("Positive")).toContain("border-l-emerald");
    expect(toneClass("Negative")).toContain("border-l-red");
    expect(toneClass("Diagnostic")).toContain("border-l-fuchsia");
  });
});
