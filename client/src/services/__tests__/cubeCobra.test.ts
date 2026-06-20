import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchCubeList } from "../cubeCobra";

beforeEach(() => {
  vi.restoreAllMocks();
});

describe("fetchCubeList", () => {
  it("loads CubeCobra list pages through the CORS-enabled JSON API", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({
        cards: {
          mainboard: [
            { name: "Lightning Bolt" },
            { name: "Lightning Bolt" },
            { details: { name: "Heliod, Sun-Crowned" } },
            { name: "Counterspell" },
          ],
        },
      }),
    });

    await expect(fetchCubeList("https://cubecobra.com/cube/list/abc123")).resolves.toBe(
      "2 Lightning Bolt\n1 Heliod, Sun-Crowned\n1 Counterspell",
    );
    expect(global.fetch).toHaveBeenCalledWith("https://cubecobra.com/cube/api/cubeJSON/abc123");
  });

  it("annotates each card with its oracle id for resilient resolution", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({
        cards: {
          mainboard: [
            { name: "Makdee and Itla, Skysnarers", details: { oracle_id: "be2b9c6d-4ecb" } },
            { name: "Island", details: { oracle_id: "b2c8-island" } },
            { name: "Island", details: { oracle_id: "b2c8-island" } },
          ],
        },
      }),
    });

    // Stale/placeholder names still emit, but carry the oracle id so the engine
    // can resolve them by id when the name no longer matches a printed card.
    await expect(fetchCubeList("https://cubecobra.com/cube/list/abc123")).resolves.toBe(
      "1 Makdee and Itla, Skysnarers [be2b9c6d-4ecb]\n2 Island [b2c8-island]",
    );
  });

  it("keeps non-CubeCobra URLs as raw text exports", async () => {
    global.fetch = vi.fn().mockResolvedValue({
      ok: true,
      text: () => Promise.resolve("1 Black Lotus\n"),
    });

    await expect(fetchCubeList("https://example.com/cube.txt")).resolves.toBe("1 Black Lotus\n");
    expect(global.fetch).toHaveBeenCalledWith("https://example.com/cube.txt");
  });
});
