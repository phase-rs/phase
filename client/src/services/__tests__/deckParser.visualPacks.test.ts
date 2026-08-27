import { describe, expect, it } from "vitest";

import {
  parseDeckFile,
  parseMtgaDeck,
  representativeDeckVisual,
} from "../deckParser";

describe("representativeDeckVisual", () => {
  it("preserves a Forge main-deck printing after skipping basic lands", () => {
    const deck = parseDeckFile("4 Forest\n1 Lightning Bolt|2XM|[117]");

    expect(representativeDeckVisual(deck)).toEqual({
      name: "Lightning Bolt",
      sourcePrinting: { setCode: "2xm", collectorNumber: "117" },
    });
  });

  it("preserves an MTGA main-deck printing", () => {
    const deck = parseMtgaDeck("Deck\n1 Forest (FDN) 281\n1 Opt (DAR) 60");

    expect(representativeDeckVisual(deck)).toEqual({
      name: "Opt",
      sourcePrinting: { setCode: "dar", collectorNumber: "60" },
    });
  });

  it("prefers a name-only commander without borrowing its main-deck printing", () => {
    expect(representativeDeckVisual({
      commander: ["Atraxa, Praetors' Voice"],
      main: [{
        count: 1,
        name: "Atraxa, Praetors' Voice",
        sourcePrinting: { setCode: "2X2", collectorNumber: "170" },
      }],
      sideboard: [],
    })).toEqual({ name: "Atraxa, Praetors' Voice" });
  });

  it("returns null for empty and basic-land-only decks", () => {
    expect(representativeDeckVisual({ main: [], sideboard: [] })).toBeNull();
    expect(representativeDeckVisual({
      main: [{ count: 20, name: "Snow-Covered Island" }],
      sideboard: [],
    })).toBeNull();
  });
});
