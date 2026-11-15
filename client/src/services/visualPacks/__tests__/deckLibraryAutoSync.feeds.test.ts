// @vitest-environment jsdom

import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
const platform = vi.hoisted(() => ({ load: vi.fn() }));
const scryfall = vi.hoisted(() => ({
  cards: {} as Record<string, unknown>,
  printings: {} as Record<string, unknown>,
  oracleIds: new Map<string, string>(),
}));

vi.mock("idb-keyval", () => idb);
vi.mock("../../platform.ts", () => ({ loadVisualPackBackend: platform.load }));
vi.mock("../../scryfall.ts", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../scryfall.ts")>(),
  loadScryfallData: vi.fn(async () => scryfall.cards),
  loadPrintingsData: vi.fn(async () => scryfall.printings),
  resolveOracleIdSync: vi.fn((name: string) => scryfall.oracleIds.get(name) ?? null),
}));
vi.mock("../../../hooks/useDecks.ts", () => ({
  isCommanderPreconDeck: (deck: { type?: string }) => deck.type === "Commander Deck",
  loadPreconDeckMap: vi.fn(async () => ({})),
}));

import { entries as idbEntries, set as idbSet } from "idb-keyval";
import { FEED_SUBSCRIPTIONS_KEY, PREFERENCES_KEY } from "../../../constants/storage.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { Feed, FeedSubscription } from "../../../types/feed.ts";
import {
  _resetFeedCacheForTests,
  getCachedFeed,
  hydrateFeedCache,
  setCachedFeed,
} from "../../feedPersistence.ts";
import { prepareDeckLibraryForOffline, useDeckLibraryAutoSync } from "../deckLibraryAutoSync.ts";
import { invalidateDeckLibraryPack, planDeckLibraryPack } from "../deckLibraryPack.ts";
import { packId } from "../types.ts";

const backend = {
  reconcileDeckLibrary: vi.fn(async () => {}),
  setDeckLibraryBackgroundPaused: vi.fn(async () => {}),
  prepareDeckLibraryForOffline: vi.fn(async () => "ready" as const),
};

function feed(id: string, decks: string[]): Feed {
  return {
    id,
    name: id,
    format: "commander",
    version: 1,
    updated: "2026-08-30T00:00:00Z",
    decks: decks.map((name) => ({
      name,
      colors: [],
      main: [{ count: 1, name }],
      sideboard: [],
    })),
  };
}

function subscription(sourceId: string): FeedSubscription {
  return {
    sourceId,
    url: `https://example.test/${sourceId}.json`,
    type: "remote",
    subscribedAt: 1,
    lastRefreshedAt: 1,
    lastVersion: 1,
  };
}

function descriptorKeys(membership: Awaited<ReturnType<typeof planDeckLibraryPack>>): string[] {
  return membership.descriptors.map((descriptor) => String(descriptor.assetKey));
}

function configureCardData(cards: Array<[name: string, oracleId: string]>, includePrintings = false): void {
  scryfall.oracleIds = new Map(cards);
  scryfall.cards = Object.fromEntries(cards.map(([name, oracleId]) => [oracleId, {
    oracle_id: oracleId,
    name,
    face_names: [name.toLowerCase()],
    faces: [{ normal: `https://cards.test/${oracleId}.jpg`, art_crop: `https://cards.test/${oracleId}-crop.jpg` }],
  }]));
  scryfall.printings = includePrintings
    ? Object.fromEntries(cards.map(([name, oracleId], index) => [oracleId, [{
      id: `${String(index + 1).repeat(8)}-aaaa-4aaa-8aaa-aaaaaaaaaaaa`,
      set: "m26",
      set_name: "Modern Horizons 26",
      collector_number: String(index + 1),
      released_at: "2026-01-01",
      border_color: "black",
      frame_effects: [],
      full_art: false,
      faces: [{ normal: `https://cards.test/printing-${oracleId}.jpg`, art_crop: `https://cards.test/printing-${oracleId}-crop.jpg` }],
      name,
    }]]))
    : {};
}

async function flush(): Promise<void> {
  await act(async () => { await vi.advanceTimersByTimeAsync(500); });
}

