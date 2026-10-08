import { beforeEach, describe, expect, it, vi } from "vitest";

const idb = vi.hoisted(() => {
  const records = new Map<string, unknown>();
  return {
    records,
    createStore: vi.fn(() => ({})),
    del: vi.fn(async (key: string) => { records.delete(key); }),
    entries: vi.fn(async () => [...records.entries()]),
    set: vi.fn(async (key: string, value: unknown) => { records.set(key, value); }),
  };
});

vi.mock("idb-keyval", () => idb);

import { del as idbDel, entries as idbEntries, set as idbSet } from "idb-keyval";
import {
  _resetFeedCacheForTests,
  getCachedFeed,
  getFeedCacheState,
  hydrateFeedCache,
  refreshFeedCache,
  removeCachedFeed,
  setCachedFeed,
  subscribeFeedCache,
} from "../feedPersistence.ts";
import type { Feed } from "../../types/feed.ts";

function feed(id: string, version: number): Feed {
  return {
    id,
    name: id,
    version,
    updated: `2026-08-30T${String(version).padStart(2, "0")}:00:00Z`,
    decks: [],
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

beforeEach(() => {
  idb.records.clear();
  vi.clearAllMocks();
  idb.createStore.mockReturnValue({});
  idb.del.mockImplementation(async (key: string) => { idb.records.delete(key); });
  idb.entries.mockImplementation(async () => [...idb.records.entries()]);
  idb.set.mockImplementation(async (key: string, value: unknown) => { idb.records.set(key, value); });
  _resetFeedCacheForTests();
});

describe("refreshFeedCache", () => {
  it("replaces stale hydrated cache entries with the durable snapshot, including removals", async () => {
    const v1 = feed("alpha", 1);
    const removed = feed("removed", 1);
    await setCachedFeed("alpha", v1);
    await setCachedFeed("removed", removed);
    await hydrateFeedCache();

    const v2 = feed("alpha", 2);
    await idbSet("alpha", v2, {} as never);
    await idbDel("removed", {} as never);
    await refreshFeedCache();

    expect(getCachedFeed("alpha")).toBe(v2);
    expect(getCachedFeed("removed")).toBeNull();
  });

  it("accepts an empty durable snapshot without writing it back or rereading in a loop", async () => {
    const v1 = feed("alpha", 1);
    await setCachedFeed("alpha", v1);
    await hydrateFeedCache();
    idb.records.clear();
    const writesBeforeRefresh = vi.mocked(idbSet).mock.calls.length;

    await refreshFeedCache();

    expect(getCachedFeed("alpha")).toBeNull();
    expect(vi.mocked(idbSet)).toHaveBeenCalledTimes(writesBeforeRefresh);
    expect(vi.mocked(idbEntries)).toHaveBeenCalledTimes(2);
  });

  it("rejects a durable read without changing the in-memory cache", async () => {
    const v1 = feed("alpha", 1);
    await setCachedFeed("alpha", v1);
    await hydrateFeedCache();
    vi.mocked(idbEntries).mockRejectedValueOnce(new Error("IDB unavailable"));

    await expect(refreshFeedCache()).rejects.toThrow("IDB unavailable");

    expect(getCachedFeed("alpha")).toBe(v1);
  });

  it("keeps a same-tab set made while the durable read is pending", async () => {
    const old = feed("old", 1);
    await setCachedFeed("old", old);
    await hydrateFeedCache();
    const reading = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries).mockReturnValueOnce(reading.promise);

    const refresh = refreshFeedCache();
    const local = feed("local", 3);
    await setCachedFeed("local", local);
    const durable = feed("durable", 2);
    reading.resolve([["durable", durable]]);
    await refresh;

    expect(getCachedFeed("local")).toBe(local);
    expect(getCachedFeed("durable")).toBe(durable);
    expect(getCachedFeed("old")).toBeNull();
  });

  it("keeps a same-tab removal made while the durable read is pending", async () => {
    const stale = feed("stale", 1);
    await setCachedFeed("stale", stale);
    await hydrateFeedCache();
    const reading = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries).mockReturnValueOnce(reading.promise);

    const refresh = refreshFeedCache();
    removeCachedFeed("stale");
    const durable = feed("durable", 2);
    reading.resolve([["stale", stale], ["durable", durable]]);
    await refresh;

    expect(getCachedFeed("stale")).toBeNull();
    expect(getCachedFeed("durable")).toBe(durable);
  });
});

describe("imperative feed-cache observation", () => {
  it("exposes the existing cache state and subscribes without a React render", async () => {
    const observed = vi.fn();
    const unsubscribe = subscribeFeedCache(observed);
    const current = feed("current", 3);

    await setCachedFeed("current", current);

    expect(getFeedCacheState()).toMatchObject({ cache: { current }, hydrated: false });
    expect(observed).toHaveBeenCalledWith(
      expect.objectContaining({ cache: { current }, hydrated: false }),
      expect.objectContaining({ cache: {}, hydrated: false }),
    );
    unsubscribe();
    removeCachedFeed("current");
    expect(observed).toHaveBeenCalledTimes(1);
  });
});

describe("hydrateFeedCache", () => {
  it("shares one durable read and preserves a cache write made before hydration settles", async () => {
    const reading = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries).mockReturnValueOnce(reading.promise);

    const first = hydrateFeedCache();
    const successor = hydrateFeedCache();
    const current = feed("current", 3);
    const currentWrite = setCachedFeed("current", current);
    const durable = feed("durable", 2);
    reading.resolve([["current", feed("current", 1)], ["durable", durable]]);

    await Promise.all([first, successor, currentWrite]);

    expect(vi.mocked(idbEntries)).toHaveBeenCalledTimes(1);
    expect(getCachedFeed("current")).toBe(current);
    expect(getCachedFeed("durable")).toBe(durable);
  });

  it("does not let a mode-style successor overwrite a newer cache publication", async () => {
    const reading = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries).mockReturnValueOnce(reading.promise);

    const older = hydrateFeedCache();
    const successor = hydrateFeedCache();
    const refreshed = feed("starter", 4);
    const write = setCachedFeed("starter", refreshed);
    reading.resolve([["starter", feed("starter", 1)]]);

    await Promise.all([older, successor, write]);

    expect(getCachedFeed("starter")).toBe(refreshed);
  });
});
