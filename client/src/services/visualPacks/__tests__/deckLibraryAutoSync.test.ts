// @vitest-environment jsdom

import { act, cleanup, renderHook } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createElement, StrictMode, type ReactNode } from "react";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

import {
  FEED_DECK_ORIGINS_KEY,
  FEED_SUBSCRIPTIONS_KEY,
  PREFERENCES_KEY,
  STORAGE_KEY_PREFIX,
} from "../../../constants/storage.ts";
import { PROFILE_REPLACED_EVENT } from "../../../stores/cloudSyncStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { DeckMap } from "../../../hooks/useDecks.ts";
import type { DeckCatalogCandidate } from "../../deckCatalog.ts";
import type { ParsedDeck } from "../../deckParser.ts";
import {
  _resetFeedCacheForTests,
  hydrateFeedCache,
  setCachedFeed,
} from "../../feedPersistence.ts";
import type { PrintingEntry } from "../../scryfall.ts";
import { prepareDeckLibraryForOffline, useDeckLibraryAutoSync } from "../deckLibraryAutoSync.ts";
import { VisualPackBackendError } from "../backend.ts";
import { planDeckLibraryPack } from "../deckLibraryPack.ts";
import { packId } from "../types.ts";
import type { CuratedCardEntry } from "../curatedMembership.ts";

const platform = vi.hoisted(() => ({ load: vi.fn() }));
const planner = vi.hoisted(() => ({ invalidate: vi.fn() }));
const feedRefresh = vi.hoisted(() => ({ refresh: vi.fn(async () => {}), subscribe: vi.fn() }));
const planning = vi.hoisted(() => ({
  cards: {} as Record<string, CuratedCardEntry>,
  printings: {} as Record<string, PrintingEntry[]>,
  catalog: [] as DeckCatalogCandidate[],
  oracleIds: new Map<string, string>(),
  precons: {} as DeckMap | null,
  subscriptions: [] as Array<{ sourceId: string }>,
  feeds: new Map<string, unknown>(),
}));

vi.mock("../../platform.ts", () => ({ loadVisualPackBackend: platform.load }));
vi.mock("../../feedPersistence.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../feedPersistence.ts")>();
  return {
    ...actual,
    refreshFeedCache: feedRefresh.refresh,
    subscribeFeedCache: (...args: Parameters<typeof actual.subscribeFeedCache>) => {
      feedRefresh.subscribe();
      return actual.subscribeFeedCache(...args);
    },
  };
});
vi.mock("../../scryfall.ts", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../scryfall.ts")>(),
  loadScryfallData: vi.fn(async () => planning.cards),
  loadPrintingsData: vi.fn(async () => planning.printings),
  resolveOracleIdSync: vi.fn((name: string) => planning.oracleIds.get(name) ?? null),
}));
vi.mock("../../deckCatalog.ts", () => ({ buildDeckCatalog: vi.fn(async () => planning.catalog) }));
vi.mock("../../../hooks/useDecks.ts", () => ({ loadPreconDeckMap: vi.fn(async () => planning.precons) }));
vi.mock("../../feedService.ts", () => ({
  listSubscriptions: vi.fn(() => planning.subscriptions),
  getCachedFeed: vi.fn((feedId: string) => planning.feeds.get(feedId) ?? null),
}));
vi.mock("../deckLibraryPack.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../deckLibraryPack.ts")>();
  return {
    ...actual,
    invalidateDeckLibraryPack: () => {
      planner.invalidate();
      actual.invalidateDeckLibraryPack();
    },
  };
});

const backend = {
  reconcileDeckLibrary: vi.fn(async () => {}),
  setDeckLibraryBackgroundPaused: vi.fn(async () => {}),
  prepareDeckLibraryForOffline: vi.fn(async () => "ready" as const),
};

