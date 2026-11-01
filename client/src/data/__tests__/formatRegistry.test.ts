import { describe, expect, it } from "vitest";

import {
  DECK_CONSTRUCTION_FORMATS,
  formatMetadata,
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
