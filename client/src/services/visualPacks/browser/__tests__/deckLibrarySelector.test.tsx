import "fake-indexeddb/auto";

import { IDBFactory } from "fake-indexeddb";
import { openDB } from "idb";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { VisualPackManager } from "../../../../components/settings/visual-packs/VisualPackManager.tsx";
import { VisualPackBackendError } from "../../backend.ts";
import { assetKey, catalogRoot, estimatedImageBytes, minimumImageBytes, operationId, packId } from "../../types.ts";
import type { DeckLibraryInstallSelector, InstallSelector, ProgressEvent } from "../../types.ts";
import type { ScryfallAssetDescriptor } from "../descriptors.ts";
import { ScryfallBrowserVisualPackBackend } from "../scryfallBackend.ts";

const DATABASE = "phase-visual-packs-scryfall-v1";
const PLANNED_DIGEST = catalogRoot("a".repeat(64));
const INSTALLED_DIGEST = catalogRoot("b".repeat(64));
const CURATED_DIGEST = catalogRoot("c".repeat(64));
const OBJECT_DIGEST = catalogRoot("d".repeat(64));
const OPERATION = operationId("e".repeat(32));
const DECK_LIBRARY = packId("deck_library");
const CURATED = packId("curated");
const EMPTY_DIGEST = catalogRoot("f".repeat(64));
const BULK_INDEX_URL = "https://api.scryfall.com/bulk-data";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

class MemoryCache {
  readonly entries = new Map<string, Response>();

  async put(path: string, response: Response): Promise<void> {
    this.entries.set(path, response.clone());
  }

  async match(path: string): Promise<Response | undefined> {
    if (path === holdCachePath) await new Promise<void>((resolve) => { releaseCacheMatch = resolve; });
    return this.entries.get(path)?.clone();
  }

  async delete(path: string): Promise<boolean> {
    if (path === holdCacheDeletePath) await new Promise<void>((resolve) => { releaseCacheDelete = resolve; });
    return this.entries.delete(path);
  }
}

let cache = new MemoryCache();
let fetchMock = vi.fn();
let holdSecondImage = false;
let releaseSecondImage: (() => void) | null = null;
let holdCachePath: string | null = null;
let releaseCacheMatch: (() => void) | null = null;
let holdCacheDeletePath: string | null = null;
let releaseCacheDelete: (() => void) | null = null;
let failImages = false;
let failedImage: string | null = null;

const state = vi.hoisted(() => ({
  cardDataResident: false,
  membership: undefined as { membershipDigest: ReturnType<typeof catalogRoot>; descriptors: readonly ScryfallAssetDescriptor[] } | undefined,
  plan: vi.fn(),
  invalidate: vi.fn(),
}));
const platform = vi.hoisted(() => ({ load: vi.fn() }));

vi.mock("../../../scryfall.ts", () => ({
  isCardDataResident: () => state.cardDataResident,
}));

vi.mock("../../deckLibraryPack.ts", () => ({
  planDeckLibraryPack: state.plan,
  invalidateDeckLibraryPack: state.invalidate,
}));
vi.mock("../../../platform.ts", () => ({ loadVisualPackBackend: platform.load }));
vi.mock("../../../../hooks/useSetSymbols.ts", () => ({ useSetCatalog: () => ({ catalog: null, isLoading: false }) }));

class FifoWebLocks {
  private tail = Promise.resolve();

  request<T>(
    _name: string,
    options: { mode: "exclusive"; signal?: AbortSignal },
    callback: () => Promise<T>,
  ): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const run = async () => {
        if (options.signal?.aborted) {
          reject(new DOMException("aborted", "AbortError"));
          return;
        }
        try {
          resolve(await callback());
        } catch (error) {
          reject(error);
        }
      };
      this.tail = this.tail.then(run, run);
    });
  }
}

function installWebLocks(): void {
  Object.defineProperty(globalThis.navigator, "locks", {
    configurable: true,
    value: new FifoWebLocks(),
  });
}

function descriptor(asset: string, sourceUrl: string): ScryfallAssetDescriptor {
  return {
    packId: DECK_LIBRARY,
    assetKey: assetKey(`asset:v1:canonical_card:${asset}`),
    candidateKeys: [],
    sourceUrl,
    media: "image/jpeg",
  };
}

const FIRST = descriptor("first", "https://cards.example/first.jpg");
const SECOND = descriptor("second", "https://cards.example/second.jpg");
const REMOVED = descriptor("removed", "https://cards.example/removed.jpg");
const MOVED = descriptor("second", "https://cards.example/second-new.jpg");
const THIRD = descriptor("third", "https://cards.example/third.jpg");
const FOURTH = descriptor("fourth", "https://cards.example/fourth.jpg");

async function seedPack(pack: ReturnType<typeof packId>, root: ReturnType<typeof catalogRoot>): Promise<void> {
  const database = await openDB(DATABASE);
  await database.put("packs", { id: pack, packId: pack, root, dependencies: [], operationId: OPERATION });
  database.close();
}

async function seedObject(
  pack: ReturnType<typeof packId>,
  root: ReturnType<typeof catalogRoot>,
  value: ScryfallAssetDescriptor,
  sourceUrl = value.sourceUrl,
): Promise<void> {
  const database = await openDB(DATABASE);
  await database.put("objects", {
    id: `${root}:${pack}:${value.assetKey}`,
    root,
    packId: pack,
    assetKey: value.assetKey,
    candidateKeys: value.candidateKeys,
    sourceUrl,
    object: OBJECT_DIGEST,
    byteLength: 1,
    media: value.media,
    path: `/objects/${value.assetKey}`,
  });
  database.close();
}

