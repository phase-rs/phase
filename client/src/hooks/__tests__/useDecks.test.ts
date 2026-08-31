import { afterEach, describe, expect, it, vi } from "vitest";

import { loadPreconDeckMap } from "../useDecks.ts";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("loadPreconDeckMap", () => {
  it("retries unavailable loads, shares an in-flight retry, and keeps a successful empty map cached", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    fetchMock.mockRejectedValueOnce(new Error("offline"));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response("", { status: 503 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    fetchMock.mockResolvedValueOnce(new Response("not json", { status: 200 }));
    await expect(loadPreconDeckMap()).resolves.toBeNull();

    let resolveFetch!: (response: Response) => void;
    fetchMock.mockReturnValueOnce(new Promise<Response>((resolve) => { resolveFetch = resolve; }));
    const first = loadPreconDeckMap();
    const second = loadPreconDeckMap();
    expect(second).toBe(first);
    expect(fetchMock).toHaveBeenCalledTimes(4);

    resolveFetch(new Response(JSON.stringify({}), { status: 200 }));
    await expect(first).resolves.toEqual({});
    await expect(second).resolves.toEqual({});
    await expect(loadPreconDeckMap()).resolves.toEqual({});
    expect(fetchMock).toHaveBeenCalledTimes(4);
  });
});