async function flush(): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(500);
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function plannerFixture(): void {
  const oracleId = "11111111-abcd-4111-8111-111111111111";
  const freshPrinting = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const image = (id: string, rung: string) => `https://cards.scryfall.io/${rung}/front/a/b/${id}.jpg`;
  planning.cards = {
    [oracleId]: {
      oracle_id: oracleId,
      name: "Fresh Art",
      face_names: ["fresh art"],
      faces: [{ normal: image(oracleId, "normal"), art_crop: image(oracleId, "art_crop") }],
    },
  };
  planning.printings = {
    [oracleId]: [{
      id: freshPrinting,
      set: "m26",
      set_name: "Modern Horizons 26",
      collector_number: "1",
      released_at: "2026-01-01",
      border_color: "black",
      frame_effects: [],
      full_art: false,
      faces: [{ normal: image(freshPrinting, "normal"), art_crop: image(freshPrinting, "art_crop") }],
    }],
  };
  planning.oracleIds = new Map([["Fresh Art", oracleId]]);
  planning.catalog = [{
    id: "saved:Fresh",
    name: "Fresh",
    source: { type: "saved" },
    deck: { main: [{ count: 1, name: "Fresh Art" }], sideboard: [] } as ParsedDeck,
  }];
}

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  _resetFeedCacheForTests();
  platform.load.mockReset();
  feedRefresh.refresh.mockReset();
  feedRefresh.refresh.mockResolvedValue(undefined);
  feedRefresh.subscribe.mockReset();
  platform.load.mockResolvedValue(backend);
  planner.invalidate.mockReset();
  backend.reconcileDeckLibrary.mockReset();
  backend.reconcileDeckLibrary.mockResolvedValue(undefined);
  backend.setDeckLibraryBackgroundPaused.mockReset();
  backend.setDeckLibraryBackgroundPaused.mockResolvedValue(undefined);
  backend.prepareDeckLibraryForOffline.mockReset();
  backend.prepareDeckLibraryForOffline.mockResolvedValue("ready");
  planning.cards = {};
  planning.printings = {};
  planning.catalog = [];
  planning.oracleIds = new Map();
  planning.precons = {};
  planning.subscriptions = [];
  planning.feeds = new Map();
  usePreferencesStore.setState({ artChain: [], artOverrides: {} });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useDeckLibraryAutoSync", () => {
  it("rejects an imperative preparation request when no mounted scheduler owns the lifecycle", async () => {
    await expect(prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "unavailable" });
  });

  it("coalesces imperative preparation callers through the mounted lifecycle", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    const rehydrate = vi.spyOn(usePreferencesStore.persist, "rehydrate");
    feedRefresh.refresh.mockClear();

    const first = prepareDeckLibraryForOffline();
    const second = prepareDeckLibraryForOffline();
    expect(second).toBe(first);
    await flush();
    await expect(first).resolves.toBe("ready");

    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1);
    expect(rehydrate).toHaveBeenCalledTimes(1);
    expect(feedRefresh.refresh).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("rejects pending imperative preparation when its mounted generation unmounts", async () => {
    await hydrateFeedCache();
    const pending = new Promise<"ready">(() => {});
    backend.prepareDeckLibraryForOffline.mockImplementationOnce(async () => pending);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    const request = prepareDeckLibraryForOffline();
    await flush();
    await vi.waitFor(() => expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1));

    mounted.unmount();

    await expect(request).rejects.toEqual(new VisualPackBackendError("cancelled"));
  });

  it("rejects preparation when the mounted backend lacks the lifecycle and retries later", async () => {
    await hydrateFeedCache();
    platform.load.mockResolvedValue(null);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();

    const unavailable = prepareDeckLibraryForOffline();
    const unavailableAssertion = expect(unavailable).rejects.toMatchObject({ kind: "unavailable" });
    await flush();
    await unavailableAssertion;

    platform.load.mockResolvedValue(backend);
    const retry = prepareDeckLibraryForOffline();
    await flush();
    await expect(retry).resolves.toBe("ready");
    mounted.unmount();
  });

  it("rejects a failed fresh feed refresh and permits a later preparation retry", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    feedRefresh.refresh.mockRejectedValueOnce(new Error("offline"));

    const failed = prepareDeckLibraryForOffline();
    const failureAssertion = expect(failed).rejects.toMatchObject({ kind: "unavailable" });
    await flush();
    await failureAssertion;

    const retry = prepareDeckLibraryForOffline();
    await flush();
    await expect(retry).resolves.toBe("ready");
    mounted.unmount();
  });

  it("rejects unsuccessful fresh preference hydration and permits a later preparation retry", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    const rehydrate = vi.spyOn(usePreferencesStore.persist, "rehydrate").mockRejectedValueOnce(new Error("offline"));

    const failed = prepareDeckLibraryForOffline();
    const failureAssertion = expect(failed).rejects.toMatchObject({ kind: "unavailable" });
    await flush();
    await failureAssertion;
    rehydrate.mockResolvedValue(undefined);

    const retry = prepareDeckLibraryForOffline();
    await flush();
    await expect(retry).resolves.toBe("ready");
    mounted.unmount();
  });

  it("retries after a resolved rehydrate leaves preferences unhydrated", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    const hydrated = vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(false);

    const request = prepareDeckLibraryForOffline();
    const assertion = expect(request).rejects.toMatchObject({ kind: "unavailable" });
    await flush();
    await assertion;
    hydrated.mockRestore();

    const retry = prepareDeckLibraryForOffline();
    await flush();
    await expect(retry).resolves.toBe("ready");
    mounted.unmount();
  });

  it("does not prepare until both explicit freshness stages settle", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    const rehydrate = deferred<void>();
    const refresh = deferred<void>();
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementationOnce(async () => rehydrate.promise);
    feedRefresh.refresh.mockImplementationOnce(async () => refresh.promise);

    const request = prepareDeckLibraryForOffline();
    await flush();
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();

    rehydrate.resolve();
    await flush();
    expect(feedRefresh.refresh).toHaveBeenCalled();
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();

    refresh.resolve();
    await flush();
    await expect(request).resolves.toBe("ready");
    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("repeats preference freshness when a newer explicit preference signal arrives during rehydration", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    const firstRehydrate = deferred<void>();
    const secondRehydrate = deferred<void>();
    const rehydrate = vi.spyOn(usePreferencesStore.persist, "rehydrate")
      .mockImplementationOnce(async () => firstRehydrate.promise)
      .mockImplementationOnce(async () => secondRehydrate.promise);

    const request = prepareDeckLibraryForOffline();
    await flush();
    expect(rehydrate).toHaveBeenCalledTimes(1);
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: PREFERENCES_KEY,
      storageArea: localStorage,
    })));
    firstRehydrate.resolve();
    await flush();
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();
    await flush();
    expect(rehydrate).toHaveBeenCalledTimes(2);
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();

    secondRehydrate.resolve();
    await flush();
    await expect(request).resolves.toBe("ready");
    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("repeats feed freshness when a newer explicit feed signal arrives during refresh", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    feedRefresh.refresh.mockClear();
    const firstRefresh = deferred<void>();
    const secondRefresh = deferred<void>();
    const refresh = feedRefresh.refresh
      .mockImplementationOnce(async () => firstRefresh.promise)
      .mockImplementationOnce(async () => secondRefresh.promise);

    const request = prepareDeckLibraryForOffline();
    await flush();
    expect(refresh).toHaveBeenCalledTimes(1);
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    firstRefresh.resolve();
    await flush();
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();
    await flush();
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();

    secondRefresh.resolve();
    await flush();
    await expect(request).resolves.toBe("ready");
    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("retries stale backend fulfillment after a plain catalog input signal", async () => {
    await hydrateFeedCache();
    const pending = deferred<"ready">();
    backend.prepareDeckLibraryForOffline.mockImplementationOnce(async () => pending.promise);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();

    const request = prepareDeckLibraryForOffline();
    await flush();
    await vi.waitFor(() => expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1));
    act(() => localStorage.setItem(STORAGE_KEY_PREFIX + "Freshness", "{}"));
    pending.resolve("ready");
    await flush();
    await expect(request).resolves.toBe("ready");
    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(2);
    mounted.unmount();
  });

  it("forwards backend outcomes, retries typed errors, and ignores stale backend rejection", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockRejectedValueOnce(new VisualPackBackendError("network"));
    const failed = prepareDeckLibraryForOffline();
    const failureAssertion = expect(failed).rejects.toMatchObject({ kind: "network" });
    await flush();
    await failureAssertion;

    const retry = prepareDeckLibraryForOffline();
    await flush();
    await expect(retry).resolves.toBe("ready");

    backend.prepareDeckLibraryForOffline.mockResolvedValueOnce("not-installed" as never);
    const absent = prepareDeckLibraryForOffline();
    await flush();
    await expect(absent).resolves.toBe("not-installed");

    const pending = deferred<"ready">();
    backend.prepareDeckLibraryForOffline.mockImplementationOnce(async () => pending.promise);
    const stale = prepareDeckLibraryForOffline();
    await flush();
    act(() => localStorage.setItem(STORAGE_KEY_PREFIX + "RejectedFreshness", "{}"));
    pending.reject(new VisualPackBackendError("network"));
    await flush();
    await expect(stale).resolves.toBe("ready");
    mounted.unmount();
  });

  it("fences a deferred preference rehydrate when offline supersedes preparation", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(
      ({ offline, feedReady }) => useDeckLibraryAutoSync(offline, feedReady),
      { initialProps: { offline: false, feedReady: true } },
    );
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    backend.setDeckLibraryBackgroundPaused.mockClear();
    const rehydrate = deferred<void>();
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementationOnce(async () => rehydrate.promise);
    const offline = prepareDeckLibraryForOffline();
    const offlineAssertion = expect(offline).rejects.toMatchObject({ kind: "cancelled" });
    await flush();
    mounted.rerender({ offline: true, feedReady: true });
    await offlineAssertion;
    rehydrate.resolve();
    await flush();
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledWith(true));
    expect(backend.setDeckLibraryBackgroundPaused).not.toHaveBeenCalledWith(false);
    mounted.unmount();
  });

  it("fences a deferred feed refresh when feed initialization supersedes preparation", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(
      ({ offline, feedReady }) => useDeckLibraryAutoSync(offline, feedReady),
      { initialProps: { offline: false, feedReady: true } },
    );
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    backend.setDeckLibraryBackgroundPaused.mockClear();
    const refresh = deferred<void>();
    feedRefresh.refresh.mockImplementationOnce(async () => refresh.promise);
    const feedInitializing = prepareDeckLibraryForOffline();
    const feedAssertion = expect(feedInitializing).rejects.toMatchObject({ kind: "cancelled" });
    await flush();
    mounted.rerender({ offline: false, feedReady: false });
    await feedAssertion;
    refresh.resolve();
    await flush();
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledWith(true));
    expect(backend.setDeckLibraryBackgroundPaused).not.toHaveBeenCalledWith(false);
    mounted.unmount();
  });

  it("fences deferred backend preparation after unmount", async () => {
    await hydrateFeedCache();
    const pending = deferred<"ready">();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    backend.prepareDeckLibraryForOffline.mockImplementationOnce(async () => pending.promise);
    backend.setDeckLibraryBackgroundPaused.mockClear();

    const request = prepareDeckLibraryForOffline();
    const assertion = expect(request).rejects.toMatchObject({ kind: "cancelled" });
    await flush();
    await vi.waitFor(() => expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1));
    mounted.unmount();
    await assertion;
    pending.resolve("ready");
    await flush();
    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledWith(true));
    expect(backend.setDeckLibraryBackgroundPaused).not.toHaveBeenCalledWith(false);
  });

  it("waits for ordinary reconciliation before preparation and leaves no trailing reconcile", async () => {
    await hydrateFeedCache();
    const ordinary = deferred<void>();
    backend.reconcileDeckLibrary.mockImplementationOnce(async () => ordinary.promise);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    await vi.waitFor(() => expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1));
    const request = prepareDeckLibraryForOffline();
    expect(backend.prepareDeckLibraryForOffline).not.toHaveBeenCalled();
    ordinary.resolve();
    await flush();
    await expect(request).resolves.toBe("ready");
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("absorbs real preference and feed-cache notifications produced by preparation freshness", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.prepareDeckLibraryForOffline.mockClear();
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementation(async () => {
      usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    });
    feedRefresh.refresh.mockImplementation(async () => { await setCachedFeed("preparation", {} as never); });

    const request = prepareDeckLibraryForOffline();
    await flush();
    await expect(request).resolves.toBe("ready");
    expect(backend.prepareDeckLibraryForOffline).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("keeps automatic reconciliation paused while offline or feed initialization is not ready, including an aborted generation", async () => {
    await hydrateFeedCache();
    const offline = renderHook(() => useDeckLibraryAutoSync(true, true));
    await flush();
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledWith(true);
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    expect(feedRefresh.subscribe).not.toHaveBeenCalled();
    backend.setDeckLibraryBackgroundPaused.mockClear();
    await act(async () => { await setCachedFeed("offline-cache-write", {} as never); });
    await flush();
    expect(backend.setDeckLibraryBackgroundPaused).not.toHaveBeenCalled();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    offline.unmount();

    backend.setDeckLibraryBackgroundPaused.mockClear();
    const pendingFeed = renderHook(() => useDeckLibraryAutoSync(false, false));
    await flush();
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledWith(true);
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    expect(feedRefresh.subscribe).not.toHaveBeenCalled();
    pendingFeed.unmount();
  });

  it("unpauses once and starts one reconciliation only after the current feed generation settles", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(
      ({ ready }) => useDeckLibraryAutoSync(false, ready),
      { initialProps: { ready: false } },
    );
    await flush();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();

    mounted.rerender({ ready: true });
    await flush();
    expect(feedRefresh.subscribe).toHaveBeenCalledTimes(1);
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenLastCalledWith(false);
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("waits for feed hydration, then performs the startup reconciliation once", async () => {
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    expect(platform.load).toHaveBeenCalledTimes(1);
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();

    await act(async () => { await hydrateFeedCache(); });
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("coalesces relevant same-tab writes and ignores unrelated storage", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    planner.invalidate.mockClear();

    act(() => {
      localStorage.setItem(STORAGE_KEY_PREFIX + "Deck", "{}");
      localStorage.setItem(STORAGE_KEY_PREFIX + "Deck", "{\"updated\":true}");
      localStorage.removeItem(STORAGE_KEY_PREFIX + "Deck");
      localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, "[]");
      localStorage.setItem(FEED_DECK_ORIGINS_KEY, "{}");
      localStorage.setItem("phase-active-game", "ignored");
      window.dispatchEvent(new StorageEvent("storage", {
        key: STORAGE_KEY_PREFIX + "CrossTabDeck",
        storageArea: localStorage,
      }));
      window.dispatchEvent(new StorageEvent("storage", {
        key: STORAGE_KEY_PREFIX + "SessionDeck",
        storageArea: sessionStorage,
      }));
    });
    await flush();

    expect(planner.invalidate).toHaveBeenCalledTimes(6);
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("refreshes durable feeds only for external catalog changes", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    feedRefresh.refresh.mockClear();

    act(() => localStorage.setItem(FEED_SUBSCRIPTIONS_KEY, "[]"));
    await flush();
    expect(feedRefresh.refresh).not.toHaveBeenCalled();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);

    backend.reconcileDeckLibrary.mockClear();
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: FEED_SUBSCRIPTIONS_KEY,
      storageArea: localStorage,
    })));
    await flush();
    expect(feedRefresh.refresh).toHaveBeenCalledTimes(1);
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("runs one trailing reconciliation after art changes during active work", async () => {
    await hydrateFeedCache();
    let complete!: () => void;
    backend.reconcileDeckLibrary.mockImplementationOnce(() => new Promise<void>((resolve) => { complete = resolve; }));
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);

    act(() => {
      usePreferencesStore.getState().setArtChain([{ type: "newest" }]);
      usePreferencesStore.getState().setArtOverride(
        "11111111-abcd-4111-8111-111111111111",
        { scryfallId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", setCode: "m26", collectorNumber: "1" },
      );
    });
    await act(async () => { complete(); });
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(2);
    mounted.unmount();
  });

  it("rehydrates for profile and cross-tab preference signals before retrying", async () => {
    await hydrateFeedCache();
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    const rehydrate = vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementation(async () => {
      usePreferencesStore.setState((current) => ({
        artChain: [...current.artChain],
        artOverrides: { ...current.artOverrides },
      }));
    });
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();

    act(() => {
      window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY, storageArea: localStorage }));
      window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT));
    });
    await flush();

    expect(rehydrate).toHaveBeenCalled();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(rehydrate).toHaveBeenCalledTimes(2);
    mounted.unmount();
  });

  it("drains a newer preference freshness request that arrives while loading the backend", async () => {
    await hydrateFeedCache();
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    let resolveLoad!: (value: typeof backend) => void;
    platform.load.mockReturnValueOnce(new Promise<typeof backend>((resolve) => { resolveLoad = resolve; }));
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    expect(platform.load).toHaveBeenCalledTimes(1);

    act(() => window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY, storageArea: localStorage })));
    await act(async () => { resolveLoad(backend); });
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("drains a second freshness request that arrives while preferences are rehydrating", async () => {
    await hydrateFeedCache();
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    let releaseRehydrate!: () => void;
    const rehydrated = new Promise<void>((resolve) => { releaseRehydrate = resolve; });
    const rehydrate = vi.spyOn(usePreferencesStore.persist, "rehydrate")
      .mockImplementationOnce(async () => rehydrated);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();

    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: PREFERENCES_KEY,
      storageArea: localStorage,
    })));
    await flush();
    expect(rehydrate).toHaveBeenCalledTimes(1);
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();

    act(() => window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT)));
    await act(async () => { releaseRehydrate(); });
    await flush();

    expect(rehydrate).toHaveBeenCalledTimes(2);
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("does not plan after an unsuccessful or rejected preference rehydration, then retries on online", async () => {
    await hydrateFeedCache();
    const hydrated = vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(false);
    const rehydrate = vi.spyOn(usePreferencesStore.persist, "rehydrate").mockResolvedValue(undefined);
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();

    expect(rehydrate).toHaveBeenCalledTimes(1);
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(rehydrate).toHaveBeenCalledTimes(1);

    hydrated.mockReturnValue(true);
    rehydrate.mockRejectedValueOnce(new Error("offline preferences"));
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY, storageArea: localStorage })));
    await flush();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    expect(warning).toHaveBeenCalled();

    act(() => window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY, storageArea: localStorage })));
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("reacts to hydrated feed-cache changes without performing another hydration", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();

    await act(async () => { await setCachedFeed("fixture", {} as never); });
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("uses rehydrated new art in the real shared planner before background reconciliation", async () => {
    await hydrateFeedCache();
    plannerFixture();
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    let releaseRehydrate!: () => void;
    const rehydrated = new Promise<void>((resolve) => { releaseRehydrate = resolve; });
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementation(async () => {
      await rehydrated;
      usePreferencesStore.getState().setArtChain([{ type: "newest" }]);
    });
    let resolvePlan!: (membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void;
    const planned = new Promise<Awaited<ReturnType<typeof planDeckLibraryPack>>>((resolve) => { resolvePlan = resolve; });
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      resolvePlan(await planDeckLibraryPack(packId("deck_library")));
    });

    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: PREFERENCES_KEY,
      storageArea: localStorage,
    })));
    await flush();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    await act(async () => { releaseRehydrate(); });
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect((await planned).descriptors.map((descriptor) => String(descriptor.assetKey)))
      .toContain("asset:v1:exact_printing:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa-0-full_card-normal");
    mounted.unmount();
  });

  it("uses rehydrated new art in the real shared planner after restored-profile replacement", async () => {
    await hydrateFeedCache();
    plannerFixture();
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    let rehydrateCount = 0;
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementation(async () => {
      rehydrateCount += 1;
      if (rehydrateCount === 2) usePreferencesStore.getState().setArtChain([{ type: "newest" }]);
    });
    let resolvePlan!: (membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void;
    const planned = new Promise<Awaited<ReturnType<typeof planDeckLibraryPack>>>((resolve) => { resolvePlan = resolve; });
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    expect(rehydrateCount).toBe(1);
    backend.reconcileDeckLibrary.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      resolvePlan(await planDeckLibraryPack(packId("deck_library")));
    });

    act(() => window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT)));
    await flush();

  expect(rehydrateCount).toBe(2);
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect((await planned).descriptors.map((descriptor) => String(descriptor.assetKey)))
      .toContain("asset:v1:exact_printing:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa-0-full_card-normal");
    mounted.unmount();
  });

  it("retries a real planner after unavailable precons recover on a later online signal", async () => {
    await hydrateFeedCache();
    plannerFixture();
    planning.precons = null;
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const observed = vi.fn<(membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void>();
    let resolvePlan!: (membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void;
    const planned = new Promise<Awaited<ReturnType<typeof planDeckLibraryPack>>>((resolve) => { resolvePlan = resolve; });
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      const membership = await planDeckLibraryPack(packId("deck_library"));
      observed(membership);
      resolvePlan(membership);
    });
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect(observed).not.toHaveBeenCalled();
    expect(warning).toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);

    planning.precons = {};
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY, storageArea: localStorage })));
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(2);
    expect((await planned).descriptors.map((descriptor) => String(descriptor.assetKey)))
      .toContain("asset:v1:canonical_card:11111111-abcd-4111-8111-111111111111-0-full_card-normal");
    mounted.unmount();
  });

  it("recovers from a background failure on a later online signal", async () => {
    await hydrateFeedCache();
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    backend.reconcileDeckLibrary.mockRejectedValueOnce(new Error("offline"));
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect(warning).toHaveBeenCalled();

    act(() => window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY, storageArea: localStorage })));
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(2);
    mounted.unmount();
  });

  it("retries one failed lifecycle unpause only after a later catalog signal", async () => {
    await hydrateFeedCache();
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    backend.setDeckLibraryBackgroundPaused.mockRejectedValueOnce(new Error("lifecycle unavailable"));
    const mounted = renderHook(() => useDeckLibraryAutoSync());

    await flush();
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledTimes(1);
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenLastCalledWith(false);
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    expect(warning).toHaveBeenCalled();

    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledTimes(1);

    act(() => localStorage.setItem(STORAGE_KEY_PREFIX + "RetryDeck", "{}"));
    await flush();
    await flush();

    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledTimes(2);
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenLastCalledWith(false);
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("keeps catalog signals received during a pending lifecycle acquisition for one coalesced retry", async () => {
    await hydrateFeedCache();
    const warning = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    let rejectUnpause!: (error: Error) => void;
    const pendingUnpause = new Promise<void>((_resolve, reject) => { rejectUnpause = reject; });
    backend.setDeckLibraryBackgroundPaused.mockImplementationOnce(async () => pendingUnpause);
    const mounted = renderHook(() => useDeckLibraryAutoSync());

    await vi.waitFor(() => expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledTimes(1));
    act(() => {
      localStorage.setItem(STORAGE_KEY_PREFIX + "PendingRetryOne", "{}");
      localStorage.setItem(STORAGE_KEY_PREFIX + "PendingRetryTwo", "{}");
    });
    await act(async () => { rejectUnpause(new Error("lifecycle unavailable")); });
    await flush();
    await flush();

    expect(warning).toHaveBeenCalled();
    expect(platform.load).toHaveBeenCalledTimes(1);
    expect(backend.setDeckLibraryBackgroundPaused).toHaveBeenCalledTimes(2);
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("removes pending lifecycle work on unmount", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    mounted.unmount();
    await flush();
    act(() => localStorage.setItem(STORAGE_KEY_PREFIX + "Deck", "{}"));
    await flush();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
  });

  it("does not begin reconciliation after unmounting during backend load or preference rehydration", async () => {
    await hydrateFeedCache();
    let releaseLoad!: (value: typeof backend) => void;
    const loading = new Promise<typeof backend>((resolve) => { releaseLoad = resolve; });
    platform.load.mockReturnValueOnce(loading);
    const loadingMount = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    loadingMount.unmount();
    await act(async () => { releaseLoad(backend); });
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();

    platform.load.mockClear();
    backend.reconcileDeckLibrary.mockClear();
    let releaseRehydrate!: () => void;
    const rehydrating = new Promise<void>((resolve) => { releaseRehydrate = resolve; });
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementationOnce(async () => rehydrating);
    const rehydratingMount = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    platform.load.mockClear();
    backend.reconcileDeckLibrary.mockClear();
    act(() => window.dispatchEvent(new StorageEvent("storage", {
      key: PREFERENCES_KEY,
      storageArea: localStorage,
    })));
    await flush();
    rehydratingMount.unmount();
    await act(async () => { releaseRehydrate(); });
    expect(platform.load).not.toHaveBeenCalled();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
  });

  it("tolerates an unavailable visual-pack backend", async () => {
    await hydrateFeedCache();
    platform.load.mockResolvedValue(null);
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();

    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    mounted.unmount();
  });

  it("keeps one effective lifecycle under StrictMode setup and cleanup", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync(), {
      wrapper: ({ children }: { children: ReactNode }) => createElement(StrictMode, null, children),
    });
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    mounted.unmount();
  });

  it("wires the nonvisual coordinator exactly once in AppContent", () => {
    const source = readFileSync(resolve(process.cwd(), "src/App.tsx"), "utf8");
    expect(source).toMatch(/import \{ useDeckLibraryAutoSync \} from "\.\/services\/visualPacks\/deckLibraryAutoSync"/);
    expect(source.match(/useDeckLibraryAutoSync\(effectiveOffline, feedInitializationReady\);/g)).toHaveLength(1);
    expect(source.indexOf("useCloudSyncStore.getState().init()"))
      .toBeLessThan(source.indexOf("useDeckLibraryAutoSync(effectiveOffline, feedInitializationReady);"));
  });
});
