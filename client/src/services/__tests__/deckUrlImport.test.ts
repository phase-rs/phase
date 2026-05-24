import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchDeckFromUrl, isSupportedDeckUrl } from "../deckUrlImport";
import { detectAndParseDeck, resolveCommander, type ParsedDeck } from "../deckParser";

// resolveCommander delegates commander eligibility to the WASM engine; every
// fixture below carries an explicit commander/sideboard so resolveCommander
// short-circuits before this is ever called, but mock it so the module graph
// never touches WASM during the test run.
vi.mock("../engineRuntime", () => ({
  isCardCommanderEligible: vi.fn().mockResolvedValue(true),
}));

beforeEach(() => {
  vi.restoreAllMocks();
});

function mockFetchJson(payload: unknown): void {
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: () => Promise.resolve(payload),
  });
}

function totalCards(entries: ParsedDeck["main"]): number {
  return entries.reduce((sum, entry) => sum + entry.count, 0);
}

describe("isSupportedDeckUrl", () => {
  it("recognizes Moxfield and Archidekt deck URLs", () => {
    expect(isSupportedDeckUrl("https://www.moxfield.com/decks/abc123")).toBe(true);
    expect(isSupportedDeckUrl("https://archidekt.com/decks/456789/my_deck")).toBe(true);
  });

  it("rejects unrelated or malformed URLs", () => {
    expect(isSupportedDeckUrl("https://example.com/decks/abc")).toBe(false);
    expect(isSupportedDeckUrl("https://archidekt.com/decks/not-numeric")).toBe(false);
    expect(isSupportedDeckUrl("not a url")).toBe(false);
  });
});

describe("fetchDeckFromUrl — Moxfield", () => {
  it("projects v2 boards onto canonical decklist text with printings", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({
        name: "Krenko Goblins",
        commanders: {
          a: { quantity: 1, card: { name: "Krenko, Mob Boss", set: "m19", cn: "145" } },
        },
        mainboard: {
          b: { quantity: 1, card: { name: "Sol Ring", set: "ltc", cn: "280" } },
          c: { quantity: 1, card: { name: "Goblin Chieftain", set: "m10", cn: "139" } },
        },
        sideboard: {},
        companions: {},
      }),
    });

    const text = await fetchDeckFromUrl("https://www.moxfield.com/decks/oEWXWHM5");
    expect(text).toBe(
      [
        "Name: Krenko Goblins",
        "[Commander]",
        "1 Krenko, Mob Boss (M19) 145",
        "[Main]",
        "1 Sol Ring (LTC) 280",
        "1 Goblin Chieftain (M10) 139",
        "",
      ].join("\n"),
    );
    expect(global.fetch).toHaveBeenCalledWith(
      "https://api2.moxfield.com/v2/decks/all/oEWXWHM5",
    );
  });

  it("captures the companion under its own section", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({
        name: "Lurrus Aggro",
        mainboard: { a: { quantity: 4, card: { name: "Mishra's Bauble" } } },
        companions: { b: { quantity: 1, card: { name: "Lurrus of the Dream-Den" } } },
      }),
    });

    const text = await fetchDeckFromUrl("https://moxfield.com/decks/xyz");
    expect(text).toContain("[Companion]\n1 Lurrus of the Dream-Den");
    // Companion section is emitted last so the parser cannot misfile later cards.
    expect(text.indexOf("[Main]")).toBeLessThan(text.indexOf("[Companion]"));
  });

  it("surfaces a friendly error for private/empty decks", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ name: "Hidden", mainboard: {}, commanders: {} }),
    });

    await expect(fetchDeckFromUrl("https://moxfield.com/decks/zzz")).rejects.toThrow(
      /no cards, or it is private/,
    );
  });

  it("reports a CORS/network failure with actionable guidance", async () => {
    global.fetch = vi.fn().mockRejectedValue(new TypeError("Failed to fetch"));

    await expect(fetchDeckFromUrl("https://moxfield.com/decks/zzz")).rejects.toThrow(
      /Couldn't reach Moxfield/,
    );
  });
});

