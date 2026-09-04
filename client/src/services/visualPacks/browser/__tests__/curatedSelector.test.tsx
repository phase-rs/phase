import "fake-indexeddb/auto";

import { IDBFactory } from "fake-indexeddb";
import { openDB } from "idb";
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { VisualPackManager } from "../../../../components/settings/visual-packs/VisualPackManager.tsx";
import { useVisualPackManager } from "../../../../components/settings/visual-packs/useVisualPackManager.ts";
import { usePreferencesStore, type ArtChainEntry } from "../../../../stores/preferencesStore.ts";
import type { PrintingEntry } from "../../../scryfall.ts";
import { packId } from "../../types.ts";
import type { CatalogRoot, InstallSelector, OperationId, ProgressEvent, ResolutionResponse } from "../../types.ts";
import type { ScryfallAssetDescriptor } from "../descriptors.ts";
import { ScryfallBrowserVisualPackBackend } from "../scryfallBackend.ts";
import { planCuratedPack } from "../../curatedPack.ts";

const platform = vi.hoisted(() => ({ load: vi.fn() }));
vi.mock("../../../platform.ts", () => ({ loadVisualPackBackend: platform.load }));
vi.mock("../../../../hooks/useSetSymbols.ts", () => ({ useSetCatalog: () => ({ catalog: null, isLoading: false }) }));

// The one URL this whole selector exists to avoid. `loadScryfallBulkSource`
// still fetches the small INDEX above it — that is intended — so the download
// uri is what a curated install must never request.
//
// The GUARD on that is the fetch stub, which throws `unexpected fetch` on any
// URL it does not recognise, this one included: a curated install that opened
// the bulk stream would fail the test at the fetch, before any assertion ran.
// The `expect(requested).not.toContain(BULK_DOWNLOAD_URL)` lines below can
// therefore never be the failing assertion. They are kept as a statement of
// intent for the reader, not as the mechanism.
const BULK_INDEX_URL = "https://api.scryfall.com/bulk-data";
const BULK_DOWNLOAD_URL = "https://data.scryfall.io/all-cards.jsonl.gz";
const BULK_RECORD = {
  type: "all_cards",
  updated_at: "2026-08-01T00:00:00.000Z",
  jsonl_download_uri: BULK_DOWNLOAD_URL,
  compressed_size: 2_400_000_000,
};

const BOLT = "11111111-abcd-4111-8111-111111111111";
const GIANT = "22222222-abcd-4222-8222-222222222222";
const TOKEN_ORACLE = "33333333-abcd-4333-8333-333333333333";

function url(token: string, size: string): string {
  return `https://cards.scryfall.io/${size}/front/a/b/${token}.jpg`;
}

function imageFace(token: string) {
  return { normal: url(token, "normal"), art_crop: url(token, "art_crop") };
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

const BOLT_NEW = printing("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "m20", "2019-07-12");
const BOLT_OLD = printing("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "lea", "1993-08-05");

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

// Keyed by oracle id AND by lowercased name, exactly as `scryfall-data.json`
// is, plus one `token:` key the planner must skip.
const CARDS = {
  [BOLT]: BOLT_ENTRY,
  "lightning bolt": BOLT_ENTRY,
  [GIANT]: GIANT_ENTRY,
  "giant growth": GIANT_ENTRY,
  [`token:${TOKEN_ORACLE}`]: cardEntry(TOKEN_ORACLE, "Soldier", "soldier"),
};
// Giant Growth is absent, so it takes the canonical path; Lightning Bolt has
// two printings, so the art chain picks a different one for each of them.
const PRINTINGS: Record<string, PrintingEntry[]> = { [BOLT]: [BOLT_NEW, BOLT_OLD] };

const NEWEST: ArtChainEntry[] = [{ type: "newest" }];
const OLDEST: ArtChainEntry[] = [{ type: "oldest" }];

/** An in-memory `Cache`, holding only what this backend asks of one. */
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
let requested: string[] = [];

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
}

function imageResponse(source: string): Response {
  // URL-derived bytes, so content-addressed paths differ per asset the way
  // real images do.
  return new Response(new TextEncoder().encode(source), { status: 200, headers: { "Content-Type": "image/jpeg" } });
}

/** A transient image outage: `fetchImage` rejects a non-200 as `network`. */
let failImages = false;
let holdLaterImages = false;
let releaseHeldImages: (() => void) | null = null;
let heldImages: Promise<void> = Promise.resolve();

const fetchStub = vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
  const source = String(input);
  requested.push(source);
  if (source === BULK_INDEX_URL) return jsonResponse({ data: [BULK_RECORD] });
  if (source === "/scryfall-data.json") return jsonResponse(CARDS);
  if (source === "/scryfall-printings.json") return jsonResponse(PRINTINGS);
  if (source.startsWith("https://cards.scryfall.io/") || source.startsWith("https://backs.scryfall.io/")) {
    if (holdLaterImages && imageRequests().length > 1) await heldImages;
    return failImages ? new Response("", { status: 503 }) : imageResponse(source);
  }
  throw new Error(`unexpected fetch: ${source}`);
});

