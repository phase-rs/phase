import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ParsedDeck } from "../deckParser";

const canonicalCardNamesMock = vi.hoisted(() => vi.fn());
vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ canonicalCardNames: canonicalCardNamesMock }),
}));

import {
  applyCanonicalNames,
  canonicalizeDeckNames,
  canonicalNameMap,
  deckCardNames,
  isCanonicalizableDeck,
} from "../canonicalCardNames";

beforeEach(() => {
  canonicalCardNamesMock.mockReset();
});

function renameTo(map: Record<string, string | null>) {
  return (names: string[]) => Promise.resolve(names.map((name) => map[name] ?? null));
}

describe("canonicalizeDeckNames", () => {
  it("renames every name field", async () => {
    canonicalCardNamesMock.mockImplementation(
      renameTo({
        "Revival/Revenge": "Revival // Revenge",
        "Summon: Choco/Mog": "Summon: Choco // Mog",
        "Fire/Ice": "Fire // Ice",
        "Bind/Liberate": "Bind // Liberate",
        "Smelt/Herd": "Smelt // Herd",
        "Start/Finish": "Start // Finish",
        "Who/What": "Who // What",
        sheet1: "SHEET-CANON",
      }),
    );
    const deck: ParsedDeck = {
      main: [{ count: 1, name: "Revival/Revenge" }],
      sideboard: [{ count: 1, name: "Summon: Choco/Mog" }],
      commander: ["Fire/Ice"],
      companion: "Bind/Liberate",
      signature_spell: ["Smelt/Herd"],
      planar_deck: ["Start/Finish"],
      scheme_deck: ["Who/What"],
      sticker_sheets: ["sheet1"],
    };

    const result = await canonicalizeDeckNames(deck);

    expect(result.main).toEqual([{ count: 1, name: "Revival // Revenge" }]);
    expect(result.sideboard).toEqual([{ count: 1, name: "Summon: Choco // Mog" }]);
    expect(result.commander).toEqual(["Fire // Ice"]);
    expect(result.companion).toBe("Bind // Liberate");
    expect(result.signature_spell).toEqual(["Smelt // Herd"]);
    expect(result.planar_deck).toEqual(["Start // Finish"]);
    expect(result.scheme_deck).toEqual(["Who // What"]);
    expect(result.sticker_sheets).toEqual(["SHEET-CANON"]);
  });

  it("merges entries that canonicalize to one name", async () => {
    canonicalCardNamesMock.mockImplementation(
      renameTo({ "Revival/Revenge": "Revival // Revenge" }),
    );
    const deck: ParsedDeck = {
      main: [
        {
          count: 2,
          name: "Revival/Revenge",
          sourcePrinting: { setCode: "MH2", collectorNumber: "1" },
        },
        { count: 2, name: "Revival // Revenge" },
      ],
      sideboard: [],
    };

    const result = await canonicalizeDeckNames(deck);

    expect(result.main).toEqual([
      {
        count: 4,
        name: "Revival // Revenge",
        sourcePrinting: { setCode: "MH2", collectorNumber: "1" },
      },
    ]);
  });

  it("leaves names the engine has no spelling for", async () => {
    canonicalCardNamesMock.mockImplementation(
      renameTo({ "Revival/Revenge": "Revival // Revenge" }),
    );
    const deck: ParsedDeck = {
      main: [
        { count: 1, name: "Summon: Choco" },
        { count: 1, name: "Not A Card" },
        { count: 1, name: "Revival/Revenge" },
      ],
      sideboard: [],
    };

    const result = await canonicalizeDeckNames(deck);

    expect(result.main).toEqual([
      { count: 1, name: "Summon: Choco" },
      { count: 1, name: "Not A Card" },
      { count: 1, name: "Revival // Revenge" },
    ]);
  });

  it("returns the deck unchanged when the engine cannot answer", async () => {
    canonicalCardNamesMock.mockRejectedValueOnce(new Error("worker unavailable"));
    const deck: ParsedDeck = {
      main: [{ count: 1, name: "Revival/Revenge" }],
      sideboard: [],
    };

    await expect(canonicalizeDeckNames(deck)).resolves.toBe(deck);

    canonicalCardNamesMock.mockResolvedValueOnce([]);
    await expect(canonicalizeDeckNames(deck)).resolves.toBe(deck);
  });

  it("asks once per distinct name and not at all for an empty deck", async () => {
    canonicalCardNamesMock.mockImplementation(
      renameTo({ "Revival/Revenge": "Revival // Revenge" }),
    );
    const deck: ParsedDeck = {
      main: [
        { count: 2, name: "Revival/Revenge" },
        { count: 1, name: "Revival/Revenge" },
      ],
      sideboard: [{ count: 1, name: "Revival/Revenge" }],
      commander: ["Revival/Revenge"],
    };

    await canonicalizeDeckNames(deck);

    expect(canonicalCardNamesMock).toHaveBeenCalledExactlyOnceWith(["Revival/Revenge"]);

    canonicalCardNamesMock.mockClear();
    await canonicalizeDeckNames({ main: [], sideboard: [] });
    expect(canonicalCardNamesMock).not.toHaveBeenCalled();
  });
});

describe("deckCardNames / canonicalNameMap / applyCanonicalNames", () => {
  it("collects every name field", () => {
    const deck: ParsedDeck = {
      main: [{ count: 1, name: "A" }],
      sideboard: [{ count: 1, name: "B" }],
      commander: ["C"],
      companion: "D",
      signature_spell: ["E"],
      planar_deck: ["F"],
      scheme_deck: ["G"],
      sticker_sheets: ["H"],
    };
    expect(deckCardNames(deck)).toEqual(["A", "B", "C", "D", "E", "F", "G", "H"]);
  });

  it("holds only names the engine spells differently", async () => {
    canonicalCardNamesMock.mockImplementation(
      renameTo({ "Revival/Revenge": "Revival // Revenge", Lightning: "Lightning" }),
    );
    const map = await canonicalNameMap(["Revival/Revenge", "Lightning", "Unknown"]);
    expect(Object.fromEntries(map)).toEqual({ "Revival/Revenge": "Revival // Revenge" });
  });

  it("returns the input deck object unchanged when renamed is empty", () => {
    const deck: ParsedDeck = { main: [], sideboard: [] };
    expect(applyCanonicalNames(deck, new Map())).toBe(deck);
  });
});

describe("isCanonicalizableDeck", () => {
  it("accepts a well-formed deck", () => {
    const deck: ParsedDeck = {
      main: [{ count: 1, name: "A" }],
      sideboard: [{ count: 1, name: "B" }],
      commander: ["C"],
      companion: "D",
    };
    expect(isCanonicalizableDeck(deck)).toBe(true);
  });

  it.each([
    ["null", null],
    ["a string", "not-a-deck"],
    ["missing main", { sideboard: [] }],
    ["missing sideboard", { main: [] }],
    ["main not an array", { main: "x", sideboard: [] }],
    ["main entry with a non-string name", { main: [{ count: 1, name: 5 }], sideboard: [] }],
    ["commander as a string instead of an array", { main: [], sideboard: [], commander: "X" }],
    ["companion as a number", { main: [], sideboard: [], companion: 1 }],
  ])("rejects %s", (_label, value) => {
    expect(isCanonicalizableDeck(value)).toBe(false);
  });
});
