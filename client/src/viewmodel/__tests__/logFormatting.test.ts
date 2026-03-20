import { describe, expect, it } from "vitest";

import type { GameLogEntry } from "../../adapter/types";
import { categoryColorClass, filterLogByVerbosity } from "../logFormatting";

function makeEntry(category: GameLogEntry["category"], segments: GameLogEntry["segments"] = [{ type: "Text", value: "test" }]): GameLogEntry {
  return { seq: 0, turn: 1, phase: "PreCombatMain", category, segments };
}

describe("categoryColorClass", () => {
  it("classifies combat as red", () => {
    expect(categoryColorClass(makeEntry("Combat"))).toBe("text-red-400");
  });

  it("classifies destroy as red", () => {
    expect(categoryColorClass(makeEntry("Destroy"))).toBe("text-red-400");
  });

  it("classifies stack as blue", () => {
    expect(categoryColorClass(makeEntry("Stack"))).toBe("text-blue-400");
  });

  it("classifies life gain as green", () => {
    const entry = makeEntry("Life", [
      { type: "Text", value: "Player 1" },
      { type: "Text", value: " gains " },
      { type: "Number", value: 3 },
      { type: "Text", value: " life" },
    ]);
    expect(categoryColorClass(entry)).toBe("text-green-400");
  });

  it("classifies life loss as red", () => {
    const entry = makeEntry("Life", [
      { type: "Text", value: "Player 1" },
      { type: "Text", value: " loses " },
      { type: "Number", value: 3 },
      { type: "Text", value: " life" },
    ]);
    expect(categoryColorClass(entry)).toBe("text-red-400");
  });

  it("classifies special as amber", () => {
    expect(categoryColorClass(makeEntry("Special"))).toBe("text-amber-400");
  });

  it("defaults to gray for other categories", () => {
    expect(categoryColorClass(makeEntry("Zone"))).toBe("text-gray-400");
    expect(categoryColorClass(makeEntry("Turn"))).toBe("text-gray-400");
    expect(categoryColorClass(makeEntry("Mana"))).toBe("text-gray-400");
  });
});

describe("filterLogByVerbosity", () => {
  // ZoneChanged entry has Zone segments (e.g., "X moves from Library to Hand")
  const zoneChangedEntry: GameLogEntry = {
    ...makeEntry("Zone"),
    segments: [
      { type: "CardName", value: { name: "Forest", object_id: 1 } },
      { type: "Text", value: " moves from " },
      { type: "Zone", value: "Library" },
      { type: "Text", value: " to " },
      { type: "Zone", value: "Hand" },
    ],
  };

  // Non-ZoneChanged Zone entry (e.g., "Player 1 plays Forest")
  const landPlayedEntry: GameLogEntry = {
    ...makeEntry("Zone"),
    segments: [
      { type: "PlayerName", value: { name: "Player 1", player_id: 0 } },
      { type: "Text", value: " plays " },
      { type: "CardName", value: { name: "Forest", object_id: 1 } },
    ],
  };

  const entries: GameLogEntry[] = [
    makeEntry("Game"),       // Game started
    { ...makeEntry("Turn"), segments: [{ type: "Text", value: "Turn " }, { type: "Number", value: 1 }] },
    makeEntry("Turn"),       // PriorityPassed (Turn category, no "Turn " prefix)
    makeEntry("Mana"),       // ManaAdded
    makeEntry("Stack"),      // SpellCast
    makeEntry("State"),      // PermanentTapped
    makeEntry("State"),      // DamageCleared
    zoneChangedEntry,        // ZoneChanged (has Zone segments)
    landPlayedEntry,         // LandPlayed (Zone category, no Zone segments)
    makeEntry("Life"),       // DamageDealt
    makeEntry("Life"),       // LifeChanged
    makeEntry("Combat"),     // AttackersDeclared
    makeEntry("Combat"),     // BlockersDeclared
    makeEntry("Destroy"),    // CreatureDestroyed
    makeEntry("Token"),      // TokenCreated
    makeEntry("Game"),       // GameOver
  ];

  it("full returns all entries", () => {
    expect(filterLogByVerbosity(entries, "full")).toHaveLength(entries.length);
  });

  it("compact excludes Mana, State, non-TurnStarted Turn, and ZoneChanged entries", () => {
    const compact = filterLogByVerbosity(entries, "compact");
    const categories = compact.map((e) => e.category);

    expect(categories).not.toContain("Mana");
    expect(categories).not.toContain("State");
    // The TurnStarted entry (with "Turn " prefix) is kept, PriorityPassed is excluded
    const turnEntries = compact.filter((e) => e.category === "Turn");
    expect(turnEntries).toHaveLength(1);
    // ZoneChanged is excluded, but LandPlayed is kept
    const zoneEntries = compact.filter((e) => e.category === "Zone");
    expect(zoneEntries).toHaveLength(1); // only LandPlayed
    expect(compact).toHaveLength(entries.length - 5); // -1 Mana, -2 State, -1 Turn(priority), -1 ZoneChanged
  });

  it("minimal only includes Game, Stack, Combat, Life, Destroy, Token", () => {
    const minimal = filterLogByVerbosity(entries, "minimal");
    const categories = minimal.map((e) => e.category);

    for (const cat of categories) {
      expect(["Game", "Stack", "Combat", "Life", "Destroy", "Token"]).toContain(cat);
    }
    expect(minimal).toHaveLength(9); // 2 Game + 1 Stack + 2 Life + 2 Combat + 1 Destroy + 1 Token
  });
});
