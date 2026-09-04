import "fake-indexeddb/auto";

import { IDBFactory } from "fake-indexeddb";
import { openDB } from "idb";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { VisualPackManager } from "../../../../components/settings/visual-packs/VisualPackManager.tsx";
import { isDeckLibraryBackgroundLifecycle, VisualPackBackendError, type VisualPackBackend } from "../../backend.ts";
import { CARD_CANDIDATE_PROJECTION_VERSION, semanticCardCandidateGroups } from "../../candidateKeys.ts";
import { assetKey, catalogRoot, estimatedImageBytes, minimumImageBytes, operationId, packId } from "../../types.ts";
import type { DeckLibraryInstallSelector, InstallSelector, ProgressEvent } from "../../types.ts";
import type { ScryfallAssetDescriptor } from "../descriptors.ts";
import { setScryfallTransactionGateForTests, ScryfallBrowserVisualPackBackend } from "../scryfallBackend.ts";


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

function projectedDescriptor(
  value: ScryfallAssetDescriptor,
  oracleId: string,
  cardName: string,
  faceName: string,
): ScryfallAssetDescriptor {
  return {
    ...value,
    candidateKeys: semanticCardCandidateGroups({
      oracleId,
      sourceSetCode: "M21",
      sourceCollectorNumber: "123",
      cardName,
      faceName,
      variant: "full_card",
      rung: "normal",
    }).flatMap((group) => group.keys),
  };
}

const PROJECTED_FIRST = projectedDescriptor(FIRST, "11111111-abcd-4111-8111-111111111111", "First", "First Face");
const PROJECTED_SECOND = projectedDescriptor(SECOND, "22222222-abcd-4222-8222-222222222222", "Second", "Second Face");

function contentFields(row: Record<string, unknown>) {
  return {
    id: row.id,
    root: row.root,
    packId: row.packId,
    assetKey: row.assetKey,
    sourceUrl: row.sourceUrl,
    object: row.object,
    byteLength: row.byteLength,
    media: row.media,
    path: row.path,
  };
}

async function makeDeckLibraryReceiptLegacy(version?: number): Promise<void> {
  const database = await openDB(DATABASE);
  const receipt = await database.get("packs", DECK_LIBRARY);
  if (!receipt) throw new Error("deck-library receipt was not installed");
  const legacyReceipt = { ...receipt } as { candidateProjectionVersion?: number } & typeof receipt;
  if (version === undefined) delete legacyReceipt.candidateProjectionVersion;
  else legacyReceipt.candidateProjectionVersion = version;
  await database.put("packs", legacyReceipt);
  for (const row of await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)) {
    await database.put("objects", { ...row, candidateKeys: [] });
  }
  database.close();
}

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

