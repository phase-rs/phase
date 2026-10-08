import "fake-indexeddb/auto";

import { gzipSync } from "node:zlib";

import { IDBFactory } from "fake-indexeddb";
import { openDB } from "idb";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore, type ArtChainEntry } from "../../../../stores/preferencesStore.ts";
import { loadScryfallData, type PrintingEntry } from "../../../scryfall.ts";
import { VisualPackStorageRefusalError } from "../../backend.ts";
import { planCuratedPack } from "../../curatedPack.ts";
import { estimatedImageBytes, IMAGE_RUNG_MEDIAN_BYTES, minimumImageBytes } from "../../types.ts";
import type { CatalogRoot, InstallSelector } from "../../types.ts";
import { ScryfallBrowserVisualPackBackend } from "../scryfallBackend.ts";

/**
 * The three-rung ladder's total median size for ONE card face.
 *
 * Summed here from the exported per-rung table rather than written as a
 * literal: a literal would have to be hand-edited whenever the sampled
 * constants move, and a figure hand-edited to match the code it is meant to
 * pin stops pinning anything. What these tests assert is the RELATION — one
 * face costs one full ladder — which is the model's actual claim.
 */
const FACE_BYTES = Object.values(IMAGE_RUNG_MEDIAN_BYTES).reduce((total, bytes) => total + bytes, 0);
const RUNGS_PER_FACE = Object.keys(IMAGE_RUNG_MEDIAN_BYTES).length;
const GIB = 1024 ** 3;

const BULK_INDEX_URL = "https://api.scryfall.com/bulk-data";
const BULK_DOWNLOAD_URL = "https://data.scryfall.io/all-cards.jsonl.gz";

const BOLT = "11111111-abcd-4111-8111-111111111111";
const GIANT = "22222222-abcd-4222-8222-222222222222";

function url(token: string, size: string): string {
  return `https://cards.scryfall.io/${size}/front/a/b/${token}.jpg`;
}

function imageFace(token: string) {
  return { normal: url(token, "normal"), art_crop: url(token, "art_crop") };
}

/** Scryfall's own shape, as the bulk JSONL carries it: all three rungs. */
function bulkImages(token: string) {
  return { small: url(token, "small"), normal: url(token, "normal"), art_crop: url(token, "art_crop") };
}

function printing(id: string, set: string, releasedAt: string): PrintingEntry {
  return {
    id,
    set,
    set_name: set.toUpperCase(),
    collector_number: "1",
    released_at: releasedAt,
    border_color: "black",
    frame_effects: [],
    full_art: false,
    faces: [imageFace(id)],
  };
}

function cardEntry(oracleId: string, name: string, faceName: string) {
  return {
    oracle_id: oracleId,
    name,
    face_names: [faceName],
    faces: [imageFace(oracleId)],
    mana_cost: "",
    cmc: 0,
    type_line: "",
    colors: [],
    color_identity: [],
    keywords: [],
  };
}

