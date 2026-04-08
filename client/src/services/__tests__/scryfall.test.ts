import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

function makeCardResponse(name: string): Response {
  return new Response(
    JSON.stringify({
      id: `${name}-id`,
      name,
      mana_cost: "{1}",
      cmc: 1,
      type_line: "Instant",
      color_identity: [],
      legalities: {},
      image_uris: {
        normal: `https://img.example/${encodeURIComponent(name)}.jpg`,
      },
    }),
    {
      status: 200,
      headers: { "Content-Type": "application/json" },
    },
  );
}

/** Response for /scryfall-data.json — empty map so tests exercise the API path. */
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

describe("scryfall service", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("deduplicates concurrent card lookups for the same name", async () => {
    global.fetch = vi
      .fn()
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      .mockResolvedValue(makeCardResponse("Lightning Bolt"));

    const { fetchCardData } = await loadScryfallModule();
    const [first, second] = await Promise.all([
      fetchCardData("Lightning Bolt"),
      fetchCardData("Lightning Bolt"),
    ]);

    expect(first.name).toBe("Lightning Bolt");
    expect(second.name).toBe("Lightning Bolt");
    // 1 for local card data map + 1 for API lookup (deduplicated)
    expect(global.fetch).toHaveBeenCalledTimes(2);
    expect(global.fetch).toHaveBeenCalledWith(
      "https://api.scryfall.com/cards/named?exact=Lightning%20Bolt",
    );
  });

  it("retries when fetch is rejected before the browser exposes the status code", async () => {
    vi.useFakeTimers();
    global.fetch = vi
      .fn()
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      .mockRejectedValueOnce(new TypeError("Failed to fetch"))
      .mockResolvedValueOnce(makeCardResponse("Counterspell"));

    const { fetchCardData } = await loadScryfallModule();
    const pending = fetchCardData("Counterspell");

    await vi.advanceTimersByTimeAsync(1000);
    const card = await pending;

    expect(card.name).toBe("Counterspell");
    // 1 for local map + 1 rejected + 1 retry
    expect(global.fetch).toHaveBeenCalledTimes(3);
  });

  it("serializes Scryfall requests so concurrent misses do not burst", async () => {
    vi.useFakeTimers();

    let inFlight = 0;
    let maxInFlight = 0;
    const resolvers: Array<(response: Response) => void> = [];
    let callIndex = 0;

    global.fetch = vi.fn(() => {
      const idx = callIndex++;
      // First call is for local card data map — resolve immediately
      if (idx === 0) {
        return Promise.resolve(makeEmptyCardDataMap());
      }
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      return new Promise<Response>((resolve) => {
        resolvers.push((response) => {
          inFlight -= 1;
          resolve(response);
        });
      });
    });

    const { fetchCardData } = await loadScryfallModule();
    const first = fetchCardData("Lightning Bolt");
    const second = fetchCardData("Counterspell");

    await vi.advanceTimersByTimeAsync(0);
    // Local map (resolved) + first API call
    expect(global.fetch).toHaveBeenCalledTimes(2);

    resolvers.shift()!(makeCardResponse("Lightning Bolt"));
    await vi.advanceTimersByTimeAsync(0);
    expect(global.fetch).toHaveBeenCalledTimes(2);

    await vi.advanceTimersByTimeAsync(100);
    expect(global.fetch).toHaveBeenCalledTimes(3);

    resolvers.shift()!(makeCardResponse("Counterspell"));
    await Promise.all([first, second]);

    expect(maxInFlight).toBe(1);
  });
});
