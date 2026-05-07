import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DeckCompatibilityResult } from "../deckCompatibility";
import type { ParsedDeck } from "../deckParser";
import { evaluateDeckCompatibility } from "../deckCompatibility";
import { buildLegalAiDeckCatalog } from "../aiDeckCatalog";
import { getCachedFeed, listSubscriptions } from "../feedService";
import { loadPreconDeckMap } from "../../hooks/useDecks";
import { FEED_DECK_ORIGINS_KEY, STORAGE_KEY_PREFIX } from "../../constants/storage";

vi.mock("../deckCompatibility", () => ({
  evaluateDeckCompatibility: vi.fn(),
}));

vi.mock("../feedService", () => ({
  feedDeckToParsedDeck: vi.fn((deck: { main: ParsedDeck["main"]; sideboard?: ParsedDeck["sideboard"]; commander?: string[] }) => ({
    main: deck.main,
    sideboard: deck.sideboard ?? [],
    commander: deck.commander,
  })),
  getCachedFeed: vi.fn(),
  listSubscriptions: vi.fn(),
}));

vi.mock("../../hooks/useDecks", () => ({
  loadPreconDeckMap: vi.fn(),
  isCommanderPreconDeck: (deck: { type: string }) => deck.type === "Commander Deck",
}));

function deck(firstCard: string, commander?: string): ParsedDeck {
  return {
    main: [{ count: 1, name: firstCard }],
    sideboard: [],
    commander: commander ? [commander] : undefined,
  };
}

function compatibility(legal: boolean): DeckCompatibilityResult {
  return {
    standard: { compatible: legal, reasons: [] },
    commander: { compatible: legal, reasons: [] },
    bo3_ready: true,
    unknown_cards: [],
    selected_format_compatible: legal,
    selected_format_reasons: legal ? [] : ["Illegal"],
    color_identity: [],
    coverage: { total_unique: 10, supported_unique: 9, unsupported_cards: [] },
  };
}

function saveDeck(name: string, parsed: ParsedDeck): void {
  localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(parsed));
}

beforeEach(() => {
  localStorage.clear();
  vi.mocked(listSubscriptions).mockReturnValue([]);
  vi.mocked(getCachedFeed).mockReturnValue(null);
  vi.mocked(loadPreconDeckMap).mockResolvedValue(null);
  vi.mocked(evaluateDeckCompatibility).mockImplementation(async (parsed) =>
    compatibility(parsed.main[0]?.name !== "Illegal Starter")
  );
});