const BOLT_ENTRY = cardEntry(BOLT, "Lightning Bolt", "lightning bolt");
const GIANT_ENTRY = cardEntry(GIANT, "Giant Growth", "giant growth");
const CARDS = {
  [BOLT]: BOLT_ENTRY,
  "lightning bolt": BOLT_ENTRY,
  [GIANT]: GIANT_ENTRY,
  "giant growth": GIANT_ENTRY,
};
const PRINTINGS: Record<string, PrintingEntry[]> = {
  [BOLT]: [printing("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "m20", "2019-07-12")],
};
const NEWEST: ArtChainEntry[] = [{ type: "newest" }];

/**
 * Three English faces across two English printings, plus one non-English
 * printing the `complete` selector must skip. Nine image records, three faces.
 */
const BULK_LINES = [
  {
    id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
    oracle_id: BOLT, set: "m20", lang: "en", name: "Lightning Bolt", collector_number: "1",
    image_uris: bulkImages("bulk-bolt"),
  },
  {
    id: "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
    oracle_id: GIANT, set: "m20", lang: "en", name: "Two // Faced", collector_number: "2",
    card_faces: [
      { name: "Two", image_uris: bulkImages("bulk-two") },
      { name: "Faced", image_uris: bulkImages("bulk-faced") },
    ],
  },
  {
    id: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
    oracle_id: BOLT, set: "m20", lang: "de", name: "Blitzschlag", collector_number: "1",
    image_uris: bulkImages("bulk-de"),
  },
];
const BULK_FACES = 3;

const BULK_BODY = gzipSync(Buffer.from(BULK_LINES.map((line) => JSON.stringify(line)).join("\n") + "\n"));
const BULK_RECORD = {
  type: "all_cards",
  updated_at: "2026-08-01T00:00:00.000Z",
  jsonl_download_uri: BULK_DOWNLOAD_URL,
  compressed_size: BULK_BODY.byteLength,
};

class MemoryCache {
  readonly entries = new Map<string, { body: Uint8Array; type: string }>();

  async put(request: string, response: Response): Promise<void> {
    this.entries.set(request, {
      body: new Uint8Array(await response.arrayBuffer()),
      type: response.headers.get("Content-Type") ?? "application/octet-stream",
    });
  }

  async match(request: string): Promise<Response | undefined> {
    const entry = this.entries.get(request);
    return entry ? new Response(entry.body, { headers: { "Content-Type": entry.type } }) : undefined;
  }

  async delete(request: string): Promise<boolean> {
    return this.entries.delete(request);
  }
}

let cache = new MemoryCache();

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
}

const fetchStub = vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
  const source = String(input);
  if (source === BULK_INDEX_URL) return jsonResponse({ data: [BULK_RECORD] });
  if (source === BULK_DOWNLOAD_URL) {
    return new Response(new Uint8Array(BULK_BODY), { status: 200, headers: { "Content-Type": "application/gzip" } });
  }
  if (source === "/scryfall-data.json") return jsonResponse(CARDS);
  if (source === "/scryfall-printings.json") return jsonResponse(PRINTINGS);
  if (source.startsWith("https://cards.scryfall.io/")) {
    return new Response(new TextEncoder().encode(source), { status: 200, headers: { "Content-Type": "image/jpeg" } });
  }
  throw new Error(`unexpected fetch: ${source}`);
});

/**
 * Replace `navigator.storage` for one test.
 *
 * Defined on the existing navigator rather than by stubbing the whole global,
 * so nothing else about the environment moves. `undefined` reproduces the
 * browsers and private windows that expose no Storage API at all — which is
 * also happy-dom's own default, hence `absent()` below is a named no-op rather
 * than a silent reliance on the environment.
 */
function stubStorage(manager: Partial<StorageManager> | undefined): void {
  Object.defineProperty(globalThis.navigator, "storage", { value: manager, configurable: true, writable: true });
}

/**
 * A browser that answers: `available` bytes free, behind a nonzero existing
 * usage so that headroom is `quota - usage` and not merely `quota`.
 */
function roomyStorage(available: number, granted = true) {
  const usage = 3 * GIB;
  return {
    estimate: vi.fn(async () => ({ usage, quota: usage + available })),
    persisted: vi.fn(async () => false),
    persist: vi.fn(async () => granted),
  };
}

const DATABASE = "phase-visual-packs-scryfall-v1";

/** Reopen the crash window between `completePack` and `finish`: the pack row
 *  is written with this operation's id while the operation is still
 *  `downloading`, which is the state a resume actually continues from. */
async function interrupt(operation: string): Promise<void> {
  const database = await openDB(DATABASE);
  const record = await database.get("operations", operation);
  await database.put("operations", { ...record, state: "downloading", completedRevision: null });
  database.close();
}

async function operationRecords(): Promise<unknown[]> {
  const database = await openDB(DATABASE);
  const records = await database.getAll("operations");
  database.close();
  return records;
}