describe("fetchDeckFromUrl — Archidekt", () => {
  it("classifies cards by category and skips excluded boards", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({
        name: "Zimone Combo",
        categories: [
          { name: "Commander", includedInDeck: true },
          { name: "Maybeboard", includedInDeck: false },
        ],
        cards: [
          {
            quantity: 1,
            categories: ["Commander"],
            card: {
              oracleCard: { name: "Zimone, All-Questioning" },
              edition: { editioncode: "dft" },
              collectorNumber: "229",
            },
          },
          {
            quantity: 1,
            categories: ["Lands"],
            card: {
              oracleCard: { name: "Command Tower" },
              edition: { editioncode: "cmr" },
              collectorNumber: "350",
            },
          },
          {
            quantity: 1,
            categories: ["Maybeboard"],
            card: {
              oracleCard: { name: "Mana Crypt" },
              edition: { editioncode: "2xm" },
              collectorNumber: "270",
            },
          },
          {
            quantity: 2,
            categories: ["Sideboard"],
            card: {
              oracleCard: { name: "Negate" },
              edition: { editioncode: "m21" },
              collectorNumber: "55",
            },
          },
        ],
      }),
    });

    const text = await fetchDeckFromUrl("https://archidekt.com/decks/123456/zimone");
    expect(text).toBe(
      [
        "Name: Zimone Combo",
        "[Commander]",
        "1 Zimone, All-Questioning (DFT) 229",
        "[Main]",
        "1 Command Tower (CMR) 350",
        "[Sideboard]",
        "2 Negate (M21) 55",
        "",
      ].join("\n"),
    );
    expect(global.fetch).toHaveBeenCalledWith("https://archidekt.com/api/decks/123456/");
  });
});

// The importer is format-agnostic — it emits canonical decklist text that the
// shared deckParser normalizes. What actually varies across the game's
// supported formats is the structural SHAPE of a legal deck (does it have a
// commander? a sideboard? a companion?). These cases import a source deck
// shaped like each format and assert the structure survives the full
// fetch → detectAndParseDeck → resolveCommander pipeline.

type DeckShape = "constructed" | "commander" | "commanderWithSideboard" | "mainOnly";

interface MoxEntry {
  quantity: number;
  card: { name: string };
}

function moxBoard(cards: Array<[string, number]>): Record<string, MoxEntry> {
  const board: Record<string, MoxEntry> = {};
  cards.forEach(([name, quantity], i) => {
    board[`c${i}`] = { quantity, card: { name } };
  });
  return board;
}

// Representative source decks per shape — small but legal-shaped (4-ofs for
// constructed, singletons for commander). Card identities are arbitrary.
function moxPayloadForShape(shape: DeckShape): Record<string, unknown> {
  const constructedMain = moxBoard([["Mountain", 24], ["Lightning Bolt", 4], ["Monastery Swiftspear", 4]]);
  const constructedSb = moxBoard([["Abrade", 2], ["Smash to Smithereens", 2]]);
  const singletonMain = moxBoard([["Sol Ring", 1], ["Command Tower", 1], ["Arcane Signet", 1]]);
  switch (shape) {
    case "constructed":
      return { name: "Mono-Red", mainboard: constructedMain, sideboard: constructedSb };
    case "commander":
      return { name: "Krenko EDH", commanders: moxBoard([["Krenko, Mob Boss", 1]]), mainboard: singletonMain };
    case "commanderWithSideboard":
      return {
        name: "Tiny Leader",
        commanders: moxBoard([["Goblin Welder", 1]]),
        mainboard: singletonMain,
        sideboard: moxBoard([["Pyroblast", 1]]),
      };
    case "mainOnly":
      return { name: "Sealed Pool", mainboard: moxBoard([["Mountain", 17], ["Shock", 2], ["Goblin Tutor", 1]]) };
  }
}

