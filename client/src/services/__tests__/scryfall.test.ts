import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

function makeLocalDataMap(
  cards: Record<string, { name: string; mana_cost?: string; cmc?: number; type_line?: string }>,
): Response {
  const map: Record<string, unknown> = {};
  for (const [key, card] of Object.entries(cards)) {
    map[key.toLowerCase()] = {
      name: card.name,
      mana_cost: card.mana_cost ?? "{1}",
      cmc: card.cmc ?? 1,
      type_line: card.type_line ?? "Instant",
      colors: [],
      color_identity: [],
      keywords: [],
      faces: [
        {
          normal: `https://img.example/${encodeURIComponent(card.name)}.jpg`,
          art_crop: `https://img.example/${encodeURIComponent(card.name)}-art.jpg`,
        },
      ],
    };
  }
  return new Response(JSON.stringify(map), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

function makeEmptyCardDataMap(): Response {
  return new Response(JSON.stringify({}), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

async function loadScryfallModule() {
  vi.resetModules();
  return import("../scryfall.ts");
}

describe("normalizeCardName", () => {
  it("strips set code brackets", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Goblin Lackey [UZ]")).toBe("Goblin Lackey");
  });

  it("strips angle-bracket treatment tags", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Abrade <retro>")).toBe("Abrade");
    expect(normalizeCardName("Kiki-Jiki, Mirror Breaker <timeshifted>")).toBe(
      "Kiki-Jiki, Mirror Breaker",
    );
  });

  it("strips collector numbers in angle brackets", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Mountain <288>")).toBe("Mountain");
  });

  it("strips foil markers", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Goblin Rabblemaster [PRM-BAB] (F)")).toBe(
      "Goblin Rabblemaster",
    );
  });

  it("strips combined decorators", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(
      normalizeCardName("Krenko, Mob Boss <retro> [RVR] (F)"),
    ).toBe("Krenko, Mob Boss");
  });

  it("leaves plain card names unchanged", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Lightning Bolt")).toBe("Lightning Bolt");
  });
});

describe("scryfallLegalityKey", () => {
  it("uses Scryfall legality keys for constructed formats", async () => {
    const { scryfallLegalityKey } = await loadScryfallModule();

    expect(scryfallLegalityKey("Modern")).toBe("modern");
    expect(scryfallLegalityKey("Premodern")).toBe("premodern");
  });

  it("maps commander variants to Scryfall legality keys", async () => {
    const { scryfallLegalityKey } = await loadScryfallModule();

    expect(scryfallLegalityKey("Brawl")).toBe("standardbrawl");
    expect(scryfallLegalityKey("HistoricBrawl")).toBe("brawl");
    expect(scryfallLegalityKey("DuelCommander")).toBe("duel");
    expect(scryfallLegalityKey("PauperCommander")).toBe("paupercommander");
  });

  it("returns undefined for formats without a Scryfall legality key", async () => {
    const { scryfallLegalityKey } = await loadScryfallModule();

    expect(scryfallLegalityKey("TinyLeaders")).toBeUndefined();
    expect(scryfallLegalityKey("FreeForAll")).toBeUndefined();
  });
});

describe("fetchCardData", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns card data from local JSON", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        "lightning bolt": { name: "Lightning Bolt" },
      }),
    );

    const { fetchCardData } = await loadScryfallModule();
    const card = await fetchCardData("Lightning Bolt");

    expect(card.name).toBe("Lightning Bolt");
    // Only the local data fetch — no API calls
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("throws when card is not in local data (no API fallback)", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeEmptyCardDataMap());

    const { fetchCardData } = await loadScryfallModule();
    await expect(fetchCardData("Nonexistent Card")).rejects.toThrow(
      /not in local data/,
    );

    // Only the local data fetch — no API calls
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("normalizes decorated names before local lookup", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        abrade: { name: "Abrade" },
      }),
    );

    const { fetchCardData } = await loadScryfallModule();
    const card = await fetchCardData("Abrade <retro>");

    expect(card.name).toBe("Abrade");
  });
});

describe("fetchCardImageUrl", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns image URL from local data", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        "lightning bolt": { name: "Lightning Bolt" },
      }),
    );

    const { fetchCardImageUrl } = await loadScryfallModule();
    const url = await fetchCardImageUrl("Lightning Bolt", 0, "normal");

    expect(url).toBe("https://img.example/Lightning%20Bolt.jpg");
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("throws when card image is not in local data (no API fallback)", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeEmptyCardDataMap());

    const { fetchCardImageUrl } = await loadScryfallModule();
    await expect(
      fetchCardImageUrl("Nonexistent Card", 0, "normal"),
    ).rejects.toThrow(/not in local data/);

    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("normalizes decorated names for image lookup", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        mountain: { name: "Mountain" },
      }),
    );

    const { fetchCardImageUrl } = await loadScryfallModule();
    const url = await fetchCardImageUrl("Mountain <288>", 0, "art_crop");

    expect(url).toBe("https://img.example/Mountain-art.jpg");
  });
});