/**
 * Move Giant Growth's `normal` face URL, as a regeneration of
 * `scryfall-data.json` does — the case in which a pack is out of date while
 * every asset key it holds is unchanged. Mutated in place because
 * `loadScryfallData` memoizes its RESOLVED value, so re-serving the fetch would
 * change nothing; every key carrying this oracle id is updated because the JSON
 * round-trip gives the oracle-id key and the name key independent objects.
 */
async function moveGiantNormal(token: string): Promise<void> {
  const cards = await loadScryfallData();
  if (!cards) throw new Error("card data unavailable");
  for (const entry of Object.values(cards)) {
    if (entry.oracle_id === GIANT) entry.faces[0].normal = url(token, "normal");
  }
}

async function curatedSelector(): Promise<{ selector: InstallSelector; records: number }> {
  const membership = await planCuratedPack();
  return {
    selector: { kind: "curated", membershipDigest: membership.membershipDigest as CatalogRoot },
    records: membership.descriptors.length,
  };
}

async function settle(backend: ScryfallBrowserVisualPackBackend, operation: string): Promise<void> {
  await vi.waitFor(async () => {
    expect((await backend.operationStatus(operation as never)).state).toBe("completed");
  }, { timeout: 5000 });
}

describe("install sizing and storage headroom", () => {
  beforeEach(() => {
    globalThis.indexedDB = new IDBFactory();
    cache = new MemoryCache();
    fetchStub.mockClear();
    vi.stubGlobal("fetch", fetchStub);
    vi.stubGlobal("caches", { open: async () => cache } as unknown as CacheStorage);
    usePreferencesStore.setState({ artChain: NEWEST, artOverrides: {} });
    stubStorage(undefined);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    stubStorage(undefined);
  });

  /**
   * FIRST IN THIS FILE, DELIBERATELY. `loadScryfallData` and `loadPrintingsData`
   * memoize at module scope, so any earlier test that plans a membership leaves
   * the card data resident and silently turns this into the warm case. The
   * `toBeNull()` below ENFORCES that rather than assuming it — a warm module
   * makes this fail loudly instead of passing vacuously.
   */
  it("measures no drift, and fetches no card data, until the card data is resident", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();

    const cold = await backend.curatedDrift();

    // The settings panel reads this on mount. `scryfall-data.json` and
    // `scryfall-printings.json` are 36,748,238 and 39,541,979 bytes, so a
    // measurement that loaded them would turn opening a tab into a 76 MB
    // download with no progress indication and no way to cancel.
    expect(cold).toBeNull();
    const cursor = fetchStub.mock.calls.map(([input]) => String(input));
    expect(cursor).not.toContain("/scryfall-data.json");
    expect(cursor).not.toContain("/scryfall-printings.json");

    // The complementary branch, so the gate is not merely stuck off: an action
    // the user took plans a membership, which loads the data...
    await curatedSelector();
    expect(fetchStub.mock.calls.map(([input]) => String(input))).toContain("/scryfall-data.json");

    const warm = await backend.curatedDrift();

    // ...and the next read measures for free.
    expect(warm).not.toBeNull();
    expect(warm?.installedDigest).toBeNull();
  });

  it("reports a curated download size derived from the faces it will fetch", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();

    const estimate = await backend.estimateInstall(selector);

    // Every planned face carries the whole ladder, which is what makes a
    // per-record mean the right weighting. Assert it rather than assume it:
    // a remainder here would mean some face contributes fewer rungs and the
    // face-derived expectation below would silently stop being the model.
    expect(records % RUNGS_PER_FACE).toBe(0);
    const faces = records / RUNGS_PER_FACE;
    expect(faces).toBeGreaterThan(0);
    expect(estimate.estimatedImageBytes).toBe(faces * FACE_BYTES);
    expect(estimate.assetRecords).toBe(String(records));
  });

  it("reports a download size for a bulk selector from the same model", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const summary = await backend.refreshCatalog();

    const estimate = await backend.estimateInstall({ kind: "complete", rootSha256: summary.catalogRoot });

    // Counted off the real bulk scan, not off a stubbed number: three English
    // faces, with the German printing skipped.
    expect(estimate.assetRecords).toBe(String(BULK_FACES * RUNGS_PER_FACE));
    expect(estimate.estimatedImageBytes).toBe(BULK_FACES * FACE_BYTES);
  });

  it("prices the two selectors on the same per-face scale", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const summary = await backend.refreshCatalog();
    const { selector, records } = await curatedSelector();

    const curated = await backend.estimateInstall(selector);
    const complete = await backend.estimateInstall({ kind: "complete", rootSha256: summary.catalogRoot });

    // The comparison the feature exists to let a user make. In this fixture
    // the bulk side is the larger one; at real catalog scale the ratio is what
    // the test below pins.
    expect(curated.estimatedImageBytes / complete.estimatedImageBytes)
      .toBeCloseTo((records / RUNGS_PER_FACE) / BULK_FACES, 10);
  });

  it("stays the right order of magnitude at real catalog scale", () => {
    // MEASURED from the shipped data files: 35,055 distinct non-token faces in
    // scryfall-data.json and 90,831 faces in scryfall-printings.json, every one
    // of which carries `normal` and `art_crop` together. A band rather than an
    // equality, so a sampled constant may be re-measured without hand-editing a
    // golden — but a constant that moved by an order of magnitude, which is the
    // way this model actually breaks, still fails here.
    const curatedGib = estimatedImageBytes(35_055 * RUNGS_PER_FACE) / GIB;
    const printingsGib = estimatedImageBytes(90_831 * RUNGS_PER_FACE) / GIB;

    expect(curatedGib).toBeGreaterThan(5);
    expect(curatedGib).toBeLessThan(8);
    expect(printingsGib).toBeGreaterThan(14);
    expect(printingsGib).toBeLessThan(20);
    expect(printingsGib / curatedGib).toBeCloseTo(90_831 / 35_055, 10);
  });

  it("calls an estimate sufficient when the browser reports room for it", async () => {
    stubStorage(roomyStorage(64 * GIB));
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector } = await curatedSelector();

    const estimate = await backend.estimateInstall(selector);

    // Headroom is what is LEFT, not the whole quota: the fixture is already
    // 3 GiB into a 67 GiB budget, so reporting the quota would overstate the
    // room by exactly that much.
    expect(estimate.storage).toEqual({
      usageBytes: 3 * GIB,
      quotaBytes: 67 * GIB,
      availableBytes: 64 * GIB,
      persistence: "best_effort",
    });
    expect(estimate.headroom).toBe("sufficient");
  });

  it("warns but still installs when the projection alone exceeds the headroom", async () => {
    // The band this whole design turns on. Free space sits BELOW the expected
    // size and ABOVE the cheapest reading of it, which is exactly where a
    // six-sample-per-rung projection cannot tell whether the install fits.
    const { selector, records } = await curatedSelector();
    const between = Math.round((minimumImageBytes(records) + estimatedImageBytes(records)) / 2);
    // Guard the fixture: if these two figures ever collapse together there is
    // no band left and this test would silently stop testing anything.
    expect(minimumImageBytes(records)).toBeLessThan(between);
    expect(between).toBeLessThan(estimatedImageBytes(records));
    const manager = roomyStorage(between);
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();

    const estimate = await backend.estimateInstall(selector);
    // Warned...
    expect(estimate.headroom).toBe("insufficient");
    expect(estimate.estimatedImageBytes).toBeGreaterThan(estimate.storage.availableBytes!);

    // ...and NOT blocked. A refusal here has no override; a quota failure
    // part-way through does, because `storage` is retryable and a resume skips
    // every object already cached.
    const response = await backend.start({ kind: "install", selector, objectEstimate: records });
    if (response.status !== "started") throw new Error("curated install did not start");
    await settle(backend, response.operationId);
    expect(cache.entries.size).toBe(records);
  });

  it("refuses only when no reading of the model could fit", async () => {
    // 1 KiB free inside a multi-gigabyte quota: below even the cheapest-rung
    // floor, so no error bar on the sampled constants rescues it. Also a check
    // that compared against the QUOTA rather than the remaining room would
    // call this sufficient.
    const manager = roomyStorage(1024);
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();
    expect(minimumImageBytes(records)).toBeGreaterThan(1024);

    const estimate = await backend.estimateInstall(selector);
    expect(estimate.headroom).toBe("insufficient");

    const refused: unknown = await backend
      .start({ kind: "install", selector, objectEstimate: records })
      .then(() => null, (error: unknown) => error);

    // Its OWN kind, not the `storage` catch-all every unrecognised error lands
    // in, so the panel can say "nothing was written, free some space" instead
    // of "the write failed".
    expect(refused).toBeInstanceOf(VisualPackStorageRefusalError);
    // The figures the gate actually compared, carried as data. A panel that
    // recomputed the floor would read the browser's quota at a different
    // instant than this comparison did.
    expect((refused as VisualPackStorageRefusalError).refusal).toEqual({
      requiredBytes: minimumImageBytes(records),
      availableBytes: 1024,
    });
    // ...and NOT as prose. `detail` is `message`, and the panel renders
    // `message` verbatim beneath a translated sentence, so a figure that
    // reached it would be an untranslatable number in seven languages.
    expect((refused as Error).message).not.toMatch(/\d/);
    // Refused BEFORE anything exists, so there is nothing to unwedge and no
    // half-written pack. Without this the rejection alone would be satisfied by
    // a guard that runs after the record is persisted.
    expect(await operationRecords()).toHaveLength(0);
    expect(cache.entries.size).toBe(0);
  });

  it("reports the origin's storage on the summary, and asks for no grant to do it", async () => {
    const manager = roomyStorage(64 * GIB);
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();

    const summary = await backend.refreshCatalog();

    // The figures a panel renders for a user who has ALREADY installed, and
    // which used to be reachable only by running an estimate for a new one.
    // Asserted as VALUES, because `storage` is a required field: `tsc` proves
    // it is present, and nothing but this proves it is the browser's answer
    // rather than a blank the backend made up. (3 GiB is the existing usage
    // `roomyStorage` reports behind its free space.)
    expect(summary.storage).toEqual({
      usageBytes: 3 * GIB,
      quotaBytes: 3 * GIB + 64 * GIB,
      availableBytes: 64 * GIB,
      persistence: "best_effort",
    });
    // A settings panel reads this summary on open, and `persist()` is the one
    // Storage API method that can raise a browser permission prompt — so a
    // summary read that reached it would prompt every user who opened the
    // panel, for a permission they never asked to give. Today that is
    // guaranteed statically: `persist()` has a single call site, inside
    // `requestPersistence`, which only `reserveStorage` calls, and
    // `reserveStorage` is reachable only from `start()`. A static trace holds
    // only until someone adds a second caller; this is what fails when they do.
    expect(manager.persist).not.toHaveBeenCalled();
    // Reach guard: the summary really did consult the browser, so the absence
    // above is a choice of METHOD rather than a storage read that never ran.
    expect(manager.persisted).toHaveBeenCalled();
    expect(manager.estimate).toHaveBeenCalled();
  });

  it("requests a persistence grant before an install writes anything", async () => {
    const manager = roomyStorage(64 * GIB);
    // The grant protects the bytes that come after it, so "before" is the
    // claim, not merely "at some point". Recorded from inside the stub because
    // a call count alone is satisfied by a grant requested at the very end.
    //
    // Weakly discriminating TODAY and deliberately kept: `start()` fires the
    // download without awaiting it, so nothing is cached by the time it
    // returns and this reads 0 almost by construction. What it pins is that a
    // later refactor cannot move the request down into the download loop,
    // where the first images would already be on disk unprotected.
    const cachedWhenGranted: number[] = [];
    manager.persist.mockImplementation(async () => {
      cachedWhenGranted.push(cache.entries.size);
      return true;
    });
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();

    const response = await backend.start({ kind: "install", selector, objectEstimate: records });

    if (response.status !== "started") throw new Error("curated install did not start");
    expect(manager.persist).toHaveBeenCalledTimes(1);
    expect(cachedWhenGranted).toEqual([0]);
    expect(response.persistence).toBe("persisted");
    await settle(backend, response.operationId);
    // ...and the install really did write something afterwards, so the 0 above
    // is an ordering fact rather than an install that never ran.
    expect(cache.entries.size).toBe(records);
  });

  it("installs anyway when the grant is refused, and says so", async () => {
    const manager = roomyStorage(64 * GIB, false);
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();

    const response = await backend.start({ kind: "install", selector, objectEstimate: records });

    if (response.status !== "started") throw new Error("curated install did not start");
    expect(response.persistence).toBe("best_effort");
    await settle(backend, response.operationId);
    // A refused grant costs eviction protection, not the download.
    expect(cache.entries.size).toBe(records);
  });

  it("installs when the Storage API is absent altogether", async () => {
    stubStorage(undefined);
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();

    const estimate = await backend.estimateInstall(selector);
    expect(estimate.headroom).toBe("unknown");
    expect(estimate.storage).toEqual({
      usageBytes: null, quotaBytes: null, availableBytes: null, persistence: "unsupported",
    });

    const response = await backend.start({ kind: "install", selector, objectEstimate: records });
    if (response.status !== "started") throw new Error("curated install did not start");
    expect(response.persistence).toBe("unsupported");
    await settle(backend, response.operationId);
    expect(cache.entries.size).toBe(records);
  });

  it("installs when every Storage API call rejects", async () => {
    // The case an `in`-style feature detection passes and then falls over on:
    // the methods are present, and they throw. Older Safari and private
    // windows reject `persist()`; a storage-pressure failure can reject
    // `estimate()`.
    const boom = () => Promise.reject(new DOMException("denied", "SecurityError"));
    const manager = { estimate: vi.fn(boom), persisted: vi.fn(boom), persist: vi.fn(boom) };
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();

    const estimate = await backend.estimateInstall(selector);
    expect(estimate.headroom).toBe("unknown");
    expect(estimate.storage.persistence).toBe("unsupported");

    const response = await backend.start({ kind: "install", selector, objectEstimate: records });
    if (response.status !== "started") throw new Error("curated install did not start");
    expect(response.persistence).toBe("unsupported");
    await settle(backend, response.operationId);
    expect(manager.persisted).toHaveBeenCalled();
    expect(cache.entries.size).toBe(records);
  });

  it("requests a persistence grant for a resume as well, and reports it", async () => {
    // A resume writes bytes exactly like the install it continues, so it needs
    // the same eviction protection. The suite's other resume tests run with no
    // Storage API at all, where "unsupported" comes out whether or not the
    // grant was ever asked for — a stubbed manager is what makes this
    // observable.
    const manager = roomyStorage(64 * GIB);
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();
    const first = await backend.start({ kind: "install", selector, objectEstimate: records });
    if (first.status !== "started") throw new Error("curated install did not start");
    await settle(backend, first.operationId);
    await interrupt(first.operationId);
    manager.persist.mockClear();
    manager.persisted.mockClear();

    const resumed = await backend.start({ kind: "resume", operationId: first.operationId });

    if (resumed.status !== "started") throw new Error("curated resume did not start");
    expect(manager.persisted).toHaveBeenCalledTimes(1);
    expect(manager.persist).toHaveBeenCalledTimes(1);
    expect(resumed.persistence).toBe("persisted");
    await settle(backend, first.operationId);
  });

  it("does not ask for a grant when a sync turns out to have nothing to do", async () => {
    const manager = roomyStorage(64 * GIB);
    stubStorage(manager);
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();

    const first = await backend.start({ kind: "install", selector, objectEstimate: records });
    if (first.status !== "started") throw new Error("curated install did not start");
    await settle(backend, first.operationId);
    const grantsAfterInstall = manager.persist.mock.calls.length + manager.persisted.mock.calls.length;
    // Reach guard: without this, an implementation that asked for no grant
    // anywhere would satisfy the "unchanged" assertion below with 0 === 0.
    expect(grantsAfterInstall).toBeGreaterThan(0);

    const second = await backend.start({ kind: "install", selector, objectEstimate: records });

    expect(second).toEqual({ status: "healthy" });
    expect(manager.persist.mock.calls.length + manager.persisted.mock.calls.length).toBe(grantsAfterInstall);
  });

  it("reports the same count the panel prices, for a pack with nothing to sync", async () => {
    stubStorage(roomyStorage(64 * GIB));
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();
    const first = await backend.start({ kind: "install", selector, objectEstimate: records });
    if (first.status !== "started") throw new Error("curated install did not start");
    await settle(backend, first.operationId);

    // What reopening the panel on an installed pack does: it auto-runs an
    // estimate against the selector it already has.
    const estimate = await backend.estimateInstall(selector);

    // `uniqueObjects` is the panel's ONLY count row for a curated estimate and
    // it renders directly beside `estimatedImageBytes` under "Images to
    // download". The two must be the same figure or the panel contradicts
    // itself — "Images to download: 105,165" beside "Estimated download size:
    // 0 B" is what reporting the membership there produced.
    expect(estimate.estimatedImageBytes).toBe(0);
    expect(Number(estimate.uniqueObjects)).toBe(0);
    // ...and the membership is STILL reported whole, on the other field. This
    // conjunct is what pins the distinction rather than collapsing the two:
    // `assetRecords` is the panel's `objectEstimate`, the denominator of a
    // progress bar whose numerator counts every object the run promotes.
    expect(estimate.assetRecords).toBe(String(records));
    expect(records).toBeGreaterThan(0);
  });

  it("sizes and gates a re-sync on what it must fetch, not on the whole membership", async () => {
    stubStorage(roomyStorage(64 * GIB));
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, records } = await curatedSelector();
    const first = await backend.start({ kind: "install", selector, objectEstimate: records });
    if (first.status !== "started") throw new Error("curated install did not start");
    await settle(backend, first.operationId);

    await moveGiantNormal("moved");
    // The card map is edited in place, which the plan memo's key cannot see.
    usePreferencesStore.setState({ artOverrides: {} });
    const next = await curatedSelector();
    expect(next.selector).not.toEqual(selector);
    expect(next.records).toBe(records);
    // `normal` moved and `small` is derived from it: two of six assets, and
    // every asset key unchanged.
    expect(await backend.curatedDrift()).toMatchObject({ add: 0, remove: 0, refresh: 2 });

    // One byte short of the cheapest possible reading of the WHOLE membership,
    // which is the figure the gate used to compare against — and room for the
    // two moved rungs several times over.
    const available = minimumImageBytes(records) - 1;
    expect(minimumImageBytes(records)).toBeGreaterThan(available);
    expect(minimumImageBytes(2)).toBeLessThanOrEqual(available);
    stubStorage(roomyStorage(available));

    const estimate = await backend.estimateInstall(next.selector);

    expect(estimate.estimatedImageBytes).toBe(estimatedImageBytes(2));
    expect(estimate.estimatedImageBytes).toBeLessThan(estimatedImageBytes(records));
    // The membership itself is still reported whole. It is what the panel
    // passes as `objectEstimate`, and that is the denominator of a progress bar
    // whose numerator counts every object the run promotes — reused included.
    expect(estimate.assetRecords).toBe(String(records));

    // Not refused, where the old figure had no reading that could fit.
    const response = await backend.start({
      kind: "install",
      selector: next.selector,
      objectEstimate: next.records,
    });

    if (response.status !== "started") throw new Error("curated re-sync did not start");
    await settle(backend, response.operationId);
  });
});
