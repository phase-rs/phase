import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchDeckFromUrl, isSupportedDeckUrl } from "../deckUrlImport";

beforeEach(() => {
  vi.restoreAllMocks();
});

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