describe("fetchTokenImageUrl — ability-aware printing selection (issue #502)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  // A Scryfall token-search response whose first hit is a vanilla 1/1 Human.
  function makeTokenSearchResponse(): Response {
    return new Response(
      JSON.stringify({
        data: [{
          name: "Human Token",
          keywords: [],
          image_uris: { normal: "https://img.example/vanilla-human.jpg" },
        }],
        total_cards: 1,
        has_more: false,
      }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  }

  function make404(): Response {
    return new Response("", { status: 404 });
  }

  // Decode every captured search URL's `q=` query string. The first fetch
  // call is always the local Scryfall-data load; search calls follow.
  function capturedQueries(fetchMock: ReturnType<typeof vi.fn>): string[] {
    return fetchMock.mock.calls
      .map((c) => String(c[0]))
      .filter((u) => u.includes("/cards/search?"))
      .map((u) => decodeURIComponent(new URL(u).searchParams.get("q") ?? ""));
  }

  it("Test 1 — a vanilla token query carries is:vanilla", async () => {
    const fetchMock = vi
      .fn()
      // Token-less local data map — forces the API path (no `token:human` key).
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      .mockResolvedValue(makeTokenSearchResponse());
    global.fetch = fetchMock;

    const { fetchTokenImageUrl } = await loadScryfallModule();
    await fetchTokenImageUrl("Human", "normal", {
      power: 1,
      toughness: 1,
      colors: ["White"],
      subtypes: ["Human"],
      hasAbilities: false,
    });

    const queries = capturedQueries(fetchMock);
    expect(queries.length).toBeGreaterThan(0);
    expect(queries[0]).toContain("is:vanilla");
  });

  it("Test 2 — is:vanilla is added only when hasAbilities === false", async () => {
    // Each sub-case re-loads the module so the module-level `loadScryfallData`
    // cache is reset and the leading empty-card-data fetch is consumed afresh.

    // hasAbilities: false → query contains is:vanilla.
    {
      const { fetchTokenImageUrl } = await loadScryfallModule();
      const falseMock = vi
        .fn()
        .mockResolvedValueOnce(makeEmptyCardDataMap())
        .mockResolvedValue(makeTokenSearchResponse());
      global.fetch = falseMock;
      await fetchTokenImageUrl("Human", "normal", {
        power: 1, toughness: 1, colors: ["White"], subtypes: ["Human"],
        hasAbilities: false,
      });
      expect(capturedQueries(falseMock)[0]).toContain("is:vanilla");
    }

    // hasAbilities: true (e.g. a Spirit with flying) → NO is:vanilla.
    {
      const { fetchTokenImageUrl } = await loadScryfallModule();
      const trueMock = vi
        .fn()
        .mockResolvedValueOnce(makeEmptyCardDataMap())
        .mockResolvedValue(makeTokenSearchResponse());
      global.fetch = trueMock;
      await fetchTokenImageUrl("Spirit", "normal", {
        power: 1, toughness: 1, colors: ["White"], subtypes: ["Spirit"],
        hasAbilities: true,
      });
      const queries = capturedQueries(trueMock);
      expect(queries.length).toBeGreaterThan(0);
      for (const q of queries) {
        expect(q).not.toContain("is:vanilla");
      }
    }

    // hasAbilities omitted (preview / no-GameObject path) → NO is:vanilla.
    {
      const { fetchTokenImageUrl } = await loadScryfallModule();
      const undefMock = vi
        .fn()
        .mockResolvedValueOnce(makeEmptyCardDataMap())
        .mockResolvedValue(makeTokenSearchResponse());
      global.fetch = undefMock;
      await fetchTokenImageUrl("Human", "normal", {
        power: 1, toughness: 1, colors: ["White"], subtypes: ["Human"],
      });
      const queries = capturedQueries(undefMock);
      expect(queries.length).toBeGreaterThan(0);
      for (const q of queries) {
        expect(q).not.toContain("is:vanilla");
      }
    }
  });

  it("Test 3 — a vanilla-narrowed query resolves to a vanilla printing", async () => {
    global.fetch = vi
      .fn()
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      .mockResolvedValue(makeTokenSearchResponse());

    const { fetchTokenImageUrl } = await loadScryfallModule();
    const url = await fetchTokenImageUrl("Human", "normal", {
      power: 1,
      toughness: 1,
      colors: ["White"],
      subtypes: ["Human"],
      hasAbilities: false,
    });

    expect(url).toBe("https://img.example/vanilla-human.jpg");
  });

  it("Test 4 — a 404 on the first is:vanilla rung advances to the next rung", async () => {
    global.fetch = vi
      .fn()
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      // First (narrowest) is:vanilla rung 404s — an empty Scryfall search.
      .mockResolvedValueOnce(make404())
      // The next relaxed rung yields the vanilla hit.
      .mockResolvedValue(makeTokenSearchResponse());

    const { fetchTokenImageUrl } = await loadScryfallModule();
    const url = await fetchTokenImageUrl("Human", "normal", {
      power: 1,
      toughness: 1,
      colors: ["White"],
      subtypes: ["Human"],
      hasAbilities: false,
    });

    expect(url).toBe("https://img.example/vanilla-human.jpg");
  });
});

describe("rateLimitedFetch (token/search API)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("retries on network error with backoff", async () => {
    vi.useFakeTimers();

    const tokenResponse = new Response(
      JSON.stringify({
        data: [{
          name: "Goblin Token",
          image_uris: { normal: "https://img.example/goblin.jpg" },
        }],
        total_cards: 1,
        has_more: false,
      }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );

    global.fetch = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("Failed to fetch"))
      .mockResolvedValueOnce(tokenResponse);

    const { fetchTokenImageUrl } = await loadScryfallModule();
    const pending = fetchTokenImageUrl("Goblin", "normal");

    await vi.advanceTimersByTimeAsync(2000);
    const url = await pending;

    expect(url).toBe("https://img.example/goblin.jpg");
    expect(global.fetch).toHaveBeenCalledTimes(2);
  });
});