const DATABASE = "phase-visual-packs-scryfall-v1";

/**
 * Reopen the crash window between `completePack` and `finish`: the pack's row
 * is already written with this operation's id while the operation itself is
 * still `downloading`. Emptying the cache alongside this makes "nothing was
 * re-downloaded" measurable in fetch counts rather than in a cache hit.
 *
 * A caller that also clears the cache must read the follow-up
 * `resolvedAssets(...)` accordingly: `resolve()` admits a row only when its
 * bytes are still cached, so an emptied cache forces that count to 0 no matter
 * what the resume did. It is an artifact of the cache clear and says nothing
 * about resume behaviour. The load-bearing assertions are the fetch count and
 * `settle()` reaching `completed`.
 */
async function interrupt(operation: OperationId): Promise<void> {
  const database = await openDB(DATABASE);
  const record = await database.get("operations", operation);
  await database.put("operations", { ...record, state: "downloading", completedRevision: null });
  database.close();
}

function imageRequests(): string[] {
  return requested.filter((entry) => entry.startsWith("https://cards.scryfall.io/"));
}

async function curatedSelector(): Promise<{ selector: InstallSelector; descriptors: readonly ScryfallAssetDescriptor[] }> {
  const membership = await planCuratedPack();
  return {
    selector: { kind: "curated", membershipDigest: membership.membershipDigest },
    descriptors: membership.descriptors,
  };
}

/** The `failed` progress events one backend instance emits, from this point on. */
async function failures(backend: ScryfallBrowserVisualPackBackend): Promise<ProgressEvent[]> {
  const events: ProgressEvent[] = [];
  await backend.subscribeProgress((event) => {
    if (event.phase === "failed") events.push(event);
  });
  return events;
}

/** Every persisted operation record, read the way a fresh launch reads them. */
async function operationRecords(): Promise<unknown[]> {
  const database = await openDB(DATABASE);
  const records = await database.getAll("operations");
  database.close();
  return records;
}

async function settle(backend: ScryfallBrowserVisualPackBackend, operation: OperationId): Promise<void> {
  await vi.waitFor(async () => {
    expect((await backend.operationStatus(operation)).state).toBe("completed");
  }, { timeout: 5000 });
}

async function installCurated(backend: ScryfallBrowserVisualPackBackend): Promise<{
  digest: CatalogRoot;
  descriptors: readonly ScryfallAssetDescriptor[];
  operation: OperationId;
}> {
  const { selector, descriptors } = await curatedSelector();
  const response = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
  if (response.status !== "started") throw new Error("curated install did not start");
  await settle(backend, response.operationId);
  return { digest: (selector as { membershipDigest: CatalogRoot }).membershipDigest, descriptors, operation: response.operationId };
}

/** Every installed object a render-time asset lookup can actually reach. This
 *  is the whole membership as the app sees it: `resolve` admits a row only
 *  when its root matches its pack's root and its bytes are still cached. */
async function resolvedAssets(
  backend: ScryfallBrowserVisualPackBackend,
  descriptors: readonly ScryfallAssetDescriptor[],
): Promise<ResolutionResponse["entries"][number]["matches"]> {
  const response = await backend.resolve(descriptors.map((value) => ({ kind: "asset", key: value.assetKey })));
  return response.entries.flatMap((entry) => entry.matches);
}