describe("deck-library selector and drift contract", () => {
  beforeEach(() => {
    globalThis.indexedDB = new IDBFactory();
    state.cardDataResident = false;
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [FIRST, SECOND] };
    state.plan.mockReset();
    state.invalidate.mockReset();
    state.plan.mockImplementation(async () => state.membership);
    cache = new MemoryCache();
    holdSecondImage = false;
    releaseSecondImage = null;
    holdCachePath = null;
    releaseCacheMatch = null;
    holdCacheDeletePath = null;
    releaseCacheDelete = null;
    failImages = false;
    failedImage = null;
    platform.load.mockReset();
    vi.stubGlobal("caches", { open: async () => cache } as unknown as CacheStorage);
    fetchMock = vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const source = String(input);
      if (source === BULK_INDEX_URL) {
        return new Response(JSON.stringify({ data: [{
          type: "all_cards",
          updated_at: "2026-08-01T00:00:00.000Z",
          jsonl_download_uri: "https://bulk.example/cards.jsonl.gz",
          compressed_size: 1,
        }] }), { status: 200, headers: { "Content-Type": "application/json" } });
      }
      if (source.startsWith("https://cards.example/")) {
        if (holdSecondImage && (source === SECOND.sourceUrl || source === MOVED.sourceUrl)) {
          await new Promise<void>((resolve) => { releaseSecondImage = resolve; });
        }
        return failImages || source === failedImage
          ? new Response("", { status: 503 })
          : new Response(source, { status: 200, headers: { "Content-Type": "image/jpeg" } });
      }
      throw new Error(`unexpected fetch: ${source}`);
    });
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    cleanup();
    releaseSecondImage?.();
    releaseCacheMatch?.();
    vi.unstubAllGlobals();
    Reflect.deleteProperty(globalThis.navigator, "locks");
    Reflect.deleteProperty(globalThis.navigator, "storage");
  });

  it("registers deck_library as an installable validated identity", () => {
    const selector: DeckLibraryInstallSelector = { kind: "deck_library", membershipDigest: PLANNED_DIGEST };
    const installSelector: InstallSelector = selector;

    expect(packId("deck_library")).toBe(DECK_LIBRARY);
    expect(selector).toEqual({ kind: "deck_library", membershipDigest: PLANNED_DIGEST });
    expect(installSelector).toBe(selector);
    expect(() => packId("deck-library")).toThrow("invalid PackId");
  });

  it("delegates explicit selector resolution to the shared deck-library planner", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();

    await expect(backend.deckLibrarySelector()).resolves.toEqual({
      kind: "deck_library",
      membershipDigest: PLANNED_DIGEST,
    });
    expect(state.plan).toHaveBeenCalledTimes(1);
    expect(state.plan).toHaveBeenCalledWith(DECK_LIBRARY);
  });

  it("reports every planned descriptor as an add when deck-library is absent", async () => {
    state.cardDataResident = true;
    const backend = await ScryfallBrowserVisualPackBackend.create();

    await expect(backend.deckLibraryDrift()).resolves.toEqual({
      membershipDigest: PLANNED_DIGEST,
      installedDigest: null,
      add: 2,
      remove: 0,
      refresh: 0,
    });
  });

  it("measures only installed deck-library rows and leaves curated rows isolated", async () => {
    state.cardDataResident = true;
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await seedPack(DECK_LIBRARY, INSTALLED_DIGEST);
    await seedObject(DECK_LIBRARY, INSTALLED_DIGEST, FIRST);
    await seedObject(DECK_LIBRARY, INSTALLED_DIGEST, SECOND, "https://cards.example/old-second.jpg");
    await seedObject(DECK_LIBRARY, INSTALLED_DIGEST, REMOVED);
    await seedPack(CURATED, CURATED_DIGEST);
    await seedObject(CURATED, CURATED_DIGEST, REMOVED);

    await expect(backend.deckLibraryDrift()).resolves.toEqual({
      membershipDigest: PLANNED_DIGEST,
      installedDigest: INSTALLED_DIGEST,
      add: 0,
      remove: 1,
      refresh: 1,
    });

    const database = await openDB(DATABASE);
    expect(await database.get("packs", CURATED)).toMatchObject({ root: CURATED_DIGEST });
    expect(await database.getAllFromIndex("objects", "by-pack", CURATED)).toHaveLength(1);
    database.close();
  });

  it("does not plan or load card data for passive drift while data is not resident", async () => {
    state.cardDataResident = false;
    state.plan.mockRejectedValue(new Error("planner must not run"));
    const backend = await ScryfallBrowserVisualPackBackend.create();

    await expect(backend.deckLibraryDrift()).resolves.toBeNull();
    expect(state.plan).not.toHaveBeenCalled();
  });

  it("converts unexpected planner failures and preserves typed backend failures", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    state.plan.mockRejectedValueOnce(new Error("planner failure"));

    await expect(backend.deckLibrarySelector()).rejects.toMatchObject({ kind: "storage", detail: "planner failure" });

    const typed = new VisualPackBackendError("network");
    state.plan.mockRejectedValueOnce(typed);
    await expect(backend.deckLibrarySelector()).rejects.toBe(typed);
  });

  it("installs and promotes the exact local membership without opening the bulk archive", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const selector: InstallSelector = { kind: "deck_library", membershipDigest: PLANNED_DIGEST };
    await expect(backend.estimateInstall(selector)).resolves.toMatchObject({
      selector: DECK_LIBRARY,
      assetRecords: "2",
      uniqueObjects: "2",
      shardCount: "0",
      shardBytes: "unknown",
    });
    const started = await backend.start({ kind: "install", selector, objectEstimate: 2 });
    if (started.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(started.operationId)).state).toBe("completed"));

    expect(fetchMock.mock.calls.map(([input]) => String(input)))
      .not.toContain("https://bulk.example/cards.jsonl.gz");
    expect((await backend.resolve([{ kind: "asset", key: FIRST.assetKey }])).entries[0].matches).toHaveLength(1);

    const persistence = vi.fn(async () => false);
    Object.defineProperty(globalThis.navigator, "storage", {
      configurable: true,
      value: { persisted: vi.fn(async () => false), persist: persistence, estimate: vi.fn(async () => ({})) },
    });
    await expect(backend.start({ kind: "install", selector, objectEstimate: 2 })).resolves.toEqual({ status: "healthy" });
    expect(persistence).not.toHaveBeenCalled();
  });

  it("rejects a stale digest before recording an operation", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await expect(backend.start({
      kind: "install",
      selector: { kind: "deck_library", membershipDigest: EMPTY_DIGEST },
      objectEstimate: 0,
    })).rejects.toMatchObject({ kind: "conflict" });

    const database = await openDB(DATABASE);
    expect(await database.getAll("operations")).toEqual([]);
    database.close();
  });

  it("repairs the receipt-root image after the current membership changes", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const selector: InstallSelector = { kind: "deck_library", membershipDigest: PLANNED_DIGEST };
    const installed = await backend.start({ kind: "install", selector, objectEstimate: 2 });
    if (installed.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(installed.operationId)).state).toBe("completed"));
    await expect(backend.start({ kind: "repair", packIds: [DECK_LIBRARY] })).resolves.toEqual({ status: "healthy" });

    const database = await openDB(DATABASE);
    const [row, healthy] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    database.close();
    if (!row || !healthy) throw new Error("deck-library install wrote too few receipt rows");
    const healthyPath = healthy.path;
    const healthyObject = healthy.object;
    cache.entries.delete(row.path);
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [] };
    fetchMock.mockClear();
    const revisions: string[] = [];
    await backend.subscribeRevision((event) => revisions.push(event.cause));

    const repaired = await backend.start({ kind: "repair", packIds: [DECK_LIBRARY] });
    if (repaired.status !== "started") throw new Error("deck-library repair did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(repaired.operationId)).state).toBe("completed"));

    expect((await backend.operationStatus(repaired.operationId)).kind).toBe("repair");
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toContain(row.sourceUrl);
    expect(revisions).toContain("repair");
    expect((await backend.resolve([{ kind: "asset", key: row.assetKey }])).entries[0].matches).toHaveLength(1);
    const afterRepair = await openDB(DATABASE);
    expect(await afterRepair.get("objects", healthy.id)).toMatchObject({ path: healthyPath, object: healthyObject });
    afterRepair.close();
    expect(cache.entries.has(healthyPath)).toBe(true);
  });

  it("re-fetches a corrupt receipt cache entry and rejects a legacy row without a source URL", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const installed = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (installed.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(installed.operationId)).state).toBe("completed"));

    const database = await openDB(DATABASE);
    const [corrupt, legacy] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    if (!corrupt || !legacy) throw new Error("deck-library install wrote too few rows");
    const original = cache.entries.get(corrupt.path);
    if (!original) throw new Error("corrupt fixture cache entry is missing");
    const originalBytes = new Uint8Array(await original.arrayBuffer());
    cache.entries.set(corrupt.path, new Response(new Uint8Array(originalBytes.byteLength), { headers: { "Content-Type": "image/jpeg" } }));
    fetchMock.mockClear();
    const repair = await backend.start({ kind: "repair", packIds: [DECK_LIBRARY] });
    if (repair.status !== "started") throw new Error("corrupt deck-library repair did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(repair.operationId)).state).toBe("completed"));
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toContain(corrupt.sourceUrl);

    const legacyRow = { ...legacy } as { sourceUrl?: string } & typeof legacy;
    delete legacyRow.sourceUrl;
    await database.put("objects", legacyRow);
    database.close();
    await expect(backend.start({ kind: "repair", packIds: [DECK_LIBRARY] })).rejects.toMatchObject({ kind: "invalid_input" });
  });

  it("repairs from its recorded source instead of adopting a corrupt cross-pack donor", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const installed = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (installed.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(installed.operationId)).state).toBe("completed"));
    const database = await openDB(DATABASE);
    const [receipt] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    if (!receipt) throw new Error("deck-library install wrote no receipt row");
    const core = packId("core");
    const donorPath = "/visual-packs/v1/corrupt-cross-pack-donor.jpg";
    await database.put("objects", {
      ...receipt,
      id: `${OBJECT_DIGEST}:${core}:${receipt.assetKey}`,
      root: OBJECT_DIGEST,
      packId: core,
      path: donorPath,
    });
    await database.put("packs", { id: core, packId: core, root: OBJECT_DIGEST, dependencies: [], operationId: OPERATION });
    database.close();
    cache.entries.delete(receipt.path);
    cache.entries.set(donorPath, new Response("not the source", { headers: { "Content-Type": "image/jpeg" } }));
    fetchMock.mockClear();

    const repair = await backend.start({ kind: "repair", packIds: [DECK_LIBRARY] });
    if (repair.status !== "started") throw new Error("deck-library repair did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(repair.operationId)).state).toBe("completed"));
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toContain(receipt.sourceUrl);
    const restored = cache.entries.get(receipt.path);
    if (!restored) throw new Error("repair did not restore the receipt cache entry");
    expect(new TextDecoder().decode(await restored.arrayBuffer())).toBe(receipt.sourceUrl);
  });

  it("keeps a failed repair resumable as repair and publishes a repair revision", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const revisions: string[] = [];
    const failures: string[] = [];
    await backend.subscribeRevision((event) => revisions.push(event.cause));
    await backend.subscribeProgress((event) => { if (event.phase === "failed") failures.push(event.error ?? "none"); });
    const installed = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (installed.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(installed.operationId)).state).toBe("completed"));
    const database = await openDB(DATABASE);
    const rows = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    database.close();
    const row = rows.find((entry) => entry.sourceUrl === SECOND.sourceUrl);
    if (!row) throw new Error("deck-library install wrote no second-image row");
    cache.entries.delete(row.path);
    failImages = true;
    const repair = await backend.start({ kind: "repair", packIds: [DECK_LIBRARY] });
    if (repair.status !== "started") throw new Error("deck-library repair did not start");
    await vi.waitFor(() => expect(failures).toEqual(["network"]));
    expect((await backend.operationStatus(repair.operationId)).kind).toBe("repair");

    failImages = false;
    const resumed = await backend.start({ kind: "resume", operationId: repair.operationId });
    if (resumed.status !== "started") throw new Error("deck-library repair resume did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(repair.operationId)).state).toBe("completed"));
    expect((await backend.operationStatus(repair.operationId)).kind).toBe("repair");
    expect(revisions).toContain("repair");
  });

  it("auto-resumes a durable failed repair as repair after backend recreation", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const failures: string[] = [];
    await backend.subscribeProgress((event) => { if (event.phase === "failed") failures.push(event.error ?? "none"); });
    const installed = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (installed.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(installed.operationId)).state).toBe("completed"));
    const database = await openDB(DATABASE);
    const rows = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    database.close();
    const row = rows.find((entry) => entry.sourceUrl === SECOND.sourceUrl);
    if (!row) throw new Error("deck-library install wrote no second-image row");
    cache.entries.delete(row.path);
    failImages = true;
    const failed = await backend.start({ kind: "repair", packIds: [DECK_LIBRARY] });
    if (failed.status !== "started") throw new Error("deck-library repair did not start");
    await vi.waitFor(() => expect(failures).toEqual(["network"]));

    failImages = false;
    holdSecondImage = true;
    const recreated = await ScryfallBrowserVisualPackBackend.create();
    const revisions: string[] = [];
    await recreated.subscribeRevision((event) => revisions.push(event.cause));
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    releaseSecondImage?.();
    await vi.waitFor(async () => expect((await recreated.operationStatus(failed.operationId)).state).toBe("completed"));
    expect((await recreated.operationStatus(failed.operationId)).kind).toBe("repair");
    expect(revisions).toContain("repair");
  });

  it("reconciles to an empty deck-library membership while retaining its receipt", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first: InstallSelector = { kind: "deck_library", membershipDigest: PLANNED_DIGEST };
    const installed = await backend.start({ kind: "install", selector: first, objectEstimate: 2 });
    if (installed.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(installed.operationId)).state).toBe("completed"));

    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [] };
    const empty = await backend.start({ kind: "install", selector: { kind: "deck_library", membershipDigest: EMPTY_DIGEST }, objectEstimate: 0 });
    if (empty.status !== "started") throw new Error("empty deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(empty.operationId)).state).toBe("completed"));

    const database = await openDB(DATABASE);
    expect(await database.get("packs", DECK_LIBRARY)).toMatchObject({ root: EMPTY_DIGEST });
    expect(await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual([]);
    database.close();
  });

  it("deltas add, remove, and refresh deck-library rows while reusing unchanged content", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [FIRST, SECOND, REMOVED] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await backend.start({ kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 3 });
    if (first.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(first.operationId)).state).toBe("completed"));

    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED, THIRD] };
    fetchMock.mockClear();
    const second = await backend.start({ kind: "install", selector: { kind: "deck_library", membershipDigest: EMPTY_DIGEST }, objectEstimate: 3 });
    if (second.status !== "started") throw new Error("deck-library delta did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(second.operationId)).state).toBe("completed"));

    const requested = fetchMock.mock.calls.map(([input]) => String(input));
    expect(requested).toContain(MOVED.sourceUrl);
    expect(requested).toContain(THIRD.sourceUrl);
    expect(requested).not.toContain(FIRST.sourceUrl);
    const database = await openDB(DATABASE);
    const rows = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    expect(rows.map((row) => row.assetKey).sort()).toEqual([FIRST, MOVED, THIRD].map((value) => value.assetKey).sort());
    expect(rows.every((row) => row.root === EMPTY_DIGEST)).toBe(true);
    database.close();
  });

  it("sizes and gates a deck-library delta by add plus refresh rather than its whole membership", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [FIRST, SECOND, REMOVED] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await backend.start({ kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 3 });
    if (first.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(first.operationId)).state).toBe("completed"));

    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED, THIRD] };
    const available = minimumImageBytes(2);
    Object.defineProperty(globalThis.navigator, "storage", {
      configurable: true,
      value: {
        persisted: vi.fn(async () => false), persist: vi.fn(async () => true),
        estimate: vi.fn(async () => ({ usage: 0, quota: available })),
      },
    });
    const selector: InstallSelector = { kind: "deck_library", membershipDigest: EMPTY_DIGEST };
    const estimate = await backend.estimateInstall(selector);
    expect(estimate.assetRecords).toBe("3");
    expect(estimate.uniqueObjects).toBe("2");
    expect(estimate.estimatedImageBytes).toBe(estimatedImageBytes(2));
    expect(minimumImageBytes(3)).toBeGreaterThan(available);
    const started = await backend.start({ kind: "install", selector, objectEstimate: 3 });
    if (started.status !== "started") throw new Error("deck-library delta did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(started.operationId)).state).toBe("completed"));
  });

  it("removes a cancelled first-install's terminal rows without a receipt and preserves shared cache", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    holdSecondImage = true;
    const started = await backend.start({
      kind: "install",
      selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST },
      objectEstimate: 2,
    });
    if (started.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      const rows = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
      database.close();
      expect(rows).toHaveLength(1);
    });

    const database = await openDB(DATABASE);
    const [shared] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    if (!shared) throw new Error("first image was not written");
    const core = packId("core");
    await database.put("objects", { ...shared, id: `${OBJECT_DIGEST}:${core}:${shared.assetKey}`, root: OBJECT_DIGEST, packId: core });
    await database.put("packs", { id: core, packId: core, root: OBJECT_DIGEST, dependencies: [], operationId: OPERATION });
    database.close();

    const cancelling = backend.cancel(started.operationId);
    releaseSecondImage?.();
    await cancelling;
    const beforeRemoval = await openDB(DATABASE);
    expect(await beforeRemoval.get("packs", DECK_LIBRARY)).toBeUndefined();
    expect(await beforeRemoval.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).not.toEqual([]);
    beforeRemoval.close();

    await backend.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    const afterRemoval = await openDB(DATABASE);
    expect(await afterRemoval.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual([]);
    afterRemoval.close();
    expect(cache.entries.has(shared.path)).toBe(true);
  });

  it("does not let an active first install restore removed rows through a shared cache donor", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const core = packId("core");
    const sharedPath = "/visual-packs/v1/shared-second.jpg";
    await seedPack(core, OBJECT_DIGEST);
    await seedObject(core, OBJECT_DIGEST, SECOND);
    const setup = await openDB(DATABASE);
    const coreRows = await setup.getAllFromIndex("objects", "by-pack", core);
    const [coreRow] = coreRows;
    if (!coreRow) throw new Error("core donor was not seeded");
    await setup.put("objects", { ...coreRow, path: sharedPath });
    setup.close();
    cache.entries.set(sharedPath, new Response("shared", { headers: { "Content-Type": "image/jpeg" } }));
    holdCachePath = sharedPath;

    const started = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (started.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect(await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toHaveLength(1);
      database.close();
    });
    await vi.waitFor(() => expect(releaseCacheMatch).not.toBeNull());
    const activeRows = await openDB(DATABASE);
    const [firstRow] = await activeRows.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    activeRows.close();
    if (!firstRow) throw new Error("first-install row was not written");

    const removing = backend.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect((await database.get("operations", started.operationId))?.state).toBe("cancelled");
      database.close();
    });
    releaseCacheMatch?.();
    await removing;

    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toBeUndefined();
    expect(await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual([]);
    after.close();
    expect(cache.entries.has(firstRow.path)).toBe(false);
    expect(cache.entries.has(sharedPath)).toBe(true);
  });

  it("publishes a committed removal before slow cache cleanup finishes", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await backend.refreshCatalog();
    await seedPack(DECK_LIBRARY, PLANNED_DIGEST);
    await seedObject(DECK_LIBRARY, PLANNED_DIGEST, FIRST);
    const database = await openDB(DATABASE);
    const [stored] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    database.close();
    if (!stored) throw new Error("seed object was not written");
    cache.entries.set(stored.path, new Response("image", { headers: { "Content-Type": "image/jpeg" } }));
    holdCacheDeletePath = stored.path;
    const revisions: string[] = [];
    await backend.subscribeRevision((event) => revisions.push(event.revision));

    const removing = backend.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    await vi.waitFor(() => expect(releaseCacheDelete).not.toBeNull());

    expect(revisions).toHaveLength(1);
    expect((await backend.catalogSummary()).installedPacks).toEqual([]);

    releaseCacheDelete?.();
    await removing;
  });

  it("restores a paused deck-library download in the settings panel", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await backend.refreshCatalog();
    holdSecondImage = true;
    const started = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (started.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => {
      expect((await backend.operationStatus(started.operationId)).objectsPromoted).toBe(1);
    });
    platform.load.mockResolvedValue(backend);
    render(<VisualPackManager />);

    expect(await screen.findByText("1/2")).toBeInTheDocument();

    releaseSecondImage?.();
    await vi.waitFor(async () => expect((await backend.operationStatus(started.operationId)).state).toBe("completed"));
  });

  it("does not replay a stale snapshot after live progress arrives during subscription", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await backend.refreshCatalog();
    holdSecondImage = true;
    const started = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (started.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const privateBackend = backend as unknown as {
      database: { getAll(store: string): Promise<unknown[]> };
      emit(event: ProgressEvent): void;
    };
    const originalGetAll = privateBackend.database.getAll.bind(privateBackend.database);
    const snapshot = deferred<unknown[]>();
    let snapshotRead = false;
    privateBackend.database.getAll = async () => {
      snapshotRead = true;
      return snapshot.promise;
    };
    const events: ProgressEvent[] = [];
    const subscribed = backend.subscribeProgress((event) => events.push(event));
    await vi.waitFor(() => expect(snapshotRead).toBe(true));
    const live = await backend.operationStatus(started.operationId);
    privateBackend.emit({ phase: "failed", operation: { ...live, objectsPromoted: 0 }, error: "network" });
    snapshot.resolve(await originalGetAll("operations"));
    const unlisten = await subscribed;

    expect(events).toEqual([expect.objectContaining({ phase: "failed", error: "network" })]);
    unlisten();
    releaseSecondImage?.();
    await backend.cancel(started.operationId);
  });

  it("removes both an installed receipt and a cancelled delta root while retaining shared cache", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    const database = await openDB(DATABASE);
    const [shared] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    if (!shared) throw new Error("deck-library install wrote no rows");
    const core = packId("core");
    await database.put("objects", { ...shared, id: `${OBJECT_DIGEST}:${core}:${shared.assetKey}`, root: OBJECT_DIGEST, packId: core });
    await database.put("packs", { id: core, packId: core, root: OBJECT_DIGEST, dependencies: [], operationId: OPERATION });
    database.close();

    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    const delta = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: EMPTY_DIGEST }, objectEstimate: 2,
    });
    if (delta.status !== "started") throw new Error("deck-library delta did not start");
    await vi.waitFor(async () => {
      const rows = await openDB(DATABASE);
      expect((await rows.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).some((row) => row.root === EMPTY_DIGEST)).toBe(true);
      rows.close();
    });
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const cancelling = backend.cancel(delta.operationId);
    releaseSecondImage?.();
    await cancelling;

    await backend.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toBeUndefined();
    expect(await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual([]);
    after.close();
    expect(cache.entries.has(shared.path)).toBe(true);
  });

  it("collects an abandoned deck-library root after delta promotion", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (first.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(first.operationId)).state).toBe("completed"));
    const abandonedPath = "/visual-packs/v1/deck-library-abandoned.jpg";
    const database = await openDB(DATABASE);
    await database.put("objects", {
      id: `${INSTALLED_DIGEST}:${DECK_LIBRARY}:${REMOVED.assetKey}`,
      root: INSTALLED_DIGEST,
      packId: DECK_LIBRARY,
      assetKey: REMOVED.assetKey,
      candidateKeys: [],
      sourceUrl: REMOVED.sourceUrl,
      object: OBJECT_DIGEST,
      byteLength: 1,
      media: "image/jpeg",
      path: abandonedPath,
    });
    const catalog = (await database.get("state", "state"))?.catalog;
    if (!catalog) throw new Error("install did not persist a catalog");
    await database.put("operations", {
      id: OPERATION,
      kind: "install",
      state: "cancelled",
      catalog,
      selectors: [{ kind: "deck_library", membershipDigest: INSTALLED_DIGEST }],
      packIds: [DECK_LIBRARY],
      packTotal: 1,
      packsPromoted: 0,
      objectTotal: 1,
      objectsPromoted: 1,
      completedRevision: null,
    });
    database.close();
    cache.entries.set(abandonedPath, new Response("x", { headers: { "Content-Type": "image/jpeg" } }));

    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST] };
    const delta = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: EMPTY_DIGEST }, objectEstimate: 1,
    });
    if (delta.status !== "started") throw new Error("deck-library delta did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(delta.operationId)).state).toBe("completed"));
    const after = await openDB(DATABASE);
    expect((await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).every((row) => row.root === EMPTY_DIGEST)).toBe(true);
    after.close();
    expect(cache.entries.has(abandonedPath)).toBe(false);
  });

  it("is an installed-only no-op before it plans, fetches, persists, or creates work", async () => {
    installWebLocks();
    const persistence = vi.fn(async () => false);
    Object.defineProperty(globalThis.navigator, "storage", {
      configurable: true,
      value: { persisted: vi.fn(async () => false), persist: persistence, estimate: vi.fn(async () => ({})) },
    });
    const backend = await ScryfallBrowserVisualPackBackend.create();

    await expect(backend.reconcileDeckLibrary()).resolves.toBeUndefined();

    expect(state.invalidate).not.toHaveBeenCalled();
    expect(state.plan).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(persistence).not.toHaveBeenCalled();
    const database = await openDB(DATABASE);
    expect(await database.getAll("operations")).toEqual([]);
    database.close();
  });

  it("reconciles an installed deck-library delta without requesting persistence", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    const before = await openDB(DATABASE);
    const generation = (await before.get("packs", DECK_LIBRARY))?.optInGeneration;
    before.close();

    const order: string[] = [];
    state.invalidate.mockImplementation(() => { order.push("invalidate"); });
    state.plan.mockImplementation(async () => {
      order.push("plan");
      return state.membership;
    });
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, THIRD] };
    fetchMock.mockClear();
    const persistence = vi.fn(async () => false);
    Object.defineProperty(globalThis.navigator, "storage", {
      configurable: true,
      value: { persisted: vi.fn(async () => false), persist: persistence, estimate: vi.fn(async () => ({})) },
    });
    installWebLocks();

    await backend.reconcileDeckLibrary();

    expect(order.slice(0, 2)).toEqual(["invalidate", "plan"]);
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toContain(THIRD.sourceUrl);
    expect(fetchMock.mock.calls.map(([input]) => String(input))).not.toContain(FIRST.sourceUrl);
    expect(persistence).not.toHaveBeenCalled();
    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toMatchObject({
      root: EMPTY_DIGEST,
      optInGeneration: generation,
    });
    after.close();
  });

  it("requires Web Locks for background reconciliation without creating work", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.plan.mockClear();

    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "unavailable" });

    expect(state.plan).not.toHaveBeenCalled();
    const database = await openDB(DATABASE);
    expect(await database.getAll("operations")).toHaveLength(1);
    database.close();
  });

  it("preserves installed receipt, rows, cache, revision, and work when fresh membership planning fails", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    const database = await openDB(DATABASE);
    const receipt = await database.get("packs", DECK_LIBRARY);
    const rows = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    const operations = await database.getAll("operations");
    const catalogState = await database.get("state", "state");
    database.close();
    const cached = await Promise.all([...cache.entries].map(async ([path, response]) => [
      path,
      await response.clone().text(),
    ] as const));
    const revisions: string[] = [];
    await backend.subscribeRevision((event) => revisions.push(event.revision));
    const persistence = vi.fn(async () => false);
    Object.defineProperty(globalThis.navigator, "storage", {
      configurable: true,
      value: { persisted: vi.fn(async () => false), persist: persistence, estimate: vi.fn(async () => ({})) },
    });
    state.plan.mockRejectedValueOnce(new VisualPackBackendError("network"));
    installWebLocks();
    fetchMock.mockClear();

    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });

    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toEqual(receipt);
    expect(await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual(rows);
    expect(await after.getAll("operations")).toEqual(operations);
    expect(await after.get("state", "state")).toEqual(catalogState);
    after.close();
    expect(await Promise.all([...cache.entries].map(async ([path, response]) => [
      path,
      await response.clone().text(),
    ] as const))).toEqual(cached);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(persistence).not.toHaveBeenCalled();
    expect(revisions).toEqual([]);

    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, THIRD] };
    await backend.reconcileDeckLibrary();
    const recovered = await openDB(DATABASE);
    expect(await recovered.get("packs", DECK_LIBRARY)).toMatchObject({ root: EMPTY_DIGEST });
    recovered.close();
    expect(revisions).toHaveLength(1);
  });

  it("uses one origin-wide worker for concurrent backend instances", async () => {
    const first = await ScryfallBrowserVisualPackBackend.create();
    const initial = await first.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await first.operationStatus(initial.operationId)).state).toBe("completed"));
    const second = await ScryfallBrowserVisualPackBackend.create();
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();
    fetchMock.mockClear();
    const firstRevisions: string[] = [];
    const secondRevisions: string[] = [];
    await first.subscribeRevision((event) => firstRevisions.push(event.revision));
    await second.subscribeRevision((event) => secondRevisions.push(event.revision));

    const fromFirst = first.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const fromSecond = second.reconcileDeckLibrary();
    releaseSecondImage?.();
    await Promise.all([fromFirst, fromSecond]);

    expect(fetchMock.mock.calls.filter(([input]) => String(input) === MOVED.sourceUrl)).toHaveLength(1);
    const database = await openDB(DATABASE);
    expect(await database.get("packs", DECK_LIBRARY)).toMatchObject({ root: EMPTY_DIGEST });
    expect((await database.getAll("operations")).filter((operation) => operation.background)).toHaveLength(1);
    database.close();
    expect(firstRevisions).toHaveLength(1);
    expect(secondRevisions).toEqual(firstRevisions);
  });

  it("runs a queued newer membership after an active reconciliation settles", async () => {
    const first = await ScryfallBrowserVisualPackBackend.create();
    const initial = await first.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await first.operationStatus(initial.operationId)).state).toBe("completed"));
    const second = await ScryfallBrowserVisualPackBackend.create();
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();

    const d2 = first.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [FIRST, SECOND] };
    const d1 = second.reconcileDeckLibrary();
    releaseSecondImage?.();
    holdSecondImage = false;
    await Promise.all([d2, d1]);

    const database = await openDB(DATABASE);
    expect(await database.get("packs", DECK_LIBRARY)).toMatchObject({ root: PLANNED_DIGEST });
    database.close();
  });

  it("carries a stable opt-in generation through queued newer memberships", async () => {
    const first = await ScryfallBrowserVisualPackBackend.create();
    const initial = await first.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await first.operationStatus(initial.operationId)).state).toBe("completed"));
    const second = await ScryfallBrowserVisualPackBackend.create();
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();

    const d2 = first.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    state.membership = { membershipDigest: INSTALLED_DIGEST, descriptors: [FIRST, THIRD] };
    const d3 = second.reconcileDeckLibrary();
    holdSecondImage = false;
    releaseSecondImage?.();
    await Promise.all([d2, d3]);

    const database = await openDB(DATABASE);
    expect(await database.get("packs", DECK_LIBRARY)).toMatchObject({ root: INSTALLED_DIGEST });
    const background = (await database.getAll("operations")).filter((operation) => operation.background);
    expect(background).toHaveLength(2);
    expect(background.map((operation) => operation.deckLibraryGeneration)).toEqual([
      initial.operationId,
      initial.operationId,
    ]);
    database.close();
  });

  it("keeps a retryable background failure resumable across backend recreation", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    failImages = true;
    installWebLocks();

    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });
    const failed = await openDB(DATABASE);
    const operation = (await failed.getAll("operations")).find((entry) => entry.background);
    failed.close();
    expect(operation).toMatchObject({ state: "downloading" });

    failImages = false;
    const recreated = await ScryfallBrowserVisualPackBackend.create();
    if (!operation) throw new Error("background operation was not persisted");
    await vi.waitFor(async () => expect((await recreated.operationStatus(operation.id)).state).toBe("completed"));
  });

  it("restarts a failed background sync at the same membership digest", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    failImages = true;
    installWebLocks();
    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });
    const failed = await openDB(DATABASE);
    const operation = (await failed.getAll("operations")).find((entry) => entry.background);
    failed.close();
    if (!operation) throw new Error("background operation was not persisted");

    const events: ProgressEvent[] = [];
    await backend.subscribeProgress((event) => events.push(event));
    platform.load.mockResolvedValue(backend);
    render(<VisualPackManager />);
    expect(await screen.findByRole("button", { name: /resume operation/i })).toBeInTheDocument();
    failImages = false;
    await expect(backend.reconcileDeckLibrary()).resolves.toBeUndefined();

    expect((await backend.operationStatus(operation.id)).state).toBe("completed");
    expect(await screen.findByText("Completed")).toBeInTheDocument();
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ phase: "started", operation: expect.objectContaining({ operationId: operation.id }) }),
      expect.objectContaining({ phase: "completed", operation: expect.objectContaining({ operationId: operation.id }) }),
    ]));
  });

  it("collects a failed delta when membership returns to its installed root", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED, THIRD, FOURTH] };
    failedImage = FOURTH.sourceUrl;
    installWebLocks();
    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });

    const failed = await openDB(DATABASE);
    const d2Row = (await failed.getAllFromIndex("objects", "by-pack", DECK_LIBRARY))
      .find((row) => row.root === EMPTY_DIGEST && row.sourceUrl === MOVED.sourceUrl);
    const d2OnlyRow = (await failed.getAllFromIndex("objects", "by-pack", DECK_LIBRARY))
      .find((row) => row.root === EMPTY_DIGEST && row.sourceUrl === THIRD.sourceUrl);
    const operation = (await failed.getAll("operations")).find((entry) => entry.background);
    if (!d2Row || !d2OnlyRow || !operation) throw new Error("failed delta did not retain its partial rows and operation");
    const core = packId("core");
    await failed.put("packs", { id: core, packId: core, root: OBJECT_DIGEST, dependencies: [], operationId: OPERATION });
    await failed.put("objects", {
      ...d2Row,
      id: `${OBJECT_DIGEST}:${core}:${d2Row.assetKey}`,
      root: OBJECT_DIGEST,
      packId: core,
    });
    failed.close();

    const events: ProgressEvent[] = [];
    await backend.subscribeProgress((event) => events.push(event));
    platform.load.mockResolvedValue(backend);
    render(<VisualPackManager />);
    expect(await screen.findByRole("button", { name: /resume operation/i })).toBeInTheDocument();
    failedImage = null;
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [FIRST, SECOND] };
    fetchMock.mockClear();
    await expect(backend.reconcileDeckLibrary()).resolves.toBeUndefined();

    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toMatchObject({ root: PLANNED_DIGEST });
    const d1Rows = await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    expect(d1Rows.map((row) => row.assetKey).sort()).toEqual([FIRST.assetKey, SECOND.assetKey].sort());
    expect(d1Rows.every((row) => row.root === PLANNED_DIGEST)).toBe(true);
    expect((await after.get("operations", operation.id))?.state).toBe("cancelled");
    after.close();
    expect(await screen.findByText("Cancelled")).toBeInTheDocument();
    expect(events).toContainEqual(expect.objectContaining({
      phase: "cancelled",
      operation: expect.objectContaining({ operationId: operation.id, state: "cancelled" }),
    }));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(cache.entries.has(d2Row.path)).toBe(true);
    expect(cache.entries.has(d2OnlyRow.path)).toBe(false);
  });

  it("terminalizes a failed background reconciliation before backend recreation", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    failImages = true;
    installWebLocks();
    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });
    const beforeRemoval = await openDB(DATABASE);
    const failed = (await beforeRemoval.getAll("operations")).find((operation) => operation.background);
    beforeRemoval.close();
    if (!failed) throw new Error("background operation was not persisted");
    expect(failed.state).toBe("downloading");

    await backend.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    failImages = false;
    const recreated = await ScryfallBrowserVisualPackBackend.create();
    expect((await recreated.operationStatus(failed.id)).state).toBe("cancelled");
    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toBeUndefined();
    expect(await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual([]);
    after.close();
  });

  it("makes same-instance removal win over an active background reconciliation", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();
    const active = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const removing = backend.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect(await database.get("packs", DECK_LIBRARY)).toBeUndefined();
      database.close();
    });
    releaseSecondImage?.();
    await Promise.allSettled([active, removing]);

    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toBeUndefined();
    expect(await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual([]);
    expect((await after.getAll("operations")).some((operation) => operation.background && operation.state === "cancelled")).toBe(true);
    after.close();
  });

  it("makes removal win over active and queued background reconciliations", async () => {
    const first = await ScryfallBrowserVisualPackBackend.create();
    const initial = await first.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await first.operationStatus(initial.operationId)).state).toBe("completed"));
    const second = await ScryfallBrowserVisualPackBackend.create();
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();

    const active = first.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const queued = second.reconcileDeckLibrary();
    const removing = second.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect(await database.get("packs", DECK_LIBRARY)).toBeUndefined();
      database.close();
    });
    releaseSecondImage?.();
    await Promise.allSettled([removing, active, queued]);

    const database = await openDB(DATABASE);
    expect(await database.get("packs", DECK_LIBRARY)).toBeUndefined();
    expect(await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual([]);
    expect((await database.getAll("operations")).filter((operation) => operation.background))
      .toEqual(expect.arrayContaining([expect.objectContaining({ state: "cancelled" })]));
    database.close();
  });

  it("does not let a removed generation's active reconciliation modify an explicit reinstall", async () => {
    const first = await ScryfallBrowserVisualPackBackend.create();
    const initial = await first.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await first.operationStatus(initial.operationId)).state).toBe("completed"));
    const before = await openDB(DATABASE);
    const initialReceipt = await before.get("packs", DECK_LIBRARY);
    before.close();
    if (!initialReceipt) throw new Error("deck-library install wrote no receipt");

    const second = await ScryfallBrowserVisualPackBackend.create();
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();
    const oldGeneration = first.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const removing = second.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect(await database.get("packs", DECK_LIBRARY)).toBeUndefined();
      database.close();
    });
    releaseSecondImage?.();
    await removing;

    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [FIRST, SECOND] };
    holdSecondImage = false;
    const reinstall = await second.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (reinstall.status !== "started") throw new Error("deck-library reinstall did not start");
    await Promise.allSettled([oldGeneration]);
    await vi.waitFor(async () => expect((await second.operationStatus(reinstall.operationId)).state).toBe("completed"));

    const after = await openDB(DATABASE);
    const reinstalled = await after.get("packs", DECK_LIBRARY);
    expect(reinstalled).toMatchObject({ root: PLANNED_DIGEST, operationId: reinstall.operationId });
    expect(reinstalled?.optInGeneration).not.toBe(initialReceipt.optInGeneration ?? initialReceipt.operationId);
    expect((await after.getAll("operations")).some((operation) => operation.background && operation.state === "cancelled")).toBe(true);
    after.close();
  });

  it("prevents a post-promotion worker from finalizing after removal", async () => {
    const first = await ScryfallBrowserVisualPackBackend.create();
    const initial = await first.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await first.operationStatus(initial.operationId)).state).toBe("completed"));
    const originalFinish = (first as unknown as { finish(selectedOperation: ReturnType<typeof operationId>): Promise<void> }).finish.bind(first);
    let releaseFinish!: () => void;
    const finishEntered = new Promise<void>((resolve) => {
      releaseFinish = resolve;
    });
    (first as unknown as { finish(selectedOperation: ReturnType<typeof operationId>): Promise<void> }).finish = async (selectedOperation) => {
      await finishEntered;
      await originalFinish(selectedOperation);
    };
    const phases: string[] = [];
    await first.subscribeProgress((event) => phases.push(event.phase));
    const second = await ScryfallBrowserVisualPackBackend.create();
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    installWebLocks();

    const reconciling = first.reconcileDeckLibrary();
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect((await database.get("packs", DECK_LIBRARY))?.root).toBe(EMPTY_DIGEST);
      database.close();
    });
    const removing = second.remove({ kind: "packs", packIds: [DECK_LIBRARY] }, "reject_dependents");
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect(await database.get("packs", DECK_LIBRARY)).toBeUndefined();
      database.close();
    });
    const afterRemoval = await openDB(DATABASE);
    const revision = (await afterRemoval.get("state", "state"))?.revision;
    afterRemoval.close();
    releaseFinish();
    await Promise.all([removing, reconciling]);

    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toBeUndefined();
    expect((await after.get("state", "state"))?.revision).toBe(revision);
    after.close();
    expect(phases).not.toContain("completed");
  });

  it("cancels a recreated finalizing worker when its receipt ownership changes", async () => {
    const initialBackend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await initialBackend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await initialBackend.operationStatus(initial.operationId)).state).toBe("completed"));
    const database = await openDB(DATABASE);
    const receipt = await database.get("packs", DECK_LIBRARY);
    const current = await database.get("state", "state");
    if (!receipt || !current?.catalog) throw new Error("deck-library install did not persist receipt and catalog");
    await database.put("packs", { ...receipt, operationId: OPERATION });
    await database.put("operations", {
      id: OPERATION,
      kind: "install",
      state: "finalizing",
      catalog: current.catalog,
      selectors: [{ kind: "deck_library", membershipDigest: PLANNED_DIGEST }],
      packIds: [DECK_LIBRARY],
      packTotal: 1,
      packsPromoted: 1,
      objectTotal: 2,
      objectsPromoted: 2,
      completedRevision: null,
      deckLibraryGeneration: receipt.optInGeneration ?? receipt.operationId,
      background: true,
    });
    database.close();
    installWebLocks();

    const prototype = ScryfallBrowserVisualPackBackend.prototype as unknown as {
      finish(selectedOperation: ReturnType<typeof operationId>): Promise<void>;
    };
    const originalFinish = prototype.finish;
    let markFinishEntered!: () => void;
    const finishStarted = new Promise<void>((resolve) => {
      markFinishEntered = resolve;
    });
    let releaseFinish!: () => void;
    const finishGate = new Promise<void>((resolve) => {
      releaseFinish = resolve;
    });
    prototype.finish = async function finish(selectedOperation) {
      markFinishEntered();
      await finishGate;
      await originalFinish.call(this, selectedOperation);
    };
    try {
      const recreated = await ScryfallBrowserVisualPackBackend.create();
      await finishStarted;
      const progress: ProgressEvent[] = [];
      const revisions: string[] = [];
      await recreated.subscribeProgress((event) => progress.push(event));
      await recreated.subscribeRevision((event) => revisions.push(event.revision));
      const changed = await openDB(DATABASE);
      const changedReceipt = await changed.get("packs", DECK_LIBRARY);
      if (!changedReceipt) throw new Error("deck-library receipt disappeared before generation fence");
      await changed.put("packs", { ...changedReceipt, operationId: operationId("1".repeat(32)) });
      changed.close();
      releaseFinish();
      await vi.waitFor(async () => expect((await recreated.operationStatus(OPERATION)).state).toBe("cancelled"));
      expect(progress[progress.length - 1]).toEqual(expect.objectContaining({
        phase: "cancelled",
        operation: expect.objectContaining({ state: "cancelled" }),
        error: null,
      }));
      expect(revisions).toEqual([]);
      const after = await openDB(DATABASE);
      expect((await after.get("state", "state"))?.revision).toBe(current.revision);
      after.close();
    } finally {
      releaseFinish();
      prototype.finish = originalFinish;
    }
  });
});
