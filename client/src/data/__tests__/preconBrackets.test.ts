import { describe, expect, it } from "vitest";

import { getPreconBracket, PRECON_BRACKETS } from "../preconBrackets";
import { isCommanderBracket } from "../../types/bracket";

describe("preconBrackets", () => {
  it("every overlay entry is a valid CommanderBracket", () => {
    for (const [deckId, bracket] of Object.entries(PRECON_BRACKETS)) {
      expect(deckId).toEqual(expect.stringMatching(/.+/));
      expect(isCommanderBracket(bracket)).toBe(true);
    }
  });

  it("getPreconBracket returns the curated value for known deckIds", () => {
    const sampleId = Object.keys(PRECON_BRACKETS)[0];
    if (!sampleId) {
      // Overlay is empty until a curator adds entries; the lookup
      // contract still has to hold.
      expect(getPreconBracket("AdaptiveEnchantment_C18")).toBeNull();
      return;
    }
    expect(getPreconBracket(sampleId)).toBe(PRECON_BRACKETS[sampleId]);
  });

  it("getPreconBracket returns null for unknown deckIds", () => {
    expect(getPreconBracket("NotARealPrecon_XXX")).toBeNull();
  });
});
