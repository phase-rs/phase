import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchDeckFromUrl, isSupportedDeckUrl, IMPORT_ERROR_KEYS } from "../deckUrlImport";
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

function mockWorkerText(text: string): void {
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    status: 200,
    text: () => Promise.resolve(text),
  });
}

function mockWorkerError(status: number, body: { error: string; message: string }): void {
  global.fetch = vi.fn().mockResolvedValue({
    ok: false,
    status,
    json: () => Promise.resolve(body),
  });
}

function totalCards(entries: ParsedDeck["main"]): number {
  return entries.reduce((sum, entry) => sum + entry.count, 0);
}

// ---------------------------------------------------------------------------
// URL validation (cheap client-side check used to gate the Import button)
// ---------------------------------------------------------------------------

describe("isSupportedDeckUrl", () => {
  it("recognizes Moxfield and Archidekt deck URLs", () => {
    expect(isSupportedDeckUrl("https://www.moxfield.com/decks/abc123")).toBe(true);
    expect(isSupportedDeckUrl("https://archidekt.com/decks/456789/my_deck")).toBe(true);
  });

  it("accepts protocol-less URLs (users often copy-paste without https://)", () => {
    expect(isSupportedDeckUrl("moxfield.com/decks/abc123")).toBe(true);
    expect(isSupportedDeckUrl("www.archidekt.com/decks/789")).toBe(true);
  });

  it("accepts pasted URLs wrapped with angle brackets or trailing punctuation", () => {
    expect(isSupportedDeckUrl("<https://archidekt.com/decks/456789/my_deck>")).toBe(true);
    expect(isSupportedDeckUrl("https://archidekt.com/decks/456789/my_deck.")).toBe(true);
    expect(isSupportedDeckUrl("<https://archidekt.com/decks/456789/my_deck>.")).toBe(true);
  });

  it("rejects unrelated or malformed URLs", () => {
    expect(isSupportedDeckUrl("https://example.com/decks/abc")).toBe(false);
    expect(isSupportedDeckUrl("https://archidekt.com/decks/not-numeric")).toBe(false);
    expect(isSupportedDeckUrl("not a url")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Worker client contract — what the browser sees is canonical decklist text.
// Source-specific projection (Moxfield/Archidekt JSON → canonical text) is
// tested in lobby-worker/test/import-deck.test.mjs, not here.
// ---------------------------------------------------------------------------

describe("fetchDeckFromUrl", () => {
  it("calls /import-deck?url=... and returns the worker's decklist text", async () => {
    mockWorkerText("Name: Krenko\n[Commander]\n1 Krenko, Mob Boss\n[Main]\n1 Sol Ring\n");
    const text = await fetchDeckFromUrl("https://www.moxfield.com/decks/oEWXWHM5");
    expect(text).toMatch(/^Name: Krenko/);
    expect(global.fetch).toHaveBeenCalledTimes(1);
    const [calledUrl] = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(calledUrl).toContain("/import-deck?url=");
    expect(calledUrl).toContain(encodeURIComponent("https://www.moxfield.com/decks/oEWXWHM5"));
  });

  it("rejects malformed input by throwing the invalid-URL translation key", async () => {
    global.fetch = vi.fn();
    await expect(fetchDeckFromUrl("nonsense")).rejects.toThrow(IMPORT_ERROR_KEYS.invalidUrl);
    expect(global.fetch).not.toHaveBeenCalled();
  });

  it("rejects unsupported sources by throwing the invalid-URL translation key", async () => {
    global.fetch = vi.fn();
    await expect(fetchDeckFromUrl("https://example.com/decks/abc")).rejects.toThrow(
      IMPORT_ERROR_KEYS.invalidUrl,
    );
    expect(global.fetch).not.toHaveBeenCalled();
  });

  it("normalizes protocol-less URLs before sending to the worker", async () => {
    mockWorkerText("Name: X\n[Main]\n1 Forest\n");
    await fetchDeckFromUrl("moxfield.com/decks/abc");
    const [calledUrl] = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(calledUrl).toContain(encodeURIComponent("https://moxfield.com/decks/abc"));
  });

  it("strips pasted URL wrappers/trailing punctuation before sending to the worker", async () => {
    mockWorkerText("Name: X\n[Main]\n1 Forest\n");
    await fetchDeckFromUrl("<https://archidekt.com/decks/123456/my_deck>.");
    const [calledUrl] = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(calledUrl).toContain(
      encodeURIComponent("https://archidekt.com/decks/123456/my_deck"),
    );
  });

  it("surfaces the worker's user-facing error message verbatim", async () => {
    mockWorkerError(404, { error: "not_found", message: "Moxfield deck not found or private." });
    await expect(fetchDeckFromUrl("https://moxfield.com/decks/zzz")).rejects.toThrow(
      /Moxfield deck not found or private/,
    );
  });

  it("falls back to a generic message when the error body is not JSON", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 503,
      json: () => Promise.reject(new SyntaxError("not json")),
    });
    await expect(fetchDeckFromUrl("https://moxfield.com/decks/zzz")).rejects.toThrow(
      /Import failed \(503\)/,
    );
  });

  it("surfaces a network failure by throwing the network-failure translation key", async () => {
    global.fetch = vi.fn().mockRejectedValue(new TypeError("Failed to fetch"));
    await expect(fetchDeckFromUrl("https://moxfield.com/decks/zzz")).rejects.toThrow(
      IMPORT_ERROR_KEYS.networkFailure,
    );
  });
});

// ---------------------------------------------------------------------------
// End-to-end shape coverage. The importer is format-agnostic — what varies
// across the game's supported formats is the STRUCTURAL shape of a legal deck
// (commander? sideboard? companion?). These cases mock the worker to return
// canonical text of each shape and assert it survives the full
// fetchDeckFromUrl → detectAndParseDeck → resolveCommander pipeline.
// ---------------------------------------------------------------------------

type DeckShape = "constructed" | "commander" | "commanderWithSideboard" | "mainOnly";

function canonicalForShape(shape: DeckShape): string {
  const cards = {
    constructedMain: "24 Mountain\n4 Lightning Bolt\n4 Monastery Swiftspear",
    constructedSb: "2 Abrade\n2 Smash to Smithereens",
    singletonMain: "1 Sol Ring\n1 Command Tower\n1 Arcane Signet",
  };
  switch (shape) {
    case "constructed":
      return `Name: Mono-Red\n[Main]\n${cards.constructedMain}\n[Sideboard]\n${cards.constructedSb}\n`;
    case "commander":
      return `Name: Krenko EDH\n[Commander]\n1 Krenko, Mob Boss\n[Main]\n${cards.singletonMain}\n`;
    case "commanderWithSideboard":
      return (
        `Name: Tiny Leader\n[Commander]\n1 Goblin Welder\n`
        + `[Main]\n${cards.singletonMain}\n[Sideboard]\n1 Pyroblast\n`
      );
    case "mainOnly":
      return `Name: Sealed Pool\n[Main]\n17 Mountain\n2 Shock\n1 Goblin Tutor\n`;
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
  ["Archenemy", "constructed"],
  ["Commander", "commander"],
  ["DuelCommander", "commander"],
  ["PauperCommander", "commander"],
  ["Brawl", "commander"],
  ["HistoricBrawl", "commander"],
  ["TinyLeaders", "commanderWithSideboard"],
  ["Limited", "mainOnly"],
];

describe("fetchDeckFromUrl — format coverage", () => {
  it.each(FORMAT_SHAPES)("imports a %s-shaped deck into the right zones", async (_format, shape) => {
    mockWorkerText(canonicalForShape(shape));
    const deck = await resolveCommander(
      detectAndParseDeck(await fetchDeckFromUrl("https://moxfield.com/decks/sample")),
    );
    assertShape(deck, shape);
  });

  it("preserves a companion alongside main + sideboard", async () => {
    mockWorkerText(
      "Name: Lurrus Burn\n"
        + "[Main]\n20 Mountain\n4 Lightning Bolt\n"
        + "[Sideboard]\n2 Smash to Smithereens\n"
        + "[Companion]\n1 Lurrus of the Dream-Den\n",
    );
    const deck = await resolveCommander(
      detectAndParseDeck(await fetchDeckFromUrl("https://moxfield.com/decks/lurrus")),
    );
    expect(deck.companion).toBe("Lurrus of the Dream-Den");
    expect(totalCards(deck.main)).toBeGreaterThan(0);
    expect(totalCards(deck.sideboard)).toBeGreaterThan(0);
  });

  it("keeps Tiny Leaders' commander and sideboard distinct from the main deck", async () => {
    mockWorkerText(canonicalForShape("commanderWithSideboard"));
    const deck = await resolveCommander(
      detectAndParseDeck(await fetchDeckFromUrl("https://moxfield.com/decks/tl")),
    );
    expect(deck.commander).toEqual(["Goblin Welder"]);
    expect(deck.sideboard.map((e) => e.name)).toEqual(["Pyroblast"]);
    expect(deck.main.map((e) => e.name)).toEqual(["Sol Ring", "Command Tower", "Arcane Signet"]);
  });
});
