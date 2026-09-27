import { describe, expect, it } from "vitest";

import { buildBracketDeckKey } from "../bracketDeckKey";

const undeclared = { kind: "undeclared" } as const;

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
    const a = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 1]]), null, undeclared);
    const b = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 2]]), null, undeclared);
    expect(a).not.toBe(b);
  });

  it("changes when only the companion differs", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, {
      ...deck([["Lightning Bolt", 1]]),
      companion: "Lutri, the Spellchaser",
    }, null, undeclared);
    const b = buildBracketDeckKey(commanders, {
      ...deck([["Lightning Bolt", 1]]),
      companion: "Zirda, the Dawnwaker",
    }, null, undeclared);
    expect(a).not.toBe(b);
  });

  it("changes when only the signature spell differs", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, {
      ...deck([["Lightning Bolt", 1]]),
      signature_spell: ["Lightning Bolt"],
    }, null, undeclared);
    const b = buildBracketDeckKey(commanders, {
      ...deck([["Lightning Bolt", 1]]),
      signature_spell: ["Shock"],
    }, null, undeclared);
    expect(a).not.toBe(b);
  });

  it("is stable for identical decks", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 1]]), "core", undeclared);
    const b = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]], [["Pyroblast", 1]]), "core", undeclared);
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
      null,
      undeclared,
    );
    const b = buildBracketDeckKey(
      commanders,
      deck(
        [["Shock", 2], ["Lightning Bolt", 1]],
        [["Red Elemental Blast", 1], ["Pyroblast", 1]],
      ),
      null,
      undeclared,
    );
    expect(a).toBe(b);
  });

  it("changes when the main deck differs", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 1]]), null, undeclared);
    const b = buildBracketDeckKey(commanders, deck([["Lightning Bolt", 2]]), null, undeclared);
    expect(a).not.toBe(b);
  });

  it("distinguishes a main-deck card from a sideboard card of the same name", () => {
    const commanders = ["Krenko, Mob Boss"];
    const a = buildBracketDeckKey(commanders, deck([["Pyroblast", 1]], []), null, undeclared);
    const b = buildBracketDeckKey(commanders, deck([], [["Pyroblast", 1]]), null, undeclared);
    expect(a).not.toBe(b);
  });

  it("changes when only the declared tier differs", () => {
    const commanders = ["Krenko, Mob Boss"];
    const value = deck([["Lightning Bolt", 1]]);
    expect(buildBracketDeckKey(commanders, value, "core", undeclared)).not.toBe(
      buildBracketDeckKey(commanders, value, "optimized", undeclared),
    );
  });

  it("is stable for the same declared tier", () => {
    const commanders = ["Krenko, Mob Boss"];
    const value = deck([["Lightning Bolt", 1]]);
    expect(buildBracketDeckKey(commanders, value, "upgraded", undeclared)).toBe(
      buildBracketDeckKey(commanders, value, "upgraded", undeclared),
    );
  });

  it("uses a stable suffix for an undeclared deck", () => {
    expect(buildBracketDeckKey(["Krenko, Mob Boss"], deck([]), null, undeclared)).toMatch(
      /#d:none#combo:undeclared:-$/,
    );
  });

  it("changes when only the combo declaration differs", () => {
    const commanders = ["Krenko, Mob Boss"];
    const value = deck([["Lightning Bolt", 1]]);
    expect(buildBracketDeckKey(commanders, value, null, undeclared)).not.toBe(
      buildBracketDeckKey(commanders, value, null, {
        kind: "intended",
        window: "early_game",
      }),
    );
  });

  it("is stable for identical combo declarations", () => {
    const commanders = ["Krenko, Mob Boss"];
    const value = deck([["Lightning Bolt", 1]]);
    const combo = { kind: "intended", window: null } as const;
    expect(buildBracketDeckKey(commanders, value, null, combo)).toBe(
      buildBracketDeckKey(commanders, value, null, combo),
    );
  });
});