describe("curated install selector", () => {
  beforeEach(() => {
    globalThis.indexedDB = new IDBFactory();
    cache = new MemoryCache();
    requested = [];
    failImages = false;
    holdLaterImages = false;
    heldImages = new Promise((resolve) => { releaseHeldImages = resolve; });
    fetchStub.mockClear();
    vi.stubGlobal("fetch", fetchStub);
    vi.stubGlobal("caches", { open: async () => cache } as unknown as CacheStorage);
    usePreferencesStore.setState({ artChain: NEWEST, artOverrides: {} });
    platform.load.mockReset();
  });

  afterEach(() => {
    releaseHeldImages?.();
    releaseHeldImages = null;
    cleanup();
    vi.unstubAllGlobals();
  });

  it("installs the planned membership without opening the bulk stream", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { digest, descriptors, operation } = await installCurated(backend);

    expect(requested).not.toContain(BULK_DOWNLOAD_URL);
    expect(imageRequests()).toHaveLength(descriptors.length);
    // Two rungs from Bolt's chain-selected printing plus its art crop, and the
    // same three for Giant Growth's canonical form. The token key contributes
    // nothing.
    expect(descriptors).toHaveLength(6);
    expect(descriptors.filter((value) => value.assetKey.startsWith("asset:v1:canonical_card:"))).toHaveLength(3);

    const matches = await resolvedAssets(backend, descriptors);
    expect(matches).toHaveLength(descriptors.length);
    expect(new Set(matches.map((match) => match.catalogRoot))).toEqual(new Set([digest]));

    // The operation reports the CATALOG's identity, not the pack's root.
    const summary = await backend.catalogSummary();
    const status = await backend.operationStatus(operation);
    expect(status.catalogRoot).toBe(summary.catalogRoot);
    expect(status.catalogRoot).not.toBe(digest);
    expect(summary.selectorCount).toBe(6);
  });

  it("records the curated pack at its membership digest and starts at the catalog root", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, descriptors } = await curatedSelector();
    const digest = (selector as { membershipDigest: CatalogRoot }).membershipDigest;
    const revisions: CatalogRoot[] = [];
    await backend.subscribeRevision((event) => {
      if (event.catalogRoot) revisions.push(event.catalogRoot);
    });

    const response = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
    if (response.status !== "started") throw new Error("curated install did not start");
    await settle(backend, response.operationId);

    const summary = await backend.catalogSummary();
    expect(summary.installedPacks).toEqual([{ packId: packId("curated"), catalogRoot: digest }]);
    // StartResponse and the revision event both name the CATALOG.
    expect(response.catalogRoot).toBe(summary.catalogRoot);
    expect(revisions).toEqual([summary.catalogRoot]);
  });

  it("reports ready and renders the pack manager after a curated-only install", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { digest } = await installCurated(backend);

    const status = await backend.catalogStatus();
    expect(status.status).toBe("ready");
    if (status.status !== "ready") throw new Error("catalog not ready");
    expect(status.summary.installedPacks).toEqual([{ packId: packId("curated"), catalogRoot: digest }]);

    platform.load.mockResolvedValue(backend);
    render(<VisualPackManager />);
    expect(await screen.findByText(/Offline card images/i)).toBeInTheDocument();
    // The installed curated row, found by the one line only that row renders.
    // Its NAME is now "One image per card", which the selector radio also
    // carries, so matching on the name alone would be ambiguous — and matching
    // on the raw `curated` pack id no longer finds anything, because the panel
    // stopped quoting wire identities at users (step 5c-ii, item 4).
    expect(await screen.findByText(/Membership fingerprint:/)).toBeInTheDocument();
  });

  it("estimates a curated pack without the bulk archive's shard figures", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const catalog = await backend.refreshCatalog();
    const { selector, descriptors } = await curatedSelector();

    const estimate = await backend.estimateInstall(selector);

    expect(estimate.shardCount).toBe("0");
    expect(estimate.shardBytes).toBe("unknown");
    expect(estimate.assetRecords).toBe(String(descriptors.length));
    expect(estimate.uniqueObjects).toBe(String(descriptors.length));
    // The estimate names the catalog it was taken against; PackSelector only
    // renders one whose root equals the summary's.
    expect(estimate.catalogRoot).toBe(catalog.catalogRoot);
    expect(requested).not.toContain(BULK_DOWNLOAD_URL);
  });

  it("refuses a curated selector the planned membership no longer hashes to", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector } = await curatedSelector();
    usePreferencesStore.setState({ artChain: OLDEST });

    // The digest IS the pack's catalog root, so honouring a stale selector
    // would store this membership under a root it does not hash to. Same
    // reason `complete` rejects a selector naming a superseded bulk root.
    await expect(backend.estimateInstall(selector)).rejects.toMatchObject({ kind: "conflict" });
  });

  it("refuses a stale curated install before any operation record exists", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, descriptors } = await curatedSelector();
    usePreferencesStore.setState({ artChain: OLDEST });

    // The common sequence: estimate, change the art chain, then press Install.
    // A curated conflict is structurally non-retryable, so it has to reach the
    // caller as a rejected request. Creating a record first and failing it
    // inside `run()` leaves an operation stuck in `downloading` whose only
    // enabled control is a Resume that can never succeed.
    await expect(backend.start({ kind: "install", selector, objectEstimate: descriptors.length }))
      .rejects.toMatchObject({ kind: "conflict" });

    // Asserted on the store, not on the return value: the defect this replaces
    // returned `{status:"started"}` and left the record behind.
    expect(await operationRecords()).toHaveLength(0);
  });

  it("keeps a curated operation resumable after a transient download failure", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, descriptors } = await curatedSelector();
    const failed = await failures(backend);

    failImages = true;
    const started = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
    if (started.status !== "started") throw new Error("curated install did not start");
    await vi.waitFor(() => { expect(failed).toHaveLength(1); }, { timeout: 5000 });

    // The regression guard on the terminate rule. Only a STRUCTURALLY
    // non-retryable failure may end a record; a network outage must leave it
    // `downloading`, because that state is precisely what keeps Resume offered
    // and able to succeed.
    expect(failed[0].error).toBe("network");
    expect((await backend.operationStatus(started.operationId)).state).toBe("downloading");

    failImages = false;
    expect(await backend.start({ kind: "resume", operationId: started.operationId }))
      .toMatchObject({ status: "started", operationId: started.operationId });
    await settle(backend, started.operationId);
    expect(await resolvedAssets(backend, descriptors)).toHaveLength(descriptors.length);
  });

  it("terminates a curated operation whose membership moved and never resurrects it", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, descriptors } = await curatedSelector();
    const failed = await failures(backend);

    // A transient failure first: `start()` now refuses a stale digest outright,
    // so the only way a curated record can outlive its own start and then meet
    // a conflict is a preference change AFTER it was created.
    failImages = true;
    const started = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
    if (started.status !== "started") throw new Error("curated install did not start");
    await vi.waitFor(() => { expect(failed).toHaveLength(1); }, { timeout: 5000 });

    usePreferencesStore.setState({ artChain: OLDEST });
    failImages = false;

    // The launch path: `create()` re-runs every record still `downloading`.
    const relaunched = await ScryfallBrowserVisualPackBackend.create();
    await vi.waitFor(async () => {
      expect((await relaunched.operationStatus(started.operationId)).state).not.toBe("downloading");
    }, { timeout: 5000 });
    expect((await relaunched.operationStatus(started.operationId)).state).toBe("cancelled");

    const again = await ScryfallBrowserVisualPackBackend.create();
    const relaunchFailures = await failures(again);

    // A fresh curated install at the CURRENT digest, on the instance that just
    // launched, does two jobs: it proves the panel is genuinely unwedged rather
    // than merely flagged, and it is the positive control for the wait window
    // below — a resurrected operation reaches its own conflict in far less work
    // than a full six-image install, so zero failures across it is a
    // measurement rather than a timeout.
    const fresh = await installCurated(again);
    expect(await resolvedAssets(again, fresh.descriptors)).toHaveLength(fresh.descriptors.length);
    expect(fresh.digest).not.toBe((selector as { membershipDigest: CatalogRoot }).membershipDigest);
    expect(relaunchFailures).toHaveLength(0);
    expect((await again.operationStatus(started.operationId)).state).toBe("cancelled");
  });

  it("survives another tab promoting the curated pack mid-install", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, descriptors } = await curatedSelector();
    const digest = (selector as { membershipDigest: CatalogRoot }).membershipDigest;
    let announce: (value: OperationId) => void = () => undefined;
    const running = new Promise<OperationId>((resolve) => { announce = resolve; });
    let promoted = false;

    // `run()` serialises work within ONE backend instance, but IndexedDB is
    // shared across tabs and `loadVisualPackBackend` is a per-tab memo, so two
    // tabs can resume the same pending operation with no lock between them.
    // This is that interleaving: the other tab promotes the pack — same
    // operation id, same root — while this one is still downloading.
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const source = String(input);
      if (!promoted && source.startsWith("https://cards.scryfall.io/")) {
        promoted = true;
        const database = await openDB(DATABASE);
        await database.put("packs", {
          id: packId("curated"),
          packId: packId("curated"),
          root: digest,
          dependencies: [],
          operationId: await running,
        });
        database.close();
      }
      return fetchStub(input);
    }));

    const response = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
    if (response.status !== "started") throw new Error("curated install did not start");
    announce(response.operationId);
    await settle(backend, response.operationId);

    // `completePack`'s replace-guard is what stops this: comparing the existing
    // row's root against the SELECTOR's root recognises the promotion and
    // returns. Comparing it against the bulk root cannot ever match a curated
    // pack, so the cursor loop below it deletes every objects row at the
    // digest — the entire membership.
    expect(await resolvedAssets(backend, descriptors)).toHaveLength(descriptors.length);
  });

  it("surfaces a curated estimate through the manager hook", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await backend.refreshCatalog();
    platform.load.mockResolvedValue(backend);
    const { selector, descriptors } = await curatedSelector();

    const { result } = renderHook(() => useVisualPackManager());
    await waitFor(() => { expect(result.current.availability.kind).toBe("ready"); });
    act(() => { result.current.estimateInstall(selector); });

    // `signedSelectorName` must map a curated selector to its PACK ID. The hook
    // discards an estimate whose `selector` field disagrees, and a curated
    // selector's identity string carries a digest the pack id does not — so
    // without that mapping the estimate is dropped SILENTLY: no estimate, no
    // error, and Install permanently disabled. `actionError` is therefore as
    // load-bearing here as the value.
    await waitFor(() => {
      expect(result.current.estimate?.value.assetRecords).toBe(String(descriptors.length));
    });
    expect(result.current.actionError).toBeNull();
  });

  it("short-circuits a re-install at an unchanged digest without churning the membership", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { descriptors } = await installCurated(backend);
    const before = await resolvedAssets(backend, descriptors);
    // Without this the "intact" comparison below would hold vacuously over two
    // empty memberships.
    expect(before).toHaveLength(descriptors.length);
    requested = [];

    const { selector } = await curatedSelector();
    const response = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });

    expect(response).toEqual({ status: "healthy" });
    expect(imageRequests()).toHaveLength(0);
    expect(await resolvedAssets(backend, descriptors)).toEqual(before);
  });

  it("leaves the membership intact when the curated pack is repaired", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { descriptors } = await installCurated(backend);
    const before = await resolvedAssets(backend, descriptors);
    // Without this the "intact" comparison below would hold vacuously over two
    // empty memberships.
    expect(before).toHaveLength(descriptors.length);
    requested = [];

    // Curated repair is structurally a no-op: the repair selector's digest is
    // sourced from the packs row it is then compared against. Sourcing it from
    // the bulk root instead would let the operation run and cursor-delete the
    // whole membership.
    const response = await backend.start({ kind: "repair", packIds: [packId("curated")] });

    expect(response).toEqual({ status: "healthy" });
    expect(imageRequests()).toHaveLength(0);
    expect(await resolvedAssets(backend, descriptors)).toEqual(before);
  });

  it("re-downloads nothing when an interrupted curated operation resumes", async () => {
    const first = await ScryfallBrowserVisualPackBackend.create();
    const { descriptors, operation } = await installCurated(first);

    await interrupt(operation);
    cache.entries.clear();
    requested = [];

    const resumed = await ScryfallBrowserVisualPackBackend.create();
    await settle(resumed, operation);

    expect(requested).toHaveLength(0);
    expect(await resolvedAssets(resumed, descriptors)).toHaveLength(0);
  });

  it("resumes an interrupted curated operation at the catalog root", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { descriptors, operation } = await installCurated(backend);
    const catalog = await backend.catalogSummary();
    await interrupt(operation);
    cache.entries.clear();
    requested = [];

    const response = await backend.start({ kind: "resume", operationId: operation });

    // `unsupported` because happy-dom exposes no `navigator.storage`; the
    // point of asserting it here is that a resume reports the grant at all,
    // since a resume writes bytes exactly like the install it continues.
    expect(response).toEqual({
      status: "started",
      operationId: operation,
      catalogRoot: catalog.catalogRoot,
      persistence: "unsupported",
    });
    await settle(backend, operation);
    expect(requested).toHaveLength(0);
    expect(await resolvedAssets(backend, descriptors)).toHaveLength(0);
  });

  it("keeps a non-curated pack on the bulk catalog root", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const response = await backend.start({ kind: "install", selector: { kind: "core" }, objectEstimate: 1 });
    if (response.status !== "started") throw new Error("core install did not start");
    await settle(backend, response.operationId);

    const summary = await backend.catalogSummary();
    expect(summary.installedPacks).toEqual([{ packId: packId("core"), catalogRoot: summary.catalogRoot }]);
    expect(response.catalogRoot).toBe(summary.catalogRoot);
    // The restructured installed-filter must still short-circuit a re-install.
    expect(await backend.start({ kind: "install", selector: { kind: "core" }, objectEstimate: 1 }))
      .toEqual({ status: "healthy" });
  });

  it("replaces the membership wholesale when the art chain changes the digest", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await installCurated(backend);

    usePreferencesStore.setState({ artChain: OLDEST });
    const second = await installCurated(backend);

    expect(second.digest).not.toBe(first.digest);
    // The chain now picks Bolt's older printing, so its three exact_printing
    // keys move while Giant Growth's canonical three stay put.
    expect(second.descriptors.map((value) => value.assetKey))
      .not.toEqual(first.descriptors.map((value) => value.assetKey));

    const matches = await resolvedAssets(backend, second.descriptors);
    expect(matches).toHaveLength(second.descriptors.length);
    expect(new Set(matches.map((match) => match.catalogRoot))).toEqual(new Set([second.digest]));

    const summary = await backend.catalogSummary();
    expect(summary.installedPacks).toEqual([{ packId: packId("curated"), catalogRoot: second.digest }]);
    // The superseded root keeps no reachable rows.
    const stale = first.descriptors.filter((value) =>
      !second.descriptors.some((kept) => kept.assetKey === value.assetKey));
    expect(stale.length).toBeGreaterThan(0);
    expect(await resolvedAssets(backend, stale)).toHaveLength(0);
  });

  it("installs a curated pack driven entirely from the settings panel", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await backend.refreshCatalog();
    platform.load.mockResolvedValue(backend);
    const { selector, descriptors } = await curatedSelector();
    const digest = (selector as { membershipDigest: CatalogRoot }).membershipDigest;
    requested = [];

    render(<VisualPackManager />);
    // The radio, not the panel title: the title renders in every availability
    // state including `loading`, so awaiting it would let the click race the
    // backend's initialize and find no radiogroup at all.
    fireEvent.click(await screen.findByRole("radio", { name: /one image per card/i }));

    // The only click between here and a running install is Install itself:
    // the estimate that enables it is requested by the selection. Install is
    // gated on a matching estimate, so its enabled state IS that assertion.
    const install = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => { expect(install).toBeEnabled(); }, { timeout: 5000 });
    fireEvent.click(install);

    await waitFor(async () => {
      expect(await resolvedAssets(backend, descriptors)).toHaveLength(descriptors.length);
    }, { timeout: 5000 });

    expect(await backend.catalogSummary())
      .toMatchObject({ installedPacks: [{ packId: packId("curated"), catalogRoot: digest }] });
    // The scan this selector exists to avoid, from the path a user actually
    // takes. The fetch stub throws on any unrecognised URL, so opening the
    // bulk stream would fail this test at the fetch rather than here.
    expect(requested).not.toContain(BULK_DOWNLOAD_URL);
    expect(imageRequests()).toHaveLength(descriptors.length);
  });

  it("restores a paused manual install with its saved-image progress", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await backend.refreshCatalog();
    const { selector, descriptors } = await curatedSelector();
    holdLaterImages = true;
    const started = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
    if (started.status !== "started") throw new Error("curated install did not start");

    await vi.waitFor(async () => {
      expect((await backend.operationStatus(started.operationId)).objectsPromoted).toBeGreaterThan(0);
    });
    platform.load.mockResolvedValue(backend);
    render(<VisualPackManager />);

    // This panel did not start the operation. Its first visible count therefore
    // comes from subscribeProgress's current-operation snapshot, not a mocked
    // progress event or an eventual completion.
    expect(await screen.findByText(`1/${descriptors.length}`)).toBeInTheDocument();
    const progress = screen.getAllByRole("progressbar").find((element) => element.getAttribute("value") === "1");
    expect(progress).toBeDefined();

    releaseHeldImages?.();
    await settle(backend, started.operationId);
  });
});