async function installDeckLibrary(backend: ScryfallBrowserVisualPackBackend): Promise<void> {
  const started = await backend.start({
    kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
  });
  if (started.status !== "started") throw new Error("deck-library install did not start");
  await vi.waitFor(async () => expect((await backend.operationStatus(started.operationId)).state).toBe("completed"));
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
    setScryfallTransactionGateForTests(null);
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
    setScryfallTransactionGateForTests(null);
  });

  it("registers deck_library as an installable validated identity", () => {
    const selector: DeckLibraryInstallSelector = { kind: "deck_library", membershipDigest: PLANNED_DIGEST };
    const installSelector: InstallSelector = selector;

    expect(packId("deck_library")).toBe(DECK_LIBRARY);
    expect(selector).toEqual({ kind: "deck_library", membershipDigest: PLANNED_DIGEST });
    expect(installSelector).toBe(selector);
    expect(() => packId("deck-library")).toThrow("invalid PackId");
  });

  it("requires both background lifecycle methods before narrowing the optional capability", () => {
    const pauseOnly = {
      setDeckLibraryBackgroundPaused: async () => {},
    } as unknown as VisualPackBackend;
    const complete = {
      setDeckLibraryBackgroundPaused: async () => {},
      prepareDeckLibraryForOffline: async () => "ready" as const,
    } as unknown as VisualPackBackend;

    expect(isDeckLibraryBackgroundLifecycle(pauseOnly)).toBe(false);
    expect(isDeckLibraryBackgroundLifecycle(complete)).toBe(true);
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
    const sameLengthCorruption = originalBytes.slice();
    sameLengthCorruption[0] ^= 0xff;
    cache.entries.set(corrupt.path, new Response(sameLengthCorruption, { headers: { "Content-Type": "image/jpeg" } }));
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

  it("reports not-installed without planning, fetching, writing, or creating work", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await backend.setDeckLibraryBackgroundPaused(false);

    await expect(backend.prepareDeckLibraryForOffline()).resolves.toBe("not-installed");

    expect(state.invalidate).not.toHaveBeenCalled();
    expect(state.plan).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();
    const database = await openDB(DATABASE);
    expect(await database.getAll("operations")).toEqual([]);
    expect(await database.getAll("packs")).toEqual([]);
    expect(await database.getAll("objects")).toEqual([]);
    database.close();
  });

  it("awaits reconciliation and verifies the final installed receipt is current", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);

    await expect(backend.prepareDeckLibraryForOffline()).resolves.toBe("ready");
  });

  it("reprojects a v1 same-root Deck Catalog from verified cache bytes without image fetches", async () => {
    state.cardDataResident = true;
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await makeDeckLibraryReceiptLegacy(1);
    const database = await openDB(DATABASE);
    const beforeReceipt = await database.get("packs", DECK_LIBRARY);
    const beforeRows = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    const beforeBytes = await Promise.all(beforeRows.map(async (row) => {
      const response = await cache.match(row.path);
      if (!response) throw new Error("installed cache entry is missing");
      return [row.path, new Uint8Array(await response.arrayBuffer())] as const;
    }));
    database.close();
    const revisions: string[] = [];
    await backend.subscribeRevision((event) => revisions.push(event.cause));
    fetchMock.mockClear();
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    await expect(backend.prepareDeckLibraryForOffline()).resolves.toBe("ready");

    const after = await openDB(DATABASE);
    const receipt = await after.get("packs", DECK_LIBRARY);
    const rows = await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    after.close();
    expect(beforeReceipt).toMatchObject({ root: PLANNED_DIGEST, candidateProjectionVersion: 1 });
    expect(receipt).toMatchObject({
      root: PLANNED_DIGEST,
      optInGeneration: beforeReceipt?.optInGeneration,
      candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
    });
    expect(rows.map(contentFields)).toEqual(beforeRows.map(contentFields));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(revisions).toEqual(["install"]);
    for (const [path, bytes] of beforeBytes) {
      const response = await cache.match(path);
      if (!response) throw new Error("reprojection removed a cache entry");
      expect(new Uint8Array(await response.arrayBuffer())).toEqual(bytes);
    }
    for (const descriptor of state.membership.descriptors) {
      const row = rows.find((entry) => entry.assetKey === descriptor.assetKey);
      expect(row?.candidateKeys).toEqual(descriptor.candidateKeys);
      const resolved = await backend.resolve(descriptor.candidateKeys.map((key) => ({ kind: "candidate" as const, key })));
      expect(resolved.entries.every((entry) => entry.matches.length === 1)).toBe(true);
    }
  });

  it("does no reconciliation work for a current v2 same-root candidate projection", async () => {
    state.cardDataResident = true;
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    const database = await openDB(DATABASE);
    const beforeOperations = await database.getAll("operations");
    const beforeRows = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    database.close();
    const revisions: string[] = [];
    await backend.subscribeRevision((event) => revisions.push(event.cause));
    fetchMock.mockClear();
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    await backend.reconcileDeckLibrary();

    const after = await openDB(DATABASE);
    expect(await after.getAll("operations")).toEqual(beforeOperations);
    expect(await after.getAllFromIndex("objects", "by-pack", DECK_LIBRARY)).toEqual(beforeRows);
    after.close();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(revisions).toEqual([]);
  });

  it("fully verifies matching-key bytes before a current projection can complete", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    const database = await openDB(DATABASE);
    const receipt = await database.get("packs", DECK_LIBRARY);
    const [first] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    if (!receipt || !first) throw new Error("deck-library install did not persist receipt and rows");
    const legacyReceipt = { ...receipt } as { candidateProjectionVersion?: number } & typeof receipt;
    delete legacyReceipt.candidateProjectionVersion;
    await database.put("packs", legacyReceipt);
    const cached = await cache.match(first.path);
    if (!cached) throw new Error("deck-library cache entry is missing");
    const corrupted = new Uint8Array(await cached.arrayBuffer());
    corrupted[0] ^= 0xff;
    cache.entries.set(first.path, new Response(corrupted, { headers: { "Content-Type": "image/jpeg" } }));
    database.close();
    fetchMock.mockClear();

    const started = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (started.status !== "started") throw new Error("legacy same-root install did not create projection work");
    await vi.waitFor(async () => expect((await backend.operationStatus(started.operationId)).state).toBe("completed"));

    expect(await backend.operationStatus(started.operationId)).toMatchObject({ objectTotal: 2, objectsPromoted: 2 });
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toContain(PROJECTED_FIRST.sourceUrl);
    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toMatchObject({
      operationId: started.operationId,
      candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
    });
    after.close();
  });

  it.each([
    "missing cache bytes",
    "same-length corrupt cache bytes",
    "malformed content digest",
    "zero byte length",
    "positive wrong byte length",
    "unsafe byte length",
    "noncanonical cache path",
    "mismatched row root",
    "mismatched row pack",
    "mismatched row asset",
    "mismatched source URL",
    "mismatched media",
  ] as const)("downloads instead of metadata-reusing %s", async (invalidity) => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await makeDeckLibraryReceiptLegacy();
    const database = await openDB(DATABASE);
    const id = `${PLANNED_DIGEST}:${DECK_LIBRARY}:${PROJECTED_FIRST.assetKey}`;
    const row = await database.get("objects", id) as unknown as Record<string, unknown>;
    if (!row) throw new Error("deck-library row was not installed");
    switch (invalidity) {
      case "missing cache bytes":
        cache.entries.delete(row.path as string);
        break;
      case "same-length corrupt cache bytes": {
        const response = await cache.match(row.path as string);
        if (!response) throw new Error("deck-library cache entry is missing");
        const corrupt = new Uint8Array(await response.arrayBuffer());
        corrupt[0] ^= 0xff;
        cache.entries.set(row.path as string, new Response(corrupt, { headers: { "Content-Type": "image/jpeg" } }));
        break;
      }
      case "malformed content digest":
        await database.put("objects", { ...row, object: "not-a-content-digest" });
        break;
      case "zero byte length":
        await database.put("objects", { ...row, byteLength: 0 });
        break;
      case "positive wrong byte length":
        await database.put("objects", { ...row, byteLength: (row.byteLength as number) + 1 });
        break;
      case "unsafe byte length":
        await database.put("objects", { ...row, byteLength: Number.MAX_SAFE_INTEGER + 1 });
        break;
      case "noncanonical cache path":
        await database.put("objects", { ...row, path: "/not-a-visual-pack-path" });
        break;
      case "mismatched row root":
        await database.put("objects", { ...row, root: INSTALLED_DIGEST });
        break;
      case "mismatched row pack":
        await database.put("objects", { ...row, packId: CURATED });
        break;
      case "mismatched row asset":
        await database.put("objects", { ...row, assetKey: assetKey("asset:v1:canonical_card:mismatched") });
        break;
      case "mismatched source URL":
        await database.put("objects", { ...row, sourceUrl: "https://cards.example/wrong.jpg" });
        break;
      case "mismatched media":
        await database.put("objects", { ...row, media: "image/svg+xml" });
        break;
    }
    database.close();
    fetchMock.mockClear();
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    await backend.reconcileDeckLibrary();

    expect(fetchMock.mock.calls.map(([input]) => String(input))).toContain(PROJECTED_FIRST.sourceUrl);
    const after = await openDB(DATABASE);
    expect(await after.get("packs", DECK_LIBRARY)).toMatchObject({
      candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
    });
    expect(await after.get("objects", id)).toMatchObject({
      candidateKeys: PROJECTED_FIRST.candidateKeys,
      sourceUrl: PROJECTED_FIRST.sourceUrl,
      media: PROJECTED_FIRST.media,
    });
    after.close();
  });

  it("leaves a legacy receipt unstamped when projection recovery fails", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await makeDeckLibraryReceiptLegacy();
    const database = await openDB(DATABASE);
    const row = await database.get("objects", `${PLANNED_DIGEST}:${DECK_LIBRARY}:${PROJECTED_FIRST.assetKey}`);
    if (!row) throw new Error("deck-library row was not installed");
    cache.entries.delete(row.path);
    database.close();
    failImages = true;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });

    const after = await openDB(DATABASE);
    const receipt = await after.get("packs", DECK_LIBRARY) as { candidateProjectionVersion?: number } | undefined;
    after.close();
    expect(receipt?.candidateProjectionVersion).toBeUndefined();
  });

  it("supersedes a v1 nonterminal operation that already promoted its receipt", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await makeDeckLibraryReceiptLegacy(1);
    const database = await openDB(DATABASE);
    const [completed] = await database.getAll("operations");
    const receipt = await database.get("packs", DECK_LIBRARY);
    if (!completed || !receipt) throw new Error("deck-library installation was not persisted");
    const legacyOperationId = operationId("9".repeat(32));
    const legacyOperation = {
      ...completed,
      id: legacyOperationId,
      state: "downloading" as const,
      completedRevision: null,
      deckLibraryGeneration: receipt.optInGeneration,
      background: true,
      candidateProjectionVersion: 1,
    } as { candidateProjectionVersion?: number } & typeof completed;
    await database.put("operations", legacyOperation);
    await database.put("packs", { ...receipt, operationId: legacyOperationId, candidateProjectionVersion: 1 });
    database.close();
    fetchMock.mockClear();
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    await backend.reconcileDeckLibrary();

    const after = await openDB(DATABASE);
    const superseded = await after.get("operations", legacyOperationId) as { candidateProjectionVersion?: number; state: string } | undefined;
    const fresh = (await after.getAll("operations")).find((operation) =>
      operation.id !== completed.id && operation.id !== legacyOperationId);
    const finalReceipt = await after.get("packs", DECK_LIBRARY);
    after.close();
    expect(superseded).toMatchObject({ state: "cancelled" });
    expect(superseded?.candidateProjectionVersion).toBe(1);
    expect(fresh).toMatchObject({
      state: "completed",
      candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
    });
    expect(finalReceipt).toMatchObject({
      operationId: fresh?.id,
      candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("cancels v1 Deck Catalog work before automatic or manual resume", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const initial = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(initial);
    const database = await openDB(DATABASE);
    const [completed] = await database.getAll("operations");
    if (!completed) throw new Error("deck-library installation did not persist an operation");
    const automaticOperationId = operationId("8".repeat(32));
    const automatic = {
      ...completed,
      id: automaticOperationId,
      state: "downloading" as const,
      completedRevision: null,
      background: false,
      repairDescriptors: [PROJECTED_FIRST],
      candidateProjectionVersion: 1,
    } as { candidateProjectionVersion?: number } & typeof completed;
    await database.put("operations", automatic);
    database.close();
    fetchMock.mockClear();

    const recreated = await ScryfallBrowserVisualPackBackend.create();

    const afterAutomatic = await openDB(DATABASE);
    const cancelledAutomatic = await afterAutomatic.get("operations", automaticOperationId);
    expect(cancelledAutomatic).toMatchObject({ state: "cancelled" });
    expect(cancelledAutomatic?.candidateProjectionVersion).toBe(1);
    const manualOperationId = operationId("7".repeat(32));
    const manual = {
      ...automatic,
      id: manualOperationId,
    };
    await afterAutomatic.put("operations", manual);
    afterAutomatic.close();

    await expect(recreated.start({ kind: "resume", operationId: manualOperationId })).rejects.toMatchObject({ kind: "cancelled" });

    const afterManual = await openDB(DATABASE);
    const cancelledManual = await afterManual.get("operations", manualOperationId);
    expect(cancelledManual).toMatchObject({ state: "cancelled" });
    expect(cancelledManual?.candidateProjectionVersion).toBe(1);
    afterManual.close();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("preserves a legacy projection marker when a deck-library repair verifies and restores bytes", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await makeDeckLibraryReceiptLegacy();
    const database = await openDB(DATABASE);
    const [row] = await database.getAllFromIndex("objects", "by-pack", DECK_LIBRARY);
    database.close();
    if (!row) throw new Error("deck-library row was not installed");
    cache.entries.delete(row.path);

    const repaired = await backend.start({ kind: "repair", packIds: [DECK_LIBRARY] });
    if (repaired.status !== "started") throw new Error("deck-library repair did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(repaired.operationId)).state).toBe("completed"));

    const after = await openDB(DATABASE);
    const receipt = await after.get("packs", DECK_LIBRARY) as { candidateProjectionVersion?: number } | undefined;
    after.close();
    expect(receipt?.candidateProjectionVersion).toBeUndefined();
  });

  it("does not stamp projection intent when cancellation wins after receipt promotion", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    holdSecondImage = true;
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const started = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (started.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    let cancelling: Promise<unknown> | null = null;
    setScryfallTransactionGateForTests((phase) => {
      if (phase === "finish-before-write" && !cancelling) cancelling = backend.cancel(started.operationId);
    });
    releaseSecondImage?.();
    await vi.waitFor(() => expect(cancelling).not.toBeNull());
    await cancelling;

    const database = await openDB(DATABASE);
    const receipt = await database.get("packs", DECK_LIBRARY) as { candidateProjectionVersion?: number } | undefined;
    database.close();
    expect((await backend.operationStatus(started.operationId)).state).toBe("cancelled");
    expect(receipt?.candidateProjectionVersion).toBeUndefined();
  });

  it("rewrites stale candidates on a resumed projection without double-counting completed objects", async () => {
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [PROJECTED_FIRST, PROJECTED_SECOND] };
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await makeDeckLibraryReceiptLegacy();
    const partialDatabase = await openDB(DATABASE);
    const second = await partialDatabase.get("objects", `${PLANNED_DIGEST}:${DECK_LIBRARY}:${PROJECTED_SECOND.assetKey}`);
    if (!second) throw new Error("deck-library row was not installed");
    cache.entries.delete(second.path);
    partialDatabase.close();
    failedImage = PROJECTED_SECOND.sourceUrl;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });

    const database = await openDB(DATABASE);
    const [partial] = (await database.getAll("operations")).filter((operation) =>
      operation.state === "downloading" && operation.candidateProjectionVersion === CARD_CANDIDATE_PROJECTION_VERSION);
    database.close();
    if (!partial) throw new Error("projection operation did not remain resumable");
    expect(partial.objectsPromoted).toBe(1);
    failedImage = null;

    await backend.reconcileDeckLibrary();

    expect(await backend.operationStatus(partial.id)).toMatchObject({ state: "completed", objectsPromoted: 2 });
  });

  it("waits for an actual changed-membership worker before preparation is ready", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    await backend.setDeckLibraryBackgroundPaused(false);

    const preparing = backend.prepareDeckLibraryForOffline();
    let settled = false;
    void preparing.then(() => { settled = true; });
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    expect(settled).toBe(false);
    releaseSecondImage?.();

    await expect(preparing).resolves.toBe("ready");
  });

  it("maps unmeasured and residual deck-library drift to deterministic preparation errors", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);
    const drift = vi.spyOn(backend, "deckLibraryDrift");

    drift.mockResolvedValueOnce(null);
    await expect(backend.prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "unavailable" });

    drift.mockResolvedValueOnce({ membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 1, remove: 0, refresh: 0 });
    await expect(backend.prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "conflict" });

    drift.mockResolvedValueOnce({ membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 1, refresh: 0 });
    await expect(backend.prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "conflict" });

    drift.mockResolvedValueOnce({ membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 0, refresh: 1 });
    await expect(backend.prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "conflict" });

    drift.mockResolvedValueOnce({ membershipDigest: EMPTY_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 0, refresh: 0 });
    await expect(backend.prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "conflict" });
  });

  it("rejects preparation when its final receipt root differs from the measured installed root", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);
    vi.spyOn(backend, "deckLibraryDrift").mockImplementationOnce(async () => {
      const database = await openDB(DATABASE);
      const receipt = await database.get("packs", DECK_LIBRARY);
      if (!receipt) throw new Error("expected deck-library receipt");
      await database.put("packs", { ...receipt, root: EMPTY_DIGEST });
      database.close();
      return { membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 0, refresh: 0 };
    });

    await expect(backend.prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "conflict" });
  });

  it("propagates reconciliation failure and permits a later preparation retry", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);
    vi.spyOn(backend, "reconcileDeckLibrary").mockRejectedValueOnce(new VisualPackBackendError("network"));

    await expect(backend.prepareDeckLibraryForOffline()).rejects.toMatchObject({ kind: "network" });
    await expect(backend.prepareDeckLibraryForOffline()).resolves.toBe("ready");
  });

  it("returns not-installed when the receipt disappears after deferred drift", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);
    const drift = deferred<Awaited<ReturnType<typeof backend.deckLibraryDrift>>>();
    const driftMethod = vi.spyOn(backend, "deckLibraryDrift").mockReturnValueOnce(drift.promise);

    const removed = backend.prepareDeckLibraryForOffline();
    await vi.waitFor(() => expect(driftMethod).toHaveBeenCalledTimes(1));
    const database = await openDB(DATABASE);
    await database.delete("packs", DECK_LIBRARY);
    database.close();
    drift.resolve({ membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 0, refresh: 0 });
    await expect(removed).resolves.toBe("not-installed");
  });

  it("cancels preparation when paused during deferred drift", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);
    const pausedDrift = deferred<Awaited<ReturnType<typeof backend.deckLibraryDrift>>>();
    const driftMethod = vi.spyOn(backend, "deckLibraryDrift").mockReturnValueOnce(pausedDrift.promise);
    const paused = backend.prepareDeckLibraryForOffline();
    const pausedAssertion = expect(paused).rejects.toMatchObject({ kind: "cancelled" });
    await vi.waitFor(() => expect(driftMethod).toHaveBeenCalledTimes(1));
    await backend.setDeckLibraryBackgroundPaused(true);
    pausedDrift.resolve({ membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 0, refresh: 0 });
    await pausedAssertion;
  });

  it("cancels preparation when paused then resumed during deferred drift", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);
    const pausedDrift = deferred<Awaited<ReturnType<typeof backend.deckLibraryDrift>>>();
    const driftMethod = vi.spyOn(backend, "deckLibraryDrift").mockReturnValueOnce(pausedDrift.promise);
    const paused = backend.prepareDeckLibraryForOffline();
    const pausedAssertion = expect(paused).rejects.toMatchObject({ kind: "cancelled" });
    await vi.waitFor(() => expect(driftMethod).toHaveBeenCalledTimes(1));
    await backend.setDeckLibraryBackgroundPaused(true);
    await backend.setDeckLibraryBackgroundPaused(false);
    pausedDrift.resolve({ membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 0, refresh: 0 });
    await pausedAssertion;
  });

  it("rejects preparation when the receipt opt-in generation is replaced during drift", async () => {
    state.cardDataResident = true;
    installWebLocks();
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installDeckLibrary(backend);
    await backend.setDeckLibraryBackgroundPaused(false);
    const replacementMethod = vi.spyOn(backend, "deckLibraryDrift").mockImplementationOnce(async () => {
      const replacementReceipt = await openDB(DATABASE);
      const receipt = await replacementReceipt.get("packs", DECK_LIBRARY);
      if (!receipt) throw new Error("expected deck-library receipt");
      await replacementReceipt.put("packs", {
        ...receipt,
        optInGeneration: operationId("1".repeat(32)),
      });
      replacementReceipt.close();
      return { membershipDigest: PLANNED_DIGEST, installedDigest: PLANNED_DIGEST, add: 0, remove: 0, refresh: 0 };
    });
    const replacement = backend.prepareDeckLibraryForOffline();
    const replacementAssertion = expect(replacement).rejects.toMatchObject({ kind: "conflict" });
    await vi.waitFor(() => expect(replacementMethod).toHaveBeenCalledTimes(1));
    await replacementAssertion;
  });

  it("starts background reconciliation paused until the scheduler lifecycle unpauses it", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, THIRD] };
    installWebLocks();
    fetchMock.mockClear();

    await backend.reconcileDeckLibrary();
    expect(fetchMock).not.toHaveBeenCalled();

    await backend.setDeckLibraryBackgroundPaused(false);
    await backend.reconcileDeckLibrary();
    expect(fetchMock.mock.calls.map(([input]) => String(input))).toContain(THIRD.sourceUrl);
  });

  it("does not create a background operation when lifecycle suspension wins during selection", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, THIRD] };
    state.plan.mockClear();
    const planned = deferred<typeof state.membership>();
    state.plan.mockImplementationOnce(async () => planned.promise);
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    const reconciling = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(state.plan).toHaveBeenCalled());
    const pausing = backend.setDeckLibraryBackgroundPaused(true);
    planned.resolve(state.membership);
    await Promise.all([reconciling, pausing]);

    const database = await openDB(DATABASE);
    expect((await database.getAll("operations")).filter((operation) => operation.background)).toEqual([]);
    database.close();
  });

  it("suspends an active background download and resumes it only after the queued lifecycle unpause", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    const reconciling = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const pausing = backend.setDeckLibraryBackgroundPaused(true);
    const resuming = backend.setDeckLibraryBackgroundPaused(false);
    const whileSuspended = backend.reconcileDeckLibrary();
    releaseSecondImage?.();
    await Promise.all([reconciling, pausing, resuming, whileSuspended]);

    const paused = await openDB(DATABASE);
    const operation = (await paused.getAll("operations")).find((entry) => entry.background);
    paused.close();
    if (!operation) throw new Error("background operation was not persisted");
    expect(operation.state).toBe("downloading");

    holdSecondImage = false;
    await backend.reconcileDeckLibrary();
    await vi.waitFor(async () => expect((await backend.operationStatus(operation.id)).state).toBe("completed"));
  });

  it("transfers a live background worker to explicit Resume so a later pause cannot abort it", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    const reconciling = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const database = await openDB(DATABASE);
    const operation = (await database.getAll("operations")).find((entry) => entry.background);
    database.close();
    if (!operation) throw new Error("background operation was not persisted");

    const resumed = await backend.start({ kind: "resume", operationId: operation.id });
    expect(resumed).toMatchObject({ status: "started", operationId: operation.id });
    let pauseResolved = false;
    const pausing = backend.setDeckLibraryBackgroundPaused(true).then(() => { pauseResolved = true; });
    await vi.waitFor(() => expect(pauseResolved).toBe(true));
    releaseSecondImage?.();
    await Promise.all([reconciling, pausing]);
    await vi.waitFor(async () => expect((await backend.operationStatus(operation.id)).state).toBe("completed"));
    const after = await openDB(DATABASE);
    expect((await after.get("operations", operation.id))?.background).toBe(false);
    after.close();
  });

  it("does not interrupt an ordinary manual worker when background work pauses", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    holdSecondImage = true;
    const started = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (started.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());

    await backend.setDeckLibraryBackgroundPaused(true);
    releaseSecondImage?.();
    await vi.waitFor(async () => expect((await backend.operationStatus(started.operationId)).state).toBe("completed"));
  });

  it("rolls back finalization when suspension lands after its final write and resumes once after unpausing", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    const beforeFinalization = await openDB(DATABASE);
    const revisionBeforeFinalization = (await beforeFinalization.get("state", "state"))?.revision;
    beforeFinalization.close();
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    const revisions: string[] = [];
    await backend.subscribeRevision((event) => revisions.push(event.revision));
    let pausing: Promise<void> | null = null;
    setScryfallTransactionGateForTests((phase) => {
      if (phase === "finish-before-commit") pausing ??= backend.setDeckLibraryBackgroundPaused(true);
    });
    const reconciling = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(pausing).not.toBeNull());
    await Promise.all([reconciling, pausing]);

    const suspended = await openDB(DATABASE);
    const receipt = await suspended.get("packs", DECK_LIBRARY);
    const revision = (await suspended.get("state", "state"))?.revision;
    const operation = (await suspended.getAll("operations")).find((entry) => entry.background);
    if (!receipt || !operation) throw new Error("background finalization did not persist its receipt and operation");
    expect(receipt.root).toBe(EMPTY_DIGEST);
    expect(revision).toBe(revisionBeforeFinalization);
    expect(operation.state).toBe("finalizing");
    suspended.close();
    expect(revisions).toEqual([]);

    setScryfallTransactionGateForTests(null);
    await backend.setDeckLibraryBackgroundPaused(false);
    await backend.reconcileDeckLibrary();
    await vi.waitFor(async () => expect((await backend.operationStatus(operation.id)).state).toBe("completed"));
    expect(revisions).toHaveLength(1);
  });

  it("rolls back pack promotion when suspension lands after its final write", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    const beforePromotion = await openDB(DATABASE);
    const receiptBeforePromotion = await beforePromotion.get("packs", DECK_LIBRARY);
    const revisionBeforePromotion = (await beforePromotion.get("state", "state"))?.revision;
    beforePromotion.close();
    if (!receiptBeforePromotion) throw new Error("deck-library install did not persist a receipt");
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    let pausing: Promise<void> | null = null;
    setScryfallTransactionGateForTests((phase) => {
      if (phase === "complete-pack-before-commit") pausing ??= backend.setDeckLibraryBackgroundPaused(true);
    });
    const reconciling = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(pausing).not.toBeNull());
    await Promise.all([reconciling, pausing]);

    const suspended = await openDB(DATABASE);
    const operation = (await suspended.getAll("operations")).find((entry) => entry.background);
    expect(await suspended.get("packs", DECK_LIBRARY)).toEqual(receiptBeforePromotion);
    expect((await suspended.get("state", "state"))?.revision).toBe(revisionBeforePromotion);
    expect(operation).toMatchObject({ state: "downloading", packsPromoted: 0 });
    suspended.close();

    setScryfallTransactionGateForTests(null);
    await backend.setDeckLibraryBackgroundPaused(false);
    await backend.reconcileDeckLibrary();
    if (!operation) throw new Error("background operation was not persisted");
    await vi.waitFor(async () => expect((await backend.operationStatus(operation.id)).state).toBe("completed"));
  });

  it("rolls back same-root automatic ownership reclaim when suspension lands before transaction commit", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    failImages = true;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);
    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });
    const failed = await openDB(DATABASE);
    const operation = (await failed.getAll("operations")).find((entry) => entry.background);
    failed.close();
    if (!operation) throw new Error("background operation was not persisted");
    const failures: ProgressEvent[] = [];
    await backend.subscribeProgress((event) => { if (event.phase === "failed") failures.push(event); });
    const manual = await backend.start({ kind: "resume", operationId: operation.id });
    if (manual.status !== "started") throw new Error("manual resume did not start");
    await vi.waitFor(() => expect(failures).toEqual(expect.arrayContaining([
      expect.objectContaining({ operation: expect.objectContaining({ operationId: operation.id }) }),
    ])));
    await vi.waitFor(async () => {
      const database = await openDB(DATABASE);
      expect((await database.get("operations", operation.id))?.background).toBe(false);
      database.close();
    });

    let pausing: Promise<void> | undefined;
    setScryfallTransactionGateForTests((phase) => {
      if (phase === "selector-before-commit") pausing ??= backend.setDeckLibraryBackgroundPaused(true);
    });
    const reconciling = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(pausing).toBeDefined());
    if (!pausing) throw new Error("selector gate did not suspend lifecycle");
    await Promise.all([reconciling, pausing]);

    const suspended = await openDB(DATABASE);
    expect(await suspended.get("operations", operation.id)).toMatchObject({ state: "downloading", background: false });
    expect((await suspended.getAll("operations")).filter((entry) => entry.background)).toEqual([]);
    suspended.close();

    setScryfallTransactionGateForTests(null);
    failImages = false;
    await backend.setDeckLibraryBackgroundPaused(false);
    await backend.reconcileDeckLibrary();
    await vi.waitFor(async () => expect((await backend.operationStatus(operation.id)).state).toBe("completed"));
  });

  it("rolls back different-root cancellation when suspension lands before transaction commit", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    failImages = true;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);
    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });
    const failed = await openDB(DATABASE);
    const operation = (await failed.getAll("operations")).find((entry) => entry.background);
    failed.close();
    if (!operation) throw new Error("background operation was not persisted");
    state.membership = { membershipDigest: PLANNED_DIGEST, descriptors: [FIRST, SECOND] };

    let pausing: Promise<void> | undefined;
    setScryfallTransactionGateForTests((phase) => {
      if (phase === "selector-before-commit") pausing ??= backend.setDeckLibraryBackgroundPaused(true);
    });
    const reconciling = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(pausing).toBeDefined());
    if (!pausing) throw new Error("selector gate did not suspend lifecycle");
    await Promise.all([reconciling, pausing]);

    const suspended = await openDB(DATABASE);
    expect(await suspended.get("operations", operation.id)).toMatchObject({ state: "downloading", background: true });
    expect((await suspended.getAll("operations")).filter((entry) => entry.state === "downloading")).toHaveLength(1);
    suspended.close();

    setScryfallTransactionGateForTests(null);
    failImages = false;
    await backend.setDeckLibraryBackgroundPaused(false);
    await backend.reconcileDeckLibrary();
    await vi.waitFor(async () => expect((await backend.operationStatus(operation.id)).state).toBe("cancelled"));
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
    await backend.setDeckLibraryBackgroundPaused(false);

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
    await backend.setDeckLibraryBackgroundPaused(false);

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
    await backend.setDeckLibraryBackgroundPaused(false);

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
    await Promise.all([
      first.setDeckLibraryBackgroundPaused(false),
      second.setDeckLibraryBackgroundPaused(false),
    ]);
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
    await Promise.all([
      first.setDeckLibraryBackgroundPaused(false),
      second.setDeckLibraryBackgroundPaused(false),
    ]);

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
    await Promise.all([
      first.setDeckLibraryBackgroundPaused(false),
      second.setDeckLibraryBackgroundPaused(false),
    ]);

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
    await backend.setDeckLibraryBackgroundPaused(false);

    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });
    const failed = await openDB(DATABASE);
    const operation = (await failed.getAll("operations")).find((entry) => entry.background);
    failed.close();
    expect(operation).toMatchObject({ state: "downloading" });

    failImages = false;
    const recreated = await ScryfallBrowserVisualPackBackend.create();
    if (!operation) throw new Error("background operation was not persisted");
    expect((await recreated.operationStatus(operation.id)).state).toBe("downloading");
    await recreated.setDeckLibraryBackgroundPaused(false);
    await recreated.reconcileDeckLibrary();
    await vi.waitFor(async () => expect((await recreated.operationStatus(operation.id)).state).toBe("completed"));
  });

  it("does not restart an inactive failed background worker when pause wins during its failure bookkeeping", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    holdSecondImage = true;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);

    const rawDatabase = Reflect.get(backend, "database") as {
      get(store: string, key: unknown): Promise<unknown>;
    };
    const originalGet = rawDatabase.get.bind(rawDatabase);
    let holdFailureRead = false;
    let failureReadStarted = false;
    let releaseFailureRead!: () => void;
    const failureRead = new Promise<void>((resolve) => { releaseFailureRead = resolve; });
    const get = vi.spyOn(rawDatabase, "get").mockImplementation(async (store, key) => {
      if (holdFailureRead && store === "operations" && !failureReadStarted) {
        failureReadStarted = true;
        await failureRead;
      }
      return originalGet(store, key);
    });

    const first = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    holdFailureRead = true;
    failImages = true;
    releaseSecondImage?.();
    await vi.waitFor(() => expect(failureReadStarted).toBe(true));

    const rawBackend = backend as unknown as {
      runAndWait(selectedOperation: ReturnType<typeof operationId>, background: boolean): Promise<void>;
    };
    const runAndWait = vi.spyOn(rawBackend, "runAndWait");
    const queued = backend.reconcileDeckLibrary();
    await vi.waitFor(() => expect(runAndWait).toHaveBeenCalledTimes(1));
    const pause = backend.setDeckLibraryBackgroundPaused(true);
    const fetchesBeforeSettlement = fetchMock.mock.calls.length;
    releaseFailureRead();
    await Promise.allSettled([first, queued, pause]);
    expect(fetchMock).toHaveBeenCalledTimes(fetchesBeforeSettlement);
    runAndWait.mockRestore();
    get.mockRestore();

    failImages = false;
    holdSecondImage = false;
    await backend.setDeckLibraryBackgroundPaused(false);
    await backend.reconcileDeckLibrary();
    const persisted = await openDB(DATABASE);
    const operation = (await persisted.getAll("operations")).find((entry) => entry.background);
    persisted.close();
    if (!operation) throw new Error("background operation was not persisted");
    await vi.waitFor(async () => expect((await backend.operationStatus(operation.id)).state).toBe("completed"));
  });

  it("reclaims a manual retry queued before its failure and leaves recreation default-paused", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const initial = await backend.start({
      kind: "install", selector: { kind: "deck_library", membershipDigest: PLANNED_DIGEST }, objectEstimate: 2,
    });
    if (initial.status !== "started") throw new Error("deck-library install did not start");
    await vi.waitFor(async () => expect((await backend.operationStatus(initial.operationId)).state).toBe("completed"));
    state.membership = { membershipDigest: EMPTY_DIGEST, descriptors: [FIRST, MOVED] };
    failImages = true;
    installWebLocks();
    await backend.setDeckLibraryBackgroundPaused(false);
    await expect(backend.reconcileDeckLibrary()).rejects.toMatchObject({ kind: "network" });
    const failed = await openDB(DATABASE);
    const operation = (await failed.getAll("operations")).find((entry) => entry.background);
    failed.close();
    if (!operation) throw new Error("background operation was not persisted");

    const failures: ProgressEvent[] = [];
    await backend.subscribeProgress((event) => { if (event.phase === "failed") failures.push(event); });
    holdSecondImage = true;
    const manual = await backend.start({ kind: "resume", operationId: operation.id });
    expect(manual).toMatchObject({ status: "started", operationId: operation.id });
    await vi.waitFor(() => expect(releaseSecondImage).not.toBeNull());
    const manualOwned = await openDB(DATABASE);
    expect((await manualOwned.get("operations", operation.id))?.background).toBe(false);
    manualOwned.close();
    const automatic = backend.reconcileDeckLibrary();
    releaseSecondImage?.();
    holdSecondImage = false;
    await expect(automatic).rejects.toMatchObject({ kind: "network" });
    await vi.waitFor(() => expect(failures).toEqual(expect.arrayContaining([
      expect.objectContaining({ operation: expect.objectContaining({ operationId: operation.id }) }),
    ])));
    const reclaimed = await openDB(DATABASE);
    expect((await reclaimed.get("operations", operation.id))?.background).toBe(true);
    reclaimed.close();
    await backend.setDeckLibraryBackgroundPaused(true);

    failImages = false;
    fetchMock.mockClear();
    const recreated = await ScryfallBrowserVisualPackBackend.create();
    expect(fetchMock).not.toHaveBeenCalled();
    expect((await recreated.operationStatus(operation.id)).state).toBe("downloading");
    await recreated.setDeckLibraryBackgroundPaused(false);
    await recreated.reconcileDeckLibrary();
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
    await backend.setDeckLibraryBackgroundPaused(false);
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
    await backend.setDeckLibraryBackgroundPaused(false);
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
    await backend.setDeckLibraryBackgroundPaused(false);
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
    await backend.setDeckLibraryBackgroundPaused(false);
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
    await Promise.all([
      first.setDeckLibraryBackgroundPaused(false),
      second.setDeckLibraryBackgroundPaused(false),
    ]);

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
    await Promise.all([
      first.setDeckLibraryBackgroundPaused(false),
      second.setDeckLibraryBackgroundPaused(false),
    ]);
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
    await Promise.all([
      first.setDeckLibraryBackgroundPaused(false),
      second.setDeckLibraryBackgroundPaused(false),
    ]);

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
      candidateProjectionVersion: CARD_CANDIDATE_PROJECTION_VERSION,
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
      await recreated.setDeckLibraryBackgroundPaused(false);
      void recreated.reconcileDeckLibrary();
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
