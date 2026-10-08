import { describe, expect, it } from "vitest";

import {
  isManaSymbolShard,
  manaSymbolCode,
  manaSymbolSourceUrl,
  MANA_SYMBOL_SHARDS,
} from "../scryfall.ts";

/**
 * Every symbol Scryfall's `/symbology` endpoint publishes, transcribed here as
 * a literal on 2026-09-04 (84 entries, each with an `svg_uri`).
 *
 * WRITTEN OUT rather than derived, and that is the entire point of this file.
 * The catalog decides two things at once: whether `RichLabel` renders a glyph
 * or literal brace text, and whether `coreDescriptors()` installs the SVG for
 * offline play. An omission is therefore invisible to any test that iterates
 * `MANA_SYMBOL_SHARDS` to build its own expectation — it would simply check a
 * smaller set against itself and pass. Only an independently-sourced list can
 * see a missing symbol.
 *
 * When Scryfall adds a symbol this test fails, which is the intended signal:
 * refresh from `https://api.scryfall.com/symbology` and add it to the catalog.
 */
const SCRYFALL_SYMBOLOGY: readonly string[] = [
  "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "G", "H", "L", "P",
  "Q", "R", "S", "T", "U", "W", "X", "Y", "Z", "½", "∞", "10", "11", "12", "13", "14", "15",
  "16", "17", "18", "19", "20", "HR", "HW", "PW", "TK", "100", "2/B", "2/G", "2/R", "2/U",
  "2/W", "B/G", "B/P", "B/R", "C/B", "C/G", "C/P", "C/R", "C/U", "C/W", "G/P", "G/U", "G/W",
  "R/G", "R/P", "R/W", "U/B", "U/P", "U/R", "W/B", "W/P", "W/U", "B/G/P", "B/R/P", "CHAOS",
  "G/U/P", "G/W/P", "R/G/P", "R/W/P", "U/B/P", "U/R/P", "W/B/P", "W/U/P", "1000000",
];

describe("the mana symbol catalog", () => {
  it("admits exactly the symbols Scryfall publishes", () => {
    expect([...MANA_SYMBOL_SHARDS].sort()).toEqual([...SCRYFALL_SYMBOLOGY].sort());
  });

  it("admits every published symbol through the render-time guard", () => {
    const rejected = SCRYFALL_SYMBOLOGY.filter((symbol) => !isManaSymbolShard(symbol));
    expect(rejected).toEqual([]);
  });

  it("rejects notation that has no Scryfall symbol", () => {
    // Guards the negative direction: a guard that returned true for everything
    // would satisfy the assertion above and admit garbage into the installer.
    for (const notation of ["", "WW", "21", "W/", "/W", "{W}", "w", "FOO", "1/2"]) {
      expect(isManaSymbolShard(notation), `expected {${notation}} to be rejected`).toBe(false);
    }
  });

  /** The codes are the last path segment of an SVG URL and the suffix of a
   *  visual-pack asset key, so a collision would silently install one symbol's
   *  art under another's identity. */
  it("maps every symbol to a distinct URL-safe code", () => {
    const codes = MANA_SYMBOL_SHARDS.map(manaSymbolCode);
    expect(new Set(codes).size).toBe(codes.length);
    expect(codes.filter((code) => !/^[A-Za-z0-9_-]+$/.test(code))).toEqual([]);
  });

  it("builds the documented Scryfall URL, spelling out the two non-ASCII symbols", () => {
    expect(manaSymbolSourceUrl("W")).toBe("https://svgs.scryfall.io/card-symbols/W.svg");
    expect(manaSymbolSourceUrl("2/W")).toBe("https://svgs.scryfall.io/card-symbols/2W.svg");
    expect(manaSymbolSourceUrl("C/P")).toBe("https://svgs.scryfall.io/card-symbols/CP.svg");
    expect(manaSymbolSourceUrl("∞")).toBe("https://svgs.scryfall.io/card-symbols/INFINITY.svg");
    expect(manaSymbolSourceUrl("½")).toBe("https://svgs.scryfall.io/card-symbols/HALF.svg");
  });
});