function observedPlan() {
  let resolve!: (membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void;
  const settled = new Promise<Awaited<ReturnType<typeof planDeckLibraryPack>>>((done) => {
    resolve = done;
  });
  const observed = vi.fn<(membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void>(resolve);
  return { observed, settled };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  idb.records.clear();
  vi.clearAllMocks();
  idb.createStore.mockReturnValue({});
  idb.del.mockImplementation(async (key: string) => { idb.records.delete(key); });
  idb.entries.mockImplementation(async () => [...idb.records.entries()]);
  idb.set.mockImplementation(async (key: string, value: unknown) => { idb.records.set(key, value); });
  _resetFeedCacheForTests();
  invalidateDeckLibraryPack();
  platform.load.mockResolvedValue(backend);
  backend.reconcileDeckLibrary.mockReset();
  backend.reconcileDeckLibrary.mockResolvedValue(undefined);
  backend.setDeckLibraryBackgroundPaused.mockReset();
  backend.setDeckLibraryBackgroundPaused.mockResolvedValue(undefined);
  backend.prepareDeckLibraryForOffline.mockReset();
  backend.prepareDeckLibraryForOffline.mockResolvedValue("ready");
  scryfall.cards = {};
  scryfall.printings = {};
  scryfall.oracleIds = new Map();
  usePreferencesStore.setState({ artChain: [], artOverrides: {} });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useDeckLibraryAutoSync feed freshness", () => {
  it("completes preparation once when real freshness notifications publish new preferences and feeds", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementation(async () => {
      usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    });
    await idbSet("preparation", feed("preparation", []), {} as never);

    const request = prepareDeckLibraryForOffline();
    await flush();

    await expect(request).resolves.toBe("ready");
    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("falls back to the hydrated feed cache when the startup freshness read fails", async () => {
    await setCachedFeed("source", feed("source", ["Old Deck"]));
    await hydrateFeedCache();
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    vi.mocked(idbEntries).mockRejectedValueOnce(new Error("offline IDB"));

    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect(warning).toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("keeps a newer external feed request pending when the startup read falls back", async () => {
    await setCachedFeed("source", feed("source", ["Old Deck"]));
    await hydrateFeedCache();
    vi.mocked(idbEntries).mockClear();
    const firstRead = deferred<Array<[IDBValidKey, Feed]>>();
    const secondRead = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries)
      .mockReturnValueOnce(firstRead.promise)
      .mockReturnValueOnce(secondRead.promise);

    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    firstRead.reject(new Error("offline IDB"));
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect(vi.mocked(idbEntries)).toHaveBeenCalledTimes(2);
    secondRead.resolve([["source", feed("source", ["New Deck"])]]);
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(2);
    mounted.unmount();
  });

  it("plans durable feed updates rather than stale hydrated feed candidates", async () => {
    const oldOracle = "11111111-1111-4111-8111-111111111111";
    const newOracle = "22222222-2222-4222-8222-222222222222";
    const addedOracle = "33333333-3333-4333-8333-333333333333";
    configureCardData([["Old Deck", oldOracle], ["New Deck", newOracle], ["Added Deck", addedOracle]]);

    await setCachedFeed("source", feed("source", ["Old Deck"]));
    await hydrateFeedCache();
    await idbSet("source", feed("source", ["New Deck"]), {} as never);
    await idbSet("added", feed("added", ["Added Deck"]), {} as never);
    localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify([subscription("source"), subscription("added")]));

    const plan = observedPlan();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    platform.load.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      plan.observed(await planDeckLibraryPack(packId("deck_library")));
    });

    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    const membership = await plan.settled;
    expect(plan.observed).toHaveBeenCalledTimes(1);
    expect(descriptorKeys(membership)).toEqual(expect.arrayContaining([
      `asset:v1:canonical_card:${newOracle}-0-full_card-normal`,
      `asset:v1:canonical_card:${addedOracle}-0-full_card-normal`,
    ]));
    expect(descriptorKeys(membership))
      .not.toContain(`asset:v1:canonical_card:${oldOracle}-0-full_card-normal`);
    expect(getCachedFeed("source")?.decks.map((deck) => deck.name)).toEqual(["New Deck"]);
    expect(getCachedFeed("added")?.decks.map((deck) => deck.name)).toEqual(["Added Deck"]);
    mounted.unmount();
  });

  it("skips stale feed planning after a refresh failure and retries when online", async () => {
    const oldOracle = "11111111-1111-4111-8111-111111111111";
    const newOracle = "22222222-2222-4222-8222-222222222222";
    configureCardData([["Old Deck", oldOracle], ["New Deck", newOracle]]);
    await setCachedFeed("source", feed("source", ["Old Deck"]));
    await hydrateFeedCache();
    await idbSet("source", feed("source", ["New Deck"]), {} as never);
    localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify([subscription("source")]));
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const plan = observedPlan();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      plan.observed(await planDeckLibraryPack(packId("deck_library")));
    });

    vi.mocked(idbEntries).mockRejectedValueOnce(new Error("offline IDB"));
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    await flush();

    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    expect(getCachedFeed("source")?.decks.map((deck) => deck.name)).toEqual(["New Deck"]);
    expect(warning).toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();

    act(() => window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY, storageArea: localStorage })));
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    const membership = await plan.settled;
    expect(plan.observed).toHaveBeenCalledTimes(1);
    expect(descriptorKeys(membership))
      .toContain(`asset:v1:canonical_card:${newOracle}-0-full_card-normal`);
    expect(descriptorKeys(membership))
      .not.toContain(`asset:v1:canonical_card:${oldOracle}-0-full_card-normal`);
    mounted.unmount();
  });

  it("drains a second external feed freshness request and does no work after unmount", async () => {
    await setCachedFeed("source", feed("source", ["Old Deck"]));
    await hydrateFeedCache();
    const firstRead = deferred<Array<[IDBValidKey, Feed]>>();
    const secondRead = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries)
      .mockReturnValueOnce(firstRead.promise)
      .mockReturnValueOnce(secondRead.promise);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    platform.load.mockClear();

    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    await flush();
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    firstRead.resolve([]);
    await flush();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();

    mounted.unmount();
    secondRead.resolve([]);
    await flush();
    expect(platform.load).not.toHaveBeenCalled();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
  });

  it("uses the newest durable feed snapshot when another external update arrives during refresh", async () => {
    const oldOracle = "11111111-1111-4111-8111-111111111111";
    const newOracle = "22222222-2222-4222-8222-222222222222";
    configureCardData([["Old Deck", oldOracle], ["New Deck", newOracle]]);
    await setCachedFeed("source", feed("source", ["Old Deck"]));
    await hydrateFeedCache();
    localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify([subscription("source")]));
    const firstRead = deferred<Array<[IDBValidKey, Feed]>>();
    const secondRead = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries)
      .mockReturnValueOnce(firstRead.promise)
      .mockReturnValueOnce(secondRead.promise);
    const plan = observedPlan();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      plan.observed(await planDeckLibraryPack(packId("deck_library")));
    });

    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    await flush();
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    firstRead.resolve([["source", feed("source", ["Old Deck"])] ]);
    await flush();
    secondRead.resolve([["source", feed("source", ["New Deck"])] ]);
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    const membership = await plan.settled;
    expect(plan.observed).toHaveBeenCalledTimes(1);
    expect(descriptorKeys(membership))
      .toContain(`asset:v1:canonical_card:${newOracle}-0-full_card-normal`);
    expect(descriptorKeys(membership))
      .not.toContain(`asset:v1:canonical_card:${oldOracle}-0-full_card-normal`);
    mounted.unmount();
  });

  it("buffers durable feed changes received while backend loading before its sole startup reconciliation", async () => {
    const oldOracle = "11111111-1111-4111-8111-111111111111";
    const newOracle = "22222222-2222-4222-8222-222222222222";
    configureCardData([["Old Deck", oldOracle], ["New Deck", newOracle]]);
    await setCachedFeed("source", feed("source", ["Old Deck"]));
    await hydrateFeedCache();
    await idbSet("source", feed("source", ["New Deck"]), {} as never);
    localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify([subscription("source")]));
    const plan = observedPlan();
    const loading = deferred<typeof backend>();
    platform.load.mockReturnValueOnce(loading.promise);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    expect(platform.load).toHaveBeenCalledTimes(1);

    backend.reconcileDeckLibrary.mockImplementation(async () => {
      plan.observed(await planDeckLibraryPack(packId("deck_library")));
    });
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    await act(async () => { loading.resolve(backend); });
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    const membership = await plan.settled;
    expect(plan.observed).toHaveBeenCalledTimes(1);
    expect(descriptorKeys(membership))
      .toContain(`asset:v1:canonical_card:${newOracle}-0-full_card-normal`);
    expect(descriptorKeys(membership))
      .not.toContain(`asset:v1:canonical_card:${oldOracle}-0-full_card-normal`);
    mounted.unmount();
  });

  it("rehydrates preferences before planning after they change during a feed refresh", async () => {
    const oracleId = "11111111-1111-4111-8111-111111111111";
    const printingId = "11111111-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    configureCardData([["Fresh Art", oracleId]], true);
    await setCachedFeed("source", feed("source", ["Fresh Art"]));
    await hydrateFeedCache();
    localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, JSON.stringify([subscription("source")]));
    const firstRead = deferred<Array<[IDBValidKey, Feed]>>();
    const secondRead = deferred<Array<[IDBValidKey, Feed]>>();
    vi.mocked(idbEntries)
      .mockReturnValueOnce(firstRead.promise)
      .mockReturnValueOnce(secondRead.promise);
    const rehydrate = vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementation(async () => {
      usePreferencesStore.getState().setArtChain([{ type: "newest" }]);
    });
    const plan = observedPlan();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      plan.observed(await planDeckLibraryPack(packId("deck_library")));
    });

    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    await flush();
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: PREFERENCES_KEY,
      storageArea: localStorage,
    })));
    firstRead.resolve([["source", feed("source", ["Fresh Art"])] ]);
    await flush();
    expect(rehydrate).toHaveBeenCalledTimes(2);
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();

    secondRead.resolve([["source", feed("source", ["Fresh Art"])] ]);
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    const membership = await plan.settled;
    expect(plan.observed).toHaveBeenCalledTimes(1);
    expect(descriptorKeys(membership))
      .toContain(`asset:v1:exact_printing:${printingId}-0-full_card-normal`);
    mounted.unmount();
  });
});
