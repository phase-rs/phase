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
