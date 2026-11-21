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

describe("buildDeckCatalog combo declaration", () => {
  function saveWith(name: string, sidecar: unknown): void {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + name,
      JSON.stringify({ main: [{ count: 1, name: "Forest" }], sideboard: [], combo_declaration: sidecar }),
    );
  }

  it("reads each saved deck's own sidecar", async () => {
    const early = { kind: "intended", window: "early_game" };
    saveWith("Early", early);
    saveWith("Plain", undefined);

    const catalog = await buildDeckCatalog({ savedDeckNames: ["Early", "Plain"], includePrecons: false });

    expect(catalog.map((candidate) => candidate.comboDeclaration)).toEqual([early, { kind: "undeclared" }]);
  });

  it("treats an unreadable sidecar as unanswered", async () => {
    saveWith("Garbled", { kind: "maybe" });

    const [candidate] = await buildDeckCatalog({ savedDeckNames: ["Garbled"], includePrecons: false });

    expect(candidate?.comboDeclaration).toEqual({ kind: "undeclared" });
  });
});
