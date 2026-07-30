import { describe, expect, it } from "vitest";

import { buildBracketDeckKey } from "../bracketDeckKey";

const deck = (
  main: [string, number][],
  sideboard: [string, number][] = [],
) => ({
  main: main.map(([name, count]) => ({ name, count })),
  sideboard: sideboard.map(([name, count]) => ({ name, count })),
});

describe("buildBracketDeckKey", () => {
  it("changes when only the sideboard differs", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 1]]));
    const b = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 2]]));
    expect(a).not.toBe(b);
  });

  it("is stable for identical decks", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 1]]));
    const b = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 1]]));
    expect(a).toBe(b);
  });

  it("is order-independent within main and sideboard", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(
      commanders,
      deck(
        [["Lightning Bolt", 1], ["Shock", 2]],
        [["Pyroblast", 1], ["Red Elemental Blast", 1]],
      ),
    );
    const b = buildBracketDeckKey(
      commanders,
      deck(
        [["Shock", 2], ["Lightning Bolt", 1]],
        [["Red Elemental Blast", 1], ["Pyroblast", 1]],
      ),
    );
    expect(a).toBe(b);
  });

  it("changes when the main deck differs", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]]));
    const b = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 2]]));
    expect(a).not.toBe(b);
  });

  it("distinguishes a main-deck card from a sideboard card of the same name", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, deck([["Pyroblast", 1]], []));
    const b = buildBracketDeckKey(commanders, deck([], [["Pyroblast", 1]]));
    expect(a).not.toBe(b);
  });
});
