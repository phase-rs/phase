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
import { useDeckLibraryAutoSync } from "../deckLibraryAutoSync.ts";
import { planDeckLibraryPack } from "../deckLibraryPack.ts";
import { packId } from "../types.ts";
import type { CuratedCardEntry } from "../curatedMembership.ts";

const platform = vi.hoisted(() => ({ load: vi.fn() }));
const planner = vi.hoisted(() => ({ invalidate: vi.fn() }));
const feedRefresh = vi.hoisted(() => ({ refresh: vi.fn(async () => {}) }));
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
vi.mock("../../feedPersistence.ts", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../feedPersistence.ts")>(),
  refreshFeedCache: feedRefresh.refresh,
}));
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

const backend = { reconcileDeckLibrary: vi.fn(async () => {}) };

async function flush(): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(500);
  });
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
  platform.load.mockResolvedValue(backend);
  planner.invalidate.mockReset();
  backend.reconcileDeckLibrary.mockReset();
  backend.reconcileDeckLibrary.mockResolvedValue(undefined);
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
  it("waits for feed hydration, then performs the startup reconciliation once", async () => {
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    expect(platform.load).not.toHaveBeenCalled();

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
    expect(rehydrate).toHaveBeenCalledTimes(1);
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
    expect(platform.load).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(rehydrate).toHaveBeenCalledTimes(1);

    hydrated.mockReturnValue(true);
    rehydrate.mockRejectedValueOnce(new Error("offline preferences"));
    act(() => window.dispatchEvent(new Event("online")));
    await flush();
    expect(platform.load).not.toHaveBeenCalled();
    expect(warning).toHaveBeenCalled();

    act(() => window.dispatchEvent(new Event("online")));
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
    const observed = vi.fn<(membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void>();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    observed.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      observed(await planDeckLibraryPack(packId("deck_library")));
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
    expect(observed.mock.calls[0][0].descriptors.map((descriptor) => String(descriptor.assetKey)))
      .toContain("asset:v1:exact_printing:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa-0-full_card-normal");
    mounted.unmount();
  });

  it("uses rehydrated new art in the real shared planner after restored-profile replacement", async () => {
    await hydrateFeedCache();
    plannerFixture();
    vi.spyOn(usePreferencesStore.persist, "hasHydrated").mockReturnValue(true);
    let releaseRehydrate!: () => void;
    const rehydrated = new Promise<void>((resolve) => { releaseRehydrate = resolve; });
    vi.spyOn(usePreferencesStore.persist, "rehydrate").mockImplementation(async () => {
      await rehydrated;
      usePreferencesStore.getState().setArtChain([{ type: "newest" }]);
    });
    const observed = vi.fn<(membership: Awaited<ReturnType<typeof planDeckLibraryPack>>) => void>();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();
    backend.reconcileDeckLibrary.mockClear();
    observed.mockClear();
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      observed(await planDeckLibraryPack(packId("deck_library")));
    });

    act(() => window.dispatchEvent(new CustomEvent(PROFILE_REPLACED_EVENT)));
    await flush();
    expect(backend.reconcileDeckLibrary).not.toHaveBeenCalled();
    await act(async () => { releaseRehydrate(); });
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect(observed.mock.calls[0][0].descriptors.map((descriptor) => String(descriptor.assetKey)))
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
    backend.reconcileDeckLibrary.mockImplementation(async () => {
      observed(await planDeckLibraryPack(packId("deck_library")));
    });
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);
    expect(observed).not.toHaveBeenCalled();
    expect(warning).toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(1);

    planning.precons = {};
    act(() => window.dispatchEvent(new Event("online")));
    await flush();

    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(2);
    expect(observed.mock.calls[0][0].descriptors.map((descriptor) => String(descriptor.assetKey)))
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

    act(() => window.dispatchEvent(new Event("online")));
    await flush();
    expect(backend.reconcileDeckLibrary).toHaveBeenCalledTimes(2);
    mounted.unmount();
  });

  it("removes pending lifecycle work on unmount", async () => {
    await hydrateFeedCache();
    const mounted = renderHook(() => useDeckLibraryAutoSync());
    mounted.unmount();
    await flush();
    act(() => localStorage.setItem(STORAGE_KEY_PREFIX + "Deck", "{}"));
    await flush();
    expect(platform.load).not.toHaveBeenCalled();
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
    expect(source.match(/useDeckLibraryAutoSync\(\);/g)).toHaveLength(1);
    expect(source.indexOf("useCloudSyncStore.getState().init()"))
      .toBeLessThan(source.indexOf("useDeckLibraryAutoSync();"));
  });
});
