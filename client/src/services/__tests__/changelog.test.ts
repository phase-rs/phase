import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The service caches meta/entries at module scope. Reset modules before each
// test so every case starts from a clean cache, and re-import dynamically.
async function load() {
  vi.resetModules();
  return import("../changelog");
}

function mockFetch(impl: (url: string) => Partial<Response> & { json?: () => Promise<unknown> }) {
  vi.stubGlobal("fetch", vi.fn((url: string) => Promise.resolve(impl(url) as Response)));
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("computeHasUnread", () => {
  let computeHasUnread: typeof import("../changelog").computeHasUnread;

  beforeEach(async () => {
    ({ computeHasUnread } = await load());
  });

  it("is false when there is no published latest id", () => {
    expect(computeHasUnread(null, 5)).toBe(false);
    expect(computeHasUnread(undefined, 5)).toBe(false);
  });

  it("is false for a first-run user with no watermark (caller seeds it)", () => {
    expect(computeHasUnread(7, null)).toBe(false);
    expect(computeHasUnread(7, undefined)).toBe(false);
  });

  it("is true only when the latest id is strictly newer than the watermark", () => {
    expect(computeHasUnread(8, 7)).toBe(true);
    expect(computeHasUnread(7, 7)).toBe(false);
    expect(computeHasUnread(6, 7)).toBe(false);
  });
});

describe("fetchChangelogMeta", () => {
  it("returns the parsed pointer on success", async () => {
    mockFetch(() => ({ ok: true, json: () => Promise.resolve({ latestId: 12 }) }));
    const { fetchChangelogMeta } = await load();
    expect(await fetchChangelogMeta()).toEqual({ latestId: 12 });
  });

  it("resolves to null on a non-ok response (no error surfaced)", async () => {
    mockFetch(() => ({ ok: false, status: 404 }));
    const { fetchChangelogMeta } = await load();
    expect(await fetchChangelogMeta()).toBeNull();
  });

  it("resolves to null when the network rejects", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new Error("offline"))));
    const { fetchChangelogMeta } = await load();
    expect(await fetchChangelogMeta()).toBeNull();
  });
});

describe("fetchChangelogEntries", () => {
  it("returns entries on success and caches the result", async () => {
    const fetchSpy = vi.fn(() =>
      Promise.resolve({
        ok: true,
        json: () => Promise.resolve({ entries: [{ id: 2 }, { id: 1 }] }),
      } as Response),
    );
    vi.stubGlobal("fetch", fetchSpy);
    const { fetchChangelogEntries } = await load();

    const first = await fetchChangelogEntries();
    expect(first.map((e) => e.id)).toEqual([2, 1]);

    // Second call is served from cache — no second network round-trip.
    await fetchChangelogEntries();
    expect(fetchSpy).toHaveBeenCalledTimes(1);
  });

  it("throws on a failed fetch so the caller leaves the watermark intact", async () => {
    // This is the contract the unread watermark depends on: the hook only
    // advances `lastSeenChangelogId` after a SUCCESSFUL load. A throw here is
    // what keeps the dot lit (for a retry) when the load fails.
    mockFetch(() => ({ ok: false, status: 500 }));
    const { fetchChangelogEntries } = await load();
    await expect(fetchChangelogEntries()).rejects.toThrow();
  });
});