function assertShape(deck: ParsedDeck, shape: DeckShape): void {
  expect(totalCards(deck.main)).toBeGreaterThan(0);

  if (shape === "commander" || shape === "commanderWithSideboard") {
    expect(deck.commander ?? []).not.toHaveLength(0);
    // The commander must not also linger in the main deck.
    expect(deck.main.map((e) => e.name)).not.toContain((deck.commander ?? [])[0]);
  } else {
    expect(deck.commander ?? []).toHaveLength(0);
  }

  if (shape === "constructed" || shape === "commanderWithSideboard") {
    expect(totalCards(deck.sideboard)).toBeGreaterThan(0);
  } else {
    expect(deck.sideboard).toHaveLength(0);
  }
}

// Every GameFormat in crates/engine/src/types/format.rs, mapped to its deck shape.
const FORMAT_SHAPES: Array<[format: string, shape: DeckShape]> = [
  ["Standard", "constructed"],
  ["Pioneer", "constructed"],
  ["Modern", "constructed"],
  ["Premodern", "constructed"],
  ["Legacy", "constructed"],
  ["Vintage", "constructed"],
  ["Historic", "constructed"],
  ["Timeless", "constructed"],
  ["Pauper", "constructed"],
  ["FreeForAll", "constructed"],
  ["TwoHeadedGiant", "constructed"],
  ["Commander", "commander"],
  ["DuelCommander", "commander"],
  ["PauperCommander", "commander"],
  ["Brawl", "commander"],
  ["HistoricBrawl", "commander"],
  ["TinyLeaders", "commanderWithSideboard"],
  ["Limited", "mainOnly"],
];

describe("fetchDeckFromUrl — format coverage", () => {
  it.each(FORMAT_SHAPES)("imports a %s-shaped deck into the right zones", async (format, shape) => {
    mockFetchJson(moxPayloadForShape(shape));
    const deck = await resolveCommander(
      detectAndParseDeck(await fetchDeckFromUrl("https://moxfield.com/decks/" + format)),
    );
    assertShape(deck, shape);
  });

  it("preserves a companion alongside any format's main/sideboard", async () => {
    mockFetchJson({
      name: "Lurrus Burn",
      mainboard: moxBoard([["Mountain", 20], ["Lightning Bolt", 4]]),
      sideboard: moxBoard([["Smash to Smithereens", 2]]),
      companions: moxBoard([["Lurrus of the Dream-Den", 1]]),
    });
    const deck = await resolveCommander(
      detectAndParseDeck(await fetchDeckFromUrl("https://moxfield.com/decks/lurrus")),
    );
    expect(deck.companion).toBe("Lurrus of the Dream-Den");
    expect(totalCards(deck.main)).toBeGreaterThan(0);
    expect(totalCards(deck.sideboard)).toBeGreaterThan(0);
  });

  it("keeps Tiny Leaders' commander and sideboard distinct from the main deck", async () => {
    mockFetchJson(moxPayloadForShape("commanderWithSideboard"));
    const deck = await resolveCommander(
      detectAndParseDeck(await fetchDeckFromUrl("https://moxfield.com/decks/tl")),
    );
    expect(deck.commander).toEqual(["Goblin Welder"]);
    expect(deck.sideboard.map((e) => e.name)).toEqual(["Pyroblast"]);
    expect(deck.main.map((e) => e.name)).toEqual(["Sol Ring", "Command Tower", "Arcane Signet"]);
  });
});

describe("fetchDeckFromUrl — unsupported", () => {
  it("rejects non-deck URLs without fetching", async () => {
    global.fetch = vi.fn();
    await expect(fetchDeckFromUrl("https://example.com/foo")).rejects.toThrow(/Unsupported link/);
    expect(global.fetch).not.toHaveBeenCalled();
  });

  it("rejects malformed input", async () => {
    await expect(fetchDeckFromUrl("nonsense")).rejects.toThrow(/valid Moxfield or Archidekt/);
  });
});
