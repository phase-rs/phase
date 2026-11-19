import { describe, expect, it } from "vitest";

import {
  DECK_CONSTRUCTION_FORMATS,
  formatMetadata,
  isArchenemyFamilyFormat,
  isPlanechaseFamilyFormat,
} from "../formatRegistry";

describe("isPlanechaseFamilyFormat", () => {
  it.each(["Planechase", "PlanechaseCommander"] as const)("covers %s", (format) => {
    expect(isPlanechaseFamilyFormat(format)).toBe(true);
  });

  it.each(["Commander", "ArchenemyCommander", "TwoHeadedGiantCommander", "Freeform"] as const)(
    "excludes %s",
    (format) => {
      expect(isPlanechaseFamilyFormat(format)).toBe(false);
    },
  );

  it("is null-safe for unselected formats", () => {
    expect(isPlanechaseFamilyFormat(undefined)).toBe(false);
    expect(isPlanechaseFamilyFormat(null)).toBe(false);
  });
});

describe("Planechase Commander registry membership", () => {
  it("sits in the Commander group so its decks build like Commander decks", () => {
    expect(formatMetadata("PlanechaseCommander")?.group).toBe("Commander");
  });

  it("is a deck-construction format alongside its Commander siblings", () => {
    const formats = DECK_CONSTRUCTION_FORMATS.map((m) => m.format);

    expect(formats).toContain("PlanechaseCommander");
    expect(formats).toContain("TwoHeadedGiantCommander");
    expect(formats).toContain("ArchenemyCommander");
    expect(formats).toContain("Planechase");
    expect(formats).not.toContain("TwoHeadedGiant");
  });
});

describe("isArchenemyFamilyFormat", () => {
  it.each(["Archenemy", "ArchenemyCommander"] as const)("covers %s", (format) => {
    expect(isArchenemyFamilyFormat(format)).toBe(true);
  });

  it.each(["Commander", "Horde", "PlanechaseCommander", "Freeform"] as const)(
    "excludes %s",
    (format) => {
      // Horde reuses the OneVsMany topology but its special seat is the
      // AI-piloted challenge deck, never a user-picked archenemy.
      expect(isArchenemyFamilyFormat(format)).toBe(false);
    },
  );

  it("is null-safe for unselected formats", () => {
    expect(isArchenemyFamilyFormat(undefined)).toBe(false);
    expect(isArchenemyFamilyFormat(null)).toBe(false);
  });
});

describe("Archenemy Commander seat minimum", () => {
  it("requires three seats so 1v1 tables cannot be set up", () => {
    // Discord report (Vesperbat): Archenemy Commander could be set up as a
    // 1v1. Archenemy uses the Team vs. Team multiplayer variant (CR 904.2)
    // and multiplayer begins with more than two players (CR 800.1), so 1v2
    // is the smallest legal table. Mirrors the engine registry.
    expect(formatMetadata("ArchenemyCommander")?.default_config.min_players).toBe(3);
  });
});
