import { describe, expect, it } from "vitest";

import type { ScryfallCard } from "../../../services/scryfall";
import { getColorIdentityViolations } from "../commanderUtils";

function card(colorIdentity: string[]): ScryfallCard {
  return { color_identity: colorIdentity } as ScryfallCard;
}

describe("getColorIdentityViolations", () => {
  it("flags off-color cards for a genuinely colorless commander (CR 903.5c)", () => {
    const cache = new Map<string, ScryfallCard>([
      ["Kozilek, Butcher of Truth", card([])],
      ["Lightning Bolt", card(["R"])],
      ["Wastes", card([])],
    ]);
    const deck = [
      { name: "Lightning Bolt", count: 1 },
      { name: "Wastes", count: 1 },
    ];

    const violations = getColorIdentityViolations(
      deck,
      ["Kozilek, Butcher of Truth"],
      cache,
    );

    expect(violations).toEqual(["Lightning Bolt"]);
  });

  it("does not flag on-identity cards for a colored commander", () => {
    const cache = new Map<string, ScryfallCard>([
      ["Krenko, Mob Boss", card(["R"])],
      ["Lightning Bolt", card(["R"])],
      ["Counterspell", card(["U"])],
    ]);
    const deck = [
      { name: "Lightning Bolt", count: 1 },
      { name: "Counterspell", count: 1 },
    ];

    expect(
      getColorIdentityViolations(deck, ["Krenko, Mob Boss"], cache),
    ).toEqual(["Counterspell"]);
  });

  it("returns no violations while commander data is still loading", () => {
    // Commander not in cache — an empty identity must NOT flag every colored card.
    const cache = new Map<string, ScryfallCard>([
      ["Lightning Bolt", card(["R"])],
    ]);
    const deck = [{ name: "Lightning Bolt", count: 1 }];

    expect(
      getColorIdentityViolations(deck, ["Kozilek, Butcher of Truth"], cache),
    ).toEqual([]);
  });
});
