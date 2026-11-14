import { afterEach, describe, expect, it, vi } from "vitest";

async function loadDecksModule() {
  vi.resetModules();
  return import("../useDecks.ts");
}

function validDeckMap() {
  return {
    starter: {
      code: "S1",
      name: "Starter",
      type: "Commander Deck",
      coveragePct: 100,
      mainBoard: [{ name: "Island", count: 1 }],
    },
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("loadPreconDeckMap", () => {
  it("retries invalid loads, shares an in-flight retry, and keeps a successful map cached", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const { loadPreconDeckMap } = await loadDecksModule();

    fetchMock.mockRejectedValueOnce(new Error("offline"));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response("", { status: 503 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response("not json", { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response("null", { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response("[]", { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({}), { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
      starter: { ...validDeckMap().starter, code: 1 },
    }), { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
      starter: { ...validDeckMap().starter, releaseDate: 2026 },
    }), { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({
      starter: { ...validDeckMap().starter, sideBoard: [{ name: "Island", count: "1" }] },
    }), { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    let resolveFetch!: (response: Response) => void;
    fetchMock.mockReturnValueOnce(new Promise<Response>((resolve) => { resolveFetch = resolve; }));
    const first = loadPreconDeckMap();
    const second = loadPreconDeckMap();
    expect(second).toBe(first);
    expect(fetchMock).toHaveBeenCalledTimes(10);

    resolveFetch(new Response(JSON.stringify(validDeckMap()), { status: 200 }));
    await expect(first).resolves.toEqual(validDeckMap());
    await expect(second).resolves.toEqual(validDeckMap());
    await expect(loadPreconDeckMap()).resolves.toEqual(validDeckMap());
    expect(fetchMock).toHaveBeenCalledTimes(10);
  });
});
