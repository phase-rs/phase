import { describe, expect, it } from "vitest";

import { sourceFormatToGameFormat } from "../deckCatalog";

describe("sourceFormatToGameFormat", () => {
  it("does not resolve Two-Headed Giant as a deck-construction source key", () => {
    expect(sourceFormatToGameFormat("TwoHeadedGiant")).toBeUndefined();
    expect(sourceFormatToGameFormat("Two-Headed Giant")).toBeUndefined();
    expect(sourceFormatToGameFormat("2HG")).toBeUndefined();
  });

  it("still resolves deck-construction source keys", () => {
    expect(sourceFormatToGameFormat("Standard")).toBe("Standard");
    expect(sourceFormatToGameFormat("CMD")).toBe("Commander");
    expect(sourceFormatToGameFormat("Planechase")).toBe("Planechase");
  });
});
