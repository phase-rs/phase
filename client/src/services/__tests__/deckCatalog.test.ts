import { beforeEach, describe, expect, it } from "vitest";

import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import { buildDeckCatalog, sourceFormatToGameFormat } from "../deckCatalog";

beforeEach(() => {
  localStorage.clear();
});

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

  it("keeps an imported Oathbreaker deck in the Oathbreaker catalog", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Daretti Oathbreaker",
      JSON.stringify({
        main: [{ count: 58, name: "Mountain" }],
        sideboard: [],
        commander: ["Daretti, Ingenious Iconoclast"],
        signature_spell: ["Scheming Symmetry"],
        format: "Oathbreaker",
      }),
    );

    const catalog = await buildDeckCatalog({
      savedDeckNames: ["Daretti Oathbreaker"],
      includePrecons: false,
    });

    expect(catalog[0]?.knownFormat).toBe("Oathbreaker");
  });
});
