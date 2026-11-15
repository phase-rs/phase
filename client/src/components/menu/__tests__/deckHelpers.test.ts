import { describe, expect, it } from "vitest";

import { getDeckColorIdentityPips } from "../deckHelpers";

describe("getDeckColorIdentityPips", () => {
  it("represents an empty color identity with the colorless mana symbol", () => {
    expect(getDeckColorIdentityPips([])).toEqual(["C"]);
  });

  it("preserves colored identity symbols", () => {
    expect(getDeckColorIdentityPips(["U", "R"])).toEqual(["U", "R"]);
  });

  it("does not label an unresolved identity as colorless", () => {
    expect(getDeckColorIdentityPips(null)).toBeNull();
  });
});