describe("buildLegalAiDeckCatalog", () => {
  it("includes legal saved Pauper Commander user decks", async () => {
    saveDeck("PDH Legal", deck("Command Tower", "Murmuring Mystic"));

    const catalog = await buildLegalAiDeckCatalog({
      selectedFormat: "PauperCommander",
      selectedMatchType: "Bo1",
    });

    expect(catalog.candidates.map((candidate) => candidate.id)).toContain("saved:PDH Legal");
    expect(evaluateDeckCompatibility).toHaveBeenCalledWith(
      expect.objectContaining({ commander: ["Murmuring Mystic"] }),
      { selectedFormat: "PauperCommander", selectedMatchType: "Bo1", summaryOnly: true },
    );
  });

  it("dedupes mirrored feed decks while preserving same-name decks from distinct feeds", async () => {
    saveDeck("Mirrored Deck", deck("Mirrored Card"));
    localStorage.setItem(FEED_DECK_ORIGINS_KEY, JSON.stringify({ "Mirrored Deck": "feed-a" }));
    vi.mocked(listSubscriptions).mockReturnValue([
      { sourceId: "feed-a", url: "feed-a.json", type: "remote", subscribedAt: 0, lastRefreshedAt: 0, lastVersion: 1 },
      { sourceId: "feed-b", url: "feed-b.json", type: "remote", subscribedAt: 0, lastRefreshedAt: 0, lastVersion: 1 },
      { sourceId: "starter", url: "starter.json", type: "bundled", subscribedAt: 0, lastRefreshedAt: 0, lastVersion: 1 },
    ]);
    vi.mocked(getCachedFeed).mockImplementation((feedId) => {
      if (feedId === "feed-a") {
        return {
          id: "feed-a",
          name: "Feed A",
          version: 1,
          updated: "2026-05-06T00:00:00Z",
          decks: [
            { name: "Mirrored Deck", colors: [], main: deck("Mirrored Card").main, sideboard: [] },
            { name: "Same Name", colors: [], main: deck("Feed A Card").main, sideboard: [] },
          ],
        };
      }
      if (feedId === "feed-b") {
        return {
          id: "feed-b",
          name: "Feed B",
          version: 1,
          updated: "2026-05-06T00:00:00Z",
          decks: [
            { name: "Same Name", colors: [], main: deck("Feed B Card").main, sideboard: [] },
          ],
        };
      }
      return {
        id: "starter",
        name: "Starter",
        version: 1,
        updated: "2026-05-06T00:00:00Z",
        decks: [
          { name: "Illegal Starter", colors: [], main: deck("Illegal Starter").main, sideboard: [] },
        ],
      };
    });

    const catalog = await buildLegalAiDeckCatalog({
      selectedFormat: "Standard",
      selectedMatchType: "Bo1",
    });
    const ids = catalog.candidates.map((candidate) => candidate.id);

    expect(ids).toContain("saved:Mirrored Deck");
    expect(ids).not.toContain("feed:feed-a:Mirrored Deck");
    expect(ids).toContain("feed:feed-a:Same Name");
    expect(ids).toContain("feed:feed-b:Same Name");
    expect(ids).not.toContain("feed:starter:Illegal Starter");
  });

  it("checks legality for same-format Starter Decks before adding them to the AI pool", async () => {
    vi.mocked(listSubscriptions).mockReturnValue([
      { sourceId: "starter-decks", url: "/feeds/starter-decks.json", type: "bundled", subscribedAt: 0, lastRefreshedAt: 0, lastVersion: 1 },
    ]);
    vi.mocked(getCachedFeed).mockReturnValue({
      id: "starter-decks",
      name: "Starter Decks",
      format: "standard",
      version: 1,
      updated: "2026-05-06T00:00:00Z",
      decks: [
        { name: "Illegal Starter", colors: [], main: deck("Illegal Starter").main, sideboard: [] },
      ],
    });

    const catalog = await buildLegalAiDeckCatalog({
      selectedFormat: "Standard",
      selectedMatchType: "Bo1",
    });

    expect(catalog.candidates.map((candidate) => candidate.id)).not.toContain(
      "feed:starter-decks:Illegal Starter",
    );
    expect(evaluateDeckCompatibility).toHaveBeenCalledWith(
      expect.objectContaining({ main: [{ count: 1, name: "Illegal Starter" }] }),
      { selectedFormat: "Standard", selectedMatchType: "Bo1", summaryOnly: true },
    );
  });

  it("exposes Commander precons from shared catalog metadata without engine compatibility", async () => {
    vi.mocked(loadPreconDeckMap).mockResolvedValue({
      secrets: {
        code: "SOS",
        name: "Secrets of Strixhaven",
        type: "Commander Deck",
        coveragePct: 100,
        mainBoard: deck("Precon Legal Card").main,
        commander: [{ count: 1, name: "Zimone, Mystery Unraveler" }],
      },
      starter: {
        code: "STD",
        name: "Illegal Starter",
        type: "Starter",
        coveragePct: 100,
        mainBoard: deck("Illegal Starter").main,
      },
    });

    const callsBefore = vi.mocked(evaluateDeckCompatibility).mock.calls.length;
    const catalog = await buildLegalAiDeckCatalog({
      selectedFormat: "Commander",
      selectedMatchType: "Bo1",
    });
    const ids = catalog.candidates.map((candidate) => candidate.id);

    expect(ids).toContain("precon:secrets");
    expect(ids).not.toContain("precon:starter");
    expect(vi.mocked(evaluateDeckCompatibility).mock.calls).toHaveLength(callsBefore);
  });
});
